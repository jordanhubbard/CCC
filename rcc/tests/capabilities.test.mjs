/**
 * Tests for per-agent capability registry
 * Run: node --test rcc/tests/capabilities.test.mjs
 */

import { test, describe, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { writeFile, unlink } from 'fs/promises';
import { tmpdir } from 'os';
import { join } from 'path';

// ── Test server setup ──────────────────────────────────────────────────────

const TEST_PORT  = 19100 + Math.floor(Math.random() * 100);
const TEST_QUEUE = join(tmpdir(), `rcc-caps-queue-${Date.now()}.json`);
const TEST_AGENTS = join(tmpdir(), `rcc-caps-agents-${Date.now()}.json`);
const TEST_CAPS   = join(tmpdir(), `rcc-caps-caps-${Date.now()}.json`);

process.env.RCC_PORT          = String(TEST_PORT);
process.env.QUEUE_PATH        = TEST_QUEUE;
process.env.AGENTS_PATH       = TEST_AGENTS;
process.env.CAPABILITIES_PATH = TEST_CAPS;
process.env.RCC_AUTH_TOKENS   = 'caps-test-token';
process.env.BRAIN_STATE_PATH  = join(tmpdir(), `rcc-caps-brain-${Date.now()}.json`);

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
});

// ── Helpers ────────────────────────────────────────────────────────────────

const AUTH = 'Bearer caps-test-token';

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

// ── Capability publishing (POST /api/agents/:name) ────────────────────────

describe('POST /api/agents/:name — publish capabilities', () => {
  test('requires auth', async () => {
    const r = await req('/api/agents/rocky', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ capabilities: { gpu: false } }),
    });
    assert.equal(r.status, 401);
  });

  test('auto-registers agent on first publish', async () => {
    const r = await authed('/api/agents/rocky', 'POST', {
      capabilities: {
        gpu: false,
        gpu_model: null,
        gpu_vram_gb: 0,
        gpu_count: 0,
        claude_cli: true,
        context_size: 'large',
        arch: 'aarch64',
        tools: ['bash', 'node', 'python', 'git'],
        preferred_tasks: ['code', 'review', 'debug'],
      },
      host: 'rocky.local',
    });
    assert.equal(r.status, 200);
    assert.ok(r.body.ok);
    assert.ok(r.body.token, 'should return a token');
    assert.equal(r.body.agent.name, 'rocky');
  });

  test('agent appears in GET /api/agents with capabilities', async () => {
    const r = await req('/api/agents');
    assert.equal(r.status, 200);
    const rocky = r.body.find(a => a.name === 'rocky');
    assert.ok(rocky, 'rocky should appear');
    assert.equal(rocky.capabilities.claude_cli, true);
    assert.equal(rocky.capabilities.arch, 'aarch64');
    assert.ok(Array.isArray(rocky.capabilities.tools), 'tools should be an array');
    assert.ok(Array.isArray(rocky.capabilities.preferred_tasks));
  });

  test('re-publishing updates capabilities', async () => {
    await authed('/api/agents/rocky', 'POST', {
      capabilities: { context_size: 'medium' },
    });
    const r = await req('/api/agents');
    const rocky = r.body.find(a => a.name === 'rocky');
    assert.equal(rocky.capabilities.context_size, 'medium');
  });
});

// ── Capability update (PATCH /api/agents/:name) ───────────────────────────

describe('PATCH /api/agents/:name — update capabilities', () => {
  test('requires auth', async () => {
    const r = await req('/api/agents/rocky', {
      method: 'PATCH',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ capabilities: { context_size: 'large' } }),
    });
    assert.equal(r.status, 401);
  });

  test('updates capabilities fields', async () => {
    const r = await authed('/api/agents/rocky', 'PATCH', {
      capabilities: {
        context_size: 'large',
        preferred_tasks: ['code', 'review', 'debug', 'triage'],
      },
    });
    assert.equal(r.status, 200);
    assert.ok(r.body.ok);
  });

  test('updated capabilities visible in GET /api/agents', async () => {
    const r = await req('/api/agents');
    const rocky = r.body.find(a => a.name === 'rocky');
    assert.equal(rocky.capabilities.context_size, 'large');
    assert.ok(rocky.capabilities.preferred_tasks.includes('triage'));
  });

  test('404 for unknown agent', async () => {
    const r = await authed('/api/agents/unknown-xyz', 'PATCH', {
      capabilities: { gpu: true },
    });
    assert.equal(r.status, 404);
  });
});

// ── Capability-based routing (GET /api/agents/best) ───────────────────────

describe('GET /api/agents/best — capability routing', () => {
  before(async () => {
    // seed multiple agents
    await authed('/api/agents/natasha', 'POST', {
      capabilities: {
        gpu: true,
        gpu_model: 'nvidia-blackwell',
        gpu_vram_gb: 192,
        gpu_count: 1,
        claude_cli: true,
        context_size: 'large',
        arch: 'aarch64',
        tools: ['bash', 'python', 'cuda'],
        preferred_tasks: ['training', 'inference', 'render', 'gpu'],
      },
    });
    await authed('/api/agents/boris', 'POST', {
      capabilities: {
        gpu: true,
        gpu_model: 'nvidia-l40',
        gpu_vram_gb: 96,
        gpu_count: 2,
        claude_cli: false,
        context_size: 'medium',
        arch: 'x86_64',
        tools: ['bash', 'python', 'docker', 'ffmpeg'],
        preferred_tasks: ['render', 'video', 'gpu'],
      },
    });
  });

  test('task=gpu returns agent with most VRAM', async () => {
    const r = await req('/api/agents/best?task=gpu');
    // natasha has 192GB vs boris 96GB — natasha wins
    assert.equal(r.status, 200);
    assert.equal(r.body.agent.name, 'natasha');
    assert.equal(r.body.task, 'gpu');
  });

  test('task=code returns claude_cli agent', async () => {
    const r = await req('/api/agents/best?task=code');
    assert.equal(r.status, 200);
    assert.ok(r.body.agent.capabilities.claude_cli);
  });

  test('task=video matches preferred_tasks', async () => {
    const r = await req('/api/agents/best?task=video');
    assert.equal(r.status, 200);
    // boris has video in preferred_tasks
    assert.equal(r.body.agent.name, 'boris');
  });

  test('task=render returns a GPU agent', async () => {
    const r = await req('/api/agents/best?task=render');
    assert.equal(r.status, 200);
    assert.ok(r.body.agent.capabilities.gpu);
  });

  test('returns agent name and task in response', async () => {
    const r = await req('/api/agents/best?task=inference');
    assert.equal(r.status, 200);
    assert.ok(r.body.agent.name);
    assert.equal(r.body.task, 'inference');
  });
});

// ── GET /api/agents includes capabilities ─────────────────────────────────

describe('GET /api/agents — full capabilities in response', () => {
  test('each agent entry has capabilities object', async () => {
    const r = await req('/api/agents');
    assert.equal(r.status, 200);
    for (const agent of r.body) {
      assert.ok(typeof agent.capabilities === 'object', `${agent.name} should have capabilities`);
    }
  });

  test('capabilities include new schema fields when published', async () => {
    const r = await req('/api/agents');
    const natasha = r.body.find(a => a.name === 'natasha');
    assert.ok(natasha, 'natasha should be registered');
    assert.equal(natasha.capabilities.gpu, true);
    assert.equal(natasha.capabilities.gpu_model, 'nvidia-blackwell');
    assert.equal(natasha.capabilities.gpu_vram_gb, 192);
    assert.equal(natasha.capabilities.context_size, 'large');
    assert.equal(natasha.capabilities.arch, 'aarch64');
    assert.ok(Array.isArray(natasha.capabilities.tools));
    assert.ok(Array.isArray(natasha.capabilities.preferred_tasks));
  });
});
