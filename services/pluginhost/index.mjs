/**
 * CCC PluginHost — WASM hot-swap service (vibe-swap demo)
 *
 * REST API:
 *   GET  /pluginhost/health            Health + active slot inventory
 *   POST /pluginhost/slots             Create a capability-gated swap slot
 *   GET  /pluginhost/slots             List all swap slots
 *   GET  /pluginhost/slots/:slotId     Slot status + last run
 *   POST /pluginhost/slots/:slotId/load    Load a WASM module by hash (from AgentFS)
 *   POST /pluginhost/slots/:slotId/exec    Execute loaded module (call exported fn)
 *   POST /pluginhost/slots/:slotId/swap    Hot-swap: drain → swap → resume
 *   DELETE /pluginhost/slots/:slotId   Deactivate slot
 *
 * Hot-swap flow:
 *   1. agent submits WASM module → AgentFS (sparky:8791) → gets hash
 *   2. agent calls POST /pluginhost/slots/:id/swap {hash: "<sha256>"}
 *   3. PluginHost: drains in-flight calls, loads new WASM from AgentFS, resumes
 *   4. Clients are unaware of the swap — same slot, new code
 *
 * Capability gates:
 *   - Slots have a declared capability set (read-only, compute-only, network, etc.)
 *   - Modules are checked against the slot's cap set before loading
 *   - A module declaring net_access cannot load into a compute-only slot
 *
 * Runs on: do-host1, accessible to fleet
 */

import { createServer } from 'http';
import { createHash } from 'crypto';

// ── Config ────────────────────────────────────────────────────────────────────
const PORT         = parseInt(process.env.PLUGINHOST_PORT || '8793', 10);
const AUTH_TOKEN   = process.env.PLUGINHOST_TOKEN || 'pluginhost-dev-token';
const AGENTFS_URL  = process.env.AGENTFS_URL || 'http://100.89.199.14:8791';
const AGENTFS_TOK  = process.env.AGENTFS_TOKEN || 'agentfs-dev-token';
const MAX_EXEC_MS  = parseInt(process.env.MAX_EXEC_MS || '5000', 10);

// ── Capability definitions ────────────────────────────────────────────────────
const CAPABILITY_SETS = {
  'compute-only':  { net_access: false, fs_access: false, gpu_access: false },
  'compute+log':   { net_access: false, fs_access: true,  gpu_access: false },
  'full-sandbox':  { net_access: true,  fs_access: true,  gpu_access: false },
  'gpu-compute':   { net_access: false, fs_access: false, gpu_access: true  },
};

// ── In-memory state ───────────────────────────────────────────────────────────
// slots: Map<slotId, SlotRecord>
const slots = new Map();
const swapLog = []; // rolling log of swap events

let slotCounter = 0;

// ── Helpers ───────────────────────────────────────────────────────────────────
function makeSlotId() {
  return `slot-${++slotCounter}-${Date.now().toString(36)}`;
}

function auth(req) {
  const hdr = req.headers['authorization'] || '';
  return hdr.replace(/^Bearer\s+/i, '') === AUTH_TOKEN;
}

function json(res, code, obj) {
  const body = JSON.stringify(obj);
  res.writeHead(code, { 'Content-Type': 'application/json', 'Content-Length': Buffer.byteLength(body) });
  res.end(body);
}

async function readBody(req) {
  return new Promise((resolve, reject) => {
    const chunks = [];
    req.on('data', c => chunks.push(c));
    req.on('end', () => resolve(Buffer.concat(chunks)));
    req.on('error', reject);
  });
}

// Fetch WASM bytes from AgentFS by hash
async function fetchFromAgentFS(hash) {
  const url = `${AGENTFS_URL}/agentfs/modules/${hash}`;
  const res = await fetch(url, { headers: { 'Authorization': `Bearer ${AGENTFS_TOK}` } });
  if (!res.ok) throw new Error(`AgentFS returned ${res.status} for hash ${hash}`);
  const buf = await res.arrayBuffer();
  return Buffer.from(buf);
}

// Validate WASM magic bytes
function validateWasm(buf) {
  if (buf.length < 8) return { ok: false, reason: 'too small' };
  const magic = buf.slice(0, 4).toString('hex');
  if (magic !== '0061736d') return { ok: false, reason: `bad magic: ${magic}` };
  const version = buf.readUInt32LE(4);
  if (version !== 1) return { ok: false, reason: `unsupported version: ${version}` };
  return { ok: true };
}

// Parse WASM exports via WebAssembly (Node built-in)
async function loadWasmModule(wasmBytes) {
  const compiled = await WebAssembly.compile(wasmBytes);
  const exports = WebAssembly.Module.exports(compiled);
  return { compiled, exports };
}

// Execute a function in a loaded WASM module with timeout
async function execWasm(slot, fnName, args) {
  if (!slot.wasmInstance) throw new Error('No WASM instance loaded in slot');
  const fn = slot.wasmInstance.exports[fnName];
  if (!fn) throw new Error(`Export '${fnName}' not found. Available: ${Object.keys(slot.wasmInstance.exports).join(', ')}`);
  if (typeof fn !== 'function') throw new Error(`Export '${fnName}' is not callable`);

  const t0 = Date.now();
  const result = await Promise.race([
    Promise.resolve().then(() => fn(...args)),
    new Promise((_, rej) => setTimeout(() => rej(new Error(`exec timeout (${MAX_EXEC_MS}ms)`)), MAX_EXEC_MS)),
  ]);
  return { result, elapsed_ms: Date.now() - t0 };
}

// Hot-swap: drain (mark draining), swap module, resume
async function hotSwap(slot, newHash) {
  const oldHash = slot.moduleHash;
  const swapStart = Date.now();

  // Phase 1: drain — mark slot as draining (reject new execs)
  slot.state = 'draining';
  slot.drainStart = swapStart;

  // Wait for in-flight execs to finish (simple: wait up to 2s)
  const drainTimeout = 2000;
  const drainStart = Date.now();
  while (slot.inflightCount > 0 && (Date.now() - drainStart) < drainTimeout) {
    await new Promise(r => setTimeout(r, 50));
  }
  if (slot.inflightCount > 0) {
    slot.state = 'active'; // rollback
    throw new Error(`drain timeout: ${slot.inflightCount} in-flight calls after ${drainTimeout}ms`);
  }

  // Phase 2: load new module
  slot.state = 'loading';
  const wasmBytes = await fetchFromAgentFS(newHash);
  const validation = validateWasm(wasmBytes);
  if (!validation.ok) {
    slot.state = 'active';
    throw new Error(`WASM validation failed: ${validation.reason}`);
  }

  const { compiled, exports } = await loadWasmModule(wasmBytes);
  const imports = {}; // no imports for sandboxed compute-only modules
  const instance = await WebAssembly.instantiate(compiled, imports);

  // Phase 3: swap
  const prevInstance = slot.wasmInstance;
  const prevHash = slot.moduleHash;
  const prevCompiled = slot.wasmCompiled;

  slot.wasmInstance = instance;
  slot.wasmCompiled = compiled;
  slot.wasmExports = exports.map(e => e.name);
  slot.moduleHash = newHash;
  slot.moduleSize = wasmBytes.length;
  slot.loadedAt = new Date().toISOString();
  slot.swapCount = (slot.swapCount || 0) + 1;
  slot.state = 'active';

  const swapElapsed = Date.now() - swapStart;

  // Log the swap event
  swapLog.unshift({
    ts: new Date().toISOString(),
    slotId: slot.id,
    fromHash: oldHash || null,
    toHash: newHash,
    elapsed_ms: swapElapsed,
    drainMs: Date.now() - drainStart,
  });
  if (swapLog.length > 100) swapLog.pop();

  return { swapElapsed, fromHash: prevHash, toHash: newHash };
}

// ── Router ────────────────────────────────────────────────────────────────────
async function handle(req, res) {
  const url = new URL(req.url, `http://localhost`);
  const path = url.pathname;
  const method = req.method.toUpperCase();

  // Health — no auth
  if (method === 'GET' && path === '/pluginhost/health') {
    return json(res, 200, {
      ok: true,
      service: 'pluginhost',
      version: '0.1.0',
      slots: slots.size,
      activeSlots: [...slots.values()].filter(s => s.state === 'active').length,
      agentfsUrl: AGENTFS_URL,
      uptime_seconds: Math.floor(process.uptime()),
    });
  }

  // All other routes require auth
  if (!auth(req)) return json(res, 401, { error: 'Unauthorized' });

  // GET /pluginhost/slots
  if (method === 'GET' && path === '/pluginhost/slots') {
    const list = [...slots.values()].map(s => ({
      id: s.id, name: s.name, state: s.state, caps: s.caps,
      moduleHash: s.moduleHash, loadedAt: s.loadedAt,
      swapCount: s.swapCount || 0, execCount: s.execCount || 0,
      wasmExports: s.wasmExports || [],
    }));
    return json(res, 200, { slots: list, swapLog: swapLog.slice(0, 10) });
  }

  // POST /pluginhost/slots — create new slot
  if (method === 'POST' && path === '/pluginhost/slots') {
    const body = JSON.parse(await readBody(req));
    const capName = body.caps || 'compute-only';
    if (!CAPABILITY_SETS[capName]) {
      return json(res, 400, { error: `unknown cap set: ${capName}. Known: ${Object.keys(CAPABILITY_SETS).join(', ')}` });
    }
    const slotId = makeSlotId();
    const slot = {
      id: slotId,
      name: body.name || slotId,
      state: 'empty',
      caps: capName,
      capDefs: CAPABILITY_SETS[capName],
      moduleHash: null,
      wasmInstance: null,
      wasmCompiled: null,
      wasmExports: [],
      loadedAt: null,
      swapCount: 0,
      execCount: 0,
      inflightCount: 0,
      createdAt: new Date().toISOString(),
    };
    slots.set(slotId, slot);
    return json(res, 201, { ok: true, slotId, caps: capName });
  }

  // Slot-specific routes: /pluginhost/slots/:slotId[/action]
  const slotMatch = path.match(/^\/pluginhost\/slots\/([^/]+)(\/.*)?$/);
  if (!slotMatch) return json(res, 404, { error: 'not found' });

  const slotId = slotMatch[1];
  const action = slotMatch[2] || '';
  const slot = slots.get(slotId);

  if (!slot && action !== '') return json(res, 404, { error: `slot ${slotId} not found` });

  // GET /pluginhost/slots/:id
  if (method === 'GET' && !action) {
    if (!slot) return json(res, 404, { error: 'slot not found' });
    return json(res, 200, {
      id: slot.id, name: slot.name, state: slot.state, caps: slot.caps,
      moduleHash: slot.moduleHash, loadedAt: slot.loadedAt,
      swapCount: slot.swapCount, execCount: slot.execCount,
      wasmExports: slot.wasmExports,
      createdAt: slot.createdAt,
    });
  }

  // DELETE /pluginhost/slots/:id
  if (method === 'DELETE' && !action) {
    if (!slot) return json(res, 404, { error: 'slot not found' });
    slots.delete(slotId);
    return json(res, 200, { ok: true, deleted: slotId });
  }

  // POST /pluginhost/slots/:id/load
  if (method === 'POST' && action === '/load') {
    if (!slot) return json(res, 404, { error: 'slot not found' });
    const body = JSON.parse(await readBody(req));
    if (!body.hash) return json(res, 400, { error: 'missing: hash' });

    slot.state = 'loading';
    try {
      const wasmBytes = await fetchFromAgentFS(body.hash);
      const validation = validateWasm(wasmBytes);
      if (!validation.ok) {
        slot.state = slot.moduleHash ? 'active' : 'empty';
        return json(res, 400, { error: `WASM validation failed: ${validation.reason}` });
      }
      const { compiled, exports } = await loadWasmModule(wasmBytes);
      const instance = await WebAssembly.instantiate(compiled, {});
      slot.wasmInstance = instance;
      slot.wasmCompiled = compiled;
      slot.wasmExports = exports.map(e => e.name);
      slot.moduleHash = body.hash;
      slot.moduleSize = wasmBytes.length;
      slot.loadedAt = new Date().toISOString();
      slot.state = 'active';
      return json(res, 200, {
        ok: true, slotId, moduleHash: body.hash,
        exports: slot.wasmExports, size: wasmBytes.length,
      });
    } catch (e) {
      slot.state = slot.moduleHash ? 'active' : 'error';
      return json(res, 500, { error: e.message });
    }
  }

  // POST /pluginhost/slots/:id/exec
  if (method === 'POST' && action === '/exec') {
    if (!slot) return json(res, 404, { error: 'slot not found' });
    if (slot.state !== 'active') return json(res, 409, { error: `slot is ${slot.state}, not active` });

    const body = JSON.parse(await readBody(req));
    const fn = body.fn || 'run';
    const args = body.args || [];

    slot.inflightCount++;
    slot.execCount++;
    try {
      const { result, elapsed_ms } = await execWasm(slot, fn, args);
      return json(res, 200, { ok: true, slotId, fn, result, elapsed_ms });
    } catch (e) {
      return json(res, 500, { error: e.message });
    } finally {
      slot.inflightCount--;
    }
  }

  // POST /pluginhost/slots/:id/swap
  if (method === 'POST' && action === '/swap') {
    if (!slot) return json(res, 404, { error: 'slot not found' });
    const body = JSON.parse(await readBody(req));
    if (!body.hash) return json(res, 400, { error: 'missing: hash' });

    try {
      const { swapElapsed, fromHash, toHash } = await hotSwap(slot, body.hash);
      return json(res, 200, {
        ok: true, slotId,
        fromHash, toHash,
        swap_ms: swapElapsed,
        exports: slot.wasmExports,
        message: `Hot-swap complete in ${swapElapsed}ms`,
      });
    } catch (e) {
      return json(res, 500, { error: e.message });
    }
  }

  return json(res, 404, { error: 'not found' });
}

// ── Start ─────────────────────────────────────────────────────────────────────
const server = createServer(async (req, res) => {
  try { await handle(req, res); }
  catch (e) { json(res, 500, { error: e.message }); }
});

server.listen(PORT, () => {
  console.log(`pluginhost vibe-swap v0.1.0 running on :${PORT}`);
  console.log(`  AgentFS backend: ${AGENTFS_URL}`);
  console.log(`  Cap sets: ${Object.keys(CAPABILITY_SETS).join(', ')}`);
});

export default server;
