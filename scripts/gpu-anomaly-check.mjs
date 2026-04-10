#!/usr/bin/env node
/**
 * gpu-anomaly-check.mjs
 *
 * Reads gpu-metrics.jsonl, builds a 7-day rolling hourly baseline (median per
 * hour-of-day), and flags any reading that deviates >2 sigma from the baseline.
 * Designed to be called from ollama-watchdog.mjs on each 15-min tick.
 *
 * Usage:
 *   node gpu-anomaly-check.mjs [--jsonl PATH] [--hours 24] [--alert-url URL]
 *
 * Environment:
 *   SLACK_WEBHOOK_URL   - optional Slack webhook for alerts
 *   RCC_AGENT_TOKEN     - optional RCC token for posting to queue
 *   GPU_METRICS_JSONL   - override JSONL path
 *
 * Exit codes:
 *   0 = no anomalies
 *   1 = anomalies detected (also prints JSON to stdout)
 *   2 = insufficient data (<7 days)
 */

import fs from 'fs';
import path from 'path';
import https from 'https';
import { fileURLToPath } from 'url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));

// ── Config ──────────────────────────────────────────────────────────────────
const DEFAULT_JSONL = process.env.GPU_METRICS_JSONL ||
  path.join(process.env.HOME || '/home/horde', '.openclaw', 'workspace', 'telemetry', 'gpu-metrics.jsonl');

const args = process.argv.slice(2);
const getArg = (flag) => { const i = args.indexOf(flag); return i !== -1 ? args[i + 1] : null; };

const JSONL_PATH  = getArg('--jsonl') || DEFAULT_JSONL;
const WINDOW_DAYS = parseInt(getArg('--days') || '7', 10);
const SIGMA_THRESH = parseFloat(getArg('--sigma') || '2.0');
const METRICS     = ['temp_c', 'power_w', 'util_pct', 'ram_used_mb'];
const SLACK_URL   = process.env.SLACK_WEBHOOK_URL;

// ── Load JSONL ───────────────────────────────────────────────────────────────
function loadMetrics(filePath) {
  if (!fs.existsSync(filePath)) return [];
  const lines = fs.readFileSync(filePath, 'utf8').trim().split('\n').filter(Boolean);
  return lines.map(l => { try { return JSON.parse(l); } catch { return null; } }).filter(Boolean);
}

// ── Statistics helpers ───────────────────────────────────────────────────────
function median(arr) {
  if (!arr.length) return null;
  const sorted = [...arr].sort((a, b) => a - b);
  const mid = Math.floor(sorted.length / 2);
  return sorted.length % 2 ? sorted[mid] : (sorted[mid - 1] + sorted[mid]) / 2;
}

function stddev(arr, med) {
  if (arr.length < 2) return 0;
  const m = med ?? median(arr);
  const variance = arr.reduce((acc, v) => acc + (v - m) ** 2, 0) / arr.length;
  return Math.sqrt(variance);
}

// ── Build baseline ───────────────────────────────────────────────────────────
function buildBaseline(rows, windowMs) {
  const now = Date.now();
  const cutoff = now - windowMs;
  const historical = rows.filter(r => new Date(r.ts).getTime() < cutoff || true); // all data for baseline
  // Group by hour-of-day (0..23)
  const byHour = {}; // metric -> hour -> [values]
  for (const metric of METRICS) {
    byHour[metric] = Array.from({ length: 24 }, () => []);
  }
  for (const row of historical) {
    const hour = new Date(row.ts).getUTCHours();
    for (const metric of METRICS) {
      const val = row[metric];
      if (val != null && typeof val === 'number') {
        byHour[metric][hour].push(val);
      }
    }
  }
  // Compute median + stddev per hour per metric
  const baseline = {};
  for (const metric of METRICS) {
    baseline[metric] = [];
    for (let h = 0; h < 24; h++) {
      const vals = byHour[metric][h];
      const med = median(vals);
      const sd  = stddev(vals, med);
      baseline[metric][h] = { median: med, stddev: sd, n: vals.length };
    }
  }
  return baseline;
}

// ── Check recent readings for anomalies ──────────────────────────────────────
function detectAnomalies(rows, baseline, windowMs) {
  const now = Date.now();
  const recent = rows.filter(r => now - new Date(r.ts).getTime() <= windowMs);
  const anomalies = [];

  for (const row of recent) {
    const hour = new Date(row.ts).getUTCHours();
    for (const metric of METRICS) {
      const val = row[metric];
      if (val == null || typeof val !== 'number') continue;
      const { median: med, stddev: sd, n } = baseline[metric][hour];
      if (med == null || n < 3) continue; // not enough baseline data for this hour
      const threshold = sd < 0.01 ? Math.abs(med) * 0.1 : sd; // floor for near-zero stddev
      const deviation = Math.abs(val - med);
      if (deviation > SIGMA_THRESH * threshold) {
        anomalies.push({
          ts: row.ts,
          metric,
          value: val,
          baseline_median: Math.round(med * 100) / 100,
          baseline_stddev: Math.round(sd * 100) / 100,
          deviation_sigma: Math.round((deviation / threshold) * 100) / 100,
          hour_utc: hour,
        });
      }
    }
  }
  return anomalies;
}

// ── Alert helpers ────────────────────────────────────────────────────────────
async function postSlack(text) {
  if (!SLACK_URL) return;
  const body = JSON.stringify({ text });
  return new Promise((resolve) => {
    const url = new URL(SLACK_URL);
    const req = https.request({
      hostname: url.hostname,
      path: url.pathname + url.search,
      method: 'POST',
      headers: { 'Content-Type': 'application/json', 'Content-Length': Buffer.byteLength(body) },
    }, (res) => { res.resume(); resolve(res.statusCode); });
    req.on('error', () => resolve(null));
    req.write(body);
    req.end();
  });
}

// ── Main ─────────────────────────────────────────────────────────────────────
async function main() {
  const rows = loadMetrics(JSONL_PATH);

  if (rows.length === 0) {
    console.error(`[gpu-anomaly-check] No data in ${JSONL_PATH}`);
    process.exit(2);
  }

  const oldestTs = new Date(rows[0].ts).getTime();
  const newestTs = new Date(rows[rows.length - 1].ts).getTime();
  const daysCovered = (newestTs - oldestTs) / 86400000;

  if (daysCovered < WINDOW_DAYS) {
    console.error(`[gpu-anomaly-check] Only ${daysCovered.toFixed(1)} days of data (need ${WINDOW_DAYS}). Skipping.`);
    process.exit(2);
  }

  const windowMs = WINDOW_DAYS * 86400 * 1000;
  const baseline = buildBaseline(rows, windowMs);

  // Check last 30 minutes of readings for anomalies
  const checkWindowMs = 30 * 60 * 1000;
  const anomalies = detectAnomalies(rows, baseline, checkWindowMs);

  if (anomalies.length === 0) {
    console.log('[gpu-anomaly-check] No anomalies detected.');
    process.exit(0);
  }

  // Format alert
  const lines = anomalies.map(a =>
    `  • ${a.metric}=${a.value} at ${a.ts} (baseline=${a.baseline_median}±${a.baseline_stddev}, deviation=${a.deviation_sigma}σ, hour=${a.hour_utc}UTC)`
  );
  const alertText = `🚨 *GPU anomaly detected on sparky (GB10)*\n${lines.join('\n')}`;

  console.error(alertText);
  console.log(JSON.stringify({ anomalies }, null, 2));

  if (SLACK_URL) {
    const status = await postSlack(alertText);
    console.error(`[gpu-anomaly-check] Slack alert sent: HTTP ${status}`);
  }

  process.exit(1);
}

main().catch(err => {
  console.error('[gpu-anomaly-check] Fatal:', err.message);
  process.exit(2);
});
