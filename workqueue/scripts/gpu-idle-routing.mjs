#!/usr/bin/env node
/**
 * gpu-idle-routing.mjs — GPU idle-state adaptive inference routing for Natasha
 * 
 * Tracks GPU utilization over time. If GPU util < 30% for >5min continuously,
 * sets accept_gpu_tasks=true in natasha-gpu-health.json on MinIO.
 * Resets to false as soon as GPU util rises above 30%.
 * 
 * Runs each cron cycle (every ~30min), uses a state file to track history.
 * 
 * wq-N-007
 */

import { execSync } from 'child_process';
import { readFileSync, writeFileSync, existsSync } from 'fs';
import { join, dirname } from 'path';
import { fileURLToPath } from 'url';
import { DecisionJournal } from '../../rcc/decision-journal/index.mjs';

const __dirname = dirname(fileURLToPath(import.meta.url));
const WORKSPACE = join(__dirname, '../..');
const STATE_FILE = join(WORKSPACE, 'workqueue/state-gpu-routing.json');

const MINIO_ENDPOINT = 'http://100.89.199.14:9000';
const MINIO_ACCESS = 'rockymoose4810f4cc7d28916f';
const MINIO_SECRET = '<MINIO_SECRET_KEY>';
const HEALTH_KEY = 'agents/shared/natasha-gpu-health.json';

const IDLE_THRESHOLD_PCT = 30;   // GPU % below which we consider "idle"
const IDLE_REQUIRED_MS = 5 * 60 * 1000;  // 5 minutes of continuous idle

function getGpuStats() {
  try {
    const out = execSync(
      'nvidia-smi --query-gpu=utilization.gpu,memory.used,memory.total,temperature.gpu,power.draw --format=csv,noheader,nounits',
      { timeout: 10000 }
    ).toString().trim();
    const [util, memUsed, memTotal, temp, power] = out.split(',').map(s => parseFloat(s.trim()));
    return {
      util_pct: isNaN(util) ? null : util,
      vram_used_mb: isNaN(memUsed) ? null : memUsed,
      vram_total_mb: isNaN(memTotal) ? null : memTotal,
      temp_c: isNaN(temp) ? null : temp,
      power_w: isNaN(power) ? null : power,
    };
  } catch (e) {
    console.error('nvidia-smi failed:', e.message);
    return null;
  }
}

function loadState() {
  if (existsSync(STATE_FILE)) {
    try {
      return JSON.parse(readFileSync(STATE_FILE, 'utf8'));
    } catch {}
  }
  return {
    idleSince: null,       // ISO timestamp when GPU first went idle (< 30%)
    lastUtil: null,        // last observed utilization
    acceptGpuTasks: false, // current routing decision
    cycleCount: 0,
  };
}

function saveState(state) {
  writeFileSync(STATE_FILE, JSON.stringify(state, null, 2));
}

function publishToMinio(payload) {
  const data = JSON.stringify(payload, null, 2);
  const tmpFile = '/tmp/natasha-gpu-health.json';
  writeFileSync(tmpFile, data);
  
  try {
    execSync(
      `curl -s --aws-sigv4 "aws:amz:us-east-1:s3" \
        --user "${MINIO_ACCESS}:${MINIO_SECRET}" \
        -X PUT -H "Content-Type: application/json" \
        --data-binary @${tmpFile} \
        "${MINIO_ENDPOINT}/${HEALTH_KEY}"`,
      { timeout: 15000 }
    );
    return true;
  } catch (e) {
    console.error('MinIO publish failed:', e.message);
    return false;
  }
}

// ─── Main ──────────────────────────────────────────────────────────────────

const now = new Date();
const nowMs = now.getTime();
const nowIso = now.toISOString().replace('+00:00', 'Z');

const gpu = getGpuStats();
const state = loadState();
state.cycleCount++;

let acceptGpuTasks = false;
let reason = '';

if (!gpu || gpu.util_pct === null) {
  reason = 'GPU stats unavailable — defaulting to no auto-accept';
  acceptGpuTasks = false;
  state.idleSince = null;
} else {
  const isIdle = gpu.util_pct < IDLE_THRESHOLD_PCT;
  
  if (isIdle) {
    if (!state.idleSince) {
      // First idle reading — start tracking
      state.idleSince = nowIso;
      reason = `GPU idle (${gpu.util_pct}%) — monitoring; need 5min continuous idle to auto-accept`;
      acceptGpuTasks = false;
    } else {
      // Already tracking idleness
      const idleDurationMs = nowMs - new Date(state.idleSince).getTime();
      if (idleDurationMs >= IDLE_REQUIRED_MS) {
        acceptGpuTasks = true;
        const idleMinutes = Math.round(idleDurationMs / 60000);
        reason = `GPU idle ${gpu.util_pct}% for ${idleMinutes}min — auto-accepting GPU-tagged routing requests`;
      } else {
        const idleSecs = Math.round(idleDurationMs / 1000);
        reason = `GPU idle (${gpu.util_pct}%) for ${idleSecs}s — waiting for 5min threshold`;
        acceptGpuTasks = false;
      }
    }
  } else {
    // GPU busy — reset idle tracking
    if (state.idleSince) {
      console.log(`GPU back above threshold (${gpu.util_pct}%) — resetting idle timer`);
    }
    state.idleSince = null;
    reason = `GPU utilization ${gpu.util_pct}% — above ${IDLE_THRESHOLD_PCT}% threshold, not auto-accepting`;
    acceptGpuTasks = false;
  }
}

state.lastUtil = gpu?.util_pct ?? null;
state.acceptGpuTasks = acceptGpuTasks;
saveState(state);

// Log routing decision to DecisionJournal for drift detection
try {
  const journal = new DecisionJournal({ agent: 'natasha', silent: true });
  await journal.log({
    principle_used: 'gpu-idle-routing',
    confidence: gpu ? 0.95 : 0.3,
    was_conflict: false,
    outcome: acceptGpuTasks ? 'accept_gpu_tasks=true' : 'accept_gpu_tasks=false',
    context: {
      util_pct: gpu?.util_pct ?? null,
      idle_since: state.idleSince,
      reason,
      cycle: state.cycleCount,
    },
  });
} catch (e) {
  // Non-fatal — don't let journal errors break routing
  console.warn('[gpu-idle-routing] DecisionJournal write failed:', e.message);
}

const payload = {
  agent: 'natasha',
  ts: nowIso,
  util_pct: gpu?.util_pct ?? null,
  temp_c: gpu?.temp_c ?? null,
  power_w: gpu?.power_w ?? null,
  vram_used_mb: gpu?.vram_used_mb ?? null,
  vram_total_mb: gpu?.vram_total_mb ?? null,
  idle_since: state.idleSince,
  routing_hints: {
    accept_gpu_tasks: acceptGpuTasks,
    reason,
    idle_threshold_pct: IDLE_THRESHOLD_PCT,
    idle_required_min: 5,
  },
};

const published = publishToMinio(payload);
console.log(`[gpu-idle-routing] cycle=${state.cycleCount} util=${gpu?.util_pct ?? 'N/A'}% accept=${acceptGpuTasks} published=${published}`);
console.log(`  reason: ${reason}`);

// Output for integration with cron/workqueue
process.exit(0);
