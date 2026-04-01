/**
 * agentOS VibeSwap — WASM hot-load pipeline with capability-gated execution
 *
 * Flow: agent submits module → AgentFS stores by hash → VibeSwap validates →
 *       loads into capability-gated slot → executes → returns result
 *
 * Security: capability-based. Each swap slot gets a restricted set of host
 * functions (capabilities). A module cannot call functions outside its grant.
 *
 * Hot-swap: drain in-flight calls, replace module in slot, resume.
 */

import { createHash } from 'node:crypto';

// ── Capability definitions ──────────────────────────────────────────────────
const BUILTIN_CAPABILITIES = {
  'io.log':        { description: 'Write to log buffer', risk: 'low' },
  'io.read':       { description: 'Read from input buffer', risk: 'low' },
  'io.write':      { description: 'Write to output buffer', risk: 'low' },
  'math.random':   { description: 'Generate random numbers', risk: 'low' },
  'time.now':      { description: 'Get current timestamp (ms)', risk: 'low' },
  'kv.get':        { description: 'Read from key-value store', risk: 'medium' },
  'kv.set':        { description: 'Write to key-value store', risk: 'medium' },
  'net.fetch':     { description: 'HTTP fetch (outbound)', risk: 'high' },
  'fs.read':       { description: 'Read filesystem (scoped)', risk: 'high' },
  'fs.write':      { description: 'Write filesystem (scoped)', risk: 'critical' },
};

// ── WASM magic bytes ────────────────────────────────────────────────────────
const WASM_MAGIC = Buffer.from([0x00, 0x61, 0x73, 0x6d]); // \0asm

// ── Swap Slot ───────────────────────────────────────────────────────────────
class SwapSlot {
  constructor(name, capabilities = []) {
    this.name = name;
    this.capabilities = new Set(capabilities);
    this.module = null;      // compiled WebAssembly.Module
    this.instance = null;    // live WebAssembly.Instance
    this.hash = null;        // SHA-256 of loaded WASM bytes
    this.loadedAt = null;
    this.callCount = 0;
    this.draining = false;
    this.inflight = 0;
    this.kvStore = new Map(); // per-slot isolated KV
    this.logBuffer = [];      // captured log output
    this.outputBuffer = [];   // captured output
  }

  get status() {
    return {
      name: this.name,
      loaded: !!this.module,
      hash: this.hash,
      capabilities: [...this.capabilities],
      callCount: this.callCount,
      inflight: this.inflight,
      draining: this.draining,
      loadedAt: this.loadedAt,
    };
  }
}

// ── VibeSwap Engine ─────────────────────────────────────────────────────────
export class VibeSwap {
  constructor(options = {}) {
    this.slots = new Map();           // name → SwapSlot
    this.agentfsUrl = options.agentfsUrl || null; // e.g. http://100.89.199.14:8791
    this.auditLog = [];               // append-only audit trail
    this.maxSlots = options.maxSlots || 16;
    this.executionTimeoutMs = options.executionTimeoutMs || 5000;
  }

  // ── Slot management ─────────────────────────────────────────────────────

  createSlot(name, capabilities = ['io.log', 'io.read', 'io.write', 'time.now']) {
    if (this.slots.has(name)) {
      throw new Error(`Slot "${name}" already exists`);
    }
    if (this.slots.size >= this.maxSlots) {
      throw new Error(`Max slots (${this.maxSlots}) reached`);
    }
    // Validate all requested capabilities
    for (const cap of capabilities) {
      if (!BUILTIN_CAPABILITIES[cap]) {
        throw new Error(`Unknown capability: ${cap}`);
      }
    }
    const slot = new SwapSlot(name, capabilities);
    this.slots.set(name, slot);
    this._audit('slot-create', { name, capabilities });
    return slot.status;
  }

  removeSlot(name) {
    const slot = this.slots.get(name);
    if (!slot) throw new Error(`Slot "${name}" not found`);
    if (slot.inflight > 0) throw new Error(`Slot "${name}" has in-flight calls`);
    this.slots.delete(name);
    this._audit('slot-remove', { name });
  }

  listSlots() {
    return [...this.slots.values()].map(s => s.status);
  }

  // ── Module validation ───────────────────────────────────────────────────

  validateWasm(wasmBytes) {
    if (!Buffer.isBuffer(wasmBytes)) {
      wasmBytes = Buffer.from(wasmBytes);
    }
    const errors = [];

    // Magic bytes check
    if (wasmBytes.length < 8) {
      errors.push('Too small to be a valid WASM module');
    } else if (!wasmBytes.subarray(0, 4).equals(WASM_MAGIC)) {
      errors.push('Invalid WASM magic bytes (expected \\0asm)');
    }

    // Version check (WASM 1.0 = 0x01000000)
    if (wasmBytes.length >= 8) {
      const version = wasmBytes.readUInt32LE(4);
      if (version !== 1) {
        errors.push(`Unsupported WASM version: ${version}`);
      }
    }

    // Size sanity (max 10MB for demo)
    if (wasmBytes.length > 10 * 1024 * 1024) {
      errors.push(`Module too large: ${wasmBytes.length} bytes (max 10MB)`);
    }

    const hash = createHash('sha256').update(wasmBytes).digest('hex');

    return {
      valid: errors.length === 0,
      errors,
      hash,
      size: wasmBytes.length,
    };
  }

  // ── Load module into slot ───────────────────────────────────────────────

  async loadModule(slotName, wasmBytes) {
    const slot = this.slots.get(slotName);
    if (!slot) throw new Error(`Slot "${slotName}" not found`);

    // Validate
    const validation = this.validateWasm(wasmBytes);
    if (!validation.valid) {
      throw new Error(`WASM validation failed: ${validation.errors.join(', ')}`);
    }

    // If hot-swapping, drain first
    if (slot.module) {
      await this._drainSlot(slot);
    }

    // Compile
    const compiled = await WebAssembly.compile(wasmBytes);

    // Build host functions based on slot capabilities
    const importObject = this._buildImportObject(slot);

    // Instantiate
    const instance = await WebAssembly.instantiate(compiled, importObject);

    // Swap in
    slot.module = compiled;
    slot.instance = instance;
    slot.hash = validation.hash;
    slot.loadedAt = new Date().toISOString();
    slot.draining = false;
    slot.logBuffer = [];
    slot.outputBuffer = [];

    this._audit('module-load', {
      slot: slotName,
      hash: validation.hash,
      size: validation.size,
      capabilities: [...slot.capabilities],
    });

    return {
      ok: true,
      slot: slotName,
      hash: validation.hash,
      size: validation.size,
      exports: Object.keys(instance.exports),
    };
  }

  // ── Load from AgentFS ───────────────────────────────────────────────────

  async loadFromAgentFS(slotName, moduleHash, options = {}) {
    if (!this.agentfsUrl) {
      throw new Error('AgentFS URL not configured');
    }

    const aot = options.aot ? '?aot=1' : '';
    const url = `${this.agentfsUrl}/agentfs/modules/${moduleHash}${aot}`;

    const resp = await fetch(url);
    if (!resp.ok) {
      throw new Error(`AgentFS fetch failed: ${resp.status} ${resp.statusText}`);
    }

    const wasmBytes = Buffer.from(await resp.arrayBuffer());
    return this.loadModule(slotName, wasmBytes);
  }

  // ── Execute function in slot ────────────────────────────────────────────

  async execute(slotName, funcName, args = []) {
    const slot = this.slots.get(slotName);
    if (!slot) throw new Error(`Slot "${slotName}" not found`);
    if (!slot.instance) throw new Error(`Slot "${slotName}" has no module loaded`);
    if (slot.draining) throw new Error(`Slot "${slotName}" is draining for hot-swap`);

    const fn = slot.instance.exports[funcName];
    if (typeof fn !== 'function') {
      throw new Error(`Export "${funcName}" not found or not a function in slot "${slotName}"`);
    }

    slot.inflight++;
    slot.callCount++;

    const startMs = Date.now();
    let result, error;

    try {
      // Execute with timeout protection
      result = await Promise.race([
        Promise.resolve(fn(...args)),
        new Promise((_, reject) =>
          setTimeout(() => reject(new Error(`Execution timeout (${this.executionTimeoutMs}ms)`)),
            this.executionTimeoutMs)
        ),
      ]);
    } catch (e) {
      error = e.message;
    } finally {
      slot.inflight--;
    }

    const elapsedMs = Date.now() - startMs;

    this._audit('execute', {
      slot: slotName,
      func: funcName,
      args,
      elapsedMs,
      ok: !error,
      error: error || undefined,
    });

    if (error) throw new Error(error);

    return {
      result,
      elapsedMs,
      logs: [...slot.logBuffer],
      output: [...slot.outputBuffer],
    };
  }

  // ── Hot-swap: replace module without restart ────────────────────────────

  async hotSwap(slotName, newWasmBytes) {
    const slot = this.slots.get(slotName);
    if (!slot) throw new Error(`Slot "${slotName}" not found`);

    const oldHash = slot.hash;

    // Drain, load new, resume — all in loadModule
    const result = await this.loadModule(slotName, newWasmBytes);

    this._audit('hot-swap', {
      slot: slotName,
      oldHash,
      newHash: result.hash,
    });

    return { ...result, swapped: true, oldHash };
  }

  // ── Introspect ──────────────────────────────────────────────────────────

  getSlot(name) {
    const slot = this.slots.get(name);
    if (!slot) return null;
    return {
      ...slot.status,
      exports: slot.instance ? Object.keys(slot.instance.exports) : [],
      logs: [...slot.logBuffer],
      output: [...slot.outputBuffer],
      kvStore: Object.fromEntries(slot.kvStore),
    };
  }

  getAuditLog(limit = 50) {
    return this.auditLog.slice(-limit);
  }

  // ── Internal: build import object based on capabilities ─────────────────

  _buildImportObject(slot) {
    const env = {};

    // Always available: abort handler
    env.abort = (msgPtr, filePtr, line, col) => {
      slot.logBuffer.push(`[ABORT] at line ${line}:${col}`);
    };

    // io.log — console-like logging
    if (slot.capabilities.has('io.log')) {
      env.log_i32 = (val) => { slot.logBuffer.push(`[log] i32: ${val}`); };
      env.log_f64 = (val) => { slot.logBuffer.push(`[log] f64: ${val}`); };
    }

    // io.write — output buffer
    if (slot.capabilities.has('io.write')) {
      env.output_i32 = (val) => { slot.outputBuffer.push(val); };
    }

    // time.now — current timestamp
    if (slot.capabilities.has('time.now')) {
      env.time_now_ms = () => Date.now();
    }

    // math.random
    if (slot.capabilities.has('math.random')) {
      env.random_f64 = () => Math.random();
    }

    // kv.get / kv.set — per-slot KV (simplified: integer keys for demo)
    if (slot.capabilities.has('kv.get')) {
      env.kv_get = (key) => slot.kvStore.get(key) ?? 0;
    }
    if (slot.capabilities.has('kv.set')) {
      env.kv_set = (key, val) => { slot.kvStore.set(key, val); };
    }

    // Memory for string passing (provided by module, not host)
    return { env };
  }

  // ── Internal: drain a slot before swap ──────────────────────────────────

  async _drainSlot(slot) {
    slot.draining = true;
    const start = Date.now();
    // Wait for in-flight calls to complete (max 5s)
    while (slot.inflight > 0 && Date.now() - start < 5000) {
      await new Promise(r => setTimeout(r, 50));
    }
    if (slot.inflight > 0) {
      this._audit('drain-timeout', { slot: slot.name, remaining: slot.inflight });
    }
  }

  // ── Internal: audit logging ─────────────────────────────────────────────

  _audit(action, detail) {
    this.auditLog.push({
      ts: new Date().toISOString(),
      action,
      ...detail,
    });
    // Keep audit log bounded
    if (this.auditLog.length > 1000) {
      this.auditLog = this.auditLog.slice(-500);
    }
  }
}

// ── REST API wrapper (for standalone service mode) ──────────────────────────

export async function startVibeSwapServer(options = {}) {
  const { createServer } = await import('node:http');

  const port = options.port || parseInt(process.env.VIBESWAP_PORT || '8792');
  const engine = new VibeSwap(options);

  const server = createServer(async (req, res) => {
    const url = new URL(req.url, `http://localhost:${port}`);
    const path = url.pathname;
    const method = req.method;

    res.setHeader('Content-Type', 'application/json');

    try {
      // Health
      if (path === '/health' && method === 'GET') {
        return send(res, 200, {
          ok: true,
          service: 'vibeswap',
          slots: engine.slots.size,
          agentfsUrl: engine.agentfsUrl || null,
        });
      }

      // List slots
      if (path === '/slots' && method === 'GET') {
        return send(res, 200, { ok: true, slots: engine.listSlots() });
      }

      // Create slot
      if (path === '/slots' && method === 'POST') {
        const body = await readBody(req);
        const result = engine.createSlot(body.name, body.capabilities);
        return send(res, 201, { ok: true, slot: result });
      }

      // Get slot detail
      const slotMatch = path.match(/^\/slots\/([^/]+)$/);
      if (slotMatch && method === 'GET') {
        const detail = engine.getSlot(slotMatch[1]);
        if (!detail) return send(res, 404, { error: 'Slot not found' });
        return send(res, 200, { ok: true, slot: detail });
      }

      // Load module into slot
      const loadMatch = path.match(/^\/slots\/([^/]+)\/load$/);
      if (loadMatch && method === 'POST') {
        const body = await readBody(req);
        let result;
        if (body.wasmHash && engine.agentfsUrl) {
          // Migration restore: load from AgentFS by hash + restore KV state
          result = await engine.loadFromAgentFS(loadMatch[1], body.wasmHash, { aot: body.aot });
          // Restore migrated state
          const slot = engine.slots.get(loadMatch[1]);
          if (slot && body.kvStore) {
            slot.kvStore = new Map(Object.entries(body.kvStore));
          }
          if (slot && typeof body.callCount === 'number') {
            slot.callCount = body.callCount;
          }
          if (slot && body.capabilities) {
            // Re-apply capability set from snapshot (may differ from slot default)
            slot.capabilities = new Set(body.capabilities);
          }
          slot?.logBuffer.push(`[migrate] restored from ${body.migratedFrom || 'unknown'} at ${body.snapshotAt || new Date().toISOString()}`);
        } else if (body.hash && engine.agentfsUrl) {
          // Load from AgentFS by hash
          result = await engine.loadFromAgentFS(loadMatch[1], body.hash, { aot: body.aot });
        } else if (body.wasm) {
          // Load from base64-encoded WASM
          const wasmBytes = Buffer.from(body.wasm, 'base64');
          result = await engine.loadModule(loadMatch[1], wasmBytes);
        } else {
          return send(res, 400, { error: 'Provide "wasmHash" (migration), "hash" (AgentFS) or "wasm" (base64)' });
        }
        return send(res, 200, { ok: true, ...result });
      }

      // Execute function in slot
      const execMatch = path.match(/^\/slots\/([^/]+)\/exec\/([^/]+)$/);
      if (execMatch && method === 'POST') {
        const body = await readBody(req);
        const result = await engine.execute(execMatch[1], execMatch[2], body.args || []);
        return send(res, 200, { ok: true, ...result });
      }

      // Hot-swap module in slot
      const swapMatch = path.match(/^\/slots\/([^/]+)\/swap$/);
      if (swapMatch && method === 'POST') {
        const body = await readBody(req);
        let result;
        if (body.hash && engine.agentfsUrl) {
          // Fetch new module from AgentFS, then swap
          const url = `${engine.agentfsUrl}/agentfs/modules/${body.hash}`;
          const resp = await fetch(url);
          if (!resp.ok) throw new Error(`AgentFS fetch: ${resp.status}`);
          const wasmBytes = Buffer.from(await resp.arrayBuffer());
          result = await engine.hotSwap(swapMatch[1], wasmBytes);
        } else if (body.wasm) {
          const wasmBytes = Buffer.from(body.wasm, 'base64');
          result = await engine.hotSwap(swapMatch[1], wasmBytes);
        } else {
          return send(res, 400, { error: 'Provide "hash" or "wasm"' });
        }
        return send(res, 200, { ok: true, ...result });
      }

      // Drain slot (pause for migration snapshot)
      const drainMatch = path.match(/^\/slots\/([^/]+)\/drain$/);
      if (drainMatch && method === 'POST') {
        const slot = engine.slots.get(drainMatch[1]);
        if (!slot) return send(res, 404, { error: 'Slot not found' });
        // Wait for in-flight calls to finish (up to 5s)
        const deadline = Date.now() + 5000;
        while (slot.inflight > 0 && Date.now() < deadline) {
          await new Promise(r => setTimeout(r, 50));
        }
        slot.draining = true;
        // Return full slot state including kvStore for snapshot
        return send(res, 200, {
          ok: true,
          slot: {
            name: slot.name,
            hash: slot.hash,
            loaded: !!slot.module,
            capabilities: [...slot.capabilities],
            callCount: slot.callCount,
            inflight: slot.inflight,
            draining: slot.draining,
            loadedAt: slot.loadedAt,
            kvStore: Object.fromEntries(slot.kvStore),
            logs: [...slot.logBuffer].slice(-50),
            output: [...slot.outputBuffer].slice(-50),
          },
        });
      }

      // DELETE slot — tear down after migration
      const deleteSlotMatch = path.match(/^\/slots\/([^/]+)$/);
      if (deleteSlotMatch && method === 'DELETE') {
        const slotName = deleteSlotMatch[1];
        if (!engine.slots.has(slotName)) return send(res, 404, { error: 'Slot not found' });
        engine.slots.delete(slotName);
        return send(res, 200, { ok: true, deleted: slotName });
      }

      // Audit log
      if (path === '/audit' && method === 'GET') {
        const limit = parseInt(url.searchParams.get('limit') || '50');
        return send(res, 200, { ok: true, audit: engine.getAuditLog(limit) });
      }

      // Validate WASM (without loading)
      if (path === '/validate' && method === 'POST') {
        const body = await readBody(req);
        const wasmBytes = Buffer.from(body.wasm, 'base64');
        const result = engine.validateWasm(wasmBytes);
        return send(res, 200, { ok: true, ...result });
      }

      // Capabilities reference
      if (path === '/capabilities' && method === 'GET') {
        return send(res, 200, { ok: true, capabilities: BUILTIN_CAPABILITIES });
      }

      return send(res, 404, { error: 'Not found' });
    } catch (e) {
      return send(res, 500, { error: e.message });
    }
  });

  server.listen(port, '0.0.0.0', () => {
    console.log(`[vibeswap] listening on :${port}`);
  });

  return { server, engine };
}

function send(res, code, obj) {
  res.writeHead(code);
  res.end(JSON.stringify(obj));
}

function readBody(req) {
  return new Promise((resolve, reject) => {
    const chunks = [];
    req.on('data', c => chunks.push(c));
    req.on('end', () => {
      try {
        resolve(JSON.parse(Buffer.concat(chunks).toString()));
      } catch { resolve({}); }
    });
    req.on('error', reject);
  });
}

// ── Standalone ──────────────────────────────────────────────────────────────
if (import.meta.url === `file://${process.argv[1]}`) {
  const agentfsUrl = process.env.AGENTFS_URL || 'http://100.89.199.14:8791';
  startVibeSwapServer({ agentfsUrl });
}
