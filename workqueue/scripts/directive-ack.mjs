#!/usr/bin/env node
/**
 * directive-ack.mjs — Directive acknowledgment tracking (wq-R-013)
 *
 * Directive items: assignee=all, source=jkh, with an "acks" dict.
 * An item is "fully_resolved" when all known agents have acked.
 *
 * Usage:
 *   node directive-ack.mjs --list              # show all directive items + ack status
 *   node directive-ack.mjs --ack <itemId>       # add this agent's ack to an item
 *   node directive-ack.mjs --check             # print any directives missing acks
 *   node directive-ack.mjs --inject-sync <json> # parse a sync message and apply acks
 */

import fs from 'fs';
import path from 'path';
import { fileURLToPath } from 'url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const QUEUE_PATH = path.join(__dirname, '..', 'queue.json');
const AGENT_NAME = process.env.AGENT_NAME || 'rocky';
const KNOWN_AGENTS = ['rocky', 'bullwinkle', 'natasha', 'boris'];

function loadQueue() {
  return JSON.parse(fs.readFileSync(QUEUE_PATH, 'utf8'));
}

function saveQueue(q) {
  fs.writeFileSync(QUEUE_PATH, JSON.stringify(q, null, 2));
}

function isDirective(item) {
  return item.source === 'jkh' && item.assignee === 'all';
}

function getDirectives(queue) {
  return [...queue.items, ...queue.completed].filter(isDirective);
}

function getMissingAcks(item) {
  const acks = item.acks || {};
  return KNOWN_AGENTS.filter(a => !acks[a]);
}

function isFullyResolved(item) {
  return getMissingAcks(item).length === 0;
}

function addAck(queue, itemId, agentName, ts) {
  ts = ts || new Date().toISOString();
  let found = false;

  for (const arr of [queue.items, queue.completed]) {
    const item = arr.find(i => i.id === itemId);
    if (item) {
      if (!item.acks) item.acks = {};
      if (!item.acks[agentName]) {
        item.acks[agentName] = ts;
        item.itemVersion = (item.itemVersion || 1) + 1;
        item.notes = (item.notes || '') +
          `\n${agentName}: acked directive ${itemId} at ${ts}.`;
        // Mark fully_resolved if all agents have acked
        if (isFullyResolved(item)) {
          item.fullyResolved = true;
          item.fullyResolvedAt = ts;
          item.notes += ` [fully_resolved: all agents acked]`;
        }
        found = true;
      }
      break;
    }
  }
  return found;
}

// Parse acks from a sync message body and apply them
function injectFromSync(queue, syncPayload) {
  const incoming = typeof syncPayload === 'string'
    ? JSON.parse(syncPayload)
    : syncPayload;

  const allIncoming = [...(incoming.items || []), ...(incoming.completed || [])];
  let applied = 0;

  for (const inItem of allIncoming) {
    if (!inItem.acks) continue;
    for (const arr of [queue.items, queue.completed]) {
      const local = arr.find(i => i.id === inItem.id);
      if (local) {
        let changed = false;
        if (!local.acks) local.acks = {};
        for (const [agent, ts] of Object.entries(inItem.acks)) {
          if (!local.acks[agent]) {
            local.acks[agent] = ts;
            changed = true;
            applied++;
          }
        }
        if (changed) {
          local.itemVersion = (local.itemVersion || 1) + 1;
          if (isFullyResolved(local) && !local.fullyResolved) {
            local.fullyResolved = true;
            local.fullyResolvedAt = new Date().toISOString();
            local.notes = (local.notes || '') +
              `\n[fully_resolved: all agents acked at ${local.fullyResolvedAt}]`;
          }
        }
      }
    }
  }
  return applied;
}

const args = process.argv.slice(2);
const cmd = args[0];

if (!cmd || cmd === '--list') {
  const q = loadQueue();
  const directives = getDirectives(q);
  if (!directives.length) {
    console.log('No directive items found.');
  } else {
    for (const d of directives) {
      const acks = d.acks || {};
      const missing = getMissingAcks(d);
      const resolved = isFullyResolved(d);
      console.log(`\n${d.id} — ${d.title}`);
      console.log(`  Status: ${d.status} | fullyResolved: ${resolved ? '✅' : '❌'}`);
      console.log(`  Acks: ${JSON.stringify(acks)}`);
      if (missing.length) console.log(`  Missing: ${missing.join(', ')}`);
    }
  }

} else if (cmd === '--ack') {
  const itemId = args[1];
  if (!itemId) { console.error('Usage: --ack <itemId>'); process.exit(1); }
  const q = loadQueue();
  const ok = addAck(q, itemId, AGENT_NAME, new Date().toISOString());
  if (ok) {
    saveQueue(q);
    console.log(`✅ Acked ${itemId} as ${AGENT_NAME}`);
  } else {
    console.log(`ℹ️  Item ${itemId} not found or already acked by ${AGENT_NAME}`);
  }

} else if (cmd === '--check') {
  const q = loadQueue();
  const directives = getDirectives(q);
  const unresolved = directives.filter(d => !isFullyResolved(d));
  if (!unresolved.length) {
    console.log('✅ All directive items fully resolved.');
  } else {
    console.log(`⚠️  ${unresolved.length} directive(s) missing acks:`);
    for (const d of unresolved) {
      console.log(`  ${d.id} — ${d.title} | missing: ${getMissingAcks(d).join(', ')}`);
    }
  }

} else if (cmd === '--inject-sync') {
  const payload = args[1];
  if (!payload) { console.error('Usage: --inject-sync <json>'); process.exit(1); }
  const q = loadQueue();
  const applied = injectFromSync(q, payload);
  saveQueue(q);
  console.log(`Applied ${applied} ack(s) from sync payload.`);

} else {
  console.error(`Unknown command: ${cmd}`);
  process.exit(1);
}

// Export for use by other scripts
export { addAck, injectFromSync, getMissingAcks, isFullyResolved, getDirectives };
