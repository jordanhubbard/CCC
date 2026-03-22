/**
 * rcc/brain — RCC LLM Request Queue + Retry Engine
 *
 * The autonomous nervous system of Rocky Command Center.
 * Never gives up. Always making progress, even if slowly.
 *
 * Model fallback chain (in order):
 *   1. nvidia/azure/anthropic/claude-sonnet-4-6  (best quality)
 *   2. nvcf/meta/llama-3.3-70b-instruct          (fast fallback)
 *   3. nvidia/nvidia/llama-3.3-nemotron-super-49b-v1.5 (last resort)
 */

import { readFile, writeFile } from 'fs/promises';
import { existsSync } from 'fs';
import { EventEmitter } from 'events';

// ── Configuration ──────────────────────────────────────────────────────────

const NVIDIA_API_BASE = process.env.NVIDIA_API_BASE || 'https://inference-api.nvidia.com/v1';
const NVIDIA_API_KEY  = process.env.NVIDIA_API_KEY  || '';
const STATE_PATH      = process.env.BRAIN_STATE_PATH || './brain-state.json';
const TICK_MS         = parseInt(process.env.BRAIN_TICK_MS || '30000', 10);

const MODEL_CHAIN = [
  {
    id: 'nvidia/azure/anthropic/claude-sonnet-4-6',
    maxTokensPerMin: 60000,    // conservative — actual limit is higher but we share with agents
    maxRequestsPerMin: 20,
    label: 'Claude Sonnet',
  },
  {
    id: 'nvcf/meta/llama-3.3-70b-instruct',
    maxTokensPerMin: 100000,
    maxRequestsPerMin: 60,
    label: 'Llama 70B',
  },
  {
    id: 'nvidia/nvidia/llama-3.3-nemotron-super-49b-v1.5',
    maxTokensPerMin: 80000,
    maxRequestsPerMin: 40,
    label: 'Nemotron 49B',
  },
];

// ── Leaky Bucket Rate Limiter ──────────────────────────────────────────────

class LeakyBucket {
  constructor({ maxTokensPerMin, maxRequestsPerMin }) {
    this.maxTokensPerMin = maxTokensPerMin;
    this.maxRequestsPerMin = maxRequestsPerMin;
    this.tokenCount = 0;
    this.requestCount = 0;
    this.windowStart = Date.now();
  }

  _tick() {
    const now = Date.now();
    if (now - this.windowStart > 60000) {
      this.tokenCount = 0;
      this.requestCount = 0;
      this.windowStart = now;
    }
  }

  canSend(estimatedTokens = 1000) {
    this._tick();
    return (
      this.tokenCount + estimatedTokens <= this.maxTokensPerMin &&
      this.requestCount + 1 <= this.maxRequestsPerMin
    );
  }

  record(tokensUsed) {
    this._tick();
    this.tokenCount += tokensUsed;
    this.requestCount += 1;
  }

  waitMs(estimatedTokens = 1000) {
    this._tick();
    const elapsed = Date.now() - this.windowStart;
    const remaining = 60000 - elapsed;
    if (this.canSend(estimatedTokens)) return 0;
    return remaining + 100; // wait until next window + small buffer
  }
}

// ── Brain State ───────────────────────────────────────────────────────────

const DEFAULT_STATE = {
  queue: [],         // pending LLM requests
  completed: [],     // last 100 completed requests
  modelStatus: {},   // per-model health: { lastSuccess, lastError, consecutiveErrors }
  lastTick: null,
  tickCount: 0,
};

async function loadState(path = STATE_PATH) {
  if (!existsSync(path)) return { ...DEFAULT_STATE };
  try {
    return JSON.parse(await readFile(path, 'utf8'));
  } catch {
    return { ...DEFAULT_STATE };
  }
}

async function saveState(state, path = STATE_PATH) {
  state.lastTick = new Date().toISOString();
  await writeFile(path, JSON.stringify(state, null, 2));
}

// ── LLM Request ───────────────────────────────────────────────────────────

/**
 * A brain request item:
 * {
 *   id: string,
 *   messages: [{role, content}],
 *   maxTokens: number,
 *   priority: 'high' | 'normal' | 'low',
 *   created: ISO,
 *   attempts: [],   // [{model, ts, error?}]
 *   status: 'pending' | 'in-progress' | 'completed' | 'failed',
 *   result: string | null,
 *   completedAt: ISO | null,
 *   callbackUrl: string | null,   // optional: POST result here when done
 *   metadata: {},                 // caller-provided context
 * }
 */

export function createRequest({ messages, maxTokens = 1024, priority = 'normal', callbackUrl = null, metadata = {} }) {
  return {
    id: `brain-${Date.now()}-${Math.random().toString(36).slice(2, 7)}`,
    messages,
    maxTokens,
    priority,
    created: new Date().toISOString(),
    attempts: [],
    status: 'pending',
    result: null,
    completedAt: null,
    callbackUrl,
    metadata,
  };
}

// ── Model Call ────────────────────────────────────────────────────────────

async function callModel(model, messages, maxTokens, timeoutMs = 30000) {
  const ctrl = new AbortController();
  const timer = setTimeout(() => ctrl.abort(), timeoutMs);

  try {
    const resp = await fetch(`${NVIDIA_API_BASE}/chat/completions`, {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        'Authorization': `Bearer ${NVIDIA_API_KEY}`,
      },
      body: JSON.stringify({
        model: model.id,
        messages,
        max_tokens: maxTokens,
      }),
      signal: ctrl.signal,
    });

    clearTimeout(timer);

    if (resp.status === 429) {
      const retryAfter = parseInt(resp.headers.get('retry-after') || '30', 10);
      throw Object.assign(new Error('Rate limited'), { code: 'RATE_LIMITED', retryAfterMs: retryAfter * 1000 });
    }

    if (!resp.ok) {
      throw Object.assign(new Error(`HTTP ${resp.status}`), { code: 'HTTP_ERROR', status: resp.status });
    }

    const data = await resp.json();
    const text = data?.choices?.[0]?.message?.content || '';
    const tokensUsed = data?.usage?.total_tokens || Math.ceil(text.length / 4);
    return { text, tokensUsed };

  } catch (err) {
    clearTimeout(timer);
    if (err.name === 'AbortError') {
      throw Object.assign(new Error('Request timed out'), { code: 'TIMEOUT' });
    }
    throw err;
  }
}

// ── Brain Engine ──────────────────────────────────────────────────────────

export class Brain extends EventEmitter {
  constructor({ statePath } = {}) {
    super();
    this.statePath = statePath || STATE_PATH;
    this.state = null;
    this.buckets = Object.fromEntries(MODEL_CHAIN.map(m => [m.id, new LeakyBucket(m)]));
    this.running = false;
    this._tickTimer = null;
  }

  async init() {
    this.state = await loadState(this.statePath);
    console.log(`[brain] Initialized (${this.statePath}). Queue: ${this.state.queue.length} pending, ${this.state.completed.length} completed.`);
    return this;
  }

  async enqueue(request) {
    this.state.queue.push(request);
    // Sort: high > normal > low, then by created time
    const ORDER = { high: 0, normal: 1, low: 2 };
    this.state.queue.sort((a, b) => {
      const po = (ORDER[a.priority] ?? 1) - (ORDER[b.priority] ?? 1);
      if (po !== 0) return po;
      return new Date(a.created) - new Date(b.created);
    });
    await saveState(this.state, this.statePath);
    this.emit('enqueued', request);
    console.log(`[brain] Enqueued ${request.id} (${request.priority}). Queue depth: ${this.state.queue.length}`);
    return request.id;
  }

  getStatus() {
    return {
      ok: true,
      queueDepth: this.state.queue.length,
      completedCount: this.state.completed.length,
      lastTick: this.state.lastTick,
      tickCount: this.state.tickCount,
      models: MODEL_CHAIN.map(m => ({
        id: m.id,
        label: m.label,
        status: this.state.modelStatus[m.id] || { consecutiveErrors: 0 },
        bucket: {
          requests: this.buckets[m.id].requestCount,
          tokens: this.buckets[m.id].tokenCount,
        },
      })),
    };
  }

  start() {
    if (this.running) return;
    this.running = true;
    console.log(`[brain] Starting. Tick interval: ${TICK_MS}ms`);
    this._schedule();
  }

  stop() {
    this.running = false;
    if (this._tickTimer) clearTimeout(this._tickTimer);
    console.log('[brain] Stopped.');
  }

  _schedule() {
    if (!this.running) return;
    this._tickTimer = setTimeout(async () => {
      try {
        await this._tick();
      } catch (err) {
        console.error('[brain] Tick error:', err.message);
      }
      this._schedule();
    }, TICK_MS);
  }

  async _tick() {
    this.state.tickCount = (this.state.tickCount || 0) + 1;
    const pending = this.state.queue.filter(r => r.status === 'pending');

    if (pending.length === 0) {
      await saveState(this.state, this.statePath);
      return;
    }

    console.log(`[brain] Tick #${this.state.tickCount}. Processing ${pending.length} pending request(s).`);

    for (const request of pending) {
      await this._processRequest(request);
      // Save after each request to avoid losing progress
      await saveState(this.state, this.statePath);
    }

    // Trim completed to last 100
    if (this.state.completed.length > 100) {
      this.state.completed = this.state.completed.slice(-100);
    }

    await saveState(this.state, this.statePath);
  }

  async _processRequest(request) {
    request.status = 'in-progress';

    // Determine which models to try (skip models with too many consecutive errors)
    const availableModels = MODEL_CHAIN.filter(m => {
      const s = this.state.modelStatus[m.id] || {};
      return (s.consecutiveErrors || 0) < 5; // skip after 5 consecutive failures
    });

    if (availableModels.length === 0) {
      console.warn('[brain] All models degraded. Marking request as failed temporarily.');
      request.status = 'pending'; // keep in queue, try again next tick
      return;
    }

    for (const model of availableModels) {
      const bucket = this.buckets[model.id];
      const estimatedTokens = request.maxTokens + Math.ceil(
        request.messages.reduce((n, m) => n + m.content.length, 0) / 4
      );

      if (!bucket.canSend(estimatedTokens)) {
        const wait = bucket.waitMs(estimatedTokens);
        console.log(`[brain] ${model.label} rate-limited. Next window in ${Math.ceil(wait/1000)}s. Trying next model.`);
        continue;
      }

      const attemptTs = new Date().toISOString();
      try {
        console.log(`[brain] ${request.id} → ${model.label}`);
        const { text, tokensUsed } = await callModel(model, request.messages, request.maxTokens);

        bucket.record(tokensUsed);
        this.state.modelStatus[model.id] = {
          consecutiveErrors: 0,
          lastSuccess: attemptTs,
        };

        request.attempts.push({ model: model.id, ts: attemptTs, tokensUsed });
        request.status = 'completed';
        request.result = text;
        request.completedAt = new Date().toISOString();

        // Move to completed
        this.state.queue = this.state.queue.filter(r => r.id !== request.id);
        this.state.completed.push(request);

        this.emit('completed', request);
        console.log(`[brain] ${request.id} completed via ${model.label} (${tokensUsed} tokens)`);

        // Fire callback if requested
        if (request.callbackUrl) {
          this._fireCallback(request).catch(err =>
            console.warn(`[brain] Callback failed for ${request.id}: ${err.message}`)
          );
        }

        return; // done — exit model loop

      } catch (err) {
        const ms = this.state.modelStatus[model.id] || {};
        ms.consecutiveErrors = (ms.consecutiveErrors || 0) + 1;
        ms.lastError = attemptTs;
        ms.lastErrorMessage = err.message;
        this.state.modelStatus[model.id] = ms;

        request.attempts.push({ model: model.id, ts: attemptTs, error: err.message, code: err.code });

        if (err.code === 'RATE_LIMITED') {
          console.warn(`[brain] ${model.label} rate limited (retry after ${err.retryAfterMs}ms). Trying next.`);
        } else if (err.code === 'TIMEOUT') {
          console.warn(`[brain] ${model.label} timed out. Trying next.`);
        } else {
          console.warn(`[brain] ${model.label} error: ${err.message}. Trying next.`);
        }
      }
    }

    // All models failed this round — put back as pending, will retry next tick
    console.warn(`[brain] ${request.id}: all models failed this round. Will retry next tick.`);
    request.status = 'pending';
    request.attempts.push({ ts: new Date().toISOString(), note: 'All models failed, queued for retry' });
  }

  async _fireCallback(request) {
    await fetch(request.callbackUrl, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        requestId: request.id,
        result: request.result,
        model: request.attempts.find(a => !a.error)?.model,
        completedAt: request.completedAt,
        metadata: request.metadata,
      }),
    });
  }
}

// ── Standalone entry point ────────────────────────────────────────────────

if (process.argv[1] === new URL(import.meta.url).pathname) {
  const brain = new Brain();
  await brain.init();
  brain.start();

  // Graceful shutdown
  process.on('SIGTERM', () => { brain.stop(); process.exit(0); });
  process.on('SIGINT',  () => { brain.stop(); process.exit(0); });

  console.log('[brain] Running. Send SIGTERM or SIGINT to stop.');
}
