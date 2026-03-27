#!/usr/bin/env node
/**
 * wq-003: Priority escalation for aging workqueue items
 *
 * Items that have been pending/unclaimed for >ESCALATION_HOURS get promoted
 * to the next priority level. Writes back to queue.json and logs escalations.
 *
 * Priority ladder: idea → low → normal → high → urgent
 * Escalation threshold: configurable per level (default 24h for normal, 48h for idea)
 *
 * Usage: node priority-escalation.mjs [--dry-run] [--queue /path/to/queue.json]
 */

import fs from 'fs';
import path from 'path';
import { execSync } from 'child_process';

const WORKSPACE = process.env.WORKSPACE || '/home/jkh/.openclaw/workspace';
const QUEUE_FILE = path.join(WORKSPACE, 'workqueue', 'queue.json');
const DRY_RUN = process.argv.includes('--dry-run');
const MC = process.env.MC_BIN || '/home/jkh/.local/bin/mc';
const MINIO_ALIAS = process.env.MINIO_ALIAS || 'local';

const PRIORITY_LADDER = ['idea', 'low', 'normal', 'high', 'urgent'];

// How long (hours) an item must be pending before escalating from that priority
const ESCALATION_HOURS = {
  idea: 72,    // ideas escalate after 3 days if not claimed
  low: 48,
  normal: 24,
  high: 8,
  urgent: 0,   // urgent never escalates (already top)
};

const MAX_ESCALATE_TO = 'high'; // never auto-escalate to urgent — that's human-only

function nextPriority(current) {
  const idx = PRIORITY_LADDER.indexOf(current);
  if (idx === -1 || idx >= PRIORITY_LADDER.indexOf(MAX_ESCALATE_TO)) return null;
  return PRIORITY_LADDER[idx + 1];
}

function hoursAgo(isoTs) {
  if (!isoTs) return Infinity;
  return (Date.now() - new Date(isoTs).getTime()) / (1000 * 60 * 60);
}

function run() {
  const raw = fs.readFileSync(QUEUE_FILE, 'utf8');
  const data = JSON.parse(raw);
  const now = new Date().toISOString();
  const escalated = [];

  for (const item of (data.items || [])) {
    // Only escalate pending/unclaimed items
    if (!['pending', 'blocked'].includes(item.status)) continue;
    if (item.claimedBy) continue; // already claimed, don't escalate

    const priority = item.priority || 'normal';
    const thresholdHours = ESCALATION_HOURS[priority];
    if (thresholdHours === undefined || thresholdHours === 0) continue;

    const age = hoursAgo(item.created || item.lastAttempt);
    if (age < thresholdHours) continue;

    const newPriority = nextPriority(priority);
    if (!newPriority) continue;

    const oldPriority = priority;
    if (!DRY_RUN) {
      item.priority = newPriority;
      item.itemVersion = (item.itemVersion || 1) + 1;
      item.notes = (item.notes || '') +
        `\n[escalated] ${oldPriority}→${newPriority} at ${now} (age: ${age.toFixed(1)}h)`;
    }

    escalated.push({ id: item.id, title: item.title, from: oldPriority, to: newPriority, age: age.toFixed(1) });
  }

  if (escalated.length === 0) {
    console.log('[priority-escalation] no items to escalate');
    return;
  }

  console.log(`[priority-escalation] ${DRY_RUN ? 'DRY RUN — ' : ''}escalated ${escalated.length} items:`);
  for (const e of escalated) {
    console.log(`  ${e.id} "${e.title}" ${e.from}→${e.to} (${e.age}h old)`);
  }

  if (!DRY_RUN) {
    data.lastSync = now;
    fs.writeFileSync(QUEUE_FILE, JSON.stringify(data, null, 2));

    // Sync to MinIO
    try {
      execSync(`echo '${JSON.stringify(data)}' | ${MC} pipe ${MINIO_ALIAS}/agents/shared/workqueue-rocky.json`, { timeout: 10000 });
      console.log('[priority-escalation] synced to MinIO');
    } catch (e) {
      console.error('[priority-escalation] MinIO sync failed:', e.message);
    }

    // Append to syncLog
    try {
      const logEntry = { ts: now, agent: 'rocky', action: 'priority_escalation', items: escalated };
      execSync(`echo '${JSON.stringify(logEntry)}' | ${MC} pipe --append ${MINIO_ALIAS}/agents/shared/workqueue-sync-log.jsonl`, { timeout: 10000 });
    } catch (_) {}
  }
}

run();
