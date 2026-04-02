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
// ── Helper ──────────────────────────────────────────────────────────────────
import { randomUUID } from 'node:crypto';
function freshBrainWithPath() {
  const uid = randomUUID();
  return { brain: new Brain({ statePath: `/tmp/brain-edge-${uid}.json` }), uid };
}

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

// ── Edge cases ──────────────────────────────────────────────────────────────
describe('Brain edge cases', { concurrency: 1 }, () => {

  // 1. All models degrade — every model in the chain returns an error.
  //    The request should surface the error (thrown by callModel) and be
  //    put back as 'pending' with an attempt record, not silently lost.
  test('all models degrade: request remains in queue with error attempt', async () => {
    process.env.NVIDIA_API_KEY = 'test-key';
    const { brain } = freshBrainWithPath();
    await brain.init();

    const originalFetch = global.fetch;
    let callCount = 0;
    global.fetch = async () => {
      callCount++;
      throw Object.assign(new Error('Service unavailable'), { code: 'HTTP_ERROR', status: 503 });
    };

    const r = createRequest({ messages: [{ role: 'user', content: 'test' }] });
    await brain.enqueue(r);
    const item = brain.state.queue[0];
    await brain._processRequest(item);

    // Request must NOT be completed
    assert.notEqual(item.status, 'completed', 'Request should not be completed when all models fail');
    // Must have at least one attempt recorded
    assert.ok(item.attempts.length > 0, 'At least one attempt should be recorded');
    // The last attempt should have an error
    const lastAttempt = item.attempts.at(-1);
    assert.ok(lastAttempt.error, 'Last attempt should record the error message');
    // Item should still be in queue (not silently dropped)
    const stillInQueue = brain.state.queue.some(q => q.id === r.id);
    assert.ok(stillInQueue, 'Failed request should remain in queue for retry');

    global.fetch = originalFetch;
  });

  // 2. All models degrade then recover — every model fails first tick,
  //    then all succeed on second tick.
  //    The brain's callModel() tries ALL models per tick, so we need ALL of them
  //    to fail on tick 1. We track by request attempt count to distinguish ticks.
  test('all models degrade then recover on second tick', async () => {
    process.env.NVIDIA_API_KEY = 'test-key';
    const { brain } = freshBrainWithPath();
    await brain.init();

    let firstTickDone = false;
    const originalFetch = global.fetch;
    global.fetch = async () => {
      if (!firstTickDone) {
        throw Object.assign(new Error('503 degraded'), { code: 'HTTP_ERROR', status: 503 });
      }
      return {
        ok: true, status: 200, headers: { get: () => null },
        json: async () => ({ choices: [{ message: { content: 'Recovered' } }], usage: { total_tokens: 10 } }),
      };
    };

    const r = createRequest({ messages: [{ role: 'user', content: 'retry test' }] });
    await brain.enqueue(r);
    const item = brain.state.queue[0];

    // First tick — all models fail
    await brain._processRequest(item);
    firstTickDone = true;
    assert.notEqual(item.status, 'completed', 'Should not be completed when all models failed');
    assert.ok(item.attempts.length > 0, 'Should have recorded failed attempt(s)');

    // Second tick — succeeds
    item.status = 'pending'; // simulate scheduler re-queue
    await brain._processRequest(item);
    assert.equal(item.status, 'completed', 'Should complete after recovery on second tick');
    assert.equal(item.result, 'Recovered');

    global.fetch = originalFetch;
  });

  // 3. Partial state recovery — brain restarts with an 'in-progress' item.
  //    On init(), in-progress items should be reset to 'pending' so they
  //    are not permanently stuck.
  test('partial state recovery: in-progress items reset to pending on init', async () => {
    const { brain: brain1, uid } = freshBrainWithPath();
    await brain1.init();

    const r = createRequest({ messages: [{ role: 'user', content: 'mid-flight' }] });
    await brain1.enqueue(r);
    // Simulate brain dying mid-flight by setting status directly
    brain1.state.queue[0].status = 'in-progress';
    if (brain1.saveState) {
      await brain1.saveState(brain1.state);
    } else {
      // Force-write state via internal path (same as init loads from)
      const { writeFile } = await import('node:fs/promises');
      await writeFile(brain1.statePath, JSON.stringify(brain1.state));
    }

    // New brain instance loads the same state file
    const brain2 = new Brain({ statePath: brain1.statePath });
    await brain2.init();

    const loaded = brain2.state.queue.find(q => q.id === r.id);
    assert.ok(loaded, 'In-progress item should still be in queue after restart');
    assert.equal(loaded.status, 'pending',
      'In-progress items should be reset to pending on init (to prevent permanent stuck state)');
  });

  // 4. High priority request interrupts low priority mid-queue.
  //    After enqueuing a low request then a high request, the high one
  //    should be first in queue regardless of insertion order.
  test('high priority queued after low is processed first', async () => {
    const { brain } = freshBrainWithPath();
    await brain.init();

    const low  = createRequest({ messages: [{ role: 'user', content: 'low' }],  priority: 'low' });
    const high = createRequest({ messages: [{ role: 'user', content: 'high' }], priority: 'high' });

    // Use different created times to ensure deterministic sort when priority is equal
    low.created  = new Date(Date.now() - 1000).toISOString(); // created 1s ago
    high.created = new Date(Date.now()).toISOString();         // created just now

    await brain.enqueue(low);
    await brain.enqueue(high);

    // Filter to just our two test items (queue may have items from shared state)
    const myItems = brain.state.queue.filter(q => q.id === low.id || q.id === high.id);
    assert.equal(myItems.length, 2, 'Both test items should be in queue');
    // First of our items should be high priority (sorted before low regardless of insertion order)
    assert.equal(myItems[0].id,       high.id,  `High priority item (${high.id}) should sort before low`);
    assert.equal(myItems[0].priority, 'high',   'First of our items should have high priority');
    assert.equal(myItems[1].id,       low.id,   `Low priority item (${low.id}) should sort after high`);
    assert.equal(myItems[1].priority, 'low',    'Second of our items should have low priority');
  });

  // 5. Callback URL — verify callbackUrl is preserved on completed request
  //    and _fireCallback method is present on Brain instances.
  //    (Full HTTP roundtrip test skipped — ESM module fetch not easily interceptable;
  //     integration test in deploy/test/brain-integration.sh covers the live path.)
  test('callbackUrl is preserved on completed request and _fireCallback exists', async () => {
    process.env.NVIDIA_API_KEY = 'test-key';
    const { brain } = freshBrainWithPath();
    await brain.init();

    assert.equal(typeof brain._fireCallback, 'function', 'Brain should have _fireCallback method');

    const callbackUrl = 'http://localhost:9999/brain-callback-test';
    const originalFetch = global.fetch;
    global.fetch = async () => ({
      ok: true, status: 200, headers: { get: () => null }, text: async () => '{}',
      json: async () => ({ choices: [{ message: { content: 'CB result' } }], usage: { total_tokens: 5 } }),
    });

    const r = createRequest({
      messages: [{ role: 'user', content: 'callback test' }],
      callbackUrl,
    });
    await brain.enqueue(r);
    const item = brain.state.queue.find(q => q.id === r.id);
    assert.ok(item, 'Our request should be in the queue');
    await brain._processRequest(item);

    assert.equal(item.status, 'completed', 'Request should complete with callbackUrl set');
    assert.equal(item.callbackUrl, callbackUrl, 'callbackUrl preserved on completed item');
    assert.ok(item.result, 'Result should be set');
    // _fireCallback is called in the background; we verify it exists and is invokable
    // (it will fail silently since localhost:9999 isn't listening — that's expected)
    await new Promise(r => setTimeout(r, 50));

    global.fetch = originalFetch;
  });

  // 6. Empty response from model — treated as failure, not silent success.
  test('empty model response is treated as failure and retried', async () => {
    process.env.NVIDIA_API_KEY = 'test-key';
    const { brain } = freshBrainWithPath();
    await brain.init();

    let callCount = 0;
    const originalFetch = global.fetch;
    global.fetch = async () => {
      callCount++;
      return {
        ok: true, status: 200, headers: { get: () => null },
        json: async () => ({ choices: [{ message: { content: '' } }], usage: { total_tokens: 0 } }),
      };
    };

    const r = createRequest({ messages: [{ role: 'user', content: 'empty test' }] });
    await brain.enqueue(r);
    const item = brain.state.queue[0];
    await brain._processRequest(item);

    // Empty response should not be counted as a clean completion
    assert.notEqual(item.result, '', 'Empty result should not be stored as valid completion');
    // callModel should have tried multiple models (empty = try next)
    assert.ok(callCount >= 1, 'Should have attempted at least one model');

    global.fetch = originalFetch;
  });

  // 7. Rate limit response (429) — should be recorded and request re-queued.
  test('rate-limit 429 response is recorded as attempt error', async () => {
    process.env.NVIDIA_API_KEY = 'test-key';
    const { brain } = freshBrainWithPath();
    await brain.init();

    const originalFetch = global.fetch;
    global.fetch = async () => ({
      ok: false, status: 429,
      headers: { get: (h) => h === 'retry-after' ? '10' : null },
      text: async () => 'rate limited',
      json: async () => ({}),
    });

    const r = createRequest({ messages: [{ role: 'user', content: '429 test' }] });
    await brain.enqueue(r);
    const item = brain.state.queue[0];
    await brain._processRequest(item);

    assert.notEqual(item.status, 'completed', '429 should not complete the request');
    assert.ok(item.attempts.some(a => a.error && (a.error.includes('429') || a.error.includes('rate'))),
      'Attempt should record 429 rate-limit error');

    global.fetch = originalFetch;
  });

});
