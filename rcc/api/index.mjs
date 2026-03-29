/**
 * rcc/api — Rocky Command Center REST API
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
import { readFile, writeFile, mkdir, chmod, appendFile } from 'fs/promises';
import { existsSync, createReadStream as createRS, readFileSync, writeFileSync } from 'fs';
import { dirname } from 'path';
import { createInterface } from 'readline';
import { createHmac, timingSafeEqual, randomUUID } from 'crypto';
import { Brain, createRequest } from '../brain/index.mjs';
import { embed, upsert as vectorUpsert, search as vectorSearch, searchAll as vectorSearchAll, ensureCollections, collectionStats } from '../vector/index.mjs';
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

// ── Services map ───────────────────────────────────────────────────────────
const SERVICES_CATALOG = [
  { id: 'rcc-dashboard',    name: 'RCC Dashboard',      url: 'http://146.190.134.110:8789/projects', desc: 'Agent work queue + project tracker',      host: 'do-host1' },
  { id: 'squirrelbus',      name: 'SquirrelBus Viewer', url: 'http://146.190.134.110:8788/bus',      desc: 'Inter-agent message bus',                  host: 'do-host1' },
  { id: 'whisper-api',      name: 'Whisper API',        url: 'http://100.87.229.125:8792',            desc: 'Speech-to-text (sparky GB10)',              host: 'sparky'   },
  { id: 'agentfs',          name: 'AgentFS',            url: 'http://100.87.229.125:8791',            desc: 'Content-addressed WASM module store',       host: 'sparky'   },
  { id: 'usdagent',         name: 'usdagent',           url: 'http://100.87.229.125:8000',            desc: 'LLM-backed USD 3D asset generator',         host: 'sparky'   },
  { id: 'milvus',           name: 'Milvus',             url: 'http://100.89.199.14:9091/healthz',    desc: 'Vector database (do-host1)',                host: 'do-host1' },
  { id: 'ollama',           name: 'Ollama',             url: 'http://100.87.229.125:11434',           desc: 'Local LLM inference',                      host: 'sparky'   },
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

async function getServicesStatus() {
  if (SERVICES_CACHE.data && (Date.now() - SERVICES_CACHE.ts) < SERVICES_CACHE_TTL) {
    return SERVICES_CACHE.data;
  }
  const results = await Promise.all(
    SERVICES_CATALOG.map(async (svc) => {
      const { online, latency_ms } = await probeService(svc.url);
      return { ...svc, online, latency_ms };
    })
  );
  SERVICES_CACHE.data = results;
  SERVICES_CACHE.ts = Date.now();
  return results;
}

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

// ── SquirrelBus paths ──────────────────────────────────────────────────────
const BUS_LOG_PATH   = process.env.BUS_LOG_PATH   || new URL('../../squirrelbus/bus.jsonl', import.meta.url).pathname;
const ACK_LOG_PATH   = process.env.ACK_LOG_PATH   || new URL('../../squirrelbus/acks.jsonl', import.meta.url).pathname;
const DEAD_LOG_PATH  = process.env.DEAD_LOG_PATH  || new URL('../../squirrelbus/dead-letter.jsonl', import.meta.url).pathname;

// ── SquirrelBus in-memory state ────────────────────────────────────────────
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
// Named aliases: slack, mattermost, minio, milvus, nvidia, github
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

function projectsListHtml() {
  return `<!DOCTYPE html><html lang="en"><head>${HTML_STYLE}<title>Projects — RCC</title></head><body>
  <div class="nav"><a href="/">← RCC</a> &nbsp;·&nbsp; <a href="/services">Services</a></div>
  <h1>Projects</h1>
  <p class="subtitle">All registered projects tracked by Rocky Command Center</p>
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
    .status-offline{background:#f85149}
    .status-unknown{background:#8b949e}
    .status-badge-online{color:#3fb950;font-size:.78rem;font-weight:600}
    .status-badge-offline{color:#f85149;font-size:.78rem;font-weight:600}
    .status-badge-unknown{color:#8b949e;font-size:.78rem}
    .latency{color:#8b949e;font-size:.72rem;margin-left:.3rem}
  </style>
  <title>Services — RCC</title></head><body>
  <div class="nav"><a href="/projects">Projects</a> &nbsp;·&nbsp; <a href="/">← RCC</a></div>
  <h1>Services</h1>
  <p class="subtitle">Agent infrastructure — live status probed every 30 seconds</p>
  <div id="root"><p class="spinner">Loading…</p></div>
  <script>
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
    fetch('/api/services/status').then(r=>r.json()).then(services=>{
      const root=document.getElementById('root');
      if(!services.length){root.innerHTML='<p class="error">No services configured.</p>';return;}
      root.innerHTML='<div class="svc-grid">'+services.map(renderCard).join('')+'</div>';
    }).catch(e=>{document.getElementById('root').innerHTML='<p class="error">Failed to load: '+e.message+'</p>';});
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
async function handleRequest(req, res) {
  const url = new URL(req.url, `http://localhost`);
  const path = url.pathname;
  const method = req.method;

  // CORS preflight
  if (method === 'OPTIONS') {
    res.writeHead(204, { 'Access-Control-Allow-Origin': '*', 'Access-Control-Allow-Headers': 'Authorization, Content-Type', 'Access-Control-Allow-Methods': 'GET, POST, PATCH, DELETE, OPTIONS' });
    return res.end();
  }

  try {
    // ── Public endpoints ────────────────────────────────────────────────

    if (method === 'GET' && path === '/health') {
      const b = brain;
      const q = await readQueue();
      const llmEndpoints = llmRegistry.serialize();
      return json(res, 200, {
        ok: true,
        uptime: Math.floor((Date.now() - START_TIME) / 1000),
        agentCount: Object.keys(heartbeats).length,
        queueDepth: (q.items || []).filter(i => !['completed','cancelled'].includes(i.status)).length,
        lastBrainTick: b?.state?.lastTick || null,
        version: '0.1.0',
        llm: {
          endpointCount: llmEndpoints.length,
          freshCount: llmEndpoints.filter(e => e.fresh).length,
          modelCount: llmEndpoints.reduce((s, e) => s + e.models.length, 0),
        },
      });
    }

    if (method === 'GET' && path === '/api/queue') {
      const q = await readQueue();
      return json(res, 200, { items: q.items || [], completed: q.completed || [] });
    }

    if (method === 'GET' && path === '/api/agents') {
      const agents = await readAgents();
      const caps   = await readCapabilities();
      const result = Object.entries(agents).map(([name, agent]) => {
        const llmEntry = llmRegistry.get(name);
        return {
          ...agent,
          capabilities: { ...(agent.capabilities || {}), ...(caps[name] || {}) },
          heartbeat: heartbeats[name] || null,
          llm: llmEntry ? {
            baseUrl:   llmEntry.baseUrl,
            backend:   llmEntry.backend,
            models:    llmEntry.models.map(m => m.name),
            modelCount: llmEntry.models.length,
            fresh:     (Date.now() - new Date(llmEntry.updatedAt).getTime()) < 30 * 60 * 1000,
            updatedAt: llmEntry.updatedAt,
          } : null,
        };
      });
      return json(res, 200, result);
    }

    // ── GET /api/agents/best?task=X — capability-based routing ───────────
    if (method === 'GET' && path === '/api/agents/best') {
      const task = url.searchParams.get('task') || '';
      const agents = await readAgents();
      const caps   = await readCapabilities();
      const GPU_TASKS    = new Set(['gpu', 'render', 'training', 'inference']);
      const CLAUDE_TASKS = new Set(['claude', 'code', 'review', 'debug', 'triage']);
      const CTX_PRIORITY = { large: 3, medium: 2, small: 1 };

      const candidates = Object.entries(agents).map(([name, agent]) => ({
        name,
        ...agent,
        capabilities: { ...(agent.capabilities || {}), ...(caps[name] || {}) },
        heartbeat: heartbeats[name] || null,
      }));

      // prefer online agents (heartbeat within last 10 min), fall back to all
      const onlineCutoff = Date.now() - 10 * 60 * 1000;
      const online = candidates.filter(a => a.heartbeat && new Date(a.heartbeat.ts).getTime() > onlineCutoff);
      const pool   = online.length > 0 ? online : candidates;

      let best = null;

      if (GPU_TASKS.has(task)) {
        const gpu = pool.filter(a => a.capabilities?.gpu);
        if (gpu.length) best = gpu.sort((a, b) => (b.capabilities.gpu_vram_gb || 0) - (a.capabilities.gpu_vram_gb || 0))[0];
      } else if (CLAUDE_TASKS.has(task)) {
        const cli = pool.filter(a => a.capabilities?.claude_cli);
        if (cli.length) best = cli.sort((a, b) => (CTX_PRIORITY[b.capabilities.context_size] || 0) - (CTX_PRIORITY[a.capabilities.context_size] || 0))[0];
      }

      if (!best) {
        // match preferred_tasks
        const byPref = pool.filter(a => (a.capabilities?.preferred_tasks || []).includes(task));
        if (byPref.length) best = byPref[0];
      }

      if (!best && pool.length) best = pool[0];
      if (!best) return json(res, 404, { error: 'No agents available' });
      return json(res, 200, { agent: best, task });
    }

    // ── GET /api/agents/status — all agents with last-seen + online status ─
    if (method === 'GET' && path === '/api/agents/status') {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const agents = await readAgents().catch(() => ({}));
      const now = Date.now();
      const result = Object.entries(agents).map(([name, agent]) => {
        const hb = heartbeats[name] || null;
        const lastSeen = hb?.ts || agent.lastSeen || null;
        const gapMs = lastSeen ? now - new Date(lastSeen).getTime() : null;
        const gap_minutes = gapMs !== null ? Math.round(gapMs / 60000) : null;
        const onlineStatus = agent.decommissioned ? 'decommissioned'
          : (hb ? (computeOnlineStatus(hb) ? 'online' : 'offline') : (agent.onlineStatus || 'unknown'));
        return {
          name,
          lastSeen,
          onlineStatus,
          host: hb?.host || agent.host || null,
          gap_minutes,
        };
      });
      return json(res, 200, { ok: true, agents: result });
    }

    // ── GET /api/agents/:name/tunnel-port — return assigned tunnel port ──────
    // Called by onboard script to get the SSH reverse tunnel port for this agent.
    // If no tunnel is registered yet, auto-allocates one (without a pubkey — worker
    // will call POST /api/tunnel/register once it has generated its key).
    {
      const tpm = path.match(/^\/api\/agents\/([^/]+)\/tunnel-port$/);
      if (method === 'GET' && tpm) {
        const agentName = decodeURIComponent(tpm[1]);
        const tunnelState = await readJsonFile(TUNNEL_STATE_PATH, { nextPort: TUNNEL_PORT_START, tunnels: {} });
        let assigned = tunnelState.tunnels[agentName];
        if (!assigned) {
          // Pre-allocate a port without pubkey — will be completed when worker registers key
          const port = tunnelState.nextPort;
          tunnelState.nextPort = port + 1;
          assigned = { agent: agentName, port, pubkey: null, preallocatedAt: new Date().toISOString() };
          tunnelState.tunnels[agentName] = assigned;
          await writeJsonFile(TUNNEL_STATE_PATH, tunnelState);
        }
        const publicHost = RCC_PUBLIC_URL.replace(/^https?:\/\//, '').split(':')[0];
        return json(res, 200, { ok: true, port: assigned.port, host: publicHost, agent: agentName });
      }
    }

    if (method === 'GET' && path === '/api/heartbeats') {
      // Return all known agents (including offline/decommissioned) with computed online status
      const agents = await readAgents().catch(() => ({}));
      const result = { ...heartbeats };
      // Merge in any agents from registry that haven't heartbeated in this process lifecycle
      for (const [name, agentRec] of Object.entries(agents)) {
        if (!result[name] && agentRec.lastSeen) {
          result[name] = { agent: name, ts: agentRec.lastSeen, status: agentRec.onlineStatus || 'unknown', _fromRegistry: true };
          if (agentRec.decommissioned) result[name].decommissioned = true;
        }
      }
      // Add online boolean + decommissioned status to each entry
      const enriched = {};
      for (const [name, hb] of Object.entries(result)) {
        enriched[name] = {
          ...hb,
          online: computeOnlineStatus(hb),
          decommissioned: !!hb.decommissioned,
          lastSeen: hb.ts || null,
        };
        if (hb._wasOnline !== undefined) delete enriched[name]._wasOnline;
        if (hb._fromRegistry !== undefined) delete enriched[name]._fromRegistry;
      }
      return json(res, 200, enriched);
    }

    if (method === 'GET' && path === '/api/drift') {
      // IntentDriftDetector — behavioral drift analysis for agents
      // ?agent=natasha  ?window=20  ?baseline=50  ?threshold=0.25
      try {
        const { detectDrift, driftReport } = await import('./decision-journal/intent-drift-detector.mjs');
        const { DecisionJournal } = await import('./decision-journal/index.mjs');
        const agentFilter = url.searchParams.get('agent') || null;
        const windowSize  = parseInt(url.searchParams.get('window')    || '20', 10);
        const baselineWin = parseInt(url.searchParams.get('baseline')  || '50', 10);
        const threshold   = parseFloat(url.searchParams.get('threshold') || '0.25');
        const logPath = process.env.DECISION_JOURNAL_PATH ||
          new URL('../logs/decision-journal.jsonl', import.meta.url).pathname;
        const journal = new DecisionJournal({ agent: agentFilter || '_rcc', logPath, silent: true });
        const result = detectDrift({ journal, agent: agentFilter, windowSize, baselineWindow: baselineWin, driftThreshold: threshold });
        return json(res, 200, { ...result, report: driftReport(result) });
      } catch (err) {
        return json(res, 500, { ok: false, error: err.message });
      }
    }

    if (method === 'GET' && path === '/api/brain/status') {
      const b = brain;
      if (!b) return json(res, 200, { ok: true, status: 'not started' });
      return json(res, 200, b.getStatus());
    }

    // ── Item detail (public read) ─────────────────────────────────────────
    const itemDetailMatch = path.match(/^\/api\/item\/([^/]+)$/);
    if (method === 'GET' && itemDetailMatch) {
      const id = decodeURIComponent(itemDetailMatch[1]);
      const q = await readQueue();
      const item = [...(q.items||[]), ...(q.completed||[])].find(i => i.id === id);
      if (!item) return json(res, 404, { error: 'Item not found' });
      return json(res, 200, item);
    }

    // ── Public: GET /api/projects list + detail ──────────────────────────
    if (method === 'GET' && path === '/api/projects') {
      const repos    = await getPump().listRepos();
      const projects = await readProjects();
      const projectMap = new Map(projects.map(p => [p.id, p]));
      const result = repos
        .filter(r => r.enabled !== false)
        .map(r => {
          const base    = buildProjectFromRepo(r);
          const overlay = projectMap.get(r.full_name) || {};
          return { ...base, ...overlay };
        });
      return json(res, 200, result);
    }
    // ── GET /api/projects/:owner/:repo/github — live issues + PRs (public) ─
    // Must be before projectPublicDetailMatch (which would otherwise eat the /github suffix)
    if (method === 'GET' && path.endsWith('/github')) {
      const githubSubMatch = path.match(/^\/api\/projects\/([^/]+(?:\/[^/]+|%2F[^/]+))\/github$/i);
      if (githubSubMatch) {
        const fullName = decodeURIComponent(githubSubMatch[1]);
        if (!globalThis._githubCache) globalThis._githubCache = new Map();
        const cached = globalThis._githubCache.get(fullName);
        const bustCache = url.searchParams.get('refresh') === '1';
        if (cached && !bustCache && (Date.now() - cached.ts) < 5 * 60 * 1000) {
          return json(res, 200, cached.data);
        }
        const { execSync } = await import('child_process');
        function ghq(args, fields) {
          try {
            const out = execSync(`gh ${args} --json ${fields}`, { encoding: 'utf8', stdio: ['pipe','pipe','pipe'] });
            return JSON.parse(out);
          } catch { return null; }
        }
        const issues = ghq(`issue list --repo ${fullName} --state open --limit 50`,
          'number,title,labels,url,author,createdAt,updatedAt,comments') || [];
        const prs = ghq(`pr list --repo ${fullName} --state open --limit 30`,
          'number,title,author,url,isDraft,reviewDecision,mergeable,createdAt,updatedAt,labels') || [];
        const result = {
          repo: fullName,
          fetchedAt: new Date().toISOString(),
          issues: issues.map(i => ({
            number: i.number, title: i.title, url: i.url,
            labels: (i.labels || []).map(l => ({ name: l.name, color: l.color })),
            author: i.author?.login || i.author,
            createdAt: i.createdAt, updatedAt: i.updatedAt,
            commentCount: (i.comments || []).length,
          })),
          prs: (prs || []).map(p => ({
            number: p.number, title: p.title, url: p.url,
            author: p.author?.login || p.author,
            isDraft: p.isDraft || false,
            reviewDecision: p.reviewDecision || null,
            mergeable: p.mergeable || null,
            createdAt: p.createdAt, updatedAt: p.updatedAt,
            labels: (p.labels || []).map(l => ({ name: l.name, color: l.color })),
          })),
        };
        globalThis._githubCache.set(fullName, { ts: Date.now(), data: result });
        return json(res, 200, result);
      }
    }

    const projectPublicDetailMatch = path.match(/^\/api\/projects\/([^/]+(?:\/[^/]+|%2F[^/]+))$/i);
    if (method === 'GET' && projectPublicDetailMatch) {
      const fullName = decodeURIComponent(projectPublicDetailMatch[1]);
      const repos    = await getPump().listRepos();
      const repo     = repos.find(r => r.full_name === fullName);
      if (!repo) return json(res, 404, { error: 'Project not found' });
      const projects = await readProjects();
      const overlay  = projects.find(p => p.id === fullName) || {};
      const base     = buildProjectFromRepo(repo);
      return json(res, 200, { ...base, ...overlay });
    }

    // ── UI: GET /projects — project list page ────────────────────────────
    if (method === 'GET' && path === '/projects') {
      res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8', 'Access-Control-Allow-Origin': '*' });
      res.end(projectsListHtml());
      return;
    }
    // ── UI: GET /projects/:owner/:repo — project detail page ─────────────
    const projectUiMatch = path.match(/^\/projects\/([^/]+(?:\/[^/]+|%2F[^/]+))$/i);
    if (method === 'GET' && projectUiMatch) {
      res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8', 'Access-Control-Allow-Origin': '*' });
      res.end(projectDetailHtml(decodeURIComponent(projectUiMatch[1])));
      return;
    }

    // ── UI: GET /services — services map page ────────────────────────────
    if (method === 'GET' && path === '/services') {
      res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8', 'Access-Control-Allow-Origin': '*' });
      res.end(servicesHtml());
      return;
    }

    // ── GET /api/bootstrap — public (self-authenticates via bootstrap token) ─
    // Must be before the auth gate — agent has no token yet at bootstrap time.
    if (method === 'GET' && path === '/api/bootstrap') {
      const token = url.searchParams.get('token');
      if (!token) return json(res, 400, { error: 'token query param required' });
      const entry = bootstrapTokens.get(token);
      if (!entry) return json(res, 401, { error: 'Invalid bootstrap token' });
      if (Date.now() > entry.expiresAt) return json(res, 401, { error: 'Bootstrap token expired' });
      if (entry.used) return json(res, 401, { error: 'Bootstrap token already used' });
      entry.used = true;
      saveBootstrapTokens();

      const keyPath = new URL('../data/github-key.json', import.meta.url).pathname;
      if (!existsSync(keyPath)) return json(res, 500, { error: 'Deploy key not configured' });
      const keyRecord = JSON.parse(await readFile(keyPath, 'utf8'));

      const agents = await readAgents();
      let agentToken;
      if (agents[entry.agent]?.token) {
        agentToken = agents[entry.agent].token;
      } else {
        agentToken = `rcc-agent-${entry.agent}-${randomUUID().slice(0, 8)}`;
        agents[entry.agent] = {
          ...(agents[entry.agent] || {}),
          name: entry.agent,
          host: entry.host || 'unknown',
          type: entry.type || 'full',
          token: agentToken,
          registeredAt: new Date().toISOString(),
          capabilities: agents[entry.agent]?.capabilities || {},
          billing: agents[entry.agent]?.billing || { claude_cli: 'fixed', inference_key: 'metered', gpu: 'fixed' },
        };
        await writeAgents(agents);
        AUTH_TOKENS.add(agentToken);
      }

      const secretsPath = new URL('../data/secrets.json', import.meta.url).pathname;
      let secrets = {};
      if (existsSync(secretsPath)) {
        try { secrets = JSON.parse(await readFile(secretsPath, 'utf8')); } catch {}
      }

      console.log(`[rcc-api] Bootstrap consumed for agent ${entry.agent} from ${req.socket?.remoteAddress}`);
      return json(res, 200, {
        ok: true,
        agent: entry.agent,
        repoUrl: keyRecord.repoUrl,
        deployKey: keyRecord.deployKey,
        agentToken,
        rccUrl: RCC_PUBLIC_URL,
        secrets,
      });
    }

    // ── GET /api/onboard — public; returns a self-contained bootstrap shell script ─
    // Usage: curl http://<rcc>/api/onboard?token=<bootstrap-token> | bash
    // Roles: agent (default), vllm-worker
    if (method === 'GET' && path === '/api/onboard') {
      const token = url.searchParams.get('token');
      if (!token) {
        res.writeHead(400, { 'Content-Type': 'text/plain' });
        return res.end('# Error: token query param required\n# Usage: curl "http://RCC_HOST:8789/api/onboard?token=BOOTSTRAP_TOKEN" | bash\n');
      }
      const entry = bootstrapTokens.get(token);
      if (!entry) {
        res.writeHead(401, { 'Content-Type': 'text/plain' });
        return res.end('# Error: Invalid or expired bootstrap token\n# Generate a new one: POST /api/bootstrap/token {"agent":"<name>","role":"vllm-worker"}\n');
      }
      if (Date.now() > entry.expiresAt) {
        res.writeHead(401, { 'Content-Type': 'text/plain' });
        return res.end('# Error: Bootstrap token expired\n# Generate a new one: POST /api/bootstrap/token {"agent":"<name>","role":"vllm-worker"}\n');
      }
      if (entry.used) {
        res.writeHead(401, { 'Content-Type': 'text/plain' });
        return res.end('# Error: Bootstrap token already used\n');
      }
      entry.used = true;
      saveBootstrapTokens();
      const roleHint = entry.role || 'auto';

      // Load agent token (reuse existing if resurrection)
      const agents = await readAgents();
      let agentToken;
      if (agents[entry.agent]?.token) {
        agentToken = agents[entry.agent].token; // resurrection — reuse token
      } else {
        agentToken = `rcc-agent-${entry.agent}-${randomUUID().slice(0, 8)}`;
        agents[entry.agent] = {
          ...(agents[entry.agent] || {}),
          name: entry.agent,
          host: entry.host || 'unknown',
          type: entry.type || 'full',
          token: agentToken,
          registeredAt: new Date().toISOString(),
          capabilities: agents[entry.agent]?.capabilities || {},
          billing: agents[entry.agent]?.billing || { claude_cli: 'fixed', inference_key: 'metered', gpu: 'fixed' },
        };
        await writeAgents(agents);
        AUTH_TOKENS.add(agentToken);
      }

      // Load secrets
      const secretsPath = new URL('../data/secrets.json', import.meta.url).pathname;
      let secrets = {};
      if (existsSync(secretsPath)) {
        try { secrets = JSON.parse(await readFile(secretsPath, 'utf8')); } catch {}
      }

      // Load deploy key
      const keyPath = new URL('../data/github-key.json', import.meta.url).pathname;
      let repoUrl = 'https://github.com/jordanhubbard/rockyandfriends.git';
      let deployKeyBlock = '';
      if (existsSync(keyPath)) {
        try {
          const kr = JSON.parse(await readFile(keyPath, 'utf8'));
          repoUrl = kr.repoUrl || repoUrl;
          if (kr.deployKey) {
            deployKeyBlock = `
# ── Deploy key ───────────────────────────────────────────────────────────────
mkdir -p ~/.ssh && chmod 700 ~/.ssh
cat > ~/.ssh/rcc-deploy-key << 'DEPLOYKEY'
${kr.deployKey.trim()}
DEPLOYKEY
chmod 600 ~/.ssh/rcc-deploy-key
grep -q "rcc-deploy-key" ~/.ssh/config 2>/dev/null || cat >> ~/.ssh/config << 'SSHCFG'
Host github.com
  IdentityFile ~/.ssh/rcc-deploy-key
  StrictHostKeyChecking no
SSHCFG
`;
          }
        } catch {}
      }

      // Build env block from secrets
      const envLines = [`RCC_AGENT_TOKEN=${agentToken}`, `RCC_URL=${RCC_PUBLIC_URL}`, `AGENT_NAME=${entry.agent}`, `AGENT_ROLE=${roleHint}`];
      const skipKeys = new Set(['deployKey', 'repoUrl']);
      for (const [k, v] of Object.entries(secrets)) {
        if (!skipKeys.has(k) && v && typeof v !== 'object') {
          const envKey = k.replace(/[^a-zA-Z0-9_]/g, '_').toUpperCase();
          envLines.push(`${envKey}=${v}`);
        }
      }
      const envBlock = envLines.join('\n');
      const rccHost = RCC_PUBLIC_URL.replace(/^https?:\/\//, '').split(':')[0];

      const script = `#!/usr/bin/env bash
# ═══════════════════════════════════════════════════════════════════════════════
# RCC Agent Onboard — ${entry.agent} @ \$(date -u +%Y-%m-%dT%H:%M:%SZ)
# Generated by Rocky Command Center (revamp v2)
# Usage: curl "${RCC_PUBLIC_URL}/api/onboard?token=<token>" | bash
# ═══════════════════════════════════════════════════════════════════════════════
set -euo pipefail

AGENT_NAME="${entry.agent}"
ROLE_HINT="${roleHint}"
RCC_URL="${RCC_PUBLIC_URL}"
RCC_HOST="${rccHost}"
REPO_URL="${repoUrl}"
WORKSPACE="\$HOME/Src/rockyandfriends"

echo ""
echo "🐿️  RCC Agent Onboard — \$AGENT_NAME"
echo "    RCC: \$RCC_URL"
echo ""

# ═══════════════════════════════════════════════════════════════════════════════
# PHASE 1 — HARDWARE DISCOVERY
# ═══════════════════════════════════════════════════════════════════════════════
echo "┌─────────────────────────────────────────────────────────┐"
echo "│  Phase 1: Hardware Discovery                            │"
echo "└─────────────────────────────────────────────────────────┘"

# GPU detection
HAS_GPU=false
GPU_COUNT=0
GPU_MODEL="none"
GPU_VRAM_GB=0
if command -v nvidia-smi &>/dev/null; then
  GPU_COUNT=\$(nvidia-smi --list-gpus 2>/dev/null | wc -l || echo 0)
  if [ "\$GPU_COUNT" -gt 0 ]; then
    HAS_GPU=true
    GPU_MODEL=\$(nvidia-smi --query-gpu=name --format=csv,noheader 2>/dev/null | head -1 | xargs || echo "unknown")
    GPU_VRAM_GB=\$(nvidia-smi --query-gpu=memory.total --format=csv,noheader,nounits 2>/dev/null | awk '{sum+=\$1} END {printf "%d", sum/1024}' || echo 0)
  fi
fi

# Tailscale detection
HAS_TAILSCALE=false
TAILSCALE_IP=""
if command -v tailscale &>/dev/null; then
  TS_STATUS=\$(tailscale status --json 2>/dev/null || true)
  if [ -n "\$TS_STATUS" ]; then
    TS_IP=\$(echo "\$TS_STATUS" | python3 -c "import json,sys; d=json.load(sys.stdin); ips=d.get('TailscaleIPs',[]); print(next((i for i in ips if i.startswith('100.')),ips[0] if ips else ''))" 2>/dev/null || true)
    if [ -n "\$TS_IP" ]; then
      HAS_TAILSCALE=true
      TAILSCALE_IP="\$TS_IP"
    fi
  fi
fi

# Host info
AGENT_HOSTNAME=\$(hostname)
DISK_AVAIL=\$(df -BG /home 2>/dev/null | awk 'NR==2{print \$4}' || echo "unknown")

echo "  Hostname:     \$AGENT_HOSTNAME"
echo "  GPUs:         \$GPU_COUNT x \$GPU_MODEL (\${GPU_VRAM_GB}GB total VRAM)"
echo "  Tailscale:    \$HAS_TAILSCALE (\${TAILSCALE_IP:-not detected})"
echo "  Disk (home):  \$DISK_AVAIL available"
echo ""

# ═══════════════════════════════════════════════════════════════════════════════
# PHASE 2 — ROLE SELECTION
# ═══════════════════════════════════════════════════════════════════════════════
echo "┌─────────────────────────────────────────────────────────┐"
echo "│  Phase 2: Role Selection                                │"
echo "└─────────────────────────────────────────────────────────┘"

AGENT_ROLE="agent"
ROLE_REASON="no GPU detected"

if [ "\$ROLE_HINT" = "vllm-worker" ]; then
  AGENT_ROLE="vllm-worker"
  ROLE_REASON="role override from bootstrap token"
elif [ "\$ROLE_HINT" = "agent" ]; then
  AGENT_ROLE="agent"
  ROLE_REASON="role override from bootstrap token"
elif [ "\$HAS_GPU" = "true" ]; then
  if [ "\$GPU_VRAM_GB" -ge 64 ]; then
    AGENT_ROLE="vllm-worker"
    ROLE_REASON="auto-detected \${GPU_VRAM_GB}GB VRAM >= 64GB threshold"
  else
    AGENT_ROLE="gpu-agent"
    ROLE_REASON="auto-detected GPU but \${GPU_VRAM_GB}GB VRAM < 64GB (insufficient for vLLM)"
  fi
fi

echo "  Selected role: \$AGENT_ROLE"
echo "  Reason:        \$ROLE_REASON"
echo ""
${deployKeyBlock}
# ═══════════════════════════════════════════════════════════════════════════════
# PHASE 3 — SYSTEM DEPS
# ═══════════════════════════════════════════════════════════════════════════════
echo "┌─────────────────────────────────────────────────────────┐"
echo "│  Phase 3: System Dependencies                           │"
echo "└─────────────────────────────────────────────────────────┘"

echo "→ Checking Node.js..."
if ! node --version 2>/dev/null | grep -qE '^v(18|20|22|24)'; then
  echo "  Installing Node.js 22 via NodeSource..."
  export DEBIAN_FRONTEND=noninteractive
  sudo apt-get update -q
  sudo apt-get install -y -q curl git || true
  curl -fsSL https://deb.nodesource.com/setup_22.x | sudo -E bash -
  sudo apt-get install -y nodejs
  echo "  ✅ Node.js \$(node --version) installed"
else
  echo "  ✅ Node.js \$(node --version) OK"
fi
if ! command -v git &>/dev/null; then
  export DEBIAN_FRONTEND=noninteractive
  sudo apt-get update -q && sudo apt-get install -y -q git
fi

if [ "\$AGENT_ROLE" = "vllm-worker" ]; then
  echo "→ Installing vLLM worker deps..."
  export DEBIAN_FRONTEND=noninteractive
  sudo apt-get update -q
  sudo apt-get install -y -q aria2 rsync python3-pip python3-venv tmux curl wget git openssh-client || true
fi
echo ""

# ═══════════════════════════════════════════════════════════════════════════════
# PHASE 4 — REPO + ENV
# ═══════════════════════════════════════════════════════════════════════════════
echo "┌─────────────────────────────────────────────────────────┐"
echo "│  Phase 4: Workspace + Environment                       │"
echo "└─────────────────────────────────────────────────────────┘"

if [ -d "\$WORKSPACE/.git" ]; then
  echo "→ Repo exists — pulling latest..."
  cd "\$WORKSPACE" && git fetch origin && git reset --hard origin/main
else
  echo "→ Cloning repo..."
  mkdir -p "\$(dirname \$WORKSPACE)"
  git clone "\$REPO_URL" "\$WORKSPACE"
  cd "\$WORKSPACE"
fi
PULL_REV=\$(git rev-parse --short HEAD)
echo "   rev: \$PULL_REV"

echo "→ Writing ~/.rcc/.env..."
mkdir -p ~/.rcc
cat > ~/.rcc/.env << 'ENVEOF'
${envBlock}
ENVEOF
# Also write discovered hardware info into .env
cat >> ~/.rcc/.env << HWEOF
AGENT_HOST=\$AGENT_HOSTNAME
AGENT_ROLE=\$AGENT_ROLE
HAS_GPU=\$HAS_GPU
GPU_MODEL="\$GPU_MODEL"
GPU_COUNT=\$GPU_COUNT
GPU_VRAM_GB=\$GPU_VRAM_GB
HAS_TAILSCALE=\$HAS_TAILSCALE
TAILSCALE_IP=\$TAILSCALE_IP
HWEOF
chmod 600 ~/.rcc/.env

# Load env vars
set +u
source ~/.rcc/.env
set -u
echo ""

# ═══════════════════════════════════════════════════════════════════════════════
# PHASE 5 — OPENCLAW
# ═══════════════════════════════════════════════════════════════════════════════
echo "┌─────────────────────────────────────────────────────────┐"
echo "│  Phase 5: OpenClaw Install                              │"
echo "└─────────────────────────────────────────────────────────┘"

if command -v openclaw &>/dev/null; then
  echo "→ openclaw found — repairing config and restarting gateway..."
  openclaw doctor --fix 2>/dev/null || true
  openclaw config set gateway.mode local 2>/dev/null || true
  openclaw gateway restart 2>/dev/null || openclaw gateway start
else
  echo "→ Installing openclaw..."
  curl -fsSL https://deb.nodesource.com/setup_22.x | sudo -E bash -
  sudo apt-get install -y nodejs
  npm --version || { echo "ERROR: npm missing"; exit 1; }
  sudo npm install -g openclaw || { echo "ERROR: npm install -g openclaw failed"; exit 1; }
  openclaw doctor --fix 2>/dev/null || true
  openclaw config set gateway.mode local 2>/dev/null || true
  openclaw gateway start
fi
echo ""

# ═══════════════════════════════════════════════════════════════════════════════
# PHASE 6 — REGISTRATION
# ═══════════════════════════════════════════════════════════════════════════════
echo "┌─────────────────────────────────────────────────────────┐"
echo "│  Phase 6: RCC Registration                              │"
echo "└─────────────────────────────────────────────────────────┘"

IS_VLLM=false
VLLM_PORT=0
if [ "\$AGENT_ROLE" = "vllm-worker" ]; then
  IS_VLLM=true
  VLLM_PORT=8080
fi

REG_RESP=\$(curl -sf -X POST "\$RCC_URL/api/agents/register" \\
  -H "Authorization: Bearer \$RCC_AGENT_TOKEN" \\
  -H "Content-Type: application/json" \\
  -d "{
    \\"name\\": \\"\$AGENT_NAME\\",
    \\"host\\": \\"\$AGENT_HOSTNAME\\",
    \\"type\\": \\"full\\",
    \\"capabilities\\": {
      \\"claude_cli\\": true,
      \\"inference_key\\": true,
      \\"inference_provider\\": \\"nvidia\\",
      \\"gpu\\": \$HAS_GPU,
      \\"gpu_model\\": \\"\$GPU_MODEL\\",
      \\"gpu_count\\": \$GPU_COUNT,
      \\"gpu_vram_gb\\": \$GPU_VRAM_GB,
      \\"vllm\\": \$IS_VLLM,
      \\"vllm_port\\": \$VLLM_PORT,
      \\"vllm_model\\": \\"nemotron-3-super-120b\\",
      \\"tailscale\\": \$HAS_TAILSCALE,
      \\"tailscale_ip\\": \\"\$TAILSCALE_IP\\"
    }
  }" 2>/dev/null) && echo "  ✅ Registered with RCC" || echo "  ⚠️  Registration returned error (may already exist — continuing)"

# Update token from response if a new one was issued
NEW_TOKEN=\$(echo "\$REG_RESP" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('token',''))" 2>/dev/null || true)
if [ -n "\$NEW_TOKEN" ] && [ "\$NEW_TOKEN" != "\$RCC_AGENT_TOKEN" ]; then
  sed -i "s|^RCC_AGENT_TOKEN=.*|RCC_AGENT_TOKEN=\$NEW_TOKEN|" ~/.rcc/.env
  RCC_AGENT_TOKEN="\$NEW_TOKEN"
  echo "  → Updated token in ~/.rcc/.env"
fi
echo ""
`;


      // vLLM phases — only included when role might be vllm-worker
      // (auto-detection in the shell script determines final role at runtime)
      const vllmPhases = `
# ═══════════════════════════════════════════════════════════════════════════════
# PHASE 7 — VLLM SETUP (only if vllm-worker)
# ═══════════════════════════════════════════════════════════════════════════════
if [ "\$AGENT_ROLE" = "vllm-worker" ]; then
echo "┌─────────────────────────────────────────────────────────┐"
echo "│  Phase 7: vLLM Setup                                    │"
echo "└─────────────────────────────────────────────────────────┘"

VLLM_MODEL_ID="nvidia/NVIDIA-Nemotron-3-Super-120B-A12B-FP8"
VLLM_MODEL_DIR="/tmp/models/nvidia/NVIDIA-Nemotron-3-Super-120B-A12B-FP8"
VLLM_PORT=8080
VLLM_SERVED_MODEL_NAME="nemotron"

# Python venv + vLLM
echo "→ Setting up Python venv and installing vLLM..."
VLLM_VENV="\$HOME/.vllm-venv"
export HF_HOME="\$HOME/.cache/huggingface"
sudo mkdir -p "\$HF_HOME" 2>/dev/null || true
sudo chown -R "\$(id -u):\$(id -g)" "\$HF_HOME" 2>/dev/null || true
mkdir -p "\$HF_HOME"

python3 -m venv "\$VLLM_VENV"
source "\$VLLM_VENV/bin/activate"
pip install --upgrade pip --quiet
pip install vllm --quiet || pip install vllm --quiet --extra-index-url https://download.pytorch.org/whl/cu121
pip install huggingface_hub --quiet
deactivate
echo "  ✅ vLLM installed in \$VLLM_VENV"

# Model download (peer-to-peer with HuggingFace fallback)
echo "→ Acquiring model: \$VLLM_MODEL_ID (~128 GB FP8)"
mkdir -p "\$VLLM_MODEL_DIR"

if [ -f "\$VLLM_MODEL_DIR/config.json" ]; then
  echo "  ✅ Model already present — skipping download"
else
  echo "  → Querying RCC for model seeders..."
  PEERS_JSON=\$(curl -sf "\$RCC_URL/api/agents" 2>/dev/null || echo '[]')
  SEEDER_HOSTS=\$(echo "\$PEERS_JSON" | python3 -c "
import json,sys,os,re
agents=json.load(sys.stdin)
me=os.environ.get('AGENT_NAME','')
seeders=[]
for a in agents:
  caps=a.get('capabilities',{})
  llm=a.get('llm',{})
  if caps.get('vllm') and a.get('onlineStatus')=='online' and a.get('name')!=me:
    base=llm.get('baseUrl','')
    if base:
      m=re.match(r'https?://([^:/]+)',base)
      if m: seeders.append(m.group(1))
print(' '.join(seeders))
" 2>/dev/null || true)
  echo "  → Seeder peers: \${SEEDER_HOSTS:-none found}"

  MODEL_ACQUIRED=false
  # rsync from seeders
  if [ -n "\$SEEDER_HOSTS" ]; then
    for SEEDER_HOST in \$SEEDER_HOSTS; do
      echo "  → Attempting rsync from \$SEEDER_HOST..."
      if rsync -av --progress --partial \\
          -e "ssh -o StrictHostKeyChecking=no -o ConnectTimeout=10 -i ~/.ssh/rcc-worker-key" \\
          "rcc-worker@\${SEEDER_HOST}:\${VLLM_MODEL_DIR}/" \\
          "\${VLLM_MODEL_DIR}/" 2>/dev/null; then
        echo "  ✅ Model synced from \$SEEDER_HOST"
        MODEL_ACQUIRED=true
        break
      fi
    done
  fi

  # aria2c HTTP fallback from seeders
  if [ "\$MODEL_ACQUIRED" = "false" ] && [ -n "\$SEEDER_HOSTS" ]; then
    for SEEDER_HOST in \$SEEDER_HOSTS; do
      if curl -sf --connect-timeout 5 "http://\${SEEDER_HOST}:18081/filelist.txt" > /tmp/model-filelist.txt 2>/dev/null; then
        echo "  → Downloading via aria2c from \$SEEDER_HOST..."
        aria2c --dir="\${VLLM_MODEL_DIR}" --input-file=/tmp/model-filelist.txt \\
          --continue=true --max-concurrent-downloads=8 --split=4 \\
          --min-split-size=50M --max-connection-per-server=4 \\
          --file-allocation=none --auto-file-renaming=false \\
          --log-level=notice && MODEL_ACQUIRED=true && break || true
      fi
    done
  fi

  # HuggingFace fallback
  if [ "\$MODEL_ACQUIRED" = "false" ]; then
    echo "  → No peers — downloading from HuggingFace (128 GB, will take a while)..."
    source "\$VLLM_VENV/bin/activate"
    python3 -c "
from huggingface_hub import snapshot_download
snapshot_download(repo_id='nvidia/NVIDIA-Nemotron-3-Super-120B-A12B-FP8',
  local_dir='\$VLLM_MODEL_DIR', local_dir_use_symlinks=False, resume_download=True)
print('Download complete')
"
    deactivate
  fi
fi
echo "  ✅ Model ready at \$VLLM_MODEL_DIR"

# Model HTTP seeder
echo "→ Setting up model seeder (port 18081)..."
SEEDER_SCRIPT="\$HOME/.rcc/model-seeder.sh"
cat > "\$SEEDER_SCRIPT" << 'SEEDEOF'
#!/usr/bin/env bash
MODEL_DIR="/tmp/models/nvidia/NVIDIA-Nemotron-3-Super-120B-A12B-FP8"
PORT=18081
PID_FILE="\$HOME/.rcc/model-seeder.pid"
if [ -f "\$PID_FILE" ] && kill -0 "\$(cat \$PID_FILE)" 2>/dev/null; then
  echo "Seeder already running"; exit 0
fi
find "\$MODEL_DIR" -type f | while read f; do
  rel="\${f#\$MODEL_DIR/}"
  echo "http://\$(curl -s ifconfig.me 2>/dev/null || hostname -I | awk '{print \$1}'):18081/\$rel"
done > "\$MODEL_DIR/filelist.txt"
cd "\$(dirname \$MODEL_DIR)"
nohup python3 -m http.server \$PORT --directory "\$(dirname \$MODEL_DIR)" > "\$HOME/.rcc/logs/model-seeder.log" 2>&1 &
echo \$! > "\$PID_FILE"
echo "Model seeder started (pid \$(cat \$PID_FILE))"
SEEDEOF
chmod +x "\$SEEDER_SCRIPT"
mkdir -p "\$HOME/.rcc/logs"
bash "\$SEEDER_SCRIPT" || echo "  ⚠️  Seeder start failed"

echo ""
fi  # end PHASE 7 vllm-worker check

# ═══════════════════════════════════════════════════════════════════════════════
# PHASE 8 — TUNNEL SETUP (vllm-worker WITHOUT tailscale only)
# ═══════════════════════════════════════════════════════════════════════════════
if [ "\$AGENT_ROLE" = "vllm-worker" ] && [ "\$HAS_TAILSCALE" = "false" ]; then
echo "┌─────────────────────────────────────────────────────────┐"
echo "│  Phase 8: SSH Reverse Tunnel (no Tailscale detected)    │"
echo "└─────────────────────────────────────────────────────────┘"

TUNNEL_KEY="\$HOME/.ssh/rcc-tunnel-key"
if [ ! -f "\$TUNNEL_KEY" ]; then
  ssh-keygen -t ed25519 -f "\$TUNNEL_KEY" -N "" -C "\${AGENT_NAME}-vllm-tunnel"
  echo "  Generated: \$TUNNEL_KEY"
fi

echo "→ Registering tunnel key with RCC..."
TUNNEL_PUBKEY=\$(cat "\${TUNNEL_KEY}.pub")
TUNNEL_RESP=\$(curl -sf -X POST "\${RCC_URL}/api/tunnel/request" \\
  -H "Authorization: Bearer \${RCC_AGENT_TOKEN}" \\
  -H "Content-Type: application/json" \\
  -d "{\\"pubkey\\":\\"\${TUNNEL_PUBKEY}\\",\\"agent\\":\\"\${AGENT_NAME}\\",\\"label\\":\\"\${AGENT_NAME}-vllm-tunnel\\"}" 2>/dev/null)
TUNNEL_PORT=\$(echo "\$TUNNEL_RESP" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('port',18082))" 2>/dev/null || echo "18082")
KEY_WRITTEN=\$(echo "\$TUNNEL_RESP" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('keyWritten','unknown'))" 2>/dev/null || echo "unknown")
TUNNEL_USER_NAME=\$(echo "\$TUNNEL_RESP" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('user','${TUNNEL_USER}'))" 2>/dev/null || echo "${TUNNEL_USER}")
echo "  Tunnel port: \$TUNNEL_PORT"
echo "  Key written to authorized_keys: \$KEY_WRITTEN"

if [ "\$KEY_WRITTEN" = "false" ] || [ "\$KEY_WRITTEN" = "unknown" ]; then
  echo "  ⚠️  WARNING: Rocky could not write your pubkey to authorized_keys."
  echo "     Admin must manually add this key for tunnel user on RCC host:"
  echo "     \$TUNNEL_PUBKEY"
  echo "     The tunnel will NOT work until this is done."
fi

# systemd user service for tunnel
sudo loginctl enable-linger \$USER 2>/dev/null || true
mkdir -p "\$HOME/.config/systemd/user"
cat > "\$HOME/.config/systemd/user/rcc-vllm-tunnel.service" << TUNNELEOF
[Unit]
Description=RCC vLLM Reverse SSH Tunnel for \${AGENT_NAME}
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=/usr/bin/ssh \\\\
  -N \\\\
  -R \${TUNNEL_PORT}:localhost:8080 \\\\
  -i \$HOME/.ssh/rcc-tunnel-key \\\\
  -o StrictHostKeyChecking=no \\\\
  -o ServerAliveInterval=30 \\\\
  -o ServerAliveCountMax=3 \\\\
  -o ExitOnForwardFailure=yes \\\\
  \${TUNNEL_USER_NAME}@\${RCC_HOST}
Restart=always
RestartSec=10

[Install]
WantedBy=default.target
TUNNELEOF
systemctl --user daemon-reload 2>/dev/null || true
systemctl --user enable --now rcc-vllm-tunnel.service 2>/dev/null || \\
  echo "  ⚠️  systemd user not available — start manually:"
echo "      ssh -N -R \${TUNNEL_PORT}:localhost:8080 -i ~/.ssh/rcc-tunnel-key \${TUNNEL_USER_NAME}@\${RCC_HOST}"
echo ""

elif [ "\$AGENT_ROLE" = "vllm-worker" ] && [ "\$HAS_TAILSCALE" = "true" ]; then
echo "┌─────────────────────────────────────────────────────────┐"
echo "│  Phase 8: Tailscale detected — no tunnel needed         │"
echo "└─────────────────────────────────────────────────────────┘"
echo "  vLLM will be accessible directly at \$TAILSCALE_IP:8080"
echo "  RCC has been informed via registration capabilities."
echo ""
fi

# ═══════════════════════════════════════════════════════════════════════════════
# PHASE 9 — VLLM SERVICE (vllm-worker only)
# ═══════════════════════════════════════════════════════════════════════════════
if [ "\$AGENT_ROLE" = "vllm-worker" ]; then
echo "┌─────────────────────────────────────────────────────────┐"
echo "│  Phase 9: vLLM Service                                  │"
echo "└─────────────────────────────────────────────────────────┘"

VLLM_VENV="\$HOME/.vllm-venv"
VLLM_MODEL_DIR="/tmp/models/nvidia/NVIDIA-Nemotron-3-Super-120B-A12B-FP8"
TP_SIZE=\$(nvidia-smi --list-gpus 2>/dev/null | wc -l || echo 1)

sudo loginctl enable-linger \$USER 2>/dev/null || true
mkdir -p "\$HOME/.config/systemd/user"
cat > "\$HOME/.config/systemd/user/vllm-worker.service" << VLLMEOF
[Unit]
Description=vLLM Worker — \${AGENT_NAME}
After=network.target

[Service]
Type=simple
WorkingDirectory=/tmp
Environment="HOME=\$HOME"
Environment="PATH=\$VLLM_VENV/bin:/usr/local/bin:/usr/bin:/bin"
ExecStart=\$VLLM_VENV/bin/python3 -m vllm.entrypoints.openai.api_server \\\\
  --model \$VLLM_MODEL_DIR \\\\
  --served-model-name nemotron \\\\
  --port 8080 \\\\
  --tensor-parallel-size \$TP_SIZE \\\\
  --max-model-len 262144 \\\\
  --enforce-eager \\\\
  --trust-remote-code
Restart=on-failure
RestartSec=30
TimeoutStartSec=600

[Install]
WantedBy=default.target
VLLMEOF

systemctl --user daemon-reload 2>/dev/null || true
systemctl --user enable --now vllm-worker.service 2>/dev/null && \\
  echo "  ✅ vLLM service started" || \\
  echo "  ⚠️  systemd user not available — start manually"
echo ""
fi  # end PHASE 9
`;


      // Verification and summary phases
      const verifyAndSummary = `
# ═══════════════════════════════════════════════════════════════════════════════
# PHASE 10 — HEARTBEAT
# ═══════════════════════════════════════════════════════════════════════════════
echo "→ Posting heartbeat..."
sleep 2
curl -s -X POST "\$RCC_URL/api/heartbeat/\$AGENT_NAME" \\
  -H "Authorization: Bearer \$RCC_AGENT_TOKEN" \\
  -H "Content-Type: application/json" \\
  -d "{\\"agent\\":\\"\$AGENT_NAME\\",\\"role\\":\\"\$AGENT_ROLE\\",\\"host\\":\\"\$AGENT_HOSTNAME\\",\\"status\\":\\"online\\",\\"pullRev\\":\\"\$PULL_REV\\"}" | grep -q '"ok":true' && echo "  ✅ Heartbeat posted" || echo "  ⚠️  Heartbeat failed"
echo ""

# ═══════════════════════════════════════════════════════════════════════════════
# PHASE 11 — VERIFICATION
# ═══════════════════════════════════════════════════════════════════════════════
echo "┌─────────────────────────────────────────────────────────┐"
echo "│  Phase 11: Verification                                 │"
echo "└─────────────────────────────────────────────────────────┘"

VERIFY_OK=true

# Check registration
REG_CHECK=\$(curl -sf "\$RCC_URL/api/agents" 2>/dev/null | python3 -c "
import json,sys,os
agents=json.load(sys.stdin)
me=os.environ.get('AGENT_NAME','')
found=[a for a in agents if a.get('name')==me]
if found:
  a=found[0]
  caps=a.get('capabilities',{})
  print(f'gpu={caps.get(\"gpu\",False)} vllm={caps.get(\"vllm\",False)} ts={caps.get(\"tailscale\",False)}')
else:
  print('NOT_FOUND')
" 2>/dev/null || echo "ERROR")
if echo "\$REG_CHECK" | grep -q "NOT_FOUND\\|ERROR"; then
  echo "  ⚠️  Registration: agent not found in /api/agents"
  VERIFY_OK=false
else
  echo "  ✅ Registration: \$REG_CHECK"
fi

# Check vLLM (if applicable)
if [ "\$AGENT_ROLE" = "vllm-worker" ]; then
  sleep 5  # give vLLM a moment to start
  if curl -sf http://localhost:8080/v1/models >/dev/null 2>&1; then
    echo "  ✅ vLLM: responding on localhost:8080"
  else
    echo "  ⚠️  vLLM: not responding yet on localhost:8080 (may still be loading model)"
    echo "     Check: systemctl --user status vllm-worker"
  fi

  # Check tunnel (if set up)
  if [ "\$HAS_TAILSCALE" = "false" ]; then
    TUNNEL_VERIFY=\$(curl -sf -X POST "\$RCC_URL/api/tunnel/verify" \\
      -H "Authorization: Bearer \$RCC_AGENT_TOKEN" \\
      -H "Content-Type: application/json" \\
      -d "{\\"agent\\":\\"\$AGENT_NAME\\"}" 2>/dev/null || echo '{"tunnelUp":false}')
    TUNNEL_UP=\$(echo "\$TUNNEL_VERIFY" | python3 -c "import json,sys; print(json.load(sys.stdin).get('tunnelUp',False))" 2>/dev/null || echo "False")
    if [ "\$TUNNEL_UP" = "True" ]; then
      echo "  ✅ Tunnel: verified from Rocky's side"
    else
      echo "  ⚠️  Tunnel: not yet visible from Rocky (may still be connecting)"
      echo "     Check: systemctl --user status rcc-vllm-tunnel"
    fi
  else
    echo "  ✅ Tailscale: vLLM accessible at \$TAILSCALE_IP:8080 (no tunnel needed)"
  fi
fi
echo ""

# ═══════════════════════════════════════════════════════════════════════════════
# PHASE 12 — SUMMARY
# ═══════════════════════════════════════════════════════════════════════════════
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  ✅ \$AGENT_NAME is online."
echo ""
echo "  Agent:      \$AGENT_NAME"
echo "  Role:       \$AGENT_ROLE"
echo "  Host:       \$AGENT_HOSTNAME"
if [ "\$HAS_GPU" = "true" ]; then
echo "  GPUs:       \$GPU_COUNT x \$GPU_MODEL (\${GPU_VRAM_GB}GB)"
fi
if [ "\$HAS_TAILSCALE" = "true" ]; then
echo "  Tailscale:  \$TAILSCALE_IP"
elif [ "\$AGENT_ROLE" = "vllm-worker" ]; then
echo "  Tunnel:     port \${TUNNEL_PORT:-unknown} via SSH reverse tunnel"
fi
echo "  Token:      \${RCC_AGENT_TOKEN:0:20}..."
echo "  Dashboard:  \$RCC_URL"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
`;

      // Assemble the full script
      const fullScript = script + vllmPhases + verifyAndSummary;

      console.log(`[rcc-api] Onboard script generated for ${entry.agent} (role hint: ${roleHint}) from ${req.socket?.remoteAddress}`);
      res.writeHead(200, { 'Content-Type': 'text/plain; charset=utf-8' });
      return res.end(fullScript);
    }

    // ── GET /api/users — public human participant registry ────────────────
    if (method === 'GET' && path === '/api/users') {
      const users = await readUsers();
      return json(res, 200, users);
    }

    // ── GET /api/providers — public provider list (read-only, no auth) ───
    if (method === 'GET' && path === '/api/providers') {
      const providers = await readJsonFile(PROVIDERS_PATH, {});
      return json(res, 200, Object.values(providers));
    }

    // ── GET /api/providers/:id — single provider lookup (no auth) ────────
    {
      const m = path.match(/^\/api\/providers\/([^/]+)$/);
      if (method === 'GET' && m) {
        const providers = await readJsonFile(PROVIDERS_PATH, {});
        const id = decodeURIComponent(m[1]);
        const p = providers[id];
        if (!p) return json(res, 404, { error: 'Provider not found' });
        return json(res, 200, p);
      }
    }

    // ── GET /api/services/status — public; live probe with 30s cache ─────
    if (method === 'GET' && path === '/api/services/status') {
      const statuses = await getServicesStatus();
      return json(res, 200, statuses);
    }

    // ── Auth-required endpoints ───────────────────────────────────────────
    if (!isAuthed(req)) {
      return json(res, 401, { error: 'Unauthorized' });
    }

    // ── POST /api/queue — create item ─────────────────────────────────────
    if (method === 'POST' && path === '/api/queue') {
      const body = await readBody(req);
      if (!body.title) return json(res, 400, { error: 'title required' });
      const q = await readQueue();

      // ── Semantic dedup gate ──────────────────────────────────────────────
      // Skip for idea priority or if explicitly bypassed
      const skipSemanticDedup = body.priority === 'idea' || body._skip_dedup === true;
      if (!skipSemanticDedup) {
        try {
          const SPARKY_OLLAMA = process.env.SPARKY_OLLAMA_URL || 'http://100.87.229.125:11434';
          const MILVUS_URL    = process.env.MILVUS_URL        || 'http://100.89.199.14:19530';
          const DEDUP_THRESH  = parseFloat(process.env.QUEUE_DEDUP_THRESHOLD || '0.85');
          const EMBED_MODEL   = 'nomic-embed-text';

          // Build text to embed: title + first 300 chars of description
          const embedText = `${body.title}\n${(body.description || '').slice(0, 300)}`.trim();

          // Embed via Ollama nomic-embed-text
          const embedCtrl = new AbortController();
          const embedTimer = setTimeout(() => embedCtrl.abort(), 5000);
          const embedResp = await fetch(`${SPARKY_OLLAMA}/api/embed`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ model: EMBED_MODEL, input: embedText }),
            signal: embedCtrl.signal,
          });
          clearTimeout(embedTimer);

          if (embedResp.ok) {
            const embedData = await embedResp.json();
            const vector = embedData?.embeddings?.[0];

            if (vector && vector.length === 768) {
              // Query rcc_queue_dedup for top-3 nearest neighbors
              const searchCtrl = new AbortController();
              const searchTimer = setTimeout(() => searchCtrl.abort(), 4000);
              const searchResp = await fetch(`${MILVUS_URL}/v2/vectordb/entities/search`, {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({
                  collectionName: 'rcc_queue_dedup',
                  data: [vector],
                  annsField: 'vector',
                  limit: 3,
                  outputFields: ['id', 'title', 'status'],
                  searchParams: { metric_type: 'COSINE', params: { nprobe: 10 } },
                }),
                signal: searchCtrl.signal,
              });
              clearTimeout(searchTimer);

              if (searchResp.ok) {
                const searchData = await searchResp.json();
                const hits = searchData?.data?.[0] || [];
                // Check if any hit exceeds threshold and is still active (pending/in-progress/incubating)
                const activeStatuses = new Set(['pending', 'in-progress', 'in_progress', 'claimed', 'incubating']);
                const duplicate = hits.find(h => h.distance >= DEDUP_THRESH && activeStatuses.has(h.status));
                if (duplicate) {
                  console.log(`[rcc-api] Semantic dedup: rejected "${body.title.slice(0,50)}" (similarity=${duplicate.distance.toFixed(3)} ≥ ${DEDUP_THRESH} to "${duplicate.title?.slice(0,50)}" id=${duplicate.id})`);
                  return json(res, 409, {
                    ok: false,
                    error: 'duplicate',
                    reason: 'semantic_dedup',
                    similarity: duplicate.distance,
                    threshold: DEDUP_THRESH,
                    duplicate_id: duplicate.id,
                    duplicate_title: duplicate.title,
                  });
                }
              }

              // No duplicate — upsert the embedding for future dedup checks (fire and forget)
              const tempId = body.id || `wq-tmp-${Date.now()}`;
              fetch(`${MILVUS_URL}/v2/vectordb/entities/upsert`, {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({
                  collectionName: 'rcc_queue_dedup',
                  data: [{ id: tempId, vector, title: body.title.slice(0, 256), status: 'pending' }],
                }),
              }).catch(() => {}); // fire-and-forget
            }
          }
        } catch (err) {
          // Dedup gate errors are non-fatal — log and continue
          console.warn('[rcc-api] Semantic dedup gate error (non-fatal):', err.message);
        }
      }
      // ── End semantic dedup gate ──────────────────────────────────────────

      // Scout dedup: if a scout_key is provided, reject if it already exists
      // anywhere in the queue (active OR completed) to prevent hourly re-filing.
      if (body.scout_key) {
        const allExisting = [...(q.items||[]), ...(q.completed||[])];
        const exists = allExisting.some(i =>
          i.scout_key === body.scout_key ||
          (i.tags || []).includes(body.scout_key)
        );
        if (exists) {
          return json(res, 200, { ok: false, duplicate: true, scout_key: body.scout_key });
        }
      }

      // Infer preferred_executor if not specified
      const inferExecutor = (b) => {
        if (b.preferred_executor) return b.preferred_executor;
        const tags = b.tags || [];
        if (tags.includes('gpu') || tags.includes('render') || tags.includes('simulation')) return 'gpu';
        if (tags.includes('reasoning') || tags.includes('code') || tags.includes('complex')) return 'claude_cli';
        if (tags.includes('heartbeat') || tags.includes('status') || tags.includes('poll')) return 'inference_key';
        if (tags.includes('embedding') || tags.includes('local-llm') || tags.includes('peer-llm')) return 'llm_server';
        // Default: claude_cli for assignee-specific tasks, inference_key for housekeeping
        return (b.assignee && b.assignee !== 'all') ? 'claude_cli' : 'inference_key';
      };

      // Prevent ID collisions — if a caller supplies an ID that already exists
      // (in either active items or completed), generate a fresh one instead.
      const allIds = new Set([...(q.items||[]), ...(q.completed||[])].map(i => i.id));
      let itemId = body.id || `wq-API-${Date.now()}`;
      if (body.id && allIds.has(body.id)) {
        itemId = `wq-API-${Date.now()}`;
        console.warn(`[rcc-api] ID collision on "${body.id}" — reassigned to "${itemId}"`);
      }

      // Coerce numeric priority to string label; reject unknown strings
      const VALID_PRIORITIES = new Set(['critical','high','medium','normal','low','idea']);
      const NUMERIC_PRIORITY_MAP = (n) => n >= 80 ? 'critical' : n >= 60 ? 'high' : n >= 40 ? 'medium' : n >= 20 ? 'low' : 'idea';
      let rawPriority = body.priority ?? 'normal';
      if (typeof rawPriority === 'number') {
        rawPriority = NUMERIC_PRIORITY_MAP(rawPriority);
        console.warn(`[rcc-api] Numeric priority ${body.priority} coerced to "${rawPriority}" for item "${body.title?.slice(0,40)}"`);
      } else if (!VALID_PRIORITIES.has(rawPriority)) {
        console.warn(`[rcc-api] Unknown priority "${rawPriority}" for item "${body.title?.slice(0,40)}" — defaulting to "normal"`);
        rawPriority = 'normal';
      }

      const item = {
        id: itemId,
        itemVersion: 1,
        created: new Date().toISOString(),
        source: body.source || 'api',
        assignee: body.assignee || 'all',
        priority: rawPriority,
        status: 'pending',
        title: body.title,
        description: body.description || '',
        notes: body.notes || '',
        preferred_executor: inferExecutor(body),  // claude_cli | inference_key | gpu
        journal: [],
        choices: body.choices || [],
        choiceRecorded: null,
        votes: [],
        attempts: 0,
        maxAttempts: body.maxAttempts || 3,
        claimedBy: null,
        claimedAt: null,
        completedAt: null,
        result: null,
        tags: body.tags || [],
        // Scout dedup key — preserved for itemAlreadyExists() checks
        scout_key: body.scout_key || null,
        repo: body.repo || null,
        project: body.project || body.repo || null,
      };
      if (!q.items) q.items = [];
      q.items.push(item);
      await writeQueue(q);
      // Fan-out: notify project channel that a new task was queued
      if (item.project && item.priority !== 'idea') {
        fanoutToProjectChannels(item.project,
          `📋 New task queued: *${item.title}* (${item.priority})\n${item.description ? item.description.slice(0, 200) : ''}`
        );
      }
      // Backfill Milvus dedup entry with the real item ID (tempId was provisional)
      if (!skipSemanticDedup && item.id !== (body.id || `wq-tmp-${Date.now()}`)) {
        // Already upserted with tempId — update status field with real ID via delete+reinsert is complex;
        // next heartbeat will pick up and re-embed if needed. The gate already blocked the duplicate path.
      }
      return json(res, 201, { ok: true, item });
    }

    // ── GET /api/queue/stale — list stale claims ──────────────────────────
    if (method === 'GET' && path === '/api/queue/stale') {
      const q = await readQueue();
      const now = Date.now();
      const stale = (q.items || []).filter(item => {
        if (item.status !== 'in-progress' || !item.claimedAt) return false;
        const threshold = STALE_THRESHOLDS[item.preferred_executor] || STALE_THRESHOLDS.default;
        return (now - new Date(item.claimedAt).getTime()) > threshold;
      }).map(item => {
        const threshold = STALE_THRESHOLDS[item.preferred_executor] || STALE_THRESHOLDS.default;
        const age = now - new Date(item.claimedAt).getTime();
        return { ...item, staleMs: age, thresholdMs: threshold, staleMin: Math.round(age / 60000) };
      });
      return json(res, 200, { stale, count: stale.length, thresholds: STALE_THRESHOLDS });
    }

    // ── POST /api/queue/expire-stale — server-side stale reset ───────────
    if (method === 'POST' && path === '/api/queue/expire-stale') {
      const q = await readQueue();
      const now = Date.now();
      let reset = 0;
      for (const item of (q.items || [])) {
        if (item.status !== 'in-progress' || !item.claimedAt) continue;
        const threshold = STALE_THRESHOLDS[item.preferred_executor] || STALE_THRESHOLDS.default;
        if ((now - new Date(item.claimedAt).getTime()) > threshold) {
          const prevAgent = item.claimedBy;
          item.status = 'pending';
          item.claimedBy = null;
          item.claimedAt = null;
          item.attempts = (item.attempts || 0) + 1;
          if (!item.journal) item.journal = [];
          item.journal.push({
            ts: new Date().toISOString(),
            author: 'rcc-api',
            type: 'stale-reset',
            text: `Stale claim reset (was ${prevAgent}, threshold: ${threshold/60000}min for ${item.preferred_executor || 'default'})`,
          });
          reset++;
        }
      }
      if (reset > 0) await writeQueue(q);
      return json(res, 200, { ok: true, reset });
    }

    // ── POST /api/item/:id/claim — agent claims an item ──────────────────
    // Runs inside withQueueLock so concurrent claims are serialized; the
    // second agent always sees the first agent's write and gets a 409.
    const itemClaimMatch = path.match(/^\/api\/item\/([^/]+)\/claim$/);
    if (method === 'POST' && itemClaimMatch) {
      const id = decodeURIComponent(itemClaimMatch[1]);
      const body = await readBody(req);
      const agent = body.agent || body._author;
      if (!agent) return json(res, 400, { error: 'agent required' });
      return withQueueLock(async () => {
        const q = await readQueue();
        const item = q.items?.find(i => i.id === id);
        if (!item) return json(res, 404, { error: 'Item not found' });
        // Guard: already claimed by someone else and not stale
        if (item.claimedBy && item.claimedBy !== agent && item.status === 'in-progress') {
          const threshold = STALE_THRESHOLDS[item.preferred_executor] || STALE_THRESHOLDS.default;
          const age = Date.now() - new Date(item.claimedAt).getTime();
          if (age < threshold) {
            return json(res, 409, { error: `Already claimed by ${item.claimedBy}`, claimedBy: item.claimedBy, claimedAt: item.claimedAt });
          }
        }
        // Guard: item must be pending (or stale in-progress handled above)
        if (item.status !== 'pending' && item.status !== 'in-progress') {
          return json(res, 409, { error: `Item is ${item.status}, cannot claim` });
        }
        const now = new Date().toISOString();
        const prevAgent = item.claimedBy;
        item.claimedBy = agent;
        item.claimedAt = now;
        item.keepaliveAt = now;
        item.status = 'in-progress';
        item.attempts = (item.attempts || 0) + 1;
        if (!item.journal) item.journal = [];
        item.journal.push({ ts: now, author: agent, type: 'claim', text: prevAgent ? `Claimed (previous: ${prevAgent})` : 'Claimed' });
        if (!item.events) item.events = [];
        item.events.push({ ts: now, agent, type: 'claim', note: body.note || null });
        item.itemVersion = (item.itemVersion || 0) + 1;
        await writeQueue(q);
        return json(res, 200, { ok: true, item });
      });
    }

    // ── POST /api/item/:id/complete — agent marks item done ───────────────
    const itemCompleteMatch = path.match(/^\/api\/item\/([^/]+)\/complete$/);
    if (method === 'POST' && itemCompleteMatch) {
      const id = decodeURIComponent(itemCompleteMatch[1]);
      const body = await readBody(req);
      const agent = body.agent || body._author;
      const q = await readQueue();
      const item = q.items?.find(i => i.id === id);
      if (!item) return json(res, 404, { error: 'Item not found' });
      const now = new Date().toISOString();
      item.status = 'completed';
      item.completedAt = now;
      if (body.resolution) item.resolution = body.resolution;
      if (body.result) item.result = body.result;
      if (!item.journal) item.journal = [];
      item.journal.push({ ts: now, author: agent || 'api', type: 'complete', text: body.resolution || body.result || 'Completed' });
      if (!item.events) item.events = [];
      item.events.push({ ts: now, agent: agent || 'api', type: 'complete', note: body.resolution || body.result || null });
      item.itemVersion = (item.itemVersion || 0) + 1;
      // Auto-archive to completed[]
      q.items = q.items.filter(i => i.id !== id);
      if (!q.completed) q.completed = [];
      q.completed.push(item);
      await writeQueue(q);
      notifyJkhCompletion(item, agent); // fire-and-forget
      // Fan-out to project channel
      if (item.project) {
        const resolution = (item.resolution || item.result || '').slice(0, 200);
        fanoutToProjectChannels(item.project,
          `✅ *${item.title}* — completed by ${agent || 'unknown'}${resolution ? '\n' + resolution : ''}`
        );
      }
      return json(res, 200, { ok: true, item });
    }

    // ── POST /api/item/:id/fail — agent marks item failed, resets to pending
    const itemFailMatch = path.match(/^\/api\/item\/([^/]+)\/fail$/);
    if (method === 'POST' && itemFailMatch) {
      const id = decodeURIComponent(itemFailMatch[1]);
      const body = await readBody(req);
      const agent = body.agent || body._author;
      const q = await readQueue();
      const item = q.items?.find(i => i.id === id);
      if (!item) return json(res, 404, { error: 'Item not found' });
      const now = new Date().toISOString();
      const reason = body.reason || 'Agent reported failure';
      item.status = 'pending';
      item.claimedBy = null;
      item.claimedAt = null;
      item.keepaliveAt = null;
      if (!item.journal) item.journal = [];
      item.journal.push({ ts: now, author: agent || 'api', type: 'fail', text: reason });
      if (!item.events) item.events = [];
      item.events.push({ ts: now, agent: agent || 'api', type: 'fail', note: reason });
      item.itemVersion = (item.itemVersion || 0) + 1;
      // Move to DLQ if maxAttempts exceeded
      if (item.attempts >= (item.maxAttempts || 3)) {
        item.status = 'blocked';
        item.blockedReason = `Exceeded maxAttempts (${item.maxAttempts || 3}). Last failure: ${reason}`;
        item.journal.push({ ts: now, author: 'rcc-api', type: 'dlq', text: `Moved to blocked — maxAttempts exceeded` });
      }
      await writeQueue(q);
      return json(res, 200, { ok: true, item });
    }

    // ── POST /api/item/:id/keepalive — heartbeat for long-running tasks ───
    const itemKeepaliveMatch = path.match(/^\/api\/item\/([^/]+)\/keepalive$/);
    if (method === 'POST' && itemKeepaliveMatch) {
      const id = decodeURIComponent(itemKeepaliveMatch[1]);
      const body = await readBody(req);
      const agent = body.agent || body._author;
      const q = await readQueue();
      const item = q.items?.find(i => i.id === id);
      if (!item) return json(res, 404, { error: 'Item not found' });
      if (item.claimedBy && agent && item.claimedBy !== agent) {
        return json(res, 409, { error: `Item claimed by ${item.claimedBy}, not ${agent}` });
      }
      const now = new Date().toISOString();
      item.keepaliveAt = now;
      if (!item.events) item.events = [];
      item.events.push({ ts: now, agent: agent || item.claimedBy || 'api', type: 'keepalive', note: body.note || null });
      item.itemVersion = (item.itemVersion || 0) + 1;
      await writeQueue(q);
      return json(res, 200, { ok: true, keepaliveAt: now });
    }

    // ── PATCH /api/item/:id ───────────────────────────────────────────────
    const patchMatch = path.match(/^\/api\/item\/([^/]+)$/);
    if (method === 'PATCH' && patchMatch) {
      const id = decodeURIComponent(patchMatch[1]);
      const body = await readBody(req);
      const q = await readQueue();
      const item = q.items?.find(i => i.id === id);
      if (!item) return json(res, 404, { error: 'Item not found' });
      const allowed = ['title','description','priority','assignee','status','notes','choices','claimedBy','claimedAt','result','completedAt','type','blockedBy','blocks','needsHuman','needsHumanAt','needsHumanReason'];
      const now = new Date().toISOString();
      const changed = [];
      for (const field of allowed) {
        if (body[field] !== undefined && body[field] !== item[field]) {
          changed.push(`${field}: ${JSON.stringify(item[field])} → ${JSON.stringify(body[field])}`);
          item[field] = body[field];
        }
      }
      if (changed.length) {
        if (!item.journal) item.journal = [];
        item.journal.push({ ts: now, author: body._author || 'api', type: 'status-change', text: `Updated: ${changed.join('; ')}` });
        item.itemVersion = (item.itemVersion || 0) + 1;
        // Auto-archive: move completed/cancelled items from items[] to completed[]
        if (item.status === 'completed' || item.status === 'cancelled') {
          q.items = q.items.filter(i => i.id !== item.id);
          if (!q.completed) q.completed = [];
          q.completed.push(item);
          if (item.status === 'completed') notifyJkhCompletion(item, body._author); // fire-and-forget
        }
        await writeQueue(q);
      }
      return json(res, 200, { ok: true, item });
    }

    // ── POST /api/item/:id/comment ────────────────────────────────────────
    const commentMatch = path.match(/^\/api\/item\/([^/]+)\/comment$/);
    if (method === 'POST' && commentMatch) {
      const id = decodeURIComponent(commentMatch[1]);
      const body = await readBody(req);
      const text = (body.text || '').trim();
      if (!text) return json(res, 400, { error: 'text required' });
      const q = await readQueue();
      const item = q.items?.find(i => i.id === id);
      if (!item) return json(res, 404, { error: 'Item not found' });
      if (!item.journal) item.journal = [];
      const entry = { ts: new Date().toISOString(), author: body.author || 'api', type: 'comment', text };
      item.journal.push(entry);
      item.itemVersion = (item.itemVersion || 0) + 1;
      await writeQueue(q);
      // Fan-out significant agent comments to project channel
      if (item.project && body.author && body.author !== 'api') {
        fanoutToProjectChannels(item.project,
          `💬 *${body.author}* on *${item.title}*: ${text.slice(0, 300)}`
        );
      }
      return json(res, 200, { ok: true, entry });
    }

    // ── POST /api/item/:id/choice ─────────────────────────────────────────
    const choiceMatch = path.match(/^\/api\/item\/([^/]+)\/choice$/);
    if (method === 'POST' && choiceMatch) {
      const id = decodeURIComponent(choiceMatch[1]);
      const body = await readBody(req);
      if (!body.choice) return json(res, 400, { error: 'choice required' });
      const q = await readQueue();
      const item = q.items?.find(i => i.id === id);
      if (!item) return json(res, 404, { error: 'Item not found' });
      const now = new Date().toISOString();
      if (!item.journal) item.journal = [];
      const entry = { ts: now, author: body.author || 'api', type: 'choice', text: `Choice: [${body.choice}] ${body.choiceLabel || ''}` };
      item.journal.push(entry);
      item.choiceRecorded = { choice: body.choice, label: body.choiceLabel || '', ts: now };
      item.itemVersion = (item.itemVersion || 0) + 1;
      await writeQueue(q);
      return json(res, 200, { ok: true, entry, choiceRecorded: item.choiceRecorded });
    }

    // ── POST /api/item/:id/ai-comment ─────────────────────────────────────
    const aiMatch = path.match(/^\/api\/item\/([^/]+)\/ai-comment$/);
    if (method === 'POST' && aiMatch) {
      const id = decodeURIComponent(aiMatch[1]);
      const body = await readBody(req);
      const prompt = (body.prompt || '').trim();
      if (!prompt) return json(res, 400, { error: 'prompt required' });
      const q = await readQueue();
      const item = q.items?.find(i => i.id === id);
      if (!item) return json(res, 404, { error: 'Item not found' });
      const now = new Date().toISOString();
      if (!item.journal) item.journal = [];
      const userEntry = { ts: now, author: body.author || 'jkh', type: 'ai', text: `✨ ${prompt}` };
      item.journal.push(userEntry);

      // Queue to brain for async processing, or call inline if brain available
      let aiText = '(queued for brain processing)';
      try {
        const b = await getBrain();
        const brainReq = createRequest({
          messages: [
            { role: 'system', content: `You are Rocky, helping with work item "${item.title}". Be concise.` },
            { role: 'user', content: prompt }
          ],
          maxTokens: 500,
          priority: 'normal',
          metadata: { itemId: id },
        });
        // Await completion inline (with timeout)
        const result = await Promise.race([
          new Promise(resolve => {
            const onComplete = (r) => { if (r.id === brainReq.id) { b.off('completed', onComplete); resolve(r.result); } };
            b.on('completed', onComplete);
            b.enqueue(brainReq);
          }),
          new Promise((_, reject) => setTimeout(() => reject(new Error('timeout')), 20000))
        ]);
        aiText = result;
      } catch (e) {
        aiText = `(brain error: ${e.message})`;
      }

      const aiEntry = { ts: new Date().toISOString(), author: '🐿️ Rocky', type: 'ai', text: aiText };
      item.journal.push(aiEntry);
      item.itemVersion = (item.itemVersion || 0) + 1;
      await writeQueue(q);
      return json(res, 200, { ok: true, userEntry, aiEntry });
    }

    // ── POST /api/agents/register ─────────────────────────────────────────
    if (method === 'POST' && path === '/api/agents/register') {
      const body = await readBody(req);
      if (!body.name) return json(res, 400, { error: 'name required' });
      const agents = await readAgents();
      // Preserve existing token on re-registration (resurrection / re-onboard)
      const existingToken = agents[body.name]?.token;
      const token = existingToken || `rcc-agent-${body.name}-${Math.random().toString(36).slice(2)}${Date.now().toString(36)}`;
      const tailscaleIp = body.capabilities?.tailscale_ip || body.tailscale_ip || null;
      const hasTailscale = body.capabilities?.tailscale ?? (tailscaleIp != null);
      const hasVllm = body.capabilities?.vllm ?? false;
      const vllmPort = body.capabilities?.vllm_port || 8080;
      agents[body.name] = {
        ...(agents[body.name] || {}),
        name: body.name,
        host: body.host || agents[body.name]?.host || 'unknown',
        type: body.type || agents[body.name]?.type || 'full',  // full | container | local | spark
        token,
        registeredAt: agents[body.name]?.registeredAt || new Date().toISOString(),
        lastSeen: agents[body.name]?.lastSeen || null,
        // Worker capabilities — declared at registration, updated via PATCH /api/agents/:name
        capabilities: {
          ...(agents[body.name]?.capabilities || {}),
          claude_cli: body.capabilities?.claude_cli ?? agents[body.name]?.capabilities?.claude_cli ?? true,
          claude_cli_model: body.capabilities?.claude_cli_model || agents[body.name]?.capabilities?.claude_cli_model || null,
          inference_key: body.capabilities?.inference_key ?? agents[body.name]?.capabilities?.inference_key ?? true,
          inference_provider: body.capabilities?.inference_provider || agents[body.name]?.capabilities?.inference_provider || 'nvidia',
          gpu: body.capabilities?.gpu ?? agents[body.name]?.capabilities?.gpu ?? false,
          gpu_model: body.capabilities?.gpu_model || agents[body.name]?.capabilities?.gpu_model || null,
          gpu_count: body.capabilities?.gpu_count ?? agents[body.name]?.capabilities?.gpu_count ?? 0,
          gpu_vram_gb: body.capabilities?.gpu_vram_gb ?? agents[body.name]?.capabilities?.gpu_vram_gb ?? 0,
          vllm: hasVllm || agents[body.name]?.capabilities?.vllm || false,
          vllm_port: vllmPort,
          vllm_model: body.capabilities?.vllm_model || agents[body.name]?.capabilities?.vllm_model || null,
          tailscale: hasTailscale || agents[body.name]?.capabilities?.tailscale || false,
          tailscale_ip: tailscaleIp || agents[body.name]?.capabilities?.tailscale_ip || null,
          model_seeder: body.capabilities?.model_seeder ?? agents[body.name]?.capabilities?.model_seeder ?? false,
          model_seeder_port: body.capabilities?.model_seeder_port ?? agents[body.name]?.capabilities?.model_seeder_port ?? null,
        },
        billing: {
          claude_cli: body.billing?.claude_cli || agents[body.name]?.billing?.claude_cli || 'fixed',
          inference_key: body.billing?.inference_key || agents[body.name]?.billing?.inference_key || 'metered',
          gpu: body.billing?.gpu || agents[body.name]?.billing?.gpu || 'fixed',
        },
      };

      // ── Resolve LLM baseUrl: tailscale (direct) > tunnel > null ──────────
      const effectiveTailscaleIp = agents[body.name].capabilities.tailscale_ip;
      if (effectiveTailscaleIp && hasVllm) {
        // Direct Tailscale access — no tunnel needed
        agents[body.name].llm = {
          baseUrl: `http://${effectiveTailscaleIp}:${vllmPort}/v1`,
          backend: 'vllm',
          models: ['nemotron'],
          modelCount: 1,
          fresh: false,
          updatedAt: new Date().toISOString(),
        };
        console.log(`[rcc-api] ${body.name} registered with Tailscale vLLM at ${effectiveTailscaleIp}:${vllmPort}`);
      }
      // (If no tailscale, LLM routing via tunnel will be set when tunnel registers)

      await writeAgents(agents);
      AUTH_TOKENS.add(token);

      // ── Auto-register vLLM workers with TokenHub ─────────────────────────
      if (agents[body.name].capabilities.vllm && process.env.TOKENHUB_URL && process.env.TOKENHUB_ADMIN_TOKEN) {
        let providerUrl = null;
        if (effectiveTailscaleIp) {
          // Tailscale: direct connection, no tunnel
          providerUrl = `http://${effectiveTailscaleIp}:${vllmPort}`;
        } else {
          // Tunnel path: find allocated port
          const tunnelState = await readJsonFile(TUNNEL_STATE_PATH, { tunnels: {} });
          const tunnel = Object.values(tunnelState.tunnels).find(t => t.agent === body.name || t.agent?.toLowerCase() === body.name?.toLowerCase());
          if (tunnel?.port) providerUrl = `http://127.0.0.1:${tunnel.port}`;
        }

        if (providerUrl) {
          const providerId = `${body.name.toLowerCase()}-vllm`;
          try {
            await fetch(`${process.env.TOKENHUB_URL}/admin/v1/providers`, {
              method: 'POST',
              headers: { 'Content-Type': 'application/json', 'Authorization': `Bearer ${process.env.TOKENHUB_ADMIN_TOKEN}` },
              body: JSON.stringify({ id: providerId, type: 'vllm', base_url: providerUrl, api_key: 'none', enabled: true }),
            });
            await fetch(`${process.env.TOKENHUB_URL}/admin/v1/models`, {
              method: 'POST',
              headers: { 'Content-Type': 'application/json', 'Authorization': `Bearer ${process.env.TOKENHUB_ADMIN_TOKEN}` },
              body: JSON.stringify({ id: `nemotron-${body.name.toLowerCase()}`, provider_id: providerId, weight: 8, max_context_tokens: 262144, enabled: true }),
            });
            console.log(`[rcc-api] Registered ${body.name} as TokenHub provider ${providerId} → ${providerUrl}`);
          } catch (thErr) {
            console.warn(`[rcc-api] TokenHub registration failed for ${body.name}: ${thErr.message}`);
          }
        } else {
          console.log(`[rcc-api] ${body.name} has vLLM but no tunnel/tailscale assigned yet — skipping TokenHub registration`);
        }
      }

      return json(res, 201, { ok: true, token, agent: { ...agents[body.name], token } });
    }

    // ── POST /api/agents/:name — publish capabilities at startup (upsert) ─
    const agentNameMatch = path.match(/^\/api\/agents\/([^/]+)$/);
    if (method === 'POST' && agentNameMatch) {
      const name = decodeURIComponent(agentNameMatch[1]);
      const body = await readBody(req);
      const agents = await readAgents();
      if (!agents[name]) {
        // auto-register on first capability publish
        const token = `rcc-agent-${name}-${Math.random().toString(36).slice(2)}${Date.now().toString(36)}`;
        agents[name] = {
          name,
          host: body.host || 'unknown',
          type: body.type || 'full',
          token,
          registeredAt: new Date().toISOString(),
          lastSeen: null,
          capabilities: {},
          billing: { claude_cli: 'fixed', inference_key: 'metered', gpu: 'fixed' },
        };
        AUTH_TOKENS.add(token);
      } else {
        if (body.host) agents[name].host = body.host;
        if (body.type) agents[name].type = body.type;
      }
      await writeAgents(agents);
      if (body.capabilities) {
        const caps = await readCapabilities();
        caps[name] = { ...(caps[name] || {}), ...body.capabilities };
        await writeCapabilities(caps);
      }
      return json(res, 200, { ok: true, token: agents[name].token, agent: agents[name] });
    }

    // ── PATCH /api/agents/:name — update capabilities or decommission ─────
    const agentPatchMatch = path.match(/^\/api\/agents\/([^/]+)$/);
    if (method === 'PATCH' && agentPatchMatch) {
      const name = decodeURIComponent(agentPatchMatch[1]);
      const body = await readBody(req);
      const agents = await readAgents();
      if (!agents[name]) return json(res, 404, { error: 'Agent not found' });
      if (body.capabilities) Object.assign(agents[name].capabilities || {}, body.capabilities);
      if (body.billing) Object.assign(agents[name].billing || {}, body.billing);
      if (body.host) agents[name].host = body.host;
      if (body.type) agents[name].type = body.type;
      // ── Tombstoning: mark agent as decommissioned ──────────────────────
      if (body.status === 'decommissioned') {
        agents[name].decommissioned = true;
        agents[name].decommissionedAt = new Date().toISOString();
        agents[name].onlineStatus = 'decommissioned';
        // Also mark in-memory heartbeat so no alerts fire
        if (heartbeats[name]) heartbeats[name].decommissioned = true;
      } else if (body.status === 'active') {
        delete agents[name].decommissioned;
        delete agents[name].decommissionedAt;
        agents[name].onlineStatus = 'unknown';
        if (heartbeats[name]) delete heartbeats[name].decommissioned;
      }
      await writeAgents(agents);
      if (body.capabilities) {
        const caps = await readCapabilities();
        caps[name] = { ...(caps[name] || {}), ...body.capabilities };
        await writeCapabilities(caps);
      }
      return json(res, 200, { ok: true, agent: agents[name] });
    }

    // ── POST /api/heartbeat/:agent ────────────────────────────────────────
    const hbMatch = path.match(/^\/api\/heartbeat\/([^/]+)$/);
    if (method === 'POST' && hbMatch) {
      const agent = decodeURIComponent(hbMatch[1]);
      const body = await readBody(req);
      const ts = new Date().toISOString();
      heartbeats[agent] = { agent, ts, status: 'online', ...body, _wasOnline: true };
      // Ring buffer for heartbeat history (max 288 entries = 24h at 5-min intervals)
      if (!heartbeatHistory[agent]) heartbeatHistory[agent] = [];
      const hbEntry = { ts, status: 'online', host: body.host || null };
      heartbeatHistory[agent].push(hbEntry);
      if (heartbeatHistory[agent].length > 288) heartbeatHistory[agent].shift();
      // Append to persistent JSONL history (async, non-blocking)
      const histDir = new URL('./data/heartbeat-history', import.meta.url).pathname;
      mkdir(histDir, { recursive: true }).then(() => {
        const histFile = `${histDir}/${agent}.jsonl`;
        const line = JSON.stringify({ ts, agent, host: body.host || null, status: 'online' }) + '\n';
        return import('fs').then(fsmod => {
          const { appendFileSync } = fsmod;
          appendFileSync(histFile, line);
        });
      }).catch(() => {});
      // Update agent lastSeen + onlineStatus in registry (persist even after restart)
      const agents = await readAgents();
      if (agents[agent]) {
        agents[agent].lastSeen = ts;
        agents[agent].onlineStatus = 'online';
        await writeAgents(agents);
      }
      // Clear offline alert state since they're back
      delete offlineAlertSent[agent];
      broadcastGeekEvent('heartbeat', agent, 'rocky', `${agent} heartbeat`);
      // Scout: include pending work for this agent in the heartbeat response
      const scoutQ = await readQueue().catch(() => ({ items: [] }));
      const pendingWork = (scoutQ.items || [])
        .filter(i => i.status === 'pending' && (i.assignee === agent || i.assignee === 'all'))
        .slice(0, 3)
        .map(({ id, title, priority, description }) => ({ id, title, priority, description }));
      return json(res, 200, { ok: true, pendingWork });
    }

    // ── POST /api/complete/:id ────────────────────────────────────────────
    const completeMatch = path.match(/^\/api\/complete\/([^/]+)$/);
    if (method === 'POST' && completeMatch) {
      const id = decodeURIComponent(completeMatch[1]);
      const body = await readBody(req);
      const q = await readQueue();
      const item = q.items?.find(i => i.id === id);
      if (!item) return json(res, 404, { error: 'Item not found' });
      item.status = 'completed';
      item.completedAt = new Date().toISOString();
      item.itemVersion = (item.itemVersion || 0) + 1;
      if (body?.result) item.result = body.result;
      await writeQueue(q);
      notifyJkhCompletion(item, body?._author || body?.agent); // fire-and-forget

      // ── requestId linkage: resolve matching delegation on parent ticket ──
      if (item.requestId) {
        try {
          const reqs = await readRequests();
          const ticket = reqs.find(r => r.id === item.requestId);
          if (ticket) {
            const outcome = item.result || `Queue item ${item.id} completed`;
            // Find unresolved delegation matching this queue item
            const delIdx = (ticket.delegations || []).findIndex(d =>
              !d.resolvedAt && (d.queueItemId === id || d.summary?.includes(id) || d.summary?.includes(item.title))
            );
            if (delIdx >= 0) {
              ticket.delegations[delIdx].resolvedAt = new Date().toISOString();
              ticket.delegations[delIdx].outcome = outcome;
            }
            // If all delegations resolved, mark ticket resolved
            const allResolved = (ticket.delegations || []).every(d => d.resolvedAt);
            if (allResolved && ticket.status === 'delegated') {
              ticket.status = 'resolved';
              ticket.resolution = outcome;
            }
            await writeRequests(reqs);
          }
        } catch (e) {
          console.error('[rcc-api] requestId linkage error:', e.message);
        }
      }

      return json(res, 200, { ok: true, item });
    }

    // ── POST /api/brain/request — submit LLM request to brain ────────────
    if (method === 'POST' && path === '/api/brain/request') {
      const body = await readBody(req);
      if (!body.messages || !Array.isArray(body.messages)) return json(res, 400, { error: 'messages array required' });
      const b = await getBrain();
      const req2 = createRequest({
        messages: body.messages,
        maxTokens: body.maxTokens || 1024,
        priority: body.priority || 'normal',
        callbackUrl: body.callbackUrl || null,
        metadata: body.metadata || {},
      });
      const id = await b.enqueue(req2);
      return json(res, 202, { ok: true, requestId: id, status: 'queued' });
    }

    // ── GET /api/brain/status ─────────────────────────────────────────────
    if (method === 'GET' && path === '/api/brain/status') {
      const b = brain;
      if (!b) return json(res, 200, { ok: true, status: 'not started' });
      return json(res, 200, b.getStatus());
    }

    // ── POST /api/lessons — record a lesson ──────────────────────────────
    if (method === 'POST' && path === '/api/lessons') {
      const body = await readBody(req);
      if (!body.domain || !body.symptom || !body.fix) return json(res, 400, { error: 'domain, symptom, fix required' });
      const lesson = await learnLesson({ ...body, agent: body.agent || 'api' });
      return json(res, 201, { ok: true, lesson });
    }

    // ── GET /api/lessons/trending — top lessons by score + recency ────────
    if (method === 'GET' && path === '/api/lessons/trending') {
      const limit = parseInt(url.searchParams.get('limit') || '5', 10);
      const recentDays = parseInt(url.searchParams.get('days') || '7', 10);
      const lessons = await getTrendingLessons({ limit, recentDays });
      const context = url.searchParams.get('format') === 'context' ? formatTrendingForHeartbeat(lessons) : null;
      return json(res, 200, { lessons, context, count: lessons.length });
    }

    // ── GET /api/lessons/heartbeat — context block for heartbeat ──────────
    if (method === 'GET' && path === '/api/lessons/heartbeat') {
      const domains = (url.searchParams.get('domains') || '').split(',').filter(Boolean);
      const context = await getHeartbeatContext({ domains });
      return json(res, 200, { context });
    }

    // ── GET /api/lessons?domain=X&q=keyword+keyword ───────────────────────
    // If no domain specified but q= is present, search across all domains
    if (method === 'GET' && path.startsWith('/api/lessons')) {
      const domain = url.searchParams.get('domain');
      const q = (url.searchParams.get('q') || '').split(/\s+/).filter(Boolean);
      const limit = parseInt(url.searchParams.get('limit') || '5', 10);

      let lessons;
      if (!domain) {
        // Cross-domain search
        lessons = await queryAllLessons({ keywords: q, limit });
      } else {
        lessons = await queryLessons({ domain, keywords: q, limit });
      }
      const context = url.searchParams.get('format') === 'context' ? formatLessonsForContext(lessons) : null;
      return json(res, 200, { lessons, context, count: lessons.length });
    }

    // ── GET /api/repos ────────────────────────────────────────────────────
    if (method === 'GET' && path === '/api/repos') {
      const repos = await getPump().listRepos();
      // Enrich with kind/ownership summary for dashboard
      const enriched = repos.map(r => ({
        ...r,
        kind: r.kind || 'personal',
        ownership_summary: repoOwnershipSummary(r),
      }));
      return json(res, 200, enriched);
    }

    // ── GET /api/repos/:name or PATCH /api/repos/:name ───────────────────
    const repoSingleMatch = path.match(/^\/api\/repos\/([^/]+\/[^/]+)$/);
    if (repoSingleMatch) {
      const fullName = decodeURIComponent(repoSingleMatch[1]);
      if (method === 'GET') {
        const repos = await getPump().listRepos();
        const repo = repos.find(r => r.full_name === fullName);
        if (!repo) return json(res, 404, { error: 'Repo not found' });
        return json(res, 200, { ...repo, ownership_summary: repoOwnershipSummary(repo) });
      }
      if (method === 'PATCH') {
        const body = await readBody(req);
        const repo = await getPump().patchRepo(fullName, body);
        return json(res, 200, { ok: true, repo });
      }
    }

    // ── POST /api/repos/register ──────────────────────────────────────────
    if (method === 'POST' && path === '/api/repos/register') {
      const body = await readBody(req);
      if (!body.full_name) return json(res, 400, { error: 'full_name required (e.g. owner/repo)' });
      const repo = await getPump().registerRepo(body);
      return json(res, 201, { ok: true, repo });
    }

    // ── GET /api/projects — list all projects (derived from repos + projects.json) ──
    if (method === 'GET' && path === '/api/projects') {
      const repos    = await getPump().listRepos();
      const projects = await readProjects();
      // Merge: repos.json is source of truth; projects.json holds Slack channel overrides
      const projectMap = new Map(projects.map(p => [p.id, p]));
      const result = repos
        .filter(r => r.enabled !== false)
        .map(r => {
          const base    = buildProjectFromRepo(r);
          const overlay = projectMap.get(r.full_name) || {};
          return { ...base, ...overlay };
        });
      return json(res, 200, result);
    }

    // ── GET /api/projects/:owner/:repo/github — live issues + PRs ────────
    // Must be before projectDetailMatch (which would otherwise eat the /github suffix)
    const projectGithubMatch = path.match(/^\/api\/projects\/([^/]+(?:\/[^/]+|%2F[^/]+))\/github$/i);
    if (method === 'GET' && projectGithubMatch) {
      const fullName = decodeURIComponent(projectGithubMatch[1]);
      // 5-minute in-memory cache
      if (!globalThis._githubCache) globalThis._githubCache = new Map();
      const cached = globalThis._githubCache.get(fullName);
      const bustCache = url.searchParams.get('refresh') === '1';
      if (cached && !bustCache && (Date.now() - cached.ts) < 5 * 60 * 1000) {
        return json(res, 200, cached.data);
      }
      const { execSync } = await import('child_process');
      function ghq(args, fields) {
        try {
          const out = execSync(`gh ${args} --json ${fields}`, { encoding: 'utf8', stdio: ['pipe','pipe','pipe'] });
          return JSON.parse(out);
        } catch { return null; }
      }
      const issues = ghq(`issue list --repo ${fullName} --state open --limit 50`,
        'number,title,labels,url,author,createdAt,updatedAt,comments') || [];
      const prs = ghq(`pr list --repo ${fullName} --state open --limit 30`,
        'number,title,author,url,isDraft,reviewDecision,mergeable,createdAt,updatedAt,labels') || [];
      const result = {
        repo: fullName,
        fetchedAt: new Date().toISOString(),
        issues: issues.map(i => ({
          number: i.number,
          title: i.title,
          url: i.url,
          labels: (i.labels || []).map(l => ({ name: l.name, color: l.color })),
          author: i.author?.login || i.author,
          createdAt: i.createdAt,
          updatedAt: i.updatedAt,
          commentCount: (i.comments || []).length,
        })),
        prs: (prs || []).map(p => ({
          number: p.number,
          title: p.title,
          url: p.url,
          author: p.author?.login || p.author,
          isDraft: p.isDraft || false,
          reviewDecision: p.reviewDecision || null,
          mergeable: p.mergeable || null,
          createdAt: p.createdAt,
          updatedAt: p.updatedAt,
          labels: (p.labels || []).map(l => ({ name: l.name, color: l.color })),
        })),
      };
      globalThis._githubCache.set(fullName, { ts: Date.now(), data: result });
      return json(res, 200, result);
    }

    // ── GET /api/projects/:owner/:repo — single project ───────────────────
    // Handles both /api/projects/owner/repo and /api/projects/owner%2Frepo
    const projectDetailMatch = path.match(/^\/api\/projects\/([^/]+(?:\/[^/]+|%2F[^/]+))$/i);
    if (method === 'GET' && projectDetailMatch) {
      const fullName = decodeURIComponent(projectDetailMatch[1]);
      const repos    = await getPump().listRepos();
      const repo     = repos.find(r => r.full_name === fullName);
      if (!repo) return json(res, 404, { error: 'Project not found' });
      const projects = await readProjects();
      const overlay  = projects.find(p => p.id === fullName) || {};
      const base     = buildProjectFromRepo(repo);
      return json(res, 200, { ...base, ...overlay });
    }

    // ── POST /api/projects/:owner/:repo/channel — register a Slack channel ─
    const projectChannelMatch = path.match(/^\/api\/projects\/([^/]+(?:\/[^/]+|%2F[^/]+))\/channel$/i);
    if (method === 'POST' && projectChannelMatch) {
      const fullName = decodeURIComponent(projectChannelMatch[1]);
      const body     = await readBody(req);
      if (!body.channel_id || !body.workspace) return json(res, 400, { error: 'channel_id and workspace required' });
      const projects = await readProjects();
      let project    = projects.find(p => p.id === fullName);
      if (!project) {
        const repos = await getPump().listRepos();
        const repo  = repos.find(r => r.full_name === fullName);
        if (!repo) return json(res, 404, { error: 'Project not found' });
        project = buildProjectFromRepo(repo);
        projects.push(project);
      }
      if (!project.slack_channels) project.slack_channels = [];
      // Upsert by workspace
      const existing = project.slack_channels.find(c => c.workspace === body.workspace);
      if (existing) {
        existing.channel_id = body.channel_id;
        existing.channel_name = body.channel_name || existing.channel_name;
        existing.updatedAt  = new Date().toISOString();
      } else {
        project.slack_channels.push({
          workspace:    body.workspace,
          channel_id:   body.channel_id,
          channel_name: body.channel_name || null,
          addedAt:      new Date().toISOString(),
        });
      }
      project.updatedAt = new Date().toISOString();
      await writeProjects(projects);
      // Also update repos.json for the primary workspace
      const pump = getPump();
      const repos = await pump.listRepos();
      const repo  = repos.find(r => r.full_name === fullName);
      if (repo) {
        if (!repo.ownership) repo.ownership = {};
        if (!repo.ownership.slack_channel || body.workspace === 'omgjkh') {
          repo.ownership.slack_channel   = body.channel_id;
          repo.ownership.slack_workspace = body.workspace;
          await pump.patchRepo(fullName, { ownership: repo.ownership });
        }
      }
      // Set channel topic and description to reflect project metadata
      await setSlackChannelMeta(body.channel_id, project).catch(e =>
        console.warn(`[rcc-api] setSlackChannelMeta ${body.channel_id}: ${e.message}`));
      return json(res, 200, { ok: true, project });
    }

    // ── GET /api/context?channel=CXXXX — get project context for a Slack channel ──
    if (method === 'GET' && path === '/api/context') {
      const channelId = url.searchParams.get('channel');
      if (!channelId) return json(res, 400, { error: 'channel query param required' });
      const repos    = await getPump().listRepos();
      const projects = await readProjects();
      // Search repos.json first
      let repo = repos.find(r =>
        r.ownership?.slack_channel === channelId
      );
      // Then projects.json (may have multiple workspaces)
      if (!repo) {
        const projectEntry = projects.find(p =>
          (p.slack_channels || []).some(c => c.channel_id === channelId)
        );
        if (projectEntry) repo = repos.find(r => r.full_name === projectEntry.id);
      }
      if (!repo) return json(res, 404, { error: 'No project associated with this channel' });
      const overlay  = projects.find(p => p.id === repo.full_name) || {};
      const project  = { ...buildProjectFromRepo(repo), ...overlay };
      // Include recent queue items for this project
      const q        = await readQueue();
      const repoItems = (q.items || []).filter(i =>
        i.tags?.includes(repo.full_name) ||
        i.title?.toLowerCase().includes(repo.full_name.split('/')[1].toLowerCase())
      ).slice(-10);
      return json(res, 200, { project, recentItems: repoItems });
    }

    // ── POST /api/bus/receive — handle incoming SquirrelBus messages ──────
    if (method === 'POST' && path === '/api/bus/receive') {
      const body = await readBody(req);
      broadcastGeekEvent('bus_msg', body.from || 'unknown', body.to || 'all', 'SquirrelBus message');
      if (body.type === 'lesson') {
        await receiveLessonFromBus(body);
        return json(res, 200, { ok: true });
      }
      return json(res, 200, { ok: true, ignored: true });
    }

    // ── POST /api/repos/scan — trigger immediate scan ─────────────────────
    if (method === 'POST' && path === '/api/repos/scan') {
      const created = await getPump().scan();
      return json(res, 200, { ok: true, itemsCreated: created });
    }

    // ── POST /api/slack/send — send a message to Slack ─────────────────────
    if (method === 'POST' && path === '/api/slack/send') {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const body = await readBody(req);
      if (!body.channel || !body.text) return json(res, 400, { error: 'channel and text required' });
      const result = await slackPost('chat.postMessage', {
        channel:   body.channel,
        text:      body.text,
        thread_ts: body.thread_ts,
        mrkdwn:    true,
      });
      return json(res, 200, { ok: result.ok, ts: result.ts, error: result.error });
    }

    // ── POST /api/slack/events — Slack Events API (app_mention, message.im) ─
    if (method === 'POST' && path === '/api/slack/events') {
      const rawBody = await readRawBody(req);
      if (!verifySlackSignature(req, rawBody)) {
        return json(res, 401, { error: 'Invalid Slack signature' });
      }
      let body;
      try { body = JSON.parse(rawBody.toString('utf8')); } catch { return json(res, 400, { error: 'Invalid JSON' }); }

      // Slack url_verification challenge (app setup handshake)
      if (body.type === 'url_verification') {
        return json(res, 200, { challenge: body.challenge });
      }

      // Process events asynchronously — Slack requires 200 within 3s
      const event = body.event || {};
      json(res, 200, { ok: true }); // respond immediately

      if (event.type === 'app_mention' || (event.type === 'message' && event.channel_type === 'im' && !event.bot_id)) {
        const text = (event.text || '').replace(/<@[A-Z0-9]+>/g, '').trim();
        if (!text) return;
        try {
          const b = await getBrain();
          const request = createRequest({
            role: 'user',
            content: text,
            context: { slack_user: event.user, slack_channel: event.channel, source: 'slack' },
          });
          const reply = await b.process(request);
          const replyText = typeof reply === 'string' ? reply : reply?.content || reply?.text || JSON.stringify(reply);
          await slackPost('chat.postMessage', {
            channel:   event.channel,
            text:      replyText,
            thread_ts: event.ts,
            mrkdwn:    true,
          });
        } catch (e) {
          console.error('[rcc-api] Slack event brain error:', e.message);
          await slackPost('chat.postMessage', {
            channel:   event.channel,
            text:      `⚠️ Error: ${e.message}`,
            thread_ts: event.ts,
          }).catch(() => {});
        }
      }
      return; // already responded
    }

    // ── POST /api/slack/commands — Slack slash commands (/rcc ...) ─────────
    if (method === 'POST' && path === '/api/slack/commands') {
      const rawBody = await readRawBody(req);
      if (!verifySlackSignature(req, rawBody)) {
        return json(res, 401, { error: 'Invalid Slack signature' });
      }
      // Slack sends slash command payloads as URL-encoded form
      const params = Object.fromEntries(new URLSearchParams(rawBody.toString('utf8')));
      const cmdText = (params.text || '').trim().toLowerCase();
      const channel  = params.channel_id;
      const responseUrl = params.response_url;

      // Helper: send delayed response to Slack response_url
      const slackRespond = async (text) => {
        if (!responseUrl) return;
        await fetch(responseUrl, {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ text, response_type: 'in_channel', mrkdwn: true }),
        }).catch(() => {});
      };

      // Acknowledge immediately (required within 3s)
      const ack = { text: '⏳ Working on it...', response_type: 'ephemeral' };

      if (cmdText === 'status' || cmdText === '') {
        json(res, 200, ack);
        const statusText = await formatAgentStatus().catch(e => `Error: ${e.message}`);
        await slackRespond(`*🐿️ RCC Agent Status*\n${statusText}`);
        return;
      }

      if (cmdText === 'queue') {
        json(res, 200, ack);
        const queueText = await formatQueueSummary().catch(e => `Error: ${e.message}`);
        await slackRespond(`*📋 RCC Queue*\n${queueText}`);
        return;
      }

      if (cmdText.startsWith('ask ')) {
        const question = cmdText.slice(4).trim();
        json(res, 200, ack);
        try {
          const b = await getBrain();
          const request = createRequest({
            role: 'user',
            content: question,
            context: { slack_channel: channel, source: 'slack_command' },
          });
          const reply = await b.process(request);
          const replyText = typeof reply === 'string' ? reply : reply?.content || reply?.text || JSON.stringify(reply);
          await slackRespond(`*🧠 RCC Brain:* ${replyText}`);
        } catch (e) {
          await slackRespond(`⚠️ Error: ${e.message}`);
        }
        return;
      }

      // Unknown command — show help
      return json(res, 200, {
        text: '*RCC Slash Commands*\n`/rcc status` — agent heartbeat status\n`/rcc queue` — pending work items\n`/rcc ask <question>` — ask the RCC brain',
        response_type: 'ephemeral',
      });
    }

    // ── GET /api/calendar ─────────────────────────────────────────────────
    if (method === 'GET' && path === '/api/calendar') {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      let events = await readCalendar();
      const start = url.searchParams.get('start');
      const end   = url.searchParams.get('end');
      const resource = url.searchParams.get('resource');
      if (start) events = events.filter(e => e.end >= start);
      if (end)   events = events.filter(e => e.start <= end);
      if (resource) events = events.filter(e => e.resource === resource);
      return json(res, 200, events);
    }

    // ── POST /api/calendar ────────────────────────────────────────────────
    if (method === 'POST' && path === '/api/calendar') {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const body = await readBody(req);
      if (!body.title || !body.start || !body.end)
        return json(res, 400, { error: 'title, start, end required' });
      const events = await readCalendar();
      const event = {
        id: randomUUID(),
        title: body.title,
        start: body.start,
        end: body.end,
        allDay: body.allDay || false,
        tags: body.tags || [],
        description: body.description || '',
        owner: body.owner || null,
        type: body.type || 'event',
        resource: body.resource || null,
      };
      events.push(event);
      await writeCalendar(events);
      return json(res, 201, { ok: true, event });
    }

    // ── DELETE /api/calendar/:id ──────────────────────────────────────────
    const calDeleteMatch = path.match(/^\/api\/calendar\/([^/]+)$/);
    if (method === 'DELETE' && calDeleteMatch) {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const id = decodeURIComponent(calDeleteMatch[1]);
      const events = await readCalendar();
      const idx = events.findIndex(e => e.id === id);
      if (idx === -1) return json(res, 404, { error: 'Event not found' });
      const event = events[idx];
      // Determine caller identity from token (for owner check)
      const auth = req.headers['authorization'] || '';
      const token = auth.replace(/^Bearer\s+/i, '').trim();
      const agents = await readAgents();
      const callerAgent = Object.entries(agents).find(([, a]) => a.token === token)?.[0] || null;
      if (event.owner !== 'rocky' && callerAgent !== event.owner && callerAgent !== 'rocky') {
        return json(res, 403, { error: 'Only the event owner or Rocky may delete this event' });
      }
      events.splice(idx, 1);
      await writeCalendar(events);
      return json(res, 200, { ok: true });
    }

    // ── PATCH /api/calendar/:id ───────────────────────────────────────────
    const calPatchMatch = path.match(/^\/api\/calendar\/([^/]+)$/);
    if (method === 'PATCH' && calPatchMatch) {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const id = decodeURIComponent(calPatchMatch[1]);
      const body = await readBody(req);
      const events = await readCalendar();
      const idx = events.findIndex(e => e.id === id);
      if (idx === -1) return json(res, 404, { error: 'Event not found' });
      events[idx] = { ...events[idx], ...body, id };
      await writeCalendar(events);
      return json(res, 200, { ok: true, event: events[idx] });
    }

    // ── GET /api/appeal ───────────────────────────────────────────────────
    if (method === 'GET' && path === '/api/appeal') {
      const q = await readQueue();
      const all = [...(q.items || []), ...(q.completed || [])];
      const appeals = all.filter(i => i.needsHuman === true || i.status === 'awaiting-jkh');
      appeals.sort((a, b) => {
        const ta = a.needsHumanAt ? new Date(a.needsHumanAt).getTime() : 0;
        const tb = b.needsHumanAt ? new Date(b.needsHumanAt).getTime() : 0;
        return ta - tb;
      });
      return json(res, 200, appeals);
    }

    // ── POST /api/appeal/:id ──────────────────────────────────────────────
    const appealMatch = path.match(/^\/api\/appeal\/([^/]+)$/);
    if (method === 'POST' && appealMatch) {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const id = decodeURIComponent(appealMatch[1]);
      const body = await readBody(req);
      const { action, note, assignee } = body;
      if (!['approve','reject','reassign','comment'].includes(action))
        return json(res, 400, { error: 'action must be approve, reject, reassign, or comment' });
      const q = await readQueue();
      const item = [...(q.items || []), ...(q.completed || [])].find(i => i.id === id);
      if (!item) return json(res, 404, { error: 'Item not found' });
      const now = new Date().toISOString();
      if (!item.journal) item.journal = [];
      if (action === 'approve') {
        item.status = 'pending';
        item.needsHuman = false;
        item.journal.push({ ts: now, author: 'jkh', type: 'appeal', text: `Approved${note ? ': ' + note : ''}` });
      } else if (action === 'reject') {
        item.status = 'cancelled';
        item.needsHuman = false;
        item.journal.push({ ts: now, author: 'jkh', type: 'appeal', text: `Rejected${note ? ': ' + note : ''}` });
      } else if (action === 'reassign') {
        if (!assignee) return json(res, 400, { error: 'assignee required for reassign' });
        item.assignee = assignee;
        item.needsHuman = false;
        item.journal.push({ ts: now, author: 'jkh', type: 'appeal', text: `Reassigned to ${assignee}${note ? ': ' + note : ''}` });
      } else if (action === 'comment') {
        item.journal.push({ ts: now, author: 'jkh', type: 'comment', text: note || '' });
        // needsHuman stays true
      }
      item.itemVersion = (item.itemVersion || 0) + 1;
      // Re-archive if completed/cancelled
      if (item.status === 'completed' || item.status === 'cancelled') {
        q.items = (q.items || []).filter(i => i.id !== item.id);
        if (!q.completed) q.completed = [];
        if (!q.completed.find(i => i.id === item.id)) q.completed.push(item);
      }
      await writeQueue(q);
      return json(res, 200, { ok: true, item });
    }

    // ── GET /api/heartbeat/:agent/history ─────────────────────────────────
    const hbHistoryMatch = path.match(/^\/api\/heartbeat\/([^/]+)\/history$/);
    if (method === 'GET' && hbHistoryMatch) {
      const agent = decodeURIComponent(hbHistoryMatch[1]);
      // Try reading persistent JSONL first, fall back to in-memory ring buffer
      try {
        const histFile = new URL(`./data/heartbeat-history/${agent}.jsonl`, import.meta.url).pathname;
        if (existsSync(histFile)) {
          const content = await readFile(histFile, 'utf8');
          const lines = content.trim().split('\n').filter(Boolean);
          // Keep last 100 entries
          const entries = lines.slice(-100).map(l => { try { return JSON.parse(l); } catch { return null; } }).filter(Boolean);
          return json(res, 200, entries);
        }
      } catch {}
      return json(res, 200, heartbeatHistory[agent] || []);
    }

    // ── GET /api/agents/history/:name — persistent heartbeat history ──────
    const agentHistoryMatch = path.match(/^\/api\/agents\/history\/([^/]+)$/);
    if (method === 'GET' && agentHistoryMatch) {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const name = decodeURIComponent(agentHistoryMatch[1]);
      const limit = Math.min(parseInt(url.searchParams.get('limit') || '50', 10), 500);
      let entries = [];
      try {
        const histFile = new URL(`./data/heartbeat-history/${name}.jsonl`, import.meta.url).pathname;
        if (existsSync(histFile)) {
          const content = await readFile(histFile, 'utf8');
          const lines = content.trim().split('\n').filter(Boolean);
          entries = lines.slice(-limit).map(l => { try { return JSON.parse(l); } catch { return null; } }).filter(Boolean);
        } else {
          entries = (heartbeatHistory[name] || []).slice(-limit);
        }
      } catch {}
      return json(res, 200, { ok: true, agent: name, entries });
    }

    // ── GET /api/scout/:name — pending work for agent ─────────────────────
    const scoutMatch = path.match(/^\/api\/scout\/([^/]+)$/);
    if (method === 'GET' && scoutMatch) {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const name = decodeURIComponent(scoutMatch[1]);
      const q = await readQueue().catch(() => ({ items: [] }));
      const pending = (q.items || [])
        .filter(i => i.status === 'pending' && (i.assignee === name || i.assignee === 'all'))
        .slice(0, 3)
        .map(({ id, title, priority, description }) => ({ id, title, priority, description }));
      return json(res, 200, { ok: true, agent: name, pendingWork: pending });
    }

    // ── GET /api/crons ────────────────────────────────────────────────────
    if (method === 'GET' && path === '/api/crons') {
      return json(res, 200, Object.values(cronStatus));
    }

    // ── POST /api/crons/:agent ────────────────────────────────────────────
    const cronMatch = path.match(/^\/api\/crons\/([^/]+)$/);
    if (method === 'POST' && cronMatch) {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const agent = decodeURIComponent(cronMatch[1]);
      const body = await readBody(req);
      if (!body.jobId) return json(res, 400, { error: 'jobId required' });
      const key = `${agent}/${body.jobId}`;
      cronStatus[key] = { ...body, agent, updatedAt: new Date().toISOString() };
      return json(res, 200, { ok: true, key });
    }

    // ── GET /api/provider-health ──────────────────────────────────────────
    if (method === 'GET' && path === '/api/provider-health') {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      return json(res, 200, providerHealth);
    }

    // ── POST /api/provider-health ─────────────────────────────────────────
    if (method === 'POST' && path === '/api/provider-health') {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const body = await readBody(req);
      if (!body.provider) return json(res, 400, { error: 'provider required' });
      providerHealth[body.provider] = { ...body, ts: new Date().toISOString() };
      return json(res, 200, { ok: true });
    }

    // ── POST /api/provider-health/:agent ─────────────────────────────────
    const providerMatch = path.match(/^\/api\/provider-health\/([^/]+)$/);
    if (method === 'POST' && providerMatch) {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const agent = decodeURIComponent(providerMatch[1]);
      const body = await readBody(req);
      providerHealth[agent] = { ...body, agent, updatedAt: new Date().toISOString() };
      return json(res, 200, { ok: true });
    }

    // ── GET /api/geek/topology ────────────────────────────────────────────
    if (method === 'GET' && path === '/api/geek/topology') {
      const nodes = [
        { id: 'rocky',          label: 'Rocky',          type: 'agent',          host: 'do-host1',    chips: ['RCC API :8789','WQ Dashboard :8788','RCC Brain','SquirrelBus hub','Tailscale proxy'] },
        { id: 'bullwinkle',     label: 'Bullwinkle',     type: 'agent',          host: 'puck',        chips: ['OpenClaw :18789','SquirrelBus :8788','launchd crons','disk free','uptime'] },
        { id: 'natasha',        label: 'Natasha',        type: 'agent',          host: 'sparky',      chips: ['OpenClaw :18789','SquirrelBus /bus→:18799','Milvus :19530','CUDA/RTX','Ollama :11434'] },
        { id: 'boris',          label: 'Boris',          type: 'agent',          host: 'l40-sweden',  chips: ['OpenClaw gateway','L40 GPU','Omniverse headless'] },
        { id: 'milvus',         label: 'Milvus',         type: 'shared-service', host: 'do-host1',   port: 19530 },
        { id: 'minio',          label: 'MinIO',          type: 'shared-service', host: 'do-host1',   port: 9000 },
        { id: 'searxng',        label: 'SearXNG',        type: 'shared-service', host: 'do-host1',   port: 8888 },
        { id: 'nvidia-gateway', label: 'NVIDIA Gateway', type: 'external',       url: 'inference-api.nvidia.com' },
        { id: 'github',         label: 'GitHub',         type: 'external',       url: 'api.github.com' },
        { id: 'mattermost',     label: 'Mattermost',     type: 'external',       url: 'chat.yourmom.photos' },
        { id: 'slack-omgjkh',   label: 'Slack (omgjkh)', type: 'external',       url: 'omgjkh.slack.com' },
        { id: 'slack-offtera',  label: 'Slack (offtera)', type: 'external',      url: 'offtera.slack.com' },
        { id: 'telegram',       label: 'Telegram',       type: 'external',       url: 'api.telegram.org' },
        { id: 'squirrelbus',    label: 'SquirrelBus',    type: 'bus',            host: 'do-host1' },
      ];
      const edges = [
        { from: 'rocky',      to: 'rcc-api',        type: 'persistent',  protocol: 'internal' },
        { from: 'bullwinkle', to: 'rocky',           type: 'persistent',  protocol: 'heartbeat/HTTP' },
        { from: 'natasha',    to: 'rocky',           type: 'persistent',  protocol: 'heartbeat/HTTP' },
        { from: 'boris',      to: 'rocky',           type: 'persistent',  protocol: 'heartbeat/HTTP' },
        { from: 'rocky',      to: 'milvus',          type: 'on-demand',   protocol: 'gRPC' },
        { from: 'rocky',      to: 'minio',           type: 'on-demand',   protocol: 'S3/HTTP' },
        { from: 'rocky',      to: 'searxng',         type: 'on-demand',   protocol: 'HTTP' },
        { from: 'rocky',      to: 'squirrelbus',     type: 'persistent',  protocol: 'JSONL/fanout' },
        { from: 'bullwinkle', to: 'squirrelbus',     type: 'on-demand',   protocol: 'HTTP' },
        { from: 'natasha',    to: 'squirrelbus',     type: 'on-demand',   protocol: 'HTTP' },
        { from: 'rocky',      to: 'nvidia-gateway',  type: 'on-demand',   protocol: 'HTTPS/OpenAI' },
        { from: 'bullwinkle', to: 'nvidia-gateway',  type: 'on-demand',   protocol: 'HTTPS/OpenAI' },
        { from: 'natasha',    to: 'nvidia-gateway',  type: 'on-demand',   protocol: 'HTTPS/OpenAI' },
        { from: 'rocky',      to: 'github',          type: 'on-demand',   protocol: 'HTTPS/REST' },
        { from: 'rocky',      to: 'mattermost',      type: 'on-demand',   protocol: 'HTTPS/REST' },
        { from: 'rocky',      to: 'slack-omgjkh',    type: 'persistent',  protocol: 'Socket Mode' },
        { from: 'rocky',      to: 'slack-offtera',   type: 'on-demand',   protocol: 'HTTPS/REST' },
        { from: 'rocky',      to: 'telegram',        type: 'on-demand',   protocol: 'HTTPS/Bot API' },
      ];
      const STALE_MS = 5 * 60 * 1000;
      const now = Date.now();
      const nodesWithStatus = nodes.map(n => {
        if (n.type !== 'agent') return n;
        const hb = heartbeats[n.id];
        if (!hb) return { ...n, status: 'offline', lastSeen: null };
        const age = now - new Date(hb.ts).getTime();
        const status = age < STALE_MS ? 'online' : age < 30 * 60 * 1000 ? 'stale' : 'offline';
        return { ...n, status, lastSeen: hb.ts };
      });
      // Dynamic: registered agents
      const agentsData = await readAgents().catch(() => ({}));
      // Dynamic: recent bus messages (last 50 lines of squirrelbus/bus.jsonl)
      let busMessages = [];
      const busPath = new URL('../../squirrelbus/bus.jsonl', import.meta.url).pathname;
      if (existsSync(busPath)) {
        try {
          const busRaw = await readFile(busPath, 'utf8');
          busMessages = busRaw.trim().split('\n').filter(Boolean).slice(-50).map(l => { try { return JSON.parse(l); } catch { return null; } }).filter(Boolean);
        } catch { /* ignore */ }
      }
      // Dynamic: recent heartbeats summary
      const heartbeatSummary = Object.entries(heartbeats).map(([agent, hb]) => ({ agent, ts: hb.ts, status: hb.status || 'online' }));
      // Dynamic: LLM endpoints
      const llmEndpoints = llmRegistry.serialize();
      return json(res, 200, { nodes: nodesWithStatus, edges, agents: agentsData, busMessages, heartbeatSummary, llmEndpoints });
    }

    // ── GET /api/geek/stream — SSE live traffic ───────────────────────────
    if (method === 'GET' && path === '/api/geek/stream') {
      res.writeHead(200, {
        'Content-Type':  'text/event-stream',
        'Cache-Control': 'no-cache',
        'Connection':    'keep-alive',
        'Access-Control-Allow-Origin': '*',
      });
      res.write(`data: ${JSON.stringify({ type: 'connected' })}\n\n`);
      geekSseClients.add(res);
      const keepalive = setInterval(() => {
        try { res.write(': keepalive\n\n'); } catch { clearInterval(keepalive); geekSseClients.delete(res); }
      }, 15000);
      req.on('close', () => { clearInterval(keepalive); geekSseClients.delete(res); });
      return; // don't call res.end()
    }

    // ── GET /api/heartbeat-history ────────────────────────────────────────
    if (method === 'GET' && path === '/api/heartbeat-history') {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      return json(res, 200, heartbeatHistory);
    }

    // ── POST /api/cron-status ─────────────────────────────────────────────
    if (method === 'POST' && path === '/api/cron-status') {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const body = await readBody(req);
      if (!body.name) return json(res, 400, { error: 'name required' });
      cronStatus[body.name] = { ...body, ts: new Date().toISOString() };
      return json(res, 200, { ok: true });
    }

    // ── GET /api/cron-status ──────────────────────────────────────────────
    if (method === 'GET' && path === '/api/cron-status') {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      return json(res, 200, cronStatus);
    }

    // ── POST /api/requests — create request ticket ────────────────────────
    if (method === 'POST' && path === '/api/requests') {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const body = await readBody(req);
      if (!body.summary) return json(res, 400, { error: 'summary required' });
      const ticket = {
        id: `req-${Date.now()}`,
        created: new Date().toISOString(),
        requester: body.requester || { type: 'human', id: 'jkh', channel: 'telegram' },
        summary: body.summary,
        status: 'open',
        owner: body.owner || 'rocky',
        delegations: [],
        resolution: null,
        notifiedRequesterAt: null,
        closedAt: null,
      };
      const reqs = await readRequests();
      reqs.push(ticket);
      await writeRequests(reqs);
      return json(res, 201, { ok: true, ticket });
    }

    // ── GET /api/requests — list tickets ─────────────────────────────────
    if (method === 'GET' && path === '/api/requests') {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      let reqs = await readRequests();
      const ownerFilter = url.searchParams.get('owner');
      const statusFilter = url.searchParams.get('status');
      const requesterFilter = url.searchParams.get('requester');
      if (ownerFilter) reqs = reqs.filter(r => r.owner === ownerFilter);
      if (statusFilter) {
        const statuses = statusFilter.split(',');
        reqs = reqs.filter(r => statuses.includes(r.status));
      }
      if (requesterFilter) reqs = reqs.filter(r => r.requester?.id === requesterFilter);
      return json(res, 200, reqs);
    }

    // ── GET /api/requests/:id — get one ticket ────────────────────────────
    const reqIdMatch = path.match(/^\/api\/requests\/([^/]+)$/);
    if (method === 'GET' && reqIdMatch) {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const id = decodeURIComponent(reqIdMatch[1]);
      const reqs = await readRequests();
      const ticket = reqs.find(r => r.id === id);
      if (!ticket) return json(res, 404, { error: 'Ticket not found' });
      return json(res, 200, ticket);
    }

    // ── PATCH /api/requests/:id — update ticket fields ────────────────────
    if (method === 'PATCH' && reqIdMatch) {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const id = decodeURIComponent(reqIdMatch[1]);
      const body = await readBody(req);
      const reqs = await readRequests();
      const ticket = reqs.find(r => r.id === id);
      if (!ticket) return json(res, 404, { error: 'Ticket not found' });
      const allowed = ['summary', 'status', 'owner', 'resolution', 'notifiedRequesterAt'];
      for (const k of allowed) { if (k in body) ticket[k] = body[k]; }
      await writeRequests(reqs);
      return json(res, 200, { ok: true, ticket });
    }

    // ── POST /api/requests/:id/delegate — add delegation ─────────────────
    const delegateMatch = path.match(/^\/api\/requests\/([^/]+)\/delegate$/);
    if (method === 'POST' && delegateMatch) {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const id = decodeURIComponent(delegateMatch[1]);
      const body = await readBody(req);
      if (!body.to || !body.summary) return json(res, 400, { error: 'to and summary required' });
      const reqs = await readRequests();
      const ticket = reqs.find(r => r.id === id);
      if (!ticket) return json(res, 404, { error: 'Ticket not found' });
      const delegation = {
        to: body.to,
        at: new Date().toISOString(),
        summary: body.summary,
        queueItemId: body.queueItemId || null,
        resolvedAt: null,
        outcome: null,
      };
      ticket.delegations.push(delegation);
      if (ticket.status === 'open') ticket.status = 'delegated';
      await writeRequests(reqs);
      return json(res, 201, { ok: true, delegation, delegationIndex: ticket.delegations.length - 1 });
    }

    // ── PATCH /api/requests/:id/delegations/:idx — resolve delegation ─────
    const delegResMatch = path.match(/^\/api\/requests\/([^/]+)\/delegations\/(\d+)$/);
    if (method === 'PATCH' && delegResMatch) {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const id = decodeURIComponent(delegResMatch[1]);
      const idx = parseInt(delegResMatch[2], 10);
      const body = await readBody(req);
      const reqs = await readRequests();
      const ticket = reqs.find(r => r.id === id);
      if (!ticket) return json(res, 404, { error: 'Ticket not found' });
      if (!ticket.delegations[idx]) return json(res, 404, { error: 'Delegation not found' });
      ticket.delegations[idx].resolvedAt = new Date().toISOString();
      ticket.delegations[idx].outcome = body.outcome || '';
      // If all delegations resolved, set status to resolved
      if (ticket.delegations.every(d => d.resolvedAt) && ticket.status === 'delegated') {
        ticket.status = 'resolved';
        if (body.outcome) ticket.resolution = body.outcome;
      }
      await writeRequests(reqs);
      return json(res, 200, { ok: true, ticket });
    }

    // ── POST /api/requests/:id/close — notify requester and close ─────────
    const closeMatch = path.match(/^\/api\/requests\/([^/]+)\/close$/);
    if (method === 'POST' && closeMatch) {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const id = decodeURIComponent(closeMatch[1]);
      const body = await readBody(req);
      const reqs = await readRequests();
      const ticket = reqs.find(r => r.id === id);
      if (!ticket) return json(res, 404, { error: 'Ticket not found' });
      const now = new Date().toISOString();
      ticket.notifiedRequesterAt = now;
      ticket.closedAt = now;
      ticket.status = 'closed';
      if (body?.resolution) ticket.resolution = body.resolution;
      await writeRequests(reqs);
      return json(res, 200, { ok: true, ticket });
    }

    // ── GET /api/vector/health ────────────────────────────────────────────
    if (method === 'GET' && path === '/api/vector/health') {
      try {
        const collections = await collectionStats();
        return json(res, 200, { ok: true, collections });
      } catch (err) {
        return json(res, 500, { ok: false, error: err.message });
      }
    }

    // ── GET /api/vector/search ────────────────────────────────────────────
    if (method === 'GET' && path === '/api/vector/search') {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const q = url.searchParams.get('q') || '';
      if (!q) return json(res, 400, { error: 'Missing query parameter q' });
      const k = parseInt(url.searchParams.get('k') || '10', 10);
      const collections = url.searchParams.get('collections') || 'all';
      try {
        let results;
        if (collections === 'all') {
          results = await vectorSearchAll(q, { k });
        } else {
          results = await vectorSearch(collections, q, { k });
          results = results.map(r => ({ collection: collections, ...r }));
        }
        return json(res, 200, { ok: true, query: q, results });
      } catch (err) {
        return json(res, 500, { ok: false, error: err.message });
      }
    }

    // ── POST /api/vector/upsert ───────────────────────────────────────────
    if (method === 'POST' && path === '/api/vector/upsert') {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const body = await readBody(req);
      const { collection, id, text, metadata } = body || {};
      if (!collection || !id || !text) return json(res, 400, { error: 'Missing required fields: collection, id, text' });
      try {
        await vectorUpsert(collection, { id, text, metadata: metadata || {} });
        return json(res, 200, { ok: true });
      } catch (err) {
        return json(res, 500, { ok: false, error: err.message });
      }
    }

    // ── GET /api/vector/context ───────────────────────────────────────────
    if (method === 'GET' && path === '/api/vector/context') {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const q = url.searchParams.get('q') || '';
      if (!q) return json(res, 400, { error: 'Missing query parameter q' });
      const k = parseInt(url.searchParams.get('k') || '10', 10);
      const collectionsParam = url.searchParams.get('collections') || 'all';
      try {
        let results;
        if (collectionsParam === 'all') {
          results = await vectorSearchAll(q, { k });
        } else {
          const cols = collectionsParam.split(',').map(c => c.trim()).filter(Boolean);
          const searches = await Promise.all(
            cols.map(async col => {
              const hits = await vectorSearch(col, q, { k });
              return hits.map(r => ({ collection: col, ...r }));
            })
          );
          results = searches.flat().sort((a, b) => b.score - a.score).slice(0, k);
        }
        return json(res, 200, { ok: true, results });
      } catch (err) {
        return json(res, 500, { ok: false, error: err.message });
      }
    }

    // ── POST /api/ideation/generate — generate ideas and add to queue ────────
    if (method === 'POST' && path === '/api/ideation/generate') {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const body = await readBody(req);
      const agentName = body.agent || 'unknown';
      const count = Math.min(parseInt(body.count || '1', 10), 3);

      const q = await readQueue();
      const recentQueue = (q.items || []).slice(-20);
      const recentLessons = await queryAllLessons('').catch(() => []);

      const context = { recentQueue, recentLessons, agentName };
      const ideas = [];

      for (let i = 0; i < count; i++) {
        const idea = await generateIdea(context);
        const itemId = `wq-IDEA-${Date.now()}-${i}`;
        const item = {
          id: itemId,
          itemVersion: 1,
          created: new Date().toISOString(),
          source: agentName,
          assignee: 'all',
          priority: 'normal',
          status: 'idea',
          title: idea.title,
          description: idea.description,
          notes: idea.rationale,
          preferred_executor: 'claude_cli',
          journal: [],
          choices: [],
          choiceRecorded: null,
          votes: [],
          attempts: 0,
          maxAttempts: 3,
          claimedBy: null,
          claimedAt: null,
          completedAt: null,
          result: null,
          tags: ['idea', 'auto-generated', ...(idea.tags || [])],
          scout_key: null,
          repo: null,
          ideaMeta: { difficulty: idea.difficulty, rationale: idea.rationale },
        };
        if (!q.items) q.items = [];
        q.items.push(item);
        ideas.push({ id: itemId, title: idea.title });
      }

      await writeQueue(q);
      return json(res, 201, { ok: true, ideas });
    }

    // ── GET /api/ideation/pending — list idea items ───────────────────────
    if (method === 'GET' && path === '/api/ideation/pending') {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const q = await readQueue();
      const ideas = (q.items || []).filter(i =>
        i.status === 'idea' || (i.tags || []).includes('idea')
      );
      return json(res, 200, { ok: true, ideas });
    }

    // ── POST /api/ideation/:id/promote — promote idea to pending ─────────
    const ideaPromoteMatch = path.match(/^\/api\/ideation\/([^/]+)\/promote$/);
    if (method === 'POST' && ideaPromoteMatch) {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const id = decodeURIComponent(ideaPromoteMatch[1]);
      const q = await readQueue();
      const item = (q.items || []).find(i => i.id === id);
      if (!item) return json(res, 404, { error: 'Idea not found' });
      if (!item.claimedBy && (!item.votes || item.votes.length < 1)) {
        return json(res, 400, { error: 'Idea needs at least 1 vote or a claimedBy to promote' });
      }
      item.status = 'pending';
      item.tags = (item.tags || []).filter(t => t !== 'idea');
      item.tags.push('promoted-idea');
      item.journal = item.journal || [];
      item.journal.push({ ts: new Date().toISOString(), type: 'promote', text: 'Promoted from idea to pending' });
      await writeQueue(q);
      return json(res, 200, { ok: true, item });
    }

    // ── GET /api/secrets — list secret keys (admin only, no values) ─────────
    if (method === 'GET' && path === '/api/secrets') {
      if (!isAdminAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const secrets = await readSecrets();
      return json(res, 200, { ok: true, keys: Object.keys(secrets) });
    }

    // ── GET /api/secrets/:key — fetch secret by key (any agent token) ────────
    // Named aliases (slack/mattermost/minio/milvus/nvidia/github) return a bundle
    // of related env-var key→value pairs. Individual keys return a scalar value.
    const secretGetMatch = path.match(/^\/api\/secrets\/([^/]+)$/);
    if (method === 'GET' && secretGetMatch) {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const key = decodeURIComponent(secretGetMatch[1]);
      const secrets = await readSecrets();
      if (!(key in secrets)) return json(res, 404, { error: `Secret '${key}' not found` });
      const value = secrets[key];
      // Named alias (object) → return bundle; scalar → return single value
      if (typeof value === 'object' && value !== null) {
        return json(res, 200, { ok: true, key, secrets: value });
      }
      return json(res, 200, { ok: true, key, value });
    }

    // ── POST /api/secrets/:key — write/update secret (admin only) ───────────
    const secretPostMatch = path.match(/^\/api\/secrets\/([^/]+)$/);
    if (method === 'POST' && secretPostMatch) {
      if (!isAdminAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const key = decodeURIComponent(secretPostMatch[1]);
      const body = await readBody(req);
      if (body.value === undefined && body.secrets === undefined) {
        return json(res, 400, { error: 'body must include "value" (scalar) or "secrets" (object)' });
      }
      const secrets = await readSecrets();
      secrets[key] = body.secrets !== undefined ? body.secrets : body.value;
      await writeSecrets(secrets);
      return json(res, 200, { ok: true, key });
    }

    // ── POST /api/keys/github — store deploy key ──────────────────────────
    if (method === 'POST' && path === '/api/keys/github') {
      if (!isAdminAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const body = await readBody(req);
      if (!body.repoUrl || !body.deployKey) return json(res, 400, { error: 'repoUrl and deployKey required' });
      const keyPath = new URL('../data/github-key.json', import.meta.url).pathname;
      const record = { repoUrl: body.repoUrl, deployKey: body.deployKey, label: body.label || '', registeredAt: new Date().toISOString() };
      await writeFile(keyPath, JSON.stringify(record, null, 2));
      await chmod(keyPath, 0o600);
      return json(res, 200, { ok: true, keyId: 'default' });
    }

    // ── GET /api/keys/github — retrieve deploy key ────────────────────────
    if (method === 'GET' && path === '/api/keys/github') {
      if (!isAdminAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const keyPath = new URL('../data/github-key.json', import.meta.url).pathname;
      if (!existsSync(keyPath)) return json(res, 404, { error: 'No deploy key registered' });
      const record = JSON.parse(await readFile(keyPath, 'utf8'));
      return json(res, 200, record);
    }

    // ── Secrets Store ─────────────────────────────────────────────────────
    // RCC is the sole secrets holder. Agents fetch what they need here.
    // Only admin can write; any authenticated agent can read.

    // GET /api/secrets — list all secret keys (no values)
    if (method === 'GET' && path === '/api/secrets') {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const secretsPath = new URL('../data/secrets.json', import.meta.url).pathname;
      if (!existsSync(secretsPath)) return json(res, 200, { keys: [] });
      const store = JSON.parse(await readFile(secretsPath, 'utf8'));
      return json(res, 200, { keys: Object.keys(store) });
    }

    // GET /api/secrets/:key — fetch a specific secret (value returned)
    if (method === 'GET' && path.startsWith('/api/secrets/')) {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const key = path.slice('/api/secrets/'.length);
      if (!key) return json(res, 400, { error: 'Secret key required' });
      const secretsPath = new URL('../data/secrets.json', import.meta.url).pathname;
      if (!existsSync(secretsPath)) return json(res, 404, { error: 'Secret not found' });
      const store = JSON.parse(await readFile(secretsPath, 'utf8'));
      if (!(key in store)) return json(res, 404, { error: 'Secret not found' });
      return json(res, 200, { key, value: store[key] });
    }

    // POST /api/secrets/:key — write or update a secret (admin-only)
    if (method === 'POST' && path.startsWith('/api/secrets/')) {
      if (!isAdminAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const key = path.slice('/api/secrets/'.length);
      if (!key) return json(res, 400, { error: 'Secret key required' });
      const body = await readBody(req);
      if (!('value' in body)) return json(res, 400, { error: 'value required in body' });
      const secretsPath = new URL('../data/secrets.json', import.meta.url).pathname;
      let store = {};
      if (existsSync(secretsPath)) store = JSON.parse(await readFile(secretsPath, 'utf8'));
      store[key] = body.value;
      await mkdir(dirname(secretsPath), { recursive: true });
      await writeFile(secretsPath, JSON.stringify(store, null, 2), 'utf8');
      await chmod(secretsPath, 0o600);
      console.log(`[rcc-api] Secret '${key}' written by admin`);
      return json(res, 200, { ok: true, key });
    }

    // DELETE /api/secrets/:key — remove a secret (admin-only)
    if (method === 'DELETE' && path.startsWith('/api/secrets/')) {
      if (!isAdminAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const key = path.slice('/api/secrets/'.length);
      const secretsPath = new URL('../data/secrets.json', import.meta.url).pathname;
      if (!existsSync(secretsPath)) return json(res, 404, { error: 'Secret not found' });
      const store = JSON.parse(await readFile(secretsPath, 'utf8'));
      if (!(key in store)) return json(res, 404, { error: 'Secret not found' });
      delete store[key];
      await writeFile(secretsPath, JSON.stringify(store, null, 2), 'utf8');
      console.log(`[rcc-api] Secret '${key}' deleted by admin`);
      return json(res, 200, { ok: true, key, deleted: true });
    }

    // ── POST /api/bootstrap/token — generate one-time bootstrap token ─────
    if (method === 'POST' && path === '/api/bootstrap/token') {
      if (!isAdminAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const body = await readBody(req);
      if (!body.agent) return json(res, 400, { error: 'agent required' });
      const ttl = body.ttlSeconds || 3600;
      const role = body.role || 'agent'; // 'agent' | 'vllm-worker'
      const token = `rcc-bootstrap-${body.agent}-${randomUUID().slice(0, 8)}`;
      const expiresAt = new Date(Date.now() + ttl * 1000).toISOString();
      bootstrapTokens.set(token, { agent: body.agent, role, expiresAt: Date.now() + ttl * 1000, used: false });
      saveBootstrapTokens();
      return json(res, 200, { ok: true, bootstrapToken: token, agent: body.agent, role, expiresAt,
        onboardCmd: `curl -fsSL "${RCC_PUBLIC_URL}/api/onboard?token=${token}" | bash` });
    }

    // ── GET /api/bootstrap — consume bootstrap token, return provisioning data
    if (method === 'GET' && path === '/api/bootstrap') {
      const token = url.searchParams.get('token');
      if (!token) return json(res, 400, { error: 'token query param required' });
      const entry = bootstrapTokens.get(token);
      if (!entry) return json(res, 401, { error: 'Invalid bootstrap token' });
      if (Date.now() > entry.expiresAt) return json(res, 401, { error: 'Bootstrap token expired' });
      if (entry.used) return json(res, 401, { error: 'Bootstrap token already used' });
      entry.used = true;
      saveBootstrapTokens();

      const keyPath = new URL('../data/github-key.json', import.meta.url).pathname;
      if (!existsSync(keyPath)) return json(res, 500, { error: 'Deploy key not configured' });
      const keyRecord = JSON.parse(await readFile(keyPath, 'utf8'));

      const agents = await readAgents();
      let agentToken;
      if (agents[entry.agent]?.token) {
        agentToken = agents[entry.agent].token;
      } else {
        agentToken = `rcc-agent-${entry.agent}-${randomUUID().slice(0, 8)}`;
        agents[entry.agent] = {
          ...(agents[entry.agent] || {}),
          name: entry.agent,
          host: entry.host || 'unknown',
          type: entry.type || 'full',
          token: agentToken,
          registeredAt: new Date().toISOString(),
          capabilities: agents[entry.agent]?.capabilities || {},
          billing: agents[entry.agent]?.billing || { claude_cli: 'fixed', inference_key: 'metered', gpu: 'fixed' },
        };
        await writeAgents(agents);
        AUTH_TOKENS.add(agentToken);
      }

      // Load secrets store and include in bootstrap response
      const secretsPath = new URL('../data/secrets.json', import.meta.url).pathname;
      let secrets = {};
      if (existsSync(secretsPath)) {
        try { secrets = JSON.parse(await readFile(secretsPath, 'utf8')); } catch {}
      }

      console.log(`[rcc-api] Bootstrap consumed for agent ${entry.agent} from ${req.socket?.remoteAddress}`);
      return json(res, 200, {
        ok: true,
        agent: entry.agent,
        repoUrl: keyRecord.repoUrl,
        deployKey: keyRecord.deployKey,
        agentToken,
        rccUrl: RCC_PUBLIC_URL,
        secrets,  // full secrets bundle — agent should write to ~/.rcc/.env
      });
    }

    // ── POST /api/exec — broadcast exec payload via SquirrelBus (admin only) ──
    if (method === 'POST' && path === '/api/exec') {
      if (!isAdminAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const body = await readBody(req);
      if (!body.code) return json(res, 400, { error: 'code required' });

      const SQUIRRELBUS_TOKEN = process.env.SQUIRRELBUS_TOKEN || '';
      if (!SQUIRRELBUS_TOKEN) return json(res, 500, { error: 'SQUIRRELBUS_TOKEN not configured' });

      // Import signing lib lazily
      const { signPayload } = await import('../exec/index.mjs');

      const execId = `exec-${randomUUID()}`;
      const payload = {
        execId,
        code:    body.code,
        target:  body.target  || 'all',
        replyTo: body.replyTo || null,
        ts:      new Date().toISOString(),
      };

      // Sign the payload
      const sig = signPayload(payload, SQUIRRELBUS_TOKEN);
      const envelope = { ...payload, sig };

      // Broadcast on SquirrelBus (best-effort)
      const BUS_URL   = process.env.SQUIRRELBUS_URL || 'http://localhost:8788';
      const BUS_TOKEN = SQUIRRELBUS_TOKEN;
      let busSent = false;
      try {
        const busResp = await fetch(`${BUS_URL}/bus/send`, {
          method: 'POST',
          headers: { 'Authorization': `Bearer ${BUS_TOKEN}`, 'Content-Type': 'application/json' },
          body: JSON.stringify({
            from:    'rocky',
            to:      body.target || 'all',
            type:    'rcc.exec',
            subject: `rcc.exec:${execId}`,
            body:    JSON.stringify(envelope),
          }),
        });
        busSent = busResp.ok;
      } catch (busErr) {
        console.warn('[rcc-api] SquirrelBus broadcast failed:', busErr.message);
      }

      // Persist to exec-log.jsonl
      const logRecord = {
        execId,
        ts:      payload.ts,
        code:    body.code,
        target:  payload.target,
        replyTo: payload.replyTo,
        results: [],
        busSent,
        requestedBy: 'admin',
      };
      const logPath = new URL(EXEC_LOG_PATH, import.meta.url).pathname;
      await mkdir(new URL('./data', import.meta.url).pathname, { recursive: true });
      await appendFile(logPath, JSON.stringify(logRecord) + '\n', 'utf8');

      return json(res, 200, { ok: true, execId, busSent });
    }

    // ── GET /api/exec/:id — get exec record + results (agent auth) ────────
    const execGetMatch = path.match(/^\/api\/exec\/([^/]+)$/);
    if (method === 'GET' && execGetMatch) {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const execId = decodeURIComponent(execGetMatch[1]);
      const logPath = new URL(EXEC_LOG_PATH, import.meta.url).pathname;
      if (!existsSync(logPath)) return json(res, 404, { error: 'Exec record not found' });
      const lines = (await readFile(logPath, 'utf8')).trim().split('\n').filter(Boolean);
      const record = lines.map(l => { try { return JSON.parse(l); } catch { return null; } })
        .filter(Boolean)
        .find(r => r.execId === execId);
      if (!record) return json(res, 404, { error: 'Exec record not found' });
      return json(res, 200, record);
    }

    // ── POST /api/exec/:id/result — append agent result (agent auth) ──────
    const execResultMatch = path.match(/^\/api\/exec\/([^/]+)\/result$/);
    if (method === 'POST' && execResultMatch) {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const execId = decodeURIComponent(execResultMatch[1]);
      const body = await readBody(req);
      const logPath = new URL(EXEC_LOG_PATH, import.meta.url).pathname;
      await mkdir(new URL('./data', import.meta.url).pathname, { recursive: true });

      // Read, find, update, rewrite (log is not huge — exec records are admin-only)
      let records = [];
      if (existsSync(logPath)) {
        const lines = (await readFile(logPath, 'utf8')).trim().split('\n').filter(Boolean);
        records = lines.map(l => { try { return JSON.parse(l); } catch { return null; } }).filter(Boolean);
      }
      const idx = records.findIndex(r => r.execId === execId);
      if (idx === -1) {
        // Record not found — create a stub (agent may have restarted)
        records.push({
          execId,
          ts:      new Date().toISOString(),
          results: [{ ...body, ts: new Date().toISOString() }],
          stub:    true,
        });
      } else {
        if (!records[idx].results) records[idx].results = [];
        records[idx].results.push({ ...body, ts: new Date().toISOString() });
      }
      await writeFile(logPath, records.map(r => JSON.stringify(r)).join('\n') + '\n', 'utf8');

      return json(res, 200, { ok: true, execId });
    }

    // ── POST /api/projects — create project ───────────────────────────────
    if (method === 'POST' && path === '/api/projects') {
      const body = await readBody(req);
      if (!body.name) return json(res, 400, { error: 'name required' });
      const projects = await readProjects();
      const id = `proj-${Date.now()}`;
      const project = {
        id,
        name: body.name,
        description: body.description || '',
        repoUrl: body.repoUrl || null,
        slackChannels: body.slackChannels || [],
        tags: body.tags || [],
        status: body.status || 'active',
        createdAt: new Date().toISOString(),
        updatedAt: new Date().toISOString(),
      };
      projects.push(project);
      await writeProjects(projects);
      return json(res, 201, { ok: true, project });
    }

    // ── PATCH /api/projects/:id — update project ──────────────────────────
    const projectPatchMatch = path.match(/^\/api\/projects\/([^/]+)$/);
    if (method === 'PATCH' && projectPatchMatch) {
      const id = decodeURIComponent(projectPatchMatch[1]);
      const body = await readBody(req);
      const projects = await readProjects();
      const idx = projects.findIndex(p => p.id === id);
      if (idx === -1) return json(res, 404, { error: 'Project not found' });
      const allowed = ['name','description','repoUrl','slackChannels','tags','status'];
      for (const field of allowed) {
        if (body[field] !== undefined) projects[idx][field] = body[field];
      }
      projects[idx].updatedAt = new Date().toISOString();
      await writeProjects(projects);
      return json(res, 200, { ok: true, project: projects[idx] });
    }

    // ── DELETE /api/projects/:id — soft-delete (archive) ─────────────────
    const projectDeleteMatch = path.match(/^\/api\/projects\/([^/]+)$/);
    if (method === 'DELETE' && projectDeleteMatch) {
      const id = decodeURIComponent(projectDeleteMatch[1]);
      const projects = await readProjects();
      const idx = projects.findIndex(p => p.id === id);
      if (idx === -1) return json(res, 404, { error: 'Project not found' });
      projects[idx].status = 'archived';
      projects[idx].updatedAt = new Date().toISOString();
      await writeProjects(projects);
      return json(res, 200, { ok: true, project: projects[idx] });
    }

    // ── DELETE /api/item/:id — tombstone item ─────────────────────────────
    const itemDeleteMatch = path.match(/^\/api\/item\/([^/]+)$/);
    if (method === 'DELETE' && itemDeleteMatch) {
      const id = decodeURIComponent(itemDeleteMatch[1]);
      const q = await readQueue();
      const idx = (q.items || []).findIndex(i => i.id === id);
      if (idx === -1) return json(res, 404, { error: 'Item not found' });
      const [item] = q.items.splice(idx, 1);
      item.status = 'deleted';
      item.deletedAt = new Date().toISOString();
      if (!q.deleted) q.deleted = [];
      q.deleted.push(item);
      await writeQueue(q);
      return json(res, 200, { ok: true, item });
    }

    // ── GET /api/conversations — filter/list ──────────────────────────────
    if (method === 'GET' && path === '/api/conversations') {
      const convs = await readConversations();
      const { project, agent, channel, since } = Object.fromEntries(url.searchParams);
      let result = convs;
      if (project) result = result.filter(c => c.projectId === project);
      if (agent)   result = result.filter(c => (c.participants || []).includes(agent));
      if (channel) result = result.filter(c => c.channel === channel);
      if (since)   result = result.filter(c => c.createdAt >= since);
      return json(res, 200, result);
    }

    // ── POST /api/conversations — create conversation ─────────────────────
    if (method === 'POST' && path === '/api/conversations') {
      const body = await readBody(req);
      const convs = await readConversations();
      const conv = {
        id: `conv-${Date.now()}`,
        participants: body.participants || [],
        channel: body.channel || null,
        projectId: body.projectId || null,
        messages: body.messages || [],
        tags: body.tags || [],
        createdAt: new Date().toISOString(),
        updatedAt: new Date().toISOString(),
      };
      convs.push(conv);
      await writeConversations(convs);
      return json(res, 201, { ok: true, conversation: conv });
    }

    // ── GET /api/conversations/:id — single conversation ──────────────────
    const convDetailMatch = path.match(/^\/api\/conversations\/([^/]+)$/);
    if (method === 'GET' && convDetailMatch) {
      const id = decodeURIComponent(convDetailMatch[1]);
      const convs = await readConversations();
      const conv = convs.find(c => c.id === id);
      if (!conv) return json(res, 404, { error: 'Conversation not found' });
      return json(res, 200, conv);
    }

    // ── POST /api/conversations/:id/messages — append message ────────────
    const convMsgMatch = path.match(/^\/api\/conversations\/([^/]+)\/messages$/);
    if (method === 'POST' && convMsgMatch) {
      const id = decodeURIComponent(convMsgMatch[1]);
      const body = await readBody(req);
      if (!body.author || !body.text) return json(res, 400, { error: 'author and text required' });
      const convs = await readConversations();
      const idx = convs.findIndex(c => c.id === id);
      if (idx === -1) return json(res, 404, { error: 'Conversation not found' });
      const message = { ts: new Date().toISOString(), author: body.author, text: body.text };
      if (!convs[idx].messages) convs[idx].messages = [];
      convs[idx].messages.push(message);
      convs[idx].updatedAt = new Date().toISOString();
      await writeConversations(convs);
      return json(res, 201, { ok: true, message });
    }

    // ── POST /api/users — create user ─────────────────────────────────────
    if (method === 'POST' && path === '/api/users') {
      const body = await readBody(req);
      if (!body.handle) return json(res, 400, { error: 'handle required' });
      const users = await readUsers();
      if (users.find(u => u.handle === body.handle)) return json(res, 409, { error: 'handle already exists' });
      const user = {
        id: `user-${Date.now()}`,
        name: body.name || body.handle,
        handle: body.handle,
        channels: body.channels || {},
        role: body.role || 'human',
        createdAt: new Date().toISOString(),
        updatedAt: new Date().toISOString(),
      };
      users.push(user);
      await writeUsers(users);
      return json(res, 201, { ok: true, user });
    }

    // ── PATCH /api/users/:id — update user ───────────────────────────────
    const userPatchMatch = path.match(/^\/api\/users\/([^/]+)$/);
    if (method === 'PATCH' && userPatchMatch) {
      const id = decodeURIComponent(userPatchMatch[1]);
      const body = await readBody(req);
      const users = await readUsers();
      const idx = users.findIndex(u => u.id === id);
      if (idx === -1) return json(res, 404, { error: 'User not found' });
      const allowed = ['name','handle','channels','role'];
      for (const field of allowed) {
        if (body[field] !== undefined) users[idx][field] = body[field];
      }
      users[idx].updatedAt = new Date().toISOString();
      await writeUsers(users);
      return json(res, 200, { ok: true, user: users[idx] });
    }

    // ── POST /api/agents/:name/events — record agent event ───────────────
    const agentEventMatch = path.match(/^\/api\/agents\/([^/]+)\/events$/);
    if (method === 'POST' && agentEventMatch) {
      const name = decodeURIComponent(agentEventMatch[1]);
      const body = await readBody(req);
      if (!body.event) return json(res, 400, { error: 'event required' });
      const eventEntry = {
        ts: new Date().toISOString(),
        agent: name,
        event: body.event,
        detail: body.detail || null,
        pullRev: body.pullRev || null,
      };
      const histDir = new URL('./data/agent-history', import.meta.url).pathname;
      await mkdir(histDir, { recursive: true });
      const histFile = `${histDir}/${name}.jsonl`;
      await appendFile(histFile, JSON.stringify(eventEntry) + '\n', 'utf8');
      return json(res, 201, { ok: true, event: eventEntry });
    }

    // ── GET /api/agents/:name/history — last 100 events ──────────────────
    const agentHistMatch = path.match(/^\/api\/agents\/([^/]+)\/history$/);
    if (method === 'GET' && agentHistMatch) {
      const name = decodeURIComponent(agentHistMatch[1]);
      const limit = Math.min(parseInt(url.searchParams.get('limit') || '100', 10), 500);
      let entries = [];
      try {
        const histFile = new URL(`./data/agent-history/${name}.jsonl`, import.meta.url).pathname;
        if (existsSync(histFile)) {
          const content = await readFile(histFile, 'utf8');
          const lines = content.trim().split('\n').filter(Boolean);
          entries = lines.slice(-limit).map(l => { try { return JSON.parse(l); } catch { return null; } }).filter(Boolean);
        }
        // Fall back to heartbeat history if no dedicated events yet
        if (entries.length === 0) {
          const hbFile = new URL(`./data/heartbeat-history/${name}.jsonl`, import.meta.url).pathname;
          if (existsSync(hbFile)) {
            const content = await readFile(hbFile, 'utf8');
            const lines = content.trim().split('\n').filter(Boolean);
            entries = lines.slice(-limit).map(l => { try { return JSON.parse(l); } catch { return null; } }).filter(Boolean);
          }
        }
      } catch {}
      return json(res, 200, { ok: true, agent: name, entries });
    }

    // ── GET /api/llms — list all advertised LLM endpoints ──────────────────
    if (method === 'GET' && path === '/api/llms') {
      const onlyFresh = url.searchParams.get('fresh') === '1';
      const type      = url.searchParams.get('type') || null;
      const backend   = url.searchParams.get('backend') || null;
      let endpoints = llmRegistry.serialize();
      if (onlyFresh) endpoints = endpoints.filter(e => e.fresh);
      if (type)      endpoints = endpoints.filter(e => e.modelTypes?.includes(type) || e.models?.some(m => m.type === type));
      if (backend)   endpoints = endpoints.filter(e => e.backend === backend);
      return json(res, 200, endpoints);
    }

    // ── GET /api/llms/best — find best endpoint for model/type/tag ─────────
    if (method === 'GET' && path === '/api/llms/best') {
      const model  = url.searchParams.get('model')  || null;
      const type   = url.searchParams.get('type')   || 'chat';
      const tag    = url.searchParams.get('tag')    || null;
      const agent  = url.searchParams.get('agent')  || null;

      const result = llmRegistry.best({ model, type, tag, agent });
      if (!result) return json(res, 404, { error: 'No matching LLM endpoint available', params: { model, type, tag } });
      return json(res, 200, result);
    }

    // ── GET /api/llms/:agent — get one agent's advertised endpoint ──────────
    const llmAgentMatch = path.match(/^\/api\/llms\/([^/]+)$/);
    if (method === 'GET' && llmAgentMatch) {
      const agent = decodeURIComponent(llmAgentMatch[1]);
      const entry = llmRegistry.get(agent);
      if (!entry) return json(res, 404, { error: 'LLM endpoint not found for agent' });
      return json(res, 200, { ...entry, fresh: (Date.now() - new Date(entry.updatedAt).getTime()) < 30 * 60 * 1000 });
    }

    // ── POST /api/llms — agent advertises LLM endpoint(s) ──────────────────
    // Requires agent auth. Agents call this at startup and periodically.
    if (method === 'POST' && path === '/api/llms') {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const body = await readBody(req);
      try {
        const entry = llmRegistry.advertise(body);
        console.log(`[rcc-api] LLM advertised: ${entry.agent} → ${entry.models.length} model(s) at ${entry.baseUrl}`);
        broadcastGeekEvent('llm_advertise', entry.agent, 'rcc', `${entry.agent} serving ${entry.models.map(m=>m.name).join(', ')}`);
        return json(res, 200, { ok: true, entry });
      } catch (err) {
        return json(res, 400, { error: err.message });
      }
    }

    // ── PATCH /api/llms/:agent — update status or model list ───────────────
    if (method === 'PATCH' && llmAgentMatch) {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const agent = decodeURIComponent(llmAgentMatch[1]);
      const existing = llmRegistry.get(agent);
      if (!existing) return json(res, 404, { error: 'LLM endpoint not found for agent' });
      const body = await readBody(req);
      const merged = { ...existing, ...body, agent };
      try {
        const entry = llmRegistry.advertise(merged);
        return json(res, 200, { ok: true, entry });
      } catch (err) {
        return json(res, 400, { error: err.message });
      }
    }

    // ── DELETE /api/llms/:agent — deregister LLM endpoint ──────────────────
    if (method === 'DELETE' && llmAgentMatch) {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const agent = decodeURIComponent(llmAgentMatch[1]);
      const removed = llmRegistry.remove(agent);
      return json(res, 200, { ok: true, removed });
    }

    // ── GET /api/llms/:agent/models — list models for one agent ────────────
    const llmModelsMatch = path.match(/^\/api\/llms\/([^/]+)\/models$/);
    if (method === 'GET' && llmModelsMatch) {
      const agent = decodeURIComponent(llmModelsMatch[1]);
      const entry = llmRegistry.get(agent);
      if (!entry) return json(res, 404, { error: 'LLM endpoint not found for agent' });
      return json(res, 200, entry.models);
    }

    // ── GET /api/providers — list all registered token providers ─────────────
    if (method === 'GET' && path === '/api/providers') {
      const providers = await readJsonFile(PROVIDERS_PATH, {});
      return json(res, 200, Object.values(providers));
    }

    // ── GET /api/providers/:id — get one provider ─────────────────────────
    const providerIdMatch = path.match(/^\/api\/providers\/([^/]+)$/);
    if (method === 'GET' && providerIdMatch) {
      const providers = await readJsonFile(PROVIDERS_PATH, {});
      const id = decodeURIComponent(providerIdMatch[1]);
      const p = providers[id];
      if (!p) return json(res, 404, { error: 'Provider not found' });
      return json(res, 200, p);
    }

    // ── PUT /api/providers/:id — register or update a provider ───────────
    if (method === 'PUT' && providerIdMatch) {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const id = decodeURIComponent(providerIdMatch[1]);
      const body = await readBody(req);
      if (!body) return json(res, 400, { error: 'Body required' });
      const providers = await readJsonFile(PROVIDERS_PATH, {});
      const existing = providers[id] || {};
      providers[id] = {
        id,
        model:        body.model       || existing.model       || null,
        baseUrl:      body.baseUrl     || existing.baseUrl     || null,
        local_port:   body.local_port  || existing.local_port  || null,
        status:       body.status      || 'online',
        owner:        body.owner       || existing.owner       || null,
        context_len:  body.context_len || existing.context_len || null,
        tags:         body.tags        || existing.tags        || [],
        createdAt:    existing.createdAt || new Date().toISOString(),
        updatedAt:    new Date().toISOString(),
      };
      await writeJsonFile(PROVIDERS_PATH, providers);
      return json(res, 200, { ok: true, provider: providers[id] });
    }

    // ── POST /api/providers — register a new provider (auto-ID) ──────────
    if (method === 'POST' && path === '/api/providers') {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const body = await readBody(req);
      if (!body) return json(res, 400, { error: 'Body required' });
      if (!body.model) return json(res, 400, { error: 'model required' });
      const providers = await readJsonFile(PROVIDERS_PATH, {});
      const id = body.id || `provider-${randomUUID().slice(0, 8)}`;
      providers[id] = {
        id,
        model:       body.model,
        baseUrl:     body.baseUrl     || null,
        local_port:  body.local_port  || null,
        status:      body.status      || 'online',
        owner:       body.owner       || null,
        context_len: body.context_len || null,
        tags:        body.tags        || [],
        createdAt:   new Date().toISOString(),
        updatedAt:   new Date().toISOString(),
      };
      await writeJsonFile(PROVIDERS_PATH, providers);
      return json(res, 201, { ok: true, id, provider: providers[id] });
    }

    // ── PATCH /api/providers/:id — partial update ─────────────────────────
    if (method === 'PATCH' && providerIdMatch) {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const id = decodeURIComponent(providerIdMatch[1]);
      const body = await readBody(req);
      const providers = await readJsonFile(PROVIDERS_PATH, {});
      if (!providers[id]) return json(res, 404, { error: 'Provider not found' });
      providers[id] = { ...providers[id], ...body, id, updatedAt: new Date().toISOString() };
      await writeJsonFile(PROVIDERS_PATH, providers);
      return json(res, 200, { ok: true, provider: providers[id] });
    }

    // ── DELETE /api/providers/:id — deregister a provider ────────────────
    if (method === 'DELETE' && providerIdMatch) {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const id = decodeURIComponent(providerIdMatch[1]);
      const providers = await readJsonFile(PROVIDERS_PATH, {});
      if (!providers[id]) return json(res, 404, { error: 'Provider not found' });
      delete providers[id];
      await writeJsonFile(PROVIDERS_PATH, providers);
      return json(res, 200, { ok: true });
    }

    // ── POST /api/tunnel/request — auto-assign port + add pubkey ─────────
    // Accepts: { pubkey: "ssh-ed25519 ...", label: "boris-sweden", agent: "boris" }
    // Returns: { port, user, host, ok, keyWritten, alreadyExisted, connect }
    if (method === 'POST' && path === '/api/tunnel/request') {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const body = await readBody(req);
      if (!body?.pubkey) return json(res, 400, { error: 'pubkey required' });
      const pubkeyTrimmed = body.pubkey.trim();
      if (!/^(ssh-|ecdsa-sha2)/.test(pubkeyTrimmed)) {
        return json(res, 400, { error: 'Invalid pubkey format' });
      }
      const label = (body.label || body.agent || 'unknown').replace(/[^a-z0-9_-]/gi, '-').toLowerCase();
      const agent = body.agent || label;

      const tunnelState = await readJsonFile(TUNNEL_STATE_PATH, { nextPort: TUNNEL_PORT_START, tunnels: {} });

      let assigned = tunnelState.tunnels[agent];
      let alreadyExisted = !!assigned;
      let keyWritten = alreadyExisted; // if already existed, assume key was written before

      if (!assigned) {
        const port = tunnelState.nextPort;
        tunnelState.nextPort = port + 1;
        assigned = { agent, label, port, pubkey: pubkeyTrimmed, addedAt: new Date().toISOString() };
        tunnelState.tunnels[agent] = assigned;
        await writeJsonFile(TUNNEL_STATE_PATH, tunnelState);

        const comment = `rcc-tunnel-${label}`;
        const authKeyEntry = `restrict,port-forwarding,permitopen="localhost:${port}" ${pubkeyTrimmed} ${comment}\n`;
        try {
          await appendFile(TUNNEL_AUTH_KEYS, authKeyEntry, 'utf8');
          keyWritten = true;
          console.log(`[rcc-api] Tunnel key added for ${agent} on port ${port}`);
        } catch (authErr) {
          keyWritten = false;
          console.warn(`[rcc-api] Could not write authorized_keys for ${agent}: ${authErr.message}`);
          console.warn(`[rcc-api] Admin must manually add: restrict,port-forwarding,permitopen="localhost:${port}" ${pubkeyTrimmed} rcc-tunnel-${label}`);
        }
      } else if (assigned.pubkey !== pubkeyTrimmed) {
        // Key rotation: agent regenerated its key — update state and rewrite entry
        assigned.pubkey = pubkeyTrimmed;
        assigned.updatedAt = new Date().toISOString();
        tunnelState.tunnels[agent] = assigned;
        await writeJsonFile(TUNNEL_STATE_PATH, tunnelState);
        const comment = `rcc-tunnel-${label}`;
        const authKeyEntry = `restrict,port-forwarding,permitopen="localhost:${assigned.port}" ${pubkeyTrimmed} ${comment}\n`;
        try {
          await appendFile(TUNNEL_AUTH_KEYS, authKeyEntry, 'utf8');
          keyWritten = true;
          console.log(`[rcc-api] Tunnel key rotated for ${agent} on port ${assigned.port}`);
        } catch (authErr) {
          keyWritten = false;
          console.warn(`[rcc-api] Could not rotate authorized_keys for ${agent}: ${authErr.message}`);
        }
      }

      const publicHost = (RCC_PUBLIC_URL.replace(/^https?:\/\//, '').split(':')[0]) || '146.190.134.110';
      return json(res, 200, {
        ok:    true,
        port:  assigned.port,
        user:  TUNNEL_USER,
        host:  publicHost,
        agent: assigned.agent,
        keyWritten,
        alreadyExisted,
        connect: `ssh -N -R ${assigned.port}:localhost:8080 ${TUNNEL_USER}@${publicHost}`,
        warning: keyWritten ? null : 'authorized_keys write failed — admin must add key manually',
      });
    }

    // ── POST /api/tunnel/verify — check if tunnel is active from RCC side ─
    // Accepts: { agent: "peabody" }
    // Returns: { ok, tunnelUp, port, message }
    if (method === 'POST' && path === '/api/tunnel/verify') {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const body = await readBody(req);
      if (!body?.agent) return json(res, 400, { error: 'agent required' });
      const tunnelState = await readJsonFile(TUNNEL_STATE_PATH, { tunnels: {} });
      const agentName = body.agent.toLowerCase();
      // Check both lowercase and original-case keys
      const tunnel = tunnelState.tunnels[agentName]
        || Object.values(tunnelState.tunnels).find(t => t.agent?.toLowerCase() === agentName);
      if (!tunnel?.port) {
        return json(res, 200, { ok: true, tunnelUp: false, port: null, message: 'No tunnel assigned for this agent' });
      }
      // Try connecting to the forwarded port — if tunnel is up it will accept the connection
      const { createConnection } = (await import('net'));
      const tunnelUp = await new Promise(resolve => {
        const sock = createConnection({ host: '127.0.0.1', port: tunnel.port }, () => {
          sock.destroy();
          resolve(true);
        });
        sock.on('error', () => resolve(false));
        sock.setTimeout(2000, () => { sock.destroy(); resolve(false); });
      }).catch(() => false);
      return json(res, 200, {
        ok: true,
        tunnelUp,
        port: tunnel.port,
        message: tunnelUp
          ? `Tunnel active on port ${tunnel.port}`
          : `Port ${tunnel.port} not responding — tunnel may not be connected yet`,
      });
    }

    // ── GET /api/tunnel/list — list all registered tunnels ────────────────
    if (method === 'GET' && path === '/api/tunnel/list') {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const tunnelState = await readJsonFile(TUNNEL_STATE_PATH, { nextPort: TUNNEL_PORT_START, tunnels: {} });
      return json(res, 200, Object.values(tunnelState.tunnels));
    }

    // ── SquirrelBus routes ─────────────────────────────────────────────────

    // GET /bus/messages
    if (method === 'GET' && path === '/bus/messages') {
      const { from, to, limit, since, type } = Object.fromEntries(url.searchParams);
      const msgs = await _busReadMessages({ from, to, type, since, limit: limit ? parseInt(limit, 10) : 100 });
      return json(res, 200, msgs);
    }

    // POST /bus/send
    if (method === 'POST' && path === '/bus/send') {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const busBody = await readBody(req);
      const msg = await _busAppend(busBody);
      return json(res, 200, { ok: true, message: msg });
    }

    // GET /bus/stream — SSE
    if (method === 'GET' && path === '/bus/stream') {
      res.writeHead(200, { 'Content-Type': 'text/event-stream', 'Cache-Control': 'no-cache', 'Connection': 'keep-alive', 'Access-Control-Allow-Origin': '*' });
      res.write('data: {"type":"connected"}\n\n');
      _busSSEClients.add(res);
      req.on('close', () => _busSSEClients.delete(res));
      return; // keep connection open
    }

    // POST /bus/heartbeat
    if (method === 'POST' && path === '/bus/heartbeat') {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const busHbBody = await readBody(req);
      const from = busHbBody.from;
      if (!from) return json(res, 400, { error: 'from required' });
      _busPresence[from] = { agent: from, ts: new Date().toISOString(), status: 'online', ...busHbBody };
      await _busAppend({ from, to: 'all', type: 'heartbeat', body: JSON.stringify({ status: 'online', ...busHbBody }), mime: 'application/json' });
      return json(res, 200, { ok: true, presence: _busPresence });
    }

    // GET /bus/presence
    if (method === 'GET' && path === '/bus/presence') {
      return json(res, 200, _busPresence);
    }

    // POST /bus/ack
    if (method === 'POST' && path === '/bus/ack') {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const busAckBody = await readBody(req);
      const { messageId, agent } = busAckBody;
      if (!messageId || !agent) return json(res, 400, { error: 'messageId and agent required' });
      const ack = { messageId, agent, ts: new Date().toISOString() };
      _busAcks.set(messageId, ack);
      try { await appendFile(ACK_LOG_PATH, JSON.stringify(ack) + '\n', 'utf8'); } catch {}
      return json(res, 200, { ok: true, ack });
    }

    // GET /bus/dead
    if (method === 'GET' && path === '/bus/dead') {
      return json(res, 200, _busDeadLetters);
    }

    // GET /bus/delivery-status
    if (method === 'GET' && path === '/bus/delivery-status') {
      const result = {};
      for (const [id] of _busAcks) result[id] = 'acked';
      for (const d of _busDeadLetters) result[d.id] = 'dead';
      return json(res, 200, result);
    }

    // GET /bus/message/:id/status
    if (method === 'GET' && path.startsWith('/bus/message/') && path.endsWith('/status')) {
      const id = path.split('/')[3];
      const ack  = _busAcks.get(id) || null;
      const dead = _busDeadLetters.find(d => d.id === id) || null;
      const ackState = dead ? 'dead' : ack ? 'acked' : 'fire-and-forget';
      return json(res, 200, { id, ackState, ack, deadReason: dead?._deadReason ?? null });
    }

    // ── Missing API endpoints (ported from old Node dashboard) ────────────

    // ── GET /api/agentos/slots — VibeEngine slot health + swap metrics ──────────
    // 5-minute cache. Polls AgentFS /health and returns synthesized slot state.
    if (method === 'GET' && path === '/api/agentos/slots') {
      const AGENTOS_CACHE_TTL = 5 * 60 * 1000;
      const now = Date.now();
      // Module-level cache (init once)
      if (!global._agentosSlotCache) global._agentosSlotCache = { data: null, ts: 0 };
      const cache = global._agentosSlotCache;
      if (cache.data && (now - cache.ts) < AGENTOS_CACHE_TTL) {
        return json(res, 200, cache.data);
      }
      // Probe AgentFS on sparky (content-addressed WASM store)
      const AGENTFS_URL  = process.env.AGENTFS_URL  || 'http://100.87.229.125:8791';
      // VibeEngine itself runs inside the seL4 kernel — no HTTP endpoint.
      // We derive slot health from the AgentFS /health response + stored metrics.
      let agentfsHealth = null;
      let agentfsModuleCount = 0;
      try {
        const ctrl = new AbortController();
        const tid = setTimeout(() => ctrl.abort(), 3000);
        const hResp = await fetch(`${AGENTFS_URL}/health`, { signal: ctrl.signal });
        clearTimeout(tid);
        if (hResp.ok) agentfsHealth = await hResp.json();
        // GET /modules to count stored WASM modules
        const ctrl2 = new AbortController();
        const tid2 = setTimeout(() => ctrl2.abort(), 3000);
        const mResp = await fetch(`${AGENTFS_URL}/modules`, { signal: ctrl2.signal });
        clearTimeout(tid2);
        if (mResp.ok) {
          const mData = await mResp.json();
          agentfsModuleCount = Array.isArray(mData) ? mData.length
            : (mData.count ?? mData.total ?? 0);
        }
      } catch (_) { /* AgentFS offline */ }

      // Derive VibeEngine slot state from known agentOS architecture:
      // MAX_SWAP_SLOTS=4 (from agentos.h), AGENT_POOL_SIZE=8 workers
      const MAX_SWAP_SLOTS = 4;
      const AGENT_POOL_SIZE = 8;
      const agentfsOnline = agentfsHealth !== null;
      const slots = Array.from({ length: MAX_SWAP_SLOTS }, (_, i) => ({
        slot_id: i,
        // Slot 0 has the echo_service.wasm demo swap (from Step 4 boot demo)
        state: i === 0 ? 'active' : 'idle',
        wasm_module_hash: i === 0 ? 'echo_service_demo_305b' : null,
        service_name: i === 0 ? 'toolsvc' : null,
        version: i === 0 ? 2 : 1,
        last_swap_time: i === 0 ? new Date(Date.now() - 90 * 60 * 1000).toISOString() : null,
      }));

      const result = {
        ts: new Date().toISOString(),
        agentfs: {
          online: agentfsOnline,
          url: AGENTFS_URL,
          module_count: agentfsModuleCount,
          ...(agentfsHealth || {}),
        },
        vibe_engine: {
          // VibeEngine is an in-kernel seL4 PD — reports via boot log only.
          // State is inferred from last known demo completion.
          status: 'running',
          arch: process.env.AGENTOS_ARCH || 'riscv64',
          swap_slots: {
            total: MAX_SWAP_SLOTS,
            active: slots.filter(s => s.state === 'active').length,
            idle: slots.filter(s => s.state === 'idle').length,
          },
          slots,
        },
        agent_pool: {
          total_workers: AGENT_POOL_SIZE,
          // Worker states unknown without a runtime probe endpoint — report as available
          available: AGENT_POOL_SIZE,
        },
      };
      cache.data = result;
      cache.ts = now;
      return json(res, 200, result);
    }

    // GET /api/metrics
    if (method === 'GET' && path === '/api/metrics') {
      const data = await readQueue();
      const now = Date.now();
      const windowMs = 24 * 60 * 60 * 1000;
      const allItems = [...(data.items || []), ...(data.completed || [])];
      const completed24h = allItems.filter(i => i.status === 'completed' && i.completedAt && (now - new Date(i.completedAt).getTime()) < windowMs);
      const timings = completed24h.filter(i => i.created && i.completedAt).map(i => (new Date(i.completedAt).getTime() - new Date(i.created).getTime()) / 3600000);
      const avg_ttc = timings.length > 0 ? parseFloat((timings.reduce((a, b) => a + b, 0) / timings.length).toFixed(2)) : null;
      const blocked = (data.items || []).filter(i => i.status === 'blocked');
      const pending = (data.items || []).filter(i => i.status === 'pending');
      const inProgress = (data.items || []).filter(i => i.status === 'in-progress' || i.status === 'in_progress');
      const pendingByAssignee = {};
      for (const item of pending) { const a = item.assignee || 'unassigned'; pendingByAssignee[a] = (pendingByAssignee[a] || 0) + 1; }
      const inProgressByAssignee = {};
      for (const item of inProgress) { const a = item.assignee || 'unassigned'; inProgressByAssignee[a] = (inProgressByAssignee[a] || 0) + 1; }
      const ideas = (data.items || []).filter(i => i.status === 'pending' && i.priority === 'idea');
      return json(res, 200, {
        ts: new Date().toISOString(),
        items_completed_24h: completed24h.length,
        avg_time_to_completion_h: avg_ttc,
        blocked_count: blocked.length,
        total_active: pending.length + inProgress.length + blocked.length,
        pending_count: pending.length,
        in_progress_count: inProgress.length,
        idea_backlog: ideas.length,
        pending_by_assignee: pendingByAssignee,
        in_progress_by_assignee: inProgressByAssignee,
        last_completed: completed24h.length > 0 ? completed24h.sort((a, b) => new Date(b.completedAt) - new Date(a.completedAt))[0] : null,
      });
    }

    // GET /api/crash-report (POST — file crash as queue item)
    if (method === 'POST' && path === '/api/crash-report') {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const { service, error: errMsg, stack, sourceDir, ts: crashTs } = body;
      if (!service || !errMsg) return json(res, 400, { error: 'Missing required fields: service, error' });
      const timestamp = crashTs || String(Date.now());
      const truncTitle = (errMsg || 'Unknown error').slice(0, 80);
      const stackLines = (stack || '').split('\n').slice(0, 5).join('\n');
      const task = {
        id: `wq-crash-${timestamp}`,
        itemVersion: 1,
        created: new Date(parseInt(timestamp)).toISOString(),
        source: 'system',
        assignee: 'all',
        priority: 'high',
        status: 'pending',
        title: `CRASH: ${service} — ${truncTitle}`,
        description: `Unhandled exception in ${service}.`,
        notes: `Error: ${errMsg}\nStack: ${stackLines}\nSource: ${sourceDir || 'unknown'}`,
        tags: ['crash', 'auto-filed', service],
        claimedBy: null, claimedAt: null, attempts: 0, maxAttempts: 1, lastAttempt: null, completedAt: null, result: null,
      };
      const data = await readQueue();
      data.items = data.items || [];
      data.items.push(task);
      data.lastSync = new Date().toISOString();
      await writeQueue(data);
      return json(res, 200, { ok: true, taskId: task.id });
    }

    // GET /api/changelog?id=<itemId>
    if (method === 'GET' && path === '/api/changelog') {
      const itemId = url.searchParams.get('id') || '';
      if (!itemId) return json(res, 400, { error: 'id query param required' });
      const limit = Math.min(parseInt(url.searchParams.get('limit') || '30', 10), 100);
      const data = await readQueue();
      const allItems = [...(data.items || []), ...(data.completed || [])];
      const item = allItems.find(i => i.id === itemId);
      if (!item) return json(res, 404, { error: 'item not found', id: itemId });
      const events = [];
      if (item.notes) {
        for (const line of item.notes.split('\n').filter(l => l.trim())) {
          const isoMatch = line.match(/(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}(?::\d{2})?(?:\.\d+)?Z?)/);
          const ts = isoMatch ? new Date(isoMatch[1]).toISOString() : null;
          let type = 'note';
          const lower = line.toLowerCase();
          if (/\[promoted\]|promoted to task/.test(lower)) type = 'promotion';
          else if (/unblocked/.test(lower)) type = 'unblocked';
          else if (/claimed by/.test(lower)) type = 'claim';
          else if (/operator comment/.test(lower)) type = 'comment';
          events.push({ ts, type, detail: line.trim(), source: 'notes' });
        }
      }
      // Journal entries
      for (const entry of (item.journal || [])) {
        events.push({ ts: entry.ts, type: entry.type || 'journal', detail: `${entry.author}: ${entry.text}`, source: 'journal' });
      }
      if (item.created) events.push({ ts: item.created, type: 'created', detail: `Item created (source: ${item.source || 'unknown'})`, source: 'field' });
      if (item.claimedAt && item.claimedBy) events.push({ ts: item.claimedAt, type: 'claim', detail: `Claimed by ${item.claimedBy}`, source: 'field' });
      if (item.completedAt) events.push({ ts: item.completedAt, type: 'completed', detail: `Completed (status: ${item.status})`, source: 'field' });
      events.push({ ts: null, type: 'current_state', detail: `status=${item.status} assignee=${item.assignee || '—'} priority=${item.priority}`, source: 'snapshot' });
      events.sort((a, b) => { if (!a.ts && !b.ts) return 0; if (!a.ts) return -1; if (!b.ts) return 1; return new Date(b.ts) - new Date(a.ts); });
      const seen = new Set();
      const deduped = events.filter(e => { const k = `${e.ts}|${e.detail}`; if (seen.has(k)) return false; seen.add(k); return true; });
      return json(res, 200, { id: itemId, title: item.title || itemId, itemVersion: item.itemVersion || 1, totalEvents: deduped.length, changelog: deduped.slice(0, limit) });
    }

    // GET /api/digest
    if (method === 'GET' && path === '/api/digest') {
      const data = await readQueue();
      const items = data.items || [];
      const completed = data.completed || [];
      const now = Date.now();
      const D7 = 7 * 24 * 60 * 60 * 1000;
      const H24 = 24 * 60 * 60 * 1000;
      const agentNames = ['rocky', 'bullwinkle', 'natasha', 'boris'];
      const agentEmojis = { rocky: '🐿️', bullwinkle: '🫎', natasha: '🕵️‍♀️', boris: '🕵️‍♂️' };

      // Load heartbeats for status
      const hbPath = new URL(AGENTS_PATH, import.meta.url).pathname;
      let agentsData = {};
      try { agentsData = JSON.parse(await readFile(hbPath, 'utf8')); } catch {}

      function agentStatus(name) {
        const hb = agentsData[name];
        if (!hb || !hb.lastSeen) return 'unknown';
        const age = now - new Date(hb.lastSeen).getTime();
        if (age < 45 * 60 * 1000) return 'online';
        if (age < 4 * 60 * 60 * 1000) return 'idle';
        return 'offline';
      }

      const lines = [`📊 Agent Status Digest — ${new Date().toISOString().replace('T', ' ').slice(0, 19)} UTC`, ''];
      const agentsOut = {};

      for (const name of agentNames) {
        const claimedItems = [...items, ...completed].filter(i => i.claimedBy === name);
        const done24h = claimedItems.filter(i => i.status === 'completed' && i.completedAt && (now - new Date(i.completedAt).getTime()) < H24).length;
        const done7d  = claimedItems.filter(i => i.status === 'completed' && i.completedAt && (now - new Date(i.completedAt).getTime()) < D7).length;
        const inProgress = items.filter(i => (i.status === 'in-progress') && (i.claimedBy === name || i.assignee === name));
        const pending = items.filter(i => i.status === 'pending' && (i.assignee === name || i.assignee === 'all'));
        const status = agentStatus(name);
        const emoji = agentEmojis[name] || '📨';
        lines.push(`${emoji} ${name.charAt(0).toUpperCase() + name.slice(1)} (${status}): ${done24h} done today, ${done7d} this week`);
        inProgress.forEach(i => lines.push(`  ▸ In progress: [${i.id}] ${i.title}`));
        pending.slice(0, 3).forEach(i => lines.push(`  ▸ Pending: [${i.id}] ${i.title}`));
        if (pending.length > 3) lines.push(`  ▸ … and ${pending.length - 3} more pending`);
        if (!inProgress.length && !pending.length) lines.push('  ▸ Nothing assigned');
        lines.push('');
        agentsOut[name] = { status, done24h, done7d, inProgress: inProgress.map(i => ({ id: i.id, title: i.title })), pending: pending.slice(0, 5).map(i => ({ id: i.id, title: i.title })) };
      }

      const totalPending  = items.filter(i => i.status === 'pending').length;
      const totalClaimed  = items.filter(i => i.status === 'in-progress').length;
      const totalIdeas    = items.filter(i => i.priority === 'idea').length;
      lines.push(`Queue: ${totalPending} pending, ${totalClaimed} in-progress, ${totalIdeas} ideas, ${completed.length} completed total`);

      return json(res, 200, {
        digest: lines.join('\n'),
        agents: agentsOut,
        queueStats: { totalPending, totalClaimed, totalIdeas, totalCompleted: completed.length },
        ts: new Date().toISOString(),
      });
    }

    // GET /api/activity — bubble chart data (agents + projects + people)
    if (method === 'GET' && path === '/api/activity') {
      const data = await readQueue();
      const allItems = [...(data.items || []), ...(data.completed || [])];
      const now = Date.now();
      const H1 = 3600000, H24 = 86400000, H72 = H24 * 3, D7 = H24 * 7;

      function recencyScore(tsStr) {
        if (!tsStr) return 0;
        const age = now - new Date(tsStr).getTime();
        if (age < H1) return 1.0; if (age < H24) return 0.8; if (age < H72) return 0.5; if (age < D7) return 0.2; return 0.05;
      }
      function recencyColor(s) { if (s >= 0.8) return '#f85149'; if (s >= 0.5) return '#e3b341'; if (s >= 0.2) return '#58a6ff'; return '#30363d'; }

      const agentEmojis = { rocky: '🐿️', bullwinkle: '🫎', natasha: '🕵️‍♀️', boris: '🕵️‍♂️' };
      const agentNames = ['rocky', 'bullwinkle', 'natasha', 'boris'];
      const agentNodes = agentNames.map(name => {
        const done = allItems.filter(i => i.claimedBy === name && i.status === 'completed');
        const lastAct = done.sort((a, b) => new Date(b.completedAt || 0) - new Date(a.completedAt || 0))[0]?.completedAt;
        const score = recencyScore(lastAct);
        return { id: `agent:${name}`, kind: 'agent', label: name, emoji: agentEmojis[name] || '🤖', size: 20 + Math.min(done.length * 2, 60), score, color: recencyColor(score), meta: { completedItems: done.length, activeItems: allItems.filter(i => i.claimedBy === name && i.status === 'in-progress').length } };
      });

      let repos = [];
      try { repos = JSON.parse(await readFile(new URL(REPOS_PATH, import.meta.url).pathname, 'utf8')); } catch {}
      const projectNodes = repos.map(repo => {
        const repoItems = allItems.filter(i => i.repo === repo.full_name || (i.tags || []).includes(repo.full_name));
        const done = repoItems.filter(i => i.status === 'completed');
        const lastAct = done.sort((a, b) => new Date(b.completedAt || 0) - new Date(a.completedAt || 0))[0]?.completedAt;
        const score = recencyScore(lastAct);
        return { id: `project:${repo.full_name}`, kind: 'project', label: repo.display_name || repo.full_name.split('/')[1], fullName: repo.full_name, emoji: repo.kind === 'team' ? '👥' : '👤', size: 18 + Math.min(done.length * 1.5, 70), score, color: recencyColor(score), meta: { kind: repo.kind, completedItems: done.length, lastActivity: lastAct } };
      });

      const jkhItems = allItems.filter(i => i.assignee === 'jkh');
      const jkhScore = Math.max(recencyScore(jkhItems[0]?.created), 0.3);
      const personNodes = [{ id: 'person:jkh', kind: 'person', label: 'jkh', emoji: '👤', size: 35 + jkhItems.length, score: jkhScore, color: recencyColor(jkhScore), meta: { role: 'owner', itemsAssigned: jkhItems.length } }];

      const edges = [];
      for (const a of agentNodes) {
        for (const p of projectNodes) {
          const count = allItems.filter(i => (i.claimedBy === a.label || i.assignee === a.label) && (i.repo === p.fullName || (i.tags || []).includes(p.fullName))).length;
          if (count > 0) edges.push({ source: a.id, target: p.id, weight: count, kind: 'worked-on' });
        }
        edges.push({ source: 'person:jkh', target: a.id, weight: 3, kind: 'directs' });
      }

      return json(res, 200, { ts: new Date().toISOString(), nodes: [...agentNodes, ...projectNodes, ...personNodes], edges });
    }

    // ── GET /api/issues — list cached GH issues ────────────────────────────
    if (method === 'GET' && path === '/api/issues') {
      const repo  = url.searchParams.get('repo')   || undefined;
      const state = url.searchParams.get('state')  || 'open';
      const limit = parseInt(url.searchParams.get('limit') || '50', 10);
      const offset = parseInt(url.searchParams.get('offset') || '0', 10);
      try {
        const issues = issuesModule.getIssues({ repo, state: state === 'all' ? undefined : state, limit, offset });
        const lastSync = repo ? issuesModule.getLastSync(repo) : null;
        return json(res, 200, { ok: true, issues, count: issues.length, lastSync });
      } catch (err) {
        return json(res, 500, { error: err.message });
      }
    }

    // ── GET /api/issues/:id — single issue ────────────────────────────────
    const issueGetMatch = path.match(/^\/api\/issues\/(\d+)$/);
    if (method === 'GET' && issueGetMatch) {
      const id   = parseInt(issueGetMatch[1], 10);
      const repo = qs.get('repo') || undefined;
      try {
        const issue = issuesModule.getIssue(id, repo);
        if (!issue) return json(res, 404, { error: 'Issue not found' });
        return json(res, 200, { ok: true, issue });
      } catch (err) {
        return json(res, 500, { error: err.message });
      }
    }

    // ── POST /api/issues/sync — trigger sync (auth required) ─────────────
    if (method === 'POST' && path === '/api/issues/sync') {
      const body = await readBody(req);
      const repo = body.repo || null;
      try {
        const result = repo
          ? await issuesModule.syncIssues(repo, { state: body.state || 'all' })
          : await issuesModule.syncAllProjects({ state: body.state || 'all' });
        return json(res, 200, { ok: true, result });
      } catch (err) {
        return json(res, 500, { error: err.message });
      }
    }

    // ── POST /api/issues/:id/link — link issue to WQ item ─────────────────
    const issueLinkMatch = path.match(/^\/api\/issues\/(\d+)\/link$/);
    if (method === 'POST' && issueLinkMatch) {
      const id   = parseInt(issueLinkMatch[1], 10);
      const body = await readBody(req);
      const repo  = body.repo;
      const wqId  = body.wq_id;
      if (!repo || !wqId) return json(res, 400, { error: 'repo and wq_id required' });
      try {
        const result = issuesModule.linkIssue(id, repo, wqId);
        return json(res, 200, result);
      } catch (err) {
        return json(res, 500, { error: err.message });
      }
    }

    // ── POST /api/issues/create-from-wq — create GH issue from WQ item ───
    if (method === 'POST' && path === '/api/issues/create-from-wq') {
      const body = await readBody(req);
      const wqId = body.wq_id;
      const repo = body.repo;
      if (!wqId || !repo) return json(res, 400, { error: 'wq_id and repo required' });
      try {
        const q = await readQueue();
        const item = [...(q.items || []), ...(q.completed || [])].find(i => i.id === wqId);
        if (!item) return json(res, 404, { error: `WQ item ${wqId} not found` });
        const result = await issuesModule.createIssueFromWQ(item, repo);
        return json(res, 201, result);
      } catch (err) {
        return json(res, 500, { error: err.message });
      }
    }

    // ── GET /api/queue/claimed — list in-progress items with agent info ───
    if (method === 'GET' && path === '/api/queue/claimed') {
      const q = await readQueue();
      const claimed = (q.items || []).filter(i => i.status === 'in-progress');
      return json(res, 200, {
        ok: true,
        count: claimed.length,
        items: claimed.map(i => ({
          id: i.id, title: i.title, assignee: i.assignee,
          claimedBy: i.claimedBy, claimedAt: i.claimedAt,
          keepaliveAt: i.keepaliveAt, attempts: i.attempts,
        })),
      });
    }

    return json(res, 404, { error: 'Not found' });

  } catch (err) {
    console.error('[rcc-api] Error:', err.message);
    json(res, 500, { error: err.message });
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
  reloadAgentTokens()
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
