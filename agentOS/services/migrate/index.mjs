/**
 * agentOS MigrateAgent — live WASM slot migration between mesh peers
 *
 * Serialises a running VibeSwap slot (module hash, KV store, cap mask,
 * call counters, log history) into a portable snapshot, ships it to a
 * target peer via TransportMesh / AgentFS, and restores it there via
 * VibeSwap hot-swap.  The source slot is torn down after confirmed restore.
 *
 * REST API (all require Bearer token):
 *   POST  /migrate/start           { slotName, targetPeer }  → { migrationId }
 *   GET   /migrate/:id             → MigrationRecord
 *   GET   /migrate                 → list recent migrations
 *   POST  /migrate/:id/restore     { snapshot }  → restores on this node
 *   GET   /migrate/health          → { ok, activeMigrations }
 *
 * SquirrelBus event (published on complete):
 *   type: "agentos.migrate"
 *   payload: { migrationId, slotName, fromPeer, toPeer, wasmHash, durationMs }
 *
 * Integration:
 *   - Source: calls VibeSwap internally (same process or HTTP if separate)
 *   - Target: receives POST /migrate/:id/restore with the snapshot body
 *   - AgentFS: WASM module bytes are fetched by hash if not locally cached
 *
 * Flow:
 *   1. POST /migrate/start  → migrationId created, status=snapshotting
 *   2. Pause source slot (drain in-flight calls)
 *   3. Serialize: {wasmHash, kvStore, callCount, logBuffer, capabilities, ts}
 *   4. Forward snapshot + migrationId to targetPeer via HTTP
 *   5. Target receives POST /migrate/:id/restore → loads WASM + restores state
 *   6. Source receives 200 from target → tears down source slot
 *   7. Publish agentos.migrate event to SquirrelBus
 */

import { createServer } from 'http';
import { createHash } from 'crypto';
import { readFile } from 'fs/promises';
import { fileURLToPath } from 'url';
import { dirname, join } from 'path';

const __dirname = dirname(fileURLToPath(import.meta.url));

// ── Config ────────────────────────────────────────────────────────────────────
const PORT           = parseInt(process.env.MIGRATE_PORT    || '8795', 10);
const AUTH_TOKEN     = process.env.MIGRATE_TOKEN            || process.env.AGENTFS_TOKEN || 'migrate-dev-token';
const VIBESWAP_URL   = process.env.VIBESWAP_URL             || 'http://localhost:8793';
const AGENTFS_URL    = process.env.AGENTFS_URL              || 'http://localhost:8791';
const SQUIRRELBUS    = process.env.SQUIRRELBUS_URL          || null;
const NODE_NAME      = process.env.AGENT_NODE               || process.env.AGENT_NAME || 'unknown';
const VIBESWAP_TOKEN = process.env.VIBESWAP_TOKEN           || process.env.AGENTFS_TOKEN || 'agentfs-dev-token';

// ── In-memory migration registry (ring buffer, max 200) ───────────────────────
const migrations = new Map();   // migrationId → MigrationRecord
const migrationOrder = [];      // insertion order for eviction

// ── MigrationRecord shape ─────────────────────────────────────────────────────
//  { id, slotName, fromPeer, toPeer, wasmHash, status, createdAt, updatedAt,
//    durationMs, error, snapshot: {...} | null }

// ── Helpers ───────────────────────────────────────────────────────────────────
function uid() {
  return `mgr-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
}

function recordMigration(rec) {
  if (migrations.size >= 200) {
    const oldest = migrationOrder.shift();
    migrations.delete(oldest);
  }
  migrations.set(rec.id, rec);
  migrationOrder.push(rec.id);
  return rec;
}

function updateMigration(id, patch) {
  const rec = migrations.get(id);
  if (!rec) return null;
  Object.assign(rec, patch, { updatedAt: new Date().toISOString() });
  return rec;
}

// ── VibeSwap client ───────────────────────────────────────────────────────────
async function vibeSwapRequest(path, method = 'GET', body = null) {
  const url = `${VIBESWAP_URL}${path}`;
  const opts = {
    method,
    headers: {
      'Authorization': `Bearer ${VIBESWAP_TOKEN}`,
      'Content-Type': 'application/json',
    },
  };
  if (body) opts.body = JSON.stringify(body);
  const res = await fetch(url, opts);
  const data = await res.json().catch(() => ({}));
  return { ok: res.ok, status: res.status, data };
}

// ── Snapshot a running slot ───────────────────────────────────────────────────
async function snapshotSlot(slotName) {
  const r = await vibeSwapRequest(`/slots/${encodeURIComponent(slotName)}`);
  if (!r.ok) throw new Error(`VibeSwap GET /slots/${slotName} → ${r.status}: ${JSON.stringify(r.data)}`);
  const slot = r.data;
  if (!slot.loaded) throw new Error(`Slot "${slotName}" is not loaded`);

  // Pause slot (drain in-flight calls)
  const drainR = await vibeSwapRequest(`/slots/${encodeURIComponent(slotName)}/drain`, 'POST');
  if (!drainR.ok) throw new Error(`Drain failed: ${JSON.stringify(drainR.data)}`);

  // Re-fetch slot state after drain
  const r2 = await vibeSwapRequest(`/slots/${encodeURIComponent(slotName)}`);
  const slotState = r2.data;

  return {
    wasmHash:     slotState.hash,
    slotName:     slotName,
    capabilities: slotState.capabilities || [],
    callCount:    slotState.callCount     || 0,
    kvStore:      slotState.kvStore       || {},
    logs:         (slotState.logs  || []).slice(-50),
    output:       (slotState.output || []).slice(-50),
    loadedAt:     slotState.loadedAt,
    snapshotAt:   new Date().toISOString(),
    fromNode:     NODE_NAME,
  };
}

// ── Tear down a slot after successful migration ───────────────────────────────
async function teardownSlot(slotName) {
  const r = await vibeSwapRequest(`/slots/${encodeURIComponent(slotName)}`, 'DELETE');
  return r.ok || r.status === 404;
}

// ── Forward snapshot to target peer ──────────────────────────────────────────
async function sendToTarget(targetPeer, migrationId, snapshot) {
  const url = `${targetPeer}/migrate/${encodeURIComponent(migrationId)}/restore`;
  const res = await fetch(url, {
    method: 'POST',
    headers: {
      'Authorization': `Bearer ${AUTH_TOKEN}`,
      'Content-Type': 'application/json',
    },
    body: JSON.stringify({ snapshot }),
    signal: AbortSignal.timeout(30000),
  });
  if (!res.ok) {
    const body = await res.text().catch(() => '');
    throw new Error(`Target restore returned ${res.status}: ${body.slice(0, 200)}`);
  }
  return res.json().catch(() => ({ ok: true }));
}

// ── Restore a snapshot into VibeSwap on this node ────────────────────────────
async function restoreSnapshot(snapshot) {
  const { wasmHash, slotName, capabilities, callCount, kvStore, logs } = snapshot;

  // Ensure the WASM module is locally available (fetch from AgentFS if needed)
  const loadR = await vibeSwapRequest(`/slots/${encodeURIComponent(slotName)}/load`, 'POST', {
    wasmHash,
    capabilities,
    kvStore,
    callCount,
    migratedFrom: snapshot.fromNode,
    snapshotAt: snapshot.snapshotAt,
  });
  if (!loadR.ok) throw new Error(`VibeSwap restore failed: ${JSON.stringify(loadR.data)}`);
  return loadR.data;
}

// ── Publish to SquirrelBus ────────────────────────────────────────────────────
async function publishMigrateEvent(rec) {
  if (!SQUIRRELBUS) return;
  const payload = {
    type: 'agentos.migrate',
    source: NODE_NAME,
    ts: new Date().toISOString(),
    data: {
      migrationId: rec.id,
      slotName:    rec.slotName,
      fromPeer:    rec.fromPeer,
      toPeer:      rec.toPeer,
      wasmHash:    rec.wasmHash,
      durationMs:  rec.durationMs,
    },
  };
  fetch(SQUIRRELBUS, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(payload),
    signal: AbortSignal.timeout(5000),
  }).catch(e => console.warn('[migrate] SquirrelBus publish failed:', e.message));
}

// ── HTTP helpers ──────────────────────────────────────────────────────────────
function jsonReply(res, status, body) {
  const data = JSON.stringify(body);
  res.writeHead(status, {
    'Content-Type': 'application/json',
    'Access-Control-Allow-Origin': '*',
    'Content-Length': Buffer.byteLength(data),
  });
  res.end(data);
}

function readBody(req) {
  return new Promise((resolve, reject) => {
    let body = '';
    req.on('data', c => body += c);
    req.on('end', () => {
      try { resolve(body ? JSON.parse(body) : {}); }
      catch (e) { reject(Object.assign(new Error('Invalid JSON'), { statusCode: 400 })); }
    });
    req.on('error', reject);
  });
}

function isAuthed(req) {
  const auth = (req.headers['authorization'] || '').replace(/^Bearer\s+/i, '').trim();
  return auth === AUTH_TOKEN;
}

// ── Request handler ───────────────────────────────────────────────────────────
async function handleRequest(req, res) {
  const { method, url } = req;
  const path = url.split('?')[0];

  // CORS preflight
  if (method === 'OPTIONS') {
    res.writeHead(204, {
      'Access-Control-Allow-Origin': '*',
      'Access-Control-Allow-Headers': 'Authorization, Content-Type',
      'Access-Control-Allow-Methods': 'GET, POST, DELETE, OPTIONS',
    });
    return res.end();
  }

  // Health (public)
  if (method === 'GET' && path === '/migrate/health') {
    return jsonReply(res, 200, {
      ok: true,
      node: NODE_NAME,
      activeMigrations: [...migrations.values()].filter(m =>
        ['snapshotting','transferring','restoring'].includes(m.status)).length,
    });
  }

  // Auth gate
  if (!isAuthed(req)) return jsonReply(res, 401, { error: 'Unauthorized' });

  // GET /migrate — list recent
  if (method === 'GET' && path === '/migrate') {
    const limit = 20;
    const list = [...migrations.values()].slice(-limit).reverse();
    return jsonReply(res, 200, { ok: true, migrations: list, total: migrations.size });
  }

  // GET /migrate/:id
  const idMatch = path.match(/^\/migrate\/([^/]+)$/);
  if (method === 'GET' && idMatch) {
    const rec = migrations.get(idMatch[1]);
    if (!rec) return jsonReply(res, 404, { error: 'Migration not found' });
    return jsonReply(res, 200, { ok: true, migration: rec });
  }

  // POST /migrate/start — initiate migration from this node
  if (method === 'POST' && path === '/migrate/start') {
    let body;
    try { body = await readBody(req); }
    catch { return jsonReply(res, 400, { error: 'Invalid JSON' }); }

    const { slotName, targetPeer } = body;
    if (!slotName)   return jsonReply(res, 400, { error: 'slotName required' });
    if (!targetPeer) return jsonReply(res, 400, { error: 'targetPeer required (URL of target node)' });

    const migrationId = uid();
    const rec = recordMigration({
      id: migrationId, slotName,
      fromPeer: NODE_NAME, toPeer: targetPeer,
      wasmHash: null, status: 'snapshotting',
      createdAt: new Date().toISOString(), updatedAt: new Date().toISOString(),
      durationMs: null, error: null, snapshot: null,
    });

    // Reply immediately — migration runs async
    jsonReply(res, 202, { ok: true, migrationId, status: 'snapshotting' });

    // Async migration pipeline
    (async () => {
      const t0 = Date.now();
      try {
        // 1. Snapshot
        const snapshot = await snapshotSlot(slotName);
        updateMigration(migrationId, { status: 'transferring', wasmHash: snapshot.wasmHash, snapshot });

        // 2. Send to target
        await sendToTarget(targetPeer, migrationId, snapshot);
        updateMigration(migrationId, { status: 'tearing-down' });

        // 3. Tear down source
        await teardownSlot(slotName);
        const durationMs = Date.now() - t0;
        updateMigration(migrationId, { status: 'completed', durationMs, snapshot: null /* clear — target has it */ });

        // 4. Publish bus event
        await publishMigrateEvent(migrations.get(migrationId));

        console.log(`[migrate] ${migrationId}: ${slotName} → ${targetPeer} OK (${durationMs}ms)`);
      } catch (err) {
        const durationMs = Date.now() - t0;
        updateMigration(migrationId, { status: 'failed', error: err.message, durationMs });
        console.error(`[migrate] ${migrationId}: failed —`, err.message);
      }
    })();

    return; // already replied
  }

  // POST /migrate/:id/restore — called by source to restore on THIS node
  const restoreMatch = path.match(/^\/migrate\/([^/]+)\/restore$/);
  if (method === 'POST' && restoreMatch) {
    let body;
    try { body = await readBody(req); }
    catch { return jsonReply(res, 400, { error: 'Invalid JSON' }); }

    const migrationId = restoreMatch[1];
    const { snapshot } = body;
    if (!snapshot) return jsonReply(res, 400, { error: 'snapshot required' });

    // Register migration record on this node (as the receiver)
    const existing = migrations.get(migrationId);
    const rec = existing || recordMigration({
      id: migrationId,
      slotName: snapshot.slotName,
      fromPeer: snapshot.fromNode,
      toPeer: NODE_NAME,
      wasmHash: snapshot.wasmHash,
      status: 'restoring',
      createdAt: snapshot.snapshotAt,
      updatedAt: new Date().toISOString(),
      durationMs: null, error: null, snapshot: null,
    });
    if (!existing) rec.status = 'restoring';

    try {
      await restoreSnapshot(snapshot);
      const durationMs = Date.now() - new Date(snapshot.snapshotAt).getTime();
      updateMigration(migrationId, { status: 'restored', durationMs });
      console.log(`[migrate] ${migrationId}: restored slot "${snapshot.slotName}" from ${snapshot.fromNode}`);
      return jsonReply(res, 200, { ok: true, migrationId, slotName: snapshot.slotName, status: 'restored' });
    } catch (err) {
      updateMigration(migrationId, { status: 'restore-failed', error: err.message });
      console.error(`[migrate] ${migrationId}: restore failed —`, err.message);
      return jsonReply(res, 500, { error: 'restore failed', reason: err.message });
    }
  }

  return jsonReply(res, 404, { error: 'Not found' });
}

// ── Start server ──────────────────────────────────────────────────────────────
const server = createServer((req, res) => {
  handleRequest(req, res).catch(err => {
    console.error('[migrate] unhandled error:', err);
    if (!res.headersSent) jsonReply(res, 500, { error: 'Internal error', reason: err.message });
  });
});

server.listen(PORT, () => {
  console.log(`[agentOS] MigrateAgent running on http://localhost:${PORT} (node=${NODE_NAME})`);
  console.log(`[agentOS] VibeSwap: ${VIBESWAP_URL} | AgentFS: ${AGENTFS_URL}`);
  console.log(`[agentOS] SquirrelBus: ${SQUIRRELBUS || 'disabled'}`);
});
