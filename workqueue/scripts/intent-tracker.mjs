#!/usr/bin/env node
/**
 * intent-tracker.mjs
 * Operator intent tracker — passive want-log from conversations.
 *
 * Maintains agents/shared/operator-intents.json on MinIO.
 * When any agent notices the operator mention wanting something in passing,
 * this script adds a low-confidence intent entry.
 *
 * Intent entries:
 *   { id, ts, agent, channel, confidence, phrase, intent, tags, status, source_context }
 *
 * Also provides read helpers:
 *   - listIntents()   — read current intents from MinIO
 *   - findRelevant(tags, keywords) — surface matching intents
 *   - suggestIdeas()  — return intents not yet in the workqueue
 *
 * Usage (CLI):
 *   node intent-tracker.mjs --list
 *   node intent-tracker.mjs --add '{"phrase":"...","intent":"...","channel":"mattermost","confidence":"low","tags":["gpu"]}'
 *   node intent-tracker.mjs --suggest    # intents not yet tasked
 *   node intent-tracker.mjs --status <id> tasked  # mark an intent as actioned
 */

import { execSync, spawnSync } from 'child_process';
import { readFileSync, writeFileSync, existsSync } from 'fs';
import { resolve, dirname } from 'path';
import { fileURLToPath } from 'url';
import crypto from 'crypto';

const __dir        = dirname(fileURLToPath(import.meta.url));
const QUEUE_PATH   = resolve(__dir, '../queue.json');
const LOCAL_CACHE  = resolve(__dir, '../operator-intents-cache.json');
const MINIO_PATH   = (process.env.MINIO_ALIAS || 'local') + '/agents/shared/operator-intents.json';
const AGENT_NAME   = process.env.AGENT_NAME || 'rocky';

// ── MinIO helpers ──────────────────────────────────────────────────────────────
function mcGet(path) {
  const result = spawnSync('mc', ['cat', path], { encoding: 'utf8' });
  if (result.status !== 0) return null;
  try { return JSON.parse(result.stdout); } catch { return null; }
}

function mcPut(path, data) {
  const json = JSON.stringify(data, null, 2);
  const result = spawnSync('mc', ['pipe', path], {
    input: json,
    encoding: 'utf8',
  });
  return result.status === 0;
}

// ── Load intents ───────────────────────────────────────────────────────────────
function loadIntents() {
  let data = mcGet(MINIO_PATH);
  if (!data) {
    // Initialize empty structure
    data = {
      schemaVersion: 1,
      lastUpdated: new Date().toISOString(),
      intents: [],
    };
  }
  // Cache locally for offline fallback
  try {
    writeFileSync(LOCAL_CACHE, JSON.stringify(data, null, 2));
  } catch {}
  return data;
}

// ── Save intents ───────────────────────────────────────────────────────────────
function saveIntents(data) {
  data.lastUpdated = new Date().toISOString();
  const ok = mcPut(MINIO_PATH, data);
  if (ok) {
    try { writeFileSync(LOCAL_CACHE, JSON.stringify(data, null, 2)); } catch {}
  }
  return ok;
}

// ── Add intent ─────────────────────────────────────────────────────────────────
/**
 * addIntent(entry) → saved intent object
 *
 * @param {object} entry
 * @param {string} entry.phrase         - operator's exact or paraphrased phrase
 * @param {string} entry.intent         - What the operator wants (normalized)
 * @param {string} entry.channel        - Source channel (mattermost, slack, telegram, etc.)
 * @param {string} [entry.agent]        - Observing agent (default: AGENT_NAME)
 * @param {'low'|'medium'|'high'} [entry.confidence] - How confident the observation is
 * @param {string[]} [entry.tags]       - Relevant tags
 * @param {string} [entry.source_context] - Surrounding context (optional snippet)
 */
export function addIntent(entry) {
  const data = loadIntents();

  // Dedup: if a very similar intent already exists (same normalized intent text), update ts
  const normalized = (entry.intent || '').toLowerCase().trim();
  const existing = data.intents.find(i =>
    i.intent.toLowerCase().trim() === normalized && i.status !== 'dismissed'
  );

  if (existing) {
    existing.lastSeen = new Date().toISOString();
    existing.seenCount = (existing.seenCount || 1) + 1;
    existing.confidence = entry.confidence || existing.confidence;
    if (entry.source_context) existing.source_context = entry.source_context;
    saveIntents(data);
    return existing;
  }

  const intent = {
    id: `intent-${Date.now()}-${crypto.randomBytes(3).toString('hex')}`,
    ts: new Date().toISOString(),
    lastSeen: new Date().toISOString(),
    seenCount: 1,
    agent: entry.agent || AGENT_NAME,
    channel: entry.channel || 'unknown',
    confidence: entry.confidence || 'low',
    phrase: entry.phrase || '',
    intent: entry.intent || entry.phrase || '',
    tags: entry.tags || [],
    status: 'open',          // open | tasked | dismissed | completed
    workqueueId: null,        // set when promoted to a work item
    source_context: entry.source_context || null,
  };

  data.intents.push(intent);
  saveIntents(data);
  return intent;
}

// ── List intents ───────────────────────────────────────────────────────────────
export function listIntents(filter = {}) {
  const data = loadIntents();
  let items = data.intents || [];

  if (filter.status) items = items.filter(i => i.status === filter.status);
  if (filter.tags?.length) {
    items = items.filter(i => filter.tags.some(t => (i.tags || []).includes(t)));
  }
  if (filter.confidence) items = items.filter(i => i.confidence === filter.confidence);

  return items;
}

// ── Suggest intents not yet in workqueue ───────────────────────────────────────
export function suggestIntents() {
  const data = loadIntents();
  let queue;
  try {
    queue = JSON.parse(readFileSync(QUEUE_PATH, 'utf8'));
  } catch { queue = { items: [], completed: [] }; }

  const allIds = new Set([
    ...(queue.items || []).map(i => i.id),
    ...(queue.completed || []).map(i => i.id),
  ]);

  return (data.intents || []).filter(i =>
    i.status === 'open' &&
    (!i.workqueueId || !allIds.has(i.workqueueId))
  );
}

// ── Update intent status ───────────────────────────────────────────────────────
export function updateIntentStatus(intentId, status, workqueueId = null) {
  const data = loadIntents();
  const intent = data.intents.find(i => i.id === intentId);
  if (!intent) return null;

  intent.status = status;
  if (workqueueId) intent.workqueueId = workqueueId;
  intent.lastUpdated = new Date().toISOString();

  saveIntents(data);
  return intent;
}

// ── CLI ────────────────────────────────────────────────────────────────────────
if (process.argv[1]?.endsWith('intent-tracker.mjs')) {
  const args = process.argv.slice(2);
  const cmd  = args[0];

  if (cmd === '--list') {
    const filter = {};
    if (args.includes('--open'))    filter.status = 'open';
    if (args.includes('--tasked'))  filter.status = 'tasked';
    const intents = listIntents(filter);
    if (!intents.length) {
      console.log('No intents found.');
    } else {
      console.log(`operator intent log (${intents.length} entries):\n`);
      for (const i of intents) {
        const age = Math.round((Date.now() - new Date(i.ts).getTime()) / 3600000);
        console.log(`[${i.id}] (${i.confidence}/${i.status}) ${i.intent}`);
        console.log(`  → seen ${i.seenCount}x, first: ${i.ts.slice(0,10)}, ${age}h ago`);
        if (i.tags.length) console.log(`  → tags: ${i.tags.join(', ')}`);
        if (i.source_context) console.log(`  → ctx: "${i.source_context}"`);
        console.log('');
      }
    }

  } else if (cmd === '--add') {
    let entry;
    try { entry = JSON.parse(args[1]); } catch {
      console.error('Usage: --add \'{"phrase":"...","intent":"...","channel":"...",...}\'');
      process.exit(1);
    }
    const saved = addIntent(entry);
    console.log(`✅ Intent saved: ${saved.id}`);
    console.log(JSON.stringify(saved, null, 2));

  } else if (cmd === '--suggest') {
    const suggestions = suggestIntents();
    if (!suggestions.length) {
      console.log('No open intents to suggest.');
    } else {
      console.log(`💡 Open operator intents (not yet tasked):\n`);
      for (const i of suggestions) {
        console.log(`  • [${i.confidence}] ${i.intent}`);
        if (i.tags.length) console.log(`    tags: ${i.tags.join(', ')}`);
      }
    }

  } else if (cmd === '--status') {
    const intentId = args[1];
    const newStatus = args[2];
    const wqId = args[3] || null;
    if (!intentId || !newStatus) {
      console.error('Usage: --status <intent-id> <open|tasked|dismissed|completed> [wq-id]');
      process.exit(1);
    }
    const updated = updateIntentStatus(intentId, newStatus, wqId);
    if (!updated) {
      console.error(`Intent '${intentId}' not found.`);
      process.exit(1);
    }
    console.log(`✅ Intent ${intentId} status → ${newStatus}`);

  } else {
    // Default: show stats
    const data = loadIntents();
    const intents = data.intents || [];
    const open    = intents.filter(i => i.status === 'open').length;
    const tasked  = intents.filter(i => i.status === 'tasked').length;
    const done    = intents.filter(i => ['completed','dismissed'].includes(i.status)).length;
    console.log(`Operator Intent Tracker — ${intents.length} total intents`);
    console.log(`  open: ${open} | tasked: ${tasked} | resolved: ${done}`);
    console.log(`  last updated: ${data.lastUpdated || 'never'}`);
    console.log('');
    console.log('Commands: --list [--open|--tasked] | --add \'JSON\' | --suggest | --status <id> <status>');
  }
}
