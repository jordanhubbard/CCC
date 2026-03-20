#!/usr/bin/env node
/**
 * idea-promotion-check.mjs — wq-20260319-009
 *
 * Implements the idea voting / promotion workflow.
 *
 * Rules:
 *   - Items with priority="idea" or priority="low" are candidates
 *   - An item is promoted to priority="normal" and status="pending" when:
 *       (a) votes.length >= 2 AND at least one vote is from a non-originating agent, OR
 *       (b) "jkh" is in votes[] (jkh's vote is always sufficient alone)
 *   - Promotion adds a note and bumps itemVersion
 *   - Already-promoted items (priority != "idea"/"low") are skipped
 *   - Does NOT auto-assign — promoted items keep assignee="all"
 *
 * Usage: node idea-promotion-check.mjs [queue.json path] [--dry-run]
 *
 * Designed to be called from workqueue cron cycles.
 * Returns exit code 0 always; logs promotions to stdout.
 */

import { readFileSync, writeFileSync } from 'fs';
import { resolve } from 'path';

const DRY_RUN = process.argv.includes('--dry-run');
const QUEUE_PATH = resolve(
  process.argv.find(a => a.endsWith('.json')) ||
  new URL('../queue.json', import.meta.url).pathname
);
const AGENT_NAME = process.env.AGENT_NAME || 'natasha';
const NOW = new Date().toISOString();

function log(...args) {
  console.log(new Date().toISOString(), '[idea-promotion]', ...args);
}

function isIdea(item) {
  return item.priority === 'idea' || item.priority === 'low';
}

function shouldPromote(item) {
  const votes = item.votes || [];
  // jkh vote is always sufficient
  if (votes.includes('jkh')) return { promote: true, reason: 'jkh voted' };
  // 2+ agent votes, with at least one non-source vote
  if (votes.length >= 2) {
    const nonSource = votes.filter(v => v !== item.source);
    if (nonSource.length >= 1) {
      return { promote: true, reason: `${votes.length} votes (${votes.join(', ')})` };
    }
  }
  return { promote: false };
}

function main() {
  let queue;
  try {
    queue = JSON.parse(readFileSync(QUEUE_PATH, 'utf8'));
  } catch (e) {
    log('ERROR: Could not read queue:', e.message);
    process.exit(1);
  }

  const items = queue.items || [];
  let promotedCount = 0;
  const promotions = [];

  for (const item of items) {
    if (!isIdea(item)) continue;
    if (item.status === 'completed' || item.status === 'failed') continue;

    const { promote, reason } = shouldPromote(item);
    if (!promote) {
      log(`  ${item.id} "${item.title}" — not enough votes yet (${(item.votes||[]).join(', ') || 'none'})`);
      continue;
    }

    log(`  PROMOTING ${item.id} "${item.title}" — ${reason}`);
    promotions.push({ id: item.id, title: item.title, reason });

    if (!DRY_RUN) {
      item.priority = 'normal';
      item.status = 'pending';
      item.itemVersion = (item.itemVersion || 1) + 1;
      item.notes = (item.notes ? item.notes + '\n' : '') +
        `Promoted from idea to normal priority at ${NOW} by ${AGENT_NAME} (reason: ${reason}).`;
    }

    promotedCount++;
  }

  if (promotedCount === 0) {
    log('No ideas eligible for promotion this cycle.');
  } else {
    log(`Promoted ${promotedCount} idea(s).`);
    if (!DRY_RUN) {
      queue.lastSync = NOW;
      writeFileSync(QUEUE_PATH, JSON.stringify(queue, null, 2));
      log(`queue.json updated.`);
    } else {
      log('[dry-run] queue.json not written.');
    }
  }

  // Output summary
  if (promotions.length > 0) {
    console.log('PROMOTIONS:', JSON.stringify(promotions));
  } else {
    console.log('PROMOTIONS: none');
  }

  return promotions;
}

main();
