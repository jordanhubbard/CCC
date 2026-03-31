/**
 * Tests for rcc/brain
 * Run: node --test rcc/brain/test.mjs
 * Note: does NOT hit real NVIDIA API — tests the queue/retry/fallback logic only
 */

import { test, describe, mock, beforeEach } from 'node:test';
import assert from 'node:assert/strict';
import { Brain, createRequest } from './index.mjs';

// ── createRequest ───────────────────────────────────────────────────────────
describe('createRequest', () => {
  test('creates request with defaults', () => {
    const r = createRequest({ messages: [{ role: 'user', content: 'hello' }] });
    assert.ok(r.id.startsWith('brain-'));
    assert.equal(r.status, 'pending');
    assert.equal(r.priority, 'normal');
    assert.equal(r.maxTokens, 1024);
    assert.deepEqual(r.attempts, []);
    assert.equal(r.result, null);
  });

  test('respects custom options', () => {
    const r = createRequest({
      messages: [{ role: 'user', content: 'test' }],
      maxTokens: 512,
      priority: 'high',
      callbackUrl: 'http://localhost/callback',
      metadata: { tag: 'test' },
    });
    assert.equal(r.maxTokens, 512);
    assert.equal(r.priority, 'high');
    assert.equal(r.callbackUrl, 'http://localhost/callback');
    assert.deepEqual(r.metadata, { tag: 'test' });
  });
});

// ── Brain queue / priority ──────────────────────────────────────────────────
describe('Brain queue management', () => {
  function freshBrain() {
    const uid = `${Date.now()}-${Math.random().toString(36).slice(2)}-${Math.random().toString(36).slice(2)}`;
    const statePath = `/tmp/brain-test-${uid}.json`;
    return new Brain({ statePath });
  }

  test('starts with empty queue', async () => {
    const brain = freshBrain();
    await brain.init();
    assert.equal(brain.state.queue.length, 0);
  });

  test('enqueues a request', async () => {
    const brain = freshBrain();
    await brain.init();
    const r = createRequest({ messages: [{ role: 'user', content: 'hi' }] });
    await brain.enqueue(r);
    assert.equal(brain.state.queue.length, 1);
  });

  test('sorts by priority: high before normal before low', async () => {
    const brain = freshBrain();
    await brain.init();
    // Reset to empty regardless of any leftover state
    brain.state.queue = [];
    const low    = createRequest({ messages: [{ role: 'user', content: 'low' }],    priority: 'low' });
    const normal = createRequest({ messages: [{ role: 'user', content: 'normal' }], priority: 'normal' });
    const high   = createRequest({ messages: [{ role: 'user', content: 'high' }],   priority: 'high' });

    await brain.enqueue(low);
    await brain.enqueue(normal);
    await brain.enqueue(high);

    assert.equal(brain.state.queue[0].priority, 'high');
    assert.equal(brain.state.queue[1].priority, 'normal');
    assert.equal(brain.state.queue[2].priority, 'low');
  });

  test('getStatus returns queue depth', async () => {
    const brain = freshBrain();
    await brain.init();
    brain.state.queue = []; // start clean
    const r = createRequest({ messages: [{ role: 'user', content: 'test' }] });
    await brain.enqueue(r);
    const status = brain.getStatus();
    assert.equal(status.queueDepth, 1);
    assert.ok(Array.isArray(status.models));
    assert.equal(status.models.length, 3);
  });
});

// ── LeakyBucket (internal — test via Brain) ─────────────────────────────────
describe('Rate limiting', () => {
  test('bucket tracks requests', async () => {
    const statePath = `/tmp/brain-test-${Date.now()}-${Math.random().toString(36).slice(2)}.json`;
    const brain = new Brain({ statePath });
    await brain.init();
    const bucket = Object.values(brain.buckets)[0];

    assert.ok(bucket.canSend(100));
    bucket.record(1000);
    assert.equal(bucket.requestCount, 1);
    assert.equal(bucket.tokenCount, 1000);
  });

  test('bucket blocks when over limit', async () => {
    const statePath = `/tmp/brain-test-${Date.now()}-${Math.random().toString(36).slice(2)}.json`;
    const brain = new Brain({ statePath });
    await brain.init();
    const bucket = Object.values(brain.buckets)[0];
    // Saturate the token budget
    bucket.tokenCount = bucket.maxTokensPerMin + 1;
    assert.equal(bucket.canSend(100), false);
    assert.ok(bucket.waitMs(100) > 0);
  });
});

// ── Model fallback (mocked) ──────────────────────────────────────────────────
describe('Model fallback logic', () => {
  test('marks request completed on success', async () => {
    const statePath = `/tmp/brain-test-${Date.now()}-${Math.random().toString(36).slice(2)}.json`;
    process.env.NVIDIA_API_KEY = 'test-key';

    const brain = new Brain({ statePath });
    await brain.init();

    // Mock the fetch to return a successful response on first model
    const originalFetch = global.fetch;
    let callCount = 0;
    global.fetch = async (url, opts) => {
      callCount++;
      return {
        ok: true,
        status: 200,
        headers: { get: () => null },
        json: async () => ({
          choices: [{ message: { content: 'Test response from mock' } }],
          usage: { total_tokens: 50 },
        }),
      };
    };

    const r = createRequest({ messages: [{ role: 'user', content: 'test' }] });
    await brain.enqueue(r);

    // Process directly
    const item = brain.state.queue[0];
    await brain._processRequest(item);
    await brain.saveState?.(brain.state);

    assert.equal(item.status, 'completed');
    assert.equal(item.result, 'Test response from mock');
    assert.equal(callCount, 1);

    global.fetch = originalFetch;
  });

  test('falls back to next model on timeout', async () => {
    const statePath = `/tmp/brain-test-${Date.now()}-${Math.random().toString(36).slice(2)}.json`;
    process.env.NVIDIA_API_KEY = 'test-key';

    const brain = new Brain({ statePath });
    await brain.init();

    let callCount = 0;
    const originalFetch = global.fetch;
    global.fetch = async (url, opts) => {
      callCount++;
      if (callCount === 1) {
        // First model: abort (simulate timeout)
        opts.signal?.dispatchEvent(new Event('abort'));
        throw Object.assign(new Error('Request timed out'), { name: 'AbortError' });
      }
      // Second model: success
      return {
        ok: true,
        status: 200,
        headers: { get: () => null },
        json: async () => ({
          choices: [{ message: { content: 'Fallback response' } }],
          usage: { total_tokens: 30 },
        }),
      };
    };

    const r = createRequest({ messages: [{ role: 'user', content: 'test' }] });
    await brain.enqueue(r);
    const item = brain.state.queue[0];
    await brain._processRequest(item);

    // Should have tried at least 2 models
    assert.ok(callCount >= 2, `Expected at least 2 calls, got ${callCount}`);

    global.fetch = originalFetch;
  });
});
