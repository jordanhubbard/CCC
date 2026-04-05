#!/usr/bin/env node
/**
 * stale-assignee-nudge.mjs
 * Check queue for items pending >48h with no claim and send Mattermost DM nudge.
 * wq-R-006 — implemented by Natasha 2026-03-21
 */

// Uses CCC API as canonical queue source (not local queue.json)
const CCC_URL   = process.env.CCC_URL   || 'http://localhost:8789';
const RCC_TOKEN = process.env.CCC_AGENT_TOKEN || process.env.CCC_AUTH_TOKENS?.split(',')[0] || '';

// Mattermost config
const MM_URL = 'https://chat.yourmom.photos';
const MM_TOKEN = process.env.MM_TOKEN || '';

// Agent → Mattermost user IDs
const AGENT_MM_IDS = {
  rocky: 'x5i7bek3r7gfbkcpxsiaw35muh',
  bullwinkle: 'ww1wef9sktf8jg8be6q5zj1aye',
  natasha: 'k8qtua6dbjfmfjk76o9bgaepua',
};

// Agent → Mattermost DM channel IDs (known direct channels)
const AGENT_DM_CHANNELS = {
  rocky: '36ir68o4itbpf8n6rfwn36zcyh',
  bullwinkle: 'd3kk39q4tbrnxbuzty94ponanc',
};

const STALE_THRESHOLD_MS = 48 * 60 * 60 * 1000; // 48 hours
const CALLING_AGENT = process.env.AGENT_NAME || 'natasha';

async function sendMattermostDM(channelId, message) {
  if (!MM_TOKEN) {
    console.log(`[DRY RUN] Would send to channel ${channelId}: ${message}`);
    return { ok: true, dry: true };
  }
  const res = await fetch(`${MM_URL}/api/v4/posts`, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      Authorization: `Bearer ${MM_TOKEN}`,
    },
    body: JSON.stringify({ channel_id: channelId, message }),
  });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(`Mattermost post failed: ${res.status} ${text}`);
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
  // Fetch queue from CCC API (canonical source)
  if (!RCC_TOKEN) {
    console.error('[stale-nudge] CCC_AGENT_TOKEN not set — cannot fetch queue.');
    process.exit(1);
  }
  const queueRes = await fetch(`${CCC_URL}/api/queue`, {
    headers: { Authorization: `Bearer ${RCC_TOKEN}` },
  });
  if (!queueRes.ok) throw new Error(`CCC queue fetch failed: ${queueRes.status}`);
  const queue = await queueRes.json();
  const now = Date.now();
  const items = Array.isArray(queue) ? queue : (queue.items || []);

  // Find stale items: pending, specific assignee (not "all"), no claim, >48h old
  const stale = items.filter((item) => {
    if (item.status !== 'pending') return false;
    if (!item.assignee || item.assignee === 'all') return false;
    if (item.claimedBy) return false;
    const created = new Date(item.created).getTime();
    const age = now - created;
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

  // Send nudge per assignee
  for (const [agent, agentItems] of Object.entries(byAssignee)) {
    // Skip nudging self
    if (agent === CALLING_AGENT) {
      console.log(`[stale-nudge] Self-skip: ${agentItems.length} items assigned to ${agent}`);
      continue;
    }

    const channelId = AGENT_DM_CHANNELS[agent];
    if (!channelId) {
      console.warn(`[stale-nudge] No DM channel known for agent: ${agent}. Skipping.`);
      continue;
    }

    const itemLines = agentItems
      .map((i) => {
        const age = formatAge(now - new Date(i.created).getTime());
        return `  • **${i.id}** [${i.priority}] ${i.title} — unclaimed for **${age}**`;
      })
      .join('\n');

    const msg =
      `👋 Hey ${agent} — just a gentle nudge from ${CALLING_AGENT}.\n\n` +
      `The following item${agentItems.length > 1 ? 's are' : ' is'} assigned to you and ` +
      `${agentItems.length > 1 ? 'have' : 'has'} been sitting unclaimed for 48h+:\n\n` +
      itemLines +
      `\n\nStill on your radar? If blocked or reassigning, drop a note in the item. No rush — just don't want these to get lost! 🕵️‍♀️`;

    try {
      const result = await sendMattermostDM(channelId, msg);
      if (result.dry) {
        console.log(`[stale-nudge] DRY RUN nudge to ${agent}: ${agentItems.length} item(s)`);
      } else {
        console.log(`[stale-nudge] Nudged ${agent} about ${agentItems.length} stale item(s).`);
      }
    } catch (err) {
      console.error(`[stale-nudge] Failed to nudge ${agent}:`, err.message);
    }
  }
}

main().catch((err) => {
  console.error('[stale-nudge] Fatal:', err);
  process.exit(1);
});
