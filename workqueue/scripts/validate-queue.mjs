#!/usr/bin/env node
/**
 * validate-queue.mjs
 * 
 * Validates workqueue/queue.json against the schema defined in SCHEMA.md.
 * Reports violations to workqueue/alerts.jsonl without blocking processing.
 * 
 * Usage:
 *   node validate-queue.mjs [--queue path/to/queue.json] [--alerts path/to/alerts.jsonl] [--strict]
 * 
 * Options:
 *   --queue   Path to queue.json (default: workqueue/queue.json relative to script)
 *   --alerts  Path to alerts.jsonl (default: workqueue/alerts.jsonl)
 *   --strict  Exit with code 1 if any violations found (default: exit 0, just log)
 *   --quiet   Suppress stdout output (violations still written to alerts.jsonl)
 * 
 * Exit codes:
 *   0 - No violations (or violations found but --strict not set)
 *   1 - Violations found with --strict
 *   2 - Could not read/parse queue.json
 */

import { readFileSync, appendFileSync, existsSync } from 'fs';
import { resolve, dirname } from 'path';
import { fileURLToPath } from 'url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const WORKQUEUE_DIR = resolve(__dirname, '..');

// Parse args
const args = process.argv.slice(2);
const flag = (name) => args.includes(name);
const arg = (name) => { const i = args.indexOf(name); return i >= 0 ? args[i + 1] : null; };

const QUEUE_PATH = arg('--queue') || resolve(WORKQUEUE_DIR, 'queue.json');
const ALERTS_PATH = arg('--alerts') || resolve(WORKQUEUE_DIR, 'alerts.jsonl');
const STRICT = flag('--strict');
const QUIET = flag('--quiet');

const VALID_PRIORITIES = ['urgent', 'high', 'medium', 'normal', 'low', 'idea'];
const VALID_STATUSES = ['pending', 'in_progress', 'in-progress', 'completed', 'failed', 'blocked', 'deferred', 'cancelled', 'closed', 'incubating'];
const VALID_AGENTS = ['rocky', 'bullwinkle', 'natasha', 'all', 'jkh'];
const VALID_DIRECTIONS = ['outbound', 'inbound'];
const ISO_RE = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}/;
const ID_RE = /^wq-([RBN]-\d+|\d{8}-\d{3,}|[a-z0-9-]+)$/i;

function isISO(v) { return typeof v === 'string' && ISO_RE.test(v); }
function isNullOrISO(v) { return v === null || isISO(v); }

const violations = [];
const warn = (path, msg) => violations.push({ path, msg });

// ── Load queue ──────────────────────────────────────────────────────────────

let queue;
try {
  queue = JSON.parse(readFileSync(QUEUE_PATH, 'utf8'));
} catch (e) {
  const msg = `Could not read/parse queue.json: ${e.message}`;
  if (!QUIET) console.error(`[validate-queue] FATAL: ${msg}`);
  appendAlert({ severity: 'fatal', path: 'queue.json', msg });
  process.exit(2);
}

// ── Top-level fields ────────────────────────────────────────────────────────

if (typeof queue.version !== 'number') warn('version', 'Missing or non-numeric top-level version field');
if (typeof queue.agent !== 'string') warn('agent', 'Missing or non-string top-level agent field');
if (!isISO(queue.lastSync)) warn('lastSync', `lastSync is not a valid ISO-8601 string: ${queue.lastSync}`);
if (!Array.isArray(queue.items)) warn('items', 'items is not an array');
if (!Array.isArray(queue.completed)) warn('completed', 'completed is not an array');
if (!Array.isArray(queue.syncLog)) warn('syncLog', 'syncLog is not an array');

// ── Item validation ──────────────────────────────────────────────────────────

const seenIds = new Set();

function validateItem(item, arrayName, idx) {
  const p = (f) => `${arrayName}[${idx}].${f}`;

  if (typeof item.id !== 'string' || !item.id) {
    warn(p('id'), 'Missing or empty id');
  } else {
    if (!ID_RE.test(item.id)) warn(p('id'), `ID format unexpected: ${item.id} (expected wq-R-NNN, wq-B-NNN, wq-N-NNN, or wq-YYYYMMDD-NNN)`);
    if (seenIds.has(item.id)) warn(p('id'), `Duplicate ID: ${item.id}`);
    seenIds.add(item.id);
  }

  if (typeof item.itemVersion !== 'number' || item.itemVersion < 1) warn(p('itemVersion'), `itemVersion must be integer ≥ 1, got: ${item.itemVersion}`);
  if (!isISO(item.created)) warn(p('created'), `created is not valid ISO-8601: ${item.created}`);
  if (typeof item.source !== 'string') warn(p('source'), 'source is missing or not a string');
  if (!VALID_AGENTS.includes(item.assignee)) warn(p('assignee'), `assignee '${item.assignee}' is not one of: ${VALID_AGENTS.join(', ')}`);
  if (!VALID_PRIORITIES.includes(item.priority)) warn(p('priority'), `priority '${item.priority}' is not one of: ${VALID_PRIORITIES.join(', ')}`);
  if (!VALID_STATUSES.includes(item.status)) warn(p('status'), `status '${item.status}' is not one of: ${VALID_STATUSES.join(', ')}`);
  if (typeof item.title !== 'string' || !item.title) warn(p('title'), 'title is missing or empty');
  if (typeof item.attempts !== 'number' || item.attempts < 0) warn(p('attempts'), `attempts must be integer ≥ 0, got: ${item.attempts}`);
  if (typeof item.maxAttempts !== 'number' || item.maxAttempts < 1) warn(p('maxAttempts'), `maxAttempts must be integer ≥ 1, got: ${item.maxAttempts}`);

  // Status-specific checks
  if (item.status === 'completed' || item.status === 'failed') {
    if (!isISO(item.completedAt)) warn(p('completedAt'), `${item.status} item must have valid completedAt`);
    if (item.result === null || item.result === undefined) {
      // Soft warning — some completed items have null result historically
      warn(p('result'), `${item.status} item should have a result string (found null/undefined)`);
    }
  }

  // Claim consistency
  if (item.claimedBy !== null && item.claimedBy !== undefined) {
    if (!isISO(item.claimedAt) && item.claimedAt !== null) warn(p('claimedAt'), `claimedBy is set but claimedAt is not valid ISO-8601: ${item.claimedAt}`);
    // Check for stale claims (>15 min old with still-pending status)
    if (item.claimedAt && isISO(item.claimedAt) && (item.status === 'pending' || item.status === 'in_progress')) {
      const age = Date.now() - new Date(item.claimedAt).getTime();
      if (age > 15 * 60 * 1000) {
        warn(p('claimedAt'), `Stale claim: ${item.id} claimed by ${item.claimedBy} at ${item.claimedAt} (${Math.round(age/60000)}m ago), status=${item.status}`);
      }
    }
  }

  // votes should be an array if present
  if (item.votes !== undefined && !Array.isArray(item.votes)) warn(p('votes'), 'votes must be an array');

  // tags should be an array if present
  if (item.tags !== undefined && !Array.isArray(item.tags)) warn(p('tags'), 'tags must be an array');

  // dependsOn should be an array if present
  if (item.dependsOn !== undefined && !Array.isArray(item.dependsOn)) warn(p('dependsOn'), 'dependsOn must be an array');
}

if (Array.isArray(queue.items)) {
  queue.items.forEach((item, i) => validateItem(item, 'items', i));
}
if (Array.isArray(queue.completed)) {
  queue.completed.forEach((item, i) => validateItem(item, 'completed', i));
}

// ── Cross-array integrity checks ─────────────────────────────────────────────

// Items in completed[] should have status 'completed' or 'failed'.
// completedAt presence is the truth — status string must agree.
if (Array.isArray(queue.completed)) {
  queue.completed.forEach((item, i) => {
    const p = (f) => `completed[${i}].${f}`;
    if (isISO(item.completedAt) && item.status !== 'completed' && item.status !== 'failed' &&
        item.status !== 'cancelled' && item.status !== 'closed') {
      warn(p('status'),
        `Item ${item.id} is in completed[] with completedAt=${item.completedAt} but status="${item.status}" — should be completed/failed/cancelled/closed`);
    }
    if (!isISO(item.completedAt) && (item.status === 'completed' || item.status === 'failed')) {
      warn(p('completedAt'),
        `Item ${item.id} is in completed[] with status="${item.status}" but completedAt is missing or invalid`);
    }
  });
}

// Items in items[] that have completedAt set are probably in the wrong array
if (Array.isArray(queue.items)) {
  queue.items.forEach((item, i) => {
    if (isISO(item.completedAt)) {
      warn(`items[${i}].completedAt`,
        `Item ${item.id} is in items[] but has completedAt=${item.completedAt} — should be in completed[]`);
    }
  });
}

// ── SyncLog validation ───────────────────────────────────────────────────────

if (Array.isArray(queue.syncLog)) {
  queue.syncLog.forEach((entry, i) => {
    const p = (f) => `syncLog[${i}].${f}`;
    if (!isISO(entry.ts)) warn(p('ts'), `ts is not valid ISO-8601: ${entry.ts}`);
    if (typeof entry.peer !== 'string') warn(p('peer'), 'peer is missing or not a string');
    if (typeof entry.channel !== 'string') warn(p('channel'), 'channel is missing or not a string');
    if (!VALID_DIRECTIONS.includes(entry.direction)) warn(p('direction'), `direction '${entry.direction}' must be 'outbound' or 'inbound'`);
    if (typeof entry.success !== 'boolean') warn(p('success'), `success must be boolean, got: ${entry.success}`);
    if (typeof entry.itemCount !== 'number') warn(p('itemCount'), `itemCount must be a number, got: ${entry.itemCount}`);
  });
}

// ── Report ───────────────────────────────────────────────────────────────────

const ts = new Date().toISOString();
const summary = {
  ts,
  agent: queue.agent || 'unknown',
  queuePath: QUEUE_PATH,
  violationCount: violations.length,
  violations,
};

if (violations.length > 0) {
  if (!QUIET) {
    console.warn(`[validate-queue] ⚠️  ${violations.length} violation(s) found in queue.json:`);
    violations.forEach(({ path, msg }) => console.warn(`  • ${path}: ${msg}`));
  }
  appendAlert({ severity: 'warning', ...summary });
} else {
  if (!QUIET) {
    console.log(`[validate-queue] ✅ queue.json is valid (${(queue.items||[]).length} active, ${(queue.completed||[]).length} completed, ${(queue.syncLog||[]).length} sync entries)`);
  }
}

function appendAlert(obj) {
  try {
    appendFileSync(ALERTS_PATH, JSON.stringify(obj) + '\n');
  } catch (e) {
    if (!QUIET) console.error(`[validate-queue] Could not write to alerts.jsonl: ${e.message}`);
  }
}

process.exit(STRICT && violations.length > 0 ? 1 : 0);
