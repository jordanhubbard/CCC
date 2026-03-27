#!/usr/bin/env node
/**
 * activity-digest.mjs — Cross-agent activity digest for Rocky
 *
 * Maintains agents/shared/agent-activity-digest.json on MinIO.
 * Each agent appends a brief summary after completing non-trivial tasks.
 * Any agent can read the last N entries to synthesize a cross-team summary.
 *
 * Usage:
 *   node activity-digest.mjs --append '{"agent":"rocky","summary":"Did X","itemId":"wq-R-017","tags":["infra"]}'
 *   node activity-digest.mjs --read [--limit 20]
 *   node activity-digest.mjs --summary [--limit 20]
 */

import { execSync } from 'child_process';
import { readFileSync, writeFileSync, existsSync } from 'fs';
import { tmpdir } from 'os';
import { join } from 'path';

const MINIO_PATH = (process.env.MINIO_ALIAS || 'local') + '/agents/shared/agent-activity-digest.jsonl';
const LOCAL_TMP = join(tmpdir(), 'agent-activity-digest.jsonl');
const AGENT_NAME = process.env.AGENT_NAME || 'rocky';

function mcCmd(args) {
  try {
    return execSync(`mc ${args}`, { encoding: 'utf8', stdio: ['pipe', 'pipe', 'pipe'] });
  } catch (e) {
    return null;
  }
}

function fetchDigest() {
  mcCmd(`cp ${MINIO_PATH} ${LOCAL_TMP} 2>/dev/null`);
  if (!existsSync(LOCAL_TMP)) return [];
  const lines = readFileSync(LOCAL_TMP, 'utf8').trim().split('\n').filter(Boolean);
  return lines.map(l => { try { return JSON.parse(l); } catch { return null; } }).filter(Boolean);
}

function appendEntry(entry) {
  const entries = fetchDigest();
  const newEntry = {
    ts: new Date().toISOString(),
    agent: entry.agent || AGENT_NAME,
    summary: entry.summary,
    itemId: entry.itemId || null,
    tags: entry.tags || [],
  };
  entries.push(newEntry);
  const jsonl = entries.map(e => JSON.stringify(e)).join('\n') + '\n';
  writeFileSync(LOCAL_TMP, jsonl, 'utf8');
  mcCmd(`cp ${LOCAL_TMP} ${MINIO_PATH}`);
  console.log('Appended:', JSON.stringify(newEntry));
  return newEntry;
}

function readDigest(limit = 20) {
  const entries = fetchDigest();
  return entries.slice(-limit);
}

function buildSummary(limit = 20) {
  const entries = readDigest(limit);
  if (entries.length === 0) return 'No recent activity recorded.';

  const byAgent = {};
  for (const e of entries) {
    if (!byAgent[e.agent]) byAgent[e.agent] = [];
    byAgent[e.agent].push(e);
  }

  const lines = [];
  lines.push(`=== Cross-Agent Activity Digest (last ${entries.length} entries) ===`);
  lines.push(`As of ${new Date().toISOString()}\n`);

  for (const [agent, items] of Object.entries(byAgent)) {
    lines.push(`## ${agent.toUpperCase()}`);
    for (const item of items.slice(-5)) {
      const ts = new Date(item.ts).toLocaleString('en-US', { timeZone: 'America/Los_Angeles', hour12: false });
      const itemRef = item.itemId ? ` [${item.itemId}]` : '';
      lines.push(`  ${ts}${itemRef}: ${item.summary}`);
    }
    lines.push('');
  }

  return lines.join('\n');
}

// CLI
const args = process.argv.slice(2);
if (args[0] === '--append') {
  const data = JSON.parse(args[1]);
  appendEntry(data);
} else if (args[0] === '--read') {
  const limitIdx = args.indexOf('--limit');
  const limit = limitIdx >= 0 ? parseInt(args[limitIdx + 1]) : 20;
  const entries = readDigest(limit);
  console.log(JSON.stringify(entries, null, 2));
} else if (args[0] === '--summary') {
  const limitIdx = args.indexOf('--limit');
  const limit = limitIdx >= 0 ? parseInt(args[limitIdx + 1]) : 20;
  console.log(buildSummary(limit));
} else {
  console.log('Usage: activity-digest.mjs --append JSON | --read [--limit N] | --summary [--limit N]');
}

export { appendEntry, readDigest, buildSummary };
