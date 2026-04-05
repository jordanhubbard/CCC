/**
 * Tests for rcc/api
 * Run: node --test rcc/api/test.mjs
 * Starts a test server on a random port, runs tests, shuts down.
 */

import { test, describe, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'http';
import { writeFile, unlink } from 'fs/promises';
import { tmpdir } from 'os';
import { join } from 'path';

// ── Test server setup ──────────────────────────────────────────────────────

// We import the handler directly to avoid port conflicts
// Patch env before import
const TEST_PORT = 18900 + Math.floor(Math.random() * 100);
const TEST_QUEUE = join(tmpdir(), `rcc-test-queue-${Date.now()}.json`);
const TEST_AGENTS = join(tmpdir(), `rcc-test-agents-${Date.now()}.json`);

process.env.RCC_PORT = String(TEST_PORT);
process.env.QUEUE_PATH = TEST_QUEUE;
process.env.AGENTS_PATH = TEST_AGENTS;
process.env.CCC_AUTH_TOKENS = 'test-token-abc';
process.env.BRAIN_STATE_PATH = join(tmpdir(), `rcc-brain-${Date.now()}.json`);

// Write initial empty queue and agents
await writeFile(TEST_QUEUE, JSON.stringify({ items: [], completed: [] }, null, 2));
await writeFile(TEST_AGENTS, JSON.stringify({}, null, 2));

const { startServer } = await import('./index.mjs');

let server;
let BASE;

before(async () => {
  server = startServer(TEST_PORT);
  await new Promise(r => server.on('listening', r));
  BASE = `http://localhost:${TEST_PORT}`;
});

after(async () => {
  server.close();
  await unlink(TEST_QUEUE).catch(() => {});
  await unlink(TEST_AGENTS).catch(() => {});
});

// ── Helpers ────────────────────────────────────────────────────────────────
const AUTH = 'Bearer test-token-abc';
const BAD  = 'Bearer wrong';

async function req(path, opts = {}) {
  const res = await fetch(BASE + path, opts);
  const body = await res.json().catch(() => ({}));
  return { status: res.status, body };
}

async function authed(path, method = 'GET', body) {
  const opts = { method, headers: { Authorization: AUTH, 'Content-Type': 'application/json' } };
  if (body) opts.body = JSON.stringify(body);
  return req(path, opts);
}

// ── Tests ──────────────────────────────────────────────────────────────────

describe('GET /health', () => {
  test('returns ok', async () => {
    const r = await req('/health');
    assert.equal(r.status, 200);
    assert.ok(r.body.ok);
    assert.equal(typeof r.body.uptime, 'number');
    assert.equal(typeof r.body.queueDepth, 'number');
  });
});

describe('GET /api/queue', () => {
  test('returns empty queue initially', async () => {
    const r = await req('/api/queue');
    assert.equal(r.status, 200);
    assert.ok(Array.isArray(r.body.items));
    assert.ok(Array.isArray(r.body.completed));
  });
});

let createdItemId;

describe('POST /api/queue', () => {
  test('requires auth', async () => {
    const r = await req('/api/queue', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ title: 'test' }) });
    assert.equal(r.status, 401);
  });

  test('rejects missing title', async () => {
    const r = await authed('/api/queue', 'POST', { description: 'no title' });
    assert.equal(r.status, 400);
  });

  test('creates item', async () => {
    const r = await authed('/api/queue', 'POST', {
      title: '[TEST] API test item',
      assignee: 'rocky',
      priority: 'high',
    });
    assert.equal(r.status, 201);
    assert.ok(r.body.ok);
    assert.ok(r.body.item.id);
    createdItemId = r.body.item.id;
    assert.equal(r.body.item.status, 'pending');
    assert.equal(r.body.item.priority, 'high');
  });

  test('item appears in queue', async () => {
    const r = await req('/api/queue');
    const found = r.body.items.find(i => i.id === createdItemId);
    assert.ok(found, 'created item should appear in queue');
  });
});

describe('GET /api/item/:id', () => {
  test('returns item detail', async () => {
    const r = await req(`/api/item/${createdItemId}`);
    assert.equal(r.status, 200);
    assert.equal(r.body.id, createdItemId);
  });

  test('404 for unknown', async () => {
    const r = await req('/api/item/wq-nonexistent-xyz');
    assert.equal(r.status, 404);
  });
});

describe('PATCH /api/item/:id', () => {
  test('requires auth', async () => {
    const r = await req(`/api/item/${createdItemId}`, { method: 'PATCH', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ priority: 'low' }) });
    assert.equal(r.status, 401);
  });

  test('updates fields and journals change', async () => {
    const r = await authed(`/api/item/${createdItemId}`, 'PATCH', { notes: 'Updated via test', _author: 'test-runner' });
    assert.equal(r.status, 200);
    assert.equal(r.body.item.notes, 'Updated via test');
    const journal = r.body.item.journal || [];
    assert.ok(journal.some(e => e.type === 'status-change'), 'should have journal entry');
  });
});

describe('POST /api/item/:id/comment', () => {
  test('requires auth', async () => {
    const r = await req(`/api/item/${createdItemId}/comment`, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ text: 'sneaky' }) });
    assert.equal(r.status, 401);
  });

  test('adds comment to journal', async () => {
    const r = await authed(`/api/item/${createdItemId}/comment`, 'POST', { text: 'Test comment', author: 'test-runner' });
    assert.equal(r.status, 200);
    assert.equal(r.body.entry.type, 'comment');
    assert.equal(r.body.entry.author, 'test-runner');
  });
});

describe('POST /api/item/:id/choice', () => {
  test('setup: add choices', async () => {
    await authed(`/api/item/${createdItemId}`, 'PATCH', {
      choices: [{ id: 'A', label: 'Option A' }, { id: 'B', label: 'Option B' }],
    });
  });

  test('records choice', async () => {
    const r = await authed(`/api/item/${createdItemId}/choice`, 'POST', { choice: 'A', choiceLabel: 'Option A' });
    assert.equal(r.status, 200);
    assert.equal(r.body.choiceRecorded.choice, 'A');
  });

  test('choice visible on item', async () => {
    const r = await req(`/api/item/${createdItemId}`);
    assert.ok(r.body.choiceRecorded);
    assert.equal(r.body.choiceRecorded.choice, 'A');
  });
});

describe('POST /api/agents/register', () => {
  test('requires auth', async () => {
    const r = await req('/api/agents/register', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ name: 'test-agent' }) });
    assert.equal(r.status, 401);
  });

  test('registers agent and returns token', async () => {
    const r = await authed('/api/agents/register', 'POST', { name: 'test-agent', host: 'test-host', type: 'full' });
    assert.equal(r.status, 201);
    assert.ok(r.body.token);
    assert.ok(r.body.token.startsWith('rcc-agent-test-agent-'));
  });

  test('registered agent appears in /api/agents', async () => {
    const r = await req('/api/agents');
    assert.equal(r.status, 200);
    const found = r.body.find(a => a.name === 'test-agent');
    assert.ok(found, 'test-agent should appear');
    assert.equal(found.host, 'test-host');
  });
});

describe('POST /api/heartbeat/:agent', () => {
  test('requires auth', async () => {
    const r = await req('/api/heartbeat/test-agent', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: '{}' });
    assert.equal(r.status, 401);
  });

  test('posts heartbeat', async () => {
    const r = await authed('/api/heartbeat/test-agent', 'POST', { host: 'test-host', status: 'online' });
    assert.equal(r.status, 200);
    assert.ok(r.body.ok);
  });

  test('heartbeat visible in /api/heartbeats', async () => {
    const r = await req('/api/heartbeats');
    assert.ok(r.body['test-agent'], 'test-agent heartbeat should appear');
  });
});

describe('POST /api/complete/:id', () => {
  test('marks item complete', async () => {
    const r = await authed(`/api/complete/${createdItemId}`, 'POST');
    assert.equal(r.status, 200);
    assert.equal(r.body.item.status, 'completed');
  });
});

describe('GET /api/brain/status', () => {
  test('returns brain status (auth required)', async () => {
    const r = await authed('/api/brain/status');
    assert.equal(r.status, 200);
    assert.ok(r.body.ok !== undefined || r.body.status !== undefined);
  });
});
