#!/usr/bin/env node
/**
 * ollama-watchdog.mjs — periodic health check for ollama models on sparky
 * Checks qwen2.5-coder:32b and qwen3-coder every 15 min.
 * Restarts degraded models, surfaces status in RCC heartbeat.
 * Also logs GPU utilization time-series to ~/.openclaw/workspace/telemetry/gpu-metrics.jsonl
 *
 * Run: node ollama-watchdog.mjs [--once]
 * Cron: add to openclaw cron or run as systemd service.
 */

import { execFile, exec } from 'child_process';
import { promisify } from 'util';
import { appendFileSync, existsSync, mkdirSync, renameSync, statSync } from 'fs';
import { createGzip } from 'zlib';
import { createReadStream, createWriteStream } from 'fs';
import { pipeline } from 'stream/promises';
import os from 'os';

const execFileAsync = promisify(execFile);
const execAsync = promisify(exec);
const OLLAMA_URL = process.env.OLLAMA_URL || 'http://localhost:11434';

// GPU time-series JSONL log
const TELEMETRY_DIR = `${os.homedir()}/.openclaw/workspace/telemetry`;
const GPU_METRICS_PATH = `${TELEMETRY_DIR}/gpu-metrics.jsonl`;
const WEEK_MS = 7 * 24 * 60 * 60 * 1000;
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

async function collectGpuMetrics() {
  try {
    // GB10 unified memory — nvidia-smi may return N/A for some fields
    const { stdout } = await execAsync(
      'nvidia-smi --query-gpu=temperature.gpu,power.draw,utilization.gpu,memory.used,memory.total --format=csv,noheader,nounits',
      { timeout: 5000 }
    );
    const parts = stdout.trim().split(',').map(s => s.trim());
    const parseNum = v => v === '[N/A]' || v === 'N/A' || v === '' ? null : parseFloat(v);
    return {
      temp_c:      parseNum(parts[0]),
      power_w:     parseNum(parts[1]),
      util_pct:    parseNum(parts[2]),
      mem_used_mb: parseNum(parts[3]),
      mem_total_mb:parseNum(parts[4]),
    };
  } catch (_) {
    return { temp_c: null, power_w: null, util_pct: null, mem_used_mb: null, mem_total_mb: null };
  }
}

async function maybeRotateGpuLog() {
  if (!existsSync(GPU_METRICS_PATH)) return;
  const st = statSync(GPU_METRICS_PATH);
  if (Date.now() - st.mtimeMs < WEEK_MS) return;
  const rotated = `${GPU_METRICS_PATH}.${new Date().toISOString().slice(0, 10)}.gz`;
  try {
    await pipeline(createReadStream(GPU_METRICS_PATH), createGzip(), createWriteStream(rotated));
    renameSync(`${GPU_METRICS_PATH}`, `${GPU_METRICS_PATH}.rotated`);
    const { unlinkSync } = await import('fs');
    unlinkSync(`${GPU_METRICS_PATH}.rotated`);
    console.log(`[watchdog] rotated gpu-metrics.jsonl → ${rotated}`);
  } catch (err) {
    console.log(`[watchdog] gpu-metrics rotation failed: ${err.message}`);
  }
}

async function appendGpuMetrics() {
  if (!existsSync(TELEMETRY_DIR)) mkdirSync(TELEMETRY_DIR, { recursive: true });
  await maybeRotateGpuLog();
  const gpu = await collectGpuMetrics();
  const ollama_model_count = MODELS_TO_CHECK.filter(m => state[m]?.status === 'ok').length;
  const entry = {
    ts: new Date().toISOString(),
    ...gpu,
    ollama_model_count,
  };
  try {
    appendFileSync(GPU_METRICS_PATH, JSON.stringify(entry) + '\n');
  } catch (err) {
    console.log(`[watchdog] gpu-metrics append failed: ${err.message}`);
  }
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
  await appendGpuMetrics();
}

const once = process.argv.includes('--once');
if (once) {
  runChecks().then(() => process.exit(0)).catch(e => { console.error(e); process.exit(1); });
} else {
  runChecks(); // immediate first run
  setInterval(runChecks, INTERVAL_MS);
  console.log(`[watchdog] running every ${INTERVAL_MS / 60000} min. Ctrl+C to stop.`);
}
