#!/usr/bin/env node
/**
 * morning-context.mjs
 * Morning context injection for agent fleet
 *
 * At 08:00 (operator timezone) each day, reads operator-state.json from MinIO
 * (agents/shared/operator-state.json) and writes a condensed context summary to
 * memory/morning-YYYY-MM-DD.md. Any agent session spinning up that day can read
 * this file instead of fetching MinIO mid-conversation — reduces latency and
 * ensures all agents start the day with the same operator situational awareness.
 *
 * Usage: node morning-context.mjs [--date YYYY-MM-DD]
 * Designed to be triggered by cron at 08:00 local time.
 */

import { execSync } from 'child_process';
import { writeFileSync, mkdirSync, existsSync } from 'fs';
import { join, dirname } from 'path';
import { fileURLToPath } from 'url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const WORKSPACE = join(__dirname, '../../');
const MC = process.env.MC_BIN || 'mc';
const MINIO_ALIAS = process.env.MINIO_ALIAS || 'local';
const SHARED_PREFIX = `${MINIO_ALIAS}/agents/shared`;

function mcCat(path) {
  try {
    return JSON.parse(execSync(`${MC} cat ${path}`, { encoding: 'utf8', stdio: ['pipe','pipe','pipe'] }));
  } catch {
    return null;
  }
}

function mcPut(localPath, remotePath) {
  try {
    execSync(`${MC} cp ${localPath} ${remotePath}`, { stdio: ['pipe','pipe','pipe'] });
    return true;
  } catch {
    return false;
  }
}

function getTodayPT() {
  // Check for --date flag
  const dateArg = process.argv.find(a => a.startsWith('--date'));
  if (dateArg) {
    const d = dateArg.split('=')[1] || process.argv[process.argv.indexOf(dateArg) + 1];
    if (d && /^\d{4}-\d{2}-\d{2}$/.test(d)) return d;
  }
  return new Date().toLocaleDateString('en-CA', { timeZone: 'America/Los_Angeles' });
}

const today = getTodayPT();
const now = new Date().toISOString();

// Fetch source data
const operatorState = mcCat(`${SHARED_PREFIX}/operator-state.json`);
const peerStatus = mcCat(`${SHARED_PREFIX}/peer-status.json`);
const skillsDrift = mcCat(`${SHARED_PREFIX}/skills-drift.json`);

if (!operatorState) {
  console.error('[morning-context] ERROR: Could not fetch operator-state.json from MinIO');
  process.exit(1);
}

// Build markdown summary
const lines = [];
lines.push(`# Operator Morning Context — ${today}`);
lines.push(`_Generated at ${now} by ${process.env.AGENT_NAME || 'agent'}_`);
lines.push('');
lines.push('## Operator Status');
lines.push(`- **Last seen:** ${operatorState.last_seen_ts || 'unknown'} on ${operatorState.last_seen_channel || 'unknown'}`);
lines.push(`- **Timezone:** ${operatorState.operator?.timezone || 'unknown'}`);
lines.push('');

if (operatorState.recent_context) {
  lines.push('## Recent Context');
  lines.push(operatorState.recent_context);
  lines.push('');
}

if (operatorState.active_topics && operatorState.active_topics.length > 0) {
  lines.push('## Active Topics');
  operatorState.active_topics.forEach(t => lines.push(`- ${t}`));
  lines.push('');
}

if (operatorState.open_asks && operatorState.open_asks.length > 0) {
  lines.push('## Open Asks (operator waiting on agents)');
  operatorState.open_asks.forEach(a => lines.push(`- ${a}`));
  lines.push('');
}

// Add per-agent last seen
if (operatorState.agents) {
  lines.push('## Per-Agent Last Contact');
  for (const [agent, data] of Object.entries(operatorState.agents)) {
    lines.push(`- **${agent}:** ${data.last_seen_ts || 'unknown'} (${data.last_seen_channel || 'unknown'})`);
  }
  lines.push('');
}

// Add fleet status
if (peerStatus) {
  lines.push('## Fleet Status');
  for (const [peer, data] of Object.entries(peerStatus.peers || {})) {
    const drift = data.skillsDrift ? ' ⚠️ skills drift' : '';
    lines.push(`- **${peer}:** ${data.status || 'unknown'}${drift}`);
  }
  lines.push('');
}

// Skills drift summary
if (skillsDrift && (skillsDrift.summary.drifted.length > 0 || skillsDrift.summary.missing_heartbeat.length > 0)) {
  lines.push('## ⚠️ Skills/Heartbeat Alerts');
  if (skillsDrift.summary.drifted.length > 0) {
    lines.push(`- Skill drift detected: ${skillsDrift.summary.drifted.join(', ')}`);
  }
  if (skillsDrift.summary.missing_heartbeat.length > 0) {
    lines.push(`- Missing heartbeat: ${skillsDrift.summary.missing_heartbeat.join(', ')}`);
  }
  lines.push('');
}

lines.push('---');
lines.push('_Read this file at session start for fast situational awareness. Do not re-fetch MinIO unless staleness matters._');

const markdown = lines.join('\n');

// Ensure memory dir exists
const memoryDir = join(WORKSPACE, 'memory');
if (!existsSync(memoryDir)) mkdirSync(memoryDir, { recursive: true });

// Write to memory/morning-YYYY-MM-DD.md
const localPath = join(memoryDir, `morning-${today}.md`);
writeFileSync(localPath, markdown, 'utf8');
console.log(`[morning-context] Written: ${localPath}`);

// Also mirror to MinIO for cross-agent access
const remoteOk = mcPut(localPath, `${SHARED_PREFIX}/morning-${today}.md`);
if (remoteOk) {
  console.log(`[morning-context] Mirrored to MinIO: agents/shared/morning-${today}.md`);
} else {
  console.warn(`[morning-context] WARNING: MinIO mirror failed — local copy still available`);
}

process.exit(0);
