#!/usr/bin/env node
/**
 * rcc/scripts/incubator-digest.mjs — Weekly incubator idea digest
 *
 * Queries all incubating queue items, sorts by vote count descending,
 * takes top-N, and sends a Mattermost DM to jkh.
 *
 * Designed to run via cron: every Monday at 09:00 PT.
 * Cron: 0 16 * * 1  (16:00 UTC = 09:00 PT, Mondays)
 *
 * Usage:
 *   node rcc/scripts/incubator-digest.mjs
 *
 * Env vars:
 *   RCC_API             default: https://api.yourmom.photos
 *   CCC_AUTH_TOKEN      default: rcc-agent-natasha-eeynvasslp8mna9bipx
 *   MM_URL              default: https://chat.yourmom.photos
 *   MM_TOKEN            required (Mattermost bot token)
 *   JKH_MM_USERNAME     default: jkh
 *   TOP_N               default: 5
 *   DRY_RUN             set to "1" to print instead of posting
 */

const RCC_API         = process.env.RCC_API         || 'https://api.yourmom.photos';
const RCC_AUTH        = process.env.CCC_AUTH_TOKEN   || 'rcc-agent-natasha-eeynvasslp8mna9bipx';
const MM_URL          = process.env.MM_URL           || 'https://chat.yourmom.photos';
const MM_TOKEN        = process.env.MM_TOKEN         || process.env.MATTERMOST_TOKEN || '';
const JKH_USERNAME    = process.env.JKH_MM_USERNAME  || 'jkh';
const TOP_N           = parseInt(process.env.TOP_N   || '5', 10);
const DRY_RUN         = process.env.DRY_RUN === '1';

function log(msg) { console.log(`[incubator-digest ${new Date().toISOString()}] ${msg}`); }

async function getQueue() {
  const res = await fetch(`${RCC_API}/api/queue`, {
    headers: { Authorization: `Bearer ${RCC_AUTH}` },
  });
  if (!res.ok) throw new Error(`Queue fetch failed: HTTP ${res.status}`);
  const data = await res.json();
  return data.items || [];
}

async function mmDm(message) {
  if (!MM_TOKEN) throw new Error('MM_TOKEN not set');

  // Get bot identity
  const meRes = await fetch(`${MM_URL}/api/v4/users/me`, {
    headers: { Authorization: `Bearer ${MM_TOKEN}` },
  });
  if (!meRes.ok) throw new Error(`MM /users/me failed: ${meRes.status}`);
  const me = await meRes.json();

  // Resolve jkh user id
  const jkhRes = await fetch(`${MM_URL}/api/v4/users/username/${JKH_USERNAME}`, {
    headers: { Authorization: `Bearer ${MM_TOKEN}` },
  });
  if (!jkhRes.ok) throw new Error(`MM user lookup failed: ${jkhRes.status}`);
  const jkh = await jkhRes.json();

  // Open DM channel
  const dmRes = await fetch(`${MM_URL}/api/v4/channels/direct`, {
    method: 'POST',
    headers: { Authorization: `Bearer ${MM_TOKEN}`, 'Content-Type': 'application/json' },
    body: JSON.stringify([me.id, jkh.id]),
  });
  if (!dmRes.ok) throw new Error(`MM DM channel failed: ${dmRes.status}`);
  const dm = await dmRes.json();

  // Post
  const postRes = await fetch(`${MM_URL}/api/v4/posts`, {
    method: 'POST',
    headers: { Authorization: `Bearer ${MM_TOKEN}`, 'Content-Type': 'application/json' },
    body: JSON.stringify({ channel_id: dm.id, message }),
  });
  if (!postRes.ok) throw new Error(`MM post failed: ${postRes.status}`);
  log(`DM sent to @${JKH_USERNAME}`);
}

async function main() {
  log('Fetching queue...');
  const items = await getQueue();

  // Filter incubating items
  const incubating = items.filter(i => i.status === 'incubating');
  log(`Found ${incubating.length} incubating items`);

  if (!incubating.length) {
    log('Nothing incubating — skipping digest.');
    return;
  }

  // Sort by vote count desc, then by created asc (older first as tiebreak)
  incubating.sort((a, b) => {
    const va = (a.votes || []).length;
    const vb = (b.votes || []).length;
    if (vb !== va) return vb - va;
    return new Date(a.created) - new Date(b.created);
  });

  const top = incubating.slice(0, TOP_N);

  // Format message
  const lines = [
    `### 💡 Weekly Incubator Digest — ${new Date().toLocaleDateString('en-US', { weekday: 'long', month: 'long', day: 'numeric' })}`,
    `${incubating.length} ideas incubating. Top ${top.length} by votes:\n`,
  ];

  top.forEach((item, idx) => {
    const votes  = (item.votes || []).length;
    const age    = Math.floor((Date.now() - new Date(item.created)) / 86400000);
    const source = item.source || '?';
    const desc   = (item.description || '').slice(0, 120).replace(/\n/g, ' ');
    lines.push(
      `**${idx + 1}. ${item.title}**\n` +
      `\`${item.id}\` · ${votes} vote${votes !== 1 ? 's' : ''} · ${age}d old · by ${source}\n` +
      (desc ? `> ${desc}${item.description?.length > 120 ? '…' : ''}\n` : '')
    );
  });

  lines.push(`\nTo promote an idea: set status to \`pending\` in the dashboard or ping me.`);

  const message = lines.join('\n');

  if (DRY_RUN) {
    log('DRY_RUN — message:\n' + message);
    return;
  }

  await mmDm(message);
  log('Done.');
}

main().catch(e => { log(`ERROR: ${e.message}`); process.exit(1); });
