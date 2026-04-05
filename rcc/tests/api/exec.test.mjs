/**
 * Tests for ClawBus remote code execution
 * Covers: signing/verification library + API endpoints
 *
 * Run: node --test rcc/tests/api/exec.test.mjs
 */

import { test, describe, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { writeFile, unlink, mkdir } from 'fs/promises';
import { tmpdir } from 'os';
import { join } from 'path';

// ── Signing library tests ──────────────────────────────────────────────────

const { canonicalize, signPayload, verifyPayload } = await import('../../exec/index.mjs');

describe('canonicalize()', () => {
  test('sorts object keys', () => {
    const result = canonicalize({ z: 1, a: 2, m: 3 });
    assert.equal(result, '{"a":2,"m":3,"z":1}');
  });

  test('handles nested objects with sorted keys', () => {
    const result = canonicalize({ b: { y: 1, a: 2 }, a: 'hello' });
    assert.equal(result, '{"a":"hello","b":{"a":2,"y":1}}');
  });

  test('preserves array order', () => {
    const result = canonicalize([3, 1, 2]);
    assert.equal(result, '[3,1,2]');
  });

  test('handles null', () => {
    assert.equal(canonicalize(null), 'null');
  });

  test('handles primitive string', () => {
    assert.equal(canonicalize('hello'), '"hello"');
  });

  test('handles numbers', () => {
    assert.equal(canonicalize(42), '42');
  });

  test('is deterministic across calls', () => {
    const obj = { code: '1+1', execId: 'abc', target: 'all', ts: '2026-01-01' };
    assert.equal(canonicalize(obj), canonicalize(obj));
  });
});

describe('signPayload()', () => {
  test('returns a hex string', () => {
    const sig = signPayload({ code: 'test' }, 'secret');
    assert.match(sig, /^[0-9a-f]{64}$/);
  });

  test('same payload + secret = same signature', () => {
    const payload = { code: '1+1', execId: 'abc', target: 'all' };
    const sig1 = signPayload(payload, 'mysecret');
    const sig2 = signPayload(payload, 'mysecret');
    assert.equal(sig1, sig2);
  });

  test('different secrets = different signatures', () => {
    const payload = { code: '1+1' };
    const sig1 = signPayload(payload, 'secret1');
    const sig2 = signPayload(payload, 'secret2');
    assert.notEqual(sig1, sig2);
  });

  test('different payloads = different signatures', () => {
    const sig1 = signPayload({ code: 'a' }, 'secret');
    const sig2 = signPayload({ code: 'b' }, 'secret');
    assert.notEqual(sig1, sig2);
  });

  test('payload key order does not matter (canonical)', () => {
    const sig1 = signPayload({ code: '1', target: 'all' }, 'secret');
    const sig2 = signPayload({ target: 'all', code: '1' }, 'secret');
    assert.equal(sig1, sig2);
  });
});

describe('verifyPayload()', () => {
  test('verifies a correctly signed envelope', () => {
    const payload = { code: 'console.log(1)', execId: 'test-1', target: 'all' };
    const sig = signPayload(payload, 'shared-secret');
    const envelope = { ...payload, sig };
    assert.equal(verifyPayload(envelope, 'shared-secret'), true);
  });

  test('rejects tampered code', () => {
    const payload = { code: 'console.log(1)', execId: 'test-2', target: 'all' };
    const sig = signPayload(payload, 'shared-secret');
    const envelope = { ...payload, sig, code: 'require("child_process").exec("rm -rf /")' };
    assert.equal(verifyPayload(envelope, 'shared-secret'), false);
  });

  test('rejects wrong secret', () => {
    const payload = { code: 'console.log(1)', execId: 'test-3', target: 'all' };
    const sig = signPayload(payload, 'correct-secret');
    const envelope = { ...payload, sig };
    assert.equal(verifyPayload(envelope, 'wrong-secret'), false);
  });

  test('rejects missing sig', () => {
    const payload = { code: 'console.log(1)', execId: 'test-4', target: 'all' };
    assert.equal(verifyPayload(payload, 'secret'), false);
  });

  test('rejects null envelope', () => {
    assert.equal(verifyPayload(null, 'secret'), false);
  });

  test('rejects corrupted sig', () => {
    const payload = { code: 'console.log(1)', execId: 'test-5', target: 'all' };
    const envelope = { ...payload, sig: 'deadbeef' };
    assert.equal(verifyPayload(envelope, 'secret'), false);
  });
});

// ── API endpoint tests ─────────────────────────────────────────────────────

const TEST_PORT   = 19200 + Math.floor(Math.random() * 100);
const TEST_QUEUE  = join(tmpdir(), `rcc-exec-queue-${Date.now()}.json`);
const TEST_AGENTS = join(tmpdir(), `rcc-exec-agents-${Date.now()}.json`);
const TEST_CAPS   = join(tmpdir(), `rcc-exec-caps-${Date.now()}.json`);
const TEST_EXEC_LOG = join(tmpdir(), `rcc-exec-log-${Date.now()}.jsonl`);

const ADMIN_TOKEN = 'exec-test-admin-token';
const AGENT_TOKEN = 'exec-test-agent-token';

process.env.RCC_PORT          = String(TEST_PORT);
process.env.QUEUE_PATH        = TEST_QUEUE;
process.env.AGENTS_PATH       = TEST_AGENTS;
process.env.CAPABILITIES_PATH = TEST_CAPS;
process.env.CCC_AUTH_TOKENS   = `${ADMIN_TOKEN},${AGENT_TOKEN}`;
process.env.RCC_ADMIN_TOKEN   = ADMIN_TOKEN;
process.env.CLAWBUS_TOKEN = 'test-clawbus-secret';
process.env.EXEC_LOG_PATH     = TEST_EXEC_LOG;
process.env.BRAIN_STATE_PATH  = join(tmpdir(), `rcc-exec-brain-${Date.now()}.json`);

await writeFile(TEST_QUEUE,  JSON.stringify({ items: [], completed: [] }));
await writeFile(TEST_AGENTS, JSON.stringify({}));
await writeFile(TEST_CAPS,   JSON.stringify({}));

const { startServer } = await import('../../api/index.mjs');
let server;

before(() => {
  server = startServer(TEST_PORT);
});

after(async () => {
  server.close();
  for (const f of [TEST_QUEUE, TEST_AGENTS, TEST_CAPS, TEST_EXEC_LOG]) {
    await unlink(f).catch(() => {});
  }
});

const BASE = `http://localhost:${TEST_PORT}`;

async function post(path, body, token = ADMIN_TOKEN) {
  const resp = await fetch(`${BASE}${path}`, {
    method: 'POST',
    headers: { 'Authorization': `Bearer ${token}`, 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  });
  return { status: resp.status, body: await resp.json() };
}

async function get(path, token = AGENT_TOKEN) {
  const resp = await fetch(`${BASE}${path}`, {
    headers: { 'Authorization': `Bearer ${token}` },
  });
  return { status: resp.status, body: await resp.json() };
}

describe('POST /api/exec', () => {
  test('requires admin token', async () => {
    const { status } = await post('/api/exec', { code: '1+1' }, AGENT_TOKEN);
    assert.equal(status, 401);
  });

  test('requires code field', async () => {
    const { status, body } = await post('/api/exec', {});
    assert.equal(status, 400);
    assert.equal(body.error, 'code required');
  });

  test('returns execId on success', async () => {
    const { status, body } = await post('/api/exec', { code: '1+1', target: 'all' });
    assert.equal(status, 200);
    assert.ok(body.ok);
    assert.match(body.execId, /^exec-/);
  });

  test('broadcasts to specified target', async () => {
    const { status, body } = await post('/api/exec', { code: '42', target: 'natasha' });
    assert.equal(status, 200);
    assert.ok(body.ok);
    assert.match(body.execId, /^exec-/);
  });
});

describe('GET /api/exec/:id', () => {
  let execId;

  before(async () => {
    const { body } = await post('/api/exec', { code: 'Math.PI', target: 'all' });
    execId = body.execId;
    // Give the server a tick to write the log
    await new Promise(r => setTimeout(r, 50));
  });

  test('returns exec record', async () => {
    const { status, body } = await get(`/api/exec/${execId}`);
    assert.equal(status, 200);
    assert.equal(body.execId, execId);
    assert.equal(body.code, 'Math.PI');
    assert.ok(Array.isArray(body.results));
  });

  test('returns 404 for unknown id', async () => {
    const { status } = await get('/api/exec/nonexistent-id');
    assert.equal(status, 404);
  });

  test('requires auth', async () => {
    const resp = await fetch(`${BASE}/api/exec/${execId}`);
    assert.equal(resp.status, 401);
  });
});

describe('POST /api/exec/:id/result', () => {
  let execId;

  before(async () => {
    const { body } = await post('/api/exec', { code: 'console.log("test"); 99', target: 'all' });
    execId = body.execId;
    await new Promise(r => setTimeout(r, 50));
  });

  test('appends result to exec record', async () => {
    const resultPayload = {
      agent:      'test-agent',
      ok:         true,
      output:     'test',
      result:     '99',
      error:      null,
      durationMs: 5,
    };

    const { status, body } = await post(`/api/exec/${execId}/result`, resultPayload, AGENT_TOKEN);
    assert.equal(status, 200);
    assert.ok(body.ok);
    assert.equal(body.execId, execId);

    // Verify it's persisted
    await new Promise(r => setTimeout(r, 50));
    const { body: record } = await get(`/api/exec/${execId}`);
    assert.ok(record.results.length > 0);
    assert.equal(record.results[0].agent, 'test-agent');
    assert.equal(record.results[0].result, '99');
  });

  test('requires auth', async () => {
    const resp = await fetch(`${BASE}/api/exec/${execId}/result`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ agent: 'bad', ok: true }),
    });
    assert.equal(resp.status, 401);
  });

  test('creates stub for unknown exec id', async () => {
    const { status, body } = await post('/api/exec/unknown-exec-id/result', {
      agent: 'test-agent', ok: true, output: 'hi', result: null, error: null, durationMs: 1,
    }, AGENT_TOKEN);
    assert.equal(status, 200);
    assert.ok(body.ok);
  });

  test('multiple agents can post results', async () => {
    const { body: newExec } = await post('/api/exec', { code: '2+2', target: 'all' });
    const eid = newExec.execId;
    await new Promise(r => setTimeout(r, 50));

    await post(`/api/exec/${eid}/result`, { agent: 'agent1', ok: true, output: '4', result: '4', error: null, durationMs: 1 }, AGENT_TOKEN);
    await post(`/api/exec/${eid}/result`, { agent: 'agent2', ok: true, output: '4', result: '4', error: null, durationMs: 2 }, AGENT_TOKEN);

    await new Promise(r => setTimeout(r, 50));
    const { body: record } = await get(`/api/exec/${eid}`);
    assert.ok(record.results.length >= 2);
    const agents = record.results.map(r => r.agent);
    assert.ok(agents.includes('agent1'));
    assert.ok(agents.includes('agent2'));
  });
});

describe('Signature round-trip with envelope from API', () => {
  test('signed envelope can be verified by agent', async () => {
    const secret = process.env.CLAWBUS_TOKEN || process.env.SQUIRRELBUS_TOKEN;
    const payload = {
      execId: 'exec-roundtrip-test',
      code:   'Math.sqrt(16)',
      target: 'all',
      replyTo: null,
      ts:     new Date().toISOString(),
    };
    const sig = signPayload(payload, secret);
    const envelope = { ...payload, sig };
    assert.equal(verifyPayload(envelope, secret), true);
    // Tamper with code
    const tampered = { ...envelope, code: 'process.exit(0)' };
    assert.equal(verifyPayload(tampered, secret), false);
  });
});
