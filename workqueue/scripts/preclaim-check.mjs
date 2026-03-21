#!/usr/bin/env node
/**
 * preclaim-check.mjs
 * wq-R-011: Pre-claim dry-run / capability check for workqueue items.
 *
 * Before formally claiming a task, run a lightweight check:
 *   1. Does the agent have the required skills/capabilities?
 *   2. Is it currently quiet hours? (GPU tasks, noisy pings blocked)
 *   3. Is the task blocked by unresolved dependencies?
 *
 * If any check fails → returns { ok: false, reason, action } and logs to alerts.jsonl.
 * If all checks pass → returns { ok: true }.
 *
 * Usage (CLI):
 *   node preclaim-check.mjs <item-id>           # check by item ID from queue.json
 *   node preclaim-check.mjs --item '{"id":...}' # check inline JSON item
 *   node preclaim-check.mjs --all               # check all pending Rocky items
 *
 * Export:
 *   preClaimCheck(item, queueItems, options) → { ok, reason, action }
 */

import { readFileSync, appendFileSync } from 'fs';
import { resolve, dirname } from 'path';
import { fileURLToPath } from 'url';

const __dir = dirname(fileURLToPath(import.meta.url));
const QUEUE_PATH   = resolve(__dir, '../queue.json');
const ALERTS_PATH  = resolve(__dir, '../alerts.jsonl');
const AGENT_NAME   = process.env.AGENT_NAME || 'rocky';
const TZ           = 'America/Los_Angeles';
const QUIET_START  = 23;
const QUIET_END    = 8;

// ── Capability map ─────────────────────────────────────────────────────────────
// Tags / keywords that require specific agents or environments.
// If a task has a required tag that maps to a different agent, we should back off.
const CAPABILITY_REQUIREMENTS = {
  // GPU tasks → Natasha only
  'gpu':        ['natasha'],
  'gpu_task':   ['natasha'],
  'render':     ['natasha'],
  // Mac/browser tasks → Bullwinkle
  'mac':        ['bullwinkle'],
  'browser':    ['bullwinkle'],
  'imessage':   ['bullwinkle'],
  // Infrastructure → Rocky (or any)
  'infrastructure': ['rocky', 'all'],
  'observability':  ['rocky', 'all'],
  'coordination':   ['rocky', 'bullwinkle', 'natasha'],
  // jkh-action items
  'action-needed': ['jkh'],
};

// ── Quiet-hours check ──────────────────────────────────────────────────────────
function currentHourPT() {
  const now = new Date();
  const str = now.toLocaleString('en-US', { timeZone: TZ, hour: '2-digit', hour12: false });
  return parseInt(str, 10);
}

function isQuietHours() {
  const h = currentHourPT();
  return h >= QUIET_START || h < QUIET_END;
}

const QUIET_SENSITIVE_TAGS = new Set(['gpu', 'gpu_task', 'render', 'slack_ping', 'noisy_external']);

function wouldViolateQuietHours(item) {
  if (!isQuietHours()) return false;
  const tags = item.tags || [];
  return tags.some(t => QUIET_SENSITIVE_TAGS.has(t.toLowerCase()));
}

// ── Dependency check ───────────────────────────────────────────────────────────
function hasUnresolvedDependencies(item, allItems, completed) {
  const deps = item.dependsOn || [];
  if (!deps.length) return { blocked: false };

  const completedIds = new Set((completed || []).map(i => i.id));
  const activeById   = Object.fromEntries((allItems || []).map(i => [i.id, i]));

  const unresolved = deps.filter(depId => {
    if (completedIds.has(depId)) return false;             // dep completed ✓
    const dep = activeById[depId];
    if (!dep) return false;                                 // dep not found — assume gone
    return dep.status !== 'completed';
  });

  return {
    blocked: unresolved.length > 0,
    unresolved,
  };
}

// ── Capability check ───────────────────────────────────────────────────────────
function checkCapability(item, agentName) {
  const tags = (item.tags || []).map(t => t.toLowerCase());
  for (const tag of tags) {
    const allowed = CAPABILITY_REQUIREMENTS[tag];
    if (allowed && !allowed.includes(agentName) && !allowed.includes('all')) {
      return {
        capable: false,
        reason: `Tag '${tag}' requires agent in [${allowed.join(', ')}]; I am '${agentName}'`,
      };
    }
  }

  // Also check assignee explicitly
  if (item.assignee && item.assignee !== agentName && item.assignee !== 'all') {
    return {
      capable: false,
      reason: `Item is assigned to '${item.assignee}', not '${agentName}'`,
    };
  }

  return { capable: true };
}

// ── Main check function ────────────────────────────────────────────────────────
/**
 * preClaimCheck(item, queueItems, completed, options) → { ok, reason, action }
 *
 * @param {object} item         - The queue item to check
 * @param {object[]} queueItems - Active items array (for dependency resolution)
 * @param {object[]} completed  - Completed items array (for dependency resolution)
 * @param {object} [options]
 * @param {string} [options.agentName]  - Override agent name (default: AGENT_NAME env)
 * @param {boolean} [options.quiet]     - Suppress console output
 * @returns {{ ok: boolean, reason?: string, action?: string }}
 */
export function preClaimCheck(item, queueItems = [], completed = [], options = {}) {
  const agent = options.agentName || AGENT_NAME;

  // 1. Capability check
  const cap = checkCapability(item, agent);
  if (!cap.capable) {
    return {
      ok: false,
      reason: cap.reason,
      action: 'skip',
      check: 'capability',
    };
  }

  // 2. Quiet hours check
  if (wouldViolateQuietHours(item)) {
    return {
      ok: false,
      reason: `Quiet hours (${QUIET_START}:00–${QUIET_END}:00 PT) — item has GPU/noisy tags`,
      action: 'defer_until_morning',
      check: 'quiet_hours',
    };
  }

  // 3. Dependency check
  const depCheck = hasUnresolvedDependencies(item, queueItems, completed);
  if (depCheck.blocked) {
    return {
      ok: false,
      reason: `Unresolved dependencies: [${depCheck.unresolved.join(', ')}]`,
      action: 'wait_for_deps',
      check: 'dependencies',
      unresolved: depCheck.unresolved,
    };
  }

  // 4. Status check — don't re-claim already-claimed items
  if (item.claimedBy && item.claimedBy !== agent) {
    const claimedAt = item.claimedAt ? new Date(item.claimedAt) : null;
    const staleMs   = 15 * 60 * 1000; // 15 minutes
    const isStale   = claimedAt ? (Date.now() - claimedAt.getTime() > staleMs) : false;
    if (!isStale) {
      return {
        ok: false,
        reason: `Already claimed by '${item.claimedBy}' at ${item.claimedAt} (not stale yet)`,
        action: 'back_off',
        check: 'claim_conflict',
      };
    }
    // If stale, proceed (stale-claim detection handles reset)
  }

  return { ok: true };
}

// ── Alert logger ───────────────────────────────────────────────────────────────
function logAlert(item, result) {
  const entry = {
    ts: new Date().toISOString(),
    agent: AGENT_NAME,
    type: 'preclaim_skip',
    itemId: item.id,
    check: result.check,
    reason: result.reason,
    action: result.action,
  };
  try {
    appendFileSync(ALERTS_PATH, JSON.stringify(entry) + '\n');
  } catch (e) {
    // Non-fatal — alert logging should never block processing
  }
}

// ── CLI entry point ────────────────────────────────────────────────────────────
if (process.argv[1]?.endsWith('preclaim-check.mjs')) {
  let queue;
  try {
    queue = JSON.parse(readFileSync(QUEUE_PATH, 'utf8'));
  } catch (e) {
    console.error(`Failed to read queue.json: ${e.message}`);
    process.exit(1);
  }

  const args = process.argv.slice(2);
  const allItems = queue.items || [];
  const completed = queue.completed || [];

  if (args[0] === '--all') {
    // Check all pending Rocky items
    const rockyPending = allItems.filter(
      i => i.assignee === AGENT_NAME && i.status === 'pending'
    );
    if (!rockyPending.length) {
      console.log(`No pending items for ${AGENT_NAME}.`);
      process.exit(0);
    }
    console.log(`Pre-claim checks for ${AGENT_NAME} (${rockyPending.length} pending items):\n`);
    let allClear = true;
    for (const item of rockyPending) {
      const result = preClaimCheck(item, allItems, completed);
      const icon = result.ok ? '✅' : '⛔';
      console.log(`${icon} ${item.id}: ${item.title}`);
      if (!result.ok) {
        console.log(`   → ${result.check}: ${result.reason}`);
        console.log(`   → action: ${result.action}`);
        logAlert(item, result);
        allClear = false;
      }
    }
    console.log('');
    console.log(allClear ? '✅ All items clear to claim.' : '⚠️  Some items have pre-claim blocks (see above).');

  } else if (args[0] === '--item') {
    // Inline JSON item
    let item;
    try {
      item = JSON.parse(args[1]);
    } catch (e) {
      console.error('Invalid JSON item provided.');
      process.exit(1);
    }
    const result = preClaimCheck(item, allItems, completed);
    console.log(JSON.stringify(result, null, 2));
    if (!result.ok) logAlert(item, result);

  } else if (args[0]) {
    // Lookup by ID
    const itemId = args[0];
    const item = allItems.find(i => i.id === itemId)
               || completed.find(i => i.id === itemId);
    if (!item) {
      console.error(`Item '${itemId}' not found in queue.json`);
      process.exit(1);
    }
    const result = preClaimCheck(item, allItems, completed);
    console.log(JSON.stringify(result, null, 2));
    if (!result.ok) logAlert(item, result);

  } else {
    // Default: run --all
    const rockyPending = allItems.filter(
      i => (i.assignee === AGENT_NAME || i.assignee === 'all') && i.status === 'pending'
    );
    console.log(`Pre-claim dry-run for agent '${AGENT_NAME}' (${rockyPending.length} claimable items):\n`);
    for (const item of rockyPending) {
      const result = preClaimCheck(item, allItems, completed);
      const icon = result.ok ? '✅' : '⛔';
      console.log(`${icon} ${item.id} [${item.priority}]: ${item.title}`);
      if (!result.ok) {
        console.log(`   ↳ blocked by ${result.check}: ${result.reason} (${result.action})`);
        logAlert(item, result);
      }
    }
  }
}
