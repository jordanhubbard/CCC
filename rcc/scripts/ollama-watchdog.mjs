#!/usr/bin/env node
/**
 * ollama-watchdog.mjs — Ollama model health watchdog for sparky
 *
 * Periodically health-checks ollama models (qwen2.5-coder:32b by default).
 * If response time > RESPONSE_TIMEOUT_MS or output is malformed, restarts the model.
 * Reports ollama_status (ok|degraded|restarting) to the RCC heartbeat payload.
 *
 * Usage:
 *   node ollama-watchdog.mjs
 *
 * Env vars:
 *   OLLAMA_BASE_URL      default: http://localhost:11434
 *   OLLAMA_MODELS        comma-separated, default: qwen2.5-coder:32b
 *   CHECK_INTERVAL_MS    default: 900000 (15 min)
 *   RESPONSE_TIMEOUT_MS  default: 30000 (30s)
 *   RCC_API              default: https://api.yourmom.photos
 *   RCC_AUTH_TOKEN       default: rcc-agent-rocky-20maaghccmbmnby63so
 *   AGENT_NAME           default: natasha
 */

import { execSync, spawn } from 'child_process';
import { writeFileSync, readFileSync, existsSync, appendFileSync, mkdirSync, renameSync, statSync } from 'fs';
import { resolve, dirname } from 'path';

const OLLAMA_BASE_URL     = process.env.OLLAMA_BASE_URL     || 'http://localhost:11434';
const OLLAMA_MODELS       = (process.env.OLLAMA_MODELS      || 'qwen2.5-coder:32b').split(',').map(s => s.trim());
const CHECK_INTERVAL_MS   = parseInt(process.env.CHECK_INTERVAL_MS   || '900000', 10);  // 15 min
const RESPONSE_TIMEOUT_MS = parseInt(process.env.RESPONSE_TIMEOUT_MS || '30000', 10);   // 30s
const RCC_API             = process.env.RCC_API             || 'https://api.yourmom.photos';
const RCC_AUTH_TOKEN      = process.env.RCC_AUTH_TOKEN      || 'rcc-agent-natasha-eeynvasslp8mna9bipx';
const AGENT_NAME          = process.env.AGENT_NAME          || 'natasha';
const STATUS_FILE         = resolve(process.env.STATUS_FILE || '/home/jkh/.rcc/ollama-status.json');
const GPU_METRICS_FILE    = resolve(process.env.GPU_METRICS_FILE || '/home/jkh/.openclaw/workspace/telemetry/gpu-metrics.jsonl');
const GPU_METRICS_MAX_MB  = parseInt(process.env.GPU_METRICS_MAX_MB || '50', 10);  // rotate when > 50MB
const TEST_PROMPT         = 'Say "ok" in one word.';

// GPU spike alerting — notify jkh via Mattermost DM when GPU util is sustained high
const MM_URL              = process.env.MATTERMOST_URL   || 'https://chat.yourmom.photos';
const MM_TOKEN            = process.env.MATTERMOST_TOKEN || '';
const JKH_MM_USER         = process.env.JKH_MM_USER     || 'jkh';  // Mattermost username
const GPU_ALERT_THRESHOLD = parseInt(process.env.GPU_ALERT_THRESHOLD_PCT || '80', 10);   // %
const GPU_ALERT_STRIKES   = parseInt(process.env.GPU_ALERT_STRIKES      || '3', 10);     // checks in a row
const GPU_ALERT_COOLDOWN  = parseInt(process.env.GPU_ALERT_COOLDOWN_MS  || '3600000', 10); // 1hr

let gpuHighStrikes = 0;         // consecutive checks above threshold
let lastAlertTs    = 0;         // timestamp of last alert sent

/** Send a Mattermost DM to jkh */
async function mmDm(message) {
  if (!MM_TOKEN) { log('WARN: no MATTERMOST_TOKEN, skipping DM'); return; }
  try {
    // 1. Resolve jkh user id
    const uRes = await fetch(`${MM_URL}/api/v4/users/username/${JKH_MM_USER}`, {
      headers: { Authorization: `Bearer ${MM_TOKEN}` },
    });
    if (!uRes.ok) { log(`WARN: mm user lookup failed ${uRes.status}`); return; }
    const user = await uRes.json();

    // 2. Open/find DM channel
    // Get bot's own id first
    const meRes = await fetch(`${MM_URL}/api/v4/users/me`, {
      headers: { Authorization: `Bearer ${MM_TOKEN}` },
    });
    const me = await meRes.json();

    const dmRes = await fetch(`${MM_URL}/api/v4/channels/direct`, {
      method: 'POST',
      headers: { Authorization: `Bearer ${MM_TOKEN}`, 'Content-Type': 'application/json' },
      body: JSON.stringify([me.id, user.id]),
    });
    if (!dmRes.ok) { log(`WARN: mm DM channel open failed ${dmRes.status}`); return; }
    const dm = await dmRes.json();

    // 3. Post message
    const postRes = await fetch(`${MM_URL}/api/v4/posts`, {
      method: 'POST',
      headers: { Authorization: `Bearer ${MM_TOKEN}`, 'Content-Type': 'application/json' },
      body: JSON.stringify({ channel_id: dm.id, message }),
    });
    if (postRes.ok) {
      log(`MM DM sent to ${JKH_MM_USER}: ${message.slice(0, 80)}`);
    } else {
      log(`WARN: mm post failed ${postRes.status}`);
    }
  } catch (e) {
    log(`WARN: mm DM error: ${e.message}`);
  }
}

/** Check GPU utilization and fire alert if sustained high */
function checkGpuSpike(gpuTelemetry) {
  if (!gpuTelemetry || !gpuTelemetry[0]) return;
  const g0 = gpuTelemetry[0];
  const util = g0.utilization_pct;
  if (util === null) return;  // GB10 [N/A] — skip

  if (util >= GPU_ALERT_THRESHOLD) {
    gpuHighStrikes++;
    log(`GPU spike check: ${util}% util (strike ${gpuHighStrikes}/${GPU_ALERT_STRIKES})`);
    if (gpuHighStrikes >= GPU_ALERT_STRIKES) {
      const now = Date.now();
      if (now - lastAlertTs > GPU_ALERT_COOLDOWN) {
        lastAlertTs = now;
        const msg = `⚠️ **sparky GPU alert** — utilization at **${util}%** for ${GPU_ALERT_STRIKES} consecutive checks (${Math.round(CHECK_INTERVAL_MS * GPU_ALERT_STRIKES / 60000)} min). Temp: ${g0.temp_c}°C, Power: ${g0.power_w}W. Natasha on it.`;
        mmDm(msg).catch(e => log(`WARN alert DM failed: ${e.message}`));
        gpuHighStrikes = 0;  // reset after alert
      } else {
        log(`GPU spike cooldown active — skipping alert`);
      }
    }
  } else {
    if (gpuHighStrikes > 0) log(`GPU util back to ${util}% — resetting strike counter`);
    gpuHighStrikes = 0;
  }
}

let ollamaStatus = {};

function log(msg) {
  console.log(`[ollama-watchdog ${new Date().toISOString()}] ${msg}`);
}

/** Save status to disk (picked up by heartbeat) */
function saveStatus() {
  try {
    const gpuTelemetry = getGpuTelemetry();
    const ram = getSystemRam();
    writeFileSync(STATUS_FILE, JSON.stringify({
      ollama: ollamaStatus,
      gpu: gpuTelemetry,
      // GB10 has unified memory — system RAM IS the GPU memory pool
      ram,
      updatedAt: new Date().toISOString(),
    }, null, 2));
  } catch (e) {
    log(`WARN: could not write status file: ${e.message}`);
  }
}

/** Collect system RAM stats (important for GB10 unified memory architecture) */
function getSystemRam() {
  try {
    const raw = execSync('free -b', { timeout: 3000, stdio: 'pipe' }).toString();
    const memLine = raw.split('\n').find(l => l.startsWith('Mem:'));
    if (!memLine) return null;
    const parts = memLine.trim().split(/\s+/);
    return {
      total_mb:     Math.round(parseInt(parts[1]) / 1024 / 1024),
      used_mb:      Math.round(parseInt(parts[2]) / 1024 / 1024),
      free_mb:      Math.round(parseInt(parts[3]) / 1024 / 1024),
      available_mb: Math.round(parseInt(parts[6]) / 1024 / 1024),
    };
  } catch (e) {
    return null;
  }
}

/** Collect GPU telemetry via nvidia-smi */
function getGpuTelemetry() {
  try {
    const fields = [
      'index', 'name', 'memory.used', 'memory.free', 'memory.total',
      'temperature.gpu', 'power.draw', 'utilization.gpu',
    ];
    const raw = execSync(
      `nvidia-smi --query-gpu=${fields.join(',')} --format=csv,noheader`,
      { timeout: 5000, stdio: 'pipe' }
    ).toString().trim();

    if (!raw) return null;

    const gpus = raw.split('\n').map(line => {
      const parts = line.split(',').map(s => s.trim());
      const parseNum = (v) => {
        const n = parseFloat(v);
        return isNaN(n) ? null : n;
      };
      return {
        index:       parseNum(parts[0]) ?? 0,
        name:        parts[1] || 'unknown',
        vram_used_mb: parseNum(parts[2]),    // null if unified memory (GB10)
        vram_free_mb: parseNum(parts[3]),
        vram_total_mb: parseNum(parts[4]),
        temp_c:       parseNum(parts[5]),
        power_w:      parseNum(parts[6]),
        utilization_pct: parseNum(parts[7]),
        unified_memory: parseNum(parts[2]) === null,  // GB10 uses system RAM as VRAM
      };
    });

    return gpus;
  } catch (e) {
    log(`WARN: nvidia-smi failed: ${e.message}`);
    return null;
  }
}

/** Append GPU metrics to local JSONL time-series file */
function appendGpuMetrics(gpuTelemetry, ram, ollamaModelCount) {
  try {
    const dir = dirname(GPU_METRICS_FILE);
    if (!existsSync(dir)) mkdirSync(dir, { recursive: true });

    // Rotate if file exceeds max size
    if (existsSync(GPU_METRICS_FILE)) {
      const sz = statSync(GPU_METRICS_FILE).size;
      if (sz > GPU_METRICS_MAX_MB * 1024 * 1024) {
        const rotated = GPU_METRICS_FILE + '.1.gz';
        try {
          execSync(`gzip -c "${GPU_METRICS_FILE}" > "${rotated}" && > "${GPU_METRICS_FILE}"`, { timeout: 10000, stdio: 'pipe' });
          log(`GPU metrics rotated to ${rotated} (was ${Math.round(sz/1024/1024)}MB)`);
        } catch (e) {
          log(`WARN: GPU metrics rotation failed: ${e.message}`);
        }
      }
    }

    const g0 = gpuTelemetry?.[0];
    const entry = {
      ts:               new Date().toISOString(),
      temp_c:           g0?.temp_c ?? null,
      power_w:          g0?.power_w ?? null,
      util_pct:         g0?.utilization_pct ?? null,
      vram_used_mb:     g0?.vram_used_mb ?? null,    // null on GB10 unified memory
      ram_used_mb:      ram?.used_mb ?? null,
      ram_avail_mb:     ram?.available_mb ?? null,
      ollama_models:    ollamaModelCount ?? 0,
    };

    appendFileSync(GPU_METRICS_FILE, JSON.stringify(entry) + '\n', 'utf8');
  } catch (e) {
    log(`WARN: failed to append GPU metrics: ${e.message}`);
  }
}

/** POST ollama_status (and GPU telemetry) to RCC heartbeat */
async function reportToRCC(model, status) {
  const gpuTelemetry = getGpuTelemetry();
  const ram = getSystemRam();

  const payload = {
    status: 'online',
    host: 'sparky',
    ts: new Date().toISOString(),
    ollama_status: ollamaStatus,
  };

  if (gpuTelemetry) {
    payload.gpu = gpuTelemetry;
    // Convenience top-level fields from first GPU
    const g0 = gpuTelemetry[0];
    if (g0) {
      if (g0.temp_c !== null)           payload.gpu_temp_c = g0.temp_c;
      if (g0.power_w !== null)          payload.gpu_power_w = g0.power_w;
      if (g0.utilization_pct !== null)  payload.gpu_util_pct = g0.utilization_pct;
      if (g0.vram_used_mb !== null)     payload.vram_used_mb = g0.vram_used_mb;
      if (g0.vram_total_mb !== null)    payload.vram_total_mb = g0.vram_total_mb;
    }
    log(`GPU telemetry: ${gpuTelemetry.length} GPU(s). g0: temp=${gpuTelemetry[0]?.temp_c}°C power=${gpuTelemetry[0]?.power_w}W util=${gpuTelemetry[0]?.utilization_pct}%`);
    checkGpuSpike(gpuTelemetry);
  }

  // Append to local time-series JSONL for historical analysis
  appendGpuMetrics(gpuTelemetry, ram, Object.keys(ollamaStatus).length);

  // GB10 unified memory — system RAM IS the GPU memory pool
  if (ram) {
    payload.ram = ram;
    // Expose as unified_vram fields for dashboard display
    payload.unified_vram_used_mb  = ram.used_mb;
    payload.unified_vram_free_mb  = ram.available_mb;
    payload.unified_vram_total_mb = ram.total_mb;
    log(`RAM (unified VRAM): ${ram.used_mb}MB used / ${ram.total_mb}MB total (${ram.available_mb}MB avail)`);
  }

  try {
    const res = await fetch(`${RCC_API}/api/heartbeat/${AGENT_NAME}`, {
      method: 'POST',
      headers: { 'Authorization': `Bearer ${RCC_AUTH_TOKEN}`, 'Content-Type': 'application/json' },
      body: JSON.stringify(payload),
    });
    if (!res.ok) log(`WARN: RCC heartbeat responded ${res.status}`);
  } catch (e) {
    log(`WARN: RCC heartbeat failed: ${e.message}`);
  }
}

/** Attempt to restart an ollama model */
async function restartModel(model) {
  log(`Restarting model: ${model}`);
  ollamaStatus[model] = 'restarting';
  saveStatus();
  try {
    // Stop the model
    execSync(`ollama stop ${model}`, { timeout: 15000, stdio: 'pipe' });
    log(`Stopped ${model}`);
  } catch (e) {
    log(`WARN: ollama stop failed (may not have been running): ${e.message}`);
  }
  // Brief pause
  await new Promise(r => setTimeout(r, 3000));
  // Pull/warm up the model (non-blocking — just load it)
  try {
    execSync(`ollama run ${model} "" 2>/dev/null || true`, { timeout: 60000, stdio: 'pipe' });
    log(`Warmed up ${model}`);
  } catch (e) {
    log(`WARN: ollama run warmup failed: ${e.message}`);
  }
  ollamaStatus[model] = 'ok';
  saveStatus();
  await reportToRCC(model, 'ok');
  log(`Model ${model} restarted successfully`);
}

/** Health-check a single model */
async function checkModel(model) {
  log(`Checking model: ${model}`);
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), RESPONSE_TIMEOUT_MS);
  const t0 = Date.now();

  try {
    const res = await fetch(`${OLLAMA_BASE_URL}/api/generate`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ model, prompt: TEST_PROMPT, stream: false }),
      signal: controller.signal,
    });
    clearTimeout(timeout);

    if (!res.ok) {
      log(`ERROR: model ${model} returned HTTP ${res.status}`);
      ollamaStatus[model] = 'degraded';
      saveStatus();
      await restartModel(model);
      return;
    }

    const data = await res.json();
    const elapsed = Date.now() - t0;
    const output = (data.response || '').trim();

    if (!output || output.length === 0) {
      log(`DEGRADED: model ${model} returned empty response in ${elapsed}ms`);
      ollamaStatus[model] = 'degraded';
      saveStatus();
      await restartModel(model);
      return;
    }

    log(`OK: model ${model} responded in ${elapsed}ms — "${output.slice(0, 60)}"`);
    ollamaStatus[model] = 'ok';
    saveStatus();
    await reportToRCC(model, 'ok');

  } catch (e) {
    clearTimeout(timeout);
    const elapsed = Date.now() - t0;
    if (e.name === 'AbortError') {
      log(`TIMEOUT: model ${model} did not respond within ${RESPONSE_TIMEOUT_MS}ms`);
    } else {
      log(`ERROR: model ${model} check failed after ${elapsed}ms: ${e.message}`);
    }
    ollamaStatus[model] = 'degraded';
    saveStatus();
    await restartModel(model);
  }
}

/** Run one full check cycle */
async function checkAll() {
  log(`Starting check cycle for models: ${OLLAMA_MODELS.join(', ')}`);
  for (const model of OLLAMA_MODELS) {
    await checkModel(model);
  }
  log('Check cycle complete');
}

// ── Main loop ──────────────────────────────────────────────────────────────
log(`Ollama watchdog starting. Models: ${OLLAMA_MODELS.join(', ')}, interval: ${CHECK_INTERVAL_MS}ms`);

// Load existing status if present
if (existsSync(STATUS_FILE)) {
  try {
    const prev = JSON.parse(readFileSync(STATUS_FILE, 'utf8'));
    ollamaStatus = prev;
    log(`Loaded previous status: ${JSON.stringify(ollamaStatus)}`);
  } catch (_) {}
}

// Run immediately, then on interval
checkAll().catch(e => log(`ERROR in check cycle: ${e.message}`));
setInterval(() => checkAll().catch(e => log(`ERROR in check cycle: ${e.message}`)), CHECK_INTERVAL_MS);
