#!/usr/bin/env node
/**
 * stale-assignee-nudge.mjs
 * Check queue for items pending >48h with no claim and post a nudge to
 * ClawBus #ops channel so all agents see it.
 *
 * Formerly sent Mattermost DMs; now uses ClawBus /bus/send.
 * wq-R-006 — updated 2026-04-10
 */

const CCC_URL   = process.env.CCC_URL   || 'http://localhost:8789';
const CCC_TOKEN = process.env.CCC_AGENT_TOKEN || process.env.CCC_AUTH_TOKENS?.split(',')[0] || '';
const CALLING_AGENT = process.env.AGENT_NAME || 'natasha';

const STALE_THRESHOLD_MS = 48 * 60 * 60 * 1000; // 48 hours

async function postToClawBus(body) {
  const res = await fetch(`${CCC_URL}/bus/send`, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      Authorization: `Bearer ${CCC_TOKEN}`,
    },
    body: JSON.stringify({
      from: CALLING_AGENT,
      to: 'all',
      type: 'text',
      subject: 'ops',
      body,
      mime: 'text/plain',
    }),
  });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(`ClawBus post failed: ${res.status} ${text}`);
  }
  return res.json();
}

function formatAge(ms) {
  const h = Math.floor(ms / (1000 * 60 * 60));
  const d = Math.floor(h / 24);
  if (d > 0) return `${d}d ${h % 24}h`;
  return `${h}h`;
}

async function main() {
  if (!CCC_TOKEN) {
    console.error('[stale-nudge] CCC_AGENT_TOKEN not set — cannot fetch queue.');
    process.exit(1);
  }

  const queueRes = await fetch(`${CCC_URL}/api/queue`, {
    headers: { Authorization: `Bearer ${CCC_TOKEN}` },
  });
  if (!queueRes.ok) throw new Error(`CCC queue fetch failed: ${queueRes.status}`);
  const queue = await queueRes.json();
  const now = Date.now();
  const items = Array.isArray(queue) ? queue : (queue.items || []);

  // Find stale items: pending, specific assignee, unclaimed, >48h old
  const stale = items.filter((item) => {
    if (item.status !== 'pending') return false;
    if (!item.assignee || item.assignee === 'all') return false;
    if (item.claimedBy) return false;
    const age = now - new Date(item.created).getTime();
    return age > STALE_THRESHOLD_MS;
  });

  if (stale.length === 0) {
    console.log('[stale-nudge] No stale items found. All clear.');
    return;
  }

  // Group by assignee
  const byAssignee = {};
  for (const item of stale) {
    const a = item.assignee;
    if (!byAssignee[a]) byAssignee[a] = [];
    byAssignee[a].push(item);
  }

  // Post a nudge per assignee to #ops
  for (const [agent, agentItems] of Object.entries(byAssignee)) {
    if (agent === CALLING_AGENT) {
      console.log(`[stale-nudge] Self-skip: ${agentItems.length} items assigned to ${agent}`);
      continue;
    }

    const itemLines = agentItems
      .map((i) => {
        const age = formatAge(now - new Date(i.created).getTime());
        return `  • ${i.id} [${i.priority}] ${i.title} — unclaimed for ${age}`;
      })
      .join('\n');

    const msg =
      `[stale-nudge] Hey ${agent} — ${agentItems.length} item(s) assigned to you ` +
      `have been unclaimed for 48h+:\n\n${itemLines}\n\n` +
      `Still on your radar? If blocked or reassigning, drop a note in the item. (from ${CALLING_AGENT})`;

    try {
      await postToClawBus(msg);
      console.log(`[stale-nudge] Posted nudge for ${agent} (${agentItems.length} item(s)) to #ops`);
    } catch (err) {
      console.error(`[stale-nudge] Failed to post nudge for ${agent}:`, err.message);
    }
  }
}

main().catch((err) => {
  console.error('[stale-nudge] Fatal:', err);
  process.exit(1);
});
