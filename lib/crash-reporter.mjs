#!/usr/bin/env node
/**
 * crash-reporter.mjs — Automatic crash reporting for CCC Node.js services
 *
 * Usage:
 *   import { initCrashReporter } from '../lib/crash-reporter.mjs';
 *   initCrashReporter({ service: 'wq-dashboard', sourceDir: '${OPENCLAW_WORKSPACE:-~/.openclaw/workspace}/dashboard' });
 *
 * On uncaught exception or unhandled rejection:
 *   1. Writes crash task to queue.json (direct file write as fallback)
 *   2. POSTs crash report to dashboard API (unless we ARE the dashboard)
 *   3. Uploads crash log to MinIO
 *   4. Re-throws / exits to let the process die
 */

import { readFile, writeFile } from 'fs/promises';
import { execFileSync } from 'child_process';
import http from 'http';

const QUEUE_PATH = '${OPENCLAW_WORKSPACE:-~/.openclaw/workspace}/workqueue/queue.json';
const MC_PATH = 'mc';
const MINIO_ALIAS = process.env.MINIO_ALIAS || 'local';
const DASHBOARD_URL = 'http://localhost:8788/api/crash-report';
const AUTH_TOKEN = process.env.RCC_AGENT_TOKEN || '';

let _initialized = false;
let _config = {};

// ── Crash-loop dedup state ─────────────────────────────────────────────────
// Grace period: don't file anything in the first 30s after process start
// (rcc-api often crashes and recovers within seconds during deploy)
const STARTUP_GRACE_MS  = 30_000;
const BURST_WINDOW_MS   = 60_000;   // window to detect burst
const BURST_MAX_ITEMS   = 3;        // if >3 items filed in window → suppress
const _processStartTime = Date.now();
const _recentCrashTimes = [];       // timestamps of crash items filed

/**
 * Build the crash task object for queue.json
 */
function buildCrashTask({ service, sourceDir, error, stack, ts }) {
  const truncTitle = (error || 'Unknown error').slice(0, 80);
  const stackLines = (stack || '').split('\n').slice(0, 5).join('\n');
  const minioPath = `agents/logs/${service}-crash-${ts}.json`;

  return {
    id: `wq-crash-${ts}`,
    itemVersion: 1,
    created: new Date(parseInt(ts)).toISOString(),
    source: 'system',
    assignee: 'all',
    priority: 'high',
    status: 'pending',
    title: `CRASH: ${service} — ${truncTitle}`,
    description: `Unhandled exception in ${service}. Stack trace and logs available.`,
    notes: `Error: ${error}\nStack: ${stackLines}\nSource: ${sourceDir}\nMinIO logs: ${minioPath}`,
    tags: ['crash', 'auto-filed', service],
    channel: 'mattermost',
    claimedBy: null,
    claimedAt: null,
    attempts: 0,
    maxAttempts: 1,
    lastAttempt: null,
    completedAt: null,
    result: null,
  };
}

/**
 * Check crash-loop dedup guards. Returns true if we should suppress filing.
 * Mutates _recentCrashTimes on pass-through.
 */
function shouldSuppressCrash(service) {
  const now = Date.now();

  // Grace period: too soon after startup
  if (now - _processStartTime < STARTUP_GRACE_MS) {
    console.error(`[crash-reporter] Suppressing crash item — within ${STARTUP_GRACE_MS / 1000}s startup grace period`);
    return true;
  }

  // Burst suppression: trim old entries, check count
  const cutoff = now - BURST_WINDOW_MS;
  // Remove entries outside the window
  while (_recentCrashTimes.length && _recentCrashTimes[0] < cutoff) _recentCrashTimes.shift();

  if (_recentCrashTimes.length >= BURST_MAX_ITEMS) {
    console.error(`[crash-reporter] Suppressing crash item — burst detected (${_recentCrashTimes.length} items in ${BURST_WINDOW_MS / 1000}s window)`);
    return true;
  }

  // Also check queue.json for recent crash items for this service (cross-process dedup)
  try {
    const queuePath = new URL(QUEUE_PATH, import.meta.url).pathname;
    const raw = readFileSync(queuePath, 'utf8');
    const data = JSON.parse(raw);
    const recentInFile = (data.items || []).filter(i =>
      i.id?.startsWith('wq-crash-') &&
      i.tags?.includes(service) &&
      i.status === 'pending' &&
      (now - new Date(i.created || 0).getTime()) < BURST_WINDOW_MS
    );
    if (recentInFile.length >= BURST_MAX_ITEMS) {
      console.error(`[crash-reporter] Suppressing crash item — ${recentInFile.length} recent crash items already in queue for ${service}`);
      return true;
    }
  } catch {}

  _recentCrashTimes.push(now);
  return false;
}

/**
 * Write crash task directly to queue.json (fallback / primary for dashboard crashes)
 */
async function writeToQueueFile(task) {
  // Apply dedup guard
  if (shouldSuppressCrash(_config.service || task.tags?.[1] || 'unknown')) {
    return false; // suppressed — not an error
  }

  try {
    const raw = await readFile(QUEUE_PATH, 'utf8');
    const data = JSON.parse(raw);
    data.items = data.items || [];
    data.items.push(task);
    data.lastSync = new Date().toISOString();
    await writeFile(QUEUE_PATH, JSON.stringify(data, null, 2) + '\n', 'utf8');
    console.error(`[crash-reporter] Wrote crash task ${task.id} to queue.json`);
    return true;
  } catch (e) {
    console.error(`[crash-reporter] Failed to write queue.json: ${e.message}`);
    return false;
  }
}

/**
 * POST crash report to dashboard API (sync-ish via promise, with short timeout)
 */
function postToDashboard({ service, error, stack, sourceDir, ts }) {
  return new Promise((resolve) => {
    const payload = JSON.stringify({ service, error, stack, sourceDir, ts });
    const url = new URL(DASHBOARD_URL);
    const req = http.request({
      hostname: url.hostname,
      port: url.port,
      path: url.pathname,
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        'Authorization': `Bearer ${AUTH_TOKEN}`,
        'Content-Length': Buffer.byteLength(payload),
      },
      timeout: 3000,
    }, (res) => {
      let body = '';
      res.on('data', (chunk) => body += chunk);
      res.on('end', () => {
        try {
          const data = JSON.parse(body);
          console.error(`[crash-reporter] Dashboard API response: ${JSON.stringify(data)}`);
          resolve(data.ok || false);
        } catch {
          resolve(false);
        }
      });
    });
    req.on('error', (e) => {
      console.error(`[crash-reporter] Dashboard API error: ${e.message}`);
      resolve(false);
    });
    req.on('timeout', () => {
      req.destroy();
      resolve(false);
    });
    req.write(payload);
    req.end();
  });
}

import { writeFileSync, unlinkSync, readFileSync } from 'fs';

function uploadToMinIOSync({ service, error, stack, sourceDir, ts }) {
  const minioPath = `${MINIO_ALIAS}/agents/logs/${service}-crash-${ts}.json`;
  const logData = JSON.stringify({
    service,
    error,
    stack,
    sourceDir,
    ts,
    pid: process.pid,
    nodeVersion: process.version,
    platform: process.platform,
    uptime: process.uptime(),
    memoryUsage: process.memoryUsage(),
    cwd: process.cwd(),
    reportedAt: new Date().toISOString(),
  }, null, 2);

  try {
    const tmpPath = `/tmp/crash-${service}-${ts}.json`;
    writeFileSync(tmpPath, logData, 'utf8');
    execFileSync(MC_PATH, ['cp', tmpPath, minioPath], { timeout: 5000 });
    try { unlinkSync(tmpPath); } catch {}
    console.error(`[crash-reporter] Uploaded crash log to ${minioPath}`);
    return true;
  } catch (e) {
    console.error(`[crash-reporter] MinIO upload failed: ${e.message}`);
    return false;
  }
}

/**
 * Main crash handler
 */
async function handleCrash(err, type) {
  const { service, sourceDir } = _config;
  const ts = String(Date.now());
  const errorMsg = err?.message || String(err);
  const stack = err?.stack || '';
  const isDashboard = service === 'wq-dashboard';

  console.error(`\n[crash-reporter] 💥 ${type} in ${service}: ${errorMsg}`);

  const crashData = { service, error: errorMsg, stack, sourceDir, ts };
  const task = buildCrashTask(crashData);

  // 1. Upload to MinIO first (non-critical, sync)
  uploadToMinIOSync(crashData);

  // 2. If we're not the dashboard, try the API first
  if (!isDashboard) {
    const apiOk = await postToDashboard(crashData);
    if (!apiOk) {
      // Fallback: write directly to queue.json
      await writeToQueueFile(task);
    }
  } else {
    // Dashboard is crashing — write directly to queue.json
    await writeToQueueFile(task);
  }

  // 3. Exit (uncaughtException handler must exit)
  if (type === 'uncaughtException') {
    process.exit(1);
  }
  // For unhandledRejection, Node will exit if --unhandled-rejections=throw (default in newer Node)
}

/**
 * Initialize crash reporter for a service.
 * Call once, early in your process lifecycle.
 */
export function initCrashReporter({ service, sourceDir }) {
  if (_initialized) {
    console.error(`[crash-reporter] Already initialized for ${_config.service}, ignoring re-init for ${service}`);
    return;
  }

  _config = { service, sourceDir };
  _initialized = true;

  process.on('uncaughtException', (err) => {
    handleCrash(err, 'uncaughtException');
  });

  process.on('unhandledRejection', (reason) => {
    const err = reason instanceof Error ? reason : new Error(String(reason));
    handleCrash(err, 'unhandledRejection');
  });

  console.log(`[crash-reporter] 🛡️ Initialized for ${service} (source: ${sourceDir})`);
}

// Also export buildCrashTask for the systemd hook
export { buildCrashTask, writeToQueueFile };
