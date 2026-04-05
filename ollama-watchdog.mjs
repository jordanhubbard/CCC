#!/usr/bin/env node
/**
 * ollama-watchdog.mjs — periodic health check for ollama models on sparky
 * Checks qwen2.5-coder:32b and qwen3-coder every 15 min.
 * Restarts degraded models, surfaces status in CCC heartbeat.
 *
 * Run: node ollama-watchdog.mjs [--once]
 * Cron: add to openclaw cron or run as systemd service.
 */

import { execFile } from 'child_process';
import { promisify } from 'util';

const execFileAsync = promisify(execFile);
const OLLAMA_URL = process.env.OLLAMA_URL || 'http://localhost:11434';
const CCC_URL    = process.env.CCC_URL    || 'http://146.190.134.110:8789';
const RCC_TOKEN  = process.env.CCC_AUTH_TOKEN || 'wq-5dcad756f6d3e345c00b5cb3dfcbdedb';
const AGENT_NAME = process.env.AGENT_NAME || 'natasha';
const INTERVAL_MS = 15 * 60 * 1000; // 15 min
const TIMEOUT_MS  = 30_000;
const TEST_PROMPT = 'Reply with exactly: OK';

const MODELS_TO_CHECK = [
  'qwen2.5-coder:32b',
  'qwen3-coder:latest',
];

const state = {};
for (const m of MODELS_TO_CHECK) state[m] = { status: 'unknown', lastCheck: null, restarts: 0 };

async function checkModel(model) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), TIMEOUT_MS);
  try {
    const t0 = Date.now();
    const res = await fetch(`${OLLAMA_URL}/api/generate`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ model, prompt: TEST_PROMPT, stream: false }),
      signal: controller.signal,
    });
    clearTimeout(timer);
    const elapsed = Date.now() - t0;
    if (!res.ok) return { ok: false, reason: `HTTP ${res.status}`, elapsed };
    const data = await res.json();
    const resp = (data.response || '').trim();
    const malformed = !resp || resp.length > 200;
    return { ok: !malformed, reason: malformed ? `malformed: ${resp.slice(0,40)}` : 'ok', elapsed };
  } catch (err) {
    clearTimeout(timer);
    return { ok: false, reason: err.name === 'AbortError' ? 'timeout >30s' : err.message, elapsed: TIMEOUT_MS };
  }
}

async function restartModel(model) {
  console.log(`[watchdog] restarting ${model}…`);
  try {
    await execFileAsync('ollama', ['stop', model], { timeout: 15000 });
  } catch (_) {}
  try {
    await execFileAsync('ollama', ['run', model, '--keepalive', '0', '--verbose'], {
      timeout: 60000,
      env: { ...process.env, OLLAMA_KEEP_ALIVE: '0' },
    });
  } catch (_) {}
}

async function pushStatus() {
  const payload = { status: 'online', host: 'sparky', ts: new Date().toISOString(), ollama: {} };
  for (const [m, s] of Object.entries(state)) payload.ollama[m] = s.status;
  try {
    await fetch(`${CCC_URL}/api/heartbeat/${AGENT_NAME}`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', 'Authorization': `Bearer ${RCC_TOKEN}` },
      body: JSON.stringify(payload),
    });
  } catch (_) {}
}

/** Warm up models after a fresh ollama restart (avoids cold-start latency on first real request) */
async function warmUpModels() {
  console.log(`[watchdog] 🔥 ollama freshly restarted — warming up models…`);
  for (const model of MODELS_TO_CHECK) {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), 45_000);
    try {
      const t0 = Date.now();
      await fetch(`${OLLAMA_URL}/api/generate`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ model, prompt: 'ping', stream: false }),
        signal: controller.signal,
      });
      clearTimeout(timer);
      console.log(`[watchdog] 🔥 warmed ${model} in ${Date.now() - t0}ms`);
    } catch (err) {
      clearTimeout(timer);
      console.log(`[watchdog] ⚠️  warm-up ${model} failed: ${err.message}`);
    }
  }
}

/** Check if ollama was recently restarted (uptime < 5 min) */
async function checkOllamaUptime() {
  try {
    const res = await fetch(`${OLLAMA_URL}/`, { signal: AbortSignal.timeout(5000) });
    const text = await res.text();
    // ollama responds with uptime in /api/version or just a text banner — no uptime field
    // Instead, track our own "first-seen" time to detect restarts
    return null; // no reliable uptime from API
  } catch (_) {
    return null;
  }
}

// Track when we last saw ollama alive to detect restarts
let lastOllamaAliveTs = null;

async function runChecks() {
  console.log(`[watchdog] ${new Date().toISOString()} — checking ${MODELS_TO_CHECK.length} models`);
  // Detect fresh ollama start: if we previously saw it as down/unknown, warm up
  let ollamaWasDown = false;
  for (const m of MODELS_TO_CHECK) {
    if (state[m].status === 'unknown' || state[m].status === 'down') { ollamaWasDown = true; break; }
  }

  for (const model of MODELS_TO_CHECK) {
    const result = await checkModel(model);
    state[model].lastCheck = new Date().toISOString();
    if (result.ok) {
      state[model].status = 'ok';
      console.log(`[watchdog] ✅ ${model} ok (${result.elapsed}ms)`);
    } else {
      console.log(`[watchdog] ⚠️  ${model} degraded: ${result.reason}`);
      state[model].status = 'degraded';
      state[model].restarts++;
      await restartModel(model);
      state[model].status = 'restarting';
    }
  }

  // If any model came back from unknown/down state, trigger warm-up
  const allNowUp = MODELS_TO_CHECK.every(m => state[m].status === 'ok');
  if (ollamaWasDown && allNowUp) {
    await warmUpModels();
  }

  await pushStatus();
}

const once = process.argv.includes('--once');
if (once) {
  runChecks().then(() => process.exit(0)).catch(e => { console.error(e); process.exit(1); });
} else {
  runChecks(); // immediate first run
  setInterval(runChecks, INTERVAL_MS);
  console.log(`[watchdog] running every ${INTERVAL_MS / 60000} min. Ctrl+C to stop.`);
}
