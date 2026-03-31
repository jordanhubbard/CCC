/**
 * rcc/brain — RCC LLM Request Queue + Retry Engine
 *
 * The autonomous nervous system of Rocky Command Center.
 * Never gives up. Always making progress, even if slowly.
 *
 * Routes all LLM calls through tokenhub (localhost:8090).
 * Tokenhub handles model selection, failover, and rate limiting.
 */

import { readFile, writeFile } from 'fs/promises';
import { existsSync } from 'fs';
import { EventEmitter } from 'events';

// ── Configuration ──────────────────────────────────────────────────────────

const TOKENHUB_URL     = process.env.TOKENHUB_URL     || 'http://localhost:8090';
const TOKENHUB_API_KEY = process.env.TOKENHUB_API_KEY || process.env.TOKENHUB_AGENT_KEY || 'tokenhub_1b8cdce9904ca32eea3d06624079b5bfd9baf5a0eec4e7d6f3dacdf4b6aeeb15';
// Preferred models in priority order — brain tries each until one succeeds
const BRAIN_MODELS = (process.env.BRAIN_MODELS || 'nemotron,nemotron-peabody,nemotron-sherman,nemotron-snidely,nemotron-dudley,llama-3.3-70b-instruct').split(',').map(s => s.trim()).filter(Boolean);
const STATE_PATH       = process.env.BRAIN_STATE_PATH || './brain-state.json';
const TICK_MS          = parseInt(process.env.BRAIN_TICK_MS || '30000', 10);

// ── Brain State ───────────────────────────────────────────────────────────

const DEFAULT_STATE = {
  queue: [],         // pending LLM requests
  completed: [],     // last 100 completed requests
  modelStatus: {},   // health tracking (kept for schema compat)
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

async function callModelWithName(model, messages, maxTokens, timeoutMs = 30000) {
  const ctrl = new AbortController();
  const timer = setTimeout(() => ctrl.abort(), timeoutMs);

  try {
    const resp = await fetch(`${TOKENHUB_URL}/v1/chat/completions`, {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        'Authorization': `Bearer ${TOKENHUB_API_KEY}`,
      },
      body: JSON.stringify({
        model,
        messages,
        max_tokens: maxTokens,
      }),
      signal: ctrl.signal,
    });

    clearTimeout(timer);

    if (!resp.ok) {
      const body = await resp.text().catch(() => '');
      throw Object.assign(new Error(`HTTP ${resp.status}: ${body.slice(0, 200)}`), { code: 'HTTP_ERROR', status: resp.status });
    }

    const data = await resp.json();
    const text = data?.choices?.[0]?.message?.content || '';
    const tokensUsed = data?.usage?.total_tokens || Math.ceil(text.length / 4);
    return { text, tokensUsed, model };

  } catch (err) {
    clearTimeout(timer);
    if (err.name === 'AbortError') {
      throw Object.assign(new Error('Request timed out'), { code: 'TIMEOUT' });
    }
    throw err;
  }
}

async function callModel(messages, maxTokens, timeoutMs = 30000) {
  let lastErr;
  for (const model of BRAIN_MODELS) {
    try {
      const result = await callModelWithName(model, messages, maxTokens, timeoutMs);
      if (result.text) return result;
      // empty response — try next model
      lastErr = new Error(`Empty response from ${model}`);
    } catch (err) {
      lastErr = err;
      console.warn(`[brain] Model ${model} failed: ${err.message} — trying next`);
    }
  }
  throw lastErr || new Error('All models failed');
}

// ── Brain Engine ──────────────────────────────────────────────────────────

export class Brain extends EventEmitter {
  constructor({ statePath } = {}) {
    super();
    this.statePath = statePath || STATE_PATH;
    this.state = null;
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
      backend: 'tokenhub',
      url: TOKENHUB_URL,
      queueDepth: this.state.queue.length,
      completedCount: this.state.completed.length,
      lastTick: this.state.lastTick,
      tickCount: this.state.tickCount,
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

    const attemptTs = new Date().toISOString();
    try {
      console.log(`[brain] ${request.id} → tokenhub`);
      const { text, tokensUsed } = await callModel(request.messages, request.maxTokens);

      request.attempts.push({ model: 'tokenhub', ts: attemptTs, tokensUsed });
      request.status = 'completed';
      request.result = text;
      request.completedAt = new Date().toISOString();

      // Move to completed
      this.state.queue = this.state.queue.filter(r => r.id !== request.id);
      this.state.completed.push(request);

      this.emit('completed', request);
      console.log(`[brain] ${request.id} completed via tokenhub (${tokensUsed} tokens)`);

      // Fire callback if requested
      if (request.callbackUrl) {
        this._fireCallback(request).catch(err =>
          console.warn(`[brain] Callback failed for ${request.id}: ${err.message}`)
        );
      }

    } catch (err) {
      request.attempts.push({ model: 'tokenhub', ts: attemptTs, error: err.message, code: err.code });
      console.warn(`[brain] ${request.id} tokenhub error: ${err.message}. Will retry next tick.`);
      // Put back as pending — retry next tick
      request.status = 'pending';
    }
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
