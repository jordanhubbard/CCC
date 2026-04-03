#!/usr/bin/env node
/**
 * rcc/scripts/preflight-gate.mjs — vRAM/RAM pre-flight gate for sparky (GB10)
 *
 * Before accepting large inference tasks, checks available memory headroom.
 * GB10 uses unified memory — system RAM IS the GPU memory pool.
 *
 * Usage (as module):
 *   import { checkMemoryGate, MemoryGateError } from './preflight-gate.mjs';
 *   await checkMemoryGate({ estimatedGb: 30, taskName: 'nemotron-120b' });
 *
 * Usage (as CLI):
 *   node preflight-gate.mjs --estimate-gb 30 --task "nemotron inference"
 *   # exits 0 if OK, exits 1 if insufficient memory (prints JSON result to stdout)
 *
 * Env vars:
 *   PREFLIGHT_MIN_FREE_PCT   default: 20   (minimum free % of total RAM)
 *   PREFLIGHT_MIN_FREE_GB    default: 8    (hard minimum free GB, regardless of %)
 *   PREFLIGHT_DRY_RUN        default: false (log but don't block)
 */

import { execSync } from 'child_process';

const MIN_FREE_PCT = parseFloat(process.env.PREFLIGHT_MIN_FREE_PCT ?? '20');
const MIN_FREE_GB  = parseFloat(process.env.PREFLIGHT_MIN_FREE_GB  ?? '8');
const DRY_RUN      = process.env.PREFLIGHT_DRY_RUN === 'true';

export class MemoryGateError extends Error {
  constructor(msg, stats) {
    super(msg);
    this.name = 'MemoryGateError';
    this.stats = stats;
  }
}

/**
 * Get current memory stats.
 * Returns { totalGb, usedGb, availableGb, freePct, gpuUtil }
 */
export function getMemoryStats() {
  // System RAM (unified memory on GB10)
  const raw = execSync('free -b', { timeout: 3000, stdio: 'pipe' }).toString();
  const memLine = raw.split('\n').find(l => l.startsWith('Mem:'));
  if (!memLine) throw new Error('Could not parse free -b output');
  const parts = memLine.trim().split(/\s+/);
  const totalBytes     = parseInt(parts[1]);
  const usedBytes      = parseInt(parts[2]);
  const availableBytes = parseInt(parts[6]);  // "available" accounts for cache/buffers

  const totalGb     = totalBytes     / 1024 / 1024 / 1024;
  const usedGb      = usedBytes      / 1024 / 1024 / 1024;
  const availableGb = availableBytes / 1024 / 1024 / 1024;
  const freePct     = (availableBytes / totalBytes) * 100;

  // GPU utilization (nvidia-smi — GB10 may return [N/A] for VRAM fields)
  let gpuUtil = null;
  try {
    const smi = execSync(
      'nvidia-smi --query-gpu=utilization.gpu --format=csv,noheader,nounits',
      { timeout: 5000, stdio: 'pipe' }
    ).toString().trim();
    const u = parseFloat(smi);
    if (!isNaN(u)) gpuUtil = u;
  } catch { /* nvidia-smi unavailable */ }

  return {
    totalGb:     Math.round(totalGb     * 10) / 10,
    usedGb:      Math.round(usedGb      * 10) / 10,
    availableGb: Math.round(availableGb * 10) / 10,
    freePct:     Math.round(freePct     * 10) / 10,
    gpuUtil,
  };
}

/**
 * Check if enough memory is available before starting a large task.
 *
 * @param {object} opts
 * @param {number} [opts.estimatedGb]   Estimated GB the task will consume
 * @param {string} [opts.taskName]      Human-readable task name for logging
 * @param {number} [opts.minFreePct]    Override minimum free % (default: PREFLIGHT_MIN_FREE_PCT)
 * @param {number} [opts.minFreeGb]     Override minimum free GB (default: PREFLIGHT_MIN_FREE_GB)
 * @throws {MemoryGateError} if insufficient memory (unless DRY_RUN)
 * @returns {{ ok: boolean, stats: object, reason?: string }}
 */
export async function checkMemoryGate(opts = {}) {
  const {
    estimatedGb = 0,
    taskName    = 'inference task',
    minFreePct  = MIN_FREE_PCT,
    minFreeGb   = MIN_FREE_GB,
  } = opts;

  const stats  = getMemoryStats();
  const label  = `[preflight-gate] ${taskName}`;
  const needed = estimatedGb > 0 ? estimatedGb : 0;

  // Check 1: absolute minimum free
  if (stats.availableGb < minFreeGb) {
    const reason = `Only ${stats.availableGb.toFixed(1)}GB available (min: ${minFreeGb}GB). RAM: ${stats.usedGb.toFixed(1)}/${stats.totalGb.toFixed(1)}GB used.`;
    console.warn(`${label}: BLOCKED — ${reason}`);
    if (!DRY_RUN) throw new MemoryGateError(reason, stats);
    return { ok: false, stats, reason };
  }

  // Check 2: minimum free percentage
  if (stats.freePct < minFreePct) {
    const reason = `Only ${stats.freePct.toFixed(1)}% free (min: ${minFreePct}%). RAM: ${stats.usedGb.toFixed(1)}/${stats.totalGb.toFixed(1)}GB used.`;
    console.warn(`${label}: BLOCKED — ${reason}`);
    if (!DRY_RUN) throw new MemoryGateError(reason, stats);
    return { ok: false, stats, reason };
  }

  // Check 3: estimated task headroom (if provided)
  if (needed > 0 && stats.availableGb - needed < minFreeGb) {
    const reason = `Task needs ~${needed}GB but only ${stats.availableGb.toFixed(1)}GB available — would leave <${minFreeGb}GB headroom. RAM: ${stats.usedGb.toFixed(1)}/${stats.totalGb.toFixed(1)}GB used.`;
    console.warn(`${label}: BLOCKED — ${reason}`);
    if (!DRY_RUN) throw new MemoryGateError(reason, stats);
    return { ok: false, stats, reason };
  }

  console.log(`${label}: OK — ${stats.availableGb.toFixed(1)}GB available (${stats.freePct.toFixed(1)}% free)${needed > 0 ? `, ~${needed}GB needed` : ''}. GPU util: ${stats.gpuUtil ?? 'N/A'}%`);
  return { ok: true, stats };
}

// ── CLI mode ──────────────────────────────────────────────────────────────
if (import.meta.url === `file://${process.argv[1]}`) {
  const args = process.argv.slice(2);
  const estimatedGb = parseFloat(args[args.indexOf('--estimate-gb') + 1] || '0');
  const taskName    = args[args.indexOf('--task') + 1] || 'cli task';

  try {
    const result = await checkMemoryGate({ estimatedGb, taskName });
    console.log(JSON.stringify(result));
    process.exit(result.ok ? 0 : 1);
  } catch (e) {
    if (e instanceof MemoryGateError) {
      console.log(JSON.stringify({ ok: false, reason: e.message, stats: e.stats }));
      process.exit(1);
    }
    throw e;
  }
}
