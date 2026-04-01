// Test 1: RCC unreachable → verify SQLite write
// Test 2: RCC returns → verify replay + mark replayed
// Test 3: Verify pruning at 100 rows

import assert from 'node:assert';
import { sendHeartbeat, getLocalHeartbeats } from './offline-heartbeat.mjs';
import Database from 'better-sqlite3';
import { homedir } from 'os';
import { join } from 'path';

const DB_PATH = join(homedir(), '.rcc', 'offline-heartbeats.db');

// Helper: clear table before each test — ensures schema exists first
function clearTable() {
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
  db.exec('DELETE FROM heartbeats');
  db.close();
}

// Test 1: offline → SQLite write
{
  clearTable();
  const ok = await sendHeartbeat('http://127.0.0.1:19999', 'test-agent', 'localhost', 'running');
  assert.strictEqual(ok, false, 'Should return false when offline');
  const rows = getLocalHeartbeats(10);
  assert.ok(rows.length > 0, 'Should have written a row to SQLite');
  assert.strictEqual(rows[0].agent_name, 'test-agent');
  assert.strictEqual(rows[0].replayed, 0);
  console.log('PASS: Test 1 — offline writes to SQLite');
}

// Test 2: replay on reconnect — use a mock HTTP server
import { createServer } from 'http';

await new Promise((resolve) => {
  clearTable();

  // Pre-insert a pending heartbeat
  const db = new Database(DB_PATH);
  db.prepare("INSERT INTO heartbeats (agent_name, host, ts, status) VALUES ('test-agent', 'localhost', datetime('now'), 'running')").run();
  db.close();

  const server = createServer((req, res) => {
    res.writeHead(200, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify({ ok: true }));
  });

  server.listen(19888, async () => {
    const ok = await sendHeartbeat('http://127.0.0.1:19888', 'test-agent', 'localhost', 'running');
    assert.strictEqual(ok, true, 'Should return true when online');

    const db2 = new Database(DB_PATH);
    const pending = db2.prepare('SELECT * FROM heartbeats WHERE replayed = 0').all();
    db2.close();
    assert.strictEqual(pending.length, 0, 'All buffered heartbeats should be replayed');
    console.log('PASS: Test 2 — replay on reconnect');
    server.close(resolve);
  });
});

// Test 3: pruning at 100 rows
{
  clearTable();
  const db = new Database(DB_PATH);
  for (let i = 0; i < 110; i++) {
    db.prepare("INSERT INTO heartbeats (agent_name, host, ts, status) VALUES ('prune-test', 'localhost', datetime('now'), 'running')").run();
  }
  db.close();

  // trigger a heartbeat that will try offline (use a definitely-unreachable port)
  await sendHeartbeat('http://127.0.0.1:19999', 'prune-test', 'localhost', 'running');

  const db2 = new Database(DB_PATH);
  const count = db2.prepare("SELECT COUNT(*) as cnt FROM heartbeats").get().cnt;
  db2.close();
  assert.ok(count <= 100, `Should prune to max 100 rows, got ${count}`);
  console.log(`PASS: Test 3 — pruning (${count} rows <= 100)`);
}

console.log('All tests passed!');
