#!/usr/bin/env node
/**
 * ollama-watchdog.mjs — periodic health check for ollama models on sparky
 * Checks qwen2.5-coder:32b and qwen3-coder every 15 min.
 * Restarts degraded models, surfaces status in RCC heartbeat.
 *
 * Run: node ollama-watchdog.mjs [--once]
 * Cron: add to openclaw cron or run as systemd service.
 */

import { execFile } from 'child_process';
import { promisify } from 'util';

const execFileAsync = promisify(execFile);
const OLLAMA_URL = process.env.OLLAMA_URL || 'http://localhost:11434';
const RCC_URL    = process.env.RCC_URL    || 'http://146.190.134.110:8789';
const RCC_TOKEN  = process.env.RCC_AUTH_TOKEN || 'wq-5dcad756f6d3e345c00b5cb3dfcbdedb';
const AGENT_NAME = process.env.AGENT_NAME || 'natasha';
const INTERVAL_MS = 15 * 60 * 1000; // 15 min
const TIMEOUT_MS  = 90_000; // cold-start for 18-32GB models can take 60-90s
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
  // Stop via API (graceful unload from VRAM)
  try {
    await fetch(`${OLLAMA_URL}/api/generate`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ model, keep_alive: 0 }),
    });
  } catch (_) {}
  // Brief pause to let model unload
  await new Promise(r => setTimeout(r, 3000));
  // Warm it back up via API (not `ollama run` which opens interactive mode)
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 120_000);
  try {
    await fetch(`${OLLAMA_URL}/api/generate`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ model, prompt: 'OK', stream: false, options: { num_predict: 1 } }),
      signal: controller.signal,
    });
    clearTimeout(timer);
    console.log(`[watchdog] ${model} reloaded`);
  } catch (err) {
    clearTimeout(timer);
    console.log(`[watchdog] ${model} reload failed: ${err.message}`);
  }
}

async function pushStatus() {
  const payload = { status: 'online', host: 'sparky', ts: new Date().toISOString(), ollama: {} };
  for (const [m, s] of Object.entries(state)) payload.ollama[m] = s.status;
  try {
    await fetch(`${RCC_URL}/api/heartbeat/${AGENT_NAME}`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', 'Authorization': `Bearer ${RCC_TOKEN}` },
      body: JSON.stringify(payload),
    });
  } catch (_) {}
}

async function runChecks() {
  console.log(`[watchdog] ${new Date().toISOString()} — checking ${MODELS_TO_CHECK.length} models`);
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
