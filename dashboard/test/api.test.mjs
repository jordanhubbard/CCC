/**
 * WQ Dashboard API Tests
 * Run: node --test dashboard/test/api.test.mjs
 * Requires dashboard to be running on port 8788
 */

import { test, describe } from 'node:test';
import assert from 'node:assert/strict';

const BASE = 'http://localhost:8788';
const AUTH = 'Bearer test-token';
const BAD_AUTH = 'Bearer wrong-token';

async function api(path, opts = {}) {
  const res = await fetch(BASE + path, opts);
  const body = await res.json().catch(() => ({}));
  return { status: res.status, body };
}

async function authed(path, method = 'GET', body) {
  const opts = {
    method,
    headers: { Authorization: AUTH, 'Content-Type': 'application/json' },
  };
  if (body) opts.body = JSON.stringify(body);
  return api(path, opts);
}

// ── Create a test item ──────────────────────────────────────────────────────
let testItemId;

describe('POST /api/queue — create item', () => {
  test('requires auth', async () => {
    const r = await api('/api/queue', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ title: 'test' }) });
    assert.equal(r.status, 401);
  });

  test('rejects missing title', async () => {
    const r = await authed('/api/queue', 'POST', { description: 'no title' });
    assert.equal(r.status, 400);
  });

  test('creates item with defaults', async () => {
    const r = await authed('/api/queue', 'POST', {
      title: '[TEST] Dashboard API test item',
      description: 'Created by automated test',
      assignee: 'rocky',
      priority: 'low',
    });
    assert.equal(r.status, 201);
    assert.ok(r.body.ok);
    assert.ok(r.body.item.id);
    testItemId = r.body.item.id;
    assert.equal(r.body.item.status, 'pending');
    assert.equal(r.body.item.assignee, 'rocky');
  });
});

// ── GET /api/queue ──────────────────────────────────────────────────────────
describe('GET /api/queue', () => {
  test('returns items array', async () => {
    const r = await api('/api/queue');
    assert.equal(r.status, 200);
    assert.ok(Array.isArray(r.body.items));
    assert.ok(r.body.items.length > 0);
  });

  test('includes our test item', async () => {
    const r = await api('/api/queue');
    const found = r.body.items.find(i => i.id === testItemId);
    assert.ok(found, 'test item should be in queue');
  });
});

// ── GET /api/item/:id ───────────────────────────────────────────────────────
describe('GET /api/item/:id', () => {
  test('returns full item detail', async () => {
    const r = await api('/api/item/' + testItemId);
    assert.equal(r.status, 200);
    assert.equal(r.body.id, testItemId);
  });

  test('404 for unknown id', async () => {
    const r = await api('/api/item/wq-nonexistent-xyz');
    assert.equal(r.status, 404);
  });
});

// ── PATCH /api/item/:id ─────────────────────────────────────────────────────
describe('PATCH /api/item/:id', () => {
  test('requires auth', async () => {
    const r = await api('/api/item/' + testItemId, {
      method: 'PATCH',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ priority: 'high' }),
    });
    assert.equal(r.status, 401);
  });

  test('updates allowed fields', async () => {
    const r = await authed('/api/item/' + testItemId, 'PATCH', {
      priority: 'normal',
      notes: 'Updated by test',
    });
    assert.equal(r.status, 200);
    assert.ok(r.body.ok);
    assert.equal(r.body.item.priority, 'normal');
  });

  test('records change in journal', async () => {
    const r = await api('/api/item/' + testItemId);
    assert.ok(r.body.journal && r.body.journal.length > 0);
    const entry = r.body.journal.find(e => e.type === 'status-change');
    assert.ok(entry, 'journal should have a status-change entry');
  });
});

// ── POST /api/item/:id/comment ──────────────────────────────────────────────
describe('POST /api/item/:id/comment', () => {
  test('requires auth', async () => {
    const r = await api('/api/item/' + testItemId + '/comment', {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ text: 'sneaky comment' }),
    });
    assert.equal(r.status, 401);
  });

  test('rejects empty text', async () => {
    const r = await authed('/api/item/' + testItemId + '/comment', 'POST', { text: '' });
    assert.equal(r.status, 400);
  });

  test('appends comment to journal', async () => {
    const r = await authed('/api/item/' + testItemId + '/comment', 'POST', {
      text: 'Test comment from automated test',
      author: 'test-runner',
    });
    assert.equal(r.status, 200);
    assert.ok(r.body.ok);
    assert.equal(r.body.entry.author, 'test-runner');
    assert.equal(r.body.entry.type, 'comment');
  });

  test('journal entry visible on item fetch', async () => {
    const r = await api('/api/item/' + testItemId);
    const comment = r.body.journal.find(e => e.type === 'comment' && e.author === 'test-runner');
    assert.ok(comment, 'comment should appear in journal');
  });
});

// ── POST /api/item/:id/choice ───────────────────────────────────────────────
describe('POST /api/item/:id/choice', () => {
  // First add choices to our test item
  test('setup: add choices via PATCH', async () => {
    const r = await authed('/api/item/' + testItemId, 'PATCH', {
      choices: [
        { id: 'X', label: 'Option X' },
        { id: 'Y', label: 'Option Y' },
      ],
    });
    assert.equal(r.status, 200);
  });

  test('requires auth', async () => {
    const r = await api('/api/item/' + testItemId + '/choice', {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ choice: 'X' }),
    });
    assert.equal(r.status, 401);
  });

  test('records choice and journal entry', async () => {
    const r = await authed('/api/item/' + testItemId + '/choice', 'POST', {
      choice: 'X',
      choiceLabel: 'Option X',
    });
    assert.equal(r.status, 200);
    assert.ok(r.body.ok);
    assert.equal(r.body.choiceRecorded.choice, 'X');
  });

  test('choiceRecorded visible on item', async () => {
    const r = await api('/api/item/' + testItemId);
    assert.ok(r.body.choiceRecorded);
    assert.equal(r.body.choiceRecorded.choice, 'X');
  });
});

// ── POST /api/heartbeat/:agent ──────────────────────────────────────────────
describe('POST /api/heartbeat/:agent', () => {
  test('requires auth', async () => {
    const r = await api('/api/heartbeat/test-agent', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: '{}' });
    assert.equal(r.status, 401);
  });

  test('registers heartbeat', async () => {
    const r = await authed('/api/heartbeat/test-agent', 'POST', { host: 'test-host', status: 'online' });
    assert.equal(r.status, 200);
    assert.ok(r.body.ok);
  });

  test('appears in /api/heartbeats', async () => {
    const r = await api('/api/heartbeats');
    assert.equal(r.status, 200);
    assert.ok(r.body['test-agent'], 'test-agent should appear');
    assert.equal(r.body['test-agent'].host, 'test-host');
  });
});

// ── POST /api/complete/:id ──────────────────────────────────────────────────
describe('POST /api/complete/:id — cleanup', () => {
  test('marks test item complete', async () => {
    const r = await authed('/api/complete/' + testItemId, 'POST');
    assert.equal(r.status, 200);
    assert.ok(r.body.ok);
    assert.equal(r.body.item.status, 'completed');
  });
});
