#!/usr/bin/env node
/**
 * decision-log.mjs
 * wq-R-007: Log agent decisions with rationale to workqueue/decision-log.jsonl
 *
 * Exports:
 *   logDecision(agentName, decisionType, summary, rationale, relatedItems=[])
 *   getRecentDecisions(agentName, limit=20)
 */

import { appendFileSync, readFileSync, existsSync } from 'fs';
import { join, dirname } from 'path';
import { fileURLToPath } from 'url';
import { randomUUID } from 'crypto';

const __dirname = dirname(fileURLToPath(import.meta.url));
const LOG_FILE  = join(__dirname, '../../workqueue/decision-log.jsonl');

const VALID_TYPES = new Set([
  'task_claim', 'task_reassign', 'vote', 'promotion', 'escalation', 'skip'
]);

/**
 * logDecision — append a decision entry to decision-log.jsonl.
 *
 * @param {string}   agentName     e.g. 'rocky', 'bullwinkle', 'natasha'
 * @param {string}   decisionType  one of: task_claim|task_reassign|vote|promotion|escalation|skip
 * @param {string}   summary       short human-readable summary
 * @param {string}   rationale     explanation of why this decision was made
 * @param {string[]} relatedItems  optional list of workqueue item IDs
 * @returns {{ id: string, ts: string }}  the generated id and timestamp
 */
export function logDecision(agentName, decisionType, summary, rationale, relatedItems = []) {
  if (!VALID_TYPES.has(decisionType)) {
    throw new Error(`Invalid decisionType "${decisionType}". Must be one of: ${[...VALID_TYPES].join(', ')}`);
  }

  const entry = {
    id:           randomUUID(),
    ts:           new Date().toISOString(),
    agent:        agentName,
    type:         decisionType,
    summary,
    rationale,
    relatedItems: Array.isArray(relatedItems) ? relatedItems : [],
  };

  appendFileSync(LOG_FILE, JSON.stringify(entry) + '\n', 'utf8');
  return { id: entry.id, ts: entry.ts };
}

/**
 * getRecentDecisions — read the last `limit` entries from decision-log.jsonl,
 * optionally filtered to a specific agent. Returns entries newest-first.
 *
 * @param {string|null} agentName  filter by agent name, or null/'' for all agents
 * @param {number}      limit      max entries to return (default 20)
 * @returns {object[]}
 */
export function getRecentDecisions(agentName = null, limit = 20) {
  if (!existsSync(LOG_FILE)) return [];

  const lines = readFileSync(LOG_FILE, 'utf8')
    .split('\n')
    .filter(l => l.trim());

  const entries = [];
  for (const line of lines) {
    try { entries.push(JSON.parse(line)); } catch { /* skip malformed lines */ }
  }

  const filtered = agentName
    ? entries.filter(e => e.agent === agentName)
    : entries;

  // Newest-first
  return filtered.reverse().slice(0, limit);
}

// ── CLI self-test ─────────────────────────────────────────────────────────────
if (process.argv[1]?.endsWith('decision-log.mjs')) {
  console.log('[decision-log] Writing 2 sample decisions...\n');

  const r1 = logDecision(
    'rocky',
    'task_claim',
    'Claimed wq-R-007 (decision-log module)',
    'No other agent has relevant infrastructure experience; falls within Rocky beat. Item unowned for <1h.',
    ['wq-R-007']
  );
  console.log('Entry 1:', r1);

  const r2 = logDecision(
    'rocky',
    'escalation',
    'Escalated wq-20260319-015 from normal→high',
    'Item pending 55h+ without a claim from Bullwinkle. Reassigned to Rocky per escalation policy (>48h unclaimed normal item).',
    ['wq-20260319-015']
  );
  console.log('Entry 2:', r2);

  console.log('\n[decision-log] Reading back recent decisions for rocky (limit 5):\n');
  const recent = getRecentDecisions('rocky', 5);
  for (const e of recent) {
    console.log(`  [${e.ts}] ${e.type.padEnd(14)} ${e.summary}`);
    console.log(`    rationale: ${e.rationale.slice(0, 80)}...`);
    console.log(`    related:   [${e.relatedItems.join(', ')}]`);
    console.log();
  }
  console.log(`[decision-log] Total returned: ${recent.length}`);
}
