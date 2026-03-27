#!/usr/bin/env node
/**
 * WQ Dashboard — Rocky's unified workqueue + SquirrelBus dashboard
 * Port 8788, dark theme, live data, client-side rendering
 */

import express from 'express';
import { readFile, writeFile, appendFile } from 'fs/promises';
import { existsSync, createReadStream, readdirSync, readFileSync } from 'fs';
import { execFile, spawn } from 'child_process';
import { promisify } from 'util';
import { randomUUID } from 'crypto';
import { createInterface } from 'readline';
import { createReadStream as createRS } from 'fs';
import { join, dirname } from 'path';
import { fileURLToPath } from 'url';
import { initCrashReporter } from '../lib/crash-reporter.mjs';

const __dirname = dirname(fileURLToPath(import.meta.url));

// Load agent names from rcc/agents/*.capabilities.json (config-driven, no hardcoded list)
function loadCapabilityNames() {
  try {
    const capDir = join(__dirname, '..', 'rcc', 'agents');
    return readdirSync(capDir)
      .filter(f => f.endsWith('.capabilities.json'))
      .map(f => { try { return JSON.parse(readFileSync(join(capDir, f), 'utf8')).name; } catch { return null; } })
      .filter(Boolean);
  } catch { return []; }
}

// Initialize crash reporter early — before anything else can throw
initCrashReporter({
  service: 'wq-dashboard',
  sourceDir: '/home/jkh/.openclaw/workspace/dashboard'
});

const execFileP = promisify(execFile);

const app = express();
const PORT = 8788;
const AUTH_TOKEN = process.env.RCC_AUTH_TOKENS || process.env.RCC_AGENT_TOKEN || '';
const QUEUE_PATH = '/home/jkh/.openclaw/workspace/workqueue/queue.json';
const MC_PATH = 'mc';
const MINIO_ALIAS = process.env.MINIO_ALIAS || 'local';
const BUS_LOG_PATH  = '/home/jkh/.openclaw/workspace/squirrelbus/bus.jsonl';
const ACK_LOG_PATH  = '/home/jkh/.openclaw/workspace/squirrelbus/acks.jsonl';
const DEAD_LOG_PATH = '/home/jkh/.openclaw/workspace/squirrelbus/dead-letter.jsonl';

// ── SquirrelBus peer fan-out registry ─────────────────────────────────────────
const BUS_PEERS = {
  bullwinkle: process.env.BULLWINKLE_BUS_URL || '',
  natasha:    process.env.NATASHA_BUS_URL    || '',
};

// Known /bus/receive URLs for delivery confirmation + retry
const BUS_RECEIVE_URLS = {
  bullwinkle: process.env.BULLWINKLE_URL || '',
  natasha:    process.env.NATASHA_URL    || '',
};

const RETRY_DELAY_MS = parseInt(process.env.BUS_RETRY_DELAY_MS || '') || 5 * 60 * 1000;
const MAX_RETRIES    = 3;
const BULLWINKLE_TOKEN = process.env.BULLWINKLE_TOKEN || '';
const NATASHA_TOKEN    = process.env.NATASHA_TOKEN    || '';
const PEER_TOKENS = { bullwinkle: BULLWINKLE_TOKEN, natasha: NATASHA_TOKEN };

async function fanOutBusMessage(msg) {
  for (const [peer, url] of Object.entries(BUS_PEERS)) {
    // Skip fan-out back to the originating agent
    if (msg.from === peer) continue;
    // Only fan out if addressed to this peer or to 'all'
    if (msg.to !== 'all' && msg.to !== peer) continue;

    const token = PEER_TOKENS[peer];
    (async () => {
      try {
        const ctrl = new AbortController();
        const timeout = setTimeout(() => ctrl.abort(), 35000);
        const resp = await fetch(url, {
          method: 'POST',
          headers: {
            'Content-Type': 'application/json',
            'Authorization': `Bearer ${token}`,
          },
          body: JSON.stringify(msg),
          signal: ctrl.signal,
        });
        clearTimeout(timeout);
        console.log(`[bus-fanout] → ${peer}: HTTP ${resp.status}`);
      } catch (err) {
        console.warn(`[bus-fanout] → ${peer}: failed (${err.message})`);
      }
    })();
  }
}

// Middleware
app.use(express.json());

// CORS for API endpoints
app.use('/api', (req, res, next) => {
  res.header('Access-Control-Allow-Origin', '*');
  res.header('Access-Control-Allow-Headers', 'Content-Type, Authorization');
  res.header('Access-Control-Allow-Methods', 'GET, POST, OPTIONS');
  if (req.method === 'OPTIONS') return res.sendStatus(200);
  next();
});

// Auth middleware for write endpoints
function requireAuth(req, res, next) {
  const auth = (req.headers.authorization || '').trim();
  const expected = `Bearer ${AUTH_TOKEN}`;
  if (!auth || auth !== expected) {
    return res.status(401).json({ error: 'Unauthorized — check your auth token ($RCC_AGENT_TOKEN)' });
  }
  next();
}

// --- Data helpers ---

async function readQueue() {
  const raw = await readFile(QUEUE_PATH, 'utf8');
  return JSON.parse(raw);
}

async function writeQueue(data) {
  await writeFile(QUEUE_PATH, JSON.stringify(data, null, 2) + '\n', 'utf8');
}

// In-memory heartbeat store
const heartbeats = {};

async function fetchMinIOHeartbeat(agent) {
  try {
    const { stdout } = await execFileP(MC_PATH, [
      'cat', `${MINIO_ALIAS}/agents/shared/agent-heartbeat-${agent}.json`
    ], { timeout: 5000 });
    return JSON.parse(stdout);
  } catch {
    return null;
  }
}

async function getHeartbeats() {
  // Start with in-memory heartbeats (includes any agent that has posted)
  const result = { ...heartbeats };
  // Also check MinIO for known persistent agents (fills gaps on cold start)
  const knownAgents = loadCapabilityNames();
  for (const agent of knownAgents) {
    if (!result[agent]) {
      const minio = await fetchMinIOHeartbeat(agent);
      if (minio) result[agent] = minio;
    }
  }
  return result;
}

function escapeHtml(str) {
  if (!str) return '';
  return str.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
}

// --- API Routes ---

app.post('/api/queue', requireAuth, async (req, res) => {
  try {
    const { title, description, priority, assignee, tags } = req.body;
    if (!title || !title.trim()) {
      return res.status(400).json({ error: 'title is required' });
    }
    const now = new Date().toISOString();
    const id = `wq-USER-${Date.now()}`;
    const item = {
      id,
      itemVersion: 1,
      created: now,
      source: process.env.OPERATOR_HANDLE || 'operator',
      assignee: assignee || 'all',
      priority: priority || 'normal',
      status: 'pending',
      title: title.trim(),
      description: (description || '').trim(),
      notes: '',
      tags: Array.isArray(tags) ? tags : (tags ? String(tags).split(',').map(t => t.trim()).filter(Boolean) : []),
      votes: [],
      claimedBy: null,
      claimedAt: null,
      attempts: 0,
      maxAttempts: 1,
      lastAttempt: null,
      completedAt: null,
      result: null,
    };
    const data = await readQueue();
    data.items.unshift(item);
    data.lastSync = now;
    await writeQueue(data);
    res.status(201).json({ ok: true, item });
  } catch (e) {
    res.status(500).json({ error: e.message });
  }
});

app.get('/api/queue', async (req, res) => {
  try {
    const data = await readQueue();
    res.json(data);
  } catch (e) {
    res.status(500).json({ error: e.message });
  }
});

app.get('/api/heartbeats', async (req, res) => {
  try {
    const hbs = await getHeartbeats();
    res.json(hbs);
  } catch (e) {
    res.status(500).json({ error: e.message });
  }
});

app.post('/api/upvote/:id', requireAuth, async (req, res) => {
  try {
    const data = await readQueue();
    const item = data.items.find(i => i.id === req.params.id);
    if (!item) return res.status(404).json({ error: 'Item not found' });
    if (item.status !== 'idea') return res.status(400).json({ error: 'Only idea items can be promoted' });
    item.status = 'pending';
    item.notes = (item.notes || '') + `\nPromoted to task by dashboard at ${new Date().toISOString()} (upvote).`;
    item.itemVersion = (item.itemVersion || 0) + 1;
    if (!item.votes) item.votes = [];
    item.votes.push('dashboard');
    await writeQueue(data);
    res.json({ ok: true, item });
  } catch (e) {
    res.status(500).json({ error: e.message });
  }
});

app.post('/api/comment/:id', requireAuth, async (req, res) => {
  try {
    const data = await readQueue();
    const idx = data.items.findIndex(i => i.id === req.params.id);
    if (idx === -1) return res.status(404).json({ error: 'Item not found' });
    const item = data.items[idx];
    const text = (req.body.text || '').trim().toLowerCase();
    const rawText = (req.body.text || '').trim();

    if (text === 'delete' || text === 'remove') {
      data.items.splice(idx, 1);
      await writeQueue(data);
      return res.json({ ok: true, action: 'deleted', id: req.params.id });
    }

    if (text.startsWith('break into') || text.includes('subtask')) {
      item.status = 'pending';
      item.notes = (item.notes || '') + `\n[Subtask note] ${rawText} — added at ${new Date().toISOString()}`;
      item.itemVersion = (item.itemVersion || 0) + 1;
      await writeQueue(data);
      return res.json({ ok: true, action: 'subtasked', item });
    }

    if (item.status === 'blocked') item.status = 'pending';
    item.notes = (item.notes || '') + `\n[Comment] ${rawText} — added at ${new Date().toISOString()}`;
    item.itemVersion = (item.itemVersion || 0) + 1;
    await writeQueue(data);
    res.json({ ok: true, action: 'commented', item });
  } catch (e) {
    res.status(500).json({ error: e.message });
  }
});

app.post('/api/complete/:id', requireAuth, async (req, res) => {
  try {
    const data = await readQueue();
    const item = data.items.find(i => i.id === req.params.id);
    if (!item) return res.status(404).json({ error: 'Item not found' });
    item.status = 'completed';
    item.completedAt = new Date().toISOString();
    item.itemVersion = (item.itemVersion || 0) + 1;
    await writeQueue(data);
    res.json({ ok: true, item });
  } catch (e) {
    res.status(500).json({ error: e.message });
  }
});

// --- Item detail, journal, choices, patch ---

app.get('/api/item/:id', async (req, res) => {
  try {
    const data = await readQueue();
    const item = [...data.items, ...(data.completed || [])].find(i => i.id === req.params.id);
    if (!item) return res.status(404).json({ error: 'Item not found' });
    res.json(item);
  } catch (e) {
    res.status(500).json({ error: e.message });
  }
});

app.patch('/api/item/:id', requireAuth, async (req, res) => {
  try {
    const data = await readQueue();
    const item = data.items.find(i => i.id === req.params.id);
    if (!item) return res.status(404).json({ error: 'Item not found' });
    const allowed = ['title','description','priority','assignee','status','notes','choices'];
    const now = new Date().toISOString();
    const changed = [];
    for (const field of allowed) {
      if (req.body[field] !== undefined && req.body[field] !== item[field]) {
        const oldVal = item[field];
        item[field] = req.body[field];
        changed.push(`${field}: ${JSON.stringify(oldVal)} → ${JSON.stringify(req.body[field])}`);
      }
    }
    if (changed.length) {
      if (!item.journal) item.journal = [];
      item.journal.push({ ts: now, author: process.env.OPERATOR_HANDLE || 'operator', type: 'status-change', text: `Updated: ${changed.join('; ')}` });
      item.itemVersion = (item.itemVersion || 0) + 1;
      await writeQueue(data);
    }
    res.json({ ok: true, item });
  } catch (e) {
    res.status(500).json({ error: e.message });
  }
});

app.post('/api/item/:id/comment', requireAuth, async (req, res) => {
  try {
    const data = await readQueue();
    const item = data.items.find(i => i.id === req.params.id);
    if (!item) return res.status(404).json({ error: 'Item not found' });
    const text = (req.body.text || '').trim();
    const author = (req.body.author || process.env.OPERATOR_HANDLE || 'operator').trim();
    if (!text) return res.status(400).json({ error: 'text required' });
    if (!item.journal) item.journal = [];
    const entry = { ts: new Date().toISOString(), author, type: 'comment', text };
    item.journal.push(entry);
    item.itemVersion = (item.itemVersion || 0) + 1;
    await writeQueue(data);
    res.json({ ok: true, entry });
  } catch (e) {
    res.status(500).json({ error: e.message });
  }
});

app.post('/api/item/:id/choice', requireAuth, async (req, res) => {
  try {
    const data = await readQueue();
    const item = data.items.find(i => i.id === req.params.id);
    if (!item) return res.status(404).json({ error: 'Item not found' });
    const { choice, choiceLabel } = req.body;
    if (!choice) return res.status(400).json({ error: 'choice required' });
    const now = new Date().toISOString();
    if (!item.journal) item.journal = [];
    const entry = { ts: now, author: process.env.OPERATOR_HANDLE || 'operator', type: 'choice', text: `Choice recorded: [${choice}] ${choiceLabel || ''}` };
    item.journal.push(entry);
    item.choiceRecorded = { choice, label: choiceLabel || '', ts: now };
    item.itemVersion = (item.itemVersion || 0) + 1;
    await writeQueue(data);
    res.json({ ok: true, entry, choiceRecorded: item.choiceRecorded });
  } catch (e) {
    res.status(500).json({ error: e.message });
  }
});

app.post('/api/item/:id/ai-comment', requireAuth, async (req, res) => {
  try {
    const data = await readQueue();
    const item = data.items.find(i => i.id === req.params.id);
    if (!item) return res.status(404).json({ error: 'Item not found' });
    const prompt = (req.body.prompt || '').trim();
    if (!prompt) return res.status(400).json({ error: 'prompt required' });
    const now = new Date().toISOString();
    if (!item.journal) item.journal = [];

    // User entry
    const userEntry = { ts: now, author: process.env.OPERATOR_HANDLE || 'operator', type: 'ai', text: `✨ ${prompt}` };
    item.journal.push(userEntry);

    // Call OpenClaw gateway
    let aiText = '(no response)';
    try {
      const gwResp = await fetch('http://localhost:18789/v1/chat/completions', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', 'Authorization': `Bearer ${process.env.PEER_GATEWAY_TOKEN || ''}` },
        body: JSON.stringify({
          model: process.env.PEER_GATEWAY_MODEL || 'claude-sonnet-4-6',
          messages: [
            { role: 'system', content: `You are Rocky, an AI assistant helping with a work item. Item title: "${item.title}". Description: "${item.description || ''}". Be concise and helpful.` },
            { role: 'user', content: prompt }
          ],
          max_tokens: 500
        })
      });
      const gwData = await gwResp.json();
      aiText = gwData?.choices?.[0]?.message?.content || '(empty response)';
    } catch (gwErr) {
      aiText = `(gateway error: ${gwErr.message})`;
    }

    const aiEntry = { ts: new Date().toISOString(), author: '🐿️ Rocky', type: 'ai', text: aiText };
    item.journal.push(aiEntry);
    item.itemVersion = (item.itemVersion || 0) + 1;
    await writeQueue(data);
    res.json({ ok: true, userEntry, aiEntry });
  } catch (e) {
    res.status(500).json({ error: e.message });
  }
});

app.post('/api/heartbeat/:agent', requireAuth, async (req, res) => {
  const agent = req.params.agent;
  heartbeats[agent] = {
    agent,
    ts: new Date().toISOString(),
    status: 'online',
    ...req.body,
  };
  res.json({ ok: true });
});

// --- Crash Report API ---

app.post('/api/crash-report', requireAuth, async (req, res) => {
  try {
    const { service, error, stack, sourceDir, ts } = req.body;
    if (!service || !error) {
      return res.status(400).json({ error: 'Missing required fields: service, error' });
    }

    const timestamp = ts || String(Date.now());
    const truncTitle = (error || 'Unknown error').slice(0, 80);
    const stackLines = (stack || '').split('\n').slice(0, 5).join('\n');
    const minioPath = `agents/logs/${service}-crash-${timestamp}.json`;

    const task = {
      id: `wq-crash-${timestamp}`,
      itemVersion: 1,
      created: new Date(parseInt(timestamp)).toISOString(),
      source: 'system',
      assignee: 'all',
      priority: 'high',
      status: 'pending',
      title: `CRASH: ${service} — ${truncTitle}`,
      description: `Unhandled exception in ${service}. Stack trace and logs available.`,
      notes: `Error: ${error}\nStack: ${stackLines}\nSource: ${sourceDir || 'unknown'}\nMinIO logs: ${minioPath}`,
      tags: ['crash', 'auto-filed', service],
      channel: 'mattermost',
      claimedBy: null,
      claimedAt: null,
      attempts: 0,
      maxAttempts: 1,
      lastAttempt: null,
      completedAt: null,
      result: null,
    };

    const data = await readQueue();
    data.items = data.items || [];
    data.items.push(task);
    data.lastSync = new Date().toISOString();
    await writeQueue(data);

    console.log(`[crash-report] Filed crash task ${task.id} for ${service}`);
    res.json({ ok: true, taskId: task.id });
  } catch (e) {
    console.error(`[crash-report] Error filing crash: ${e.message}`);
    res.status(500).json({ error: e.message });
  }
});

// --- Unified Dashboard HTML ---

function renderUnifiedPage() {
  return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>🐿️ Rocky Command Center</title>
  <style>
    * { box-sizing: border-box; margin: 0; padding: 0; }
    body { background: #0d1117; color: #c9d1d9; font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Helvetica, Arial, sans-serif; }
    a { color: #58a6ff; text-decoration: none; }
    .container { max-width: 1200px; margin: 0 auto; padding: 20px; }
    .section-header { font-size: 18px; font-weight: 700; color: #f0f6fc; padding: 12px 0 8px 0; border-bottom: 1px solid #21262d; margin-bottom: 12px; }
    .section { margin-bottom: 28px; }

    /* Agent cards */
    .agent-cards { display: flex; gap: 12px; flex-wrap: wrap; }
    .agent-card { background: #161b22; border: 1px solid #30363d; border-radius: 8px; padding: 12px 16px; flex: 1; min-width: 200px; max-height: 110px; }
    .agent-name { font-size: 17px; font-weight: 700; margin-bottom: 4px; }
    .agent-meta { color: #8b949e; font-size: 12px; margin-top: 2px; }
    .status-online { color: #3fb950; font-weight: 600; }
    .status-stale { color: #d29922; font-weight: 600; }
    .status-offline { color: #f85149; font-weight: 600; }

    /* Queue table */
    table { width: 100%; border-collapse: collapse; }
    th { text-align: left; padding: 8px 10px; color: #8b949e; font-size: 12px; border-bottom: 1px solid #30363d; }
    td { padding: 8px 10px; }
    tbody tr { border-bottom: 1px solid #21262d; }
    tbody tr:hover { background: #161b22; }
    .pill { display: inline-block; padding: 2px 8px; border-radius: 4px; font-size: 11px; font-weight: 600; color: #fff; }
    .pill-pending { background: #1f6feb; }
    .pill-in-progress { background: #a371f7; }
    .pill-blocked { background: #f85149; }
    .pill-deferred { background: #8b949e; }
    .pill-completed { background: #3fb950; }
    .pill-idea { background: #d29922; }
    .filter-bar { display: flex; gap: 6px; margin-bottom: 12px; flex-wrap: wrap; }
    .filter-btn { background: #21262d; color: #c9d1d9; border: 1px solid #30363d; padding: 5px 12px; border-radius: 16px; cursor: pointer; font-size: 12px; }
    .filter-btn.active { background: #1f6feb !important; color: #fff !important; border-color: #1f6feb !important; }
    .q-table-wrap { background: #161b22; border: 1px solid #30363d; border-radius: 8px; overflow-x: auto; }
    .wq-card { transition: border-color 0.15s, box-shadow 0.15s; }
    .wq-card:hover { box-shadow: 0 0 0 1px #58a6ff33; }
    .action-btn { border: none; padding: 3px 8px; border-radius: 4px; cursor: pointer; font-size: 11px; color: #fff; margin: 1px; }
    .action-btn.promote { background: #1f6feb; }
    .action-btn.complete { background: #238636; }
    .action-btn.comment { background: #d29922; }
    .cmt-input { background: #0d1117; color: #c9d1d9; border: 1px solid #30363d; padding: 3px 6px; border-radius: 4px; font-size: 11px; width: 110px; }

    /* Bus messages */
    .bus-filters { display: flex; gap: 6px; margin-bottom: 10px; flex-wrap: wrap; }
    .bus-filter-btn { background: #21262d; color: #c9d1d9; border: 1px solid #30363d; padding: 5px 12px; border-radius: 16px; cursor: pointer; font-size: 12px; }
    .bus-filter-btn.active { background: #1f6feb !important; color: #fff !important; border-color: #1f6feb !important; }
    .bus-msg { background: #161b22; border: 1px solid #30363d; border-radius: 8px; padding: 10px 14px; margin-bottom: 6px; }
    .bus-msg.compact { background: transparent; border: none; padding: 4px 14px; margin-bottom: 2px; color: #484f58; font-size: 12px; }
    .bus-msg.hidden { display: none; }
    .bus-header { display: flex; justify-content: space-between; align-items: center; margin-bottom: 4px; font-size: 13px; }
    .type-badge { display: inline-block; padding: 1px 6px; border-radius: 3px; font-size: 10px; color: #fff; margin-left: 4px; }

    /* Send form */
    .send-form { background: #161b22; border: 1px solid #30363d; border-radius: 8px; padding: 12px; margin-bottom: 12px; }
    .send-form summary { cursor: pointer; font-weight: 600; color: #58a6ff; font-size: 13px; }
    .send-form input, .send-form textarea, .send-form select { background: #0d1117; color: #c9d1d9; border: 1px solid #30363d; padding: 5px 8px; border-radius: 4px; font-size: 12px; }
    .send-form textarea { width: 100%; resize: vertical; min-height: 50px; }
    .send-btn { background: #238636; color: #fff; border: none; padding: 5px 14px; border-radius: 4px; cursor: pointer; font-weight: 600; font-size: 12px; }
    .send-btn:hover { background: #2ea043; }
    .new-task-btn { background: #238636; border: 1px solid #2ea04388; color: #fff; border-radius: 6px; padding: 4px 14px; font-size: 12px; font-weight: 600; cursor: pointer; }
    .new-task-btn:hover { background: #2ea043; }

    /* Master clock banner */
    .master-clock { background: #161b22; border: 1px solid #30363d; border-radius: 8px; padding: 8px 16px; margin-bottom: 16px; display: flex; align-items: center; gap: 16px; }
    .master-clock-time { font-size: 20px; font-weight: 700; font-family: 'SF Mono', 'Fira Code', monospace; color: #f0f6fc; letter-spacing: 0.04em; }
    .master-clock-label { color: #8b949e; font-size: 11px; font-weight: 600; text-transform: uppercase; letter-spacing: 0.08em; }
    .agent-checkin { font-size: 11px; color: #58a6ff; margin-top: 3px; font-family: 'SF Mono', 'Fira Code', monospace; }

    /* Toast */
    .toast { position: fixed; bottom: 20px; right: 20px; background: #238636; color: #fff; padding: 10px 18px; border-radius: 8px; font-size: 13px; display: none; z-index: 999; }
    .toast.error { background: #f85149; }

    /* Footer */
    .footer { color: #484f58; font-size: 11px; text-align: center; margin-top: 16px; padding-top: 12px; border-top: 1px solid #21262d; }
  </style>
</head>
<body>
  <div class="container">
    <!-- Master UTC Clock -->
    <div class="master-clock">
      <div>
        <div class="master-clock-label">Server UTC</div>
        <div class="master-clock-time" id="master-clock-time">--:--:-- UTC</div>
      </div>
      <div style="width:1px;height:36px;background:#30363d;margin:0 4px"></div>
      <div>
        <div class="master-clock-label">Server Date</div>
        <div style="font-size:13px;color:#c9d1d9;font-family:'SF Mono','Fira Code',monospace;margin-top:2px" id="master-clock-date">----</div>
      </div>
    </div>
    <div style="display:flex;align-items:center;justify-content:space-between;margin-bottom:20px">
      <h1 style="font-size:24px;margin:0;color:#f0f6fc">🐿️ Rocky Command Center</h1>
      <div style="display:flex;gap:8px;align-items:center">
        <a href="/activity" style="background:transparent;border:1px solid #30363d;color:#8b949e;border-radius:6px;padding:4px 12px;font-size:12px;cursor:pointer;text-decoration:none;transition:border-color .15s,color .15s" onmouseover="this.style.borderColor='#58a6ff';this.style.color='#58a6ff'" onmouseout="this.style.borderColor='#30363d';this.style.color='#8b949e'">🗺️ Activity Map</a>
        <a href="/geek" style="background:transparent;border:1px solid #30363d;color:#8b949e;border-radius:6px;padding:4px 12px;font-size:12px;cursor:pointer;text-decoration:none;transition:border-color .15s,color .15s" onmouseover="this.style.borderColor='#58a6ff';this.style.color='#58a6ff'" onmouseout="this.style.borderColor='#30363d';this.style.color='#8b949e'">🖥️ Geek View</a>
        <button onclick="openDigestModal()" style="background:transparent;border:1px solid #30363d;color:#8b949e;border-radius:6px;padding:4px 12px;font-size:12px;cursor:pointer;transition:border-color .15s,color .15s" onmouseover="this.style.borderColor='#58a6ff';this.style.color='#58a6ff'" onmouseout="this.style.borderColor='#30363d';this.style.color='#8b949e'">📊 Status</button>
        <button onclick="authFlow()" style="background:transparent;border:1px solid #30363d;color:#8b949e;border-radius:6px;padding:4px 12px;font-size:12px;cursor:pointer;transition:border-color .15s,color .15s" onmouseover="this.style.borderColor='#58a6ff';this.style.color='#58a6ff'" onmouseout="this.style.borderColor='#30363d';this.style.color='#8b949e'">🔑 Auth</button>
      </div>
    </div>

    <!-- Section 1: Agent Status -->
    <div class="section">
      <div class="section-header">🟢 Agent Status</div>
      <div class="agent-cards" id="agent-cards">
        <div class="agent-card"><span class="agent-meta">Loading...</span></div>
      </div>
    </div>

    <!-- Section 2: Work Queue -->
    <div class="section">
      <div class="section-header" style="display:flex;align-items:center;justify-content:space-between">
        <span>📋 Work Queue</span>
        <button class="new-task-btn" onclick="openNewTaskModal()">＋ New Task</button>
      </div>
      <div class="filter-bar" id="queue-filters"></div>
      <div id="queue-cards" style="display:grid;grid-template-columns:repeat(auto-fill,minmax(340px,1fr));gap:12px;margin-top:12px">
        <div style="padding:16px;color:#8b949e">Loading...</div>
      </div>
    </div>

    <!-- Item Detail Modal -->
    <div id="item-modal" style="display:none;position:fixed;top:0;left:0;right:0;bottom:0;background:rgba(0,0,0,0.7);z-index:1000;align-items:center;justify-content:center" onclick="if(event.target===this)closeItemModal()">
      <div style="background:#161b22;border:1px solid #30363d;border-radius:10px;width:90%;max-width:700px;max-height:90vh;overflow-y:auto;padding:24px">
        <div id="item-modal-body">Loading…</div>
      </div>
    </div>

    <!-- Section 3: Request Tickets -->
    <div class="section">
      <div class="section-header" style="display:flex;align-items:center;justify-content:space-between">
        <span>🎫 Request Tickets</span>
        <span id="requests-badge" style="font-size:12px;color:#8b949e"></span>
      </div>
      <div id="requests-table-wrap" class="q-table-wrap" style="margin-top:12px">
        <table style="width:100%;border-collapse:collapse">
          <thead>
            <tr style="border-bottom:1px solid #30363d;color:#8b949e;font-size:12px;text-align:left">
              <th style="padding:8px 12px">ID</th>
              <th style="padding:8px 12px">Requester</th>
              <th style="padding:8px 12px">Owner</th>
              <th style="padding:8px 12px">Summary</th>
              <th style="padding:8px 12px">Status</th>
              <th style="padding:8px 12px">Age</th>
              <th style="padding:8px 12px">Actions</th>
            </tr>
          </thead>
          <tbody id="requests-tbody"><tr><td colspan="7" style="padding:12px;color:#8b949e">Loading…</td></tr></tbody>
        </table>
      </div>
    </div>

    <!-- Section 4: SquirrelBus -->
    <div class="section">
      <div class="section-header">📡 SquirrelBus</div>
      <details class="send-form">
        <summary>✉️ Send a message</summary>
        <div style="display:grid;grid-template-columns:1fr 1fr 1fr;gap:6px;margin-top:8px">
          <div><label style="font-size:11px;color:#8b949e">From</label><select id="msg-from" style="width:100%"><option value="rocky">Rocky</option><option value="operator">operator</option></select></div>
          <div><label style="font-size:11px;color:#8b949e">To</label><select id="msg-to" style="width:100%"><option value="all">All</option><option value="rocky">Rocky</option><option value="bullwinkle">Bullwinkle</option><option value="natasha">Natasha</option><option value="operator">operator</option></select></div>
          <div><label style="font-size:11px;color:#8b949e">Type</label><select id="msg-type" style="width:100%"><option value="text">text</option><option value="memo">memo</option></select></div>
        </div>
        <div style="margin-top:6px"><label style="font-size:11px;color:#8b949e">Subject</label><input id="msg-subject" style="width:100%" placeholder="Optional subject..."></div>
        <div style="margin-top:6px"><label style="font-size:11px;color:#8b949e">Body</label><textarea id="msg-body" placeholder="Type your message..."></textarea></div>
        <div style="margin-top:6px;text-align:right"><button class="send-btn" onclick="sendBusMessage()">Send</button></div>
      </details>
      <div class="bus-filters" id="bus-filters"></div>
      <div id="bus-messages"><div style="color:#8b949e;padding:12px">Loading messages...</div></div>
    </div>

    <div class="footer" id="footer">Loading...</div>
  </div>

  <div class="toast" id="toast"></div>

  <!-- New Task Modal -->
  <div id="new-task-overlay" style="display:none;position:fixed;inset:0;background:#00000088;z-index:200;align-items:center;justify-content:center">
    <div style="background:#161b22;border:1px solid #30363d;border-radius:10px;padding:1.5rem;width:100%;max-width:480px;margin:1rem;box-shadow:0 8px 32px #000a">
      <div style="display:flex;align-items:center;justify-content:space-between;margin-bottom:1rem">
        <span style="font-size:1rem;font-weight:600;color:#f0f6fc">＋ New Task</span>
        <button onclick="closeNewTaskModal()" style="background:none;border:none;color:#6e7681;font-size:1.2rem;cursor:pointer;padding:0 4px">✕</button>
      </div>
      <div style="display:flex;flex-direction:column;gap:0.6rem">
        <div>
          <label style="font-size:11px;color:#8b949e;display:block;margin-bottom:3px">Title <span style="color:#f85149">*</span></label>
          <input id="nt-title" style="width:100%;background:#0d1117;border:1px solid #30363d;border-radius:5px;padding:6px 10px;color:#c9d1d9;font-size:13px;font-family:inherit" placeholder="Short task title..." />
        </div>
        <div>
          <label style="font-size:11px;color:#8b949e;display:block;margin-bottom:3px">Description</label>
          <textarea id="nt-desc" style="width:100%;background:#0d1117;border:1px solid #30363d;border-radius:5px;padding:6px 10px;color:#c9d1d9;font-size:13px;font-family:inherit;resize:vertical;min-height:70px" placeholder="What needs to be done?"></textarea>
        </div>
        <div style="display:grid;grid-template-columns:1fr 1fr;gap:0.6rem">
          <div>
            <label style="font-size:11px;color:#8b949e;display:block;margin-bottom:3px">Priority</label>
            <select id="nt-priority" style="width:100%;background:#0d1117;border:1px solid #30363d;border-radius:5px;padding:6px 8px;color:#c9d1d9;font-size:13px">
              <option value="normal" selected>normal</option>
              <option value="idea">idea</option>
              <option value="high">high</option>
              <option value="urgent">urgent</option>
            </select>
          </div>
          <div>
            <label style="font-size:11px;color:#8b949e;display:block;margin-bottom:3px">Assignee</label>
            <select id="nt-assignee" style="width:100%;background:#0d1117;border:1px solid #30363d;border-radius:5px;padding:6px 8px;color:#c9d1d9;font-size:13px">
              <option value="all" selected>all</option>
              <option value="rocky">rocky</option>
              <option value="bullwinkle">bullwinkle</option>
              <option value="natasha">natasha</option>
              <option value="boris">boris</option>
            </select>
          </div>
        </div>
        <div>
          <label style="font-size:11px;color:#8b949e;display:block;margin-bottom:3px">Tags <span style="color:#484f58">(comma-separated)</span></label>
          <input id="nt-tags" style="width:100%;background:#0d1117;border:1px solid #30363d;border-radius:5px;padding:6px 10px;color:#c9d1d9;font-size:13px;font-family:inherit" placeholder="e.g. infrastructure, gpu, memory" />
        </div>
        <div style="display:flex;gap:0.5rem;justify-content:flex-end;margin-top:0.4rem">
          <button onclick="closeNewTaskModal()" style="background:#21262d;border:1px solid #30363d;color:#c9d1d9;border-radius:6px;padding:6px 16px;font-size:13px;cursor:pointer">Cancel</button>
          <button id="nt-submit" onclick="submitNewTask()" style="background:#238636;border:none;color:#fff;border-radius:6px;padding:6px 18px;font-size:13px;font-weight:600;cursor:pointer">Create Task</button>
        </div>
      </div>
    </div>
  </div>

  <!-- Digest Modal -->
  <div id="digest-overlay" style="display:none;position:fixed;inset:0;background:#00000088;z-index:200;align-items:center;justify-content:center">
    <div style="background:#161b22;border:1px solid #30363d;border-radius:10px;padding:1.5rem;width:100%;max-width:600px;margin:1rem;box-shadow:0 8px 32px #000a;max-height:80vh;display:flex;flex-direction:column">
      <div style="display:flex;align-items:center;justify-content:space-between;margin-bottom:1rem">
        <span style="font-size:1rem;font-weight:600;color:#f0f6fc">📊 Agent Status Digest</span>
        <button onclick="closeDigestModal()" style="background:none;border:none;color:#6e7681;font-size:1.2rem;cursor:pointer;padding:0 4px">✕</button>
      </div>
      <div id="digest-content" style="font-family:monospace;font-size:12px;color:#c9d1d9;white-space:pre-wrap;overflow-y:auto;flex:1;line-height:1.6;background:#0d1117;border:1px solid #21262d;border-radius:6px;padding:12px">Loading…</div>
      <div style="margin-top:0.8rem;display:flex;justify-content:flex-end;gap:8px">
        <button onclick="refreshDigest()" style="background:#21262d;border:1px solid #30363d;color:#c9d1d9;border-radius:6px;padding:6px 14px;font-size:12px;cursor:pointer">↻ Refresh</button>
        <button onclick="closeDigestModal()" style="background:#21262d;border:1px solid #30363d;color:#c9d1d9;border-radius:6px;padding:6px 14px;font-size:12px;cursor:pointer">Close</button>
      </div>
    </div>
  </div>

  <script>
    // === Token management ===
    function getToken() {
      let t = localStorage.getItem('wq-token');
      if (!t) {
        t = prompt('Enter auth token:');
        if (t) { t = t.trim(); localStorage.setItem('wq-token', t); }
      }
      return t ? t.trim() : t;
    }
    function authFlow() {
      localStorage.removeItem('wq-token');
      const t = prompt('Enter auth token:');
      if (t && t.trim()) {
        localStorage.setItem('wq-token', t.trim());
        showToast('✅ Token updated');
      } else {
        showToast('❌ No token entered', true);
      }
    }
    async function authedFetch(url, opts = {}) {
      const token = getToken();
      if (!token) return null;
      opts.headers = Object.assign({}, opts.headers, { 'Authorization': 'Bearer ' + token });
      const r = await fetch(url, opts);
      if (r.status === 401) {
        showToast('⚠️ Unauthorized — update your token', true);
        authFlow();
        return null;
      }
      return r;
    }
    function resetToken() {
      localStorage.removeItem('wq-token');
      showToast('Token cleared — reload to re-enter');
    }
    function showToast(msg, isError) {
      const el = document.getElementById('toast');
      el.textContent = msg;
      el.className = 'toast' + (isError ? ' error' : '');
      el.style.display = 'block';
      setTimeout(() => el.style.display = 'none', 3000);
    }

    // === Helpers ===
    function esc(s) { if (!s) return ''; const d = document.createElement('div'); d.textContent = s; return d.innerHTML; }
    function tickClock() {
      const now = new Date();
      const hh = String(now.getUTCHours()).padStart(2, '0');
      const mm = String(now.getUTCMinutes()).padStart(2, '0');
      const ss = String(now.getUTCSeconds()).padStart(2, '0');
      const dateStr = now.toISOString().slice(0, 10);
      const el = document.getElementById('master-clock-time');
      const del = document.getElementById('master-clock-date');
      if (el) el.textContent = hh + ':' + mm + ':' + ss + ' UTC';
      if (del) del.textContent = dateStr;
    }
    tickClock();
    function timeAgo(ds) {
      if (!ds) return 'never';
      const diff = Date.now() - new Date(ds).getTime();
      const m = Math.floor(diff / 60000);
      if (m < 1) return 'just now';
      if (m < 60) return m + 'm ago';
      const h = Math.floor(m / 60);
      if (h < 24) return h + 'h ago';
      return Math.floor(h / 24) + 'd ago';
    }
    const OPERATOR_HANDLE = '${process.env.OPERATOR_HANDLE || "operator"}';
    const EMOJIS = { rocky: '🐿️', bullwinkle: '🫎', natasha: '🕵️‍♀️', boris: '🕵️‍♂️', [OPERATOR_HANDLE]: '👤' };
    const TYPE_COLORS = { text: '#58a6ff', memo: '#3fb950', blob: '#a371f7', heartbeat: '#8b949e', queue_sync: '#d29922', ping: '#3fb950', pong: '#3fb950', event: '#f85149', handoff: '#f0883e' };

    // === Section 1: Agent Cards ===
    async function loadAgents() {
      try {
        const hbs = await fetch('/api/heartbeats').then(r => r.json());
        const el = document.getElementById('agent-cards');
        const agentNames = Object.keys(hbs);
        el.innerHTML = agentNames.map(name => {
          const hb = hbs[name] || {};
          const emoji = EMOJIS[name] || '📨';
          let stClass = 'status-offline', stEmoji = '🔴', stLabel = 'offline';
          if (hb.ts) {
            const age = Date.now() - new Date(hb.ts).getTime();
            if (age < 45 * 60 * 1000) { stClass = 'status-online'; stEmoji = '🟢'; stLabel = 'online'; }
            else if (age < 4 * 60 * 60 * 1000) { stClass = 'status-stale'; stEmoji = '🟡'; stLabel = 'stale'; }
          }
          const host = hb.host || '—';
          const lastSeen = timeAgo(hb.ts);
          const checkinUtc = hb.ts ? new Date(hb.ts).toISOString().replace('T', ' ').slice(0, 19) + ' UTC' : 'never';
          const queueDepth = hb.queueDepth != null ? '<div class="agent-meta">Queue: ' + hb.queueDepth + ' items</div>' : '';
          return '<div class="agent-card">' +
            '<div class="agent-name">' + emoji + ' ' + name.charAt(0).toUpperCase() + name.slice(1) + '</div>' +
            '<div class="' + stClass + '">' + stEmoji + ' ' + stLabel + ' · ' + lastSeen + '</div>' +
            '<div class="agent-meta">Host: ' + esc(host) + '</div>' +
            '<div class="agent-checkin">⏱ ' + checkinUtc + '</div>' +
            queueDepth +
            '</div>';
        }).join('');
      } catch (e) { console.error('Agent load error:', e); }
    }

    // === Section 2: Work Queue ===
    let queueItems = [];
    let currentFilter = 'operator'; // default to jkh-assigned so blocked items are front and center
    const STATUS_ORDER = { 'in-progress': 0, blocked: 1, pending: 2, deferred: 3, idea: 4, completed: 5 };
    const PILL_CLASS = { pending: 'pill-pending', 'in-progress': 'pill-in-progress', blocked: 'pill-blocked', deferred: 'pill-deferred', completed: 'pill-completed', idea: 'pill-idea', cancelled: 'pill-deferred' };
    const PILL_LABEL = { pending: 'Pending', 'in-progress': 'In Progress', blocked: 'Blocked', deferred: 'Deferred', completed: 'Completed', idea: 'Idea', cancelled: 'Cancelled' };
    const PRIORITY_COLOR = { urgent: '#f85149', high: '#f0883e', normal: '#58a6ff', low: '#8b949e', idea: '#6e40c9' };

    async function loadQueue() {
      try {
        const data = await fetch('/api/queue').then(r => r.json());
        queueItems = data.items || [];
        renderQueueFilters();
        renderQueue();
      } catch (e) { console.error('Queue load error:', e); }
    }

    function renderQueueFilters() {
      const counts = { all: queueItems.length, jkh: 0 };
      for (const item of queueItems) {
        counts[item.status] = (counts[item.status] || 0) + 1;
        if (item.assignee === (OPERATOR_HANDLE || 'operator')) counts.operator++;
      }
      const filters = [['operator','⏳ Needs Me'],['all','All'],['pending','Pending'],['in-progress','In Progress'],['blocked','Blocked'],['deferred','Deferred'],['completed','Completed'],['idea','Ideas']];
      document.getElementById('queue-filters').innerHTML = filters.map(([val, label]) => {
        const c = counts[val] || 0;
        const active = currentFilter === val ? ' active' : '';
        return '<button class="filter-btn' + active + '" onclick="setQueueFilter(\\'' + val + '\\')">' + label + (c ? ' (' + c + ')' : '') + '</button>';
      }).join('');
    }

    function setQueueFilter(f) {
      currentFilter = f;
      renderQueueFilters();
      renderQueue();
    }

    function renderQueue() {
      let filtered = [...queueItems].sort((a, b) => (STATUS_ORDER[a.status] ?? 99) - (STATUS_ORDER[b.status] ?? 99));
      if (currentFilter === 'operator') filtered = filtered.filter(i => i.assignee === 'jkh');
      else if (currentFilter !== 'all') filtered = filtered.filter(i => i.status === currentFilter);
      const container = document.getElementById('queue-cards');
      if (!container) return;
      if (filtered.length === 0) {
        container.innerHTML = '<div style="padding:32px;color:#8b949e;text-align:center">No items' + (currentFilter === 'operator' ? ' — nothing needs your attention right now 🎉' : '') + '</div>';
        return;
      }
      container.innerHTML = filtered.map(item => renderCard(item)).join('');
    }

    function renderCard(item) {
      const isJkh = item.assignee === (OPERATOR_HANDLE || 'operator');
      const borderColor = isJkh ? '#f0883e' : (item.priority === 'high' || item.priority === 'urgent' ? '#f85149' : '#30363d');
      const pill = '<span class="pill ' + (PILL_CLASS[item.status] || 'pill-deferred') + '">' + (PILL_LABEL[item.status] || item.status) + '</span>';
      const prioColor = PRIORITY_COLOR[item.priority] || '#8b949e';
      const prioBadge = '<span style="font-size:11px;color:' + prioColor + ';font-weight:600;text-transform:uppercase">' + (item.priority || 'normal') + '</span>';
      const assigneeBadge = '<span style="font-size:11px;background:#21262d;border-radius:4px;padding:1px 6px;color:#8b949e">' + esc(item.assignee || '—') + '</span>';
      const age = timeAgo(item.created);
      const jkhBanner = isJkh ? '<div style="background:#f0883e22;border-left:3px solid #f0883e;padding:4px 8px;margin-bottom:8px;font-size:12px;color:#f0883e">⏳ Awaiting your decision</div>' : '';

      // Last journal entry preview
      const lastComment = item.journal && item.journal.length ? item.journal[item.journal.length-1] : null;
      const commentPreview = lastComment ? '<div style="margin-top:6px;font-size:11px;color:#8b949e;border-left:2px solid #30363d;padding-left:6px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis">' + esc(lastComment.author) + ': ' + esc(lastComment.text.slice(0,80)) + '</div>' : '';

      // Choice buttons
      let choiceButtons = '';
      if (item.choices && item.choices.length && !item.choiceRecorded) {
        choiceButtons = '<div style="margin-top:10px;display:flex;flex-wrap:wrap;gap:6px">' +
          item.choices.map(c =>
            '<button onclick="recordChoice(event,\\'' + item.id + '\\',\\'' + esc(c.id) + '\\',\\'' + esc(c.label) + '\\')" style="font-size:12px;padding:5px 10px;background:#21262d;border:1px solid #58a6ff;color:#58a6ff;border-radius:6px;cursor:pointer" onmouseover="this.style.background=\\'#58a6ff22\\'" onmouseout="this.style.background=\\'#21262d\\'">[' + esc(c.id) + '] ' + esc(c.label) + '</button>'
          ).join('') + '</div>';
      } else if (item.choiceRecorded) {
        choiceButtons = '<div style="margin-top:8px;font-size:12px;color:#3fb950">✅ Choice recorded: [' + esc(item.choiceRecorded.choice) + '] ' + esc(item.choiceRecorded.label) + '</div>';
      }

      // Quick actions
      let quickActions = '<div style="margin-top:10px;display:flex;gap:6px;flex-wrap:wrap">';
      if (item.status !== 'completed' && item.status !== 'cancelled') {
        quickActions += '<button onclick="quickComplete(event,\\'' + item.id + '\\')" style="font-size:11px;padding:3px 8px;background:#238636;border:none;color:#fff;border-radius:4px;cursor:pointer">✓ Complete</button>';
      }
      if (item.status === 'idea') {
        quickActions += '<button onclick="queueAction(event,\\'/api/upvote/' + item.id + '\\',\\'POST\\')" style="font-size:11px;padding:3px 8px;background:#6e40c9;border:none;color:#fff;border-radius:4px;cursor:pointer">⬆️ Promote</button>';
      }
      quickActions += '<span style="font-size:11px;color:#8b949e;align-self:center">' + age + '</span>';
      quickActions += '</div>';

      return '<div class="wq-card" style="background:#161b22;border:1px solid ' + borderColor + ';border-radius:8px;padding:14px;cursor:pointer;transition:border-color 0.15s" ' +
        'ondblclick="openItemModal(\\'' + item.id + '\\')" ' +
        'onmouseenter="this.style.borderColor=\\'' + (isJkh ? '#f0883e' : '#58a6ff') + '\\';" ' +
        'onmouseleave="this.style.borderColor=\\'' + borderColor + '\\';">' +
        jkhBanner +
        '<div style="display:flex;align-items:flex-start;justify-content:space-between;gap:8px">' +
          '<div style="flex:1;min-width:0">' +
            '<div style="font-weight:600;font-size:13px;margin-bottom:4px">' + esc(item.title) + '</div>' +
            '<div style="display:flex;gap:6px;flex-wrap:wrap;align-items:center">' + pill + prioBadge + assigneeBadge + '</div>' +
          '</div>' +
          '<div style="font-size:10px;color:#8b949e;white-space:nowrap">' + esc(item.id) + '</div>' +
        '</div>' +
        (item.description ? '<div style="margin-top:8px;font-size:12px;color:#8b949e">' + esc(item.description.slice(0,160)) + (item.description.length > 160 ? '…' : '') + '</div>' : '') +
        commentPreview +
        choiceButtons +
        quickActions +
        '<div style="margin-top:8px;font-size:10px;color:#484f58">Double-click to edit &amp; view journal · ' + (item.journal ? item.journal.length : 0) + ' journal entr' + ((item.journal && item.journal.length === 1) ? 'y' : 'ies') + '</div>' +
        '</div>';
    }

    async function recordChoice(e, id, choice, label) {
      e.stopPropagation();
      const token = getToken();
      if (!token) return showToast('No auth token', true);
      const resp = await fetch('/api/item/' + id + '/choice', {
        method: 'POST',
        headers: { 'Authorization': 'Bearer ' + token, 'Content-Type': 'application/json' },
        body: JSON.stringify({ choice, choiceLabel: label })
      });
      const data = await resp.json();
      if (data.ok) { showToast('Choice recorded: [' + choice + '] ' + label); loadQueue(); }
      else showToast(data.error || 'Error', true);
    }

    async function quickComplete(e, id) {
      e.stopPropagation();
      const token = getToken();
      if (!token) return showToast('No auth token', true);
      const resp = await fetch('/api/complete/' + id, { method: 'POST', headers: { 'Authorization': 'Bearer ' + token } });
      const data = await resp.json();
      if (data.ok) { showToast('Completed!'); loadQueue(); }
      else showToast(data.error || 'Error', true);
    }

    async function queueAction(e, url, method, body) {
      if (e) e.stopPropagation();
      const token = getToken();
      if (!token) return showToast('No token', true);
      try {
        const opts = { method, headers: { 'Authorization': 'Bearer ' + token, 'Content-Type': 'application/json' } };
        if (body) opts.body = JSON.stringify(body);
        const resp = await fetch(url, opts);
        const data = await resp.json();
        if (data.ok) { showToast('Done!'); loadQueue(); }
        else showToast(data.error || 'Error', true);
      } catch (err) { showToast(err.message, true); }
    }

    // === Item Detail Modal ===
    let modalItemId = null;

    async function openItemModal(id) {
      modalItemId = id;
      const item = await fetch('/api/item/' + id).then(r => r.json());
      const modal = document.getElementById('item-modal');
      const body = document.getElementById('item-modal-body');
      if (!modal || !body) return;

      const journalHtml = (item.journal || []).map(e => {
        const color = e.type === 'ai' ? '#a371f7' : e.type === 'choice' ? '#3fb950' : e.type === 'status-change' ? '#8b949e' : '#58a6ff';
        return '<div style="border-left:2px solid ' + color + ';padding:6px 10px;margin-bottom:8px;background:#0d1117;border-radius:0 4px 4px 0">' +
          '<div style="font-size:10px;color:#8b949e">' + esc(e.author) + ' · ' + new Date(e.ts).toLocaleString() + '</div>' +
          '<div style="font-size:13px;margin-top:2px;white-space:pre-wrap">' + esc(e.text) + '</div></div>';
      }).join('') || '<div style="color:#8b949e;font-size:13px">No journal entries yet.</div>';

      const choicesHtml = (item.choices && item.choices.length && !item.choiceRecorded) ?
        '<div style="margin:12px 0"><div style="font-size:12px;color:#8b949e;margin-bottom:6px">Choices:</div>' +
        item.choices.map(c => '<button onclick="recordChoice(null,\\'' + item.id + '\\',\\'' + esc(c.id) + '\\',\\'' + esc(c.label) + '\\')" style="display:block;width:100%;text-align:left;margin-bottom:6px;padding:8px 12px;background:#21262d;border:1px solid #58a6ff;color:#58a6ff;border-radius:6px;cursor:pointer;font-size:13px">[' + esc(c.id) + '] ' + esc(c.label) + '</button>').join('') + '</div>' :
        (item.choiceRecorded ? '<div style="margin:8px 0;color:#3fb950;font-size:13px">✅ Choice: [' + esc(item.choiceRecorded.choice) + '] ' + esc(item.choiceRecorded.label) + '</div>' : '');

      body.innerHTML =
        '<div style="display:flex;justify-content:space-between;align-items:flex-start;margin-bottom:16px">' +
          '<div style="font-size:11px;color:#8b949e">' + esc(item.id) + ' · created ' + timeAgo(item.created) + '</div>' +
          '<button onclick="closeItemModal()" style="background:none;border:none;color:#8b949e;font-size:18px;cursor:pointer">✕</button>' +
        '</div>' +
        '<div style="margin-bottom:12px"><label style="font-size:11px;color:#8b949e;display:block;margin-bottom:4px">Title</label>' +
          '<input id="modal-title" value="' + esc(item.title) + '" style="width:100%;box-sizing:border-box;background:#0d1117;border:1px solid #30363d;color:#c9d1d9;border-radius:4px;padding:8px;font-size:14px"></div>' +
        '<div style="display:grid;grid-template-columns:1fr 1fr 1fr;gap:8px;margin-bottom:12px">' +
          '<div><label style="font-size:11px;color:#8b949e;display:block;margin-bottom:4px">Status</label>' +
            '<select id="modal-status" style="width:100%;background:#0d1117;border:1px solid #30363d;color:#c9d1d9;border-radius:4px;padding:6px">' +
            ['pending','in-progress','blocked','deferred','completed','cancelled','idea'].map(s => '<option value="' + s + '"' + (item.status===s?' selected':'') + '>' + s + '</option>').join('') + '</select></div>' +
          '<div><label style="font-size:11px;color:#8b949e;display:block;margin-bottom:4px">Priority</label>' +
            '<select id="modal-priority" style="width:100%;background:#0d1117;border:1px solid #30363d;color:#c9d1d9;border-radius:4px;padding:6px">' +
            ['urgent','high','normal','low','idea'].map(p => '<option value="' + p + '"' + (item.priority===p?' selected':'') + '>' + p + '</option>').join('') + '</select></div>' +
          '<div><label style="font-size:11px;color:#8b949e;display:block;margin-bottom:4px">Assignee</label>' +
            '<input id="modal-assignee" value="' + esc(item.assignee||'') + '" style="width:100%;box-sizing:border-box;background:#0d1117;border:1px solid #30363d;color:#c9d1d9;border-radius:4px;padding:6px"></div>' +
        '</div>' +
        '<div style="margin-bottom:12px"><label style="font-size:11px;color:#8b949e;display:block;margin-bottom:4px">Description</label>' +
          '<textarea id="modal-description" rows="3" style="width:100%;box-sizing:border-box;background:#0d1117;border:1px solid #30363d;color:#c9d1d9;border-radius:4px;padding:8px;font-size:13px;resize:vertical">' + esc(item.description||'') + '</textarea></div>' +
        '<div style="margin-bottom:12px"><label style="font-size:11px;color:#8b949e;display:block;margin-bottom:4px">Notes</label>' +
          '<textarea id="modal-notes" rows="2" style="width:100%;box-sizing:border-box;background:#0d1117;border:1px solid #30363d;color:#c9d1d9;border-radius:4px;padding:8px;font-size:13px;resize:vertical">' + esc(item.notes||'') + '</textarea></div>' +
        '<div style="margin-bottom:16px"><button onclick="saveModalItem()" style="background:#238636;border:none;color:#fff;padding:8px 20px;border-radius:6px;cursor:pointer;font-size:13px;font-weight:600">💾 Save Changes</button></div>' +
        choicesHtml +
        '<div style="border-top:1px solid #30363d;padding-top:16px;margin-bottom:12px"><div style="font-size:13px;font-weight:600;margin-bottom:10px">📋 Journal (' + (item.journal||[]).length + ' entries)</div>' +
          '<div style="max-height:300px;overflow-y:auto;margin-bottom:12px">' + journalHtml + '</div>' +
          '<div style="display:flex;gap:8px;margin-bottom:8px">' +
            '<input id="modal-comment" placeholder="Add a comment…" style="flex:1;background:#0d1117;border:1px solid #30363d;color:#c9d1d9;border-radius:4px;padding:8px;font-size:13px" onkeydown="if(event.key===\\'Enter\\' && !event.shiftKey){addModalComment();event.preventDefault()}">' +
            '<button onclick="addModalComment()" style="background:#21262d;border:1px solid #30363d;color:#c9d1d9;padding:8px 14px;border-radius:4px;cursor:pointer">💬</button>' +
          '</div>' +
          '<div style="display:flex;gap:8px">' +
            '<input id="modal-ai" placeholder="✨ Ask AI about this item…" style="flex:1;background:#0d1117;border:1px solid #6e40c9;color:#c9d1d9;border-radius:4px;padding:8px;font-size:13px" onkeydown="if(event.key===\\'Enter\\' && !event.shiftKey){addModalAI();event.preventDefault()}">' +
            '<button onclick="addModalAI()" style="background:#6e40c9;border:none;color:#fff;padding:8px 14px;border-radius:4px;cursor:pointer">✨</button>' +
          '</div>' +
        '</div>';

      modal.style.display = 'flex';
    }

    function closeItemModal() {
      const modal = document.getElementById('item-modal');
      if (modal) modal.style.display = 'none';
      modalItemId = null;
    }

    async function saveModalItem() {
      if (!modalItemId) return;
      const token = getToken();
      if (!token) return showToast('No auth token', true);
      const patch = {
        title: document.getElementById('modal-title').value,
        status: document.getElementById('modal-status').value,
        priority: document.getElementById('modal-priority').value,
        assignee: document.getElementById('modal-assignee').value,
        description: document.getElementById('modal-description').value,
        notes: document.getElementById('modal-notes').value,
      };
      const resp = await fetch('/api/item/' + modalItemId, {
        method: 'PATCH',
        headers: { 'Authorization': 'Bearer ' + token, 'Content-Type': 'application/json' },
        body: JSON.stringify(patch)
      });
      const data = await resp.json();
      if (data.ok) { showToast('Saved!'); loadQueue(); openItemModal(modalItemId); }
      else showToast(data.error || 'Error', true);
    }

    async function addModalComment() {
      if (!modalItemId) return;
      const token = getToken();
      if (!token) return showToast('No auth token', true);
      const input = document.getElementById('modal-comment');
      const text = input.value.trim();
      if (!text) return;
      const resp = await fetch('/api/item/' + modalItemId + '/comment', {
        method: 'POST',
        headers: { 'Authorization': 'Bearer ' + token, 'Content-Type': 'application/json' },
        body: JSON.stringify({ text, author: OPERATOR_HANDLE })
      });
      const data = await resp.json();
      if (data.ok) { input.value = ''; showToast('Comment added!'); openItemModal(modalItemId); loadQueue(); }
      else showToast(data.error || 'Error', true);
    }

    async function addModalAI() {
      if (!modalItemId) return;
      const token = getToken();
      if (!token) return showToast('No auth token', true);
      const input = document.getElementById('modal-ai');
      const prompt = input.value.trim();
      if (!prompt) return;
      input.value = '';
      input.placeholder = '✨ Thinking…';
      input.disabled = true;
      const resp = await fetch('/api/item/' + modalItemId + '/ai-comment', {
        method: 'POST',
        headers: { 'Authorization': 'Bearer ' + token, 'Content-Type': 'application/json' },
        body: JSON.stringify({ prompt })
      });
      const data = await resp.json();
      input.placeholder = '✨ Ask AI about this item…';
      input.disabled = false;
      if (data.ok) { showToast('AI responded!'); openItemModal(modalItemId); loadQueue(); }
      else showToast(data.error || 'Error', true);
    }

    function sendComment(id) {
      const input = document.getElementById('cmt-' + id);
      const text = input ? input.value.trim() : '';
      if (!text) return showToast('Enter a comment first', true);
      queueAction(null, '/api/comment/' + id, 'POST', { text });
    }

    // === Section 3: SquirrelBus ===
    let busMessages = [];
    let busFilter = 'all';
    let lastBusTs = null;
    let busDeliveryStatus = {}; // messageId → 'acked'|'pending-ack'|'dead'

    async function loadBus(initial) {
      try {
        const url = initial ? '/bus/messages?limit=50' : '/bus/messages?limit=50' + (lastBusTs ? '&since=' + encodeURIComponent(lastBusTs) : '');
        const [msgs, delivery] = await Promise.all([
          fetch(url).then(r => r.json()),
          fetch('/bus/delivery-status').then(r => r.json()).catch(() => ({})),
        ]);
        busDeliveryStatus = delivery;
        if (initial) {
          busMessages = msgs;
        } else if (msgs.length > 0) {
          // Prepend new messages (they come newest-first)
          const existingIds = new Set(busMessages.map(m => m.id));
          const newMsgs = msgs.filter(m => !existingIds.has(m.id));
          if (newMsgs.length > 0) busMessages = [...newMsgs, ...busMessages];
        }
        if (busMessages.length > 0 && busMessages[0].ts) lastBusTs = busMessages[0].ts;
        renderBusFilters();
        renderBus();
      } catch (e) { console.error('Bus load error:', e); }
    }

    function renderBusFilters() {
      const agents = ['all', 'rocky', 'bullwinkle', 'natasha', 'jkh'];
      document.getElementById('bus-filters').innerHTML = agents.map(agent => {
        const emoji = agent === 'all' ? '📡' : (EMOJIS[agent] || '📨');
        const active = busFilter === agent ? ' active' : '';
        return '<button class="bus-filter-btn' + active + '" onclick="setBusFilter(\\'' + agent + '\\')">' + emoji + ' ' + agent.charAt(0).toUpperCase() + agent.slice(1) + '</button>';
      }).join('');
    }

    function setBusFilter(f) {
      busFilter = f;
      renderBusFilters();
      renderBus();
    }

    function renderBus() {
      const filtered = busFilter === 'all' ? busMessages : busMessages.filter(m => m.from === busFilter || m.to === busFilter);
      if (filtered.length === 0) {
        document.getElementById('bus-messages').innerHTML = '<div style="color:#8b949e;padding:12px">No messages</div>';
        return;
      }
      document.getElementById('bus-messages').innerHTML = filtered.map(renderBusMsg).join('');
    }

    function renderBusMsg(msg) {
      const fromEmoji = EMOJIS[msg.from] || '📨';
      const toLabel = msg.to === 'all' ? 'all' : msg.to;
      const ts = new Date(msg.ts).toLocaleString('en-US', { timeZone: 'America/Los_Angeles', month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' });
      const typeColor = TYPE_COLORS[msg.type] || '#8b949e';

      // Compact rendering for heartbeat/ping/pong
      if (msg.type === 'heartbeat' || msg.type === 'ping' || msg.type === 'pong') {
        const icon = msg.type === 'heartbeat' ? '💓' : '🏓';
        return '<div class="bus-msg compact" data-from="' + esc(msg.from) + '">' +
          fromEmoji + ' ' + esc(msg.from) + ' ' + icon + ' ' + msg.type + ' · #' + msg.seq + ' · ' + esc(ts) +
          '</div>';
      }

      let subject = msg.subject ? '<div style="font-weight:600;color:#58a6ff;margin-bottom:3px;font-size:13px">' + esc(msg.subject) + '</div>' : '';
      let bodyHtml = '';

      switch (msg.type) {
        case 'text':
        case 'memo':
          bodyHtml = '<div style="white-space:pre-wrap;font-size:13px">' + esc(msg.body) + '</div>';
          break;
        case 'blob':
          if (msg.mime && msg.mime.startsWith('image/')) {
            const src = msg.enc === 'base64' ? 'data:' + msg.mime + ';base64,' + msg.body : esc(msg.body);
            bodyHtml = '<img src="' + src + '" style="max-width:360px;border-radius:6px;margin-top:4px">';
          } else if (msg.mime && msg.mime.startsWith('audio/')) {
            const src = msg.enc === 'base64' ? 'data:' + msg.mime + ';base64,' + msg.body : esc(msg.body);
            bodyHtml = '<audio controls src="' + src + '" style="margin-top:4px"></audio>';
          } else if (msg.mime && msg.mime.startsWith('video/')) {
            const src = msg.enc === 'base64' ? 'data:' + msg.mime + ';base64,' + msg.body : esc(msg.body);
            bodyHtml = '<video controls src="' + src + '" style="max-width:360px;border-radius:6px;margin-top:4px"></video>';
          } else {
            bodyHtml = '<pre style="background:#0d1117;padding:6px;border-radius:4px;overflow-x:auto;font-size:11px">' + esc((msg.body || '').slice(0, 500)) + '</pre>';
          }
          break;
        case 'queue_sync':
          bodyHtml = '<details style="margin-top:4px"><summary style="cursor:pointer;color:#58a6ff;font-size:12px">Queue sync data</summary><pre style="background:#0d1117;padding:6px;border-radius:4px;overflow-x:auto;font-size:11px;margin-top:4px">' + esc(typeof msg.body === 'string' ? msg.body : JSON.stringify(msg.body, null, 2)) + '</pre></details>';
          break;
        default:
          bodyHtml = '<pre style="background:#0d1117;padding:6px;border-radius:4px;overflow-x:auto;font-size:11px">' + esc(JSON.stringify(msg, null, 2)) + '</pre>';
      }

      const dStatus = busDeliveryStatus[msg.id];
      const dIcon = dStatus === 'acked' ? ' ✅' : dStatus === 'dead' ? ' 💀' : dStatus === 'pending-ack' ? ' 🔴' : '';

      return '<div class="bus-msg" data-from="' + esc(msg.from) + '">' +
        '<div class="bus-header">' +
          '<div>' + fromEmoji + ' <strong style="color:#f0f6fc">' + esc(msg.from) + '</strong>' +
          ' <span style="color:#484f58">→</span> <strong>' + esc(toLabel) + '</strong>' +
          ' <span class="type-badge" style="background:' + typeColor + '">' + esc(msg.type) + '</span>' +
          (dIcon ? '<span title="' + (dStatus || '') + '" style="margin-left:6px;font-size:13px">' + dIcon + '</span>' : '') + '</div>' +
          '<div style="color:#484f58;font-size:11px">#' + msg.seq + ' · ' + esc(ts) + '</div>' +
        '</div>' +
        subject + bodyHtml +
        '</div>';
    }

    // === Send bus message ===
    async function sendBusMessage() {
      const token = getToken();
      if (!token) return showToast('No token', true);
      const body = {
        from: document.getElementById('msg-from').value,
        to: document.getElementById('msg-to').value,
        type: document.getElementById('msg-type').value,
        subject: document.getElementById('msg-subject').value || null,
        body: document.getElementById('msg-body').value,
      };
      if (!body.body && body.type !== 'ping') return showToast('Body required', true);
      try {
        const resp = await fetch('/bus/send', {
          method: 'POST',
          headers: { 'Authorization': 'Bearer ' + token, 'Content-Type': 'application/json' },
          body: JSON.stringify(body),
        });
        const data = await resp.json();
        if (data.ok) {
          showToast('Message sent!');
          document.getElementById('msg-body').value = '';
          document.getElementById('msg-subject').value = '';
          loadBus(true);
        } else showToast(data.error || 'Error', true);
      } catch (e) { showToast(e.message, true); }
    }

    // === New Task Modal ===
    // === Digest Modal ===
    async function openDigestModal() {
      const overlay = document.getElementById('digest-overlay');
      overlay.style.display = 'flex';
      document.getElementById('digest-content').textContent = 'Loading…';
      await refreshDigest();
    }

    function closeDigestModal() {
      document.getElementById('digest-overlay').style.display = 'none';
    }

    async function refreshDigest() {
      const el = document.getElementById('digest-content');
      try {
        const resp = await fetch('/api/digest');
        if (!resp.ok) throw new Error('HTTP ' + resp.status);
        const data = await resp.json();
        el.textContent = data.digest || '(empty)';
      } catch (e) {
        el.textContent = '⚠️ Failed to load digest: ' + e.message;
      }
    }

    document.getElementById('digest-overlay').addEventListener('click', function(e) {
      if (e.target === this) closeDigestModal();
    });

    function openNewTaskModal() {
      const el = document.getElementById('new-task-overlay');
      el.style.display = 'flex';
      document.getElementById('nt-title').focus();
    }

    function closeNewTaskModal() {
      document.getElementById('new-task-overlay').style.display = 'none';
      document.getElementById('nt-title').value = '';
      document.getElementById('nt-desc').value = '';
      document.getElementById('nt-priority').value = 'normal';
      document.getElementById('nt-assignee').value = 'all';
      document.getElementById('nt-tags').value = '';
      const btn = document.getElementById('nt-submit');
      btn.disabled = false;
      btn.textContent = 'Create Task';
    }

    // Close modal on overlay click
    document.getElementById('new-task-overlay').addEventListener('click', function(e) {
      if (e.target === this) closeNewTaskModal();
    });

    // Close modal on Escape key
    document.addEventListener('keydown', function(e) {
      if (e.key === 'Escape') closeNewTaskModal();
    });

    async function submitNewTask() {
      const title = document.getElementById('nt-title').value.trim();
      if (!title) {
        document.getElementById('nt-title').style.borderColor = '#f85149';
        document.getElementById('nt-title').focus();
        return;
      }
      document.getElementById('nt-title').style.borderColor = '';

      const btn = document.getElementById('nt-submit');
      btn.disabled = true;
      btn.textContent = '⏳ Creating…';

      const tagsRaw = document.getElementById('nt-tags').value;
      const tags = tagsRaw ? tagsRaw.split(',').map(t => t.trim()).filter(Boolean) : [];

      const payload = {
        title,
        description: document.getElementById('nt-desc').value.trim(),
        priority: document.getElementById('nt-priority').value,
        assignee: document.getElementById('nt-assignee').value,
        tags,
      };

      try {
        const r = await authedFetch('/api/queue', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify(payload),
        });
        if (!r) { btn.disabled = false; btn.textContent = 'Create Task'; return; }
        const j = await r.json();
        if (r.ok && j.ok) {
          closeNewTaskModal();
          showToast('✅ Created: ' + j.item.id);
          loadQueue();
        } else {
          showToast('⚠️ ' + (j.error || 'Unknown error'), true);
          btn.disabled = false;
          btn.textContent = 'Create Task';
        }
      } catch (e) {
        showToast('⚠️ ' + e.message, true);
        btn.disabled = false;
        btn.textContent = 'Create Task';
      }
    }

    // === Request Tickets ===
    async function loadRequests() {
      try {
        const r = await authedFetch('/api/requests');
        if (!r) return;
        const tickets = await r.json();
        const tbody = document.getElementById('requests-tbody');
        const badge = document.getElementById('requests-badge');
        const open = tickets.filter(t => t.status !== 'closed');
        badge.textContent = open.length ? open.length + ' open' : 'all closed';
        if (!tickets.length) {
          tbody.innerHTML = '<tr><td colspan="7" style="padding:12px;color:#8b949e">No tickets</td></tr>';
          return;
        }
        const now = Date.now();
        tbody.innerHTML = tickets.map(t => {
          const age = now - new Date(t.created).getTime();
          const ageStr = age < 60000 ? '<1m' : age < 3600000 ? Math.round(age/60000)+'m' : age < 86400000 ? Math.round(age/3600000)+'h' : Math.round(age/86400000)+'d';
          const stale = age > 24*3600000 && t.status !== 'closed';
          const rowStyle = stale ? 'background:#2d1b1b' : '';
          const statusColor = {open:'#3fb950',delegated:'#d29922',resolved:'#58a6ff',closed:'#8b949e'}[t.status]||'#8b949e';
          const reqId = esc(t.requester?.id||'?') + (t.requester?.channel ? ' ('+esc(t.requester.channel)+')' : '');
          const delegHtml = (t.delegations||[]).length ? '<details style="margin-top:4px"><summary style="cursor:pointer;color:#58a6ff;font-size:11px">'+t.delegations.length+' delegation(s)</summary><div style="margin-top:4px;padding:4px 8px;background:#0d1117;border-radius:4px;font-size:11px">'+(t.delegations.map((d,i) => '<div style="margin:4px 0"><b>→ '+esc(d.to)+'</b>: '+esc(d.summary)+(d.resolvedAt?' ✅ '+esc(d.outcome||''):'')+'</div>').join(''))+'</div></details>' : '';
          const closeBtn = t.status !== 'closed' ? '<button onclick="closeTicket(\''+esc(t.id)+'\')" style="background:#21262d;border:1px solid #30363d;color:#8b949e;border-radius:4px;padding:2px 8px;font-size:11px;cursor:pointer">Close</button>' : '';
          return '<tr style="border-bottom:1px solid #21262d;'+rowStyle+'">'
            + '<td style="padding:8px 12px;font-size:11px;color:#8b949e;white-space:nowrap">'+esc(t.id)+'</td>'
            + '<td style="padding:8px 12px;font-size:12px">'+reqId+'</td>'
            + '<td style="padding:8px 12px;font-size:12px">'+esc(t.owner||'?')+'</td>'
            + '<td style="padding:8px 12px;font-size:12px">'+esc(t.summary)+''+delegHtml+'</td>'
            + '<td style="padding:8px 12px"><span style="color:'+statusColor+';font-size:12px;font-weight:600">'+t.status+'</span>'+(stale?' <span style="color:#f85149;font-size:10px">⚠️ stale</span>':'')+'</td>'
            + '<td style="padding:8px 12px;font-size:12px;white-space:nowrap">'+ageStr+'</td>'
            + '<td style="padding:8px 12px">'+closeBtn+'</td>'
            + '</tr>';
        }).join('');
      } catch(e) { console.error('Requests load error:', e); }
    }

    async function closeTicket(id) {
      const resolution = prompt('Resolution summary (shown to requester):');
      if (resolution === null) return;
      const r = await authedFetch('/api/requests/'+id+'/close', {
        method: 'POST',
        headers: {'Content-Type':'application/json'},
        body: JSON.stringify({ resolution }),
      });
      if (r?.ok) { showToast('Ticket closed'); loadRequests(); }
    }

    // === Init & refresh ===
    loadAgents();
    loadQueue();
    loadBus(true);
    loadRequests();

    // Auto-refresh agent cards every 30s
    // Master clock — tick every second
    setInterval(tickClock, 1000);
    // Auto-refresh agent cards every 30s
    setInterval(loadAgents, 30000);
    // Auto-refresh queue every 60s
    setInterval(loadQueue, 60000);
    // Auto-refresh bus every 10s (incremental)
    setInterval(() => loadBus(false), 10000);
    // Auto-refresh requests every 60s
    setInterval(loadRequests, 60000);

    document.getElementById('footer').textContent = '🐿️ Rocky Command Center · Auto-refreshing · Rendered: ' + new Date().toLocaleString();
  </script>
</body>
</html>`;
}

// --- Metrics API ---

app.get('/api/metrics', async (req, res) => {
  try {
    const data = await readQueue();
    const now = Date.now();
    const windowMs = 24 * 60 * 60 * 1000; // 24h

    // All items (active + completed array)
    const allItems = [...(data.items || []), ...(data.completed || [])];

    // items_completed_24h
    const completed24h = allItems.filter(i =>
      i.status === 'completed' && i.completedAt &&
      (now - new Date(i.completedAt).getTime()) < windowMs
    );

    // avg_time_to_completion_h (for items with both createdAt and completedAt)
    const timings = completed24h
      .filter(i => i.created && i.completedAt)
      .map(i => (new Date(i.completedAt).getTime() - new Date(i.created).getTime()) / 3600000);
    const avg_ttc = timings.length > 0
      ? parseFloat((timings.reduce((a, b) => a + b, 0) / timings.length).toFixed(2))
      : null;

    // blocked_count
    const blocked = (data.items || []).filter(i => i.status === 'blocked');

    // pending_by_assignee
    const pending = (data.items || []).filter(i => i.status === 'pending');
    const pendingByAssignee = {};
    for (const item of pending) {
      const a = item.assignee || 'unassigned';
      pendingByAssignee[a] = (pendingByAssignee[a] || 0) + 1;
    }

    // in_progress_by_assignee
    const inProgress = (data.items || []).filter(i => i.status === 'in_progress' || i.status === 'in-progress');
    const inProgressByAssignee = {};
    for (const item of inProgress) {
      const a = item.assignee || 'unassigned';
      inProgressByAssignee[a] = (inProgressByAssignee[a] || 0) + 1;
    }

    // total_active (pending + in_progress + blocked)
    const totalActive = pending.length + inProgress.length + blocked.length;

    // idea backlog count
    const ideas = (data.items || []).filter(i => i.status === 'pending' && i.priority === 'idea');

    res.json({
      ts: new Date().toISOString(),
      items_completed_24h: completed24h.length,
      avg_time_to_completion_h: avg_ttc,
      blocked_count: blocked.length,
      total_active: totalActive,
      pending_count: pending.length,
      in_progress_count: inProgress.length,
      idea_backlog: ideas.length,
      pending_by_assignee: pendingByAssignee,
      in_progress_by_assignee: inProgressByAssignee,
      last_completed: completed24h.length > 0
        ? completed24h.sort((a, b) => new Date(b.completedAt) - new Date(a.completedAt))[0]
        : null,
    });
  } catch (e) {
    res.status(500).json({ error: e.message });
  }
});

// --- Changelog API ---
// GET /api/changelog?id=<itemId>&limit=30
// Returns reconstructed status-change history for a queue item, newest first.
// History is assembled from: (a) notes field lines containing bracketed events,
// (b) key timestamp fields (created, claimedAt, completedAt), (c) itemVersion bumps.
app.get('/api/changelog', async (req, res) => {
  try {
    const itemId = (req.query.id || '').trim();
    if (!itemId) return res.status(400).json({ error: 'id query param required' });
    const limit = Math.min(parseInt(req.query.limit) || 30, 100);

    const data = await readQueue();
    const allItems = [...(data.items || []), ...(data.completed || [])];
    const item = allItems.find(i => i.id === itemId);
    if (!item) return res.status(404).json({ error: 'item not found', id: itemId });

    const events = [];

    // 1. Parse bracketed/timestamped events from notes field
    // Supports patterns like:
    //   "[promoted] idea→normal via quorum 2026-03-21T04:08Z"
    //   "operator comment [2026-03-21T04:08:00.000Z]: text"
    //   "[escalated] normal→high at 2026-03-21T04:08:00.000Z"
    //   "Unblocked by completion of X at 2026-03-21T04:08:00.000Z"
    if (item.notes) {
      const noteLines = item.notes.split('\n').filter(l => l.trim());
      for (const line of noteLines) {
        // Extract ISO timestamp from line if present
        const isoMatch = line.match(/(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}(?::\d{2})?(?:\.\d+)?Z?)/);
        const ts = isoMatch ? new Date(isoMatch[1]).toISOString() : null;

        // Classify event type from content
        let type = 'note';
        const lower = line.toLowerCase();
        if (/\[promoted\]|promoted to task/.test(lower))     type = 'promotion';
        else if (/\[escalated\]/.test(lower))                type = 'escalation';
        else if (/unblocked/.test(lower))                    type = 'unblocked';
        else if (/claimed by|claimedby/.test(lower))         type = 'claim';
        else if (/operator comment/.test(lower))                  type = 'comment';
        else if (/completed by|marked complete/.test(lower)) type = 'completion';
        else if (/assigned to/.test(lower))                  type = 'assignment';
        else if (/proposed/.test(lower))                     type = 'proposed';

        events.push({ ts, type, detail: line.trim(), source: 'notes' });
      }
    }

    // 2. Synthetic events from structured timestamp fields
    if (item.created) {
      events.push({ ts: item.created, type: 'created', detail: `Item created (source: ${item.source || 'unknown'})`, source: 'field' });
    }
    if (item.claimedAt && item.claimedBy) {
      events.push({ ts: item.claimedAt, type: 'claim', detail: `Claimed by ${item.claimedBy}`, source: 'field' });
    }
    if (item.lastAttempt) {
      events.push({ ts: item.lastAttempt, type: 'attempt', detail: `Last attempt recorded`, source: 'field' });
    }
    if (item.completedAt) {
      events.push({ ts: item.completedAt, type: 'completed', detail: `Completed (status: ${item.status})`, source: 'field' });
    }

    // 3. Current state snapshot
    events.push({
      ts: null,
      type: 'current_state',
      detail: `status=${item.status} assignee=${item.assignee || '—'} priority=${item.priority} itemVersion=${item.itemVersion || 1}`,
      source: 'snapshot',
    });

    // Sort: timestamped entries newest-first, null-ts (current_state) at top
    events.sort((a, b) => {
      if (!a.ts && !b.ts) return 0;
      if (!a.ts) return -1;
      if (!b.ts) return 1;
      return new Date(b.ts) - new Date(a.ts);
    });

    // Deduplicate by (ts, detail) — notes and field events can overlap
    const seen = new Set();
    const deduped = events.filter(e => {
      const key = `${e.ts}|${e.detail}`;
      if (seen.has(key)) return false;
      seen.add(key);
      return true;
    });

    res.json({
      id: itemId,
      title: item.title || itemId,
      itemVersion: item.itemVersion || 1,
      totalEvents: deduped.length,
      changelog: deduped.slice(0, limit),
    });
  } catch (e) {
    res.status(500).json({ error: e.message });
  }
});

// --- Dashboard route ---
app.get('/', (req, res) => {
  res.type('html').send(renderUnifiedPage());
});

// Redirect /bus to /
app.get('/activity', (req, res) => {
  res.send(`<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Activity Map — RCC</title>
<style>
  * { box-sizing: border-box; margin: 0; padding: 0; }
  body { background: #0d1117; color: #c9d1d9; font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif; height: 100vh; overflow: hidden; }
  #header { display: flex; align-items: center; gap: 16px; padding: 12px 20px; background: #161b22; border-bottom: 1px solid #30363d; }
  #header h1 { font-size: 16px; font-weight: 600; color: #f0f6fc; }
  #header .subtitle { font-size: 12px; color: #8b949e; }
  #legend { display: flex; gap: 20px; margin-left: auto; font-size: 11px; }
  #legend span { display: flex; align-items: center; gap: 5px; }
  .dot { width: 10px; height: 10px; border-radius: 50%; display: inline-block; }
  #canvas-wrap { width: 100%; height: calc(100vh - 53px); position: relative; }
  svg { width: 100%; height: 100%; }
  .node circle { stroke-width: 2; cursor: pointer; transition: opacity 0.2s; }
  .node:hover circle { opacity: 0.85; }
  .node text { font-size: 11px; fill: #c9d1d9; text-anchor: middle; pointer-events: none; user-select: none; }
  .node .emoji { font-size: 16px; }
  .link { stroke: #30363d; stroke-opacity: 0.4; }
  .link.worked-on { stroke: #58a6ff; stroke-opacity: 0.3; }
  .link.contributor { stroke: #3fb950; stroke-opacity: 0.3; }
  .link.directs { stroke: #f85149; stroke-opacity: 0.2; }
  #tooltip { position: fixed; background: #161b22; border: 1px solid #30363d; border-radius: 8px; padding: 10px 14px; font-size: 12px; line-height: 1.6; pointer-events: none; opacity: 0; transition: opacity 0.15s; z-index: 100; max-width: 260px; }
  #tooltip .tt-title { font-weight: 600; color: #f0f6fc; font-size: 13px; margin-bottom: 4px; }
  #back { color: #58a6ff; text-decoration: none; font-size: 13px; }
  #back:hover { text-decoration: underline; }
  .kind-badge { font-size: 10px; padding: 1px 6px; border-radius: 10px; background: #21262d; color: #8b949e; margin-left: 6px; }
</style>
</head>
<body>
<div id="header">
  <a href="/" id="back">← Dashboard</a>
  <h1>🗺️ Activity Map</h1>
  <div class="subtitle">People · Agents · Projects — bubble size = activity, color = recency</div>
  <div id="legend">
    <span><span class="dot" style="background:#f85149"></span>Hot (&lt;1h)</span>
    <span><span class="dot" style="background:#e3b341"></span>Warm (&lt;3d)</span>
    <span><span class="dot" style="background:#58a6ff"></span>Cool (&lt;7d)</span>
    <span><span class="dot" style="background:#30363d"></span>Cold</span>
  </div>
</div>
<div id="canvas-wrap">
  <svg id="viz"></svg>
</div>
<div id="tooltip"></div>

<script src="https://cdnjs.cloudflare.com/ajax/libs/d3/7.9.0/d3.min.js" crossorigin="anonymous"></script>
<script>
const TOKEN = localStorage.getItem('wq_auth_token') || '';

async function load() {
  const r = await fetch('/api/activity', { headers: { Authorization: 'Bearer ' + TOKEN } });
  if (!r.ok) { document.body.innerHTML += '<p style="padding:20px;color:#f85149">Error loading data: ' + r.status + '</p>'; return; }
  const data = await r.json();
  render(data);
}

function render({ nodes, edges }) {
  const svg = d3.select('#viz');
  const wrap = document.getElementById('canvas-wrap');
  let W = wrap.clientWidth, H = wrap.clientHeight;
  svg.attr('viewBox', \`0 0 \${W} \${H}\`);

  const kindForce = { agent: { cx: W * 0.5, cy: H * 0.4 }, project: { cx: W * 0.5, cy: H * 0.7 }, person: { cx: W * 0.5, cy: H * 0.15 } };

  const sim = d3.forceSimulation(nodes)
    .force('link', d3.forceLink(edges).id(d => d.id).distance(d => 80 + d.weight * 8).strength(0.15))
    .force('charge', d3.forceManyBody().strength(d => -d.size * 6))
    .force('collision', d3.forceCollide(d => d.size + 8))
    .force('x', d3.forceX(d => kindForce[d.kind]?.cx ?? W/2).strength(0.06))
    .force('y', d3.forceY(d => kindForce[d.kind]?.cy ?? H/2).strength(0.1))
    .force('center', d3.forceCenter(W/2, H/2).strength(0.01));

  // Kind zone labels
  const zones = [
    { label: '👤 People', x: W * 0.12, y: H * 0.08 },
    { label: '🤖 Agents', x: W * 0.12, y: H * 0.38 },
    { label: '📁 Projects', x: W * 0.12, y: H * 0.68 },
  ];
  zones.forEach(z => {
    svg.append('text').attr('x', z.x).attr('y', z.y)
      .attr('fill', '#21262d').attr('font-size', '13px').attr('font-weight', '600')
      .text(z.label);
  });

  // Divider lines
  [H * 0.27, H * 0.56].forEach(y => {
    svg.append('line').attr('x1', 0).attr('y1', y).attr('x2', W).attr('y2', y)
      .attr('stroke', '#21262d').attr('stroke-dasharray', '4,6');
  });

  // Links
  const link = svg.append('g').selectAll('line')
    .data(edges).enter().append('line')
    .attr('class', d => 'link ' + d.kind)
    .attr('stroke-width', d => Math.sqrt(d.weight));

  // Nodes
  const node = svg.append('g').selectAll('g')
    .data(nodes).enter().append('g')
    .attr('class', 'node')
    .call(d3.drag()
      .on('start', (e, d) => { if (!e.active) sim.alphaTarget(0.3).restart(); d.fx = d.x; d.fy = d.y; })
      .on('drag',  (e, d) => { d.fx = e.x; d.fy = e.y; })
      .on('end',   (e, d) => { if (!e.active) sim.alphaTarget(0); d.fx = null; d.fy = null; }));

  node.append('circle')
    .attr('r', d => d.size)
    .attr('fill', d => d.color + '22')
    .attr('stroke', d => d.color);

  // Emoji label
  node.append('text').attr('class', 'emoji').attr('dy', '0.35em').text(d => d.emoji);
  // Name label below
  node.append('text').attr('dy', d => d.size + 14).attr('font-size', '10px')
    .attr('fill', '#8b949e').text(d => d.label);

  // Tooltip
  const tip = document.getElementById('tooltip');
  node.on('mouseover', (e, d) => {
    const m = d.meta || {};
    let rows = '';
    if (d.kind === 'agent') rows = \`
      <div>✅ Completed: \${m.completedItems || 0}</div>
      <div>⚡ In progress: \${m.activeItems || 0}</div>
      <div>💓 Last heartbeat: \${m.lastHeartbeat ? new Date(m.lastHeartbeat).toLocaleTimeString() : 'unknown'}</div>
      <div>🖥️ Host: \${m.host || '?'}</div>\`;
    else if (d.kind === 'project') rows = \`
      <div>\${m.kind === 'team' ? '👥 Team' : '👤 Personal'} · \${m.issueTracker || 'github'}</div>
      <div>✅ Items completed: \${m.completedItems || 0}</div>
      <div>📅 Items last 7d: \${m.recentItems || 0}</div>
      <div>👥 Contributors: \${m.contributors || 0}</div>
      <div>🕐 Last activity: \${m.lastActivity ? new Date(m.lastActivity).toLocaleDateString() : 'unknown'}</div>\`;
    else rows = \`
      <div>Role: \${m.role || 'contributor'}</div>
      \${m.commits ? '<div>📝 Commits: ' + m.commits + '</div>' : ''}
      \${m.itemsAssigned ? '<div>📋 Items assigned: ' + m.itemsAssigned + '</div>' : ''}
      \${m.repos ? '<div>Repos: ' + (m.repos||[]).join(', ') + '</div>' : ''}\`;
    tip.innerHTML = \`<div class="tt-title">\${d.emoji} \${d.label}<span class="kind-badge">\${d.kind}</span></div>\${rows}\`;
    tip.style.opacity = '1';
    tip.style.left = (e.pageX + 14) + 'px';
    tip.style.top  = (e.pageY - 10) + 'px';
  }).on('mousemove', e => {
    tip.style.left = (e.pageX + 14) + 'px';
    tip.style.top  = (e.pageY - 10) + 'px';
  }).on('mouseout', () => { tip.style.opacity = '0'; });

  sim.on('tick', () => {
    link
      .attr('x1', d => d.source.x).attr('y1', d => d.source.y)
      .attr('x2', d => d.target.x).attr('y2', d => d.target.y);
    node.attr('transform', d => \`translate(\${d.x},\${d.y})\`);
  });

  // Auto-refresh every 60s
  setTimeout(() => { svg.selectAll('*').remove(); load(); }, 60000);
}

load();
</script>
</body>
</html>`);
});

app.get('/bus', (req, res) => {
  // Check if this is an API-like request or browser request
  if (req.path === '/bus' && !req.path.startsWith('/bus/')) {
    return res.redirect('/');
  }
  res.redirect('/');
});

// ========================================
// SquirrelBus v1 — Inter-agent comms
// ========================================

let busSeq = 0;
const busSSEClients = new Set();
const busPresence = {};

// Delivery confirmation state
const busAcks       = new Map(); // messageId → { messageId, agent, ts }
const busRetryQueue = new Map(); // messageId → { msg, targetAgent, retryCount, nextRetryAt, lastAttemptAt, timer }
const busDeadLetters = [];       // [{ ...msg, _deadReason, _deadAt, _retryCount }]

async function persistAck(ack) {
  try { await appendFile(ACK_LOG_PATH, JSON.stringify(ack) + '\n', 'utf8'); } catch {}
}

async function persistDead(entry) {
  try { await appendFile(DEAD_LOG_PATH, JSON.stringify(entry) + '\n', 'utf8'); } catch {}
}

function moveToDead(messageId, reason) {
  const entry = busRetryQueue.get(messageId);
  if (!entry) return;
  if (entry.timer) clearTimeout(entry.timer);
  busRetryQueue.delete(messageId);
  const dead = { ...entry.msg, _deadReason: reason, _deadAt: new Date().toISOString(), _retryCount: entry.retryCount };
  busDeadLetters.push(dead);
  persistDead(dead);
  console.log(`[bus-dead] ${messageId} → dead letter (${reason})`);
}

async function attemptRetry(messageId) {
  // Already acked?
  if (busAcks.has(messageId)) { busRetryQueue.delete(messageId); return; }
  const entry = busRetryQueue.get(messageId);
  if (!entry) return;

  entry.retryCount++;
  entry.lastAttemptAt = new Date().toISOString();
  const url   = BUS_RECEIVE_URLS[entry.targetAgent];
  const token = PEER_TOKENS[entry.targetAgent];

  if (url) {
    try {
      const ctrl = new AbortController();
      const tid = setTimeout(() => ctrl.abort(), 10000);
      const resp = await fetch(url, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', 'Authorization': `Bearer ${token}` },
        body: JSON.stringify(entry.msg),
        signal: ctrl.signal,
      });
      clearTimeout(tid);
      console.log(`[bus-retry] → ${entry.targetAgent} (attempt ${entry.retryCount}): HTTP ${resp.status}`);
    } catch (err) {
      console.warn(`[bus-retry] → ${entry.targetAgent} (attempt ${entry.retryCount}): ${err.message}`);
    }
  }

  if (entry.retryCount >= MAX_RETRIES) {
    moveToDead(messageId, 'max-retries');
  } else {
    scheduleRetry(messageId);
  }
}

function scheduleRetry(messageId) {
  const entry = busRetryQueue.get(messageId);
  if (!entry) return;
  if (entry.timer) clearTimeout(entry.timer);
  entry.nextRetryAt = new Date(Date.now() + RETRY_DELAY_MS).toISOString();
  entry.timer = setTimeout(() => attemptRetry(messageId), RETRY_DELAY_MS);
}

function trackMessageForRetry(msg) {
  // Only track directed messages to known agents with a receive URL
  if (msg.to === 'all' || !BUS_RECEIVE_URLS[msg.to]) return;
  busRetryQueue.set(msg.id, {
    msg,
    targetAgent: msg.to,
    retryCount: 0,
    createdAt: new Date().toISOString(),
    nextRetryAt: null,
    lastAttemptAt: null,
    timer: null,
  });
  scheduleRetry(msg.id);
}

async function initBusSeq() {
  try {
    if (!existsSync(BUS_LOG_PATH)) return;
    const rl = createInterface({ input: createRS(BUS_LOG_PATH), crlfDelay: Infinity });
    for await (const line of rl) {
      try {
        const msg = JSON.parse(line);
        if (msg.seq && msg.seq > busSeq) busSeq = msg.seq;
      } catch {}
    }
    console.log(`📡 SquirrelBus: initialized seq=${busSeq}`);
  } catch (e) {
    console.error('Bus seq init error:', e.message);
  }
}

async function readBusMessages({ from, to, limit = 100, since, type } = {}) {
  const messages = [];
  try {
    if (!existsSync(BUS_LOG_PATH)) return messages;
    const rl = createInterface({ input: createRS(BUS_LOG_PATH), crlfDelay: Infinity });
    for await (const line of rl) {
      try {
        const msg = JSON.parse(line);
        if (from && msg.from !== from) continue;
        if (to && msg.to !== to && msg.to !== 'all') continue;
        if (type && msg.type !== type) continue;
        if (since && new Date(msg.ts) <= new Date(since)) continue;
        messages.push(msg);
      } catch {}
    }
  } catch {}
  return messages.slice(-limit).reverse();
}

async function appendBusMessage(msg) {
  const full = {
    id: msg.id || randomUUID(),
    from: msg.from || 'unknown',
    to: msg.to || 'all',
    ts: msg.ts || new Date().toISOString(),
    seq: ++busSeq,
    type: msg.type || 'text',
    mime: msg.mime || 'text/plain',
    enc: msg.enc || 'none',
    body: msg.body || '',
    ref: msg.ref || null,
    subject: msg.subject || null,
    ttl: msg.ttl ?? 604800,
  };
  const line = JSON.stringify(full) + '\n';
  await appendFile(BUS_LOG_PATH, line, 'utf8');

  try {
    execFile(MC_PATH, ['cp', BUS_LOG_PATH, `${MINIO_ALIAS}/agents/shared/squirrelbus.jsonl`], { timeout: 10000 }, (err) => {
      if (err) console.error('MinIO bus upload error:', err.message);
    });
  } catch {}

  for (const client of busSSEClients) {
    try {
      client.write(`data: ${JSON.stringify(full)}\n\n`);
    } catch { busSSEClients.delete(client); }
  }

  // Fire-and-forget fan-out to registered peer endpoints
  fanOutBusMessage(full);

  // Track for delivery confirmation + retry (directed messages only)
  trackMessageForRetry(full);

  return full;
}

// CORS for bus endpoints
app.use('/bus', (req, res, next) => {
  res.header('Access-Control-Allow-Origin', '*');
  res.header('Access-Control-Allow-Headers', 'Content-Type, Authorization');
  res.header('Access-Control-Allow-Methods', 'GET, POST, OPTIONS');
  if (req.method === 'OPTIONS') return res.sendStatus(200);
  next();
});

app.post('/bus/send', requireAuth, async (req, res) => {
  try {
    const msg = await appendBusMessage(req.body);
    // Emit geek topology traffic event for live particle animations
    if (msg.from && msg.to) {
      _emitGeekOnBusSend(msg.from, msg.to, msg.type || msg.kind || 'message');
    }
    res.json({ ok: true, message: msg });
  } catch (e) {
    res.status(500).json({ error: e.message });
  }
});

app.get('/bus/messages', async (req, res) => {
  try {
    const { from, to, limit, since, type } = req.query;
    const messages = await readBusMessages({
      from, to, type, since,
      limit: limit ? parseInt(limit, 10) : 100,
    });
    res.json(messages);
  } catch (e) {
    res.status(500).json({ error: e.message });
  }
});

app.get('/bus/stream', (req, res) => {
  res.writeHead(200, {
    'Content-Type': 'text/event-stream',
    'Cache-Control': 'no-cache',
    'Connection': 'keep-alive',
  });
  res.write('data: {"type":"connected"}\n\n');
  busSSEClients.add(res);
  req.on('close', () => busSSEClients.delete(res));
});

app.post('/bus/heartbeat', requireAuth, async (req, res) => {
  try {
    const { from } = req.body;
    if (!from) return res.status(400).json({ error: 'Missing "from" field' });
    busPresence[from] = {
      agent: from,
      ts: new Date().toISOString(),
      status: 'online',
      ...req.body,
    };
    await appendBusMessage({
      from,
      to: 'all',
      type: 'heartbeat',
      body: JSON.stringify({ status: 'online', ...req.body }),
      mime: 'application/json',
    });
    res.json({ ok: true, presence: busPresence });
  } catch (e) {
    res.status(500).json({ error: e.message });
  }
});

app.get('/bus/presence', (req, res) => {
  res.json(busPresence);
});

// POST /bus/ack — recipient confirms delivery
app.post('/bus/ack', requireAuth, async (req, res) => {
  const { messageId, agent } = req.body;
  if (!messageId || !agent) return res.status(400).json({ error: 'messageId and agent required' });
  const ack = { messageId, agent, ts: new Date().toISOString() };
  busAcks.set(messageId, ack);
  // Clear retry timer
  const retryEntry = busRetryQueue.get(messageId);
  if (retryEntry) {
    if (retryEntry.timer) clearTimeout(retryEntry.timer);
    busRetryQueue.delete(messageId);
  }
  await persistAck(ack);
  res.json({ ok: true, ack });
});

// GET /bus/dead — inspect dead letter queue
app.get('/bus/dead', (req, res) => {
  res.json(busDeadLetters);
});

// GET /bus/message/:id/status — delivery status for a message
app.get('/bus/message/:id/status', async (req, res) => {
  const id = req.params.id;
  const ack   = busAcks.get(id) || null;
  const retry = busRetryQueue.get(id) || null;
  const dead  = busDeadLetters.find(d => d.id === id) || null;

  let ackState = 'fire-and-forget';
  if (dead)  ackState = 'dead';
  else if (ack) ackState = 'acked';
  else if (retry) ackState = 'pending-ack';

  res.json({
    id,
    ackState,
    ack: ack || null,
    retryCount: retry?.retryCount ?? dead?._retryCount ?? 0,
    nextRetryAt: retry?.nextRetryAt ?? null,
    deadReason: dead?._deadReason ?? null,
  });
});

// GET /bus/delivery-status — bulk status map for dashboard
app.get('/bus/delivery-status', (req, res) => {
  const result = {};
  for (const [id] of busAcks) result[id] = 'acked';
  for (const [id] of busRetryQueue) result[id] = 'pending-ack';
  for (const d of busDeadLetters) result[d.id] = 'dead';
  res.json(result);
});

// ── Agent Status Digest ────────────────────────────────────────────────────────

const DIGEST_AGENTS = loadCapabilityNames().map(name => ({
  name,
  emoji: '📨',
  healthFile: `agent-health-${name}.json`,
}));

async function fetchMinIOJson(path) {
  try {
    const { stdout } = await execFileP(MC_PATH, ['cat', `${MINIO_ALIAS}/${path}`], { timeout: 6000 });
    return JSON.parse(stdout);
  } catch { return null; }
}

function digestOnlineStatus(health) {
  if (!health) return 'unknown';
  const tsField = health.ts || health.timestamp || health.lastTs || null;
  if (!tsField) return 'unknown';
  const ageMs = Date.now() - new Date(tsField).getTime();
  if (ageMs < 40 * 60 * 1000) return 'online';
  if (ageMs < 4 * 60 * 60 * 1000) return 'idle';
  return 'offline';
}

async function buildAgentDigest() {
  let queueData = { items: [], completed: [] };
  try {
    queueData = await readQueue();
  } catch { /* queue unavailable */ }

  const items     = queueData.items     || [];
  const completed = queueData.completed || [];
  const now       = Date.now();
  const day24h    = now - 24 * 60 * 60 * 1000;
  const day7d     = now - 7 * 24 * 60 * 60 * 1000;

  function agentStats(name) {
    const claimedBy  = i => i.claimedBy === name;
    const isAssigned = i => i.assignee === name || i.assignee === 'all' || claimedBy(i);
    return {
      done24h:    completed.filter(i => claimedBy(i) && i.completedAt && new Date(i.completedAt).getTime() > day24h).length,
      done7d:     completed.filter(i => claimedBy(i) && i.completedAt && new Date(i.completedAt).getTime() > day7d).length,
      inProgress: items.filter(i => (i.status === 'claimed' || i.status === 'in-progress') && claimedBy(i)),
      pending:    items.filter(i => i.status === 'pending' && isAssigned(i)),
    };
  }

  const healthResults = await Promise.all(
    DIGEST_AGENTS.map(a => fetchMinIOJson(`agents/shared/${a.healthFile}`))
  );

  const agentData = DIGEST_AGENTS.map((a, i) => ({
    ...a,
    health: healthResults[i],
    status: digestOnlineStatus(healthResults[i]),
    stats:  agentStats(a.name),
  }));

  const totalPending   = items.filter(i => i.status === 'pending').length;
  const totalClaimed   = items.filter(i => i.status === 'claimed' || i.status === 'in-progress').length;
  const totalCompleted = completed.length;
  const totalIdeas     = items.filter(i => i.status === 'idea').length;

  const ts = new Date().toISOString().replace('T', ' ').slice(0, 19) + ' UTC';
  const lines = [`📊 Agent Status Digest — ${ts}`, ''];

  for (const a of agentData) {
    const { done24h, done7d, inProgress, pending } = a.stats;
    lines.push(`${a.emoji} ${a.name.charAt(0).toUpperCase() + a.name.slice(1)} (${a.status}): ${done24h} done today, ${done7d} this week`);
    inProgress.forEach(i => lines.push(`  ▸ In progress: [${i.id}] ${i.title}`));
    pending.slice(0, 3).forEach(i => lines.push(`  ▸ Pending: [${i.id}] ${i.title}`));
    if (pending.length > 3) lines.push(`  ▸ … and ${pending.length - 3} more pending`);
    if (inProgress.length === 0 && pending.length === 0) lines.push(`  ▸ Nothing assigned`);
    lines.push('');
  }
  lines.push(`Queue: ${totalPending} pending, ${totalClaimed} in-progress, ${totalIdeas} ideas, ${totalCompleted} completed total`);

  const agents = {};
  for (const a of agentData) {
    agents[a.name] = {
      status:     a.status,
      done24h:    a.stats.done24h,
      done7d:     a.stats.done7d,
      inProgress: a.stats.inProgress.map(i => ({ id: i.id, title: i.title })),
      pending:    a.stats.pending.slice(0, 5).map(i => ({ id: i.id, title: i.title })),
    };
  }

  return {
    digest: lines.join('\n'),
    agents,
    queueStats: { totalPending, totalClaimed, totalIdeas, totalCompleted },
    ts,
  };
}

app.get('/api/digest', async (req, res) => {
  try {
    const result = await buildAgentDigest();
    res.json(result);
  } catch (e) {
    res.status(500).json({ error: e.message });
  }
});

// ── Activity Bubble Chart API ──────────────────────────────────────────────────
// Returns nodes for people, agents, and projects with activity scores + recency
// Used by /activity bubble chart visualization
app.get('/api/activity', async (req, res) => {
  try {
    const data = await readQueue();
    const hbs = await getHeartbeats();
    const now = Date.now();
    const allItems = [...(data.items || []), ...(data.completed || [])];

    // Time windows for recency scoring
    const H1  = 60 * 60 * 1000;
    const H24 = 24 * H1;
    const H72 = 72 * H1;
    const D7  = 7 * 24 * H1;

    function recencyScore(tsStr) {
      if (!tsStr) return 0;
      const age = now - new Date(tsStr).getTime();
      if (age < H1)  return 1.0;
      if (age < H24) return 0.8;
      if (age < H72) return 0.5;
      if (age < D7)  return 0.2;
      return 0.05;
    }

    // Color from recency score: red (hot) → amber → blue (cold)
    function recencyColor(score) {
      if (score >= 0.8) return '#f85149'; // hot red
      if (score >= 0.5) return '#e3b341'; // warm amber
      if (score >= 0.2) return '#58a6ff'; // cool blue
      return '#30363d';                   // cold grey
    }

    // ── AGENT nodes ────────────────────────────────────────────────────────
    const AGENT_EMOJIS = { rocky:'🐿️', bullwinkle:'🫎', natasha:'🕵️‍♀️', boris:'🕵️‍♂️' };
    const agentNodes = [];
    for (const [name, hb] of Object.entries(hbs)) {
      const agentItems = allItems.filter(i =>
        i.claimedBy === name || i.assignee === name
      );
      const completed = agentItems.filter(i => i.status === 'completed');
      const lastAct = hb.ts || completed.sort((a,b) =>
        new Date(b.completedAt||0) - new Date(a.completedAt||0))[0]?.completedAt;
      const score = recencyScore(lastAct);
      agentNodes.push({
        id: `agent:${name}`,
        kind: 'agent',
        label: name,
        emoji: AGENT_EMOJIS[name] || '🤖',
        size: 20 + Math.min(completed.length * 2, 60),
        score,
        color: recencyColor(score),
        meta: {
          completedItems: completed.length,
          activeItems: agentItems.filter(i => i.status === 'in-progress').length,
          lastHeartbeat: hb.ts,
          host: hb.host,
        },
      });
    }

    // ── PROJECT nodes ──────────────────────────────────────────────────────
    // Load registered repos
    let repos = [];
    try {
      const reposPath = '/home/jkh/.openclaw/workspace/rcc/api/repos.json';
      repos = JSON.parse(await readFile(reposPath, 'utf8'));
    } catch {}

    const projectNodes = [];
    for (const repo of repos) {
      const repoItems = allItems.filter(i =>
        i.repo === repo.full_name ||
        (i.tags || []).includes(repo.full_name) ||
        (i.title || '').toLowerCase().includes(repo.full_name.split('/')[1].toLowerCase())
      );
      const completed = repoItems.filter(i => i.status === 'completed');
      const lastCompletion = completed.sort((a,b) =>
        new Date(b.completedAt||0) - new Date(a.completedAt||0))[0]?.completedAt;

      // Get recent GitHub commits velocity (rough: use item creation timestamps)
      const recent7d = repoItems.filter(i =>
        i.created && (now - new Date(i.created).getTime()) < D7
      );

      const score = recencyScore(lastCompletion);
      const contribs = repo.ownership?.contributors || [];
      const contribCount = Array.isArray(contribs) ? contribs.length : 0;

      projectNodes.push({
        id: `project:${repo.full_name}`,
        kind: 'project',
        label: repo.display_name || repo.full_name.split('/')[1],
        fullName: repo.full_name,
        emoji: repo.kind === 'team' ? '👥' : '👤',
        size: 18 + Math.min(completed.length * 1.5 + contribCount * 0.5, 70),
        score,
        color: recencyColor(score),
        meta: {
          kind: repo.kind,
          completedItems: completed.length,
          recentItems: recent7d.length,
          contributors: contribCount,
          issueTracker: repo.issue_tracker,
          lastActivity: lastCompletion,
        },
      });
    }

    // ── PERSON nodes ───────────────────────────────────────────────────────
    // People = jkh + any human contributors found in repo data
    const peopleMap = new Map();

    // jkh is always present
    const op = OPERATOR_HANDLE; const jkhItems = allItems.filter(i => i.assignee === op);
    const jkhLastAct = jkhItems.sort((a,b) =>
      new Date(b.completedAt||b.created||0) - new Date(a.completedAt||a.created||0))[0];
    const jkhScore = recencyScore(jkhLastAct?.completedAt || jkhLastAct?.created);
    peopleMap.set(op, {
      id: 'person:' + op,
      kind: 'person',
      label: op,
      emoji: '👤',
      size: 35 + jkhItems.length,
      score: Math.max(jkhScore, 0.3), // jkh is always relevant
      color: recencyColor(Math.max(jkhScore, 0.3)),
      meta: { role: 'owner', itemsAssigned: jkhItems.length },
    });

    // Contributors from team repos
    for (const repo of repos.filter(r => r.kind === 'team')) {
      const contribs = repo.ownership?.contributors || [];
      for (const c of contribs) {
        const login = typeof c === 'string' ? c : c.github;
        const commits = typeof c === 'object' ? c.commits : 0;
        if (login === (process.env.GITHUB_OWNER || '')) continue; // skip repo owner
        if (!peopleMap.has(login)) {
          peopleMap.set(login, {
            id: `person:${login}`,
            kind: 'person',
            label: login,
            emoji: '👤',
            size: 12 + Math.min(Math.log1p(commits || 1) * 5, 40),
            score: 0.15, // no direct activity data yet — grey-ish
            color: '#30363d',
            meta: { role: 'contributor', commits, repos: [repo.full_name] },
          });
        } else {
          const existing = peopleMap.get(login);
          existing.meta.repos = [...(existing.meta.repos || []), repo.full_name];
          existing.size = Math.min(existing.size + 5, 60);
        }
      }
    }

    // ── EDGES ──────────────────────────────────────────────────────────────
    const edges = [];

    // Agent → Project edges (agent worked on project)
    for (const agentNode of agentNodes) {
      const agentName = agentNode.label;
      for (const projectNode of projectNodes) {
        const repoName = projectNode.fullName;
        const count = allItems.filter(i =>
          (i.claimedBy === agentName || i.assignee === agentName) &&
          ((i.repo === repoName) || (i.tags||[]).includes(repoName) ||
           (i.title||'').toLowerCase().includes(repoName.split('/')[1].toLowerCase()))
        ).length;
        if (count > 0) {
          edges.push({ source: agentNode.id, target: projectNode.id, weight: count, kind: 'worked-on' });
        }
      }
    }

    // Person → Project edges (owner/contributor)
    for (const [login, personNode] of peopleMap) {
      for (const repo of repos) {
        const contribs = repo.ownership?.contributors || [];
        const isContrib = contribs.some(c => (typeof c === 'string' ? c : c.github) === login)
          || (login === op && repo.ownership?.owner === op);
        if (isContrib) {
          edges.push({ source: personNode.id, target: `project:${repo.full_name}`, weight: 2, kind: 'contributor' });
        }
      }
    }

    // Person → Agent edges (jkh directs agents)
    for (const agentNode of agentNodes) {
      edges.push({ source: 'person:' + op, target: agentNode.id, weight: 3, kind: 'directs' });
    }

    const nodes = [...agentNodes, ...projectNodes, ...[...peopleMap.values()]];

    res.json({
      ts: new Date().toISOString(),
      nodes,
      edges,
      legend: {
        kinds: ['agent', 'project', 'person'],
        sizeMetric: 'activity volume (items completed + contributors)',
        colorMetric: 'recency: red=hot (<1h), amber=warm (<3d), blue=cool (<7d), grey=cold',
      },
    });
  } catch (e) {
    console.error('/api/activity error:', e);
    res.status(500).json({ error: e.message });
  }
});

// ── S3 Proxy Routes ────────────────────────────────────────────────────────────
// Provides authenticated MinIO access for agents without Tailscale (e.g. Boris)
// All write/read routes require Bearer $RCC_AGENT_TOKEN

app.use('/s3', (req, res, next) => {
  res.header('Access-Control-Allow-Origin', '*');
  res.header('Access-Control-Allow-Headers', 'Content-Type, Authorization');
  res.header('Access-Control-Allow-Methods', 'GET, PUT, DELETE, OPTIONS');
  if (req.method === 'OPTIONS') return res.sendStatus(200);
  next();
});

// Health check — no auth required
app.get('/s3/health', async (req, res) => {
  try {
    await execFileP(MC_PATH, ['ls', MINIO_ALIAS], { timeout: 5000 });
    res.json({ ok: true, minio: 'connected' });
  } catch {
    res.status(503).json({ ok: false, minio: 'unreachable' });
  }
});

// List objects in bucket: GET /s3/:bucket
app.get('/s3/:bucket', requireAuth, async (req, res) => {
  const { bucket } = req.params;
  try {
    const { stdout } = await execFileP(MC_PATH, [
      'ls', '--json', `${MINIO_ALIAS}/${bucket}/`
    ], { timeout: 15000 });
    const objects = stdout.trim().split('\n').filter(Boolean).map(line => {
      try { return JSON.parse(line); } catch { return null; }
    }).filter(Boolean);
    res.json({ bucket, objects });
  } catch (e) {
    res.status(500).json({ error: e.message });
  }
});

// Get object: GET /s3/:bucket/*key
app.get('/s3/:bucket/*splat', requireAuth, async (req, res) => {
  req.params.key = req.params[0];
  const { bucket } = req.params;
  const key = [].concat(req.params.splat).join('/');
  try {
    const { stdout } = await execFileP(MC_PATH, [
      'cat', `${MINIO_ALIAS}/${bucket}/${key}`
    ], { timeout: 30000, maxBuffer: 50 * 1024 * 1024, encoding: 'buffer' });
    // Detect content type by extension
    const ext = key.split('.').pop().toLowerCase();
    const ctypes = { json: 'application/json', txt: 'text/plain', html: 'text/html', png: 'image/png', jpg: 'image/jpeg' };
    res.set('Content-Type', ctypes[ext] || 'application/octet-stream');
    res.send(stdout);
  } catch (e) {
    if (e.stderr && e.stderr.toString().includes('does not exist')) {
      return res.status(404).json({ error: 'Object not found' });
    }
    res.status(500).json({ error: e.message });
  }
});

// Put object: PUT /s3/:bucket/*key — streams body via mc pipe
app.put('/s3/:bucket/*splat', requireAuth, (req, res) => {
  const { bucket } = req.params;
  const key = [].concat(req.params.splat).join('/');
  const target = `${MINIO_ALIAS}/${bucket}/${key}`;

  const child = spawn(MC_PATH, ['pipe', target]);
  let stderr = '';
  child.stderr.on('data', d => stderr += d.toString());
  child.on('close', code => {
    if (code !== 0) return res.status(500).json({ error: stderr.trim() || 'Upload failed' });
    res.json({ ok: true, bucket, key: key });
  });

  // Body may already be parsed by express.json (if Content-Type: application/json)
  if (req.body !== undefined && typeof req.body === 'object' && !(req.body instanceof Buffer)) {
    child.stdin.write(JSON.stringify(req.body));
    child.stdin.end();
  } else if (req.body instanceof Buffer) {
    child.stdin.write(req.body);
    child.stdin.end();
  } else {
    req.pipe(child.stdin);
    req.on('error', err => { child.stdin.destroy(); res.status(500).json({ error: err.message }); });
  }
});

// Delete object: DELETE /s3/:bucket/*key
app.delete('/s3/:bucket/*splat', requireAuth, async (req, res) => {
  const { bucket } = req.params;
  const key = [].concat(req.params.splat).join('/');
  try {
    await execFileP(MC_PATH, [
      'rm', `${MINIO_ALIAS}/${bucket}/${key}`
    ], { timeout: 10000 });
    res.json({ ok: true, bucket, key });
  } catch (e) {
    res.status(500).json({ error: e.message });
  }
});

// ═══════════════════════════════════════════════════════════════════════════════
// Dashboard v2 — Phase 2 Frontend
// New tabs: Overview, Kanban, Calendar, Projects, SquirrelBus, Settings
// API stubs for endpoints Rocky is building in parallel (Phase 1)
// ═══════════════════════════════════════════════════════════════════════════════

// ── Stub / Mock Data ───────────────────────────────────────────────────────────

const V2_CALENDAR_EVENTS = [
  { id: 'cal-001', title: 'jkh in Taiwan', start: '2026-03-20T00:00:00Z', end: '2026-03-31T23:59:00Z', owner: 'rocky', type: 'travel', allDay: true, description: 'jkh traveling — async-first communication' },
  { id: 'cal-002', title: 'Sparky GPU block — render job', start: '2026-03-26T02:00:00Z', end: '2026-03-26T06:00:00Z', owner: 'natasha', type: 'block', resource: 'sparky-gpu', allDay: false, description: 'RTX 4090 reserved for diffusion run' },
  { id: 'cal-003', title: 'PR review deadline — usdagent #12', start: '2026-03-27T17:00:00Z', end: '2026-03-27T17:00:00Z', owner: 'bullwinkle', type: 'deadline', allDay: false, description: 'jordanhubbard/usdagent PR #12 needs review' },
  { id: 'cal-004', title: 'NVIDIA all-hands', start: '2026-03-28T16:00:00Z', end: '2026-03-28T18:00:00Z', owner: 'jkh', type: 'event', allDay: false, description: 'jkh attending NVIDIA all-hands' },
  { id: 'cal-005', title: 'Puck maintenance window', start: '2026-03-29T04:00:00Z', end: '2026-03-29T05:00:00Z', owner: 'bullwinkle', type: 'maintenance', resource: 'puck', allDay: false, description: 'macOS update + disk cleanup' },
  { id: 'cal-006', title: 'Boris L40 benchmark run', start: '2026-03-30T08:00:00Z', end: '2026-03-30T12:00:00Z', owner: 'boris', type: 'block', resource: 'l40-sweden', allDay: false, description: 'Omniverse headless rendering benchmark' },
  { id: 'cal-007', title: 'RCC weekly sync (soul commits)', start: '2026-04-01T14:00:00Z', end: '2026-04-01T14:30:00Z', owner: 'rocky', type: 'event', allDay: false, description: 'Weekly soul commit review and lessons sync' },
];

const V2_APPEAL_ITEMS = [
  { id: 'wq-appeal-001', title: 'Merge strategy for usdagent PR #12 — 3 conflicting approaches', agent: 'bullwinkle', reason: 'Need jkh to pick the architecture direction before we write more code on top', waitingSince: new Date(Date.now() - 14 * 3600 * 1000).toISOString(), priority: 'high', tags: ['usdagent', 'architecture'] },
  { id: 'wq-appeal-002', title: 'GPU budget approval — $80/mo Vast.ai for Boris overflow', agent: 'rocky', reason: 'L40 in Sweden is saturated during EU hours. Need cost approval to rent burst capacity.', waitingSince: new Date(Date.now() - 52 * 3600 * 1000).toISOString(), priority: 'normal', tags: ['budget', 'infrastructure'] },
  { id: 'wq-appeal-003', title: 'Delete old souls/ branch from 2025 experiment?', agent: 'natasha', reason: 'Branch is stale (180d), but has uncommitted experiments. Confirm before purge.', waitingSince: new Date(Date.now() - 3 * 3600 * 1000).toISOString(), priority: 'low', tags: ['git', 'cleanup'] },
];

const V2_CRON_STATUS = {
  rocky: [
    { name: 'heartbeat-rcc', schedule: '*/5 * * * *', lastSuccess: new Date(Date.now() - 3 * 60 * 1000).toISOString(), lastFailure: null, streak: 0, status: 'ok' },
    { name: 'queue-sync', schedule: '*/15 * * * *', lastSuccess: new Date(Date.now() - 11 * 60 * 1000).toISOString(), lastFailure: null, streak: 0, status: 'ok' },
    { name: 'minio-backup', schedule: '0 3 * * *', lastSuccess: new Date(Date.now() - 18 * 3600 * 1000).toISOString(), lastFailure: null, streak: 0, status: 'ok' },
  ],
  bullwinkle: [
    { name: 'heartbeat-rcc.plist', schedule: '*/5 * * * *', lastSuccess: new Date(Date.now() - 4 * 60 * 1000).toISOString(), lastFailure: new Date(Date.now() - 2 * 3600 * 1000).toISOString(), streak: 0, status: 'ok' },
    { name: 'openclaw-plist', schedule: 'launchd', lastSuccess: new Date(Date.now() - 90 * 60 * 1000).toISOString(), lastFailure: null, streak: 0, status: 'ok' },
  ],
  natasha: [
    { name: 'heartbeat-rcc', schedule: '*/5 * * * *', lastSuccess: new Date(Date.now() - 2 * 60 * 1000).toISOString(), lastFailure: null, streak: 0, status: 'ok' },
    { name: 'cuda-health-check', schedule: '*/30 * * * *', lastSuccess: new Date(Date.now() - 28 * 60 * 1000).toISOString(), lastFailure: null, streak: 0, status: 'ok' },
    { name: 'ollama-keepalive', schedule: '*/10 * * * *', lastSuccess: new Date(Date.now() - 7 * 60 * 1000).toISOString(), lastFailure: null, streak: 0, status: 'ok' },
  ],
  boris: [
    { name: 'heartbeat-rcc', schedule: '*/5 * * * *', lastSuccess: new Date(Date.now() - 23 * 60 * 1000).toISOString(), lastFailure: new Date(Date.now() - 28 * 60 * 1000).toISOString(), streak: 1, status: 'warning' },
    { name: 'omniverse-watchdog', schedule: '*/15 * * * *', lastSuccess: null, lastFailure: new Date(Date.now() - 47 * 3600 * 1000).toISOString(), streak: 47, status: 'error' },
  ],
};

const V2_PROVIDER_HEALTH = {
  rocky:      { model: 'claude-sonnet-4-6', provider: 'NVIDIA/Azure/Anthropic', status: 'ok', lastError: null, lastErrorTs: null, requestsToday: 142 },
  bullwinkle: { model: 'claude-sonnet-4-6', provider: 'OpenClaw gateway', status: 'ok', lastError: null, lastErrorTs: null, requestsToday: 89 },
  natasha:    { model: 'qwen2.5-coder:32b', provider: 'Ollama (local)', status: 'ok', lastError: null, lastErrorTs: null, requestsToday: 34 },
  boris:      { model: 'claude-sonnet-4-6', provider: 'NVIDIA Inference', status: 'degraded', lastError: 'HTTP 400 — invalid_request_error', lastErrorTs: new Date(Date.now() - 3 * 3600 * 1000).toISOString(), requestsToday: 0 },
};

const V2_MOCK_PROJECTS = [
  { id: 'rockyandfriends', full_name: 'jordanhubbard/rockyandfriends', display_name: 'rockyandfriends', description: 'The shared workspace for Rocky, Bullwinkle, Natasha', kind: 'team', activeAgent: 'rocky', lastCommit: new Date(Date.now() - 2 * 3600 * 1000).toISOString(), branch: 'main', openIssues: 3, openPRs: 1, queueItems: 2, slackChannel: '#rockyandfriends (omgjkh)', ciStatus: 'passing' },
  { id: 'usdagent', full_name: 'jordanhubbard/usdagent', display_name: 'usdagent', description: 'USD-native AI agent for 3D scene manipulation', kind: 'team', activeAgent: 'bullwinkle', lastCommit: new Date(Date.now() - 8 * 3600 * 1000).toISOString(), branch: 'main', openIssues: 7, openPRs: 2, queueItems: 5, slackChannel: '#usdagent (omgjkh)', ciStatus: 'failing' },
  { id: 'openclaw', full_name: 'jordanhubbard/openclaw', display_name: 'openclaw', description: 'Claude Code gateway + agent orchestration layer', kind: 'personal', activeAgent: 'rocky', lastCommit: new Date(Date.now() - 30 * 60 * 1000).toISOString(), branch: 'main', openIssues: 1, openPRs: 0, queueItems: 1, slackChannel: null, ciStatus: 'passing' },
  { id: 'itsallgeektome', full_name: 'jordanhubbard/itsallgeektome', display_name: 'itsallgeektome', description: 'The geek blog — posts, tools, long-form writing', kind: 'personal', activeAgent: 'natasha', lastCommit: new Date(Date.now() - 4 * 24 * 3600 * 1000).toISOString(), branch: 'main', openIssues: 0, openPRs: 0, queueItems: 0, slackChannel: '#itsallgeektome (offtera)', ciStatus: 'passing' },
];

// In-memory stores (fallback when RCC is unreachable)
const v2CalendarStore = [...V2_CALENDAR_EVENTS];
const v2CronStore     = JSON.parse(JSON.stringify(V2_CRON_STATUS));
const v2ProviderStore = JSON.parse(JSON.stringify(V2_PROVIDER_HEALTH));

// ── RCC Proxy Helper ───────────────────────────────────────────────────────────

const RCC_BASE = 'http://localhost:8789';

async function rccFetch(authHeader, path, options = {}) {
  const ctrl = new AbortController();
  const tid = setTimeout(() => ctrl.abort(), 8000);
  try {
    const resp = await fetch(`${RCC_BASE}${path}`, {
      ...options,
      headers: {
        'Content-Type': 'application/json',
        ...(authHeader ? { Authorization: authHeader } : {}),
        ...options.headers,
      },
      signal: ctrl.signal,
    });
    if (!resp.ok) throw new Error(`RCC ${resp.status}`);
    return await resp.json();
  } finally {
    clearTimeout(tid);
  }
}

// ── API Routes (proxy to RCC, fallback to stub) ────────────────────────────────

app.get('/api/calendar', async (req, res) => {
  try {
    const qs = new URLSearchParams(req.query).toString();
    const data = await rccFetch(req.headers.authorization, `/api/calendar${qs ? '?' + qs : ''}`);
    return res.json(data);
  } catch {
    let events = v2CalendarStore;
    if (req.query.start) events = events.filter(e => new Date(e.end) >= new Date(req.query.start));
    if (req.query.end)   events = events.filter(e => new Date(e.start) <= new Date(req.query.end));
    if (req.query.resource) events = events.filter(e => e.resource === req.query.resource);
    res.json({ ok: true, events });
  }
});

app.post('/api/calendar', requireAuth, async (req, res) => {
  try {
    const data = await rccFetch(req.headers.authorization, '/api/calendar', {
      method: 'POST', body: JSON.stringify(req.body),
    });
    return res.json(data);
  } catch {
    const event = { id: 'cal-' + Date.now(), ...req.body, created: new Date().toISOString() };
    v2CalendarStore.push(event);
    res.json({ ok: true, event });
  }
});

app.delete('/api/calendar/:id', requireAuth, async (req, res) => {
  try {
    const data = await rccFetch(req.headers.authorization, `/api/calendar/${req.params.id}`, { method: 'DELETE' });
    return res.json(data);
  } catch {
    const idx = v2CalendarStore.findIndex(e => e.id === req.params.id);
    if (idx === -1) return res.status(404).json({ error: 'Event not found' });
    const [removed] = v2CalendarStore.splice(idx, 1);
    res.json({ ok: true, removed });
  }
});

// ── Request tickets proxy ─────────────────────────────────────────────────────
app.get('/api/requests', async (req, res) => {
  try {
    const qs = new URLSearchParams(req.query).toString();
    const data = await rccFetch(req.headers.authorization, `/api/requests${qs ? '?' + qs : ''}`);
    return res.json(data);
  } catch (e) {
    res.json([]);
  }
});

app.post('/api/requests', requireAuth, async (req, res) => {
  try {
    const data = await rccFetch(req.headers.authorization, '/api/requests', {
      method: 'POST', body: JSON.stringify(req.body),
    });
    return res.json(data);
  } catch (e) {
    res.status(502).json({ error: e.message });
  }
});

app.get('/api/requests/:id', async (req, res) => {
  try {
    const data = await rccFetch(req.headers.authorization, `/api/requests/${req.params.id}`);
    return res.json(data);
  } catch (e) {
    res.status(502).json({ error: e.message });
  }
});

app.patch('/api/requests/:id', requireAuth, async (req, res) => {
  try {
    const data = await rccFetch(req.headers.authorization, `/api/requests/${req.params.id}`, {
      method: 'PATCH', body: JSON.stringify(req.body),
    });
    return res.json(data);
  } catch (e) {
    res.status(502).json({ error: e.message });
  }
});

app.post('/api/requests/:id/delegate', requireAuth, async (req, res) => {
  try {
    const data = await rccFetch(req.headers.authorization, `/api/requests/${req.params.id}/delegate`, {
      method: 'POST', body: JSON.stringify(req.body),
    });
    return res.json(data);
  } catch (e) {
    res.status(502).json({ error: e.message });
  }
});

app.patch('/api/requests/:id/delegations/:idx', requireAuth, async (req, res) => {
  try {
    const data = await rccFetch(req.headers.authorization, `/api/requests/${req.params.id}/delegations/${req.params.idx}`, {
      method: 'PATCH', body: JSON.stringify(req.body),
    });
    return res.json(data);
  } catch (e) {
    res.status(502).json({ error: e.message });
  }
});

app.post('/api/requests/:id/close', requireAuth, async (req, res) => {
  try {
    const data = await rccFetch(req.headers.authorization, `/api/requests/${req.params.id}/close`, {
      method: 'POST', body: JSON.stringify(req.body),
    });
    return res.json(data);
  } catch (e) {
    res.status(502).json({ error: e.message });
  }
});

app.get('/api/appeal', async (req, res) => {
  try {
    const data = await rccFetch(req.headers.authorization, '/api/appeal');
    return res.json(data);
  } catch {
    res.json({ ok: true, items: V2_APPEAL_ITEMS });
  }
});

app.post('/api/appeal/:id', requireAuth, async (req, res) => {
  const { action, comment } = req.body;
  if (!action) return res.status(400).json({ error: 'action required' });
  try {
    const data = await rccFetch(req.headers.authorization, `/api/appeal/${req.params.id}`, {
      method: 'POST', body: JSON.stringify(req.body),
    });
    return res.json(data);
  } catch {
    res.json({ ok: true, id: req.params.id, action, comment, ts: new Date().toISOString() });
  }
});

app.get('/api/heartbeat/:agent/history', async (req, res) => {
  const agent = req.params.agent;
  try {
    const data = await rccFetch(req.headers.authorization, `/api/heartbeat/${agent}/history`);
    return res.json(data);
  } catch {
    // Synthetic fallback: 288 5-min slots for the last 24h
    const now = Date.now();
    const slots = [];
    for (let i = 287; i >= 0; i--) {
      const ts = new Date(now - i * 5 * 60 * 1000).toISOString();
      let status = 'online';
      if (agent === 'boris' && i >= 84 && i <= 168) status = 'offline';
      else if (agent === 'boris' && i >= 169 && i <= 180) status = 'stale';
      else if (Math.random() < 0.015) status = 'stale';
      slots.push({ ts, status });
    }
    res.json({ ok: true, agent, slots });
  }
});

app.get('/api/crons', async (req, res) => {
  try {
    const data = await rccFetch(req.headers.authorization, '/api/cron-status');
    return res.json(data);
  } catch {
    res.json({ ok: true, crons: v2CronStore });
  }
});

app.post('/api/crons/:agent', requireAuth, async (req, res) => {
  const agent = req.params.agent;
  try {
    const data = await rccFetch(req.headers.authorization, `/api/cron-status/${agent}`, {
      method: 'POST', body: JSON.stringify(req.body),
    });
    return res.json(data);
  } catch {
    if (!v2CronStore[agent]) v2CronStore[agent] = [];
    const { name, status, lastSuccess, lastFailure, streak } = req.body;
    const existing = v2CronStore[agent].find(c => c.name === name);
    if (existing) {
      Object.assign(existing, { status, lastSuccess, lastFailure, streak, updatedAt: new Date().toISOString() });
    } else {
      v2CronStore[agent].push({ name, status, lastSuccess, lastFailure, streak: streak || 0, schedule: req.body.schedule || '?' });
    }
    res.json({ ok: true });
  }
});

app.get('/api/provider-health', async (req, res) => {
  try {
    const data = await rccFetch(req.headers.authorization, '/api/provider-health');
    return res.json(data);
  } catch {
    res.json({ ok: true, providers: v2ProviderStore });
  }
});

app.post('/api/provider-health/:agent', requireAuth, async (req, res) => {
  const agent = req.params.agent;
  try {
    const data = await rccFetch(req.headers.authorization, `/api/provider-health/${agent}`, {
      method: 'POST', body: JSON.stringify(req.body),
    });
    return res.json(data);
  } catch {
    v2ProviderStore[agent] = { ...v2ProviderStore[agent], ...req.body, updatedAt: new Date().toISOString() };
    res.json({ ok: true });
  }
});

// ── Shared v2 Rendering Helpers ────────────────────────────────────────────────

function v2Head(title, extraStyle = '') {
  return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>${title} — RCC</title>
  <style>
    * { box-sizing: border-box; margin: 0; padding: 0; }
    body { background: #0d1117; color: #c9d1d9; font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Helvetica, Arial, sans-serif; min-height: 100vh; }
    a { color: #58a6ff; text-decoration: none; }
    a:hover { text-decoration: underline; }
    .pill { display: inline-block; padding: 2px 8px; border-radius: 4px; font-size: 11px; font-weight: 600; color: #fff; }
    .pill-pending   { background: #1f6feb; }
    .pill-progress  { background: #a371f7; }
    .pill-blocked   { background: #f85149; }
    .pill-completed { background: #3fb950; }
    .pill-idea      { background: #d29922; }
    .toast { position: fixed; bottom: 20px; right: 20px; background: #238636; color: #fff; padding: 10px 18px; border-radius: 8px; font-size: 13px; display: none; z-index: 9999; box-shadow: 0 4px 12px #000a; }
    .toast.error { background: #f85149; }
    ${extraStyle}
  </style>
</head>
<body>`;
}

function v2Nav(active) {
  const tabs = [
    { id: 'overview',    label: 'Overview',    href: '/overview',    icon: '🏠' },
    { id: 'kanban',      label: 'Kanban',      href: '/kanban',      icon: '📋' },
    { id: 'calendar',    label: 'Calendar',    href: '/calendar',    icon: '📅' },
    { id: 'projects',    label: 'Projects',    href: '/projects',    icon: '📁' },
    { id: 'squirrelbus', label: 'SquirrelBus', href: '/squirrelbus', icon: '📡' },
  ];
  const tabHtml = tabs.map(t => {
    const isActive = t.id === active;
    return `<a href="${t.href}" style="display:flex;align-items:center;gap:6px;padding:12px 16px;font-size:13px;font-weight:${isActive ? '600' : '400'};color:${isActive ? '#f0f6fc' : '#8b949e'};border-bottom:2px solid ${isActive ? '#58a6ff' : 'transparent'};transition:color .15s,border-color .15s;text-decoration:none" onmouseover="if(!this.classList.contains('active'))this.style.color='#c9d1d9'" onmouseout="if(!this.classList.contains('active'))this.style.color='#8b949e'">${t.icon} ${t.label}</a>`;
  }).join('');
  const settingsActive = active === 'settings';
  return `<nav style="background:#161b22;border-bottom:1px solid #30363d;display:flex;align-items:center;position:sticky;top:0;z-index:100">
  <div style="display:flex;align-items:center;padding:0 16px;border-right:1px solid #30363d;flex-shrink:0">
    <a href="/overview" style="font-size:15px;font-weight:700;color:#f0f6fc;text-decoration:none">🐿️ RCC</a>
  </div>
  <div style="display:flex;align-items:stretch;flex:1;overflow-x:auto">${tabHtml}</div>
  <div style="padding:0 12px;flex-shrink:0;border-left:1px solid #30363d">
    <a href="/settings" style="display:flex;align-items:center;padding:12px 8px;font-size:16px;color:${settingsActive ? '#f0f6fc' : '#8b949e'};text-decoration:none;border-bottom:2px solid ${settingsActive ? '#58a6ff' : 'transparent'}" title="Settings">⚙️</a>
  </div>
  <div style="padding:0 12px;flex-shrink:0;border-left:1px solid #30363d">
    <a href="/" style="font-size:11px;color:#484f58;padding:12px 4px;display:flex;align-items:center;text-decoration:none" title="Legacy dashboard">v1</a>
  </div>
</nav>`;
}

function v2Foot(js = '') {
  return `<div class="toast" id="toast"></div>
<script>
function getToken(){let t=localStorage.getItem('wq-token');if(!t){t=prompt('Enter auth token:');if(t){t=t.trim();localStorage.setItem('wq-token',t);}}return t?t.trim():t;}
function showToast(msg,isErr){const el=document.getElementById('toast');el.textContent=msg;el.className='toast'+(isErr?' error':'');el.style.display='block';setTimeout(()=>el.style.display='none',3500);}
function esc(s){if(!s)return'';const d=document.createElement('div');d.textContent=s;return d.innerHTML;}
function timeAgo(ds){if(!ds)return'never';const diff=Date.now()-new Date(ds).getTime();const m=Math.floor(diff/60000);if(m<1)return'just now';if(m<60)return m+'m ago';const h=Math.floor(m/60);if(h<24)return h+'h ago';return Math.floor(h/24)+'d ago';}
async function authedFetch(url,opts={}){const token=getToken();if(!token)return null;opts.headers=Object.assign({},opts.headers,{'Authorization':'Bearer '+token});const r=await fetch(url,opts);if(r.status===401){showToast('⚠️ Unauthorized — update token',true);return null;}return r;}
const EMOJIS={rocky:'🐿️',bullwinkle:'🫎',natasha:'🕵️',boris:'🕵️',jkh:'👤'};
${js}
</script>
</body>
</html>`;
}

// ── Overview Page ─────────────────────────────────────────────────────────────

app.get('/overview', (req, res) => {
  res.type('html').send(v2Head('Overview', `
    .container { max-width: 1400px; margin: 0 auto; padding: 20px; }
    .grid-2 { display: grid; grid-template-columns: 1fr 1fr; gap: 16px; }
    .grid-3 { display: grid; grid-template-columns: 1fr 1fr 1fr; gap: 16px; }
    .panel { background: #161b22; border: 1px solid #30363d; border-radius: 8px; padding: 16px; }
    .panel-title { font-size: 13px; font-weight: 600; color: #8b949e; text-transform: uppercase; letter-spacing: .04em; margin-bottom: 12px; display: flex; align-items: center; gap: 6px; }
    .agent-strip { display: flex; gap: 10px; flex-wrap: wrap; }
    .agent-hb-card { background: #0d1117; border: 1px solid #30363d; border-radius: 8px; padding: 12px 14px; min-width: 200px; flex: 1; cursor: pointer; transition: border-color .15s; }
    .agent-hb-card:hover { border-color: #58a6ff; }
    .sparkline { display: flex; gap: 1px; align-items: flex-end; height: 20px; margin-top: 6px; }
    .sparkline-dot { width: 3px; border-radius: 1px; flex-shrink: 0; }
    .appeal-card { background: #0d1117; border: 1px solid #30363d; border-radius: 6px; padding: 10px 12px; margin-bottom: 8px; transition: border-color .15s; }
    .appeal-card:hover { border-color: #58a6ff; }
    .cron-row { display: flex; align-items: center; gap: 8px; padding: 5px 0; border-bottom: 1px solid #21262d; font-size: 12px; }
    .cron-row:last-child { border-bottom: none; }
    .status-dot { width: 8px; height: 8px; border-radius: 50%; flex-shrink: 0; }
    .provider-row { display: flex; align-items: center; justify-content: space-between; padding: 7px 0; border-bottom: 1px solid #21262d; font-size: 12px; }
    .provider-row:last-child { border-bottom: none; }
    .digest-event { font-size: 12px; color: #8b949e; padding: 4px 0; border-bottom: 1px solid #1a1f26; }
    .digest-event:last-child { border-bottom: none; }
  `) + v2Nav('overview') + `
  <div class="container">

    <!-- Agent Status Strip -->
    <div class="panel" style="margin-bottom:16px">
      <div class="panel-title">🟢 Agent Status</div>
      <div class="agent-strip" id="agent-strip">
        <div style="color:#8b949e;font-size:13px">Loading…</div>
      </div>
    </div>

    <div class="grid-2" style="margin-bottom:16px">
      <!-- jkh Appeal Queue -->
      <div class="panel">
        <div class="panel-title">⏳ jkh Appeal Queue <span id="appeal-count" style="background:#f0883e;color:#fff;border-radius:10px;padding:1px 7px;font-size:10px;font-weight:700"></span></div>
        <div id="appeal-list"><div style="color:#8b949e;font-size:13px">Loading…</div></div>
      </div>

      <!-- Session Digest -->
      <div class="panel">
        <div class="panel-title">📊 Session Digest</div>
        <div id="session-digest" style="font-size:12px;color:#8b949e">Loading…</div>
      </div>
    </div>

    <div class="grid-3" style="margin-bottom:16px">
      <!-- Heartbeat Sparklines -->
      <div class="panel">
        <div class="panel-title">💓 Heartbeat (24h)</div>
        <div id="sparklines"><div style="color:#8b949e;font-size:13px">Loading…</div></div>
      </div>

      <!-- Cron Status -->
      <div class="panel">
        <div class="panel-title">⏰ Cron Jobs</div>
        <div id="cron-panel"><div style="color:#8b949e;font-size:13px">Loading…</div></div>
      </div>

      <!-- Provider Health -->
      <div class="panel">
        <div class="panel-title">🤖 Provider Health</div>
        <div id="provider-panel"><div style="color:#8b949e;font-size:13px">Loading…</div></div>
      </div>
    </div>

  </div>
` + v2Foot(`
async function loadOverview() {
  const [hbs, appeals, cronData, provData, digest] = await Promise.all([
    fetch('/api/heartbeats').then(r=>r.json()).catch(()=>({})),
    fetch('/api/appeal').then(r=>r.json()).catch(()=>({items:[]})),
    fetch('/api/crons').then(r=>r.json()).catch(()=>({crons:{}})),
    fetch('/api/provider-health').then(r=>r.json()).catch(()=>({providers:{}})),
    fetch('/api/digest').then(r=>r.json()).catch(()=>({digest:'(unavailable)'})),
  ]);

  // Agent status strip
  const strip = document.getElementById('agent-strip');
  const agents = ['rocky','bullwinkle','natasha','boris'];
  strip.innerHTML = agents.map(name => {
    const hb = hbs[name] || {};
    const age = hb.ts ? Date.now() - new Date(hb.ts).getTime() : Infinity;
    let dot = '🔴', cls = '#f85149', label = 'offline';
    if (age < 5*60*1000)  { dot='🟢'; cls='#3fb950'; label='online'; }
    else if (age < 30*60*1000) { dot='🟡'; cls='#d29922'; label='stale'; }
    const emoji = EMOJIS[name]||'🤖';
    const activity = hb.activity || 'idle';
    return '<div class="agent-hb-card" style="border-color:'+cls+'33">' +
      '<div style="display:flex;align-items:center;justify-content:space-between;margin-bottom:4px">' +
        '<span style="font-weight:700;font-size:14px">'+emoji+' '+name.charAt(0).toUpperCase()+name.slice(1)+'</span>' +
        '<span style="color:'+cls+';font-size:11px;font-weight:600">'+dot+' '+label+'</span>' +
      '</div>' +
      '<div style="font-size:11px;color:#8b949e">'+esc(hb.host||'—')+'</div>' +
      '<div style="font-size:11px;color:#c9d1d9;margin-top:2px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis">'+esc(activity)+'</div>' +
      '<div style="font-size:10px;color:#484f58;margin-top:2px">'+timeAgo(hb.ts)+'</div>' +
    '</div>';
  }).join('');

  // Appeal queue
  const items = appeals.items || [];
  const aCount = document.getElementById('appeal-count');
  if (aCount) aCount.textContent = items.length;
  const appealList = document.getElementById('appeal-list');
  if (items.length === 0) {
    appealList.innerHTML = '<div style="color:#3fb950;font-size:13px;padding:8px 0">✅ Nothing awaiting your judgment</div>';
  } else {
    appealList.innerHTML = items.map(item => {
      const waitMs = Date.now() - new Date(item.waitingSince).getTime();
      const waitH = waitMs / 3600000;
      let bg = 'transparent', border = '#30363d';
      if (waitH > 72) { bg='#f8514920'; border='#f85149'; }
      else if (waitH > 24) { bg='#d2992220'; border='#d29922'; }
      else if (waitH > 2) { bg='#f0883e10'; border='#f0883e44'; }
      const waitBadge = waitH > 72 ? '⚠️ ' : '';
      return '<div class="appeal-card" style="background:'+bg+';border-color:'+border+'">' +
        '<div style="font-weight:600;font-size:13px;margin-bottom:3px">'+waitBadge+esc(item.title)+'</div>' +
        '<div style="font-size:11px;color:#8b949e;margin-bottom:6px">'+esc(EMOJIS[item.agent]||'🤖')+' '+esc(item.agent)+' · '+timeAgo(item.waitingSince)+' waiting</div>' +
        '<div style="font-size:11px;color:#c9d1d9;margin-bottom:8px">'+esc(item.reason)+'</div>' +
        '<div style="display:flex;gap:6px">' +
          '<button onclick="appealAction(\''+item.id+'\',\'approve\')" style="background:#238636;border:none;color:#fff;border-radius:4px;padding:3px 10px;font-size:11px;cursor:pointer">✅ Approve</button>' +
          '<button onclick="appealAction(\''+item.id+'\',\'reject\')" style="background:#6e7681;border:none;color:#fff;border-radius:4px;padding:3px 10px;font-size:11px;cursor:pointer">❌ Reject</button>' +
          '<button onclick="appealAction(\''+item.id+'\',\'comment\')" style="background:#1f6feb;border:none;color:#fff;border-radius:4px;padding:3px 10px;font-size:11px;cursor:pointer">💬 Comment</button>' +
        '</div>' +
      '</div>';
    }).join('');
  }

  // Session digest
  const sd = document.getElementById('session-digest');
  if (digest.digest) {
    const lines = digest.digest.split('\\n').slice(0,12);
    sd.innerHTML = '<pre style="white-space:pre-wrap;font-size:11px;line-height:1.7;color:#8b949e">'+esc(lines.join('\\n'))+'</pre>';
  }

  // Sparklines — load for each agent
  const sparkDiv = document.getElementById('sparklines');
  const sparkResults = await Promise.all(['rocky','bullwinkle','natasha','boris'].map(a=>
    fetch('/api/heartbeat/'+a+'/history').then(r=>r.json()).catch(()=>({slots:[]}))
  ));
  sparkDiv.innerHTML = ['rocky','bullwinkle','natasha','boris'].map((name,i) => {
    const slots = sparkResults[i].slots || [];
    const recent = slots.slice(-96); // last 8h at 5min intervals
    const dots = recent.map(s => {
      const c = s.status==='online'?'#3fb950':s.status==='stale'?'#d29922':'#f85149';
      return '<div class="sparkline-dot" style="background:'+c+';height:'+(s.status==='online'?14:s.status==='stale'?8:4)+'px" title="'+s.status+' at '+new Date(s.ts).toLocaleTimeString()+'"></div>';
    }).join('');
    return '<div style="margin-bottom:10px">' +
      '<div style="font-size:12px;font-weight:600;margin-bottom:4px">'+EMOJIS[name]+' '+name+'</div>' +
      '<div class="sparkline">'+dots+'</div>' +
    '</div>';
  }).join('');

  // Cron status
  const crons = cronData.crons || {};
  const cronDiv = document.getElementById('cron-panel');
  let cronHtml = '';
  for (const [agentName, jobs] of Object.entries(crons)) {
    if (!jobs || jobs.length === 0) continue;
    cronHtml += '<div style="font-size:11px;font-weight:600;color:#8b949e;margin:8px 0 4px 0">'+EMOJIS[agentName]+' '+agentName+'</div>';
    cronHtml += jobs.map(job => {
      const dot = job.status==='ok'?'#3fb950':job.status==='warning'?'#d29922':'#f85149';
      const streak = job.streak > 0 ? '<span style="color:#f85149;margin-left:4px">⚠️ '+job.streak+' fails</span>' : '';
      return '<div class="cron-row">' +
        '<div class="status-dot" style="background:'+dot+'"></div>' +
        '<div style="flex:1;min-width:0;white-space:nowrap;overflow:hidden;text-overflow:ellipsis">'+esc(job.name)+'</div>' +
        streak +
        '<div style="color:#484f58;flex-shrink:0">'+timeAgo(job.lastSuccess)+'</div>' +
      '</div>';
    }).join('');
  }
  cronDiv.innerHTML = cronHtml || '<div style="color:#8b949e">No cron data</div>';

  // Provider health
  const providers = provData.providers || {};
  const pDiv = document.getElementById('provider-panel');
  pDiv.innerHTML = Object.entries(providers).map(([name, p]) => {
    const dot = p.status==='ok'?'#3fb950':p.status==='degraded'?'#d29922':'#f85149';
    const errLine = p.lastError ? '<div style="font-size:10px;color:#f85149;margin-top:2px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis">'+esc(p.lastError)+'</div>' : '';
    return '<div class="provider-row">' +
      '<div>' +
        '<div style="display:flex;align-items:center;gap:6px">' +
          '<div class="status-dot" style="background:'+dot+'"></div>' +
          '<span style="font-weight:600">'+EMOJIS[name]+' '+name+'</span>' +
        '</div>' +
        '<div style="font-size:10px;color:#8b949e;margin-left:14px">'+esc(p.model)+'</div>' +
        errLine +
      '</div>' +
      '<div style="text-align:right;flex-shrink:0;margin-left:8px">' +
        '<div style="font-size:10px;color:#484f58">'+p.requestsToday+' req</div>' +
      '</div>' +
    '</div>';
  }).join('');
}

async function appealAction(id, action) {
  const r = await authedFetch('/api/appeal/'+id, {method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({action})});
  if (r) { showToast('Action sent: '+action); loadOverview(); }
}

loadOverview();
setInterval(loadOverview, 30000);
`));
});

// ── Kanban Page ───────────────────────────────────────────────────────────────

app.get('/kanban', (req, res) => {
  res.type('html').send(v2Head('Kanban', `
    .board { display: flex; gap: 12px; padding: 16px; overflow-x: auto; min-height: calc(100vh - 53px); align-items: flex-start; }
    .column { background: #161b22; border: 1px solid #30363d; border-radius: 8px; min-width: 240px; max-width: 280px; flex-shrink: 0; display: flex; flex-direction: column; }
    .col-header { padding: 12px 14px; border-bottom: 1px solid #30363d; display: flex; align-items: center; justify-content: space-between; }
    .col-title { font-size: 13px; font-weight: 700; color: #f0f6fc; display: flex; align-items: center; gap: 6px; }
    .col-count { font-size: 11px; background: #21262d; border-radius: 10px; padding: 1px 7px; color: #8b949e; }
    .col-body { padding: 10px; display: flex; flex-direction: column; gap: 8px; min-height: 100px; }
    .kcard { background: #0d1117; border: 1px solid #30363d; border-radius: 6px; padding: 10px 12px; cursor: grab; transition: border-color .15s, box-shadow .15s; user-select: none; }
    .kcard:hover { border-color: #58a6ff; box-shadow: 0 2px 8px #0005; }
    .kcard.dragging { opacity: .5; cursor: grabbing; }
    .kcard-type-bug      { border-left: 3px solid #f85149 !important; }
    .kcard-type-feature  { border-left: 3px solid #3fb950 !important; }
    .kcard-type-idea     { border-left: 3px solid #d29922 !important; }
    .kcard-type-proposal { border-left: 3px solid #a371f7 !important; }
    .kcard-type-task     { border-left: 3px solid #30363d !important; }
    .type-chip { display: inline-block; padding: 1px 6px; border-radius: 3px; font-size: 10px; font-weight: 700; color: #fff; margin-right: 4px; }
    .drop-zone { border: 2px dashed #30363d; border-radius: 6px; min-height: 40px; background: transparent; transition: border-color .15s, background .15s; }
    .drop-zone.drag-over { border-color: #58a6ff; background: #58a6ff10; }
    .filter-bar { display: flex; gap: 6px; padding: 10px 16px; border-bottom: 1px solid #21262d; flex-wrap: wrap; align-items: center; background: #0d1117; }
    .fbtn { background: #21262d; color: #c9d1d9; border: 1px solid #30363d; padding: 4px 12px; border-radius: 16px; cursor: pointer; font-size: 12px; }
    .fbtn.active { background: #1f6feb; border-color: #1f6feb; color: #fff; }
    .appeal-mini { background: #f0883e15; border-color: #f0883e44; }
  `) + v2Nav('kanban') + `
  <div class="filter-bar">
    <span style="font-size:12px;color:#8b949e;margin-right:4px">Type:</span>
    <button class="fbtn active" id="f-all"     onclick="setTypeFilter('all')">All</button>
    <button class="fbtn"       id="f-bug"     onclick="setTypeFilter('bug')">🔴 Bugs</button>
    <button class="fbtn"       id="f-feature" onclick="setTypeFilter('feature')">🟢 Features</button>
    <button class="fbtn"       id="f-idea"    onclick="setTypeFilter('idea')">💡 Ideas</button>
    <button class="fbtn"       id="f-proposal"onclick="setTypeFilter('proposal')">🟣 Proposals</button>
    <span style="margin-left:12px;font-size:12px;color:#8b949e">Priority:</span>
    <button class="fbtn active" id="p-all"    onclick="setPrioFilter('all')">All</button>
    <button class="fbtn"        id="p-urgent" onclick="setPrioFilter('urgent')">Urgent</button>
    <button class="fbtn"        id="p-high"   onclick="setPrioFilter('high')">High</button>
    <label style="font-size:12px;color:#8b949e;margin-left:12px;display:flex;align-items:center;gap:5px;cursor:pointer">
      <input type="checkbox" id="show-completed" onchange="renderBoard()"> Show completed
    </label>
    <span style="margin-left:auto;font-size:11px;color:#484f58" id="kb-status">Loading…</span>
  </div>
  <div class="board" id="board">
    <div style="color:#8b949e;padding:20px">Loading…</div>
  </div>
` + v2Foot(`
const AGENTS = ['rocky','bullwinkle','natasha','boris'];
let kItems = [], kAppeals = [], kHeartbeats = {}, kTypeFilter = 'all', kPrioFilter = 'all';

const TYPE_COLORS = { bug:'#f85149', feature:'#3fb950', idea:'#d29922', proposal:'#a371f7', task:'#8b949e' };
const TYPE_EMOJIS = { bug:'🔴', feature:'🟢', idea:'💡', proposal:'🟣', task:'⬜' };

function itemType(item) {
  const tags = item.tags || [];
  for (const t of ['bug','feature','idea','proposal']) if (tags.includes(t)) return t;
  if (item.type) return item.type;
  if (item.priority === 'idea') return 'idea';
  return 'task';
}

function setTypeFilter(f) {
  kTypeFilter = f;
  document.querySelectorAll('[id^="f-"]').forEach(b => b.classList.remove('active'));
  document.getElementById('f-'+f)?.classList.add('active');
  renderBoard();
}
function setPrioFilter(f) {
  kPrioFilter = f;
  document.querySelectorAll('[id^="p-"]').forEach(b => b.classList.remove('active'));
  document.getElementById('p-'+f)?.classList.add('active');
  renderBoard();
}

function filterItems(items) {
  const showCompleted = document.getElementById('show-completed')?.checked;
  return items.filter(item => {
    if (!showCompleted && (item.status === 'completed' || item.status === 'cancelled')) return false;
    if (kTypeFilter !== 'all' && itemType(item) !== kTypeFilter) return false;
    if (kPrioFilter !== 'all' && item.priority !== kPrioFilter) return false;
    return true;
  });
}

function renderCard(item) {
  const type = itemType(item);
  const typeColor = TYPE_COLORS[type] || '#8b949e';
  const typeEmoji = TYPE_EMOJIS[type] || '⬜';
  const blocked = (item.blockedBy && item.blockedBy.length) || item.status === 'blocked';
  const needsHuman = item.needsHuman || item.status === 'awaiting-jkh';
  const statusColor = item.status==='in-progress'?'#a371f7':item.status==='blocked'?'#f85149':item.status==='completed'?'#3fb950':'#58a6ff';
  return '<div class="kcard kcard-type-'+type+'" draggable="true" data-id="'+item.id+'" data-assignee="'+(item.assignee||'unassigned')+'" ondragstart="onDragStart(event,\''+item.id+'\')" ondblclick="openItemModal(\''+item.id+'\')">' +
    '<div style="display:flex;align-items:flex-start;justify-content:space-between;gap:6px;margin-bottom:6px">' +
      '<div style="font-size:12px;font-weight:600;line-height:1.35;flex:1">'+esc(item.title)+'</div>' +
      '<span style="font-size:10px;color:#484f58;flex-shrink:0">'+esc(item.id.slice(-6))+'</span>' +
    '</div>' +
    '<div style="display:flex;gap:4px;flex-wrap:wrap;align-items:center">' +
      '<span class="type-chip" style="background:'+typeColor+'">'+typeEmoji+' '+type+'</span>' +
      '<span style="font-size:10px;padding:1px 5px;background:#21262d;border-radius:3px;color:'+statusColor+'">'+item.status+'</span>' +
      (blocked ? '<span style="font-size:10px" title="Blocking/blocked">🔗</span>' : '') +
      (needsHuman ? '<span style="font-size:10px;color:#f0883e" title="Needs jkh">⏳</span>' : '') +
    '</div>' +
    (item.description ? '<div style="font-size:11px;color:#8b949e;margin-top:5px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis">'+esc(item.description.slice(0,80))+'</div>' : '') +
    '<div style="margin-top:6px;font-size:10px;color:#484f58">'+timeAgo(item.created)+'</div>' +
  '</div>';
}

function renderBoard() {
  const board = document.getElementById('board');
  const filtered = filterItems(kItems);
  const columns = [
    ...AGENTS.map(a => ({ id:a, label: a.charAt(0).toUpperCase()+a.slice(1), emoji: EMOJIS[a]||'🤖', items: filtered.filter(i=>i.assignee===a) })),
    { id:'unassigned', label:'Unassigned', emoji:'📥', items: filtered.filter(i=>!i.assignee||i.assignee==='all'||!AGENTS.includes(i.assignee)) },
  ];
  // Only show Boris if he has items (keep board clean)
  const visibleCols = columns.filter(c => c.id !== 'boris' || c.items.length > 0);

  const appealFiltered = filterItems(kAppeals);

  board.innerHTML = visibleCols.map(col => {
    const hb = kHeartbeats[col.id];
    const hbAge = hb && hb.ts ? (Date.now() - new Date(hb.ts).getTime()) / 1000 : Infinity;
    const hbColor = !hb ? '#484f58' : hbAge < 300 ? '#3fb950' : hbAge < 1800 ? '#d29922' : '#f85149';
    return '<div class="column" id="col-'+col.id+'" ondragover="onDragOver(event)" ondrop="onDrop(event,\''+col.id+'\')" ondragleave="onDragLeave(event)">' +
      '<div class="col-header">' +
        '<div class="col-title">'+col.emoji+' '+col.label+'</div>' +
        '<span class="col-count">'+col.items.length+'</span>' +
      '</div>' +
      '<div class="col-body">'+
        (col.items.length ? col.items.map(renderCard).join('') : '<div style="color:#484f58;font-size:12px;padding:8px 0;text-align:center">Empty</div>') +
        '<div class="drop-zone" data-col="'+col.id+'"></div>' +
      '</div>' +
    '</div>';
  }).join('') +
  // Appeal mini-column
  '<div class="column appeal-mini">' +
    '<div class="col-header">' +
      '<div class="col-title">⏳ Appeals</div>' +
      '<span class="col-count" style="background:#f0883e22;color:#f0883e">'+appealFiltered.length+'</span>' +
    '</div>' +
    '<div class="col-body">' +
      (appealFiltered.length ? appealFiltered.map(item =>
        '<div class="kcard" style="border-color:#f0883e44">' +
          '<div style="font-size:11px;font-weight:600">'+esc(item.title)+'</div>' +
          '<div style="font-size:10px;color:#f0883e;margin-top:4px">'+timeAgo(item.waitingSince)+' waiting</div>' +
        '</div>'
      ).join('') : '<div style="color:#484f58;font-size:12px;padding:8px 0;text-align:center">None pending</div>') +
    '</div>' +
  '</div>';

  document.getElementById('kb-status').textContent = filtered.length + ' items · ' + new Date().toLocaleTimeString();
}

// Drag-to-reassign
let dragId = null;
function onDragStart(e, id) { dragId=id; e.currentTarget.classList.add('dragging'); }
function onDragOver(e) { e.preventDefault(); e.currentTarget.classList.add('drag-over'); }
function onDragLeave(e) { e.currentTarget.classList.remove('drag-over'); }
async function onDrop(e, targetAgent) {
  e.preventDefault();
  e.currentTarget.classList.remove('drag-over');
  if (!dragId) return;
  const item = kItems.find(i => i.id === dragId);
  if (!item || item.assignee === targetAgent) { dragId=null; return; }
  const prev = item.assignee;
  item.assignee = targetAgent === 'unassigned' ? 'all' : targetAgent;
  renderBoard();
  const r = await authedFetch('/api/item/'+dragId, {method:'PATCH',headers:{'Content-Type':'application/json'},body:JSON.stringify({assignee:item.assignee})});
  if (r && r.ok) {
    showToast('Reassigned to '+targetAgent);
    // Undo toast
    setTimeout(()=>{
      const undo = confirm('Undo reassignment of "'+item.title+'" to '+targetAgent+'?');
      if (undo) { item.assignee=prev; authedFetch('/api/item/'+dragId,{method:'PATCH',headers:{'Content-Type':'application/json'},body:JSON.stringify({assignee:prev})}); renderBoard(); }
    }, 100);
  } else {
    item.assignee = prev; renderBoard();
    showToast('Reassign failed', true);
  }
  dragId = null;
}
function openItemModal(id) { window.open('/api/item/'+id,'_blank'); }

async function loadKanban() {
  const [qData, appealData, hbData] = await Promise.all([
    fetch('/api/queue').then(r=>r.json()).catch(()=>({items:[]})),
    fetch('/api/appeal').then(r=>r.json()).catch(()=>({items:[]})),
    fetch('/api/heartbeats').then(r=>r.json()).catch(()=>({})),
  ]);
  kItems = qData.items || [];
  kAppeals = (appealData.items || []).map(a => ({...a, created: a.waitingSince}));
  kHeartbeats = hbData && typeof hbData === 'object' && !Array.isArray(hbData) ? hbData : {};
  renderBoard();
}

loadKanban();
setInterval(loadKanban, 60000);
`));
});

// ── Calendar Page ─────────────────────────────────────────────────────────────

app.get('/calendar', (req, res) => {
  res.type('html').send(v2Head('Calendar', `
    .cal-container { max-width: 1200px; margin: 0 auto; padding: 20px; }
    .cal-header { display: flex; align-items: center; gap: 12px; margin-bottom: 16px; }
    .cal-nav-btn { background: #21262d; border: 1px solid #30363d; color: #c9d1d9; padding: 5px 12px; border-radius: 6px; cursor: pointer; font-size: 13px; }
    .cal-nav-btn:hover { border-color: #58a6ff; }
    .toggle-btn { background: #21262d; border: 1px solid #30363d; color: #8b949e; padding: 5px 10px; cursor: pointer; font-size: 12px; }
    .toggle-btn:first-child { border-radius: 6px 0 0 6px; }
    .toggle-btn:last-child { border-radius: 0 6px 6px 0; }
    .toggle-btn.active { background: #1f6feb; border-color: #1f6feb; color: #fff; }
    .cal-grid { display: grid; grid-template-columns: repeat(7, 1fr); gap: 2px; }
    .cal-day-header { text-align: center; padding: 6px; font-size: 11px; color: #8b949e; font-weight: 600; }
    .cal-cell { background: #161b22; border: 1px solid #21262d; border-radius: 4px; min-height: 90px; padding: 6px; position: relative; cursor: pointer; transition: border-color .15s; }
    .cal-cell:hover { border-color: #30363d; background: #1c2128; }
    .cal-cell.today { border-color: #58a6ff55; }
    .cal-cell.other-month { background: #0d1117; opacity: .6; }
    .cal-day-num { font-size: 12px; color: #8b949e; margin-bottom: 4px; }
    .cal-day-num.today-num { color: #58a6ff; font-weight: 700; }
    .cal-event { font-size: 10px; padding: 2px 5px; border-radius: 3px; margin-bottom: 2px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; color: #fff; }
    .legend-dot { width: 10px; height: 10px; border-radius: 50%; display: inline-block; }
    .event-panel { background: #161b22; border: 1px solid #30363d; border-radius: 8px; padding: 16px; margin-top: 16px; }
  `) + v2Nav('calendar') + `
  <div class="cal-container">
    <div class="cal-header">
      <button class="cal-nav-btn" onclick="navMonth(-1)">‹</button>
      <h2 id="cal-title" style="font-size:18px;font-weight:700;color:#f0f6fc;min-width:180px;text-align:center"></h2>
      <button class="cal-nav-btn" onclick="navMonth(1)">›</button>
      <button class="cal-nav-btn" onclick="goToday()">Today</button>
      <div style="margin-left:auto;display:flex">
        <button class="toggle-btn active" id="view-month" onclick="setView('month')">Monthly</button>
        <button class="toggle-btn"       id="view-week"  onclick="setView('week')">Weekly</button>
      </div>
      <button onclick="openNewEvent()" style="background:#238636;border:none;color:#fff;border-radius:6px;padding:5px 14px;font-size:13px;cursor:pointer;font-weight:600">＋ Event</button>
    </div>

    <!-- Legend -->
    <div style="display:flex;gap:14px;margin-bottom:12px;flex-wrap:wrap">
      <span style="font-size:11px;color:#8b949e">Owner:</span>
      <span style="font-size:11px;display:flex;align-items:center;gap:4px"><span class="legend-dot" style="background:#58a6ff"></span>Rocky</span>
      <span style="font-size:11px;display:flex;align-items:center;gap:4px"><span class="legend-dot" style="background:#3fb950"></span>Bullwinkle</span>
      <span style="font-size:11px;display:flex;align-items:center;gap:4px"><span class="legend-dot" style="background:#a371f7"></span>Natasha</span>
      <span style="font-size:11px;display:flex;align-items:center;gap:4px"><span class="legend-dot" style="background:#f0883e"></span>Boris</span>
      <span style="font-size:11px;display:flex;align-items:center;gap:4px"><span class="legend-dot" style="background:#d29922"></span>jkh</span>
      <span style="margin-left:12px;font-size:11px;color:#8b949e">Type:</span>
      <span style="font-size:11px">✈️ travel</span><span style="font-size:11px">🔒 block</span>
      <span style="font-size:11px">📅 deadline</span><span style="font-size:11px">🔧 maintenance</span>
    </div>

    <div id="cal-view"></div>

    <!-- Upcoming events list -->
    <div class="event-panel">
      <div style="font-size:13px;font-weight:600;color:#8b949e;text-transform:uppercase;letter-spacing:.04em;margin-bottom:10px">Upcoming Events</div>
      <div id="event-list"><div style="color:#8b949e">Loading…</div></div>
    </div>
  </div>

  <!-- New Event Modal -->
  <div id="event-modal" style="display:none;position:fixed;inset:0;background:#00000088;z-index:200;align-items:center;justify-content:center" onclick="if(event.target===this)closeEventModal()">
    <div style="background:#161b22;border:1px solid #30363d;border-radius:10px;padding:1.5rem;width:100%;max-width:460px;margin:1rem">
      <div style="display:flex;align-items:center;justify-content:space-between;margin-bottom:1rem">
        <span style="font-size:1rem;font-weight:600;color:#f0f6fc">＋ New Calendar Event</span>
        <button onclick="closeEventModal()" style="background:none;border:none;color:#6e7681;font-size:1.2rem;cursor:pointer">✕</button>
      </div>
      <div style="display:flex;flex-direction:column;gap:10px">
        <div><label style="font-size:11px;color:#8b949e;display:block;margin-bottom:3px">Title *</label><input id="ev-title" style="width:100%;background:#0d1117;border:1px solid #30363d;border-radius:5px;padding:7px 10px;color:#c9d1d9;font-size:13px"></div>
        <div style="display:grid;grid-template-columns:1fr 1fr;gap:8px">
          <div><label style="font-size:11px;color:#8b949e;display:block;margin-bottom:3px">Start</label><input type="datetime-local" id="ev-start" style="width:100%;background:#0d1117;border:1px solid #30363d;border-radius:5px;padding:7px 10px;color:#c9d1d9;font-size:12px"></div>
          <div><label style="font-size:11px;color:#8b949e;display:block;margin-bottom:3px">End</label><input type="datetime-local" id="ev-end" style="width:100%;background:#0d1117;border:1px solid #30363d;border-radius:5px;padding:7px 10px;color:#c9d1d9;font-size:12px"></div>
        </div>
        <div style="display:grid;grid-template-columns:1fr 1fr;gap:8px">
          <div><label style="font-size:11px;color:#8b949e;display:block;margin-bottom:3px">Owner</label><select id="ev-owner" style="width:100%;background:#0d1117;border:1px solid #30363d;border-radius:5px;padding:7px;color:#c9d1d9;font-size:13px"><option value="rocky">Rocky</option><option value="bullwinkle">Bullwinkle</option><option value="natasha">Natasha</option><option value="boris">Boris</option><option value="jkh">jkh</option></select></div>
          <div><label style="font-size:11px;color:#8b949e;display:block;margin-bottom:3px">Type</label><select id="ev-type" style="width:100%;background:#0d1117;border:1px solid #30363d;border-radius:5px;padding:7px;color:#c9d1d9;font-size:13px"><option value="event">event</option><option value="block">block</option><option value="deadline">deadline</option><option value="travel">travel</option><option value="maintenance">maintenance</option></select></div>
        </div>
        <div><label style="font-size:11px;color:#8b949e;display:block;margin-bottom:3px">Description</label><textarea id="ev-desc" rows="2" style="width:100%;background:#0d1117;border:1px solid #30363d;border-radius:5px;padding:7px 10px;color:#c9d1d9;font-size:13px;resize:vertical"></textarea></div>
        <div style="display:flex;gap:8px;justify-content:flex-end;margin-top:4px">
          <button onclick="closeEventModal()" style="background:#21262d;border:1px solid #30363d;color:#c9d1d9;border-radius:6px;padding:6px 16px;font-size:13px;cursor:pointer">Cancel</button>
          <button onclick="submitEvent()" style="background:#238636;border:none;color:#fff;border-radius:6px;padding:6px 18px;font-size:13px;font-weight:600;cursor:pointer">Create</button>
        </div>
      </div>
    </div>
  </div>
` + v2Foot(`
const OWNER_COLORS = { rocky:'#58a6ff', bullwinkle:'#3fb950', natasha:'#a371f7', boris:'#f0883e', jkh:'#d29922' };
const TYPE_ICONS = { travel:'✈️', block:'🔒', deadline:'📅', maintenance:'🔧', event:'📆' };
let calEvents = [], calView = 'month', calDate = new Date();

function navMonth(d) { calDate.setMonth(calDate.getMonth()+d); render(); }
function goToday() { calDate = new Date(); render(); }
function setView(v) {
  calView = v;
  document.getElementById('view-month').classList.toggle('active', v==='month');
  document.getElementById('view-week').classList.toggle('active',  v==='week');
  render();
}

function eventsForDay(y, m, d) {
  const dayStr = y+'-'+String(m+1).padStart(2,'0')+'-'+String(d).padStart(2,'0');
  return calEvents.filter(e => {
    const start = e.start.slice(0,10);
    const end   = e.end.slice(0,10);
    return dayStr >= start && dayStr <= end;
  });
}

function renderMonthly() {
  const y = calDate.getFullYear(), m = calDate.getMonth();
  document.getElementById('cal-title').textContent = calDate.toLocaleString('en-US',{month:'long',year:'numeric'});
  const first = new Date(y,m,1).getDay();
  const days  = new Date(y,m+1,0).getDate();
  const today = new Date();
  let html = '<div class="cal-grid">';
  ['Sun','Mon','Tue','Wed','Thu','Fri','Sat'].forEach(d => { html += '<div class="cal-day-header">'+d+'</div>'; });
  for (let i=0;i<first;i++) {
    const prevDay = new Date(y,m,0-first+i+2);
    html += '<div class="cal-cell other-month"><div class="cal-day-num">'+prevDay.getDate()+'</div></div>';
  }
  for (let d=1;d<=days;d++) {
    const isToday = today.getFullYear()===y && today.getMonth()===m && today.getDate()===d;
    const evs = eventsForDay(y,m,d);
    html += '<div class="cal-cell'+(isToday?' today':'')+'"><div class="cal-day-num'+(isToday?' today-num':'')+'">'+d+'</div>' +
      evs.slice(0,3).map(e => '<div class="cal-event" style="background:'+(OWNER_COLORS[e.owner]||'#58a6ff')+'99" title="'+esc(e.title)+'">'+TYPE_ICONS[e.type]+' '+esc(e.title)+'</div>').join('') +
      (evs.length>3?'<div style="font-size:9px;color:#8b949e">+'+( evs.length-3)+' more</div>':'') +
    '</div>';
  }
  html += '</div>';
  document.getElementById('cal-view').innerHTML = html;
}

function renderWeekly() {
  const startOfWeek = new Date(calDate);
  startOfWeek.setDate(calDate.getDate() - calDate.getDay());
  const y = startOfWeek.getFullYear(), m = startOfWeek.getMonth(), d = startOfWeek.getDate();
  document.getElementById('cal-title').textContent = 'Week of '+startOfWeek.toLocaleString('en-US',{month:'short',day:'numeric',year:'numeric'});
  const today = new Date();
  let html = '<div class="cal-grid">';
  ['Sun','Mon','Tue','Wed','Thu','Fri','Sat'].forEach((_,i) => {
    const day = new Date(y,m,d+i);
    const isToday = today.toDateString()===day.toDateString();
    const evs = eventsForDay(day.getFullYear(),day.getMonth(),day.getDate());
    html += '<div class="cal-cell'+(isToday?' today':'')+'"><div class="cal-day-num'+(isToday?' today-num':'')+'">'+['Sun','Mon','Tue','Wed','Thu','Fri','Sat'][i]+' '+day.getDate()+'</div>' +
      evs.map(e => '<div class="cal-event" style="background:'+(OWNER_COLORS[e.owner]||'#58a6ff')+'" title="'+esc(e.title)+'">'+TYPE_ICONS[e.type]+' '+esc(e.title)+'</div>').join('') +
    '</div>';
  });
  html += '</div>';
  document.getElementById('cal-view').innerHTML = html;
}

function renderEventList() {
  const now = new Date();
  const upcoming = calEvents.filter(e => new Date(e.end) >= now).sort((a,b)=>new Date(a.start)-new Date(b.start)).slice(0,10);
  document.getElementById('event-list').innerHTML = upcoming.length ? upcoming.map(e => {
    const c = OWNER_COLORS[e.owner]||'#58a6ff';
    return '<div style="display:flex;align-items:center;gap:10px;padding:7px 0;border-bottom:1px solid #21262d">' +
      '<div style="width:3px;height:32px;background:'+c+';border-radius:2px;flex-shrink:0"></div>' +
      '<div style="flex:1;min-width:0">' +
        '<div style="font-size:13px;font-weight:600">'+TYPE_ICONS[e.type]+' '+esc(e.title)+'</div>' +
        '<div style="font-size:11px;color:#8b949e">'+esc(e.owner)+' · '+new Date(e.start).toLocaleString('en-US',{month:'short',day:'numeric',hour:'2-digit',minute:'2-digit'})+'</div>' +
      '</div>' +
      '<button onclick="deleteEvent(\''+e.id+'\')" style="background:none;border:none;color:#484f58;cursor:pointer;font-size:14px" title="Delete">🗑️</button>' +
    '</div>';
  }).join('') : '<div style="color:#8b949e;font-size:13px">No upcoming events</div>';
}

function render() {
  if (calView==='month') renderMonthly(); else renderWeekly();
  renderEventList();
}

async function deleteEvent(id) {
  if (!confirm('Delete this event?')) return;
  const r = await authedFetch('/api/calendar/'+id, {method:'DELETE'});
  if (r && r.ok) { calEvents = calEvents.filter(e=>e.id!==id); showToast('Event deleted'); render(); }
  else showToast('Delete failed', true);
}

function openNewEvent() { document.getElementById('event-modal').style.display='flex'; }
function closeEventModal() { document.getElementById('event-modal').style.display='none'; }
async function submitEvent() {
  const title = document.getElementById('ev-title').value.trim();
  if (!title) return showToast('Title required', true);
  const payload = {
    title,
    start: document.getElementById('ev-start').value || new Date().toISOString(),
    end:   document.getElementById('ev-end').value   || new Date().toISOString(),
    owner: document.getElementById('ev-owner').value,
    type:  document.getElementById('ev-type').value,
    description: document.getElementById('ev-desc').value,
  };
  const r = await authedFetch('/api/calendar',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(payload)});
  if (r && r.ok) {
    const data = await r.json();
    calEvents.push(data.event);
    showToast('Event created'); closeEventModal(); render();
  } else showToast('Failed to create', true);
}

async function loadCalendar() {
  const r = await fetch('/api/calendar').catch(()=>null);
  if (r && r.ok) { const d = await r.json(); calEvents = d.events || []; }
  render();
}

loadCalendar();
`));
});

// ── Projects Page (Enhanced) ──────────────────────────────────────────────────

app.get('/projects', async (req, res) => {
  // Try to load real projects from repos.json; fall back to mock data
  let projects = V2_MOCK_PROJECTS;
  try {
    const raw = await readFile('/home/jkh/.openclaw/workspace/rcc/api/repos.json', 'utf8');
    const repos = JSON.parse(raw);
    // Merge mock health data where we have it
    projects = repos.map(r => {
      const mock = V2_MOCK_PROJECTS.find(m => m.id === r.full_name?.split('/')[1] || m.full_name === r.full_name) || {};
      return { ...mock, ...r, id: r.full_name?.split('/')[1] || r.id || r.display_name };
    });
  } catch { /* use mock data */ }

  res.type('html').send(v2Head('Projects', `
    .container { max-width: 1200px; margin: 0 auto; padding: 20px; }
    .projects-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(360px, 1fr)); gap: 14px; }
    .project-card { background: #161b22; border: 1px solid #30363d; border-radius: 10px; padding: 16px; cursor: pointer; transition: border-color .15s, box-shadow .15s; }
    .project-card:hover { border-color: #58a6ff; box-shadow: 0 4px 16px #0004; }
    .project-header { display: flex; align-items: flex-start; justify-content: space-between; margin-bottom: 8px; }
    .project-name { font-size: 15px; font-weight: 700; color: #f0f6fc; }
    .project-desc { font-size: 12px; color: #8b949e; margin-bottom: 10px; line-height: 1.4; }
    .project-stats { display: flex; gap: 10px; flex-wrap: wrap; font-size: 11px; color: #8b949e; }
    .project-stat { display: flex; align-items: center; gap: 4px; }
    .health-dot { width: 8px; height: 8px; border-radius: 50%; }
    .ci-chip { padding: 2px 7px; border-radius: 4px; font-size: 10px; font-weight: 600; }
    .ci-passing { background: #23863622; color: #3fb950; border: 1px solid #23863644; }
    .ci-failing { background: #f8514922; color: #f85149; border: 1px solid #f8514944; }
    .ci-pending { background: #1f6feb22; color: #58a6ff; border: 1px solid #1f6feb44; }
  `) + v2Nav('projects') + `
  <div class="container">
    <div style="display:flex;align-items:center;justify-content:space-between;margin-bottom:16px">
      <h1 style="font-size:20px;font-weight:700;color:#f0f6fc">📁 Projects</h1>
      <div style="display:flex;gap:8px">
        <input id="proj-search" placeholder="Search projects…" style="background:#161b22;border:1px solid #30363d;border-radius:6px;padding:6px 12px;color:#c9d1d9;font-size:13px;width:200px" oninput="filterProjects()">
      </div>
    </div>
    <div class="projects-grid" id="projects-grid">
      <div style="color:#8b949e;padding:20px">Loading…</div>
    </div>
  </div>
` + v2Foot(`
const ALL_PROJECTS = ${JSON.stringify(projects)};
let visibleProjects = [...ALL_PROJECTS];

function healthColor(lastCommit) {
  if (!lastCommit) return '#484f58';
  const age = Date.now() - new Date(lastCommit).getTime();
  if (age < 24*3600*1000)  return '#3fb950';
  if (age < 7*24*3600*1000) return '#d29922';
  return '#f85149';
}

function filterProjects() {
  const q = document.getElementById('proj-search').value.toLowerCase();
  visibleProjects = ALL_PROJECTS.filter(p =>
    !q || (p.display_name||p.id||'').toLowerCase().includes(q) ||
    (p.description||'').toLowerCase().includes(q)
  );
  renderProjects();
}

function renderProjects() {
  const grid = document.getElementById('projects-grid');
  if (!visibleProjects.length) {
    grid.innerHTML = '<div style="color:#8b949e;padding:20px">No projects found</div>';
    return;
  }
  grid.innerHTML = visibleProjects.map(p => {
    const hc = healthColor(p.lastCommit);
    const ciClass = p.ciStatus==='passing'?'ci-passing':p.ciStatus==='failing'?'ci-failing':'ci-pending';
    const ciLabel = p.ciStatus==='passing'?'✓ passing':p.ciStatus==='failing'?'✗ failing':'⏳ pending';
    const kindBadge = p.kind==='team'?'<span style="font-size:10px;background:#1f6feb22;color:#58a6ff;border:1px solid #1f6feb44;border-radius:4px;padding:1px 6px">team</span>':'<span style="font-size:10px;background:#21262d;color:#8b949e;border:1px solid #30363d;border-radius:4px;padding:1px 6px">personal</span>';
    const agentEmoji = EMOJIS[p.activeAgent||'rocky']||'🤖';
    return '<div class="project-card" onclick="window.location.href=\'/projects/'+esc(p.id)+'\'">' +
      '<div class="project-header">' +
        '<div>' +
          '<div class="project-name" style="display:flex;align-items:center;gap:8px">' +
            '<span class="health-dot" style="background:'+hc+'" title="Last commit: '+timeAgo(p.lastCommit)+'"></span>' +
            esc(p.display_name||p.id) +
          '</div>' +
          '<div style="font-size:10px;color:#484f58;margin-top:2px">'+esc(p.full_name||p.id)+'</div>' +
        '</div>' +
        '<div style="display:flex;gap:6px;align-items:center">'+kindBadge+'<span class="ci-chip '+ciClass+'">'+ciLabel+'</span></div>' +
      '</div>' +
      '<div class="project-desc">'+esc(p.description||'No description')+'</div>' +
      '<div class="project-stats">' +
        '<div class="project-stat">📝 '+timeAgo(p.lastCommit)+'</div>' +
        (p.openIssues!=null?'<div class="project-stat" style="color:'+(p.openIssues>0?'#f85149':'#8b949e')+'">🔴 '+p.openIssues+' issues</div>':'') +
        (p.openPRs!=null?'<div class="project-stat" style="color:'+(p.openPRs>0?'#a371f7':'#8b949e')+'">🟣 '+p.openPRs+' PRs</div>':'') +
        (p.queueItems!=null?'<div class="project-stat">📋 '+p.queueItems+' tasks</div>':'') +
        (p.activeAgent?'<div class="project-stat">'+agentEmoji+' '+esc(p.activeAgent)+'</div>':'') +
        (p.slackChannel?'<div class="project-stat" style="color:#8b949e">💬 '+esc(p.slackChannel)+'</div>':'') +
      '</div>' +
    '</div>';
  }).join('');
}

renderProjects();
`));
});

// ── SquirrelBus Tab ───────────────────────────────────────────────────────────

app.get('/squirrelbus', (req, res) => {
  res.type('html').send(v2Head('SquirrelBus', `
    .sb-container { max-width: 1100px; margin: 0 auto; padding: 20px; }
    .sb-toolbar { display: flex; gap: 8px; align-items: center; margin-bottom: 14px; flex-wrap: wrap; }
    .sb-search { background: #161b22; border: 1px solid #30363d; border-radius: 6px; padding: 7px 12px; color: #c9d1d9; font-size: 13px; flex: 1; min-width: 200px; }
    .sb-search:focus { outline: none; border-color: #58a6ff; }
    .filter-btn { background: #21262d; color: #c9d1d9; border: 1px solid #30363d; padding: 5px 12px; border-radius: 16px; cursor: pointer; font-size: 12px; }
    .filter-btn.active { background: #1f6feb; border-color: #1f6feb; color: #fff; }
    .bus-msg { background: #161b22; border: 1px solid #30363d; border-radius: 8px; padding: 10px 14px; margin-bottom: 6px; }
    .bus-msg.compact { background: transparent; border: none; padding: 3px 14px; margin-bottom: 1px; color: #484f58; font-size: 11px; }
    .bus-header { display: flex; justify-content: space-between; align-items: center; margin-bottom: 4px; font-size: 13px; }
    .type-badge { display: inline-block; padding: 1px 6px; border-radius: 3px; font-size: 10px; color: #fff; margin-left: 4px; }
    .send-panel { background: #161b22; border: 1px solid #30363d; border-radius: 8px; padding: 14px; margin-bottom: 14px; }
    summary { cursor: pointer; font-weight: 600; color: #58a6ff; font-size: 13px; list-style: none; }
  `) + v2Nav('squirrelbus') + `
  <div class="sb-container">
    <div style="display:flex;align-items:center;justify-content:space-between;margin-bottom:12px">
      <h1 style="font-size:18px;font-weight:700;color:#f0f6fc">📡 SquirrelBus</h1>
      <span id="sb-status" style="font-size:11px;color:#484f58"></span>
    </div>

    <!-- Send message panel -->
    <details class="send-panel">
      <summary>✉️ Send a message</summary>
      <div style="display:grid;grid-template-columns:1fr 1fr 1fr;gap:8px;margin-top:10px">
        <div><label style="font-size:11px;color:#8b949e">From</label><select id="msg-from" style="width:100%;background:#0d1117;border:1px solid #30363d;border-radius:4px;padding:5px;color:#c9d1d9;font-size:12px"><option value="rocky">Rocky</option><option value="jkh">jkh</option></select></div>
        <div><label style="font-size:11px;color:#8b949e">To</label><select id="msg-to" style="width:100%;background:#0d1117;border:1px solid #30363d;border-radius:4px;padding:5px;color:#c9d1d9;font-size:12px"><option value="all">All</option><option value="rocky">Rocky</option><option value="bullwinkle">Bullwinkle</option><option value="natasha">Natasha</option><option value="jkh">jkh</option></select></div>
        <div><label style="font-size:11px;color:#8b949e">Type</label><select id="msg-type" style="width:100%;background:#0d1117;border:1px solid #30363d;border-radius:4px;padding:5px;color:#c9d1d9;font-size:12px"><option value="text">text</option><option value="memo">memo</option></select></div>
      </div>
      <div style="margin-top:8px"><label style="font-size:11px;color:#8b949e">Subject</label><input id="msg-subject" style="width:100%;background:#0d1117;border:1px solid #30363d;border-radius:4px;padding:5px 8px;color:#c9d1d9;font-size:12px" placeholder="Optional…"></div>
      <div style="margin-top:6px"><label style="font-size:11px;color:#8b949e">Body</label><textarea id="msg-body" rows="3" style="width:100%;background:#0d1117;border:1px solid #30363d;border-radius:4px;padding:5px 8px;color:#c9d1d9;font-size:12px;resize:vertical"></textarea></div>
      <div style="margin-top:6px;text-align:right"><button onclick="sendMsg()" style="background:#238636;border:none;color:#fff;border-radius:4px;padding:5px 16px;font-size:12px;font-weight:600;cursor:pointer">Send</button></div>
    </details>

    <!-- Filters -->
    <div class="sb-toolbar">
      <input class="sb-search" id="sb-search" placeholder="Search messages…" oninput="renderBus()">
      <span style="font-size:12px;color:#8b949e">Agent:</span>
      <button class="filter-btn active" id="ba-all"         onclick="setBusAgent('all')">📡 All</button>
      <button class="filter-btn"       id="ba-rocky"        onclick="setBusAgent('rocky')">🐿️ Rocky</button>
      <button class="filter-btn"       id="ba-bullwinkle"   onclick="setBusAgent('bullwinkle')">🫎 Bullwinkle</button>
      <button class="filter-btn"       id="ba-natasha"      onclick="setBusAgent('natasha')">🕵️ Natasha</button>
      <button class="filter-btn"       id="ba-jkh"          onclick="setBusAgent('jkh')">👤 jkh</button>
      <span style="margin-left:8px;font-size:12px;color:#8b949e">Type:</span>
      <button class="filter-btn active" id="bt-all"         onclick="setBusType('all')">All</button>
      <button class="filter-btn"        id="bt-text"        onclick="setBusType('text')">text</button>
      <button class="filter-btn"        id="bt-memo"        onclick="setBusType('memo')">memo</button>
      <button class="filter-btn"        id="bt-event"       onclick="setBusType('event')">event</button>
      <label style="font-size:12px;color:#8b949e;display:flex;align-items:center;gap:5px;cursor:pointer;margin-left:8px">
        <input type="checkbox" id="show-hb" onchange="renderBus()"> Show heartbeats
      </label>
    </div>

    <div id="bus-messages"><div style="color:#8b949e;padding:12px">Loading…</div></div>
  </div>
` + v2Foot(`
const TYPE_COLORS_BUS = { text:'#58a6ff', memo:'#3fb950', blob:'#a371f7', heartbeat:'#8b949e', queue_sync:'#d29922', ping:'#3fb950', pong:'#3fb950', event:'#f85149', handoff:'#f0883e' };
let busMessages=[], busAgent='all', busType='all', lastBusTs=null;

function setBusAgent(a) {
  busAgent=a;
  document.querySelectorAll('[id^="ba-"]').forEach(b=>b.classList.remove('active'));
  document.getElementById('ba-'+a)?.classList.add('active');
  renderBus();
}
function setBusType(t) {
  busType=t;
  document.querySelectorAll('[id^="bt-"]').forEach(b=>b.classList.remove('active'));
  document.getElementById('bt-'+t)?.classList.add('active');
  renderBus();
}

function renderBus() {
  const q     = (document.getElementById('sb-search')?.value||'').toLowerCase();
  const showHB = document.getElementById('show-hb')?.checked;
  let msgs = busMessages;
  if (!showHB) msgs = msgs.filter(m => m.type!=='heartbeat'&&m.type!=='ping'&&m.type!=='pong');
  if (busAgent !== 'all') msgs = msgs.filter(m => m.from===busAgent||m.to===busAgent);
  if (busType  !== 'all') msgs = msgs.filter(m => m.type===busType);
  if (q) msgs = msgs.filter(m => (m.body||'').toLowerCase().includes(q)||(m.subject||'').toLowerCase().includes(q));
  const el = document.getElementById('bus-messages');
  document.getElementById('sb-status').textContent = msgs.length+' messages · last updated '+new Date().toLocaleTimeString();
  if (!msgs.length) { el.innerHTML='<div style="color:#8b949e;padding:12px">No messages match filter</div>'; return; }
  el.innerHTML = msgs.map(msg => {
    const fromEmoji = EMOJIS[msg.from]||'📨';
    const toLabel   = msg.to==='all'?'all':msg.to;
    const ts        = new Date(msg.ts).toLocaleString('en-US',{timeZone:'America/Los_Angeles',month:'short',day:'numeric',hour:'2-digit',minute:'2-digit'});
    const typeColor = TYPE_COLORS_BUS[msg.type]||'#8b949e';
    if (msg.type==='heartbeat'||msg.type==='ping'||msg.type==='pong') {
      return '<div class="bus-msg compact">'+fromEmoji+' '+esc(msg.from)+' 💓 '+msg.type+' · #'+msg.seq+' · '+esc(ts)+'</div>';
    }
    let bodyHtml = '';
    if (msg.type==='text'||msg.type==='memo') bodyHtml = '<div style="white-space:pre-wrap;font-size:13px">'+esc(msg.body||'')+'</div>';
    else bodyHtml = '<pre style="background:#0d1117;padding:6px;border-radius:4px;overflow-x:auto;font-size:11px">'+esc(typeof msg.body==='string'?msg.body.slice(0,300):JSON.stringify(msg.body,null,2).slice(0,300))+'</pre>';
    return '<div class="bus-msg">' +
      '<div class="bus-header">' +
        '<div>'+fromEmoji+' <strong style="color:#f0f6fc">'+esc(msg.from)+'</strong> <span style="color:#484f58">→</span> <strong>'+esc(toLabel)+'</strong><span class="type-badge" style="background:'+typeColor+'">'+esc(msg.type)+'</span></div>' +
        '<div style="color:#484f58;font-size:11px">#'+msg.seq+' · '+esc(ts)+'</div>' +
      '</div>' +
      (msg.subject?'<div style="font-weight:600;color:#58a6ff;margin-bottom:3px;font-size:13px">'+esc(msg.subject)+'</div>':'') +
      bodyHtml +
    '</div>';
  }).join('');
}

async function loadBus(initial) {
  const url = initial ? '/bus/messages?limit=100' : '/bus/messages?limit=100'+(lastBusTs?'&since='+encodeURIComponent(lastBusTs):'');
  const msgs = await fetch(url).then(r=>r.json()).catch(()=>[]);
  if (initial) { busMessages = msgs; }
  else if (msgs.length) {
    const existingIds = new Set(busMessages.map(m=>m.id));
    const newMsgs = msgs.filter(m=>!existingIds.has(m.id));
    if (newMsgs.length) busMessages = [...newMsgs,...busMessages];
  }
  if (busMessages.length && busMessages[0].ts) lastBusTs = busMessages[0].ts;
  renderBus();
}

async function sendMsg() {
  const token = getToken();
  if (!token) return showToast('No token', true);
  const body = {
    from: document.getElementById('msg-from').value,
    to:   document.getElementById('msg-to').value,
    type: document.getElementById('msg-type').value,
    subject: document.getElementById('msg-subject').value||null,
    body: document.getElementById('msg-body').value,
  };
  if (!body.body) return showToast('Body required', true);
  const r = await fetch('/bus/send',{method:'POST',headers:{'Authorization':'Bearer '+token,'Content-Type':'application/json'},body:JSON.stringify(body)});
  const d = await r.json();
  if (d.ok) { showToast('Sent!'); document.getElementById('msg-body').value=''; loadBus(true); }
  else showToast(d.error||'Error', true);
}

loadBus(true);
setInterval(()=>loadBus(false), 10000);
`));
});

// ── Settings Page ─────────────────────────────────────────────────────────────

app.get('/settings', (req, res) => {
  res.type('html').send(v2Head('Settings', `
    .settings-container { max-width: 960px; margin: 0 auto; padding: 20px; }
    .settings-section { background: #161b22; border: 1px solid #30363d; border-radius: 8px; padding: 20px; margin-bottom: 20px; }
    .settings-title { font-size: 15px; font-weight: 700; color: #f0f6fc; margin-bottom: 14px; display: flex; align-items: center; gap: 8px; }
    table { width: 100%; border-collapse: collapse; }
    th { text-align: left; padding: 8px 10px; color: #8b949e; font-size: 11px; font-weight: 600; text-transform: uppercase; border-bottom: 1px solid #30363d; }
    td { padding: 9px 10px; font-size: 13px; border-bottom: 1px solid #21262d; }
    tr:last-child td { border-bottom: none; }
    .status-chip { display: inline-flex; align-items: center; gap: 4px; padding: 2px 8px; border-radius: 10px; font-size: 11px; font-weight: 600; }
    .status-active  { background: #23863622; color: #3fb950; }
    .status-linked  { background: #d2992222; color: #d29922; }
    .status-offline { background: #f8514922; color: #f85149; }
    .token-masked { font-family: monospace; color: #8b949e; font-size: 12px; letter-spacing: .1em; }
  `) + v2Nav('settings') + `
  <div class="settings-container">
    <div style="display:flex;align-items:center;justify-content:space-between;margin-bottom:16px">
      <h1 style="font-size:20px;font-weight:700;color:#f0f6fc">⚙️ Settings</h1>
    </div>

    <!-- Communication Channels -->
    <div class="settings-section">
      <div class="settings-title">📡 Communication Channels</div>
      <table>
        <thead><tr><th>Channel</th><th>Type</th><th>Endpoint</th><th>Status</th><th>Last Activity</th></tr></thead>
        <tbody>
          <tr>
            <td>rocky↔bullwinkle DM</td>
            <td><span style="color:#8b949e">Mattermost</span></td>
            <td style="font-size:11px;color:#484f58">chat.yourmom.photos</td>
            <td><span class="status-chip status-active">🟢 active</span></td>
            <td style="color:#8b949e">3m ago</td>
          </tr>
          <tr>
            <td>rocky↔natasha DM</td>
            <td><span style="color:#8b949e">Mattermost</span></td>
            <td style="font-size:11px;color:#484f58">chat.yourmom.photos</td>
            <td><span class="status-chip status-active">🟢 active</span></td>
            <td style="color:#8b949e">18m ago</td>
          </tr>
          <tr>
            <td>#agent-shared</td>
            <td><span style="color:#8b949e">Mattermost</span></td>
            <td style="font-size:11px;color:#484f58">chat.yourmom.photos</td>
            <td><span class="status-chip status-active">🟢 active</span></td>
            <td style="color:#8b949e">8m ago</td>
          </tr>
          <tr>
            <td>#itsallgeektome</td>
            <td><span style="color:#8b949e">Slack</span></td>
            <td style="font-size:11px;color:#484f58">offtera.slack.com</td>
            <td><span class="status-chip status-linked">🟡 linked</span></td>
            <td style="color:#8b949e">2h ago</td>
          </tr>
          <tr>
            <td>#rockyandfriends</td>
            <td><span style="color:#8b949e">Slack</span></td>
            <td style="font-size:11px;color:#484f58">omgjkh.slack.com</td>
            <td><span class="status-chip status-active">🟢 active</span></td>
            <td style="color:#8b949e">15m ago</td>
          </tr>
          <tr>
            <td>jkh Telegram</td>
            <td><span style="color:#8b949e">Telegram</span></td>
            <td style="font-size:11px;color:#484f58">jkh's phone</td>
            <td><span class="status-chip status-active">🟢 active</span></td>
            <td style="color:#8b949e">1h ago</td>
          </tr>
          <tr>
            <td>SquirrelBus (local)</td>
            <td><span style="color:#8b949e">Internal</span></td>
            <td style="font-size:11px;color:#484f58">/bus (this host)</td>
            <td id="sb-local-status"><span class="status-chip status-active">🟢 active</span></td>
            <td id="sb-local-last" style="color:#8b949e">—</td>
          </tr>
          <tr>
            <td>SquirrelBus → Bullwinkle</td>
            <td><span style="color:#8b949e">Internal</span></td>
            <td style="font-size:11px;color:#484f58" id="bw-url">puck:8788</td>
            <td id="bw-status"><span class="status-chip status-linked">🟡 linked</span></td>
            <td style="color:#8b949e">—</td>
          </tr>
          <tr>
            <td>Milvus</td>
            <td><span style="color:#8b949e">Vector DB</span></td>
            <td style="font-size:11px;color:#484f58">do-host1:19530</td>
            <td><span class="status-chip status-linked">🟡 unchecked</span></td>
            <td style="color:#8b949e">—</td>
          </tr>
        </tbody>
      </table>
    </div>

    <!-- Agent Registry -->
    <div class="settings-section">
      <div class="settings-title">🤖 Agent Registry</div>
      <table>
        <thead><tr><th>Agent</th><th>Host</th><th>Capabilities</th><th>Status</th><th>Last Seen</th></tr></thead>
        <tbody id="agent-registry-rows">
          <tr><td colspan="5" style="color:#8b949e">Loading…</td></tr>
        </tbody>
      </table>
    </div>

    <!-- Auth Tokens -->
    <div class="settings-section">
      <div class="settings-title">🔑 Auth Tokens</div>
      <table>
        <thead><tr><th>Token</th><th>Scope</th><th>Status</th><th>Actions</th></tr></thead>
        <tbody>
          <tr>
            <td><span class="token-masked">wq-5dca••••••••••••••••••••••••••••••••</span></td>
            <td>All agents</td>
            <td><span class="status-chip status-active">🟢 active</span></td>
            <td><button onclick="showToast('Token rotation requires direct server access — see docs',true)" style="background:#21262d;border:1px solid #30363d;color:#8b949e;border-radius:4px;padding:3px 10px;font-size:11px;cursor:pointer">🔄 Rotate</button></td>
          </tr>
          <tr>
            <td><span class="token-masked">bw-••••••••••••••••••••••••••••••••••••</span></td>
            <td>Bullwinkle peer</td>
            <td><span class="status-chip status-active">🟢 active</span></td>
            <td><button onclick="showToast('Peer token rotation via Telegram — jkh only',true)" style="background:#21262d;border:1px solid #30363d;color:#8b949e;border-radius:4px;padding:3px 10px;font-size:11px;cursor:pointer">🔄 Rotate</button></td>
          </tr>
          <tr>
            <td><span class="token-masked">nat-••••••••••••••••••••••••••••••••••••</span></td>
            <td>Natasha peer</td>
            <td><span class="status-chip status-active">🟢 active</span></td>
            <td><button onclick="showToast('Peer token rotation via Telegram — jkh only',true)" style="background:#21262d;border:1px solid #30363d;color:#8b949e;border-radius:4px;padding:3px 10px;font-size:11px;cursor:pointer">🔄 Rotate</button></td>
          </tr>
        </tbody>
      </table>
    </div>

    <!-- Quick Links -->
    <div class="settings-section">
      <div class="settings-title">🔗 Quick Links</div>
      <div style="display:flex;gap:12px;flex-wrap:wrap">
        <a href="/" style="background:#21262d;border:1px solid #30363d;border-radius:6px;padding:8px 16px;font-size:13px;color:#c9d1d9;display:flex;align-items:center;gap:6px">📜 Legacy Dashboard (v1)</a>
        <a href="/activity" style="background:#21262d;border:1px solid #30363d;border-radius:6px;padding:8px 16px;font-size:13px;color:#c9d1d9;display:flex;align-items:center;gap:6px">🗺️ Activity Map</a>
        <a href="/api/digest" target="_blank" style="background:#21262d;border:1px solid #30363d;border-radius:6px;padding:8px 16px;font-size:13px;color:#c9d1d9;display:flex;align-items:center;gap:6px">📊 Digest API</a>
        <a href="/api/heartbeats" target="_blank" style="background:#21262d;border:1px solid #30363d;border-radius:6px;padding:8px 16px;font-size:13px;color:#c9d1d9;display:flex;align-items:center;gap:6px">💓 Heartbeats API</a>
        <a href="/bus/messages" target="_blank" style="background:#21262d;border:1px solid #30363d;border-radius:6px;padding:8px 16px;font-size:13px;color:#c9d1d9;display:flex;align-items:center;gap:6px">📡 Bus Messages API</a>
      </div>
    </div>
  </div>
` + v2Foot(`
async function loadSettings() {
  // Load agent registry from heartbeats
  const hbs = await fetch('/api/heartbeats').then(r=>r.json()).catch(()=>({}));
  const rows = document.getElementById('agent-registry-rows');
  const agents = ['rocky','bullwinkle','natasha','boris'];
  rows.innerHTML = agents.map(name => {
    const hb = hbs[name] || {};
    const age = hb.ts ? Date.now()-new Date(hb.ts).getTime() : Infinity;
    let dot='🔴', chipCls='status-offline', label='offline';
    if (age<5*60*1000) {dot='🟢';chipCls='status-active';label='online';}
    else if (age<30*60*1000) {dot='🟡';chipCls='status-linked';label='stale';}
    const caps = (hb.capabilities||[]).join(', ') || '—';
    return '<tr>' +
      '<td>'+EMOJIS[name]+' '+name+'</td>' +
      '<td style="font-size:12px;color:#8b949e">'+esc(hb.host||'—')+'</td>' +
      '<td style="font-size:11px;color:#8b949e">'+esc(caps)+'</td>' +
      '<td><span class="status-chip '+chipCls+'">'+dot+' '+label+'</span></td>' +
      '<td style="color:#8b949e">'+timeAgo(hb.ts)+'</td>' +
    '</tr>';
  }).join('');
}

loadSettings();
`));
});

// Initialize bus sequence on startup
initBusSeq();

// ─────────────────────────────────────────────────────────────────────────────
// /geek — Infrastructure Topology View
// Hybrid model: machines as primary nodes, service chips hanging off each.
// Shared infrastructure (Milvus, MinIO, SearXNG) get their own nodes with
// edges from each agent that calls them.
// SSE stream at /api/geek/stream feeds live traffic particles along edges.
// ─────────────────────────────────────────────────────────────────────────────

// In-memory SSE clients for /api/geek/stream
const geekSseClients = new Set();

// Broadcast a traffic event to all geek SSE clients
function broadcastGeekEvent(event) {
  const data = `data: ${JSON.stringify(event)}\n\n`;
  for (const res of geekSseClients) {
    try { res.write(data); } catch { geekSseClients.delete(res); }
  }
}

// Emit a geek traffic event (called from bus/send hook)
const _emitGeekOnBusSend = (from, to, type) => {
  broadcastGeekEvent({ from, to, type, ts: new Date().toISOString() });
};

// SSE endpoint — geek traffic stream
app.get('/api/geek/stream', (req, res) => {
  res.writeHead(200, {
    'Content-Type': 'text/event-stream',
    'Cache-Control': 'no-cache',
    'Connection': 'keep-alive',
    'X-Accel-Buffering': 'no',
  });
  res.write('retry: 3000\n\n');
  geekSseClients.add(res);
  req.on('close', () => geekSseClients.delete(res));
  const hb = setInterval(() => {
    try { res.write(': ping\n\n'); } catch { clearInterval(hb); geekSseClients.delete(res); }
  }, 20000);
  req.on('close', () => clearInterval(hb));
});

// Pull soul commit timeline for the geek view
async function getSoulTimeline() {
  try {
    const { stdout } = await execFileP('git', [
      'log', '--pretty=format:%H|%ai|%s',
      '--', 'openclaw/souls/rocky.md', 'openclaw/souls/bullwinkle.md', 'openclaw/souls/natasha.md'
    ], { cwd: __dirname + '/..', timeout: 5000 });
    return stdout.trim().split('\n').filter(Boolean).slice(0, 20).map(line => {
      const [hash, date, ...msgParts] = line.split('|');
      const msg = msgParts.join('|');
      const agent = /natasha/i.test(msg) ? 'natasha' : /bullwinkle/i.test(msg) ? 'bullwinkle' : 'rocky';
      return { hash: hash?.slice(0, 7), date, msg, agent };
    });
  } catch { return []; }
}

// API endpoint: soul timeline data
app.get('/api/geek/souls', async (req, res) => {
  const timeline = await getSoulTimeline();
  res.json(timeline);
});

// Geek view HTML
function renderGeekPage() {
  return `<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Geek View — RCC Infrastructure</title>
<style>
  * { box-sizing: border-box; margin: 0; padding: 0; }
  body { background: #0d1117; color: #c9d1d9; font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Helvetica, Arial, sans-serif; height: 100vh; overflow: hidden; display: flex; flex-direction: column; }
  #header { display: flex; align-items: center; gap: 16px; padding: 10px 20px; background: #161b22; border-bottom: 1px solid #30363d; flex-shrink: 0; }
  #header h1 { font-size: 15px; font-weight: 700; color: #f0f6fc; }
  #header .subtitle { font-size: 11px; color: #8b949e; }
  #header a { color: #58a6ff; text-decoration: none; font-size: 12px; margin-left: auto; }
  #header a:hover { text-decoration: underline; }
  #main { display: flex; flex: 1; overflow: hidden; }
  #topology { flex: 1; position: relative; overflow: hidden; }
  #topology svg { width: 100%; height: 100%; }
  #sidebar { width: 280px; border-left: 1px solid #30363d; display: flex; flex-direction: column; overflow: hidden; flex-shrink: 0; }
  #soul-panel { flex: 1; overflow-y: auto; padding: 12px; }
  #traffic-panel { height: 220px; border-top: 1px solid #30363d; overflow-y: auto; padding: 8px 12px; flex-shrink: 0; }
  .panel-title { font-size: 11px; font-weight: 700; color: #8b949e; text-transform: uppercase; letter-spacing: 0.05em; margin-bottom: 8px; }
  .soul-entry { display: flex; gap: 8px; align-items: flex-start; margin-bottom: 10px; }
  .soul-dot { width: 8px; height: 8px; border-radius: 50%; flex-shrink: 0; margin-top: 4px; }
  .soul-dot.rocky { background: #f85149; }
  .soul-dot.bullwinkle { background: #a371f7; }
  .soul-dot.natasha { background: #3fb950; }
  .soul-hash { font-size: 10px; color: #484f58; font-family: 'SF Mono', Consolas, monospace; }
  .soul-msg { font-size: 12px; color: #c9d1d9; line-height: 1.4; }
  .soul-date { font-size: 10px; color: #6e7681; }
  .traffic-entry { font-size: 11px; color: #8b949e; margin-bottom: 4px; border-bottom: 1px solid #21262d; padding-bottom: 4px; font-family: 'SF Mono', Consolas, monospace; }
  .traffic-entry .from { color: #58a6ff; }
  .traffic-entry .to { color: #3fb950; }
  .traffic-entry .type { color: #d29922; }
  @keyframes particleMove { from { offset-distance: 0%; } to { offset-distance: 100%; } }
  .machine-node:hover rect { stroke: #58a6ff !important; }
  .service-node:hover circle { stroke: #58a6ff !important; }
</style>
</head>
<body>
<div id="header">
  <h1>🖥️ Geek View — Fleet Topology</h1>
  <span class="subtitle" id="live-status">connecting…</span>
  <a href="/">← Dashboard</a>
</div>
<div id="main">
  <div id="topology">
    <svg id="topo-svg" viewBox="0 0 900 580" preserveAspectRatio="xMidYMid meet">
      <defs>
        <marker id="arrow" markerWidth="6" markerHeight="6" refX="5" refY="3" orient="auto">
          <path d="M0,0 L0,6 L6,3 z" fill="#30363d"/>
        </marker>
        <marker id="arrow-active" markerWidth="6" markerHeight="6" refX="5" refY="3" orient="auto">
          <path d="M0,0 L0,6 L6,3 z" fill="#58a6ff"/>
        </marker>
      </defs>
      <!-- Edges -->
      <path id="edge-rocky-natasha" fill="none" stroke="#30363d" stroke-width="1.5" opacity="0.5" d="M 300,160 C 450,100 550,100 600,160" marker-end="url(#arrow)"/>
      <path id="edge-rocky-bullwinkle" fill="none" stroke="#30363d" stroke-width="1.5" opacity="0.5" d="M 220,280 C 200,380 200,420 220,460" marker-end="url(#arrow)"/>
      <path id="edge-natasha-bullwinkle" fill="none" stroke="#30363d" stroke-width="1.5" opacity="0.5" d="M 650,280 C 640,380 500,440 360,460" marker-end="url(#arrow)"/>
      <path id="edge-rocky-milvus" fill="none" stroke="#21262d" stroke-width="1" stroke-dasharray="4,3" d="M 300,200 L 440,300"/>
      <path id="edge-rocky-minio" fill="none" stroke="#21262d" stroke-width="1" stroke-dasharray="4,3" d="M 260,200 L 390,330"/>
      <path id="edge-rocky-searxng" fill="none" stroke="#21262d" stroke-width="1" stroke-dasharray="4,3" d="M 240,200 L 350,340"/>
      <path id="edge-natasha-milvus" fill="none" stroke="#21262d" stroke-width="1" stroke-dasharray="4,3" d="M 600,200 L 480,300"/>
      <path id="edge-natasha-minio" fill="none" stroke="#21262d" stroke-width="1" stroke-dasharray="4,3" d="M 620,200 L 490,330"/>
      <path id="edge-boris-rocky" fill="none" stroke="#30363d" stroke-width="1.5" opacity="0.4" stroke-dasharray="5,3" d="M 800,200 C 700,180 400,180 300,200" marker-end="url(#arrow)"/>
      <path id="edge-natasha-nvidia" fill="none" stroke="#1a3a2a" stroke-width="1" stroke-dasharray="4,3" d="M 700,130 L 820,70"/>
      <!-- Particle container -->
      <g id="particles"></g>
      <!-- Rocky (do-host1) -->
      <g class="machine-node" id="node-rocky" transform="translate(140,130)">
        <rect width="180" height="130" rx="10" fill="#161b22" stroke="#f85149" stroke-width="1.5"/>
        <text x="10" y="20" font-size="12" font-weight="700" fill="#f85149">🐿️ Rocky</text>
        <text x="10" y="34" font-size="9" fill="#6e7681">do-host1 · CPU-only VPS</text>
        <text x="10" y="52" font-size="9" fill="#8b949e" font-family="monospace">[RCC API :8789]</text>
        <text x="10" y="64" font-size="9" fill="#8b949e" font-family="monospace">[Dashboard :8788]</text>
        <text x="10" y="76" font-size="9" fill="#8b949e" font-family="monospace">[SearXNG :8888]</text>
        <text x="10" y="88" font-size="9" fill="#8b949e" font-family="monospace">[MinIO :9000/:9001]</text>
        <text x="10" y="100" font-size="9" fill="#8b949e" font-family="monospace">[Milvus :19530]</text>
        <text x="10" y="112" font-size="9" fill="#8b949e" font-family="monospace">[SquirrelBus :8788/bus]</text>
        <circle id="dot-rocky" cx="168" cy="12" r="5" fill="#3fb950"/>
      </g>
      <!-- Natasha (sparky) -->
      <g class="machine-node" id="node-natasha" transform="translate(570,130)">
        <rect width="200" height="150" rx="10" fill="#161b22" stroke="#3fb950" stroke-width="1.5"/>
        <text x="10" y="20" font-size="12" font-weight="700" fill="#3fb950">🕵️ Natasha</text>
        <text x="10" y="34" font-size="9" fill="#6e7681">sparky · GB10 · 128GB unified</text>
        <text x="10" y="52" font-size="9" fill="#8b949e" font-family="monospace">[OpenClaw :18789]</text>
        <text x="10" y="64" font-size="9" fill="#8b949e" font-family="monospace">[SquirrelBus /bus→:18799]</text>
        <text x="10" y="76" font-size="9" fill="#8b949e" font-family="monospace">[Ollama :11434 ✓]</text>
        <text x="10" y="88" font-size="9" fill="#484f58" font-family="monospace"> qwen2.5-coder:32b</text>
        <text x="10" y="100" font-size="9" fill="#484f58" font-family="monospace"> qwen3-coder:latest</text>
        <text x="10" y="112" font-size="9" fill="#3fb950" font-family="monospace">[CUDA/RTX ⚡]</text>
        <text x="10" y="124" font-size="9" fill="#8b949e" font-family="monospace">[Milvus :19530]</text>
        <circle id="dot-natasha" cx="188" cy="12" r="5" fill="#3fb950"/>
      </g>
      <!-- Bullwinkle (puck) -->
      <g class="machine-node" id="node-bullwinkle" transform="translate(140,430)">
        <rect width="180" height="110" rx="10" fill="#161b22" stroke="#a371f7" stroke-width="1.5"/>
        <text x="10" y="20" font-size="12" font-weight="700" fill="#a371f7">🫎 Bullwinkle</text>
        <text x="10" y="34" font-size="9" fill="#6e7681">puck · Mac · CPU-only</text>
        <text x="10" y="52" font-size="9" fill="#8b949e" font-family="monospace">[OpenClaw :18789]</text>
        <text x="10" y="64" font-size="9" fill="#8b949e" font-family="monospace">[Calendar / iMessage]</text>
        <text x="10" y="76" font-size="9" fill="#8b949e" font-family="monospace">[Google Workspace]</text>
        <text x="10" y="88" font-size="9" fill="#8b949e" font-family="monospace">[Sonos]</text>
        <circle id="dot-bullwinkle" cx="168" cy="12" r="5" fill="#3fb950"/>
      </g>
      <!-- Boris (l40-sweden) -->
      <g class="machine-node" id="node-boris" transform="translate(760,130)">
        <rect width="120" height="110" rx="10" fill="#161b22" stroke="#d29922" stroke-width="1.5" stroke-dasharray="5,3"/>
        <text x="10" y="20" font-size="12" font-weight="700" fill="#d29922">⚡ Boris</text>
        <text x="10" y="34" font-size="9" fill="#6e7681">l40-sweden</text>
        <text x="10" y="52" font-size="9" fill="#8b949e" font-family="monospace">[4x L40]</text>
        <text x="10" y="64" font-size="9" fill="#8b949e" font-family="monospace">[128GB RAM]</text>
        <text x="10" y="76" font-size="9" fill="#8b949e" font-family="monospace">[Omniverse]</text>
        <text x="10" y="88" font-size="9" fill="#d29922" font-family="monospace">[Isaac Lab]</text>
        <circle id="dot-boris" cx="108" cy="12" r="5" fill="#484f58"/>
      </g>
      <!-- Shared infra nodes -->
      <g class="service-node" transform="translate(430,280)">
        <circle cx="0" cy="0" r="32" fill="#161b22" stroke="#1f6feb" stroke-width="1.5"/>
        <text x="0" y="-8" font-size="10" font-weight="700" fill="#1f6feb" text-anchor="middle">Milvus</text>
        <text x="0" y="6" font-size="8" fill="#6e7681" text-anchor="middle">:19530</text>
        <text x="0" y="18" font-size="8" fill="#484f58" text-anchor="middle">do-host1</text>
      </g>
      <g class="service-node" transform="translate(430,360)">
        <circle cx="0" cy="0" r="30" fill="#161b22" stroke="#1f6feb" stroke-width="1.5"/>
        <text x="0" y="-6" font-size="10" font-weight="700" fill="#1f6feb" text-anchor="middle">MinIO</text>
        <text x="0" y="8" font-size="8" fill="#6e7681" text-anchor="middle">:9000/:9001</text>
        <text x="0" y="19" font-size="8" fill="#484f58" text-anchor="middle">do-host1</text>
      </g>
      <g class="service-node" transform="translate(350,420)">
        <circle cx="0" cy="0" r="28" fill="#161b22" stroke="#1f6feb" stroke-width="1.5"/>
        <text x="0" y="-5" font-size="10" font-weight="700" fill="#1f6feb" text-anchor="middle">SearXNG</text>
        <text x="0" y="9" font-size="8" fill="#6e7681" text-anchor="middle">:8888</text>
        <text x="0" y="19" font-size="8" fill="#484f58" text-anchor="middle">do-host1</text>
      </g>
      <!-- External: NVIDIA Gateway -->
      <g transform="translate(820,30)">
        <rect width="70" height="38" rx="6" fill="#0d1117" stroke="#3fb95044" stroke-width="1" stroke-dasharray="3,2"/>
        <text x="35" y="14" font-size="9" fill="#3fb950" text-anchor="middle">NVIDIA</text>
        <text x="35" y="26" font-size="9" fill="#3fb950" text-anchor="middle">Gateway</text>
        <text x="35" y="35" font-size="8" fill="#484f58" text-anchor="middle">cloud</text>
      </g>
    </svg>
  </div>
  <div id="sidebar">
    <div id="soul-panel">
      <div class="panel-title">🧠 Soul Commits</div>
      <div id="soul-timeline"><span style="font-size:11px;color:#484f58">Loading…</span></div>
    </div>
    <div id="traffic-panel">
      <div class="panel-title">📡 Live Traffic <span id="traffic-count" style="color:#484f58;font-weight:400">(0)</span></div>
      <div id="traffic-log"></div>
    </div>
  </div>
</div>
<script>
(function() {
  fetch('/api/geek/souls').then(r => r.json()).then(entries => {
    const el = document.getElementById('soul-timeline');
    if (!entries.length) { el.innerHTML = '<span style="font-size:11px;color:#484f58">No soul commits yet.</span>'; return; }
    el.innerHTML = entries.map(e => {
      const d = new Date(e.date);
      const rel = formatRel(d);
      return \`<div class="soul-entry">
        <div class="soul-dot \${esc(e.agent)}"></div>
        <div>
          <div class="soul-msg">\${esc(e.msg)}</div>
          <div class="soul-hash">\${esc(e.hash)} · \${esc(rel)} · \${esc(e.agent)}</div>
        </div>
      </div>\`;
    }).join('');
  }).catch(() => {
    document.getElementById('soul-timeline').innerHTML = '<span style="font-size:11px;color:#484f58">Unavailable.</span>';
  });

  let trafficCount = 0;
  const trafficLog = document.getElementById('traffic-log');
  const trafficCountEl = document.getElementById('traffic-count');
  const statusEl = document.getElementById('live-status');
  const particles = document.getElementById('particles');
  const edgeMap = {
    'rocky\u2192natasha': 'edge-rocky-natasha', 'natasha\u2192rocky': 'edge-rocky-natasha',
    'rocky\u2192bullwinkle': 'edge-rocky-bullwinkle', 'bullwinkle\u2192rocky': 'edge-rocky-bullwinkle',
    'natasha\u2192bullwinkle': 'edge-natasha-bullwinkle', 'bullwinkle\u2192natasha': 'edge-natasha-bullwinkle',
  };
  const agentColors = { rocky: '#f85149', natasha: '#3fb950', bullwinkle: '#a371f7', boris: '#d29922' };

  function spawnParticle(from, to, color) {
    const edgeId = edgeMap[from + '\u2192' + to] || edgeMap[to + '\u2192' + from];
    if (!edgeId) return;
    const edgeEl = document.getElementById(edgeId);
    if (!edgeEl) return;
    const dur = 1200 + Math.random() * 800;
    const circle = document.createElementNS('http://www.w3.org/2000/svg', 'circle');
    circle.setAttribute('r', '3');
    circle.setAttribute('fill', color || '#58a6ff');
    circle.style.offsetPath = 'path("' + edgeEl.getAttribute('d') + '")';
    circle.style.offsetDistance = '0%';
    circle.style.animation = 'particleMove ' + dur + 'ms linear forwards';
    particles.appendChild(circle);
    setTimeout(() => circle.remove(), dur + 100);
    edgeEl.setAttribute('stroke', color || '#58a6ff');
    edgeEl.setAttribute('marker-end', 'url(#arrow-active)');
    setTimeout(() => {
      edgeEl.setAttribute('stroke', '#30363d');
      edgeEl.setAttribute('marker-end', 'url(#arrow)');
    }, Math.max(dur, 1500));
  }

  function addTrafficEntry(evt) {
    trafficCount++;
    trafficCountEl.textContent = '(' + trafficCount + ')';
    const div = document.createElement('div');
    div.className = 'traffic-entry';
    const time = new Date(evt.ts || Date.now()).toLocaleTimeString();
    div.innerHTML = '<span style="color:#484f58">' + esc(time) + '</span> <span class="from">' + esc(evt.from||'?') + '</span> → <span class="to">' + esc(evt.to||'?') + '</span> <span class="type">' + esc(evt.type||evt.kind||'msg') + '</span>';
    trafficLog.insertBefore(div, trafficLog.firstChild);
    while (trafficLog.children.length > 20) trafficLog.removeChild(trafficLog.lastChild);
    spawnParticle(evt.from, evt.to, agentColors[evt.from] || '#58a6ff');
  }

  // Use Rocky's RCC API (port 8789) for topology data — richer node/edge/status model
  const RCC_API = window.location.protocol + '//' + window.location.hostname + ':8789';

  function connectSSE() {
    statusEl.textContent = 'connecting…';
    statusEl.style.color = '#8b949e';
    // Primary: Rocky's authoritative SSE stream on port 8789
    // Fallback: local dashboard stream on same port
    const primaryUrl = RCC_API + '/api/geek/stream';
    const fallbackUrl = '/api/geek/stream';
    let useFallback = false;

    function tryConnect(url) {
      const es = new EventSource(url);
      const timeout = setTimeout(() => {
        if (!useFallback && url === primaryUrl) {
          es.close();
          useFallback = true;
          tryConnect(fallbackUrl);
        }
      }, 5000);
      es.onopen = () => {
        clearTimeout(timeout);
        statusEl.textContent = '● live' + (useFallback ? ' (local)' : '');
        statusEl.style.color = '#3fb950';
      };
      es.onerror = () => {
        clearTimeout(timeout);
        statusEl.textContent = '○ reconnecting…';
        statusEl.style.color = '#d29922';
        es.close();
        setTimeout(() => tryConnect(useFallback ? fallbackUrl : primaryUrl), 4000);
      };
      es.onmessage = e => {
        try {
          const evt = JSON.parse(e.data);
          if (evt.type === 'connected') return; // Rocky sends a connected event on open
          addTrafficEntry(evt);
        } catch {}
      };
    }
    tryConnect(primaryUrl);
  }
  connectSSE();

  // Use /api/geek/topology from RCC API for richer status data (nodes with status field)
  function updateDots() {
    fetch(RCC_API + '/api/geek/topology')
      .then(r => r.json())
      .then(data => {
        const nodes = data.nodes || [];
        for (const node of nodes) {
          if (node.type !== 'agent') continue;
          const dot = document.getElementById('dot-' + node.id);
          if (!dot) continue;
          const color = node.status === 'online' ? '#3fb950' : node.status === 'stale' ? '#d29922' : '#f85149';
          dot.setAttribute('fill', color);
        }
      })
      .catch(() => {
        // Fallback to /api/heartbeats on the dashboard port
        fetch('/api/heartbeats').then(r => r.json()).then(data => {
          for (const a of ['rocky','natasha','bullwinkle','boris']) {
            const dot = document.getElementById('dot-' + a);
            if (!dot) continue;
            const hb = data[a];
            if (!hb || !hb.ts) { dot.setAttribute('fill', '#484f58'); continue; }
            const age = (Date.now() - new Date(hb.ts).getTime()) / 1000;
            dot.setAttribute('fill', age < 120 ? '#3fb950' : age < 300 ? '#d29922' : '#f85149');
          }
        }).catch(() => {});
      });
  }
  updateDots();
  setInterval(updateDots, 30000);

  function esc(s) { return String(s||'').replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;'); }
  function formatRel(d) {
    const diff = Date.now() - d.getTime();
    const m = Math.floor(diff / 60000);
    if (m < 60) return m + 'm ago';
    const h = Math.floor(m / 60);
    if (h < 24) return h + 'h ago';
    return Math.floor(h / 24) + 'd ago';
  }
})();
</script>
</body>
</html>`;
}

// Route: /geek
app.get('/geek', (req, res) => {
  res.type('html').send(renderGeekPage());
});

// Start server
app.listen(PORT, '0.0.0.0', () => {
  console.log(`🐿️ Rocky Command Center running on http://0.0.0.0:${PORT}`);
});
