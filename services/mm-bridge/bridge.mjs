#!/usr/bin/env node
/**
 * mm-bridge — Mattermost ↔ SquirrelChat bidirectional message bridge
 *
 * Direction A: SquirrelChat #general → Mattermost #agent-shared
 *   Consumes SquirrelChat SSE stream (/api/stream), posts to Mattermost
 *   incoming webhook.
 *
 * Direction B: Mattermost #agent-shared → SquirrelChat #general
 *   Polls Mattermost posts on a configurable interval and relays new
 *   messages to SquirrelChat via its REST API.
 *
 * Environment variables (all required unless noted):
 *   SC_BASE_URL        SquirrelChat base URL (e.g. http://146.190.134.110:8793)
 *   SC_BOT_TOKEN       SquirrelChat bot auth token
 *   SC_CHANNEL         SquirrelChat channel to bridge (default: general)
 *   SC_BOT_NAME        Display name for messages relayed from Mattermost (default: mattermost-bridge)
 *   MM_BASE_URL        Mattermost server base URL (e.g. https://mm.jordanhubbard.net)
 *   MM_BOT_TOKEN       Mattermost bot account token
 *   MM_CHANNEL_ID      Mattermost channel ID for #agent-shared
 *   MM_WEBHOOK_URL     Mattermost incoming webhook URL (for SC→MM direction)
 *   MM_POLL_INTERVAL   Milliseconds between MM polls for MM→SC direction (default: 5000)
 *   BRIDGE_LOG_LEVEL   'debug' | 'info' (default: info)
 */

import { EventSource } from 'eventsource';
import { setTimeout as sleep } from 'node:timers/promises';

/* ── Config ─────────────────────────────────────────────────────────────── */

const cfg = {
  sc: {
    baseUrl:   env('SC_BASE_URL',  'http://146.190.134.110:8793'),
    token:     env('SC_BOT_TOKEN', ''),
    channel:   env('SC_CHANNEL',   'general'),
    botName:   env('SC_BOT_NAME',  'mattermost-bridge'),
  },
  mm: {
    baseUrl:    env('MM_BASE_URL',    ''),
    botToken:   env('MM_BOT_TOKEN',   ''),
    channelId:  env('MM_CHANNEL_ID',  ''),
    webhookUrl: env('MM_WEBHOOK_URL', ''),
    pollMs:     parseInt(env('MM_POLL_INTERVAL', '5000'), 10),
  },
  debug: env('BRIDGE_LOG_LEVEL', 'info') === 'debug',
};

function env(key, def) {
  return process.env[key] ?? def;
}

function log(level, ...args) {
  if (level === 'debug' && !cfg.debug) return;
  console.log(`[mm-bridge] [${level.toUpperCase()}]`, ...args);
}

function die(msg) {
  console.error(`[mm-bridge] FATAL: ${msg}`);
  process.exit(1);
}

/* ── Validation ─────────────────────────────────────────────────────────── */

function validate() {
  const missing = [];
  if (!cfg.sc.token)      missing.push('SC_BOT_TOKEN');
  if (!cfg.mm.baseUrl)    missing.push('MM_BASE_URL');
  if (!cfg.mm.botToken)   missing.push('MM_BOT_TOKEN');
  if (!cfg.mm.channelId)  missing.push('MM_CHANNEL_ID');
  if (!cfg.mm.webhookUrl) missing.push('MM_WEBHOOK_URL');
  if (missing.length) die(`Missing required env vars: ${missing.join(', ')}`);
}

/* ── Dedup: track message IDs already relayed to avoid loops ────────────── */

const relayedSC  = new Set(); // SC message IDs we relayed to MM
const relayedMM  = new Set(); // MM post IDs we relayed to SC
const MAX_DEDUP  = 2000;

function markSC(id)  { relayedSC.add(id);  if (relayedSC.size  > MAX_DEDUP) relayedSC.delete(relayedSC.values().next().value); }
function markMM(id)  { relayedMM.add(id);  if (relayedMM.size  > MAX_DEDUP) relayedMM.delete(relayedMM.values().next().value); }
function seenSC(id)  { return relayedSC.has(id); }
function seenMM(id)  { return relayedMM.has(id); }

/* ── SquirrelChat helpers ────────────────────────────────────────────────── */

async function scPost(text) {
  const url = `${cfg.sc.baseUrl}/api/messages`;
  const body = JSON.stringify({
    text,
    channel: cfg.sc.channel,
    from: cfg.sc.botName,
  });
  const res = await fetch(url, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      'Authorization': `Bearer ${cfg.sc.token}`,
    },
    body,
  });
  if (!res.ok) {
    const txt = await res.text().catch(() => '');
    log('debug', `SC post failed ${res.status}: ${txt}`);
    throw new Error(`SC post ${res.status}`);
  }
  const data = await res.json().catch(() => ({}));
  return data;
}

/* ── Direction A: SquirrelChat → Mattermost ─────────────────────────────── */

function startScToMm() {
  const streamUrl = `${cfg.sc.baseUrl}/api/stream?channel=${encodeURIComponent(cfg.sc.channel)}&token=${encodeURIComponent(cfg.sc.token)}`;
  log('info', `[SC→MM] Connecting to SquirrelChat SSE stream: ${streamUrl.replace(cfg.sc.token, '***')}`);

  const es = new EventSource(streamUrl);

  es.addEventListener('message', async (event) => {
    let msg;
    try { msg = JSON.parse(event.data); } catch { return; }

    // Only relay chat messages (type=message or no type), skip system/join events
    if (msg.type && msg.type !== 'message') return;
    // Skip messages sent by our own bridge bot to avoid echo
    if (msg.from === cfg.sc.botName || msg.username === cfg.sc.botName) return;
    // Skip if already relayed
    const msgId = msg.id ?? msg.ts ?? JSON.stringify(msg);
    if (seenSC(msgId)) return;
    markSC(msgId);

    const from = msg.from || msg.username || 'unknown';
    const text = msg.text || msg.content || '';
    if (!text.trim()) return;

    const mmText = `**[SquirrelChat/${cfg.sc.channel}] ${from}:** ${text}`;
    log('debug', `[SC→MM] Relaying: ${mmText.slice(0, 80)}`);

    try {
      await mmWebhookPost(mmText);
      log('info', `[SC→MM] Relayed message from ${from}`);
    } catch (err) {
      log('debug', `[SC→MM] Relay failed: ${err.message}`);
    }
  });

  es.addEventListener('error', (err) => {
    log('debug', `[SC→MM] SSE error, will reconnect: ${err.message ?? err}`);
    // EventSource auto-reconnects
  });

  es.addEventListener('open', () => {
    log('info', '[SC→MM] SSE stream connected');
  });
}

async function mmWebhookPost(text) {
  const res = await fetch(cfg.mm.webhookUrl, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ text }),
  });
  if (!res.ok) {
    const txt = await res.text().catch(() => '');
    throw new Error(`MM webhook ${res.status}: ${txt}`);
  }
}

/* ── Direction B: Mattermost → SquirrelChat ─────────────────────────────── */

let mmSinceTs = Date.now(); // only relay MM posts created after bridge start

async function pollMmToSc() {
  const url = `${cfg.mm.baseUrl}/api/v4/channels/${cfg.mm.channelId}/posts?since=${mmSinceTs}`;
  let data;
  try {
    const res = await fetch(url, {
      headers: { Authorization: `Bearer ${cfg.mm.botToken}` },
    });
    if (!res.ok) {
      log('debug', `[MM→SC] Poll failed: ${res.status}`);
      return;
    }
    data = await res.json();
  } catch (err) {
    log('debug', `[MM→SC] Poll error: ${err.message}`);
    return;
  }

  const order = data?.order ?? [];
  const posts  = data?.posts ?? {};

  for (const postId of order) {
    const post = posts[postId];
    if (!post) continue;

    // Update high-water mark
    if (post.create_at > mmSinceTs) mmSinceTs = post.create_at + 1;

    // Skip echo: posts created by our bot token
    if (post.user_id && post.user_id === cfg.mm.botUserId) continue;
    // Skip if already relayed
    if (seenMM(postId)) continue;
    markMM(postId);

    const text = post.message || '';
    if (!text.trim()) continue;

    // Resolve username (best-effort, may be a display name)
    const username = post.props?.override_username ?? post.user_id ?? 'mm-user';
    const scText = `[Mattermost/${cfg.mm.channelName ?? 'agent-shared'}] ${username}: ${text}`;
    log('debug', `[MM→SC] Relaying: ${scText.slice(0, 80)}`);

    try {
      const result = await scPost(scText);
      // Track the SC message ID so SC→MM direction won't echo it back
      if (result?.id) markSC(result.id);
      if (result?.ts) markSC(result.ts);
      log('info', `[MM→SC] Relayed post ${postId} from ${username}`);
    } catch (err) {
      log('debug', `[MM→SC] SC post failed: ${err.message}`);
    }
  }
}

async function startMmToSc() {
  // Resolve our bot's user ID once so we can filter self-posts
  try {
    const res = await fetch(`${cfg.mm.baseUrl}/api/v4/users/me`, {
      headers: { Authorization: `Bearer ${cfg.mm.botToken}` },
    });
    if (res.ok) {
      const me = await res.json();
      cfg.mm.botUserId = me.id;
      log('info', `[MM→SC] Bot user ID: ${me.id} (${me.username})`);
    }
  } catch { /* non-fatal */ }

  // Resolve channel name for display
  try {
    const res = await fetch(`${cfg.mm.baseUrl}/api/v4/channels/${cfg.mm.channelId}`, {
      headers: { Authorization: `Bearer ${cfg.mm.botToken}` },
    });
    if (res.ok) {
      const ch = await res.json();
      cfg.mm.channelName = ch.display_name || ch.name;
    }
  } catch { /* non-fatal */ }

  log('info', `[MM→SC] Starting poll every ${cfg.mm.pollMs}ms`);
  // eslint-disable-next-line no-constant-condition
  while (true) {
    await pollMmToSc();
    await sleep(cfg.mm.pollMs);
  }
}

/* ── Main ─────────────────────────────────────────────────────────────────── */

validate();
log('info', 'mm-bridge starting');
log('info', `  SC: ${cfg.sc.baseUrl} channel=#${cfg.sc.channel}`);
log('info', `  MM: ${cfg.mm.baseUrl} channel=${cfg.mm.channelId}`);

startScToMm();
startMmToSc().catch((err) => {
  log('debug', `[MM→SC] Fatal error: ${err.message}`);
  process.exit(1);
});

process.on('SIGINT',  () => { log('info', 'Shutting down'); process.exit(0); });
process.on('SIGTERM', () => { log('info', 'Shutting down'); process.exit(0); });
