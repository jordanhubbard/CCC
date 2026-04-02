#!/usr/bin/env node
/**
 * WQ Dashboard — CCC unified workqueue + ClawBus dashboard
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
const AUTH_TOKEN = process.env.RCC_AUTH_TOKENS || process.env.RCC_AGENT_TOKEN || '<YOUR_RCC_TOKEN>';
const QUEUE_PATH = '/home/jkh/.openclaw/workspace/workqueue/queue.json';
const MC_PATH = '/home/jkh/.local/bin/mc';
const MINIO_ALIAS = process.env.MINIO_ALIAS || 'local';
const BUS_LOG_PATH  = '/home/jkh/.openclaw/workspace/squirrelbus/bus.jsonl';
const ACK_LOG_PATH  = '/home/jkh/.openclaw/workspace/squirrelbus/acks.jsonl';
const DEAD_LOG_PATH = '/home/jkh/.openclaw/workspace/squirrelbus/dead-letter.jsonl';

// ── ClawBus peer fan-out registry ─────────────────────────────────────────
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
const BULLWINKLE_TOKEN = process.env.BULLWINKLE_TOKEN || 'SQUIRRELBUS_TOKEN_REMOVED';
const NATASHA_TOKEN    = process.env.NATASHA_TOKEN    || 'RCC_AUTH_TOKEN_REMOVED';
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
    return res.status(401).json({ error: 'Unauthorized — check your auth token (RCC_AUTH_TOKEN_REMOVED)' });
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
      source: 'jkh',
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
      item.journal.push({ ts: now, author: 'jkh', type: 'status-change', text: `Updated: ${changed.join('; ')}` });
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
    const author = (req.body.author || 'jkh').trim();
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
    const entry = { ts: now, author: 'jkh', type: 'choice', text: `Choice recorded: [${choice}] ${choiceLabel || ''}` };
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
    const userEntry = { ts: now, author: 'jkh', type: 'ai', text: `✨ ${prompt}` };
    item.journal.push(userEntry);

    // Call OpenClaw gateway
    let aiText = '(no response)';
    try {
      const gwResp = await fetch('http://localhost:18789/v1/chat/completions', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', 'Authorization': 'Bearer SQUIRRELBUS_TOKEN_REMOVED' },
        body: JSON.stringify({
          model: 'nvidia/azure/anthropic/claude-sonnet-4-6',
          messages: [
            { role: 'system', content: `You are CCC AI agent helping with a work item. Item title: "${item.title}". Description: "${item.description || ''}". Be concise and helpful.` },
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

    const aiEntry = { ts: new Date().toISOString(), author: '🐾 CCC', type: 'ai', text: aiText };
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
      channel: 'slack',
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
  <title>🐿️ Claw Command Center</title>
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

    /* Toast */
    .toast { position: fixed; bottom: 20px; right: 20px; background: #238636; color: #fff; padding: 10px 18px; border-radius: 8px; font-size: 13px; display: none; z-index: 999; }
    .toast.error { background: #f85149; }

    /* Footer */
    .footer { color: #484f58; font-size: 11px; text-align: center; margin-top: 16px; padding-top: 12px; border-top: 1px solid #21262d; }
  </style>
</head>
<body>
  <div class="container">
    <div style="display:flex;align-items:center;justify-content:space-between;margin-bottom:20px">
      <h1 style="font-size:24px;margin:0;color:#f0f6fc">🐿️ Claw Command Center</h1>
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

    <!-- Section 3: ClawBus -->
    <div class="section">
      <div class="section-header">📡 ClawBus</div>
      <details class="send-form">
        <summary>✉️ Send a message</summary>
        <div style="display:grid;grid-template-columns:1fr 1fr 1fr;gap:6px;margin-top:8px">
          <div><label style="font-size:11px;color:#8b949e">From</label><select id="msg-from" style="width:100%"><option value="rocky">Rocky</option><option value="jkh">jkh</option></select></div>
          <div><label style="font-size:11px;color:#8b949e">To</label><select id="msg-to" style="width:100%"><option value="all">All</option><option value="rocky">Rocky</option><option value="bullwinkle">Bullwinkle</option><option value="natasha">Natasha</option><option value="jkh">jkh</option></select></div>
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
    const EMOJIS = { rocky: '🐿️', bullwinkle: '🫎', natasha: '🕵️‍♀️', boris: '🕵️‍♂️', jkh: '👤' };
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
          const queueDepth = hb.queueDepth != null ? '<div class="agent-meta">Queue: ' + hb.queueDepth + ' items</div>' : '';
          return '<div class="agent-card">' +
            '<div class="agent-name">' + emoji + ' ' + name.charAt(0).toUpperCase() + name.slice(1) + '</div>' +
            '<div class="' + stClass + '">' + stEmoji + ' ' + stLabel + '</div>' +
            '<div class="agent-meta">Host: ' + esc(host) + '</div>' +
            '<div class="agent-meta">Last seen: ' + lastSeen + '</div>' +
            queueDepth +
            '</div>';
        }).join('');
      } catch (e) { console.error('Agent load error:', e); }
    }

    // === Section 2: Work Queue ===
    let queueItems = [];
    let currentFilter = 'jkh'; // default to jkh-assigned so blocked items are front and center
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
        if (item.assignee === 'jkh') counts.jkh++;
      }
      const filters = [['jkh','⏳ Needs Me'],['all','All'],['pending','Pending'],['in-progress','In Progress'],['blocked','Blocked'],['deferred','Deferred'],['completed','Completed'],['idea','Ideas']];
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
      if (currentFilter === 'jkh') filtered = filtered.filter(i => i.assignee === 'jkh');
      else if (currentFilter !== 'all') filtered = filtered.filter(i => i.status === currentFilter);
      const container = document.getElementById('queue-cards');
      if (!container) return;
      if (filtered.length === 0) {
        container.innerHTML = '<div style="padding:32px;color:#8b949e;text-align:center">No items' + (currentFilter === 'jkh' ? ' — nothing needs your attention right now 🎉' : '') + '</div>';
        return;
      }
      container.innerHTML = filtered.map(item => renderCard(item)).join('');
    }

    function renderCard(item) {
      const isJkh = item.assignee === 'jkh';
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
        body: JSON.stringify({ text, author: 'jkh' })
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

    // === Section 3: ClawBus ===
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

    // === Init & refresh ===
    loadAgents();
    loadQueue();
    loadBus(true);

    // Auto-refresh agent cards every 30s
    setInterval(loadAgents, 30000);
    // Auto-refresh queue every 60s
    setInterval(loadQueue, 60000);
    // Auto-refresh bus every 10s (incremental)
    setInterval(() => loadBus(false), 10000);

    document.getElementById('footer').textContent = '🐿️ Claw Command Center · Auto-refreshing · Rendered: ' + new Date().toLocaleString();
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
    //   "jkh comment [2026-03-21T04:08:00.000Z]: text"
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
        else if (/jkh comment/.test(lower))                  type = 'comment';
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
// ClawBus v1 — Inter-agent comms
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
    console.log(`📡 ClawBus: initialized seq=${busSeq}`);
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
    const jkhItems = allItems.filter(i => i.assignee === 'jkh');
    const jkhLastAct = jkhItems.sort((a,b) =>
      new Date(b.completedAt||b.created||0) - new Date(a.completedAt||a.created||0))[0];
    const jkhScore = recencyScore(jkhLastAct?.completedAt || jkhLastAct?.created);
    peopleMap.set('jkh', {
      id: 'person:jkh',
      kind: 'person',
      label: 'jkh',
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
        if (login === 'jordanhubbard') continue; // that's jkh
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
          || (login === 'jkh' && repo.ownership?.owner === 'jkh');
        if (isContrib) {
          edges.push({ source: personNode.id, target: `project:${repo.full_name}`, weight: 2, kind: 'contributor' });
        }
      }
    }

    // Person → Agent edges (jkh directs agents)
    for (const agentNode of agentNodes) {
      edges.push({ source: 'person:jkh', target: agentNode.id, weight: 3, kind: 'directs' });
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
// All write/read routes require Bearer RCC_AUTH_TOKEN_REMOVED

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
        <text x="10" y="20" font-size="12" font-weight="700" fill="#f85149">🐾 Rocky</text>
        <text x="10" y="34" font-size="9" fill="#6e7681">do-host1 · CPU-only VPS</text>
        <text x="10" y="52" font-size="9" fill="#8b949e" font-family="monospace">[RCC API :8789]</text>
        <text x="10" y="64" font-size="9" fill="#8b949e" font-family="monospace">[Dashboard :8788]</text>
        <text x="10" y="76" font-size="9" fill="#8b949e" font-family="monospace">[SearXNG :8888]</text>
        <text x="10" y="88" font-size="9" fill="#8b949e" font-family="monospace">[MinIO :9000/:9001]</text>
        <text x="10" y="100" font-size="9" fill="#8b949e" font-family="monospace">[Milvus :19530]</text>
        <text x="10" y="112" font-size="9" fill="#8b949e" font-family="monospace">[ClawBus :8788/bus]</text>
        <circle id="dot-rocky" cx="168" cy="12" r="5" fill="#3fb950"/>
      </g>
      <!-- Natasha (sparky) -->
      <g class="machine-node" id="node-natasha" transform="translate(570,130)">
        <rect width="200" height="150" rx="10" fill="#161b22" stroke="#3fb950" stroke-width="1.5"/>
        <text x="10" y="20" font-size="12" font-weight="700" fill="#3fb950">🕵️ Natasha</text>
        <text x="10" y="34" font-size="9" fill="#6e7681">sparky · GB10 · 128GB unified</text>
        <text x="10" y="52" font-size="9" fill="#8b949e" font-family="monospace">[OpenClaw :18789]</text>
        <text x="10" y="64" font-size="9" fill="#8b949e" font-family="monospace">[ClawBus /bus→:18799]</text>
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

  function connectSSE() {
    statusEl.textContent = 'connecting…';
    statusEl.style.color = '#8b949e';
    const es = new EventSource('/api/geek/stream');
    es.onopen = () => { statusEl.textContent = '● live'; statusEl.style.color = '#3fb950'; };
    es.onerror = () => { statusEl.textContent = '○ reconnecting…'; statusEl.style.color = '#d29922'; es.close(); setTimeout(connectSSE, 4000); };
    es.onmessage = e => { try { addTrafficEntry(JSON.parse(e.data)); } catch {} };
  }
  connectSSE();

  function updateDots() {
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
  console.log(`🐿️ Claw Command Center running on http://0.0.0.0:${PORT}`);
});
