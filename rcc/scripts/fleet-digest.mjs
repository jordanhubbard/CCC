#!/usr/bin/env node
/**
 * rcc/scripts/fleet-digest.mjs — Weekly fleet health digest
 *
 * Sends a Slack DM to jkh every Monday at 09:00 PT with:
 *   1. sparky GB10 avg temp/power/util from gpu-metrics.jsonl (last 7 days)
 *   2. Fleet heartbeat uptime per agent (from /api/agents)
 *   3. Queue stats (completed/filed/archived this week)
 *   4. Current vLLM model on Sweden fleet
 *
 * Usage:
 *   node rcc/scripts/fleet-digest.mjs
 *
 * Cron: 0 16 * * 1  (16:00 UTC = 09:00 PT, Mondays)
 *
 * Env vars:
 *   RCC_API             default: https://api.yourmom.photos
 *   CCC_AUTH_TOKEN      default: rcc-agent-natasha-eeynvasslp8mna9bipx
 *   SLACK_BOT_TOKEN     required (Slack bot token)
 *   JKH_SLACK_USER      default: UDYR7H4SC
 *   GPU_METRICS_FILE    default: ~/.openclaw/workspace/telemetry/gpu-metrics.jsonl
 *   DRY_RUN             set to "1" to print instead of posting
 */

import { readFileSync, existsSync } from 'fs';
import { homedir } from 'os';
import { resolve } from 'path';
import { createReadStream } from 'fs';
import { createInterface } from 'readline';

const RCC_API     = process.env.RCC_API         || 'https://api.yourmom.photos';
const RCC_AUTH    = process.env.CCC_AUTH_TOKEN   || 'rcc-agent-natasha-eeynvasslp8mna9bipx';
const SLACK_TOKEN = process.env.SLACK_BOT_TOKEN  || '';
const JKH_USER    = process.env.JKH_SLACK_USER   || 'UDYR7H4SC';
const DRY_RUN     = process.env.DRY_RUN === '1';
const GPU_FILE    = process.env.GPU_METRICS_FILE  ||
                    resolve(homedir(), '.openclaw/workspace/telemetry/gpu-metrics.jsonl');

// ── helpers ──────────────────────────────────────────────────────────────────

async function fetchRCC(path) {
  const res = await fetch(`${RCC_API}${path}`, {
    headers: { Authorization: `Bearer ${RCC_AUTH}` },
  });
  if (!res.ok) throw new Error(`CCC ${path} → ${res.status}`);
  return res.json();
}

async function readJsonlLast7Days(filePath) {
  if (!existsSync(filePath)) return [];
  const cutoff = Date.now() - 7 * 24 * 60 * 60 * 1000;
  const lines = [];
  const rl = createInterface({ input: createReadStream(filePath), crlfDelay: Infinity });
  for await (const line of rl) {
    try {
      const obj = JSON.parse(line);
      if (new Date(obj.ts).getTime() >= cutoff) lines.push(obj);
    } catch {}
  }
  return lines;
}

function avg(arr, key) {
  const vals = arr.map(r => r[key]).filter(v => typeof v === 'number' && !isNaN(v));
  if (!vals.length) return null;
  return vals.reduce((a, b) => a + b, 0) / vals.length;
}

function fmt(n, digits = 1) {
  return n == null ? 'N/A' : n.toFixed(digits);
}

function timeSince(isoStr) {
  if (!isoStr) return 'never';
  const diff = Date.now() - new Date(isoStr).getTime();
  const mins = Math.round(diff / 60000);
  if (mins < 60) return `${mins}m ago`;
  const hrs = Math.round(diff / 3600000);
  if (hrs < 24) return `${hrs}h ago`;
  return `${Math.round(hrs / 24)}d ago`;
}

// ── sections ─────────────────────────────────────────────────────────────────

async function sparkySection() {
  const rows = await readJsonlLast7Days(GPU_FILE);
  if (!rows.length) return '🖥️ *sparky GB10* — no telemetry data yet';

  const avgTemp   = avg(rows, 'temp_c');
  const avgPower  = avg(rows, 'power_w');
  const avgUtil   = avg(rows, 'util_pct');
  const avgRam    = avg(rows, 'ram_used_mb');
  const lastRow   = rows[rows.length - 1];
  const ramTotal  = lastRow?.ram_used_mb != null && lastRow?.ram_avail_mb != null
    ? (lastRow.ram_used_mb + lastRow.ram_avail_mb)
    : null;

  const ramPct = ramTotal ? Math.round((avg(rows, 'ram_used_mb') / ramTotal) * 100) : null;

  return [
    `🖥️ *sparky GB10 — last 7 days avg*`,
    `• Temp: ${fmt(avgTemp)}°C  Power: ${fmt(avgPower)}W  Util: ${fmt(avgUtil)}%`,
    `• RAM: ${fmt(avgRam != null ? avgRam / 1024 : null)}GB used${ramPct != null ? ` (${ramPct}%)` : ''}`,
    `• Samples: ${rows.length} (every 15min)`,
  ].join('\n');
}

async function agentsSection() {
  let agents;
  try {
    agents = await fetchRCC('/api/agents');
  } catch (e) {
    return `🤖 *Fleet agents* — failed to fetch: ${e.message}`;
  }

  const SWEDEN = ['boris', 'peabody', 'sherman', 'snidely', 'dudley'];
  const lines = ['🤖 *Fleet heartbeat status*'];

  const agentMap = {};
  const agentArr0 = agents?.agents || (Array.isArray(agents) ? agents : Object.values(agents));
  for (const a of agentArr0) {
    agentMap[a.name || a.id || '?'] = a;
  }

  const allNames = Object.keys(agentMap).sort();
  for (const name of allNames) {
    const a = agentMap[name];
    const lastHB = a.lastHeartbeat || a.last_heartbeat || a.lastSeen;
    const diff = lastHB ? Date.now() - new Date(lastHB).getTime() : null;
    const status = diff == null ? '❓ unknown'
      : diff < 5 * 60 * 1000 ? '✅ live'
      : diff < 30 * 60 * 1000 ? '⚠️ recent'
      : '🔴 stale';
    lines.push(`• ${name}: ${status} (${timeSince(lastHB)})`);
  }

  if (!allNames.length) lines.push('• No agents in registry');
  return lines.join('\n');
}

async function queueSection() {
  let all;
  try {
    all = await fetchRCC('/api/queue');
  } catch (e) {
    return `📋 *Queue* — failed to fetch: ${e.message}`;
  }

  const items = Array.isArray(all) ? all : (all.items || all.completed || []);
  const cutoff = Date.now() - 7 * 24 * 60 * 60 * 1000;

  const thisWeek = items.filter(i => {
    const ts = i.completedAt || i.created;
    return ts && new Date(ts).getTime() >= cutoff;
  });

  const completed = thisWeek.filter(i => i.status === 'completed').length;
  const filed     = items.filter(i => new Date(i.created).getTime() >= cutoff).length;
  const pending   = items.filter(i => ['pending', 'in-progress', 'in_progress'].includes(i.status)).length;

  return [
    `📋 *Queue this week*`,
    `• Completed: ${completed}  Filed: ${filed}  Active: ${pending}`,
  ].join('\n');
}

async function swedenModelSection() {
  let agents;
  try {
    agents = await fetchRCC('/api/agents');
  } catch (e) {
    return `🇸🇪 *Sweden vLLM fleet* — failed to fetch agents`;
  }

  const SWEDEN = ['boris', 'peabody', 'sherman', 'snidely', 'dudley'];
  const agentArr = agents?.agents || (Array.isArray(agents) ? agents : Object.values(agents));
  const swedenAgents = agentArr.filter(a => SWEDEN.includes((a.name || a.id || '').toLowerCase()));

  if (!swedenAgents.length) return `🇸🇪 *Sweden vLLM fleet* — no agents found`;

  const models = new Set();
  for (const a of swedenAgents) {
    const m = a.vllm_model || a.model || a.capabilities?.model;
    if (m) models.add(m);
  }

  const modelStr = models.size ? [...models].join(', ') : 'unknown';
  return `🇸🇪 *Sweden vLLM fleet* (${swedenAgents.length} nodes) — model: \`${modelStr}\``;
}

// ── main ──────────────────────────────────────────────────────────────────────

async function buildDigest() {
  const [sparky, agents, queue, sweden] = await Promise.allSettled([
    sparkySection(),
    agentsSection(),
    queueSection(),
    swedenModelSection(),
  ]);

  const parts = [sparky, agents, queue, sweden].map(r =>
    r.status === 'fulfilled' ? r.value : `⚠️ Section failed: ${r.reason}`
  );

  const header = `*📊 Weekly Fleet Digest* — ${new Date().toLocaleDateString('en-US', {
    weekday: 'long', month: 'short', day: 'numeric', year: 'numeric',
    timeZone: 'America/Los_Angeles',
  })}`;

  return [header, '', ...parts].join('\n\n');
}

async function sendSlackDM(user, text) {
  if (!SLACK_TOKEN) throw new Error('SLACK_BOT_TOKEN not set');

  // Open DM channel
  const openRes = await fetch('https://slack.com/api/conversations.open', {
    method: 'POST',
    headers: {
      Authorization: `Bearer ${SLACK_TOKEN}`,
      'Content-Type': 'application/json',
    },
    body: JSON.stringify({ users: user }),
  });
  const opened = await openRes.json();
  if (!opened.ok) throw new Error(`conversations.open failed: ${opened.error}`);
  const channelId = opened.channel.id;

  // Post message
  const postRes = await fetch('https://slack.com/api/chat.postMessage', {
    method: 'POST',
    headers: {
      Authorization: `Bearer ${SLACK_TOKEN}`,
      'Content-Type': 'application/json',
    },
    body: JSON.stringify({ channel: channelId, text }),
  });
  const posted = await postRes.json();
  if (!posted.ok) throw new Error(`chat.postMessage failed: ${posted.error}`);
  return posted.ts;
}

// ── run ───────────────────────────────────────────────────────────────────────

try {
  const digest = await buildDigest();

  if (DRY_RUN) {
    console.log('=== DRY RUN ===');
    console.log(digest);
  } else {
    const ts = await sendSlackDM(JKH_USER, digest);
    console.log(`[fleet-digest] Sent to ${JKH_USER} (ts=${ts})`);
  }
} catch (err) {
  console.error('[fleet-digest] Fatal:', err.message);
  process.exit(1);
}
