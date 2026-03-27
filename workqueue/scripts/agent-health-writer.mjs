#!/usr/bin/env node
/**
 * agent-health-writer.mjs
 * wq-N-004: Write compact agent health JSON to MinIO.
 *
 * Writes agents/shared/agent-health-rocky.json with:
 *   agentName, cycleCount, lastCycleTs, lastSyncTs,
 *   pendingOwned, completedLast7d, errors
 *
 * Usage: node agent-health-writer.mjs
 */

import { execFileSync } from 'child_process';
import { readFileSync, writeFileSync, existsSync } from 'fs';
import { join, dirname } from 'path';
import { fileURLToPath } from 'url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const WORKSPACE  = join(__dirname, '../../');
const MC         = process.env.MC_BIN || '/home/jkh/.local/bin/mc';
const MINIO_PATH = (process.env.MINIO_ALIAS || 'local') + '/agents/shared/agent-health-' + (process.env.AGENT_NAME || 'agent') + '.json';
const AGENT_NAME = 'rocky';

function readJson(filePath) {
  if (!existsSync(filePath)) return null;
  try { return JSON.parse(readFileSync(filePath, 'utf8')); } catch { return null; }
}

function mcPut(json, remotePath) {
  const tmp = `/tmp/agent-health-${AGENT_NAME}-${Date.now()}.json`;
  writeFileSync(tmp, JSON.stringify(json, null, 2), 'utf8');
  execFileSync(MC, ['cp', tmp, remotePath], { stdio: ['pipe', 'pipe', 'pipe'] });
  // best-effort cleanup
  try { execFileSync('rm', [tmp], { stdio: 'pipe' }); } catch {}
}

const now        = new Date();
const sevenDaysAgo = new Date(now.getTime() - 7 * 24 * 60 * 60 * 1000);

const stateFile = join(WORKSPACE, 'workqueue', 'state-rocky.json');
const queueFile = join(WORKSPACE, 'workqueue', 'queue.json');

const state = readJson(stateFile);
const queue = readJson(queueFile);

const cycleCount  = state?.cycleCount  ?? 0;
const lastCycleTs = state?.lastCycleTs ?? null;
const lastSyncTs  = queue?.lastSync    ?? null;

const allItems    = [
  ...(queue?.items     ?? []),
  ...(queue?.completed ?? []),
];

const pendingOwned = allItems.filter(
  i => i.assignee === AGENT_NAME && i.status === 'pending'
).length;

const completedLast7d = allItems.filter(
  i => i.assignee === AGENT_NAME &&
       i.status   === 'completed' &&
       i.completedAt &&
       new Date(i.completedAt) > sevenDaysAgo
).length;

const health = {
  agentName:       AGENT_NAME,
  cycleCount,
  lastCycleTs,
  lastSyncTs,
  writtenAt:       now.toISOString(),
  pendingOwned,
  completedLast7d,
  errors:          [],
};

mcPut(health, MINIO_PATH);
console.log('[agent-health-writer] Wrote to MinIO:', MINIO_PATH);
console.log(JSON.stringify(health, null, 2));
