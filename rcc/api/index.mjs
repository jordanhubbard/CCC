/**
 * rcc/api — Claw Command Center REST API
 *
 * Single source of truth for the work queue, agent registry, and heartbeats.
 * Agents talk to this instead of maintaining local queue copies.
 *
 * Port: RCC_PORT env var (default 8789)
 * Auth: Bearer token — must be in RCC_AUTH_TOKENS (comma-separated)
 */

import { createServer } from 'http';
import * as _http from 'http';
import * as _https from 'https';
import { readFile, writeFile, mkdir, chmod, appendFile, readdir } from 'fs/promises';
import { existsSync, createReadStream as createRS, readFileSync, writeFileSync } from 'fs';
import { dirname, join as pathJoin } from 'path';
import { createInterface } from 'readline';
import { createHmac, timingSafeEqual, randomUUID } from 'crypto';
import { Brain, createRequest } from '../brain/index.mjs';
import { embed, upsert as vectorUpsert, search as vectorSearch, searchAll as vectorSearchAll, ensureCollections, collectionStats, channelMemoryIngest, channelMemoryRecall } from '../vector/index.mjs';
import { Pump } from '../scout/pump.mjs';
import * as llmRegistry from '../llm/registry.mjs';
import { learnLesson, queryLessons, queryAllLessons, formatLessonsForContext, getTrendingLessons, formatTrendingForHeartbeat, getHeartbeatContext, receiveLessonFromBus, seedKnownLessons } from '../lessons/index.mjs';
import { generateIdea } from '../ideation/ideation.mjs';
import * as issuesModule from '../issues/index.mjs';

// ── Config ─────────────────────────────────────────────────────────────────
const PORT            = parseInt(process.env.RCC_PORT || '8789', 10);
const EXEC_LOG_PATH   = process.env.EXEC_LOG_PATH || './data/exec-log.jsonl';
const QUEUE_PATH      = process.env.QUEUE_PATH    || '../../workqueue/queue.json';
const AGENTS_PATH        = process.env.AGENTS_PATH        || './agents.json';
const CAPABILITIES_PATH  = process.env.CAPABILITIES_PATH  || './data/agent-capabilities.json';
const REPOS_PATH      = process.env.REPOS_PATH    || './repos.json';
const PROJECTS_PATH   = process.env.PROJECTS_PATH || './projects.json';
const RCC_PUBLIC_URL  = process.env.RCC_PUBLIC_URL || 'http://localhost:8789';
const AUTH_TOKENS  = new Set((process.env.RCC_AUTH_TOKENS || '').split(',').map(t => t.trim()).filter(Boolean));
const RCC_ADMIN_TOKEN = process.env.RCC_ADMIN_TOKEN || process.env.RCC_AUTH_TOKENS?.split(',')[0];
const START_TIME   = Date.now();
const CALENDAR_PATH   = process.env.CALENDAR_PATH   || './data/calendar.json';
const REQUESTS_PATH   = process.env.REQUESTS_PATH   || './data/requests.json';
const SECRETS_PATH       = process.env.SECRETS_PATH       || '../data/secrets.json';
const CONVERSATIONS_PATH = process.env.CONVERSATIONS_PATH || './data/conversations.json';
const USERS_PATH         = process.env.USERS_PATH         || './data/users.json';
const LLM_REGISTRY_PATH  = process.env.LLM_REGISTRY_PATH  || './data/llm-registry.json';
const PROVIDERS_PATH     = process.env.PROVIDERS_PATH     || './data/providers.json';
const TUNNEL_STATE_PATH  = process.env.TUNNEL_STATE_PATH  || './data/tunnel-state.json';
const TUNNEL_USER        = process.env.TUNNEL_USER        || 'tunnel';
const TUNNEL_AUTH_KEYS   = process.env.TUNNEL_AUTH_KEYS   || '/home/tunnel/.ssh/authorized_keys';
const TUNNEL_PORT_START  = parseInt(process.env.TUNNEL_PORT_START || '18080', 10);
const SBOM_DIR           = process.env.SBOM_DIR || './sbom';

// ── Services map ───────────────────────────────────────────────────────────
const SERVICES_CATALOG = [
  { id: 'rcc-dashboard',    name: 'RCC Dashboard',      url: 'http://146.190.134.110:8789/projects',  desc: 'Agent work queue + project tracker',       host: 'do-host1' },
  { id: 'services-map',     name: 'Services Map',       url: 'http://146.190.134.110:8789/services',  desc: 'This page — live status of all services',  host: 'do-host1' },
  { id: 'squirrelchat',     name: 'ClawChat',       url: 'http://146.190.134.110:8790/',           desc: 'Self-hosted team chat (Slack replacement)', host: 'do-host1' },
  { id: 'tokenhub-admin',   name: 'Tokenhub Admin',     url: 'http://146.190.134.110:8090/admin/',     desc: 'LLM router — provider health + config',    host: 'do-host1' },
  { id: 'squirrelbus',      name: 'ClawBus',        url: 'http://146.190.134.110:8789/api/bus/stream', desc: 'Inter-agent message bus (SSE stream)',   host: 'do-host1' },
  { id: 'boris-vllm',       name: 'Boris vLLM',         url: 'http://127.0.0.1:18080/v1/models',       desc: 'Nemotron-120B FP8 — 4x L40 (Sweden)',      host: 'boris'    },
  { id: 'peabody-vllm',     name: 'Peabody vLLM',       url: 'http://127.0.0.1:18081/v1/models',       desc: 'Nemotron-120B FP8 — 4x L40 (Sweden)',      host: 'peabody'  },
  { id: 'sherman-vllm',     name: 'Sherman vLLM',       url: 'http://127.0.0.1:18082/v1/models',       desc: 'Nemotron-120B FP8 — 4x L40 (Sweden)',      host: 'sherman'  },
  { id: 'snidely-vllm',     name: 'Snidely vLLM',       url: 'http://127.0.0.1:18083/v1/models',       desc: 'Nemotron-120B FP8 — 4x L40 (Sweden)',      host: 'snidely'  },
  { id: 'dudley-vllm',      name: 'Dudley vLLM',        url: 'http://127.0.0.1:18084/v1/models',       desc: 'Nemotron-120B FP8 — 4x L40 (Sweden)',      host: 'dudley'   },
  { id: 'whisper-api',      name: 'Whisper API',        url: 'http://100.87.229.125:8792',             desc: 'Speech-to-text (sparky GB10)',              host: 'sparky'   },
  { id: 'agentfs',          name: 'AgentFS',            url: 'http://100.87.229.125:8791',             desc: 'Content-addressed WASM module store',      host: 'sparky'   },
  { id: 'usdagent',         name: 'usdagent',           url: 'http://100.87.229.125:8000',             desc: 'LLM-backed USD 3D asset generator',        host: 'sparky'   },
  { id: 'milvus',           name: 'Milvus',             url: 'http://100.89.199.14:9091/healthz',      desc: 'Vector database (do-host1)',               host: 'do-host1' },
  { id: 'ollama',           name: 'Ollama',             url: 'http://100.87.229.125:11434',            desc: 'Local LLM inference',                     host: 'sparky'   },
];
const SERVICES_CACHE = { data: null, ts: 0 };
const SERVICES_CACHE_TTL = 30_000; // 30 seconds

/** Probe a single URL with a 2-second timeout; returns { online, latency_ms } */
function probeService(rawUrl) {
  return new Promise((resolve) => {
    const start = Date.now();
    let parsed;
    try { parsed = new URL(rawUrl); } catch { return resolve({ online: false, latency_ms: null }); }
    const lib = parsed.protocol === 'https:' ? _https : _http;
    const opts = { hostname: parsed.hostname, port: parsed.port || (parsed.protocol === 'https:' ? 443 : 80), path: parsed.pathname || '/', method: 'HEAD', timeout: 2000 };
    const req = lib.request(opts, (r) => {
      r.resume(); // drain
      resolve({ online: true, latency_ms: Date.now() - start });
    });
    req.on('timeout', () => { req.destroy(); resolve({ online: false, latency_ms: null }); });
    req.on('error', () => resolve({ online: false, latency_ms: null }));
    req.end();
  });
}

let _probing = false;
async function getServicesStatus() {
  // If cache is fresh, return immediately
  if (SERVICES_CACHE.data && (Date.now() - SERVICES_CACHE.ts) < SERVICES_CACHE_TTL) {
    return SERVICES_CACHE.data;
  }
  // If stale cache exists, return it immediately and re-probe in background
  if (SERVICES_CACHE.data && !_probing) {
    _probing = true;
    Promise.all(
      SERVICES_CATALOG.map(async (svc) => {
        const { online, latency_ms } = await probeService(svc.url);
        return { ...svc, online, latency_ms };
      })
    ).then(results => {
      SERVICES_CACHE.data = results;
      SERVICES_CACHE.ts = Date.now();
      _probing = false;
    }).catch(() => { _probing = false; });
    return SERVICES_CACHE.data; // return stale immediately
  }
  // Cold start: must probe (first request ever)
  _probing = true;
  const results = await Promise.all(
    SERVICES_CATALOG.map(async (svc) => {
      const { online, latency_ms } = await probeService(svc.url);
      return { ...svc, online, latency_ms };
    })
  );
  SERVICES_CACHE.data = results;
  SERVICES_CACHE.ts = Date.now();
  _probing = false;
  return results;
}

// Warm the cache on startup so first browser request is instant
setTimeout(() => getServicesStatus().catch(() => {}), 3000);

// ── Semantic dedup: background indexer ────────────────────────────────────
async function indexPendingQueueItems() {
  try {
    const SPARKY_OLLAMA = process.env.SPARKY_OLLAMA_URL || 'http://100.87.229.125:11434';
    const MILVUS_URL    = process.env.MILVUS_URL        || 'http://100.89.199.14:19530';
    const q = await readQueue();
    const active = (q.items || []).filter(i => ['pending','in-progress','in_progress','claimed','incubating'].includes(i.status));
    console.log(`[dedup-indexer] Indexing ${active.length} active queue items into rcc_queue_dedup`);
    let indexed = 0;
    for (const item of active) {
      try {
        const embedText = `${item.title}\n${(item.description || '').slice(0, 300)}`.trim();
        const ctrl = new AbortController();
        const timer = setTimeout(() => ctrl.abort(), 5000);
        const resp = await fetch(`${SPARKY_OLLAMA}/api/embed`, {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ model: 'nomic-embed-text', input: embedText }),
          signal: ctrl.signal,
        });
        clearTimeout(timer);
        if (!resp.ok) continue;
        const data = await resp.json();
        const vector = data?.embeddings?.[0];
        if (!vector || vector.length !== 768) continue;
        await fetch(`${MILVUS_URL}/v2/vectordb/entities/upsert`, {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({
            collectionName: 'rcc_queue_dedup',
            data: [{ id: item.id, vector, title: (item.title || '').slice(0, 256), status: item.status }],
          }),
        });
        indexed++;
        // Small delay to avoid hammering Ollama
        await new Promise(r => setTimeout(r, 200));
      } catch (_) { /* skip individual failures */ }
    }
    console.log(`[dedup-indexer] Indexed ${indexed}/${active.length} items`);
  } catch (err) {
    console.warn('[dedup-indexer] Background indexer error:', err.message);
  }
}

// ── ClawBus paths ──────────────────────────────────────────────────────
const BUS_LOG_PATH   = process.env.BUS_LOG_PATH   || new URL('../../squirrelbus/bus.jsonl', import.meta.url).pathname;
const ACK_LOG_PATH   = process.env.ACK_LOG_PATH   || new URL('../../squirrelbus/acks.jsonl', import.meta.url).pathname;
const DEAD_LOG_PATH  = process.env.DEAD_LOG_PATH  || new URL('../../squirrelbus/dead-letter.jsonl', import.meta.url).pathname;

// ── ClawBus in-memory state ────────────────────────────────────────────
let _busSeq = 0;
const _busSSEClients  = new Set();
const _busPresence    = {};
const _busAcks        = new Map();   // messageId → ack entry
const _busDeadLetters = [];          // [{...msg, _deadReason, _deadAt}]

// Seed seq from log on startup (async, best-effort)
(async () => {
  try {
    if (!existsSync(BUS_LOG_PATH)) return;
    const rl = createInterface({ input: createRS(BUS_LOG_PATH), crlfDelay: Infinity });
    for await (const line of rl) {
      try { const m = JSON.parse(line); if (m.seq > _busSeq) _busSeq = m.seq; } catch {}
    }
    console.log(`[bus] seq seeded at ${_busSeq}`);
  } catch {}
})();

async function _busReadMessages({ from, to, limit = 100, since, type } = {}) {
  const msgs = [];
  try {
    if (!existsSync(BUS_LOG_PATH)) return msgs;
    const rl = createInterface({ input: createRS(BUS_LOG_PATH), crlfDelay: Infinity });
    for await (const line of rl) {
      try {
        const m = JSON.parse(line);
        if (from && m.from !== from) continue;
        if (to && m.to !== to && m.to !== 'all') continue;
        if (type && m.type !== type) continue;
        if (since && new Date(m.ts) <= new Date(since)) continue;
        msgs.push(m);
      } catch {}
    }
  } catch {}
  return msgs.slice(-limit).reverse();
}

async function _busAppend(msg) {
  const full = {
    id: msg.id || randomUUID(),
    from: msg.from || 'unknown',
    to: msg.to || 'all',
    ts: msg.ts || new Date().toISOString(),
    seq: ++_busSeq,
    type: msg.type || 'text',
    mime: msg.mime || 'text/plain',
    enc: msg.enc || 'none',
    body: msg.body || '',
    ref: msg.ref || null,
    subject: msg.subject || null,
    ttl: msg.ttl ?? 604800,
  };
  await appendFile(BUS_LOG_PATH, JSON.stringify(full) + '\n', 'utf8');
  for (const client of _busSSEClients) {
    try { client.write(`data: ${JSON.stringify(full)}\n\n`); }
    catch { _busSSEClients.delete(client); }
  }
  return full;
}

// ── Slack config ───────────────────────────────────────────────────────────
const SLACK_SIGNING_SECRET = process.env.SLACK_SIGNING_SECRET || '';
const SLACK_BOT_TOKEN      = process.env.SLACK_BOT_TOKEN      || process.env.OMGJKH_BOT || '';
const SLACK_API            = 'https://slack.com/api';

// ── jkh completion notifications ──────────────────────────────────────────
const JKH_SLACK_USER       = process.env.SLACK_NOTIFY_USER || 'UDYR7H4SC';  // omgjkh
const notifiedCompletions  = new Set(); // dedup within process lifetime

async function notifyJkhCompletion(item, agent) {
  try {
    // Skip: ideas, jkh-assigned items, silent-tagged items, already notified
    if (!item || !item.id) return;
    if (notifiedCompletions.has(item.id)) return;
    if ((item.priority === 'idea') || (item.assignee === 'jkh')) return;
    if ((item.tags || []).some(t => t === 'silent' || t === 'no-notify')) return;
    const token = SLACK_BOT_TOKEN;
    if (!token) return;
    notifiedCompletions.add(item.id);
    const resolution = (item.resolution || item.result || '').slice(0, 200);
    const text = `✅ *${item.title}* — completed by ${agent || item.claimedBy || 'unknown'}\n${resolution ? resolution + '\n' : ''}_${item.id}_`;
    fetch(`${SLACK_API}/chat.postMessage`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', 'Authorization': `Bearer ${token}` },
      body: JSON.stringify({ channel: JKH_SLACK_USER, text }),
    }).catch(e => console.warn('[notify-jkh] Slack DM failed:', e.message));
  } catch (e) {
    console.warn('[notify-jkh] error:', e.message);
  }
}

// ── Project Slack channel fan-out ─────────────────────────────────────────
async function fanoutToProjectChannels(projectId, text) {
  if (!SLACK_BOT_TOKEN || !projectId) return;
  try {
    const projects = await readProjects();
    const project = projects.find(p => p.id === projectId);
    if (!project?.slack_channels?.length) return;
    for (const ch of project.slack_channels) {
      slackPost('chat.postMessage', { channel: ch.channel_id, text, mrkdwn: true })
        .catch(e => console.warn(`[fanout] ${ch.channel_id}: ${e.message}`));
    }
  } catch (e) {
    console.warn('[fanout] error:', e.message);
  }
}

// ── Stale claim thresholds (ms) by executor type ───────────────────────────
// claude_cli: real coding agents, can run 60-90min on complex tasks
// gpu: render jobs, can run hours
// inference_key: fast LLM calls, should finish in minutes
const STALE_THRESHOLDS = {
  claude_cli:    parseInt(process.env.STALE_CLAUDE_MS    || String(120 * 60 * 1000), 10), // 2h
  gpu:           parseInt(process.env.STALE_GPU_MS       || String(6  * 60 * 60 * 1000), 10), // 6h
  inference_key: parseInt(process.env.STALE_INFERENCE_MS || String(30 * 60 * 1000), 10), // 30min
  llm_server:    parseInt(process.env.STALE_LLM_MS       || String(45 * 60 * 1000), 10), // 45min
  default:       parseInt(process.env.STALE_DEFAULT_MS   || String(60 * 60 * 1000), 10), // 1h
};

// ── In-memory heartbeats ───────────────────────────────────────────────────
const heartbeats = {};
const heartbeatHistory = {};

// Warm heartbeat map from JSONL history on startup so metrics don't zero out after restarts.
// Reads the last line of each agent's history file; marks entries >5min old as stale.
async function warmHeartbeatsFromHistory() {
  const STALE_MS = 5 * 60 * 1000;
  const histDir = new URL('./data/heartbeat-history', import.meta.url).pathname;
  let files;
  try { files = await readdir(histDir); } catch { return; }
  const jsonlFiles = files.filter(f => f.endsWith('.jsonl'));
  for (const file of jsonlFiles) {
    try {
      const content = await readFile(`${histDir}/${file}`, 'utf8');
      const lines = content.trim().split('\n').filter(Boolean);
      if (!lines.length) continue;
      const last = JSON.parse(lines[lines.length - 1]);
      if (!last.agent) continue;
      const agentKey = last.agent.toLowerCase(); // normalize casing
      const age = Date.now() - new Date(last.ts).getTime();
      heartbeats[agentKey] = {
        agent: agentKey,
        ts: last.ts,
        status: age > STALE_MS ? 'stale' : (last.status || 'online'),
        host: last.host || null,
        _restoredFromHistory: true,
        _wasOnline: age <= STALE_MS,
      };
    } catch { /* skip malformed files */ }
  }
  const count = Object.keys(heartbeats).length;
  if (count) console.log(`[heartbeats] Warmed ${count} agent(s) from history on startup`);
}
const cronStatus = {};
const providerHealth = {};
const geekSseClients = new Set();
const BOOTSTRAP_TOKENS_PATH = process.env.BOOTSTRAP_TOKENS_PATH || '/home/jkh/.openclaw/workspace/rcc/data/bootstrap-tokens.json';
const bootstrapTokens = (() => {
  const m = new Map();
  try {
    const raw = JSON.parse(readFileSync(BOOTSTRAP_TOKENS_PATH, 'utf8'));
    const now = Date.now();
    for (const [k, v] of Object.entries(raw)) {
      if (v.expiresAt > now && !v.used) m.set(k, v);
    }
    console.log(`[bootstrap] Loaded ${m.size} token(s) from disk`);
  } catch (e) { if (e.code !== 'ENOENT') console.error('[bootstrap] load failed:', e.message); }
  return m;
})();
function saveBootstrapTokens() {
  try { writeFileSync(BOOTSTRAP_TOKENS_PATH, JSON.stringify(Object.fromEntries(bootstrapTokens), null, 2)); }
  catch (e) { console.error('[bootstrap] save failed:', e.message); }
}

// ── Disappearance detection config ────────────────────────────────────────
const OFFLINE_THRESHOLD_MS = parseInt(process.env.OFFLINE_THRESHOLD_MS || String(60 * 60 * 1000), 10); // 1h
const offlineAlertSent = {};  // agent -> timestamp of last offline alert sent

// ── Offline detection + Slack alert ───────────────────────────────────────
function computeOnlineStatus(hb) {
  if (!hb || !hb.ts) return false;
  if (hb.decommissioned) return false;
  return (Date.now() - new Date(hb.ts).getTime()) < OFFLINE_THRESHOLD_MS;
}

async function runDisappearanceCheck() {
  const SLACK_AGENT_CHANNEL = process.env.SLACK_AGENT_CHANNEL || '#agent-shared';
  const now = Date.now();
  const agents = await readAgents().catch(() => ({}));
  for (const [agent, hb] of Object.entries(heartbeats)) {
    if (hb.decommissioned) continue;
    const age = now - new Date(hb.ts).getTime();
    const isOffline = age >= OFFLINE_THRESHOLD_MS;
    const wasOnline = hb._wasOnline !== false;  // default: assume was online
    if (isOffline && wasOnline) {
      hb._wasOnline = false;
      // Only alert if we haven't alerted in the last 2h
      const lastAlert = offlineAlertSent[agent] || 0;
      if (now - lastAlert > 2 * 60 * 60 * 1000) {
        offlineAlertSent[agent] = now;
        const lastSeenMin = Math.round(age / 60000);
        const msg = `:red_circle: *${agent}* has gone offline — last seen ${lastSeenMin} minutes ago (${hb.ts}). No heartbeat for >${Math.round(OFFLINE_THRESHOLD_MS/60000)} min.`;
        if (SLACK_BOT_TOKEN) {
          slackPost('chat.postMessage', { channel: SLACK_AGENT_CHANNEL, text: msg }).catch(() => {});
        }
        // Persist offline status to agents registry
        if (agents[agent]) {
          agents[agent].lastSeen = hb.ts;
          agents[agent].onlineStatus = 'offline';
          await writeAgents(agents).catch(() => {});
        }
      }
    } else if (!isOffline) {
      hb._wasOnline = true;
      if (agents[agent] && agents[agent].onlineStatus === 'offline') {
        agents[agent].onlineStatus = 'online';
        await writeAgents(agents).catch(() => {});
      }
    }
  }
}

// Run disappearance check every 5 minutes
setInterval(runDisappearanceCheck, 5 * 60 * 1000);

// ── Generic JSON file helpers ─────────────────────────────────────────────
async function readJsonFile(pathSpec, defaultValue = {}) {
  const p = pathSpec.startsWith('/') ? pathSpec : new URL(pathSpec, import.meta.url).pathname;
  if (!existsSync(p)) return defaultValue;
  try { return JSON.parse(await readFile(p, 'utf8')); }
  catch { return defaultValue; }
}

async function writeJsonFile(pathSpec, data) {
  const p = pathSpec.startsWith('/') ? pathSpec : new URL(pathSpec, import.meta.url).pathname;
  await mkdir(dirname(p), { recursive: true });
  await writeFile(p, JSON.stringify(data, null, 2));
}

// ── Projects I/O ─────────────────────────────────────────────────────────
async function readProjects() {
  const p = new URL(PROJECTS_PATH, import.meta.url).pathname;
  if (!existsSync(p)) return [];
  return JSON.parse(await readFile(p, 'utf8'));
}

async function writeProjects(data) {
  const p = new URL(PROJECTS_PATH, import.meta.url).pathname;
  await writeFile(p, JSON.stringify(data, null, 2));
}

function projectUrl(fullName) {
  return `${RCC_PUBLIC_URL}/api/projects/${encodeURIComponent(fullName)}`;
}

function buildProjectFromRepo(repo) {
  return {
    id:            repo.full_name,
    display_name:  repo.display_name || repo.full_name.split('/')[1],
    description:   repo.description || '',
    github_url:    `https://github.com/${repo.full_name}`,
    rcc_url:       projectUrl(repo.full_name),
    issue_tracker: repo.issue_tracker_url ? `https://${repo.issue_tracker_url}` : `https://github.com/${repo.full_name}/issues`,
    slack_channels: repo.ownership?.slack_channel
      ? [{ workspace: repo.ownership.slack_workspace || 'omgjkh', channel_id: repo.ownership.slack_channel }]
      : [],
    triaging_agent: repo.ownership?.triaging_agent || process.env.DEFAULT_TRIAGING_AGENT || '',
    enabled:        repo.enabled !== false,
    kind:           repo.kind || 'personal',
    scouts:         repo.scouts || [],
    notes:          repo.notes || '',
    registeredAt:   repo.registeredAt || new Date().toISOString(),
    updatedAt:      repo.updatedAt || new Date().toISOString(),
  };
}

// ── Repo helpers ───────────────────────────────────────────────────────────
function repoOwnershipSummary(repo) {
  if (!repo.ownership) return { kind: repo.kind || 'personal', label: repo.full_name.split('/')[0] };
  const o = repo.ownership;
  if (o.model === 'sole') {
    return { kind: 'personal', label: o.owner || repo.full_name.split('/')[0], sole: true };
  }
  // team/org: list contributor logins
  const contributors = Array.isArray(o.contributors)
    ? o.contributors.map(c => typeof c === 'string' ? c : c.github)
    : [];
  return {
    kind: repo.kind || 'team',
    label: contributors.slice(0, 3).join(', ') + (contributors.length > 3 ? ` +${contributors.length - 3}` : ''),
    contributors,
    slack_channel: o.slack_channel || null,
  };
}

// ── Brain (lazy init) ─────────────────────────────────────────────────────
let brain = null;
async function getBrain() {
  if (!brain) {
    brain = new Brain();
    await brain.init();
    brain.start();
  }
  return brain;
}

// ── Pump (lazy init) ──────────────────────────────────────────────────────
let pump = null;
function getPump() {
  if (!pump) {
    pump = new Pump();
    pump.start();
  }
  return pump;
}

// ── Auth ───────────────────────────────────────────────────────────────────
function isAuthed(req) {
  if (AUTH_TOKENS.size === 0) return true; // no tokens configured = open (dev mode)
  const auth = req.headers['authorization'] || '';
  const token = auth.replace(/^Bearer\s+/i, '').trim();
  return AUTH_TOKENS.has(token);
}

function isAdminAuthed(req) {
  if (!RCC_ADMIN_TOKEN) return true; // no admin token configured = open (dev mode)
  const auth = req.headers['authorization'] || '';
  const token = auth.replace(/^Bearer\s+/i, '').trim();
  return token === RCC_ADMIN_TOKEN;
}

// ── Queue I/O ──────────────────────────────────────────────────────────────
// ── Queue mutex — prevents concurrent read-mutate-write races ─────────────
// Node.js is single-threaded, but interleaved async I/O can cause two claim
// requests to both read 'pending', both pass the guard, and both write back.
// A simple promise-chain mutex collapses concurrent calls into serial order.
let _queueMutexTail = Promise.resolve();

async function withQueueLock(fn) {
  // Append to the tail of the chain; each caller waits for prior work to finish
  const next = _queueMutexTail.then(() => fn()).catch(err => { throw err; });
  // Always advance tail (even if fn throws) so subsequent callers aren't blocked
  _queueMutexTail = next.catch(() => {});
  return next;
}

async function readQueue() {
  const p = new URL(QUEUE_PATH, import.meta.url).pathname;
  if (!existsSync(p)) return { items: [], completed: [] };
  return JSON.parse(await readFile(p, 'utf8'));
}

async function writeQueue(data) {
  const p = new URL(QUEUE_PATH, import.meta.url).pathname;
  await writeFile(p, JSON.stringify(data, null, 2));
}

// ── Request tickets I/O ───────────────────────────────────────────────────
async function readRequests() {
  const p = new URL(REQUESTS_PATH, import.meta.url).pathname;
  if (!existsSync(p)) return [];
  return JSON.parse(await readFile(p, 'utf8'));
}

async function writeRequests(data) {
  const p = new URL(REQUESTS_PATH, import.meta.url).pathname;
  await writeFile(p, JSON.stringify(data, null, 2));
}

// ── Secrets I/O ───────────────────────────────────────────────────────────
// secrets.json stores named bundles (service aliases) and scalar key→value pairs.
// Named aliases: slack, minio, milvus, nvidia, github
// Each alias maps to an object of env-var-name → value.
// Individual secrets are stored as top-level key → scalar string.
async function readSecrets() {
  const p = new URL(SECRETS_PATH, import.meta.url).pathname;
  if (!existsSync(p)) return {};
  return JSON.parse(await readFile(p, 'utf8'));
}

async function writeSecrets(data) {
  const p = new URL(SECRETS_PATH, import.meta.url).pathname;
  await mkdir(dirname(p), { recursive: true });
  await writeFile(p, JSON.stringify(data, null, 2));
  await chmod(p, 0o600);
}

// ── Agent registry I/O ────────────────────────────────────────────────────
async function readAgents() {
  const p = new URL(AGENTS_PATH, import.meta.url).pathname;
  if (!existsSync(p)) return {};
  const raw = JSON.parse(await readFile(p, 'utf8'));
  // Normalise: if stored as array (legacy []), convert to {} keyed by name
  if (Array.isArray(raw)) {
    const obj = {};
    for (const a of raw) { if (a && a.name) obj[a.name] = a; }
    return obj;
  }
  return raw;
}

async function writeAgents(data) {
  const p = new URL(AGENTS_PATH, import.meta.url).pathname;
  await writeFile(p, JSON.stringify(data, null, 2));
}

// ── Agent capabilities I/O ────────────────────────────────────────────────
async function readCapabilities() {
  const p = new URL(CAPABILITIES_PATH, import.meta.url).pathname;
  if (!existsSync(p)) return {};
  return JSON.parse(await readFile(p, 'utf8'));
}

async function writeCapabilities(data) {
  const p = new URL(CAPABILITIES_PATH, import.meta.url).pathname;
  await mkdir(dirname(p), { recursive: true });
  await writeFile(p, JSON.stringify(data, null, 2));
}

// ── Calendar I/O ───────────────────────────────────────────────────────────
async function readCalendar() {
  const p = new URL(CALENDAR_PATH, import.meta.url).pathname;
  if (!existsSync(p)) return [];
  return JSON.parse(await readFile(p, 'utf8'));
}

async function writeCalendar(data) {
  const p = new URL(CALENDAR_PATH, import.meta.url).pathname;
  await mkdir(dirname(p), { recursive: true });
  await writeFile(p, JSON.stringify(data, null, 2));
}

// ── Conversations I/O ─────────────────────────────────────────────────────
async function readConversations() {
  const p = new URL(CONVERSATIONS_PATH, import.meta.url).pathname;
  if (!existsSync(p)) return [];
  return JSON.parse(await readFile(p, 'utf8'));
}

async function writeConversations(data) {
  const p = new URL(CONVERSATIONS_PATH, import.meta.url).pathname;
  await mkdir(dirname(p), { recursive: true });
  await writeFile(p, JSON.stringify(data, null, 2));
}

// ── Users I/O ─────────────────────────────────────────────────────────────
async function readUsers() {
  const p = new URL(USERS_PATH, import.meta.url).pathname;
  if (!existsSync(p)) return [];
  return JSON.parse(await readFile(p, 'utf8'));
}

async function writeUsers(data) {
  const p = new URL(USERS_PATH, import.meta.url).pathname;
  await mkdir(dirname(p), { recursive: true });
  await writeFile(p, JSON.stringify(data, null, 2));
}

// ── HTTP helpers ───────────────────────────────────────────────────────────
function json(res, status, body) {
  const payload = JSON.stringify(body);
  res.writeHead(status, { 'Content-Type': 'application/json', 'Access-Control-Allow-Origin': '*' });
  res.end(payload);
}

function readBody(req) {
  return new Promise((resolve, reject) => {
    let body = '';
    req.on('data', chunk => { body += chunk; if (body.length > 1e6) reject(new Error('Body too large')); });
    req.on('end', () => { try { resolve(body ? JSON.parse(body) : {}); } catch { reject(new Error('Invalid JSON')); } });
    req.on('error', reject);
  });
}

// ── Geek SSE broadcast ─────────────────────────────────────────────────────
function broadcastGeekEvent(type, from, to, label) {
  if (geekSseClients.size === 0) return;
  const data = JSON.stringify({ type, from, to, label, ts: new Date().toISOString() });
  const msg = `data: ${data}\n\n`;
  for (const client of geekSseClients) {
    try { client.write(msg); } catch { geekSseClients.delete(client); }
  }
}

// ── HTML UI helpers ────────────────────────────────────────────────────────
const HTML_STYLE = `
  <meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
  <style>
    *{box-sizing:border-box;margin:0;padding:0}
    body{font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',sans-serif;background:#0d1117;color:#e6edf3;min-height:100vh;padding:2rem}
    a{color:#58a6ff;text-decoration:none}a:hover{text-decoration:underline}
    .nav{font-size:.85rem;color:#8b949e;margin-bottom:1.5rem}
    .nav a{color:#8b949e}
    h1{font-size:1.8rem;font-weight:700;margin-bottom:.4rem}
    .subtitle{color:#8b949e;font-size:.95rem;margin-bottom:1.5rem}
    .card{background:#161b22;border:1px solid #30363d;border-radius:8px;padding:1.25rem 1.5rem;margin-bottom:1rem}
    .card h2{font-size:1rem;font-weight:600;margin-bottom:.5rem}
    .meta{display:flex;flex-wrap:wrap;gap:.5rem 1.5rem;font-size:.85rem;color:#8b949e;margin-bottom:.75rem}
    .meta span{display:flex;align-items:center;gap:.3rem}
    .badge{display:inline-block;padding:.15rem .55rem;border-radius:999px;font-size:.75rem;font-weight:600;background:#21262d;border:1px solid #30363d;color:#8b949e}
    .badge.team{border-color:#388bfd55;color:#58a6ff}
    .badge.personal{border-color:#3fb95055;color:#3fb950}
    .scouts{display:flex;flex-wrap:wrap;gap:.35rem;margin-top:.75rem}
    .scout-tag{background:#21262d;border:1px solid #30363d;border-radius:4px;padding:.1rem .5rem;font-size:.75rem;color:#8b949e}
    .notes{color:#c9d1d9;font-size:.875rem;margin-top:.75rem;line-height:1.5;border-top:1px solid #21262d;padding-top:.75rem}
    .links{display:flex;flex-wrap:wrap;gap:.5rem 1.5rem;margin-top:.75rem;font-size:.85rem}
    .project-grid{display:grid;grid-template-columns:repeat(auto-fill,minmax(340px,1fr));gap:1rem}
    .project-card{background:#161b22;border:1px solid #30363d;border-radius:8px;padding:1.25rem;cursor:pointer;transition:border-color .15s}
    .project-card:hover{border-color:#58a6ff}
    .project-card h3{font-size:1rem;font-weight:600;margin-bottom:.35rem}
    .project-card .desc{font-size:.85rem;color:#8b949e;line-height:1.45;display:-webkit-box;-webkit-line-clamp:2;-webkit-box-orient:vertical;overflow:hidden}
    .error{color:#f85149;margin-top:2rem;font-size:1rem}
    .spinner{color:#8b949e;margin-top:2rem}
    .detail-header{margin-bottom:1.5rem}
    .detail-header h1{margin-bottom:.3rem}
    .queue-section h2{font-size:1.1rem;font-weight:600;margin-bottom:.75rem}
    .queue-item{background:#161b22;border:1px solid #30363d;border-radius:6px;padding:.75rem 1rem;margin-bottom:.5rem;font-size:.875rem}
    .queue-item .qi-title{font-weight:600;margin-bottom:.25rem}
    .qi-meta{font-size:.78rem;color:#8b949e;display:flex;gap:.75rem;flex-wrap:wrap}
    .status-badge{display:inline-block;padding:.1rem .45rem;border-radius:4px;font-size:.72rem;font-weight:600;text-transform:uppercase}
    .status-pending{background:#1f2d3d;color:#58a6ff;border:1px solid #388bfd55}
    .status-active{background:#1a2f1a;color:#3fb950;border:1px solid #3fb95055}
    .status-completed{background:#1c1c1c;color:#8b949e;border:1px solid #30363d}
    .status-cancelled{background:#1c1c1c;color:#8b949e;border:1px solid #30363d}
    .status-failed{background:#2d1a1a;color:#f85149;border:1px solid #f8514955}
    .gh-panel{margin-top:1rem}
    .gh-columns{display:grid;grid-template-columns:1fr 1fr;gap:1rem}
    @media(max-width:680px){.gh-columns{grid-template-columns:1fr}}
    .gh-col-header{font-size:.95rem;font-weight:600;margin-bottom:.6rem;display:flex;align-items:center;gap:.5rem}
    .gh-item{background:#0d1117;border:1px solid #21262d;border-radius:6px;padding:.65rem .85rem;margin-bottom:.45rem;font-size:.835rem;transition:border-color .15s}
    .gh-item:hover{border-color:#388bfd55}
    .gh-item-title{font-weight:500;line-height:1.35;margin-bottom:.3rem}
    .gh-item-title a{color:#e6edf3}.gh-item-title a:hover{color:#58a6ff}
    .gh-meta{display:flex;flex-wrap:wrap;align-items:center;gap:.3rem .6rem;font-size:.75rem;color:#8b949e}
    .gh-num{color:#6e7681;font-size:.78rem;margin-right:.2rem}
    .label-chip{display:inline-block;padding:.1rem .42rem;border-radius:999px;font-size:.7rem;font-weight:600;border:1px solid transparent;line-height:1.4}
    .draft-badge{background:#21262d;color:#8b949e;border:1px solid #30363d;padding:.1rem .4rem;border-radius:4px;font-size:.7rem;font-weight:600;margin-right:.2rem}
    .review-approved{color:#3fb950;font-weight:600}.review-changes{color:#f85149;font-weight:600}.review-pending{color:#d29922}
    .merge-ok{color:#a371f7;font-weight:600}.merge-conflict{color:#f85149}
    .gh-empty{color:#8b949e;font-size:.85rem;padding:.4rem 0}
    .gh-refresh-btn{background:transparent;border:1px solid #30363d;color:#8b949e;border-radius:4px;padding:.15rem .55rem;font-size:.75rem;cursor:pointer;transition:border-color .15s,color .15s;margin-left:.5rem}
    .gh-refresh-btn:hover{border-color:#58a6ff;color:#58a6ff}
    .gh-fetched{font-size:.72rem;color:#484f58}
    .gh-error{color:#f85149;font-size:.82rem;padding:.4rem 0}
  </style>`;

function dashboardHtml() {
  return `<!DOCTYPE html><html lang="en"><head>${HTML_STYLE}
  <style>
    body{padding:0;overflow:hidden;height:100vh;display:flex;flex-direction:column}
    .topbar{display:flex;align-items:center;gap:1rem;padding:.6rem 1.25rem;background:#010409;border-bottom:1px solid #21262d;flex-shrink:0}
    .topbar-logo{font-weight:700;font-size:1rem;color:#e6edf3;display:flex;align-items:center;gap:.45rem}
    .topbar-logo span{color:#58a6ff}
    .tab-bar{display:flex;gap:.15rem;flex:1;overflow-x:auto}
    .tab-bar::-webkit-scrollbar{height:3px}.tab-bar::-webkit-scrollbar-thumb{background:#30363d}
    .tab{padding:.35rem .85rem;border-radius:6px;font-size:.85rem;color:#8b949e;cursor:pointer;white-space:nowrap;border:none;background:none;transition:background .15s,color .15s}
    .tab:hover{background:#161b22;color:#e6edf3}
    .tab.active{background:#161b22;color:#58a6ff;font-weight:600}
    .topbar-links{display:flex;gap:.75rem;font-size:.82rem;flex-shrink:0}
    .topbar-links a{color:#8b949e}.topbar-links a:hover{color:#58a6ff}
    .content{flex:1;overflow:hidden;position:relative}
    .pane{position:absolute;inset:0;overflow-y:auto;padding:1.5rem;display:none}
    .pane.active{display:block}
    /* Services pane */
    .svc-grid{display:grid;grid-template-columns:repeat(auto-fill,minmax(300px,1fr));gap:1rem}
    .svc-card{background:#161b22;border:1px solid #30363d;border-radius:8px;padding:1.1rem 1.3rem;display:flex;flex-direction:column;gap:.4rem}
    .svc-card:hover{border-color:#58a6ff}
    .svc-name{font-weight:700;font-size:.95rem}
    .svc-desc{font-size:.82rem;color:#8b949e;line-height:1.4;flex:1}
    .svc-footer{display:flex;align-items:center;justify-content:space-between;font-size:.78rem;margin-top:.2rem}
    .svc-url a{color:#58a6ff;word-break:break-all}
    .host-tag{background:#21262d;border:1px solid #30363d;border-radius:4px;padding:.1rem .4rem;font-size:.7rem;color:#8b949e}
    .status-dot{display:inline-block;width:.5rem;height:.5rem;border-radius:50%;margin-right:.3rem}
    .s-online{background:#3fb950}.s-offline{background:#f85149}.s-unknown{background:#8b949e}
    .s-badge-online{color:#3fb950;font-size:.76rem;font-weight:600}
    .s-badge-offline{color:#f85149;font-size:.76rem;font-weight:600}
    .s-badge-unknown{color:#8b949e;font-size:.76rem}
    .latency{color:#8b949e;font-size:.7rem;margin-left:.2rem}
    /* Queue pane */
    .queue-toolbar{display:flex;gap:.75rem;margin-bottom:1rem;flex-wrap:wrap;align-items:center}
    .queue-filter{background:#161b22;border:1px solid #30363d;border-radius:6px;padding:.3rem .7rem;color:#e6edf3;font-size:.82rem}
    .q-list{display:flex;flex-direction:column;gap:.5rem}
    .q-item{background:#161b22;border:1px solid #30363d;border-radius:6px;padding:.75rem 1rem}
    .q-item:hover{border-color:#388bfd55}
    .q-title{font-weight:600;font-size:.9rem;margin-bottom:.3rem}
    .q-meta{display:flex;gap:.6rem;flex-wrap:wrap;font-size:.76rem;color:#8b949e;align-items:center}
    /* Agents pane */
    .agents-grid{display:grid;grid-template-columns:repeat(auto-fill,minmax(240px,1fr));gap:1rem}
    .agent-card{background:#161b22;border:1px solid #30363d;border-radius:8px;padding:1rem 1.2rem}
    .agent-card.online{border-color:#3fb95044}
    .agent-card.away{border-color:#e3b34144}
    .agent-card.offline{border-color:#f8514933;opacity:.75}
    .agent-name{font-weight:700;font-size:.95rem;margin-bottom:.3rem}
    .agent-meta{font-size:.78rem;color:#8b949e;line-height:1.6}
    .agent-badge{display:inline-block;font-size:.7rem;border-radius:3px;padding:.1rem .4rem;margin-right:.3rem}
    .b-gpu{background:#6e40c922;color:#a371f7;border:1px solid #6e40c955}
    .b-vllm{background:#1f6feb22;color:#58a6ff;border:1px solid #1f6feb55}
    /* Bus pane */
    .bus-toolbar{display:flex;gap:.75rem;margin-bottom:.75rem;align-items:center}
    .bus-status{font-size:.78rem;color:#8b949e}
    .bus-status.live{color:#3fb950}
    .bus-log{font-family:'SF Mono',Consolas,monospace;font-size:.78rem;background:#010409;border:1px solid #21262d;border-radius:6px;padding:.75rem 1rem;height:calc(100vh - 200px);overflow-y:auto;display:flex;flex-direction:column;gap:.3rem}
    .bus-entry{display:flex;gap:.75rem;padding:.25rem 0;border-bottom:1px solid #21262d22}
    .bus-ts{color:#484f58;min-width:6rem;flex-shrink:0}
    .bus-type{color:#a371f7;min-width:7rem;flex-shrink:0}
    .bus-from{color:#58a6ff;min-width:6rem;flex-shrink:0}
    .bus-body{color:#8b949e;white-space:pre-wrap;word-break:break-all}
    /* Logs pane */
    .log-stream{font-family:'SF Mono',Consolas,monospace;font-size:.78rem;background:#010409;border:1px solid #21262d;border-radius:6px;padding:.75rem 1rem;height:calc(100vh - 180px);overflow-y:auto}
    .log-line{padding:.15rem 0;border-bottom:1px solid #21262d22;display:flex;gap:.75rem}
    .log-ts{color:#484f58;min-width:5rem;flex-shrink:0}
    .log-src{min-width:5rem;flex-shrink:0;font-weight:600}
    .log-msg{color:#8b949e;word-break:break-all}
    .src-rcc{color:#58a6ff}.src-brain{color:#a371f7}.src-queue{color:#3fb950}.src-agent{color:#e3b341}
    /* Shared */
    h1{font-size:1.4rem;font-weight:700;margin-bottom:.25rem}
    .subtitle{color:#8b949e;font-size:.875rem;margin-bottom:1.25rem}
    .section-title{font-size:1rem;font-weight:600;color:#8b949e;margin:1.25rem 0 .6rem}
    .spinner{color:#8b949e;font-size:.9rem}
    .err{color:#f85149;font-size:.875rem}
    .refresh-btn{background:transparent;border:1px solid #30363d;color:#8b949e;border-radius:4px;padding:.2rem .6rem;font-size:.76rem;cursor:pointer}
    .refresh-btn:hover{border-color:#58a6ff;color:#58a6ff}
    .empty{color:#8b949e;font-size:.875rem;padding:.5rem 0}
    /* Timeline pane */
    .tl-panel{background:#161b22;border:1px solid #30363d;border-radius:8px;padding:1.1rem 1.3rem;overflow-x:auto}
    .tl-legend{display:flex;flex-wrap:wrap;gap:.4rem .9rem;margin-bottom:1rem;font-size:.78rem}
    .tl-legend-item{display:flex;align-items:center;gap:.35rem;color:#8b949e}
    .tl-dot{width:9px;height:9px;border-radius:50%;flex-shrink:0}
    .tl-slot-row{display:flex;align-items:center;gap:.6rem;margin-bottom:.5rem}
    .tl-slot-label{min-width:3.8rem;font-size:.8rem;font-weight:600;color:#8b949e;flex-shrink:0}
    .tl-axis{position:relative;height:26px;flex:1;background:#0d1117;border:1px solid #21262d;border-radius:4px}
    .tl-marker{position:absolute;top:50%;transform:translate(-50%,-50%);width:9px;height:9px;border-radius:50%;cursor:pointer;transition:transform .1s}
    .tl-marker:hover{transform:translate(-50%,-50%) scale(1.7)!important}
    .tl-marker.tl-fault{border-radius:2px;transform:translate(-50%,-50%) rotate(45deg)}
    .tl-time-axis{display:flex;justify-content:space-between;font-size:.68rem;color:#484f58;margin-top:.25rem;padding:0 0 .15rem}
    .tl-tooltip{position:fixed;background:#161b22;border:1px solid #30363d;border-radius:6px;padding:.45rem .7rem;font-size:.78rem;color:#e6edf3;pointer-events:none;z-index:200;max-width:230px;display:none;line-height:1.5}
  </style>
  <title>Claw Command Center</title>
</head><body>
  <div class="topbar">
    <div class="topbar-logo">🐿️ <span>RCC</span> Claw Command Center</div>
    <div class="tab-bar" id="tabs">
      <button class="tab active" data-pane="services">Services</button>
      <button class="tab" data-pane="queue">Queue</button>
      <button class="tab" data-pane="agents">Agents</button>
      <button class="tab" data-pane="projects">Projects</button>
      <button class="tab" data-pane="bus">ClawBus</button>
      <button class="tab" data-pane="logs">Logs</button>
      <button class="tab" data-pane="timeline">⏱ Timeline</button>
    </div>
    <div class="topbar-links">
      <a href="http://146.190.134.110:8790/" target="_blank">💬 Chat</a>
      <a href="http://146.190.134.110:8090/admin/" target="_blank">🔀 Tokenhub</a>
    </div>
  </div>
  <div class="content">
    <!-- SERVICES -->
    <div class="pane active" id="pane-services">
      <h1>Services</h1>
      <p class="subtitle">Live health — probed on load</p>
      <div id="svc-root"><p class="spinner">Probing…</p></div>
    </div>
    <!-- QUEUE -->
    <div class="pane" id="pane-queue">
      <h1>Work Queue</h1>
      <p class="subtitle">All items across all agents</p>
      <div class="queue-toolbar">
        <select class="queue-filter" id="q-status-filter">
          <option value="active,pending">Active &amp; Pending</option>
          <option value="pending">Pending only</option>
          <option value="active">Active only</option>
          <option value="failed">Failed</option>
          <option value="">All (incl. completed)</option>
        </select>
        <select class="queue-filter" id="q-agent-filter"><option value="">All agents</option></select>
        <button class="refresh-btn" onclick="loadQueue()">↻ Refresh</button>
        <span id="q-count" style="font-size:.78rem;color:#8b949e"></span>
      </div>
      <div class="q-list" id="q-root"><p class="spinner">Loading…</p></div>
    </div>
    <!-- AGENTS -->
    <div class="pane" id="pane-agents">
      <h1>Agents</h1>
      <p class="subtitle">Heartbeat status across the fleet</p>
      <button class="refresh-btn" onclick="loadAgents()" style="margin-bottom:1rem">↻ Refresh</button>
      <div class="agents-grid" id="agents-root"><p class="spinner">Loading…</p></div>
    </div>
    <!-- PROJECTS -->
    <div class="pane" id="pane-projects">
      <h1>Projects</h1>
      <p class="subtitle">Registered repos tracked by RCC</p>
      <div id="proj-root"><p class="spinner">Loading…</p></div>
    </div>
    <!-- BUS -->
    <div class="pane" id="pane-bus">
      <h1>ClawBus</h1>
      <div class="bus-toolbar">
        <span class="bus-status" id="bus-status">connecting…</span>
        <button class="refresh-btn" onclick="reconnectBus()">↻ Reconnect</button>
      </div>
      <div class="bus-log" id="bus-log"></div>
    </div>
    <!-- LOGS -->
    <div class="pane" id="pane-logs">
      <h1>Activity Log</h1>
      <p class="subtitle">Recent events across RCC</p>
      <button class="refresh-btn" onclick="loadLogs()" style="margin-bottom:.75rem">↻ Refresh</button>
      <div class="log-stream" id="log-root"><p class="spinner">Loading…</p></div>
    </div>
    <!-- TIMELINE -->
    <div class="pane" id="pane-timeline">
      <h1>Agent Lifecycle Timeline</h1>
      <p class="subtitle">agentOS per-slot events · last 30 min · auto-refreshes every 10s</p>
      <div class="tl-legend">
        <div class="tl-legend-item"><div class="tl-dot" style="background:#3fb950"></div>spawn</div>
        <div class="tl-legend-item"><div class="tl-dot" style="background:#2ea043"></div>exit</div>
        <div class="tl-legend-item"><div class="tl-dot" style="background:#58a6ff"></div>cap_grant</div>
        <div class="tl-legend-item"><div class="tl-dot" style="background:#f85149"></div>cap_revoke</div>
        <div class="tl-legend-item"><div class="tl-dot" style="background:#e3b341"></div>quota_exceeded</div>
        <div class="tl-legend-item"><div class="tl-dot" style="background:#f85149;border-radius:2px"></div>fault</div>
        <div class="tl-legend-item"><div class="tl-dot" style="background:#d29922"></div>watchdog_reset</div>
        <div class="tl-legend-item"><div class="tl-dot" style="background:#a371f7"></div>memory_alert</div>
      </div>
      <div id="tl-root"><p class="spinner">Loading…</p></div>
      <div class="tl-tooltip" id="tl-tooltip"></div>
    </div>
  </div>
  <script>
    function esc(s){return String(s||'').replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;');}
    function timeAgo(ds){if(!ds)return'never';const s=Math.floor((Date.now()-new Date(ds))/1000);if(s<5)return'just now';if(s<60)return s+'s ago';if(s<3600)return Math.floor(s/60)+'m ago';if(s<86400)return Math.floor(s/3600)+'h ago';return Math.floor(s/86400)+'d ago';}

    // ── Tab routing ──────────────────────────────────────────────────────
    const paneLoaders = {};
    function switchTab(name) {
      document.querySelectorAll('.tab').forEach(t => t.classList.toggle('active', t.dataset.pane === name));
      document.querySelectorAll('.pane').forEach(p => p.classList.toggle('active', p.id === 'pane-'+name));
      location.hash = name;
      if (paneLoaders[name]) paneLoaders[name]();
    }
    document.querySelectorAll('.tab').forEach(t => t.addEventListener('click', () => switchTab(t.dataset.pane)));

    // ── SERVICES ─────────────────────────────────────────────────────────
    function renderServices(svcs) {
      const root = document.getElementById('svc-root');
      if (!root) return;
      root.innerHTML = '<div class="svc-grid">' + svcs.map(s => {
        const ok = s.online, lat = ok && s.latency_ms != null ? '<span class="latency">' + s.latency_ms + 'ms</span>' : '';
        const dot = ok === null ? 's-unknown' : ok ? 's-online' : 's-offline';
        const badge = ok === null ? 's-badge-unknown' : ok ? 's-badge-online' : 's-badge-offline';
        const label = ok === null ? 'unknown' : ok ? 'online' : 'offline';
        return \`<div class="svc-card">
          <div style="display:flex;align-items:center;justify-content:space-between">
            <span class="svc-name">\${esc(s.name)}</span>
            <span class="\${badge}"><span class="status-dot \${dot}"></span>\${label}\${lat}</span>
          </div>
          <div class="svc-desc">\${esc(s.desc)}</div>
          <div class="svc-footer">
            <div class="svc-url"><a href="\${esc(s.url)}" target="_blank">\${esc(s.url)}</a></div>
            <span class="host-tag">\${esc(s.host)}</span>
          </div>
        </div>\`;
      }).join('') + '</div>';
    }
    function loadServices() {
      const root = document.getElementById('svc-root');
      if (!root) return;
      root.innerHTML = '<p class="spinner">Probing… (takes ~2s on cold load)</p>';
      fetch('/api/services/status')
        .then(r => r.json())
        .then(svcs => renderServices(svcs))
        .catch(e => {
          const root2 = document.getElementById('svc-root');
          if (root2) root2.innerHTML = '<p class="err">Failed: ' + esc(e.message) + '</p><button class="refresh-btn" onclick="loadServices()" style="margin-top:.75rem">↻ Retry</button>';
        });
    }
    paneLoaders['services'] = loadServices;

    // ── QUEUE ─────────────────────────────────────────────────────────────
    let _queueData = [];
    function renderQueue() {
      const sf = document.getElementById('q-status-filter').value;
      const af = document.getElementById('q-agent-filter').value;
      let items = _queueData;
      if (sf) {
        const statuses = sf.split(',');
        items = items.filter(i => statuses.includes(i.status));
      }
      if (af) items = items.filter(i=>i.assignee===af||i.agent===af);
      document.getElementById('q-count').textContent = items.length + ' item' + (items.length!==1?'s':'');
      if (!items.length) { document.getElementById('q-root').innerHTML='<p class="empty">No items.</p>'; return; }
      const statusClass = s => ({'pending':'status-pending','active':'status-active','completed':'status-completed','failed':'status-failed','cancelled':'status-cancelled'}[s]||'');
      document.getElementById('q-root').innerHTML = items.slice(0,100).map(i=>\`
        <div class="q-item">
          <div class="q-title">\${esc(i.title)}</div>
          <div class="q-meta">
            <span class="status-badge \${statusClass(i.status)}">\${esc(i.status)}</span>
            \${i.priority?'<span>⚡ '+esc(i.priority)+'</span>':''}
            \${(i.assignee||i.agent)?'<span>👤 '+esc(i.assignee||i.agent)+'</span>':''}
            \${i.createdAt?'<span>'+timeAgo(i.createdAt)+'</span>':''}
            \${i.description?'<span style="color:#c9d1d9;font-size:.78rem;white-space:normal">'+esc(i.description.slice(0,120))+'</span>':''}
          </div>
        </div>\`).join('');
    }
    function loadQueue() {
      document.getElementById('q-root').innerHTML='<p class="spinner">Loading…</p>';
      fetch('/api/queue').then(r=>r.json()).then(data=>{
        _queueData = Array.isArray(data) ? data : (data.items||data.queue||[]);
        // Populate agent filter
        const agents = [...new Set(_queueData.map(i=>i.assignee||i.agent).filter(Boolean))];
        const af = document.getElementById('q-agent-filter');
        af.innerHTML = '<option value="">All agents</option>' + agents.map(a=>\`<option value="\${esc(a)}">\${esc(a)}</option>\`).join('');
        renderQueue();
      }).catch(e=>{document.getElementById('q-root').innerHTML='<p class="err">Failed: '+esc(e.message)+'</p>';});
    }
    document.getElementById('q-status-filter').addEventListener('change', renderQueue);
    document.getElementById('q-agent-filter').addEventListener('change', renderQueue);
    paneLoaders['queue'] = loadQueue;

    // ── AGENTS ────────────────────────────────────────────────────────────
    function loadAgents() {
      document.getElementById('agents-root').innerHTML='<p class="spinner">Loading…</p>';
      fetch('/api/heartbeats').then(r=>r.json()).then(data=>{
        const agents = Array.isArray(data) ? data : (data.agents || Object.values(data) || []);
        if (!agents.length) { document.getElementById('agents-root').innerHTML='<p class="empty">No agents.</p>'; return; }
        // Sort: online first, then away, then offline
        const statusRank = s => s==='online'?0:s==='away'?1:2;
        const sorted = [...agents].sort((a,b) => {
          const sa = a.online ? 'online' : ((Date.now()-new Date(a.lastSeen||a.ts||0))/1000 < 600 ? 'away' : 'offline');
          const sb = b.online ? 'online' : ((Date.now()-new Date(b.lastSeen||b.ts||0))/1000 < 600 ? 'away' : 'offline');
          return statusRank(sa) - statusRank(sb);
        });
        document.getElementById('agents-root').innerHTML = sorted.map(a=>{
          // Use server-computed online field when present; fall back to lastSeen age
          const seen = new Date(a.lastSeen || a.ts || 0);
          const age = (Date.now() - seen) / 1000;
          const status = a.online === true ? 'online' : a.online === false ? (age < 600 ? 'away' : 'offline') : (age < 120 ? 'online' : age < 600 ? 'away' : 'offline');
          const statusLabel = status === 'online' ? '● Online' : status === 'away' ? '◐ Away' : '○ Offline';
          const statusStyle = status === 'online' ? 'color:#3fb950;font-weight:700' : status === 'away' ? 'color:#e3b341;font-weight:600' : 'color:#f85149;font-weight:600';
          const badges = (a.gpu?'<span class="agent-badge b-gpu">GPU</span>':'')+(a.vllm?'<span class="agent-badge b-vllm">vLLM</span>':'');
          const decom = a.decommissioned ? '<span style="color:#f85149;font-size:.72rem"> [decommissioned]</span>' : '';
          return \`<div class="agent-card \${status}" style="\${a.decommissioned?'opacity:.45':''}">
            <div style="display:flex;align-items:center;justify-content:space-between;margin-bottom:.4rem">
              <span class="agent-name">\${esc(a.name||a.agent||'?')}\${decom}</span>
              <span style="\${statusStyle};font-size:.8rem">\${statusLabel}</span>
            </div>
            <div class="agent-meta">
              \${badges}
              \${a.host?'<div>🖥 '+esc(a.host)+'</div>':''}
              \${a.status&&a.status!=='online'?'<div>'+esc(a.status)+'</div>':''}
              <div style="color:#6e7681;font-size:.75rem">last seen: \${timeAgo(a.lastSeen||a.ts)}</div>
            </div>
          </div>\`;
        }).join('');
      }).catch(e=>{document.getElementById('agents-root').innerHTML='<p class="err">Failed: '+esc(e.message)+'</p>';});
    }
    paneLoaders['agents'] = loadAgents;

    // ── PROJECTS ──────────────────────────────────────────────────────────
    function loadProjects() {
      document.getElementById('proj-root').innerHTML='<p class="spinner">Loading…</p>';
      fetch('/api/projects').then(r=>r.json()).then(projects=>{
        if (!projects.length) { document.getElementById('proj-root').innerHTML='<p class="empty">No projects.</p>'; return; }
        const renderCard = p => \`<a href="/projects/\${encodeURIComponent(p.id)}" style="text-decoration:none;display:block" target="_blank">
          <div class="project-card" style="background:#161b22;border:1px solid #30363d;border-radius:8px;padding:1.1rem;margin-bottom:.6rem;cursor:pointer">
            <div style="display:flex;align-items:center;justify-content:space-between;margin-bottom:.35rem">
              <strong style="font-size:.95rem">\${esc(p.display_name||p.id)}</strong>
              <span class="badge \${p.kind||''}">\${esc(p.kind||'project')}</span>
            </div>
            <div style="font-size:.82rem;color:#8b949e;line-height:1.4">\${esc(p.description||'')}</div>
          </div></a>\`;
        document.getElementById('proj-root').innerHTML = projects.map(renderCard).join('');
      }).catch(e=>{document.getElementById('proj-root').innerHTML='<p class="err">Failed: '+esc(e.message)+'</p>';});
    }
    paneLoaders['projects'] = loadProjects;

    // ── BUS ───────────────────────────────────────────────────────────────
    let _busEs = null;
    function appendBusEntry(msg) {
      const log = document.getElementById('bus-log');
      const div = document.createElement('div');
      div.className = 'bus-entry';
      const ts = msg.ts ? new Date(msg.ts).toLocaleTimeString() : new Date().toLocaleTimeString();
      div.innerHTML = \`<span class="bus-ts">\${esc(ts)}</span><span class="bus-type">\${esc(msg.type||'message')}</span><span class="bus-from">\${esc(msg.from||'?')}</span><span class="bus-body">\${esc(typeof msg.body==='object'?JSON.stringify(msg.body):msg.body||'')}</span>\`;
      log.appendChild(div);
      if (log.children.length > 500) log.removeChild(log.firstChild);
      log.scrollTop = log.scrollHeight;
    }
    function reconnectBus() {
      if (_busEs) { _busEs.close(); _busEs = null; }
      const status = document.getElementById('bus-status');
      status.textContent = 'connecting…'; status.className = 'bus-status';
      document.getElementById('bus-log').innerHTML = '';
      _busEs = new EventSource('/api/bus/stream');
      _busEs.onopen = () => { status.textContent = '● live'; status.className = 'bus-status live'; };
      _busEs.onerror = () => { status.textContent = '✗ disconnected'; status.className = 'bus-status'; };
      _busEs.onmessage = e => { try { appendBusEntry(JSON.parse(e.data)); } catch {} };
    }
    paneLoaders['bus'] = () => { if (!_busEs || _busEs.readyState === 2) reconnectBus(); };

    // ── LOGS ──────────────────────────────────────────────────────────────
    function loadLogs() {
      document.getElementById('log-root').innerHTML='<p class="spinner">Loading…</p>';
      // Pull from queue activity + heartbeat stream as a synthetic log
      Promise.all([
        fetch('/api/queue').then(r=>r.json()).catch(()=>[]),
        fetch('/api/heartbeats').then(r=>r.json()).catch(()=>[]),
      ]).then(([qRaw, hbRaw]) => {
        const q = Array.isArray(qRaw) ? qRaw : (qRaw.items||qRaw.queue||[]);
        const hb = Array.isArray(hbRaw) ? hbRaw : (hbRaw.agents||Object.values(hbRaw)||[]);
        const lines = [];
        q.slice(0,40).forEach(i => {
          if (i.updatedAt||i.createdAt) lines.push({ts:new Date(i.updatedAt||i.createdAt),src:'queue',msg:\`[\${i.status}] \${i.title||''}\`});
        });
        hb.forEach(a => {
          if (a.ts||a.lastSeen) lines.push({ts:new Date(a.ts||a.lastSeen),src:'agent',msg:\`heartbeat from \${a.name||a.agent||'?'} (\${a.status||'online'})\`});
        });
        lines.sort((a,b)=>b.ts-a.ts);
        if (!lines.length) { document.getElementById('log-root').innerHTML='<p class="empty">No recent activity.</p>'; return; }
        document.getElementById('log-root').innerHTML = lines.slice(0,100).map(l=>\`
          <div class="log-line">
            <span class="log-ts">\${l.ts.toLocaleTimeString()}</span>
            <span class="log-src src-\${l.src}">\${esc(l.src)}</span>
            <span class="log-msg">\${esc(l.msg)}</span>
          </div>\`).join('');
      });
    }
    paneLoaders['logs'] = loadLogs;

    // ── TIMELINE ──────────────────────────────────────────────────────────
    const TL_COLORS = {spawn:'#3fb950',exit:'#2ea043',cap_grant:'#58a6ff',cap_revoke:'#f85149',quota_exceeded:'#e3b341',fault:'#f85149',watchdog_reset:'#d29922',memory_alert:'#a371f7'};
    let _tlTimer = null;
    const _tlTooltip = document.getElementById('tl-tooltip');
    document.addEventListener('mousemove', e => { _tlTooltip.style.left=(e.clientX+14)+'px'; _tlTooltip.style.top=(e.clientY-8)+'px'; });
    function renderTimeline(events) {
      const now = Date.now(), wMs = 30*60*1000, tMin = now - wMs;
      const slots = {};
      for (let i=0;i<8;i++) slots[i]=[];
      (events||[]).forEach(ev => { if(ev.slot_id>=0&&ev.slot_id<=7) slots[ev.slot_id].push(ev); });
      const labels = [];
      for (let i=0;i<=5;i++) labels.push(new Date(tMin+wMs*i/5).toLocaleTimeString([], {hour:'2-digit',minute:'2-digit'}));
      let html='<div class="tl-panel">';
      for (let s=0;s<8;s++) {
        const markers = slots[s].map(ev => {
          const pct = Math.max(0,Math.min(99,(ev.ts-tMin)/wMs*100)).toFixed(2);
          const col = TL_COLORS[ev.type]||'#8b949e';
          const isFault = ev.type==='fault';
          const tip = \`<b>\${esc(ev.type)}</b><br>Slot \${s} · \${new Date(ev.ts).toLocaleTimeString()}\${ev.details?'<br>'+esc(ev.details):''}\`;
          return \`<div class="tl-marker\${isFault?' tl-fault':''}" style="left:\${pct}%;background:\${col}\${isFault?';transform:translate(-50%,-50%) rotate(45deg)':''}" data-tip="\${tip.replace(/"/g,'&quot;')}"></div>\`;
        }).join('');
        html += \`<div class="tl-slot-row"><div class="tl-slot-label">Slot \${s}</div><div class="tl-axis">\${markers}</div></div>\`;
      }
      html += \`<div class="tl-time-axis">\${labels.map(l=>\`<span>\${esc(l)}</span>\`).join('')}</div></div>\`;
      document.getElementById('tl-root').innerHTML = html;
      document.querySelectorAll('.tl-marker').forEach(m => {
        m.addEventListener('mouseenter', () => { _tlTooltip.innerHTML=m.dataset.tip; _tlTooltip.style.display='block'; });
        m.addEventListener('mouseleave', () => { _tlTooltip.style.display='none'; });
      });
    }
    function loadTimeline() {
      fetch('/api/agentos/events').then(r=>r.json()).then(d=>renderTimeline(d.events||[])).catch(e=>{document.getElementById('tl-root').innerHTML='<p class="err">Failed: '+esc(e.message)+'</p>';});
    }
    paneLoaders['timeline'] = () => {
      loadTimeline();
      if (_tlTimer) clearInterval(_tlTimer);
      _tlTimer = setInterval(loadTimeline, 10000);
    };

    // ── Init: switch to default tab AFTER all loaders are registered ────
    const initTab = (location.hash||'').replace('#','') || 'services';
    switchTab(initTab);

    // Auto-refresh services every 30s when on that pane
    setInterval(() => {
      if (document.getElementById('pane-services').classList.contains('active')) loadServices();
    }, 30000);
  </script>
</body></html>`;
}

function projectsListHtml() {
  return `<!DOCTYPE html><html lang="en"><head>${HTML_STYLE}<title>Projects — RCC</title></head><body>
  <div class="nav"><a href="/">← RCC</a> &nbsp;·&nbsp; <a href="/services">Services</a></div>
  <h1>Projects</h1>
  <p class="subtitle">All registered projects tracked by Claw Command Center</p>
  <div id="root"><p class="spinner">Loading…</p></div>
  <script>
    fetch('/api/projects').then(r=>r.json()).then(projects=>{
      const root=document.getElementById('root');
      if(!projects.length){root.innerHTML='<p class="error">No projects found.</p>';return;}
      const byKind=(k)=>projects.filter(p=>p.kind===k);
      const renderCard=(p)=>\`<a href="/projects/\${encodeURIComponent(p.id)}" style="text-decoration:none">
        <div class="project-card">
          <div style="display:flex;align-items:center;justify-content:space-between;margin-bottom:.4rem">
            <h3>\${p.display_name||p.id}</h3>
            <span class="badge \${p.kind||''}">\${p.kind||'project'}</span>
          </div>
          <div class="desc">\${p.description||''}</div>
        </div></a>\`;
      const sections=[];
      const team=byKind('team'), personal=byKind('personal'), other=projects.filter(p=>p.kind!=='team'&&p.kind!=='personal');
      if(team.length) sections.push(\`<h2 style="font-size:1rem;font-weight:600;color:#8b949e;margin:1.25rem 0 .6rem">Team Projects</h2><div class="project-grid">\${team.map(renderCard).join('')}</div>\`);
      if(personal.length) sections.push(\`<h2 style="font-size:1rem;font-weight:600;color:#8b949e;margin:1.25rem 0 .6rem">Personal Projects</h2><div class="project-grid">\${personal.map(renderCard).join('')}</div>\`);
      if(other.length) sections.push(\`<div class="project-grid">\${other.map(renderCard).join('')}</div>\`);
      root.innerHTML=sections.join('');
    }).catch(e=>{document.getElementById('root').innerHTML='<p class="error">Failed to load projects: '+e.message+'</p>';});
  </script></body></html>`;
}

function packagesHtml() {
  return `<!DOCTYPE html><html lang="en"><head>${HTML_STYLE}
  <title>nano packages</title>
  <style>
    body{background:#0d1117;color:#c9d1d9;font-family:'Segoe UI',system-ui,sans-serif;margin:0;padding:1.5rem}
    .nav{margin-bottom:1.5rem;color:#8b949e;font-size:.9rem}
    .nav a{color:#58a6ff;text-decoration:none}
    h1{margin:0 0 1rem;font-size:1.6rem}
    .search-wrap{display:flex;gap:.75rem;margin-bottom:1.5rem}
    #q{flex:1;background:#161b22;border:1px solid #30363d;color:#c9d1d9;border-radius:6px;padding:.5rem .85rem;font-size:.95rem}
    #q:focus{outline:none;border-color:#58a6ff}
    .pkg-grid{display:grid;grid-template-columns:repeat(auto-fill,minmax(340px,1fr));gap:1rem}
    .pkg-card{background:#161b22;border:1px solid #30363d;border-radius:8px;padding:1.1rem 1.3rem;display:flex;flex-direction:column;gap:.4rem;transition:border-color .15s}
    .pkg-card:hover{border-color:#58a6ff}
    .pkg-name{font-size:1.05rem;font-weight:600;color:#e6edf3}
    .pkg-name a{color:#58a6ff;text-decoration:none}
    .pkg-name a:hover{text-decoration:underline}
    .pkg-ver{font-size:.78rem;background:#21262d;border:1px solid #30363d;color:#8b949e;border-radius:4px;padding:.1rem .4rem}
    .pkg-desc{color:#8b949e;font-size:.88rem;line-height:1.45}
    .pkg-meta{font-size:.78rem;color:#6e7681;display:flex;gap:1rem;flex-wrap:wrap;margin-top:.2rem}
    .pkg-deps{font-size:.78rem;color:#6e7681}
    .pkg-deps span{background:#161b22;border:1px solid #30363d;border-radius:4px;padding:.1rem .4rem;margin:.1rem .2rem .1rem 0;display:inline-block}
    .badge{font-size:.72rem;padding:.15rem .45rem;border-radius:10px;font-weight:600}
    .badge-nano{background:#1a3a2a;color:#3fb950;border:1px solid #3fb950}
    #status{color:#8b949e;font-size:.85rem;margin-bottom:.75rem;min-height:1.2em}
    .empty{color:#8b949e;text-align:center;padding:3rem;font-size:.95rem}
    .error-msg{color:#f85149;font-size:.85rem;padding:1rem;background:#21262d;border-radius:6px}
  </style>
</head><body>
<div class="nav"><a href="/">← RCC</a> &nbsp;·&nbsp; <a href="/services">Services</a></div>
<h1>📦 nano packages</h1>
<div class="search-wrap">
  <input id="q" type="text" placeholder="Search packages…" autocomplete="off"/>
</div>
<div id="status">Loading…</div>
<div id="grid" class="pkg-grid"></div>
<script>
const REGISTRY_API = 'https://api.github.com/repos/jordanhubbard/nano-packages/contents/packages';
const REGISTRY_RAW = 'https://raw.githubusercontent.com/jordanhubbard/nano-packages/main/packages';
let allPkgs = [];

function renderPkg(p) {
  const deps = (p.dependencies && Object.keys(p.dependencies).length)
    ? '<div class="pkg-deps">deps: '
      + Object.entries(p.dependencies).map(([k,v])=>\`<span>\${k}@\${v}</span>\`).join('')
      + '</div>'
    : '';
  const srcLink = p.github
    ? \`<a href="\${p.github}" target="_blank" rel="noopener">source</a>\`
    : '';
  return \`<div class="pkg-card">
    <div class="pkg-name"><a href="\${p.github||'#'}" target="_blank" rel="noopener">\${p.name}</a>
      &nbsp;<span class="pkg-ver">\${p.version||'?'}</span>
      &nbsp;<span class="badge badge-nano">nano</span></div>
    <div class="pkg-desc">\${p.description||'No description.'}</div>
    \${deps}
    <div class="pkg-meta">
      \${p.author ? '<span>👤 '+p.author+'</span>' : ''}
      \${p.license ? '<span>📄 '+p.license+'</span>' : ''}
      \${srcLink}
    </div>
  </div>\`;
}

function filter(q) {
  const lq = q.toLowerCase();
  const shown = lq ? allPkgs.filter(p =>
    (p.name||'').toLowerCase().includes(lq) ||
    (p.description||'').toLowerCase().includes(lq) ||
    (p.author||'').toLowerCase().includes(lq)
  ) : allPkgs;
  document.getElementById('grid').innerHTML = shown.length
    ? shown.map(renderPkg).join('')
    : '<div class="empty">No packages match "'+q+'"</div>';
  document.getElementById('status').textContent = shown.length + ' package' + (shown.length===1?'':'s') + (lq?' matching "'+q+'"':'');
}

async function loadRegistry() {
  const status = document.getElementById('status');
  try {
    const r = await fetch(REGISTRY_API, {headers: {'Accept':'application/vnd.github+json'}});
    if (!r.ok) {
      if (r.status === 404) {
        // Registry repo doesn't exist yet or no packages dir — show placeholder
        status.textContent = '';
        document.getElementById('grid').innerHTML = '<div class="empty">Registry is empty — publish your first package with <code>nano-pkg publish</code></div>';
        return;
      }
      throw new Error('GitHub API ' + r.status);
    }
    const dirs = await r.json();
    const pkgDirs = Array.isArray(dirs) ? dirs.filter(d => d.type === 'dir') : [];
    if (!pkgDirs.length) {
      status.textContent = '';
      document.getElementById('grid').innerHTML = '<div class="empty">No packages published yet.</div>';
      return;
    }
    status.textContent = 'Loading ' + pkgDirs.length + ' packages…';
    const manifests = await Promise.allSettled(
      pkgDirs.map(d => fetch(REGISTRY_RAW + '/' + d.name + '/nano.toml')
        .then(r => r.ok ? r.text() : null)
        .then(txt => txt ? parseToml(txt, d.name) : null)
      )
    );
    allPkgs = manifests
      .filter(r => r.status === 'fulfilled' && r.value)
      .map(r => r.value)
      .sort((a,b) => (a.name||'').localeCompare(b.name||''));
    filter(document.getElementById('q').value);
  } catch(e) {
    status.textContent = '';
    document.getElementById('grid').innerHTML = '<div class="error-msg">⚠️ Failed to load registry: ' + e.message + '</div>';
  }
}

// Minimal TOML parser for nano.toml package manifests
// Handles: key = "value", key = ["a","b"], [section], [[array]]
function parseToml(txt, pkgName) {
  const out = { name: pkgName };
  let section = out;
  const lines = txt.split(/\\r?\\n/);
  for (const raw of lines) {
    const line = raw.trim();
    if (!line || line.startsWith('#')) continue;
    // [section] header
    const secM = line.match(/^\\[([^\\[\\]]+)\\]$/);
    if (secM) { const k = secM[1].trim(); out[k]=out[k]||{}; section=out[k]; continue; }
    // key = value
    const eqIdx = line.indexOf('=');
    if (eqIdx < 0) continue;
    const key = line.slice(0, eqIdx).trim();
    let val = line.slice(eqIdx+1).trim();
    // string
    if ((val.startsWith('"') && val.endsWith('"')) ||
        (val.startsWith("'") && val.endsWith("'")))
      val = val.slice(1,-1);
    // inline array
    else if (val.startsWith('[') && val.endsWith(']')) {
      val = val.slice(1,-1).split(',')
        .map(s=>s.trim().replace(/^['"]|['"]$/g,''))
        .filter(Boolean);
    }
    section[key] = val;
  }
  // Flatten [package] section into top-level
  if (out.package) Object.assign(out, out.package);
  // Map github URL
  if (!out.github && out.repository) out.github = out.repository;
  if (!out.github && out.name)
    out.github = 'https://github.com/jordanhubbard/nano-packages/tree/main/packages/' + out.name;
  return out;
}

document.getElementById('q').addEventListener('input', e => filter(e.target.value));
loadRegistry();
</script>
</body></html>`;
}

function playgroundHtml() {
  return `<!DOCTYPE html><html lang="en"><head>${HTML_STYLE}
  <style>
    .playground-layout{display:grid;grid-template-columns:1fr 1fr;gap:1rem;height:calc(100vh - 140px)}
    @media(max-width:700px){.playground-layout{grid-template-columns:1fr;height:auto}}
    .editor-pane,.output-pane{display:flex;flex-direction:column;gap:.5rem}
    .pane-label{font-size:.8rem;color:#8b949e;text-transform:uppercase;letter-spacing:.05em;font-weight:600}
    textarea#src{flex:1;font-family:monospace;font-size:.9rem;background:#0d1117;color:#e6edf3;border:1px solid #30363d;border-radius:6px;padding:1rem;resize:none;min-height:300px;outline:none;tab-size:2}
    textarea#src:focus{border-color:#58a6ff}
    #out{flex:1;font-family:monospace;font-size:.85rem;background:#0d1117;color:#e6edf3;border:1px solid #30363d;border-radius:6px;padding:1rem;overflow:auto;white-space:pre-wrap;min-height:300px}
    #out.has-error{color:#f85149;border-color:#f85149}
    #out.has-success{color:#3fb950}
    .run-bar{display:flex;align-items:center;gap:.75rem}
    #run-btn{padding:.45rem 1.2rem;background:#238636;border:none;border-radius:6px;color:#fff;font-size:.9rem;font-weight:600;cursor:pointer}
    #run-btn:hover{background:#2ea043}
    #run-btn:disabled{opacity:.5;cursor:not-allowed}
    .status{font-size:.8rem;color:#8b949e}
    .example-bar{display:flex;gap:.5rem;flex-wrap:wrap;margin-bottom:.25rem}
    .example-btn{padding:.2rem .6rem;background:#161b22;border:1px solid #30363d;border-radius:4px;color:#8b949e;font-size:.78rem;cursor:pointer}
    .example-btn:hover{border-color:#58a6ff;color:#58a6ff}
    .share-btn{padding:.2rem .6rem;background:#161b22;border:1px solid #30363d;border-radius:4px;color:#8b949e;font-size:.78rem;cursor:pointer;margin-left:auto}
  </style>
</head><body>
<div class="nav"><a href="/">← RCC</a> &nbsp;·&nbsp; <a href="/packages">Packages</a> &nbsp;·&nbsp; <a href="/services">Services</a></div>
<h1>🎮 nanolang Playground</h1>
<p style="color:#8b949e;font-size:.9rem;margin-bottom:.75rem">Write and run nanolang programs in your browser — no install required.</p>
<div class="example-bar">
  <span style="font-size:.78rem;color:#8b949e;align-self:center">Examples:</span>
  <button class="example-btn" data-ex="hello">Hello World</button>
  <button class="example-btn" data-ex="fib">Fibonacci</button>
  <button class="example-btn" data-ex="fact">Factorial</button>
  <button class="example-btn" data-ex="list">Lists</button>
  <button class="example-btn" data-ex="match">Pattern Match</button>
</div>
<div class="run-bar">
  <button id="run-btn">▶ Run</button>
  <span class="status" id="status"></span>
  <button class="share-btn" id="share-btn">🔗 Share</button>
</div>
<div class="playground-layout">
  <div class="editor-pane">
    <div class="pane-label">nano source</div>
    <textarea id="src" spellcheck="false" placeholder="Write your nano program here..."></textarea>
  </div>
  <div class="output-pane">
    <div class="pane-label">output</div>
    <div id="out">Click ▶ Run to execute your program.</div>
  </div>
</div>
<script>
const EXAMPLES = {
  hello: \`fn main() -> int {
    (println "Hello from nanolang!")
    (println "The answer is: 42")
    return 0
}\`,
  fib: \`fn fib(n: int) -> int {
    if (< n 2) { return n }
    return (+ (fib (- n 1)) (fib (- n 2)))
}

fn main() -> int {
    let i: int = 0
    while (< i 10) {
        (print (fib i))
        (print " ")
        set i = (+ i 1)
    }
    (println "")
    return 0
}\`,
  fact: \`fn factorial(n: int) -> int {
    if (<= n 1) { return 1 }
    return (* n (factorial (- n 1)))
}

fn main() -> int {
    (println (factorial 10))
    (println (factorial 5))
    return 0
}\`,
  list: \`fn sum(lst: [int]) -> int {
    let acc: int = 0
    let i: int = 0
    while (< i (list_length lst)) {
        set acc = (+ acc (list_get lst i))
        set i = (+ i 1)
    }
    return acc
}

fn main() -> int {
    let nums: [int] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]
    (println (sum nums))
    return 0
}\`,
  match: \`fn describe(n: int) -> string {
    match n {
        0 => "zero",
        1 => "one",
        _ => "many"
    }
}

fn main() -> int {
    (println (describe 0))
    (println (describe 1))
    (println (describe 99))
    return 0
}\`
};

const src = document.getElementById('src');
const out = document.getElementById('out');
const runBtn = document.getElementById('run-btn');
const status = document.getElementById('status');

// Load from URL hash or default to hello
function loadFromHash() {
  const hash = location.hash.slice(1);
  if (hash) {
    try { src.value = decodeURIComponent(atob(hash)); return; } catch(e) {}
  }
  src.value = EXAMPLES.hello;
}
loadFromHash();

// Example buttons
document.querySelectorAll('.example-btn').forEach(btn => {
  btn.addEventListener('click', () => {
    src.value = EXAMPLES[btn.dataset.ex] || '';
    out.textContent = 'Click ▶ Run to execute your program.';
    out.className = '';
    location.hash = '';
  });
});

// Share button
document.getElementById('share-btn').addEventListener('click', () => {
  const encoded = btoa(encodeURIComponent(src.value));
  location.hash = encoded;
  navigator.clipboard?.writeText(location.href);
  status.textContent = 'Link copied!';
  setTimeout(() => { status.textContent = ''; }, 2000);
});

// Tab key support in textarea
src.addEventListener('keydown', e => {
  if (e.key === 'Tab') {
    e.preventDefault();
    const s = src.selectionStart, end = src.selectionEnd;
    src.value = src.value.substring(0, s) + '  ' + src.value.substring(end);
    src.selectionStart = src.selectionEnd = s + 2;
  }
  if ((e.ctrlKey || e.metaKey) && e.key === 'Enter') {
    runBtn.click();
  }
});

// Run
runBtn.addEventListener('click', async () => {
  runBtn.disabled = true;
  status.textContent = 'Compiling…';
  out.className = '';
  out.textContent = '';
  const t0 = Date.now();
  try {
    const resp = await fetch('/api/playground/run', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ source: src.value })
    });
    const data = await resp.json();
    const elapsed = ((Date.now() - t0) / 1000).toFixed(2);
    if (data.error) {
      out.textContent = data.error;
      out.className = 'has-error';
      status.textContent = 'Error';
    } else {
      out.textContent = data.stdout || '(no output)';
      if (data.stderr) out.textContent += '\\n--- stderr ---\\n' + data.stderr;
      out.className = data.exit_code === 0 ? 'has-success' : 'has-error';
      status.textContent = \`Done in \${elapsed}s (exit \${data.exit_code})\`;
    }
  } catch(e) {
    out.textContent = 'Network error: ' + e.message;
    out.className = 'has-error';
    status.textContent = 'Failed';
  }
  runBtn.disabled = false;
});
</script>
</body></html>`;
}

function timelineHtml() {
  return `<!DOCTYPE html><html lang="en"><head>${HTML_STYLE}
  <style>
    .tl-panel{background:#161b22;border:1px solid #30363d;border-radius:8px;padding:1.25rem 1.5rem;overflow-x:auto}
    .tl-legend{display:flex;flex-wrap:wrap;gap:.4rem 1rem;margin-bottom:1.1rem;font-size:.8rem}
    .tl-legend-item{display:flex;align-items:center;gap:.35rem;color:#8b949e}
    .tl-dot{width:10px;height:10px;border-radius:50%;flex-shrink:0}
    .tl-slot-row{display:flex;align-items:center;gap:.75rem;margin-bottom:.6rem}
    .tl-slot-label{min-width:4.25rem;font-size:.82rem;font-weight:600;color:#8b949e;flex-shrink:0}
    .tl-axis{position:relative;height:28px;flex:1;background:#0d1117;border:1px solid #21262d;border-radius:4px;min-width:300px}
    .tl-marker{position:absolute;top:50%;transform:translate(-50%,-50%);width:10px;height:10px;border-radius:50%;cursor:pointer;transition:transform .1s}
    .tl-marker:hover{transform:translate(-50%,-50%) scale(1.7)!important}
    .tl-fault{border-radius:2px;transform:translate(-50%,-50%) rotate(45deg)}
    .tl-time-axis{display:flex;justify-content:space-between;font-size:.7rem;color:#484f58;margin-top:.25rem;padding-bottom:.15rem}
    .tl-tooltip{position:fixed;background:#161b22;border:1px solid #30363d;border-radius:6px;padding:.5rem .75rem;font-size:.78rem;color:#e6edf3;pointer-events:none;z-index:200;max-width:240px;display:none;line-height:1.6}
    .tl-refresh{float:right;background:none;border:1px solid #30363d;color:#8b949e;border-radius:4px;padding:.2rem .6rem;font-size:.78rem;cursor:pointer}
    .tl-refresh:hover{border-color:#58a6ff;color:#58a6ff}
    .tl-auto{font-size:.72rem;color:#484f58;float:right;margin-top:.1rem;margin-right:.5rem}
  </style>
  <title>Timeline — RCC</title></head><body>
  <div class="nav"><a href="/">← RCC</a> &nbsp;·&nbsp; <a href="/services">Services</a></div>
  <h1>agentOS Agent Lifecycle Timeline</h1>
  <p class="subtitle">Per-slot event markers · last 30 min · auto-refreshes every 10s</p>
  <div class="tl-legend">
    <div class="tl-legend-item"><div class="tl-dot" style="background:#3fb950"></div>spawn</div>
    <div class="tl-legend-item"><div class="tl-dot" style="background:#2ea043"></div>exit</div>
    <div class="tl-legend-item"><div class="tl-dot" style="background:#58a6ff"></div>cap_grant</div>
    <div class="tl-legend-item"><div class="tl-dot" style="background:#f85149"></div>cap_revoke</div>
    <div class="tl-legend-item"><div class="tl-dot" style="background:#e3b341"></div>quota_exceeded</div>
    <div class="tl-legend-item"><div class="tl-dot" style="background:#f85149;border-radius:2px;transform:rotate(45deg)"></div>fault</div>
    <div class="tl-legend-item"><div class="tl-dot" style="background:#d29922"></div>watchdog_reset</div>
    <div class="tl-legend-item"><div class="tl-dot" style="background:#a371f7"></div>memory_alert</div>
  </div>
  <span class="tl-auto" id="tl-auto"></span>
  <button class="tl-refresh" onclick="load()">↻ Refresh</button>
  <div id="tl-root" style="margin-top:.5rem"><p class="spinner">Loading…</p></div>
  <div class="tl-tooltip" id="tl-tooltip"></div>
  <script>
    const TL_COLORS = {spawn:'#3fb950',exit:'#2ea043',cap_grant:'#58a6ff',cap_revoke:'#f85149',quota_exceeded:'#e3b341',fault:'#f85149',watchdog_reset:'#d29922',memory_alert:'#a371f7'};
    function esc(s){return String(s||'').replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;');}
    const tooltip = document.getElementById('tl-tooltip');
    document.addEventListener('mousemove', e => { tooltip.style.left=(e.clientX+14)+'px'; tooltip.style.top=(e.clientY-8)+'px'; });
    function renderTimeline(events) {
      const now = Date.now(), wMs = 30*60*1000, tMin = now - wMs;
      const slots = {};
      for (let i=0;i<8;i++) slots[i]=[];
      (events||[]).forEach(ev => { if(ev.slot_id>=0&&ev.slot_id<=7) slots[ev.slot_id].push(ev); });
      const labels = [];
      for (let i=0;i<=5;i++) labels.push(new Date(tMin+wMs*i/5).toLocaleTimeString([],{hour:'2-digit',minute:'2-digit'}));
      let html = '<div class="tl-panel">';
      for (let s=0;s<8;s++) {
        const markers = slots[s].map(ev => {
          const pct = Math.max(0,Math.min(99,(ev.ts-tMin)/wMs*100)).toFixed(2);
          const col = TL_COLORS[ev.type]||'#8b949e';
          const isFault = ev.type==='fault';
          const tip = \`<b>\${esc(ev.type)}</b><br>Slot \${s} · \${new Date(ev.ts).toLocaleTimeString()}\${ev.details?'<br>'+esc(ev.details):''}\`;
          const extraStyle = isFault ? \`;transform:translate(-50%,-50%) rotate(45deg)\` : '';
          return \`<div class="tl-marker\${isFault?' tl-fault':''}" style="left:\${pct}%;background:\${col}\${extraStyle}" data-tip="\${tip.replace(/"/g,'&quot;')}"></div>\`;
        }).join('');
        html += \`<div class="tl-slot-row"><div class="tl-slot-label">Slot \${s}</div><div class="tl-axis">\${markers}</div></div>\`;
      }
      html += \`<div class="tl-time-axis">\${labels.map(l=>\`<span>\${esc(l)}</span>\`).join('')}</div></div>\`;
      document.getElementById('tl-root').innerHTML = html;
      document.querySelectorAll('.tl-marker').forEach(m => {
        m.addEventListener('mouseenter', () => { tooltip.innerHTML=m.dataset.tip; tooltip.style.display='block'; });
        m.addEventListener('mouseleave', () => { tooltip.style.display='none'; });
      });
    }
    function load() {
      fetch('/api/agentos/events')
        .then(r=>r.json())
        .then(d=>{ renderTimeline(d.events||[]); document.getElementById('tl-auto').textContent='updated '+new Date().toLocaleTimeString(); })
        .catch(e=>{document.getElementById('tl-root').innerHTML='<p class="error">Failed: '+esc(e.message)+'</p>';});
    }
    load();
    setInterval(load, 10000);
  </script></body></html>`;
}

function servicesHtml() {
  return `<!DOCTYPE html><html lang="en"><head>${HTML_STYLE}
  <style>
    .svc-grid{display:grid;grid-template-columns:repeat(auto-fill,minmax(320px,1fr));gap:1rem;margin-top:1rem}
    .svc-card{background:#161b22;border:1px solid #30363d;border-radius:8px;padding:1.25rem 1.5rem;display:flex;flex-direction:column;gap:.5rem}
    .svc-card:hover{border-color:#58a6ff}
    .svc-header{display:flex;align-items:center;justify-content:space-between;gap:.5rem}
    .svc-name{font-size:1rem;font-weight:700}
    .svc-desc{font-size:.85rem;color:#8b949e;line-height:1.45}
    .svc-footer{display:flex;align-items:center;justify-content:space-between;margin-top:.25rem;font-size:.8rem}
    .svc-url a{color:#58a6ff;word-break:break-all}
    .host-tag{background:#21262d;border:1px solid #30363d;border-radius:4px;padding:.1rem .45rem;font-size:.72rem;color:#8b949e}
    .status-dot{display:inline-block;width:.55rem;height:.55rem;border-radius:50%;margin-right:.3rem}
    .status-online{background:#3fb950}
    .status-away{background:#e3b341}
    .status-offline{background:#f85149}
    .status-unknown{background:#8b949e}
    .status-badge-online{color:#3fb950;font-size:.78rem;font-weight:600}
    .status-badge-away{color:#e3b341;font-size:.78rem;font-weight:600}
    .status-badge-offline{color:#f85149;font-size:.78rem;font-weight:600}
    .status-badge-unknown{color:#8b949e;font-size:.78rem}
    .latency{color:#8b949e;font-size:.72rem;margin-left:.3rem}
    /* Mesh panel */
    .mesh-panel{background:#161b22;border:1px solid #30363d;border-radius:8px;padding:1.25rem 1.5rem;margin-top:1.5rem}
    .mesh-panel h2{font-size:1.05rem;font-weight:700;margin-bottom:1rem;color:#f0f6fc}
    .mesh-grid{display:grid;grid-template-columns:repeat(auto-fill,minmax(200px,1fr));gap:.75rem;margin-bottom:1rem}
    .mesh-node{background:#0d1117;border:1px solid #30363d;border-radius:6px;padding:.75rem 1rem}
    .mesh-node.online{border-color:#3fb95044}
    .mesh-node.away{border-color:#e3b34144}
    .mesh-node.offline{border-color:#f8514944;opacity:.7}
    .mesh-node-name{font-weight:700;font-size:.9rem;margin-bottom:.3rem}
    .mesh-node-meta{font-size:.78rem;color:#8b949e;line-height:1.5}
    .mesh-node-badge{display:inline-block;font-size:.7rem;border-radius:3px;padding:.1rem .4rem;margin-right:.3rem}
    .badge-gpu{background:#6e40c922;color:#a371f7;border:1px solid #6e40c955}
    .badge-vllm{background:#1f6feb22;color:#58a6ff;border:1px solid #1f6feb55}
    .vibe-slots{display:flex;gap:.5rem;flex-wrap:wrap;margin-top:.5rem}
    .slot{display:flex;align-items:center;gap:.3rem;background:#0d1117;border:1px solid #30363d;border-radius:4px;padding:.2rem .5rem;font-size:.75rem}
    .slot.active{border-color:#3fb95066;color:#3fb950}
    .slot.idle{color:#8b949e}
    .spawn-log{margin-top:.75rem;border-top:1px solid #21262d;padding-top:.75rem}
    .spawn-log h3{font-size:.85rem;color:#8b949e;font-weight:600;margin-bottom:.5rem}
    .spawn-entry{font-size:.78rem;color:#8b949e;padding:.2rem 0;border-bottom:1px solid #21262d22;display:flex;gap:.5rem}
    .spawn-ts{color:#6e7681;min-width:5rem}
    .mesh-refresh{float:right;background:none;border:1px solid #30363d;color:#8b949e;border-radius:4px;padding:.2rem .6rem;font-size:.78rem;cursor:pointer}
    .mesh-refresh:hover{border-color:#58a6ff;color:#58a6ff}
  </style>
  <title>Services — RCC</title></head><body>
  <div class="nav"><a href="/projects">Projects</a> &nbsp;·&nbsp; <a href="/timeline">⏱ Timeline</a> &nbsp;·&nbsp; <a href="/">← RCC</a></div>
  <h1>Services</h1>
  <p class="subtitle">Agent infrastructure — live status probed every 30 seconds</p>
  <div id="root"><p class="spinner">Loading…</p></div>
  <div id="mesh-root"><p class="spinner">Loading mesh…</p></div>
  <script>
    function esc(s){return String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;');}
    function timeAgo(ds){if(!ds)return'never';const s=Math.floor((Date.now()-new Date(ds))/1000);if(s<60)return s+'s ago';if(s<3600)return Math.floor(s/60)+'m ago';if(s<86400)return Math.floor(s/3600)+'h ago';return Math.floor(s/86400)+'d ago';}
    function renderCard(s){
      const online=s.online;
      const dotClass=online===null?'status-unknown':online?'status-online':'status-offline';
      const badgeClass=online===null?'status-badge-unknown':online?'status-badge-online':'status-badge-offline';
      const badgeText=online===null?'unknown':online?'online':'offline';
      const latency=online&&s.latency_ms!=null?'<span class="latency">'+s.latency_ms+'ms</span>':'';
      return \`<div class="svc-card">
        <div class="svc-header">
          <span class="svc-name">\${s.name}</span>
          <span class="\${badgeClass}"><span class="status-dot \${dotClass}"></span>\${badgeText}\${latency}</span>
        </div>
        <div class="svc-desc">\${s.desc}</div>
        <div class="svc-footer">
          <div class="svc-url"><a href="\${s.url}" target="_blank">\${s.url}</a></div>
          <span class="host-tag">\${s.host}</span>
        </div>
      </div>\`;
    }
    function renderMesh(d){
      const nodes=d.nodes||[];
      const vibe=d.vibe_engine;
      const spawnLog=d.spawn_log||[];
      const nodeCards=nodes.map(n=>{
        const badges=(n.gpu?'<span class="mesh-node-badge badge-gpu">GPU</span>':'')+(n.vllm?'<span class="mesh-node-badge badge-vllm">vLLM:'+n.vllm_port+'</span>':'');
        const seen='<div>last seen: '+timeAgo(n.lastSeen)+'</div>';
        const hostTag=n.host&&n.host!==n.name?'<div>host: '+esc(n.host)+'</div>':'';
        const gpuInfo=n.gpu_model?'<div>'+esc(n.gpu_model)+(n.gpu_count?' ×'+n.gpu_count:'')+'</div>':'';
        return \`<div class="mesh-node \${n.status}">
          <div class="mesh-node-name"><span class="status-dot status-\${n.status}"></span>\${esc(n.name)}</div>
          <div class="mesh-node-meta">\${badges}\${hostTag}\${gpuInfo}\${seen}</div>
        </div>\`;
      }).join('');
      let vibeHtml='';
      if(vibe){
        const slots=(vibe.slots||[]).map(s=>\`<div class="slot \${s.state}">\${s.slot_id}: \${s.state}\${s.service_name?' ('+esc(s.service_name)+')':''}</div>\`).join('');
        vibeHtml=\`<div style="margin-top:.75rem;border-top:1px solid #21262d;padding-top:.75rem">
          <div style="font-size:.85rem;font-weight:600;color:#8b949e;margin-bottom:.4rem">agentOS VibeEngine — \${esc(vibe.arch||'riscv64')} · \${vibe.swap_slots?.active||0}/\${vibe.swap_slots?.total||4} slots active</div>
          <div class="vibe-slots">\${slots}</div>
        </div>\`;
      }
      const spawnHtml=spawnLog.length?\`<div class="spawn-log"><h3>Recent Spawns</h3>\${spawnLog.slice(0,5).map(e=>\`<div class="spawn-entry"><span class="spawn-ts">\${timeAgo(e.ts)}</span><span>\${esc(e.agent||'?')} → \${esc(e.type||'?')}</span></div>\`).join('')}</div>\`:'';
      return \`<div class="mesh-panel">
        <div style="display:flex;align-items:center;justify-content:space-between;margin-bottom:.75rem">
          <h2 style="margin-bottom:0">🕸️ agentOS Mesh</h2>
          <button class="mesh-refresh" onclick="loadMesh()">↻ Refresh</button>
        </div>
        <div class="mesh-grid">\${nodeCards||'<p style="color:#8b949e;font-size:.85rem">No nodes found.</p>'}</div>
        \${vibeHtml}
        \${spawnHtml}
        <div style="font-size:.72rem;color:#6e7681;margin-top:.75rem">Updated: \${new Date(d.ts).toLocaleTimeString()}</div>
      </div>\`;
    }
    function loadMesh(){
      fetch('/api/mesh').then(r=>r.json()).then(d=>{
        document.getElementById('mesh-root').innerHTML=renderMesh(d);
      }).catch(e=>{document.getElementById('mesh-root').innerHTML='<p class="error">Mesh unavailable: '+e.message+'</p>';});
    }
    fetch('/api/services/status').then(r=>r.json()).then(services=>{
      const root=document.getElementById('root');
      if(!services.length){root.innerHTML='<p class="error">No services configured.</p>';return;}
      root.innerHTML='<div class="svc-grid">'+services.map(renderCard).join('')+'</div>';
    }).catch(e=>{document.getElementById('root').innerHTML='<p class="error">Failed to load: '+e.message+'</p>';});
    loadMesh();
    setInterval(loadMesh, 30000);
  </script></body></html>`;
}

function projectDetailHtml(projectId) {
  const encodedId = encodeURIComponent(projectId);
  return `<!DOCTYPE html><html lang="en"><head>${HTML_STYLE}<title>${projectId} — RCC</title></head><body>
  <div class="nav"><a href="/projects">← Projects</a></div>
  <div id="root"><p class="spinner">Loading…</p></div>
  <script>
    const projectId=${JSON.stringify(projectId)};
    const encodedId=${JSON.stringify(encodedId)};
    function timeAgo(ds){if(!ds)return'';const s=Math.floor((Date.now()-new Date(ds))/1000);if(s<60)return s+'s ago';if(s<3600)return Math.floor(s/60)+'m ago';if(s<86400)return Math.floor(s/3600)+'h ago';return Math.floor(s/86400)+'d ago';}
    function labelFg(hex){if(!hex||hex==='000000')return'#8b949e';const r=parseInt(hex.slice(0,2),16),g=parseInt(hex.slice(2,4),16),b=parseInt(hex.slice(4,6),16);return(r*299+g*587+b*114)/1000>128?'#0d1117':'#f0f6fc';}
    function labelChip(l){const bg='#'+((l.color&&l.color!=='000000')?l.color:'333');const fg=labelFg(l.color);return\`<span class="label-chip" style="background:\${bg}33;border-color:\${bg}88;color:\${fg}">\${esc(l.name||'')}</span>\`;}
    function renderIssue(i){return\`<div class="gh-item"><div class="gh-item-title"><span class="gh-num">#\${i.number}</span><a href="\${i.url}" target="_blank">\${esc(i.title||'')}</a></div><div class="gh-meta">\${(i.labels||[]).map(labelChip).join('')}<span>\${esc(i.author||'')}</span><span title="\${i.createdAt||''}">\${timeAgo(i.createdAt)}</span>\${i.commentCount?\`<span>💬 \${i.commentCount}</span>\`:''}</div></div>\`;}
    function renderPR(pr){const rc=pr.reviewDecision==='APPROVED'?'review-approved':pr.reviewDecision==='CHANGES_REQUESTED'?'review-changes':'review-pending';const rl=pr.reviewDecision==='APPROVED'?'✓ approved':pr.reviewDecision==='CHANGES_REQUESTED'?'✗ changes req':'⏳ pending review';const mc=pr.mergeable==='MERGEABLE'?'merge-ok':pr.mergeable==='CONFLICTING'?'merge-conflict':'';const ml=pr.mergeable==='MERGEABLE'?'mergeable':pr.mergeable==='CONFLICTING'?'⚠ conflicts':'';return\`<div class="gh-item"><div class="gh-item-title"><span class="gh-num">#\${pr.number}</span>\${pr.isDraft?'<span class="draft-badge">draft</span>':''}<a href="\${pr.url}" target="_blank">\${esc(pr.title||'')}</a></div><div class="gh-meta">\${(pr.labels||[]).map(labelChip).join('')}<span>\${esc(pr.author||'')}</span><span class="\${rc}">\${rl}</span>\${ml?\`<span class="\${mc}">\${ml}</span>\`:''}<span title="\${pr.createdAt||''}">\${timeAgo(pr.createdAt)}</span></div></div>\`;}
    function renderGitHub(ghData){if(!ghData)return'';if(ghData.error)return\`<div class="card gh-panel"><p class="gh-error">GitHub data unavailable: \${esc(ghData.error)}</p></div>\`;const issues=ghData.issues||[];const prs=ghData.prs||[];return\`<div class="card gh-panel"><div style="display:flex;align-items:center;justify-content:space-between;margin-bottom:.85rem"><h2 style="font-size:1.05rem;font-weight:600">🐙 GitHub</h2><span><span class="gh-fetched">fetched \${timeAgo(ghData.fetchedAt)}</span><button class="gh-refresh-btn" onclick="refreshGitHub()">↻ Refresh</button></span></div><div class="gh-columns"><div><div class="gh-col-header">🔴 Issues <span style="color:#8b949e;font-size:.82rem;font-weight:400">\${issues.length} open</span></div>\${issues.length?issues.map(renderIssue).join(''):'<p class="gh-empty">No open issues ✓</p>'}</div><div><div class="gh-col-header">🟣 Pull Requests <span style="color:#8b949e;font-size:.82rem;font-weight:400">\${prs.length} open</span></div>\${prs.length?prs.map(renderPR).join(''):'<p class="gh-empty">No open PRs ✓</p>'}</div></div></div>\`;}
    function refreshGitHub(){const panel=document.querySelector('.gh-panel');if(panel)panel.style.opacity='0.5';fetch('/api/projects/'+encodedId+'/github?refresh=1').then(()=>location.reload()).catch(()=>{if(panel)panel.style.opacity='1';});}
    Promise.all([
      fetch('/api/projects/'+encodedId).then(r=>r.json()),
      fetch('/api/queue').then(r=>r.json()),
      fetch('/api/projects/'+encodedId+'/github').then(r=>r.json()).catch(()=>null),
    ]).then(([p, qdata, ghData])=>{
      if(p.error){document.getElementById('root').innerHTML='<p class="error">'+p.error+'</p>';return;}
      const items=[...(qdata.items||[]),...(qdata.completed||[])].filter(i=>i.project===projectId||i.repo===projectId||(i.slack_channels||[]).some(c=>c===projectId));
      const active=items.filter(i=>!['completed','cancelled'].includes(i.status));
      const done=items.filter(i=>['completed','cancelled'].includes(i.status)).slice(0,10);
      const statusBadge=(s)=>\`<span class="status-badge status-\${s||'pending'}">\${s||'pending'}</span>\`;
      const renderItem=(i)=>\`<div class="queue-item">
        <div class="qi-title">\${i.title||'Untitled'}</div>
        <div class="qi-meta">
          \${statusBadge(i.status)}
          \${i.preferred_executor?'<span>'+i.preferred_executor+'</span>':''}
          \${i.assignedTo?'<span>→ '+i.assignedTo+'</span>':''}
          <span>\${new Date(i.completedAt||i.createdAt||i.created||i.ts||null).toLocaleDateString()}</span>
        </div>
      </div>\`;
      const scoutTags=(p.scouts||[]).map(s=>'<span class="scout-tag">'+s+'</span>').join('');
      const channelLinks=(p.slack_channels||[]).map(c=>'<span>Slack #'+c.channel_id+(c.workspace?' ('+c.workspace+')':'')+'</span>').join('');
      document.getElementById('root').innerHTML=\`
        <div class="detail-header">
          <div style="display:flex;align-items:center;gap:.75rem;margin-bottom:.3rem">
            <h1>\${p.display_name||p.id}</h1>
            <span class="badge \${p.kind||''}">\${p.kind||'project'}</span>
          </div>
          <p class="subtitle">\${p.description||''}</p>
          <div class="links">
            \${p.github_url?'<a href="'+p.github_url+'" target="_blank">GitHub →</a>':''}
            \${p.issue_tracker&&p.issue_tracker!==p.github_url+'/issues'?'<a href="'+p.issue_tracker+'" target="_blank">Issues →</a>':''}
            \${channelLinks}
          </div>
          \${scoutTags?'<div class="scouts">'+scoutTags+'</div>':''}
          \${p.notes?'<div class="notes">'+p.notes+'</div>':''}
        </div>
        \${active.length?'<div class="queue-section card"><h2>Active Work ('+active.length+')</h2>'+active.map(renderItem).join('')+'</div>':''}
        \${done.length?'<div class="queue-section card" style="margin-top:.5rem"><h2>Recent Completed</h2>'+done.map(renderItem).join('')+'</div>':''}
        \${!active.length&&!done.length?'<div class="card"><p style="color:#8b949e;font-size:.875rem">No queue items for this project yet.</p></div>':''}
        \${renderGitHub(ghData)}
      \`
    }).catch(e=>{document.getElementById('root').innerHTML='<p class="error">Failed to load: '+e.message+'</p>';});
  </script></body></html>`;
}

// ── Slack helpers ──────────────────────────────────────────────────────────

/** Read raw body bytes (needed for Slack signature verification) */
function readRawBody(req) {
  return new Promise((resolve, reject) => {
    const chunks = [];
    req.on('data', c => chunks.push(c));
    req.on('end', () => resolve(Buffer.concat(chunks)));
    req.on('error', reject);
  });
}

/** Verify Slack request signature — returns true if valid */
function verifySlackSignature(req, rawBody) {
  if (!SLACK_SIGNING_SECRET) return true; // dev mode — no secret configured
  const ts = req.headers['x-slack-request-timestamp'];
  const sig = req.headers['x-slack-signature'];
  if (!ts || !sig) return false;
  // Replay attack: reject if >5 minutes old
  if (Math.abs(Date.now() / 1000 - parseInt(ts, 10)) > 300) return false;
  const baseString = `v0:${ts}:${rawBody.toString('utf8')}`;
  const hmac = createHmac('sha256', SLACK_SIGNING_SECRET).update(baseString).digest('hex');
  const computed = Buffer.from(`v0=${hmac}`);
  const provided  = Buffer.from(sig);
  if (computed.length !== provided.length) return false;
  return timingSafeEqual(computed, provided);
}

/** Post a message to Slack */
async function slackPost(endpoint, payload) {
  if (!SLACK_BOT_TOKEN) throw new Error('SLACK_BOT_TOKEN not configured');
  const resp = await fetch(`${SLACK_API}/${endpoint}`, {
    method: 'POST',
    headers: {
      'Authorization': `Bearer ${SLACK_BOT_TOKEN}`,
      'Content-Type': 'application/json; charset=utf-8',
    },
    body: JSON.stringify(payload),
  });
  return resp.json();
}

/**
 * Set the Slack channel topic and description (purpose) based on project metadata.
 * Topic:   "<description> | GitHub: <url> | Issues: <tracker> | RCC: <rcc_url>"
 * Purpose: "<display_name> project channel. Post requests here — channel context = project context."
 * Fire-and-forget — errors are logged but do not fail the caller.
 */
async function setSlackChannelMeta(channelId, project) {
  if (!SLACK_BOT_TOKEN || !channelId || !project) return;

  const parts = [];
  if (project.description) parts.push(project.description);
  if (project.github_url)  parts.push(`GitHub: ${project.github_url}`);
  if (project.issue_tracker) parts.push(`Issues: ${project.issue_tracker}`);
  if (project.rcc_url)     parts.push(`RCC: ${project.rcc_url}`);
  const topic   = parts.join(' | ');
  const purpose = `${project.display_name || project.id} project channel. Post requests here — channel context = project context.`;

  await Promise.all([
    slackPost('conversations.setTopic', { channel: channelId, topic }).catch(e =>
      console.warn(`[rcc-api] setTopic ${channelId}: ${e.message}`)),
    slackPost('conversations.setPurpose', { channel: channelId, purpose }).catch(e =>
      console.warn(`[rcc-api] setPurpose ${channelId}: ${e.message}`)),
  ]);
}

/** Format queue summary for Slack */
async function formatQueueSummary() {
  const qdata = await readQueue();
  const pending = (qdata.items || []).filter(i => i.status === 'pending');
  const inProgress = (qdata.items || []).filter(i => i.status === 'in-progress');
  const top = pending
    .sort((a, b) => {
      const pri = { urgent: 0, high: 1, medium: 2, normal: 2, low: 3, idea: 4 };
      return (pri[a.priority] ?? 2) - (pri[b.priority] ?? 2);
    })
    .slice(0, 3);
  let text = `*Queue status:* ${pending.length} pending, ${inProgress.length} in-progress`;
  if (top.length) {
    text += '\n*Top items:*\n' + top.map(i =>
      `• [${i.priority}] ${i.title?.slice(0, 80) ?? i.id} _(${i.assignee})_`
    ).join('\n');
  }
  return text;
}

/** Format heartbeat/agent status for Slack */
async function formatAgentStatus() {
  const agentsData = await readAgents().catch(() => []);
  const aList = Array.isArray(agentsData) ? agentsData : (agentsData.agents || []);
  const agents = aList.map(a => {
    const mins = a.lastSeen ? Math.round((Date.now() - new Date(a.lastSeen).getTime()) / 60000) : null;
    const status = mins === null ? '?' : mins < 5 ? '🟢' : mins < 30 ? '🟡' : '🔴';
    return `${status} *${a.name || a.id}* — ${mins === null ? 'never' : `${mins}m ago`} (${a.host || 'unknown host'})`;
  });
  return agents.length ? agents.join('\n') : '_No agents registered_';
}

// ── Router ─────────────────────────────────────────────────────────────────

// ── Route imports ──────────────────────────────────────────────────────────
import registerQueue    from './routes/queue.mjs';
import registerAgents   from './routes/agents.mjs';
import registerBus      from './routes/bus.mjs';
import registerAgentOS  from './routes/agentos.mjs';
import registerMemory   from './routes/memory.mjs';
import registerUI       from './routes/ui.mjs';
import registerServices from './routes/services.mjs';
import registerProjects from './routes/projects.mjs';
import registerSetup    from './routes/setup.mjs';

// ── Custom router ──────────────────────────────────────────────────────────
function createRouter() {
  const routes = [];
  return {
    on(method, pattern, handler) {
      routes.push({ method, pattern, handler });
    },
    async dispatch(req, res, url, method, path) {
      for (const route of routes) {
        if (route.method !== method && route.method !== '*') continue;
        let m;
        if (typeof route.pattern === 'string') {
          if (path !== route.pattern) continue;
          m = {};
        } else {
          m = path.match(route.pattern);
          if (!m) continue;
        }
        return route.handler(req, res, m, url);
      }
      return false;
    },
  };
}

// ── Resolve EXEC_LOG_PATH to an absolute path for route modules ────────────
const EXEC_LOG_PATH_ABS = EXEC_LOG_PATH.startsWith('/')
  ? EXEC_LOG_PATH
  : new URL(EXEC_LOG_PATH, import.meta.url).pathname;

// ── Build state object ─────────────────────────────────────────────────────
const state = {
  // config
  RCC_PUBLIC_URL, AUTH_TOKENS, TUNNEL_USER,
  BUS_LOG_PATH, ACK_LOG_PATH,
  EXEC_LOG_PATH, EXEC_LOG_PATH_ABS, AGENTS_PATH, CAPABILITIES_PATH, REPOS_PATH, PROJECTS_PATH,
  CALENDAR_PATH, REQUESTS_PATH, SECRETS_PATH, CONVERSATIONS_PATH, USERS_PATH,
  LLM_REGISTRY_PATH, PROVIDERS_PATH, TUNNEL_STATE_PATH, TUNNEL_AUTH_KEYS,
  TUNNEL_PORT_START, SBOM_DIR, SLACK_SIGNING_SECRET, SLACK_BOT_TOKEN, SLACK_API,
  START_TIME, STALE_THRESHOLDS,
  // mutable collections
  heartbeats, heartbeatHistory, cronStatus, providerHealth, geekSseClients,
  bootstrapTokens,
  // bus state
  _busSSEClients, _busPresence, _busAcks, _busDeadLetters,
  get _busSeq() { return _busSeq; },
  set _busSeq(v) { _busSeq = v; },
  // helpers
  json, readBody, isAuthed, isAdminAuthed,
  readQueue, writeQueue, withQueueLock,
  readAgents, writeAgents,
  readCapabilities, writeCapabilities,
  readProjects, writeProjects, buildProjectFromRepo, repoOwnershipSummary, projectUrl,
  readCalendar, writeCalendar,
  readRequests, writeRequests,
  readConversations, writeConversations,
  readUsers, writeUsers,
  readSecrets, writeSecrets,
  readJsonFile, writeJsonFile,
  getServicesStatus, SERVICES_CATALOG,
  getPump, getBrain,
  saveBootstrapTokens,
  broadcastGeekEvent,
  _busAppend, _busReadMessages,
  slackPost, verifySlackSignature, readRawBody,
  setSlackChannelMeta, formatQueueSummary, formatAgentStatus,
  notifyJkhCompletion, fanoutToProjectChannels,
  llmRegistry,
  // HTML helpers
  projectsListHtml, projectDetailHtml, servicesHtml, timelineHtml, packagesHtml, playgroundHtml, dashboardHtml,
  // lesson/vector/memory functions
  learnLesson, queryLessons, queryAllLessons, formatLessonsForContext,
  getTrendingLessons, formatTrendingForHeartbeat, getHeartbeatContext,
  receiveLessonFromBus,
  generateIdea,
  issuesModule,
  vectorUpsert, vectorSearch, vectorSearchAll, collectionStats,
  channelMemoryIngest, channelMemoryRecall,
};

// ── Create router and register all route modules ───────────────────────────
const app = createRouter();
registerUI(app, state);        // includes GET / redirect and onboard — must be first for proxy catch-all
registerBus(app, state);
registerAgentOS(app, state);
registerMemory(app, state);
registerServices(app, state);
registerProjects(app, state);
registerQueue(app, state);
registerAgents(app, state);
registerSetup(app, state);

// ── Request handler ────────────────────────────────────────────────────────
async function handleRequest(req, res) {
  const url    = new URL(req.url, 'http://localhost');
  const path   = url.pathname;
  const method = req.method;

  // CORS preflight
  if (method === 'OPTIONS') {
    res.writeHead(204, {
      'Access-Control-Allow-Origin':  '*',
      'Access-Control-Allow-Headers': 'Authorization, Content-Type',
      'Access-Control-Allow-Methods': 'GET, POST, PATCH, DELETE, OPTIONS',
    });
    return res.end();
  }

  try {
    const handled = await app.dispatch(req, res, url, method, path);
    if (handled === false) {
      json(res, 404, { error: 'Not found', path });
    }
  } catch (e) {
    console.error('[rcc-api] Unhandled error:', e.message, e.stack);
    try { json(res, 500, { error: 'Internal server error', message: e.message }); } catch {}
  }
}

// ── Start server ───────────────────────────────────────────────────────────
export function startServer(port = PORT) {
  const server = createServer(handleRequest);
  server.listen(port, '0.0.0.0', () => {
    console.log(`[rcc-api] 🐿️ RCC API running on http://0.0.0.0:${port}`);
    console.log(`[rcc-api] Auth: ${AUTH_TOKENS.size > 0 ? `${AUTH_TOKENS.size} token(s) configured` : 'OPEN (no tokens set)'}`);
  });

  // ── authorized_keys sanitize scan ──────────────────────────────────────
  // Warn about bare pubkeys that lack restrict/permitopen guards.
  // These were likely written out-of-band and may give the tunnel user
  // broader SSH access than intended.
  setImmediate(async () => {
    try {
      const akContent = await readFile(TUNNEL_AUTH_KEYS, 'utf8');
      const bareLines = akContent.split('\n').filter(line => {
        const t = line.trim();
        return t.length > 0 && !t.startsWith('#') && /^(ssh-|ecdsa-sha2)/.test(t);
      });
      if (bareLines.length > 0) {
        console.warn(`[rcc-api] ⚠️  authorized_keys has ${bareLines.length} bare pubkey(s) without restrict/permitopen guards:`);
        bareLines.forEach(l => console.warn(`[rcc-api]    ${l.slice(0, 80)}...`));
        console.warn(`[rcc-api]    These should be prefixed with: restrict,port-forwarding,permitopen="localhost:<PORT>"`);
      }
    } catch { /* file may not exist yet — that's fine */ }
  });

  return server;
}

// ── Reload persisted agent tokens into AUTH_TOKENS on startup ─────────────
// Without this, agent tokens from agents.json are lost on every RCC restart,
// causing Boris/RTX/etc to 401 and appear dead.
async function reloadAgentTokens() {
  try {
    const agents = await readAgents();
    const agentMap = typeof agents === 'object' && !Array.isArray(agents) ? agents : {};
    let reloaded = 0;
    for (const [, agent] of Object.entries(agentMap)) {
      if (agent.token) { AUTH_TOKENS.add(agent.token); reloaded++; }
    }
    if (reloaded > 0) console.log(`[rcc-api] Reloaded ${reloaded} agent token(s) from agents.json`);
  } catch (e) {
    console.warn('[rcc-api] Could not reload agent tokens:', e.message);
  }
}

// ── LLM Registry init ──────────────────────────────────────────────────────
async function initLLMRegistry() {
  const p = new URL(LLM_REGISTRY_PATH, import.meta.url).pathname;
  llmRegistry.configure({ path: p });
  await llmRegistry.load(p);
  console.log('[rcc-api] LLM registry initialized');
}

// ── Stale claim auto-expiry ────────────────────────────────────────────────
// Run every 5 minutes to reset abandoned in-progress items back to pending.
setInterval(async () => {
  try {
    await withQueueLock(async () => {
      const q = await readQueue();
      const now = Date.now();
      let reset = 0;
      for (const item of (q.items || [])) {
        if (item.status !== 'in-progress' || !item.claimedAt) continue;
        const threshold = STALE_THRESHOLDS[item.preferred_executor] || STALE_THRESHOLDS.default;
        if ((now - new Date(item.claimedAt).getTime()) > threshold) {
          const prev = item.claimedBy;
          item.claimedBy = null;
          item.claimedAt = null;
          item.status = 'pending';
          if (!item.journal) item.journal = [];
          item.journal.push({ ts: new Date().toISOString(), author: 'rcc', type: 'stale-reset', text: `Auto-reset stale claim (was ${prev})` });
          reset++;
        }
      }
      if (reset > 0) {
        await writeQueue(q);
        console.log(`[rcc-api] Auto-expired ${reset} stale claim(s)`);
      }
    });
  } catch (e) {
    console.error('[rcc-api] Stale expiry error:', e.message);
  }
}, 5 * 60 * 1000).unref();

if (process.argv[1] === new URL(import.meta.url).pathname) {
  warmHeartbeatsFromHistory()
    .then(() => reloadAgentTokens())
    .then(() => initLLMRegistry())
    .then(() => {
      startServer();
      // Start periodic GitHub issues sync (every 15 min)
      issuesModule.startPeriodicSync(15 * 60 * 1000);
      // Background: index existing pending queue items into rcc_queue_dedup (once at startup, best-effort)
      setTimeout(() => indexPendingQueueItems(), 30_000);
    });
  process.on('SIGTERM', () => process.exit(0));
  process.on('SIGINT',  () => process.exit(0));
}
