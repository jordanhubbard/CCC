/**
 * heartbeat-local — offline heartbeat buffer for sparky
 *
 * When CCC (do-host1:8789) is unreachable, agents write here instead.
 * Exposes GET /local/heartbeat so the WASM dashboard can distinguish
 * network partition from true agent death.
 * On reconnect, replays buffered heartbeats to CCC.
 *
 * Usage: node server.mjs [--port 8790] [--rcc http://146.190.134.110:8789]
 * Systemd: see deploy/heartbeat-local.service
 */

import http from 'node:http';
import { createRequire } from 'node:module';
import { existsSync, mkdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { sendHeartbeat } from '../agent/offline-heartbeat.mjs';

const __dir = dirname(fileURLToPath(import.meta.url));

// ── Config ────────────────────────────────────────────────────────────────────
const PORT     = parseInt(process.env.HB_LOCAL_PORT   || '8790', 10);
const CCC_URL  = process.env.CCC_URL                  || 'http://146.190.134.110:8789';
const RCC_TOKEN= process.env.CCC_AUTH_TOKEN           || 'wq-5dcad756f6d3e345c00b5cb3dfcbdedb';
const DB_PATH  = process.env.HB_DB_PATH               || join(__dir, 'heartbeats.db');

// ── SQLite (better-sqlite3 if available, else JSON file fallback) ─────────────
let db = null;
try {
  const require = createRequire(import.meta.url);
  const Database = require('better-sqlite3');
  db = new Database(DB_PATH);
  db.exec(`
    CREATE TABLE IF NOT EXISTS heartbeats (
      agent     TEXT NOT NULL,
      ts        TEXT NOT NULL,
      host      TEXT,
      status    TEXT DEFAULT 'online',
      replayed  INTEGER DEFAULT 0,
      PRIMARY KEY (agent, ts)
    );
    CREATE TABLE IF NOT EXISTS state (
      key   TEXT PRIMARY KEY,
      value TEXT
    );
  `);
  console.log('[hb-local] SQLite backend:', DB_PATH);
} catch {
  console.log('[hb-local] better-sqlite3 unavailable — using in-memory store');
}

// ── In-memory fallback ────────────────────────────────────────────────────────
const memStore = {};  // agent → [{ts, host, status, replayed}]

function insertHb(agent, ts, host, status = 'online') {
  if (db) {
    db.prepare(
      'INSERT OR IGNORE INTO heartbeats (agent, ts, host, status) VALUES (?,?,?,?)'
    ).run(agent, ts, host || null, status);
  } else {
    if (!memStore[agent]) memStore[agent] = [];
    memStore[agent].push({ ts, host, status, replayed: 0 });
    if (memStore[agent].length > 500) memStore[agent].shift();
  }
}

function getAgents() {
  if (db) {
    return db.prepare(`
      SELECT agent, MAX(ts) as last_ts, host, status
      FROM heartbeats GROUP BY agent ORDER BY last_ts DESC
    `).all();
  }
  return Object.entries(memStore).map(([agent, entries]) => {
    const last = entries[entries.length - 1];
    return { agent, last_ts: last.ts, host: last.host, status: last.status };
  });
}

function getUnreplayed() {
  if (db) {
    return db.prepare(
      'SELECT agent, ts, host, status FROM heartbeats WHERE replayed=0 ORDER BY ts ASC LIMIT 100'
    ).all();
  }
  return Object.entries(memStore).flatMap(([agent, entries]) =>
    entries.filter(e => !e.replayed).map(e => ({ agent, ...e }))
  );
}

function markReplayed(agent, ts) {
  if (db) {
    db.prepare('UPDATE heartbeats SET replayed=1 WHERE agent=? AND ts=?').run(agent, ts);
  } else {
    const entries = memStore[agent] || [];
    const e = entries.find(x => x.ts === ts);
    if (e) e.replayed = 1;
  }
}

function setState(key, value) {
  if (db) {
    db.prepare('INSERT OR REPLACE INTO state (key,value) VALUES (?,?)').run(key, String(value));
  }
}

function getState(key) {
  if (!db) return null;
  const row = db.prepare('SELECT value FROM state WHERE key=?').get(key);
  return row ? row.value : null;
}

// ── CCC reachability + replay ─────────────────────────────────────────────────
let rccReachable = false;
let lastRccCheck = 0;

async function checkRcc() {
  try {
    const ctrl = new AbortController();
    const t = setTimeout(() => ctrl.abort(), 3000);
    const r = await fetch(`${CCC_URL}/health`, { signal: ctrl.signal });
    clearTimeout(t);
    rccReachable = r.ok;
  } catch {
    rccReachable = false;
  }
  lastRccCheck = Date.now();
  setState('rcc_reachable', rccReachable ? '1' : '0');
  setState('rcc_last_check', new Date().toISOString());
}

async function replayToRcc() {
  if (!rccReachable) return;
  const pending = getUnreplayed();
  for (const hb of pending) {
    try {
      const r = await fetch(`${CCC_URL}/api/heartbeat/${encodeURIComponent(hb.agent)}`, {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          'Authorization': `Bearer ${RCC_TOKEN}`,
        },
        body: JSON.stringify({ status: hb.status, host: hb.host, ts: hb.ts, _replayed: true }),
        signal: AbortSignal.timeout(4000),
      });
      if (r.ok) {
        markReplayed(hb.agent, hb.ts);
        console.log(`[hb-local] replayed ${hb.agent}@${hb.ts} → CCC`);
      }
    } catch {
      // CCC went away mid-replay — stop, try next cycle
      break;
    }
  }
}

// Check CCC every 30s, replay when reachable
setInterval(async () => {
  await checkRcc();
  if (rccReachable) await replayToRcc();
}, 30_000);
// Initial check
checkRcc();

// ── HTTP server ───────────────────────────────────────────────────────────────
function readBody(req) {
  return new Promise(resolve => {
    let body = '';
    req.on('data', c => { body += c; });
    req.on('end', () => {
      try { resolve(JSON.parse(body)); } catch { resolve({}); }
    });
  });
}

function json(res, status, obj) {
  const buf = JSON.stringify(obj);
  res.writeHead(status, { 'Content-Type': 'application/json', 'Content-Length': Buffer.byteLength(buf) });
  res.end(buf);
}

const server = http.createServer(async (req, res) => {
  res.setHeader('Access-Control-Allow-Origin', '*');
  if (req.method === 'OPTIONS') { res.writeHead(204); return res.end(); }

  const url = new URL(req.url, `http://localhost:${PORT}`);
  const path = url.pathname;
  const method = req.method;

  // GET /local/heartbeat — all agents, local status
  if (method === 'GET' && path === '/local/heartbeat') {
    const agents = getAgents();
    const now = Date.now();
    const enriched = agents.map(a => ({
      ...a,
      age_s: a.last_ts ? Math.round((now - new Date(a.last_ts).getTime()) / 1000) : null,
    }));
    return json(res, 200, {
      ok: true,
      rcc_reachable: rccReachable,
      rcc_last_check: getState('rcc_last_check'),
      agents: enriched,
    });
  }

  // POST /api/heartbeat/:agent — drop-in replacement for CCC endpoint
  const hbMatch = path.match(/^\/api\/heartbeat\/([^/]+)$/);
  if (method === 'POST' && hbMatch) {
    const agent = decodeURIComponent(hbMatch[1]).toLowerCase();
    const body = await readBody(req);
    const ts = body.ts || new Date().toISOString();
    const host = body.host || 'unknown';
    const status = body.status || 'online';
    insertHb(agent, ts, host, status);
    console.log(`[hb-local] ${agent}@${ts} (rcc_reachable=${rccReachable})`);

    // Use sendHeartbeat for store-and-forward: buffers to SQLite when offline,
    // replays buffered rows when CCC is reachable again.
    sendHeartbeat(CCC_URL, agent, host, status).then(online => {
      if (online) markReplayed(agent, ts);
    }).catch(() => {});

    return json(res, 200, { ok: true, buffered: !rccReachable });
  }

  // GET /health
  if (method === 'GET' && path === '/health') {
    return json(res, 200, { ok: true, rcc_reachable: rccReachable });
  }

  json(res, 404, { error: 'not found' });
});

server.listen(PORT, '0.0.0.0', () => {
  console.log(`[hb-local] listening on :${PORT}`);
  console.log(`[hb-local] CCC=${CCC_URL}`);
});
