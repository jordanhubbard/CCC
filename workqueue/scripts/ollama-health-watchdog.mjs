#!/usr/bin/env node
/**
 * ollama-health-watchdog.mjs — Periodic health check for ollama models on sparky
 *
 * Checks ollama model health every 15 minutes (or on-demand).
 * If a model is slow (>30s) or produces garbled output, attempts restart.
 * Surfaces ollama_status in CCC heartbeat payload.
 *
 * Usage:
 *   node ollama-health-watchdog.mjs [--once] [--model qwen2.5-coder:32b]
 *
 * Environment:
 *   OLLAMA_BASE_URL  default: http://localhost:11434
 *   CCC_URL          default: http://100.89.199.14:8789
 *   RCC_TOKEN        default: wq-5dcad756f6d3e345c00b5cb3dfcbdedb
 *   HEALTH_INTERVAL  default: 900000 (15 min in ms)
 *   HEALTH_TIMEOUT   default: 30000 (30s in ms)
 *   HEALTH_MODEL     default: qwen2.5-coder:32b
 */

import { execSync, spawn } from 'node:child_process';
import { readFileSync, writeFileSync, existsSync } from 'node:fs';
import { parseArgs } from 'node:util';

const OLLAMA_BASE_URL  = process.env.OLLAMA_BASE_URL  || 'http://localhost:11434';
const CCC_URL          = process.env.CCC_URL          || 'http://100.89.199.14:8789';
const RCC_TOKEN        = process.env.RCC_TOKEN        || 'wq-5dcad756f6d3e345c00b5cb3dfcbdedb';
const HEALTH_INTERVAL  = parseInt(process.env.HEALTH_INTERVAL  || '900000', 10);
const HEALTH_TIMEOUT   = parseInt(process.env.HEALTH_TIMEOUT   || '30000', 10);
const HEALTH_MODEL     = process.env.HEALTH_MODEL     || 'qwen2.5-coder:32b';
const STATE_FILE       = process.env.STATE_FILE       || '/tmp/ollama-health-state.json';

// Test prompt: simple, deterministic, fast
const TEST_PROMPT = 'def add(a, b): return';
const EXPECTED_FRAGMENT = /return|a \+|b/i;  // any reasonable completion

const { values: args } = parseArgs({
  options: {
    once:  { type: 'boolean', default: false },
    model: { type: 'string',  default: HEALTH_MODEL },
    verbose: { type: 'boolean', default: false },
  },
  strict: false,
});

const model = args.model || HEALTH_MODEL;

// ── State persistence ──────────────────────────────────────────────────────────

function loadState() {
  try {
    if (existsSync(STATE_FILE)) return JSON.parse(readFileSync(STATE_FILE, 'utf8'));
  } catch {}
  return { status: 'unknown', lastCheck: null, lastRestart: null, consecutiveFails: 0, history: [] };
}

function saveState(state) {
  try { writeFileSync(STATE_FILE, JSON.stringify(state, null, 2)); } catch {}
}

// ── Ollama API helpers ─────────────────────────────────────────────────────────

async function checkOllamaRunning() {
  try {
    const resp = await fetch(`${OLLAMA_BASE_URL}/api/tags`, {
      signal: AbortSignal.timeout(5000),
    });
    return resp.ok;
  } catch {
    return false;
  }
}

async function getLoadedModels() {
  try {
    const resp = await fetch(`${OLLAMA_BASE_URL}/api/ps`, {
      signal: AbortSignal.timeout(5000),
    });
    if (!resp.ok) return [];
    const data = await resp.json();
    return (data.models || []).map(m => m.name);
  } catch {
    return [];
  }
}

async function testModelGeneration(modelName, timeoutMs) {
  const start = Date.now();
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);

  try {
    const resp = await fetch(`${OLLAMA_BASE_URL}/api/generate`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        model: modelName,
        prompt: TEST_PROMPT,
        stream: false,
        options: { num_predict: 20, temperature: 0 },
      }),
      signal: controller.signal,
    });

    clearTimeout(timer);
    const elapsed = Date.now() - start;

    if (!resp.ok) {
      return { ok: false, reason: `HTTP ${resp.status}`, elapsed };
    }

    const data = await resp.json();
    const output = (data.response || '').trim();

    if (!output || output.length < 1) {
      return { ok: false, reason: 'empty_response', elapsed, output };
    }

    // Check for garbled output (non-ASCII garbage, null bytes)
    const garbled = /[\x00-\x08\x0e-\x1f\x7f-\x9f]/.test(output);
    if (garbled) {
      return { ok: false, reason: 'garbled_output', elapsed, output: output.substring(0, 100) };
    }

    return { ok: true, elapsed, output: output.substring(0, 100) };

  } catch (err) {
    clearTimeout(timer);
    const elapsed = Date.now() - start;
    if (err.name === 'AbortError') {
      return { ok: false, reason: `timeout_${timeoutMs}ms`, elapsed };
    }
    return { ok: false, reason: err.message, elapsed };
  }
}

// ── Restart logic ─────────────────────────────────────────────────────────────

async function restartModel(modelName) {
  console.log(`[watchdog] Attempting to restart model: ${modelName}`);
  try {
    // Unload the model
    await fetch(`${OLLAMA_BASE_URL}/api/generate`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ model: modelName, keep_alive: 0, prompt: '' }),
      signal: AbortSignal.timeout(10000),
    }).catch(() => {});

    // Small wait for cleanup
    await new Promise(r => setTimeout(r, 3000));

    // Warm up the model with a trivial generation
    const warmResp = await fetch(`${OLLAMA_BASE_URL}/api/generate`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        model: modelName,
        prompt: 'Hi',
        stream: false,
        options: { num_predict: 3 },
      }),
      signal: AbortSignal.timeout(60000),
    });

    if (warmResp.ok) {
      console.log(`[watchdog] Model ${modelName} reloaded successfully`);
      return { success: true };
    } else {
      return { success: false, reason: `warm_up_http_${warmResp.status}` };
    }
  } catch (err) {
    return { success: false, reason: err.message };
  }
}

// ── CCC heartbeat with ollama status ──────────────────────────────────────────

async function reportToRCC(status, detail) {
  try {
    await fetch(`${CCC_URL}/api/heartbeat/natasha`, {
      method: 'POST',
      headers: {
        'Authorization': `Bearer ${RCC_TOKEN}`,
        'Content-Type': 'application/json',
      },
      body: JSON.stringify({
        status: 'online',
        host: 'sparky',
        ts: new Date().toISOString(),
        services: {
          ollama_status: status,  // ok | degraded | restarting | unreachable
          ollama_model: model,
          ollama_detail: detail,
        },
      }),
      signal: AbortSignal.timeout(8000),
    });
  } catch {
    // Heartbeat failure is non-fatal
  }
}

// ── Main health check loop ────────────────────────────────────────────────────

async function runHealthCheck() {
  const state = loadState();
  const ts = new Date().toISOString();
  console.log(`[watchdog] ${ts} — Checking ollama model health (${model})`);

  // Step 1: Is ollama daemon up?
  const ollamaRunning = await checkOllamaRunning();
  if (!ollamaRunning) {
    console.log('[watchdog] Ollama daemon unreachable on localhost:11434');
    state.status = 'unreachable';
    state.lastCheck = ts;
    state.consecutiveFails++;
    state.history.unshift({ ts, status: 'unreachable', reason: 'daemon_unreachable' });
    state.history = state.history.slice(0, 20);
    saveState(state);
    await reportToRCC('unreachable', 'daemon_unreachable');
    return state;
  }

  // Step 2: Test model generation with timeout
  console.log(`[watchdog] Testing ${model} (timeout: ${HEALTH_TIMEOUT}ms)...`);
  const result = await testModelGeneration(model, HEALTH_TIMEOUT);

  if (args.verbose) {
    console.log(`[watchdog] Test result:`, result);
  }

  if (result.ok) {
    state.status = 'ok';
    state.consecutiveFails = 0;
    state.lastCheck = ts;
    state.history.unshift({ ts, status: 'ok', elapsed: result.elapsed });
    state.history = state.history.slice(0, 20);
    saveState(state);
    console.log(`[watchdog] ✓ Model healthy (${result.elapsed}ms)`);
    await reportToRCC('ok', `response_${result.elapsed}ms`);
    return state;
  }

  // Step 3: Model failed — attempt restart if < 3 restarts in last hour
  console.log(`[watchdog] ✗ Model degraded: ${result.reason} (${result.elapsed}ms)`);
  state.consecutiveFails++;
  state.lastCheck = ts;

  const oneHourAgo = Date.now() - 3600000;
  const recentRestarts = (state.history || []).filter(
    h => h.status === 'restarting' && new Date(h.ts).getTime() > oneHourAgo
  ).length;

  if (recentRestarts >= 3) {
    console.log(`[watchdog] Too many restarts (${recentRestarts}/hr) — marking degraded, not restarting`);
    state.status = 'degraded';
    state.history.unshift({ ts, status: 'degraded', reason: result.reason, elapsed: result.elapsed });
    state.history = state.history.slice(0, 20);
    saveState(state);
    await reportToRCC('degraded', `${result.reason}_restart_throttled`);
    return state;
  }

  console.log(`[watchdog] Restarting model (${recentRestarts} recent restarts)...`);
  state.status = 'restarting';
  state.lastRestart = ts;
  state.history.unshift({ ts, status: 'restarting', reason: result.reason });
  state.history = state.history.slice(0, 20);
  saveState(state);
  await reportToRCC('restarting', result.reason);

  const restartResult = await restartModel(model);
  const ts2 = new Date().toISOString();

  if (restartResult.success) {
    // Re-test after restart
    const retest = await testModelGeneration(model, HEALTH_TIMEOUT * 2);
    state.status = retest.ok ? 'ok' : 'degraded';
    state.history.unshift({ ts: ts2, status: state.status, reason: 'post_restart_check', elapsed: retest.elapsed });
    console.log(`[watchdog] Post-restart status: ${state.status} (${retest.elapsed}ms)`);
  } else {
    state.status = 'degraded';
    state.history.unshift({ ts: ts2, status: 'degraded', reason: restartResult.reason });
    console.log(`[watchdog] Restart failed: ${restartResult.reason}`);
  }

  state.history = state.history.slice(0, 20);
  saveState(state);
  await reportToRCC(state.status, state.history[0]?.reason || 'post_restart');
  return state;
}

// ── Entry point ───────────────────────────────────────────────────────────────

if (args.once) {
  // Single run mode
  runHealthCheck().then(state => {
    console.log(`[watchdog] Final status: ${state.status}`);
    process.exit(state.status === 'ok' ? 0 : 1);
  }).catch(err => {
    console.error('[watchdog] Fatal error:', err);
    process.exit(2);
  });
} else {
  // Daemon mode — run immediately then every HEALTH_INTERVAL
  console.log(`[watchdog] Starting ollama health watchdog daemon`);
  console.log(`[watchdog] Model: ${model} | Interval: ${HEALTH_INTERVAL}ms | Timeout: ${HEALTH_TIMEOUT}ms`);

  runHealthCheck().catch(console.error);
  setInterval(() => runHealthCheck().catch(console.error), HEALTH_INTERVAL);
}
