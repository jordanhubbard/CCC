/*
 * offline-heartbeat/server.mjs — Local SQLite heartbeat fallback server
 *
 * When CCC API (146.190.134.110:8789) is unreachable, agents POST heartbeats
 * here instead.  The dashboard reads GET /local/heartbeat to distinguish
 * "network partition" from "agent truly dead".  On reconnect, buffered
 * heartbeats are replayed to the CCC API.
 *
 * Start: node offline-heartbeat/server.mjs
 * Port:  8792 (HB_PORT env to override)
 *
 * Endpoints:
 *   POST /local/heartbeat        store offline heartbeat
 *   GET  /local/heartbeat        agent liveness summary
 *   GET  /local/heartbeat/stream SSE stream of new events
 *   POST /local/heartbeat/replay replay buffered heartbeats to CCC
 *   GET  /local/status           server + DB status
 *
 * Copyright 2026 agentOS Project (BSD-2-Clause)
 */

import http from 'node:http';
import { createRequire } from 'node:module';
import { randomUUID } from 'node:crypto';
import { existsSync, mkdirSync, statSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const require   = createRequire(import.meta.url);

const PORT      = parseInt(process.env.HB_PORT   || '8792', 10);
const CCC_URL   = process.env.CCC_URL            || 'http://146.190.134.110:8789';
const RCC_TOKEN = process.env.RCC_TOKEN          || 'wq-5dcad756f6d3e345c00b5cb3dfcbdedb';
const DB_DIR    = process.env.HB_DB_DIR          || path.join(__dirname, '../data');
const DB_PATH   = path.join(DB_DIR, 'heartbeat-offline.sqlite');

if (!existsSync(DB_DIR)) mkdirSync(DB_DIR, { recursive: true });

// ── SQLite (better-sqlite3 if available, else in-memory fallback) ──────────

let db = null;
const _mem = [];

try {
  const Database = require('better-sqlite3');
  db = new Database(DB_PATH);
  db.pragma('journal_mode = WAL');
  db.exec(`
    CREATE TABLE IF NOT EXISTS heartbeats (
      id TEXT PRIMARY KEY, agent TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'online',
      host TEXT, ts TEXT NOT NULL, extra TEXT, replayed_at TEXT
    );
    CREATE INDEX IF NOT EXISTS idx_agent ON heartbeats(agent);
    CREATE INDEX IF NOT EXISTS idx_ts    ON heartbeats(ts);
  `);
  console.log(`[offline-hb] SQLite: ${DB_PATH}`);
} catch (e) {
  console.warn(`[offline-hb] no better-sqlite3 (${e.message}) — using in-memory store`);
}

function dbInsert(r) {
  if (db) {
    db.prepare('INSERT OR REPLACE INTO heartbeats VALUES(?,?,?,?,?,?,?)')
      .run(r.id, r.agent, r.status, r.host, r.ts, r.extra || null, null);
  } else { _mem.push(r); if (_mem.length > 10000) _mem.shift(); }
}

function dbLatestPerAgent() {
  if (db) {
    return db.prepare(`SELECT agent, MAX(ts) last_ts, status, host,
      COUNT(*) count, SUM(CASE WHEN replayed_at IS NULL THEN 1 ELSE 0 END) pending
      FROM heartbeats GROUP BY agent ORDER BY last_ts DESC`).all();
  }
  const m = {};
  for (const r of _mem) {
    if (!m[r.agent] || r.ts > m[r.agent].last_ts)
      m[r.agent] = { agent: r.agent, last_ts: r.ts, status: r.status, host: r.host, count: 0, pending: 0 };
    m[r.agent].count++;
    if (!r.replayed_at) m[r.agent].pending++;
  }
  return Object.values(m);
}

function dbUnreplayed(n = 500) {
  return db ? db.prepare('SELECT * FROM heartbeats WHERE replayed_at IS NULL ORDER BY ts LIMIT ?').all(n)
            : _mem.filter(r => !r.replayed_at).slice(0, n);
}

function dbMarkReplayed(ids) {
  const ts = new Date().toISOString();
  if (db) { const s = db.prepare('UPDATE heartbeats SET replayed_at=? WHERE id=?'); for (const id of ids) s.run(ts, id); }
  else for (const r of _mem) if (ids.includes(r.id)) r.replayed_at = ts;
}

function dbCount() {
  return db ? db.prepare('SELECT COUNT(*) n FROM heartbeats').get().n : _mem.length;
}

// ── CCC reachability probe ──────────────────────────────────────────────────

let rccReachable = null, offlineSince = null;

async function checkRcc() {
  try {
    const ac  = new AbortController();
    const tid = setTimeout(() => ac.abort(), 4000);
    const r   = await fetch(`${CCC_URL}/health`, { signal: ac.signal });
    clearTimeout(tid);
    if (!rccReachable && offlineSince) {
      console.log(`[offline-hb] CCC back online after ${offlineSince}`);
      offlineSince = null;
    }
    rccReachable = r.ok;
  } catch {
    if (rccReachable !== false) {
      offlineSince = new Date().toISOString();
      console.log(`[offline-hb] CCC unreachable — offline mode since ${offlineSince}`);
    }
    rccReachable = false;
  }
}
checkRcc();
setInterval(checkRcc, 30_000);

// ── SSE clients ─────────────────────────────────────────────────────────────

const sseClients = new Set();
function broadcast(evt) {
  const d = JSON.stringify(evt);
  for (const c of sseClients) { try { c.write(`data: ${d}\n\n`); } catch { sseClients.delete(c); } }
}

// ── Replay ──────────────────────────────────────────────────────────────────

async function replayToRcc(url, token) {
  const rows = dbUnreplayed(500);
  let ok = 0, fail = 0;
  for (const r of rows) {
    try {
      const ac = new AbortController();
      setTimeout(() => ac.abort(), 5000);
      const res = await fetch(`${url}/api/heartbeat/${r.agent}`, {
        method: 'POST', signal: ac.signal,
        headers: { 'Content-Type': 'application/json', 'Authorization': `Bearer ${token}` },
        body: JSON.stringify({ status: r.status, host: r.host, ts: r.ts, replayed: true }),
      });
      if (res.ok) { dbMarkReplayed([r.id]); ok++; } else fail++;
    } catch { fail++; }
  }
  return { replayed: ok, failed: fail, total: rows.length };
}

// ── HTTP helpers ─────────────────────────────────────────────────────────────

function resp(res, code, body) {
  res.writeHead(code, { 'Content-Type': 'application/json', 'Access-Control-Allow-Origin': '*' });
  res.end(JSON.stringify(body));
}
async function readBody(req) {
  return new Promise(resolve => {
    let b = '';
    req.on('data', c => { b += c; });
    req.on('end', () => { try { resolve(JSON.parse(b)); } catch { resolve({}); } });
  });
}

// ── Server ───────────────────────────────────────────────────────────────────

const startTime = Date.now();
http.createServer(async (req, res) => {
  const url    = new URL(req.url, `http://localhost`);
  const p      = url.pathname;
  const method = req.method;

  if (method === 'OPTIONS') { res.writeHead(204, { 'Access-Control-Allow-Origin': '*', 'Access-Control-Allow-Methods': 'GET,POST', 'Access-Control-Allow-Headers': 'Content-Type,Authorization' }); return res.end(); }

  if (method === 'POST' && p === '/local/heartbeat') {
    const body = await readBody(req);
    const r = { id: randomUUID(), agent: body.agent||'unknown', status: body.status||'online', host: body.host||null, ts: body.ts||new Date().toISOString(), extra: body.extra?JSON.stringify(body.extra):null };
    dbInsert(r);
    broadcast({ type: 'heartbeat', ...r });
    console.log(`[offline-hb] ${r.agent} @ ${r.ts}`);
    return resp(res, 200, { ok: true, id: r.id, offline: true });
  }

  if (method === 'GET' && p === '/local/heartbeat') {
    const now = Date.now();
    return resp(res, 200, {
      ok: true, offline_mode: !rccReachable, offline_since: offlineSince, rcc_url: CCC_URL,
      agents: dbLatestPerAgent().map(a => ({ ...a, online: (now - new Date(a.last_ts).getTime()) < 120_000 })),
    });
  }

  if (method === 'GET' && p === '/local/heartbeat/stream') {
    res.writeHead(200, { 'Content-Type': 'text/event-stream', 'Cache-Control': 'no-cache', 'Connection': 'keep-alive', 'Access-Control-Allow-Origin': '*' });
    res.flushHeaders?.();
    res.write(`data: ${JSON.stringify({ type: 'connected', rcc_reachable: rccReachable })}\n\n`);
    sseClients.add(res);
    const ka = setInterval(() => res.write(': ping\n\n'), 20_000);
    req.on('close', () => { sseClients.delete(res); clearInterval(ka); });
    return;
  }

  if (method === 'POST' && p === '/local/heartbeat/replay') {
    const body = await readBody(req);
    const result = await replayToRcc(body.rcc_url||CCC_URL, body.token||RCC_TOKEN);
    return resp(res, 200, { ok: true, ...result });
  }

  if (method === 'GET' && p === '/local/status') {
    let sz = 0; try { sz = statSync(DB_PATH).size; } catch {}
    return resp(res, 200, { ok: true, db_path: DB_PATH, db_size_bytes: sz, record_count: dbCount(), rcc_reachable: rccReachable, offline_since: offlineSince, port: PORT, uptime_s: Math.floor((Date.now()-startTime)/1000) });
  }

  res.writeHead(404); res.end('Not found');
}).listen(PORT, '0.0.0.0', () => console.log(`[offline-hb] :${PORT} | DB: ${DB_PATH} | CCC: ${CCC_URL}`));
