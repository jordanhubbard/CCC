// rcc/agent/offline-heartbeat.mjs
// Offline heartbeat fallback: writes to local SQLite when RCC API is unreachable,
// replays buffered heartbeats when connectivity is restored.

import Database from 'better-sqlite3';
import { homedir } from 'os';
import { mkdirSync } from 'fs';
import { join } from 'path';

const DB_DIR = join(homedir(), '.rcc');
const DB_PATH = join(DB_DIR, 'offline-heartbeats.db');
const MAX_ROWS = 100;
const HEARTBEAT_TIMEOUT_MS = 5000;

function getDb() {
  mkdirSync(DB_DIR, { recursive: true });
  const db = new Database(DB_PATH);
  db.exec(`
    CREATE TABLE IF NOT EXISTS heartbeats (
      id INTEGER PRIMARY KEY AUTOINCREMENT,
      agent_name TEXT NOT NULL,
      host TEXT NOT NULL,
      ts TEXT NOT NULL,
      status TEXT NOT NULL,
      replayed INTEGER DEFAULT 0,
      created_at TEXT DEFAULT (datetime('now'))
    );
  `);
  return db;
}

async function postHeartbeat(rccUrl, agentName, host, status) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), HEARTBEAT_TIMEOUT_MS);
  try {
    const res = await fetch(`${rccUrl}/api/heartbeat/${encodeURIComponent(agentName)}`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ host, status, ts: new Date().toISOString() }),
      signal: controller.signal,
    });
    return res.ok;
  } catch {
    return false;
  } finally {
    clearTimeout(timer);
  }
}

async function replayBuffered(db, rccUrl) {
  const pending = db.prepare('SELECT * FROM heartbeats WHERE replayed = 0 ORDER BY id ASC').all();
  for (const row of pending) {
    const ok = await postHeartbeat(rccUrl, row.agent_name, row.host, row.status);
    if (ok) {
      db.prepare('UPDATE heartbeats SET replayed = 1 WHERE id = ?').run(row.id);
    } else {
      break; // stop replaying if still offline
    }
  }
}

function pruneOldRows(db) {
  db.prepare(`
    DELETE FROM heartbeats WHERE id NOT IN (
      SELECT id FROM heartbeats ORDER BY id DESC LIMIT ?
    )
  `).run(MAX_ROWS);
}

export async function sendHeartbeat(rccUrl, agentName, host, status) {
  const ts = new Date().toISOString();
  const online = await postHeartbeat(rccUrl, agentName, host, status);
  const db = getDb();
  try {
    if (online) {
      await replayBuffered(db, rccUrl);
    } else {
      db.prepare('INSERT INTO heartbeats (agent_name, host, ts, status) VALUES (?, ?, ?, ?)').run(agentName, host, ts, status);
      pruneOldRows(db);
    }
  } finally {
    db.close();
  }
  return online;
}

export function getLocalHeartbeats(limit = 10) {
  const db = getDb();
  try {
    return db.prepare('SELECT * FROM heartbeats ORDER BY id DESC LIMIT ?').all(limit);
  } finally {
    db.close();
  }
}
