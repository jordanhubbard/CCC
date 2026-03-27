#!/usr/bin/env node
/**
 * jkh-morning-context.mjs
 * wq-R-003: jkh morning context injection
 *
 * At 08:00 PT each day, reads jkh-state.json from MinIO (agents/shared/jkh-state.json)
 * and writes a condensed context summary to memory/jkh-morning-YYYY-MM-DD.md.
 * Any agent session spinning up that day can read this file instead of fetching
 * MinIO mid-conversation — reduces latency and ensures all agents start the day
 * with the same jkh situational awareness.
 *
 * Usage: node jkh-morning-context.mjs [--date YYYY-MM-DD]
 * Designed to be triggered by cron at 08:00 PT.
 */

import { execSync } from 'child_process';
import { writeFileSync, mkdirSync, existsSync } from 'fs';
import { join, dirname } from 'path';
import { fileURLToPath } from 'url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const WORKSPACE = join(__dirname, '../../');
const MC = process.env.MC_BIN || '/home/jkh/.local/bin/mc';
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
const jkhState = mcCat(`${SHARED_PREFIX}/jkh-state.json`);
const peerStatus = mcCat(`${SHARED_PREFIX}/peer-status.json`);
const skillsDrift = mcCat(`${SHARED_PREFIX}/skills-drift.json`);

if (!jkhState) {
  console.error('[jkh-morning-context] ERROR: Could not fetch jkh-state.json from MinIO');
  process.exit(1);
}

// Build markdown summary
const lines = [];
lines.push(`# jkh Morning Context — ${today}`);
lines.push(`_Generated at ${now} by ${process.env.AGENT_NAME || 'agent'}_`);
lines.push('');
lines.push('## jkh Status');
lines.push(`- **Last seen:** ${jkhState.last_seen_ts || 'unknown'} on ${jkhState.last_seen_channel || 'unknown'}`);
lines.push(`- **Timezone:** America/Los_Angeles`);
lines.push(`- **Phone:** +18312277540`);
lines.push('');

if (jkhState.recent_context) {
  lines.push('## Recent Context');
  lines.push(jkhState.recent_context);
  lines.push('');
}

if (jkhState.active_topics && jkhState.active_topics.length > 0) {
  lines.push('## Active Topics');
  jkhState.active_topics.forEach(t => lines.push(`- ${t}`));
  lines.push('');
}

if (jkhState.open_asks && jkhState.open_asks.length > 0) {
  lines.push('## Open Asks (jkh waiting on agents)');
  jkhState.open_asks.forEach(a => lines.push(`- ${a}`));
  lines.push('');
}

// Add per-agent last seen
if (jkhState.agents) {
  lines.push('## Per-Agent Last Contact');
  for (const [agent, data] of Object.entries(jkhState.agents)) {
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

// Write to memory/jkh-morning-YYYY-MM-DD.md
const localPath = join(memoryDir, `jkh-morning-${today}.md`);
writeFileSync(localPath, markdown, 'utf8');
console.log(`[jkh-morning-context] Written: ${localPath}`);

// Also mirror to MinIO for cross-agent access
const remoteOk = mcPut(localPath, `${SHARED_PREFIX}/jkh-morning-${today}.md`);
if (remoteOk) {
  console.log(`[jkh-morning-context] Mirrored to MinIO: agents/shared/jkh-morning-${today}.md`);
} else {
  console.warn(`[jkh-morning-context] WARNING: MinIO mirror failed — local copy still available`);
}

process.exit(0);
