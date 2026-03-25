#!/usr/bin/env node
/**
 * cron-gap-detector.mjs
 * wq-R-008: Detect agents that have gone silent (missed heartbeat cadence).
 *
 * Reads agents/shared/agent-heartbeat-*.json from MinIO, checks if any agent's
 * last heartbeat is older than SILENCE_THRESHOLD_MS (default 35 min). Writes
 * an alert entry to workqueue/alerts.jsonl and prints a warning for each silent agent.
 *
 * Usage: node cron-gap-detector.mjs
 */

import { execFileSync } from 'child_process';
import { appendFileSync, existsSync, mkdirSync } from 'fs';
import { join, dirname } from 'path';
import { fileURLToPath } from 'url';
import { randomUUID } from 'crypto';

const __dirname = dirname(fileURLToPath(import.meta.url));
const WORKSPACE   = join(__dirname, '../../');
const ALERTS_FILE = join(WORKSPACE, 'workqueue/alerts.jsonl');
const MC          = process.env.MC_BIN || 'mc';
const MINIO_ALIAS = process.env.MINIO_ALIAS || 'local';
const SHARED_PATH = `${MINIO_ALIAS}/agents/shared`;

const SILENCE_THRESHOLD_MS = 35 * 60 * 1000; // 35 minutes
const EXPECTED_AGENTS = ['rocky', 'bullwinkle', 'natasha'];

function mcLs(path) {
  try {
    const out = execFileSync(MC, ['ls', path], { encoding: 'utf8', stdio: ['pipe', 'pipe', 'pipe'] });
    return out.trim().split('\n').filter(Boolean);
  } catch {
    return [];
  }
}

function mcCat(path) {
  try {
    const out = execFileSync(MC, ['cat', path], { encoding: 'utf8', stdio: ['pipe', 'pipe', 'pipe'] });
    return JSON.parse(out);
  } catch {
    return null;
  }
}

function writeAlert(alert) {
  appendFileSync(ALERTS_FILE, JSON.stringify(alert) + '\n', 'utf8');
}

const now     = Date.now();
const nowIso  = new Date(now).toISOString();
const alerts  = [];
const checked = [];

// Discover heartbeat files from MinIO listing
const listing = mcLs(SHARED_PATH);
const hbFiles = listing
  .map(l => l.trim().split(/\s+/).pop())          // last token is filename
  .filter(f => f && f.match(/^agent-heartbeat-.+\.json$/));

// Also ensure we check all expected agents even if file is absent
const foundAgents = new Set(hbFiles.map(f => f.replace('agent-heartbeat-', '').replace('.json', '')));
for (const a of EXPECTED_AGENTS) {
  if (!foundAgents.has(a)) hbFiles.push(`agent-heartbeat-${a}.json`);
}

for (const filename of hbFiles) {
  const agentName = filename.replace('agent-heartbeat-', '').replace('.json', '');
  const remotePath = `${SHARED_PATH}/${filename}`;
  const hb = mcCat(remotePath);

  if (!hb) {
    // File missing entirely — treat as silent
    const alert = {
      id:        randomUUID(),
      ts:        nowIso,
      alertType: 'missing_heartbeat',
      agent:     agentName,
      lastSeen:  null,
      silentMs:  null,
      message:   `Agent "${agentName}" has no heartbeat file — may never have started.`,
    };
    alerts.push(alert);
    writeAlert(alert);
    console.warn(`[cron-gap-detector] ⚠️  ALERT: ${alert.message}`);
    checked.push({ agent: agentName, status: 'missing' });
    continue;
  }

  const ts      = hb.ts || hb.lastSeen || null;
  const lastMs  = ts ? new Date(ts).getTime() : 0;
  const silentMs = now - lastMs;
  const silentMin = Math.round(silentMs / 60000);

  checked.push({ agent: agentName, lastSeen: ts, silentMin });

  if (silentMs > SILENCE_THRESHOLD_MS) {
    const alert = {
      id:        randomUUID(),
      ts:        nowIso,
      alertType: 'silent_agent',
      agent:     agentName,
      lastSeen:  ts,
      silentMs,
      message:   `Agent "${agentName}" last heartbeat ${silentMin}m ago (threshold: ${SILENCE_THRESHOLD_MS / 60000}m). May have crashed or stalled.`,
    };
    alerts.push(alert);
    writeAlert(alert);
    console.warn(`[cron-gap-detector] ⚠️  ALERT: ${alert.message}`);
  } else {
    console.log(`[cron-gap-detector] ✓ ${agentName.padEnd(12)} last seen ${silentMin}m ago — OK`);
  }
}

if (alerts.length === 0) {
  console.log(`[cron-gap-detector] All ${checked.length} agents healthy. No alerts.`);
} else {
  console.warn(`[cron-gap-detector] ${alerts.length} alert(s) written to workqueue/alerts.jsonl`);
}

// --- Also check cron cycle gaps from state files ---
// Each agent's state-<agent>.json tracks lastCycleTs; if gap > 2x expected interval (60min), flag it.
const CRON_INTERVAL_MS = 30 * 60 * 1000; // expected: 30 min crons
const CRON_GAP_THRESHOLD_MS = CRON_INTERVAL_MS * 2; // flag if gap > 60 min

const cronHealth = {};
for (const agentName of EXPECTED_AGENTS) {
  // Try to read state file from MinIO
  const stateFile = `${SHARED_PATH}/state-${agentName}.json`;
  const state = mcCat(stateFile);

  let lastCycleTs = null;
  let cycleCount   = null;
  if (state && state.lastCycleTs) {
    lastCycleTs = state.lastCycleTs;
    cycleCount  = state.cycleCount || null;
  }

  const lastCycleMs  = lastCycleTs ? new Date(lastCycleTs).getTime() : 0;
  const gapMs        = now - lastCycleMs;
  const gapMin       = Math.round(gapMs / 60000);
  const healthy      = lastCycleTs ? gapMs <= CRON_GAP_THRESHOLD_MS : false;

  cronHealth[agentName] = {
    agent:       agentName,
    lastCycleTs,
    cycleCount,
    gapMs:       lastCycleTs ? gapMs : null,
    gapMin:      lastCycleTs ? gapMin : null,
    healthy,
    threshold:   CRON_GAP_THRESHOLD_MS,
    checkedAt:   nowIso,
  };

  if (!healthy) {
    const msg = lastCycleTs
      ? `Agent "${agentName}" cron gap ${gapMin}m (threshold: ${CRON_GAP_THRESHOLD_MS/60000}m). Cron may have stalled.`
      : `Agent "${agentName}" has no state file — cron may never have run.`;
    console.warn(`[cron-gap-detector] ⚠️  CRON GAP: ${msg}`);
    const alert = {
      id:        randomUUID(),
      ts:        nowIso,
      alertType: 'cron_gap',
      agent:     agentName,
      lastCycleTs,
      gapMs:     lastCycleTs ? gapMs : null,
      message:   msg,
    };
    writeAlert(alert);
  } else {
    console.log(`[cron-gap-detector] ✓ ${agentName.padEnd(12)} cron last cycle ${gapMin}m ago — OK`);
  }
}

// Write cron-health.json to MinIO
const cronHealthSummary = {
  ts:          nowIso,
  generatedBy: 'rocky',
  agents:      cronHealth,
  heartbeats:  Object.fromEntries(checked.map(c => [c.agent, c])),
};

try {
  const tmpFile = `/tmp/cron-health-${Date.now()}.json`;
  const { writeFileSync } = await import('fs');
  writeFileSync(tmpFile, JSON.stringify(cronHealthSummary, null, 2), 'utf8');
  execFileSync(MC, ['cp', tmpFile, `${SHARED_PATH}/cron-health.json`], { stdio: ['pipe', 'pipe', 'pipe'] });
  console.log(`[cron-gap-detector] cron-health.json written to MinIO.`);
} catch (e) {
  console.warn(`[cron-gap-detector] Failed to write cron-health.json to MinIO: ${e.message}`);
}
