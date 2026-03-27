/**
 * RCC API unit tests — local test server
 * Run: node --test rcc/tests/api.test.mjs
 *
 * Spins up an isolated in-process server on a random port.
 * Uses temp files so nothing touches production data.
 */

import { test, describe, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { writeFile, unlink } from 'fs/promises';
import { tmpdir } from 'os';
import { join } from 'path';

// ── Test server setup ───────────────────────────────────────────────────────

const TEST_PORT   = 19200 + Math.floor(Math.random() * 100);
const TEST_QUEUE  = join(tmpdir(), `rcc-api-queue-${Date.now()}.json`);
const TEST_AGENTS = join(tmpdir(), `rcc-api-agents-${Date.now()}.json`);
const TEST_CAPS   = join(tmpdir(), `rcc-api-caps-${Date.now()}.json`);

// Use first token as admin (matches RCC_ADMIN_TOKEN logic)
const ADMIN_TOKEN = 'api-test-admin-token';
const AGENT_TOKEN = 'api-test-agent-token';

process.env.RCC_PORT          = String(TEST_PORT);
process.env.QUEUE_PATH        = TEST_QUEUE;
process.env.AGENTS_PATH       = TEST_AGENTS;
process.env.CAPABILITIES_PATH = TEST_CAPS;
process.env.RCC_AUTH_TOKENS   = `${ADMIN_TOKEN},${AGENT_TOKEN}`;
process.env.RCC_ADMIN_TOKEN   = ADMIN_TOKEN;
process.env.BRAIN_STATE_PATH  = join(tmpdir(), `rcc-api-brain-${Date.now()}.json`);

await writeFile(TEST_QUEUE,  JSON.stringify({ items: [], completed: [] }, null, 2));
await writeFile(TEST_AGENTS, JSON.stringify({}, null, 2));
await writeFile(TEST_CAPS,   JSON.stringify({}, null, 2));

const { startServer } = await import('../api/index.mjs');

let server;
let BASE;

before(async () => {
  server = startServer(TEST_PORT);
  await new Promise(r => server.on('listening', r));
  BASE = `http://localhost:${TEST_PORT}`;
});

after(async () => {
  server.close();
  await Promise.all([
    unlink(TEST_QUEUE).catch(() => {}),
    unlink(TEST_AGENTS).catch(() => {}),
    unlink(TEST_CAPS).catch(() => {}),
  ]);
  // Force exit: the API sets a 5-minute setInterval (disappearance check)
  // that keeps the event loop alive after server.close(). We exit explicitly.
  setTimeout(() => process.exit(0), 500).unref();
});

// ── Helpers ─────────────────────────────────────────────────────────────────

async function req(path, opts = {}) {
  const res = await fetch(BASE + path, opts);
  const body = await res.json().catch(() => ({}));
  return { status: res.status, body };
}

async function authed(path, method = 'GET', body, token = AGENT_TOKEN) {
  const opts = {
    method,
    headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' },
  };
  if (body !== undefined) opts.body = JSON.stringify(body);
  return req(path, opts);
}

async function adminReq(path, method = 'GET', body) {
  return authed(path, method, body, ADMIN_TOKEN);
}

// ── Health ───────────────────────────────────────────────────────────────────

describe('GET /health — public endpoint', () => {
  test('returns ok:true without auth', async () => {
    const r = await req('/health');
    assert.equal(r.status, 200);
    assert.equal(r.body.ok, true);
  });

  test('returns uptime and version', async () => {
    const r = await req('/health');
    assert.ok(typeof r.body.uptime === 'number', 'uptime should be a number');
    assert.ok(r.body.version, 'version should be present');
  });
});

// ── Auth & bootstrap flow ────────────────────────────────────────────────────

describe('Bootstrap flow', () => {
  test('POST /api/bootstrap/token requires admin auth — 401 without auth', async () => {
    const r = await req('/api/bootstrap/token', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ agent: 'test-agent' }),
    });
    assert.equal(r.status, 401);
  });

  test('POST /api/bootstrap/token requires admin auth — 401 with non-admin token', async () => {
    const r = await authed('/api/bootstrap/token', 'POST', { agent: 'test-agent' }, AGENT_TOKEN);
    assert.equal(r.status, 401);
  });

  test('POST /api/bootstrap/token with admin token returns 200 and bootstrapToken', async () => {
    const r = await adminReq('/api/bootstrap/token', 'POST', { agent: 'test-bootstrap-agent' });
    assert.equal(r.status, 200);
    assert.ok(r.body.ok);
    assert.ok(r.body.bootstrapToken, 'should return bootstrapToken');
    assert.ok(r.body.expiresAt, 'should return expiresAt');
    assert.equal(r.body.agent, 'test-bootstrap-agent');
  });

  test('POST /api/bootstrap/token requires agent field', async () => {
    const r = await adminReq('/api/bootstrap/token', 'POST', {});
    assert.equal(r.status, 400);
  });

  test('GET /api/bootstrap with invalid token returns 401', async () => {
    // NOTE: No Authorization header — this is intentional per the critical regression test
    const r = await req('/api/bootstrap?token=this-is-not-a-valid-token');
    assert.equal(r.status, 401);
  });

  test('GET /api/bootstrap returns 401 without token param (auth guard fires before route handler)', async () => {
    // BUG: GET /api/bootstrap is positioned AFTER the auth middleware in api/index.mjs.
    // This means it returns 401 (Unauthorized) when called without an Authorization header,
    // even before it can check for a missing ?token= param.
    // The intended behavior (documented regression: commit 2302f1c) is that this endpoint
    // should be reachable WITHOUT auth. See BUGS.md.
    const r = await req('/api/bootstrap');
    // The auth guard at line 763 fires first → 401
    assert.equal(r.status, 401);
  });

  // NOTE: Full bootstrap flow (token → GET /api/bootstrap) requires a github deploy key
  // to be configured on the server (data/github-key.json). Without it, GET /api/bootstrap
  // returns 500 {"error":"Deploy key not configured"}. This is documented in BUGS.md.
  // We test the 401 (invalid token) path above; the success path is covered in integration.test.mjs.
});

// ── Heartbeat ────────────────────────────────────────────────────────────────

describe('Heartbeat', () => {
  test('POST /api/heartbeat/:agent requires auth', async () => {
    const r = await req('/api/heartbeat/test-agent', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ host: 'testhost' }),
    });
    assert.equal(r.status, 401);
  });

  test('POST /api/heartbeat/:agent with auth returns 200 and ok:true', async () => {
    const r = await authed('/api/heartbeat/test-agent', 'POST', { host: 'testhost', status: 'online' });
    assert.equal(r.status, 200);
    assert.ok(r.body.ok);
    assert.ok(Array.isArray(r.body.pendingWork), 'pendingWork should be an array');
  });

  test('GET /api/agents returns agents list (public)', async () => {
    const r = await req('/api/agents');
    assert.equal(r.status, 200);
    assert.ok(Array.isArray(r.body), 'should return an array');
  });

  test('GET /api/agents/status requires auth', async () => {
    const r = await req('/api/agents/status');
    assert.equal(r.status, 401);
  });

  test('GET /api/agents/status with auth returns ok:true and agents array', async () => {
    const r = await authed('/api/agents/status');
    assert.equal(r.status, 200);
    assert.ok(r.body.ok);
    assert.ok(Array.isArray(r.body.agents));
  });
});

// ── Queue ────────────────────────────────────────────────────────────────────

describe('Queue', () => {
  let createdItemId;

  test('GET /api/queue returns items array — no auth required', async () => {
    const r = await req('/api/queue');
    assert.equal(r.status, 200);
    assert.ok(Array.isArray(r.body.items), 'items should be an array');
    assert.ok(Array.isArray(r.body.completed), 'completed should be an array');
  });

  test('POST /api/queue without auth returns 401', async () => {
    const r = await req('/api/queue', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ title: 'Unauthorized item' }),
    });
    assert.equal(r.status, 401);
  });

  test('POST /api/queue with auth creates item and returns 201', async () => {
    const r = await authed('/api/queue', 'POST', {
      title: 'Test queue item',
      description: 'Created by api.test.mjs',
      priority: 'normal',
    });
    assert.equal(r.status, 201);
    assert.ok(r.body.ok);
    assert.ok(r.body.item, 'should return the created item');
    assert.ok(r.body.item.id, 'item should have an id');
    assert.equal(r.body.item.title, 'Test queue item');
    assert.equal(r.body.item.status, 'pending');
    createdItemId = r.body.item.id;
  });

  test('POST /api/queue requires title field', async () => {
    const r = await authed('/api/queue', 'POST', { description: 'No title' });
    assert.equal(r.status, 400);
  });

  test('created item appears in GET /api/queue', async () => {
    const r = await req('/api/queue');
    assert.equal(r.status, 200);
    const found = r.body.items.find(i => i.id === createdItemId);
    assert.ok(found, 'created item should appear in queue');
    assert.equal(found.title, 'Test queue item');
  });

  test('PATCH /api/item/:id updates status', async () => {
    assert.ok(createdItemId, 'need a created item');
    const r = await authed(`/api/item/${createdItemId}`, 'PATCH', {
      status: 'in-progress',
      _author: 'api-test',
    });
    assert.equal(r.status, 200);
    assert.ok(r.body.ok);
    assert.equal(r.body.item.status, 'in-progress');
  });

  test('PATCH /api/item/:id — nonexistent id returns 404', async () => {
    const r = await authed('/api/item/nonexistent-id-xyz', 'PATCH', { status: 'completed' });
    assert.equal(r.status, 404);
  });

  test('GET /api/item/:id returns item detail (public)', async () => {
    assert.ok(createdItemId, 'need a created item');
    const r = await req(`/api/item/${createdItemId}`);
    assert.equal(r.status, 200);
    assert.equal(r.body.id, createdItemId);
  });

  test('GET /api/item/nonexistent returns 404', async () => {
    const r = await req('/api/item/nonexistent-xyz-123');
    assert.equal(r.status, 404);
  });
});

// ── Secrets (not implemented) ────────────────────────────────────────────────

describe('Secrets — endpoint existence', () => {
  // /api/secrets is NOT implemented in this version of the RCC API.
  // Authenticated requests → 404 (not found, falls through to 404 handler)
  // Unauthenticated requests → 401 (auth guard fires before 404)
  // See BUGS.md: "Bug: /api/secrets endpoint not implemented"

  test('GET /api/secrets/any-key with auth returns 404 (endpoint does not exist)', async () => {
    const r = await authed('/api/secrets/some-key');
    assert.equal(r.status, 404);
  });

  test('GET /api/secrets/any-key without auth returns 401 (auth guard fires before route matching)', async () => {
    // Because /api/secrets/* has no explicit route, the auth guard at line 763
    // fires before the 404 fallback, returning 401 for unauthenticated callers.
    const r = await req('/api/secrets/some-key');
    assert.equal(r.status, 401);
  });
});

// ── Agent registration ────────────────────────────────────────────────────────

describe('Agent registration', () => {
  test('POST /api/agents/register requires auth', async () => {
    const r = await req('/api/agents/register', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ name: 'unauthed-agent' }),
    });
    assert.equal(r.status, 401);
  });

  test('POST /api/agents/register requires name field', async () => {
    const r = await authed('/api/agents/register', 'POST', {});
    assert.equal(r.status, 400);
  });

  test('POST /api/agents/register creates agent and returns token', async () => {
    const r = await authed('/api/agents/register', 'POST', {
      name: 'test-registered-agent',
      host: 'testhost.local',
      type: 'full',
    });
    assert.equal(r.status, 201);
    assert.ok(r.body.ok);
    assert.ok(r.body.token, 'should return a token');
    assert.equal(r.body.agent.name, 'test-registered-agent');
  });
});

// ── Error cases ──────────────────────────────────────────────────────────────

describe('Error cases', () => {
  test('GET /nonexistent-route without auth returns 401 (auth guard before 404)', async () => {
    // BUG: The auth middleware at line 763 of api/index.mjs fires before the
    // final 404 fallback. So unauthenticated requests to any unknown route
    // return 401 (Unauthorized), not 404 (Not Found). This leaks information
    // about the auth boundary. See BUGS.md.
    const r = await req('/nonexistent-route-xyz');
    assert.equal(r.status, 401);
  });

  test('GET /nonexistent-route with auth returns 404', async () => {
    const r = await authed('/nonexistent-route-xyz');
    assert.equal(r.status, 404);
  });

  test('GET /api/nonexistent with auth returns 404', async () => {
    const r = await authed('/api/nonexistent-endpoint');
    assert.equal(r.status, 404);
  });

  test('OPTIONS returns 204 (CORS preflight)', async () => {
    const res = await fetch(BASE + '/api/queue', { method: 'OPTIONS' });
    assert.equal(res.status, 204);
  });
});

// ── Scout & Crons ────────────────────────────────────────────────────────────

describe('Scout and cron endpoints', () => {
  test('GET /api/scout/:agent requires auth', async () => {
    const r = await req('/api/scout/test-agent');
    assert.equal(r.status, 401);
  });

  test('GET /api/scout/:agent with auth returns pendingWork', async () => {
    const r = await authed('/api/scout/test-agent');
    assert.equal(r.status, 200);
    assert.ok(r.body.ok);
    assert.ok(Array.isArray(r.body.pendingWork));
  });

  test('GET /api/crons returns array', async () => {
    const r = await authed('/api/crons');
    assert.equal(r.status, 200);
    assert.ok(Array.isArray(r.body));
  });

  test('POST /api/crons/:agent requires auth', async () => {
    const r = await req('/api/crons/test-agent', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ jobId: 'test-job' }),
    });
    assert.equal(r.status, 401);
  });

  test('POST /api/crons/:agent with auth records cron status', async () => {
    const r = await authed('/api/crons/test-agent', 'POST', { jobId: 'test-job', status: 'ok' });
    assert.equal(r.status, 200);
    assert.ok(r.body.ok);
  });
});
