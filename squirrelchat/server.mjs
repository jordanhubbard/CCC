import express from 'express';
import { ingestMessage } from '../../../.openclaw/workspace/rcc/vector/ingest.mjs';
import { DatabaseSync as Database } from 'node:sqlite';
import archiver from 'archiver';
import multer from 'multer';
import { createServer } from 'http';
import { fileURLToPath } from 'url';
import { dirname, join } from 'path';


const __dirname = dirname(fileURLToPath(import.meta.url));
const app = express();
const PORT = process.env.PORT || 8790;
const ADMIN_TOKEN = 'sc-squirrelchat-admin-2026';
const RCC_BASE = 'http://localhost:8789';
const RCC_AGENT_TOKEN = process.env.RCC_AGENT_TOKEN || 'wq-5dcad756f6d3e345c00b5cb3dfcbdedb';

// DB setup
const db = new Database('./squirrelchat.db');

// Phase 1 schema — legacy tables + new SC tables (additive migration)
db.exec(`
  -- Legacy tables (kept for backward compat)
  CREATE TABLE IF NOT EXISTS messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ts INTEGER NOT NULL,
    from_agent TEXT NOT NULL,
    text TEXT NOT NULL,
    channel TEXT NOT NULL DEFAULT 'chat',
    mentions TEXT,
    slash_result TEXT,
    thread_id INTEGER REFERENCES messages(id),
    edited_at INTEGER,
    created_at INTEGER DEFAULT (strftime('%s','now') * 1000)
  );
  CREATE TABLE IF NOT EXISTS projects (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT,
    tags TEXT,
    assignee TEXT,
    status TEXT DEFAULT 'active',
    created_at INTEGER DEFAULT (strftime('%s','now') * 1000),
    updated_at INTEGER DEFAULT (strftime('%s','now') * 1000)
  );
  CREATE TABLE IF NOT EXISTS project_files (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id TEXT NOT NULL,
    filename TEXT NOT NULL,
    content BLOB,
    encoding TEXT DEFAULT 'utf8',
    size INTEGER,
    created_at INTEGER DEFAULT (strftime('%s','now') * 1000),
    UNIQUE(project_id, filename)
  );

  -- New SC tables (Phase 1)
  CREATE TABLE IF NOT EXISTS users (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    role TEXT NOT NULL DEFAULT 'user',
    avatar_url TEXT,
    token_hash TEXT,
    status TEXT DEFAULT 'offline',
    last_seen INTEGER,
    created_at INTEGER DEFAULT (strftime('%s','now') * 1000)
  );
  CREATE TABLE IF NOT EXISTS channels (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT,
    type TEXT NOT NULL DEFAULT 'channel',
    participants TEXT,
    created_at INTEGER DEFAULT (strftime('%s','now') * 1000),
    last_message_at INTEGER
  );
  CREATE TABLE IF NOT EXISTS reactions (
    message_id INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL,
    emoji TEXT NOT NULL,
    created_at INTEGER DEFAULT (strftime('%s','now') * 1000),
    PRIMARY KEY (message_id, user_id, emoji)
  );
  CREATE TABLE IF NOT EXISTS files (
    id TEXT PRIMARY KEY,
    filename TEXT NOT NULL,
    size INTEGER,
    mime_type TEXT,
    storage_key TEXT NOT NULL,
    uploader TEXT NOT NULL,
    channel TEXT,
    created_at INTEGER DEFAULT (strftime('%s','now') * 1000)
  );
`);

// Additive migrations for existing messages table (add thread_id, edited_at if missing)
try { db.exec('ALTER TABLE messages ADD COLUMN thread_id INTEGER REFERENCES messages(id)'); } catch {}
try { db.exec('ALTER TABLE messages ADD COLUMN edited_at INTEGER'); } catch {}

// Additive migrations for users table
try { db.exec('ALTER TABLE users ADD COLUMN avatar_url TEXT'); } catch {}
try { db.exec('ALTER TABLE users ADD COLUMN role TEXT NOT NULL DEFAULT \'user\''); } catch {}
// Rename users.token → users.token_hash if needed (backward compat — just add alias col)
try { db.exec('ALTER TABLE users ADD COLUMN token_hash TEXT'); } catch {}

// Additive migrations for channels table
try { db.exec('ALTER TABLE channels ADD COLUMN last_message_at INTEGER'); } catch {}

// Align reactions table — add user_id col if it only has from_agent
try { db.exec('ALTER TABLE reactions ADD COLUMN user_id TEXT'); } catch {}
// Backfill user_id from from_agent if exists
try { db.exec('UPDATE reactions SET user_id = from_agent WHERE user_id IS NULL AND from_agent IS NOT NULL'); } catch {}

// FTS5 for message search
try {
  db.exec(`
    CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(text, content=messages, content_rowid=id);
    CREATE TRIGGER IF NOT EXISTS messages_ai AFTER INSERT ON messages BEGIN
      INSERT INTO messages_fts(rowid, text) VALUES (new.id, new.text);
    END;
    CREATE TRIGGER IF NOT EXISTS messages_ad AFTER DELETE ON messages BEGIN
      INSERT INTO messages_fts(messages_fts, rowid, text) VALUES('delete', old.id, old.text);
    END;
    CREATE TRIGGER IF NOT EXISTS messages_au AFTER UPDATE ON messages BEGIN
      INSERT INTO messages_fts(messages_fts, rowid, text) VALUES('delete', old.id, old.text);
      INSERT INTO messages_fts(rowid, text) VALUES (new.id, new.text);
    END;
  `);
} catch {}

// Indexes
try { db.exec('CREATE INDEX IF NOT EXISTS idx_messages_channel_ts ON messages(channel, ts)'); } catch {}
try { db.exec('CREATE INDEX IF NOT EXISTS idx_messages_thread ON messages(thread_id) WHERE thread_id IS NOT NULL'); } catch {}
try { db.exec('CREATE INDEX IF NOT EXISTS idx_reactions_message ON reactions(message_id)'); } catch {}
try { db.exec('CREATE INDEX IF NOT EXISTS idx_files_channel ON files(channel)'); } catch {}

// Phase 5.5: Pinned messages table
db.exec(`
  CREATE TABLE IF NOT EXISTS pins (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    channel_id TEXT NOT NULL,
    message_id INTEGER NOT NULL,
    pinned_by TEXT,
    pinned_at INTEGER DEFAULT (strftime('%s','now') * 1000),
    UNIQUE(channel_id, message_id)
  )
`);
try { db.exec('CREATE INDEX IF NOT EXISTS idx_pins_channel ON pins(channel_id)'); } catch {}

// Seed default channels if the channels table is empty
const channelCount = db.prepare('SELECT COUNT(*) as cnt FROM channels').get();
if (channelCount.cnt === 0) {
  const seedChannels = [
    { id: 'general', name: 'General', description: 'General discussion' },
    { id: 'agents', name: 'Agents', description: 'Agent coordination' },
    { id: 'ops', name: 'Ops', description: 'Operations & infra' },
    { id: 'random', name: 'Random', description: 'Off-topic' },
  ];
  const insertCh = db.prepare('INSERT OR IGNORE INTO channels (id, name, description) VALUES (?, ?, ?)');
  for (const ch of seedChannels) insertCh.run(ch.id, ch.name, ch.description);
  console.log('[squirrelchat] Seeded default channels');
}

// Ensure #worklog channel always exists (even on existing DBs that skipped seeding)
db.prepare('INSERT OR IGNORE INTO channels (id, name, description) VALUES (?, ?, ?)')
  .run('worklog', 'Worklog', 'Agent task completion journal — auto-posted by workqueue.completed');

// Seed default agent users
const agentSeeds = ['rocky', 'bullwinkle', 'natasha', 'boris', 'sparky', 'peabody', 'snidely', 'sherman', 'dudley'];
const insertUser = db.prepare('INSERT OR IGNORE INTO users (id, name, role) VALUES (?, ?, ?)');
for (const a of agentSeeds) insertUser.run(a, a.charAt(0).toUpperCase() + a.slice(1), 'agent');
// Seed human users
insertUser.run('jkh', 'jkh', 'admin');

// SSE clients
const sseClients = new Set();

// Multer for multipart/form-data (voice uploads)
const upload = multer({ storage: multer.memoryStorage(), limits: { fileSize: 25 * 1024 * 1024 } });

// ElevenLabs voice IDs
const EL_VOICES = {
  Rachel: '21m00Tcm4TlvDq8ikWAM',
  Domi: 'AZnzlk1XvdvUeBnXmlld',
  Bella: 'EXAVITQu4vr4xnSDxMaL',
  Josh: 'TxGEqnHWrfWFTfGW9XjX',
};

// Middleware
app.use(express.json({ limit: '10mb' }));
app.use((req, res, next) => {
  res.header('Access-Control-Allow-Origin', '*');
  res.header('Access-Control-Allow-Methods', 'GET,POST,PATCH,DELETE,OPTIONS');
  res.header('Access-Control-Allow-Headers', 'Content-Type,Authorization');
  if (req.method === 'OPTIONS') return res.sendStatus(204);
  next();
});
app.use(express.static(join(__dirname, 'public')));

// Auth middleware — accepts admin token OR any valid sc-token-<id>
function auth(req, res, next) {
  const token = (req.headers.authorization || '').replace('Bearer ', '').trim();
  if (!token) return res.status(401).json({ error: 'Unauthorized' });
  if (token === ADMIN_TOKEN) { req.userId = 'admin'; return next(); }
  // sc-token-<user_id> format
  const match = token.match(/^sc-token-(.+)$/);
  if (match) {
    const userId = match[1];
    const user = db.prepare('SELECT id FROM users WHERE id = ?').get(userId);
    if (user) { req.userId = userId; return next(); }
    // Auto-create user on first login (new name-claim token)
    db.prepare('INSERT OR IGNORE INTO users (id, name, role) VALUES (?, ?, \'user\')').run(userId, userId);
    req.userId = userId;
    return next();
  }
  return res.status(401).json({ error: 'Unauthorized' });
}

// Helper: fetch from RCC
async function rccFetch(path, options = {}) {
  return new Promise((resolve, reject) => {
    const url = new URL(RCC_BASE + path);
    const reqOptions = {
      hostname: url.hostname,
      port: url.port || 80,
      path: url.pathname + url.search,
      method: options.method || 'GET',
      headers: {
        'Content-Type': 'application/json',
        'Authorization': `Bearer ${RCC_AGENT_TOKEN}`,
        ...(options.headers || {}),
      },
    };
    const req = fetch.request(reqOptions, (res) => {
      let data = '';
      res.on('data', chunk => data += chunk);
      res.on('end', () => {
        try { resolve({ ok: res.statusCode < 400, status: res.statusCode, data: JSON.parse(data) }); }
        catch { resolve({ ok: false, status: res.statusCode, data }); }
      });
    });
    req.on('error', reject);
    if (options.body) req.write(typeof options.body === 'string' ? options.body : JSON.stringify(options.body));
    req.end();
  });
}

// Known fleet agents for @mention routing
const FLEET_AGENTS = ['rocky', 'natasha', 'boris', 'peabody', 'sherman', 'dudley', 'snidely', 'bullwinkle'];

// @agent mention handler — detects @<agentname> <task> and posts a workqueue item
// Returns { agentName, wqId, title } if a queue item was created, else null.
async function handleAgentMention(from, text) {
  const mentionRe = /^@([a-z][a-z0-9_-]*)\s+(.+)/is;
  const match = text.trim().match(mentionRe);
  if (!match) return null;

  const agentName = match[1].toLowerCase();
  if (!FLEET_AGENTS.includes(agentName)) return null;

  const taskTitle = match[2].trim();
  const wqId = `wq-SC-${Date.now()}`;

  try {
    const r = await rccFetch('/api/queue', {
      method: 'POST',
      body: {
        id: wqId,
        title: taskTitle,
        description: `Delegated via SquirrelChat @mention by ${from}`,
        assignee: agentName,
        source: 'squirrelchat',
        priority: 'normal',
        tags: ['squirrelchat', 'mention', `from:${from}`],
      },
    });
    if (r.ok) {
      return { agentName, wqId: r.data?.id || wqId, title: taskTitle };
    }
    console.warn('[squirrelchat] @mention queue create failed:', r.status, r.data);
    return null;
  } catch (err) {
    console.warn('[squirrelchat] @mention queue create error:', err.message);
    return null;
  }
}

// Slash command handler
async function handleSlashCommand(text) {
  const parts = text.trim().split(/\s+/);
  const cmd = parts[0];
  const sub = parts[1];

  if (cmd === '/task') {
    if (sub === 'list') {
      try {
        const r = await rccFetch('/api/queue?status=pending');
        if (r.ok) {
          const tasks = r.data;
          if (!tasks || tasks.length === 0) return 'No pending tasks.';
          return tasks.map(t => `• [${t.id}] ${t.title || t.text || JSON.stringify(t)}`).join('\n');
        }
      } catch {}
      return 'Could not fetch tasks from RCC.';
    }
    if (sub === 'create') {
      const title = parts.slice(2).join(' ');
      if (!title) return 'Usage: /task create <title>';
      try {
        const r = await rccFetch('/api/queue', { method: 'POST', body: { title, source: 'squirrelchat' } });
        if (r.ok) return `Task created: ${title}`;
      } catch {}
      return 'Could not create task in RCC.';
    }
    return 'Usage: /task list | /task create <title>';
  }

  if (cmd === '/project') {
    if (sub === 'list') {
      const projects = db.prepare('SELECT id, name, status FROM projects ORDER BY created_at DESC').all();
      if (!projects.length) return 'No projects yet.';
      return projects.map(p => `• [${p.id}] ${p.name} (${p.status})`).join('\n');
    }
    if (sub === 'create') {
      const rest = parts.slice(2);
      const name = rest[0] || 'Unnamed';
      const desc = rest.slice(1).join(' ') || '';
      const id = name.toLowerCase().replace(/[^a-z0-9]+/g, '-') + '-' + Date.now().toString(36);
      db.prepare('INSERT INTO projects (id, name, description) VALUES (?, ?, ?)').run(id, name, desc);
      return `Project created: ${name} (id: ${id})`;
    }
    return 'Usage: /project list | /project create <name> [desc]';
  }

  return null;
}

// === ROUTES ===

// Health
app.get('/health', (req, res) => res.json({ ok: true }));

// Channels (DB-backed)
app.get('/api/channels', (req, res) => {
  const rows = db.prepare('SELECT * FROM channels ORDER BY created_at ASC').all();
  const channels = rows.map(ch => ({
    ...ch,
    participants: ch.participants ? JSON.parse(ch.participants) : undefined,
  }));
  res.json(channels);
});

app.post('/api/channels', auth, (req, res) => {
  const { id, name, description, type = 'channel', participants } = req.body;
  if (!id || !name) return res.status(400).json({ error: 'id and name required' });
  const slug = id.toLowerCase().replace(/[^a-z0-9-]+/g, '-');
  db.prepare('INSERT INTO channels (id, name, description, type, participants) VALUES (?, ?, ?, ?, ?)')
    .run(slug, name, description || null, type, participants ? JSON.stringify(participants) : null);
  const channel = db.prepare('SELECT * FROM channels WHERE id = ?').get(slug);
  // Broadcast channel creation via SSE
  const payload = JSON.stringify({ type: 'channel_create', data: channel });
  for (const client of sseClients) client.write(`data: ${payload}\n\n`);
  res.json({ ok: true, channel });
});

app.get('/api/channels/:id', (req, res) => {
  const ch = db.prepare('SELECT * FROM channels WHERE id = ?').get(req.params.id);
  if (!ch) return res.status(404).json({ error: 'Not found' });
  if (ch.participants) ch.participants = JSON.parse(ch.participants);
  res.json(ch);
});

app.patch('/api/channels/:id', auth, (req, res) => {
  const ch = db.prepare('SELECT * FROM channels WHERE id = ?').get(req.params.id);
  if (!ch) return res.status(404).json({ error: 'Not found' });
  const { name, description } = req.body;
  db.prepare('UPDATE channels SET name = COALESCE(?, name), description = COALESCE(?, description) WHERE id = ?')
    .run(name || null, description || null, req.params.id);
  res.json({ ok: true });
});

// Current user identity
app.get('/api/me', (req, res) => {
  const token = (req.headers.authorization || '').replace('Bearer ', '').trim();
  // Admin token — return admin identity
  if (token === ADMIN_TOKEN) {
    return res.json({ id: 'admin', name: 'Admin', role: 'admin', needs_name: false });
  }
  // Check if token matches an agent/user token (format: sc-token-<user_id>)
  const match = token.match(/^sc-token-(.+)$/);
  if (match) {
    const userId = match[1];
    const user = db.prepare('SELECT id, name, role, COALESCE(avatar_url, \'\') as avatar_url FROM users WHERE id = ?').get(userId);
    if (user) {
      return res.json({ ...user, needs_name: false });
    }
    // Unknown token but valid format — auto-create as guest
    db.prepare('INSERT OR IGNORE INTO users (id, name, role) VALUES (?, ?, \'user\')').run(userId, userId);
    return res.json({ id: userId, name: userId, role: 'user', needs_name: false });
  }
  // No token or unknown — guest with set-name hint
  const name = req.query.name || null;
  res.json({ id: name || 'anonymous', name: name || 'anonymous', role: 'user', needs_name: !name });
});

// Login — name-claim: POST { name } → { token, user }
// For jkh or other humans: claim a display name, get a persistent token
app.post('/api/login', (req, res) => {
  const { name } = req.body;
  if (!name) return res.status(400).json({ error: 'name required' });
  const id = name.toLowerCase().replace(/[^a-z0-9_-]/g, '-');
  const token = `sc-token-${id}`;
  // Upsert user — preserve existing role if already in DB
  const existing = db.prepare('SELECT id, name, role FROM users WHERE id = ?').get(id);
  if (existing) {
    db.prepare('UPDATE users SET name = ?, last_seen = ? WHERE id = ?').run(name, Date.now(), id);
    _userNameCache[id] = name;
    return res.json({ ok: true, token, user: { id, name, role: existing.role } });
  }
  db.prepare('INSERT INTO users (id, name, role) VALUES (?, ?, \'user\')').run(id, name);
  _userNameCache[id] = name;
  res.json({ ok: true, token, user: { id, name, role: 'user' } });
});

// Helper: build reactions map for a message (emoji → [user_ids])
function getReactionsMap(messageId) {
  // Support both old (from_agent) and new (user_id) schemas
  const rows = db.prepare('SELECT emoji, COALESCE(user_id, from_agent) as uid FROM reactions WHERE message_id = ? ORDER BY created_at ASC').all(messageId);
  const map = {};
  for (const r of rows) {
    if (!map[r.emoji]) map[r.emoji] = [];
    if (r.uid) map[r.emoji].push(r.uid);
  }
  return map;
}

// Helper: get thread reply count for a message
function getThreadCount(messageId) {
  const row = db.prepare('SELECT COUNT(*) as cnt FROM messages WHERE thread_id = ?').get(messageId);
  return row ? row.cnt : 0;
}

// User name cache (populated lazily)
const _userNameCache = {};
function getUserName(id) {
  if (!id) return id;
  if (_userNameCache[id]) return _userNameCache[id];
  const u = db.prepare('SELECT name FROM users WHERE id = ?').get(id);
  if (u) { _userNameCache[id] = u.name; return u.name; }
  return id;
}

// Helper: format a message row for the wire
function formatMessage(r) {
  const fromAgent = r.from_agent;
  return {
    id: r.id,
    ts: r.ts,
    from: fromAgent,
    from_name: getUserName(fromAgent),
    text: r.text,
    channel: r.channel,
    thread_id: r.thread_id || null,
    thread_count: r.thread_id ? 0 : getThreadCount(r.id),
    mentions: r.mentions ? JSON.parse(r.mentions) : [],
    reactions: getReactionsMap(r.id),
    edited_at: r.edited_at || null,
    created_at: r.created_at,
    slash_result: r.slash_result || null,
  };
}

// Messages
app.get('/api/messages', (req, res) => {
  const since = parseInt(req.query.since) || 0;
  const before = parseInt(req.query.before) || Infinity;
  const channel = req.query.channel || 'general';
  const threadId = req.query.thread_id || null;
  const limit = Math.min(parseInt(req.query.limit) || 50, 200);

  let query, params;
  if (threadId) {
    // Thread replies
    query = 'SELECT * FROM messages WHERE thread_id = ? ORDER BY ts ASC LIMIT ?';
    params = [parseInt(threadId), limit];
  } else if (channel === 'all') {
    query = 'SELECT * FROM messages WHERE ts > ? AND ts < ? AND thread_id IS NULL ORDER BY ts DESC LIMIT ?';
    params = [since, before === Infinity ? 9999999999999 : before, limit];
  } else {
    query = 'SELECT * FROM messages WHERE ts > ? AND ts < ? AND channel = ? AND thread_id IS NULL ORDER BY ts DESC LIMIT ?';
    params = [since, before === Infinity ? 9999999999999 : before, channel, limit];
  }

  const rows = db.prepare(query).all(...params);
  // Reverse DESC results so they're chronological (thread queries are already ASC)
  const ordered = threadId ? rows : rows.reverse();
  const messages = ordered.map(formatMessage);
  res.json(messages);
});

app.post('/api/messages', auth, async (req, res) => {
  const { from, text, channel = 'general', mentions, thread_id } = req.body;
  if (!from || !text) return res.status(400).json({ error: 'from and text required' });

  const ts = Date.now();
  const stmt = db.prepare('INSERT INTO messages (ts, from_agent, text, channel, mentions, thread_id) VALUES (?, ?, ?, ?, ?, ?)');
  const r = stmt.run(ts, from, text, channel, mentions ? JSON.stringify(mentions) : null, thread_id || null);
  const message = formatMessage({ id: r.lastInsertRowid, ts, from_agent: from, text, channel, mentions: mentions ? JSON.stringify(mentions) : null, thread_id: thread_id || null, edited_at: null, created_at: ts, slash_result: null });

  // Update channel last_message_at
  db.prepare('UPDATE channels SET last_message_at = ? WHERE id = ?').run(ts, channel);

  // Broadcast via SSE
  const payload = JSON.stringify({ type: 'message', data: message });
  for (const client of sseClients) {
    client.write(`data: ${payload}\n\n`);
  }

  // ── @agent mention → workqueue item ───────────────────────────────────────
  // Pattern: @agentname <task description>
  // Known agents: natasha, rocky, boris, bullwinkle, peabody, sherman, snidely, dudley
  const KNOWN_AGENTS = ['natasha', 'rocky', 'boris', 'bullwinkle', 'peabody', 'sherman', 'snidely', 'dudley'];
  const mentionMatch = text.match(/^@([a-z]+)\s+(.+)$/si);
  if (mentionMatch) {
    const mentionedAgent = mentionMatch[1].toLowerCase();
    const taskText = mentionMatch[2].trim();
    if (KNOWN_AGENTS.includes(mentionedAgent) && taskText.length >= 3) {
      // Create a workqueue item assigned to the mentioned agent
      try {
        const wqRes = await rccFetch('/api/queue', {
          method: 'POST',
          body: {
            title: taskText.slice(0, 120),
            description: `From ${from} in #${channel} via SquirrelChat @mention.\n\nOriginal: ${text}`,
            assignee: mentionedAgent,
            priority: 'normal',
            source: 'squirrelchat',
            tags: ['squirrelchat', 'mention'],
          },
        });
        if (wqRes.ok !== false && (wqRes.id || wqRes.item?.id)) {
          const itemId = wqRes.id || wqRes.item?.id;
          const bts = Date.now();
          const botText = `📋 Created task for @${mentionedAgent}: **${taskText.slice(0, 80)}**\n\`${itemId}\``;
          const br = db.prepare('INSERT INTO messages (ts, from_agent, text, channel) VALUES (?, ?, ?, ?)').run(
            bts, 'squirrelbot', botText, channel
          );
          const mentionReply = formatMessage({ id: br.lastInsertRowid, ts: bts, from_agent: 'squirrelbot', text: botText, channel, mentions: null, thread_id: null, edited_at: null, created_at: bts, slash_result: null });
          const mp = JSON.stringify({ type: 'message', data: mentionReply });
          for (const client of sseClients) client.write(`data: ${mp}\n\n`);
        }
      } catch (err) {
        console.warn('[squirrelchat] @mention wq create error:', err.message);
      }
    }
  }

  // Handle slash commands
  let botReply = null;
  const trimmed = text.trim();
  if (trimmed.startsWith('/')) {
    const slashResult = await handleSlashCommand(trimmed);
    if (slashResult !== null) {
      const bts = Date.now();
      const br = db.prepare('INSERT INTO messages (ts, from_agent, text, channel, slash_result) VALUES (?, ?, ?, ?, ?)').run(
        bts, 'squirrelbot', slashResult, channel, trimmed
      );
      botReply = formatMessage({ id: br.lastInsertRowid, ts: bts, from_agent: 'squirrelbot', text: slashResult, channel, mentions: null, thread_id: null, edited_at: null, created_at: bts, slash_result: trimmed });

      // Update original message with slash_result
      db.prepare('UPDATE messages SET slash_result = ? WHERE id = ?').run(trimmed, r.lastInsertRowid);

      const botPayload = JSON.stringify({ type: 'message', data: botReply });
      for (const client of sseClients) {
        client.write(`data: ${botPayload}\n\n`);
      }
    }
  }

  // Handle @agent mentions — delegate task to named agent via RCC workqueue
  if (!botReply && trimmed.startsWith('@')) {
    const mention = await handleAgentMention(from, trimmed);
    if (mention) {
      const bts = Date.now();
      const replyText = `📬 Task queued for @${mention.agentName}: "${mention.title}" (${mention.wqId})`;
      const br = db.prepare('INSERT INTO messages (ts, from_agent, text, channel) VALUES (?, ?, ?, ?)').run(
        bts, 'squirrelbot', replyText, channel
      );
      botReply = formatMessage({ id: br.lastInsertRowid, ts: bts, from_agent: 'squirrelbot', text: replyText, channel, mentions: null, thread_id: null, edited_at: null, created_at: bts, slash_result: null });
      const botPayload = JSON.stringify({ type: 'message', data: botReply });
      for (const client of sseClients) {
        client.write(`data: ${botPayload}\n\n`);
      }
    }
  }

  // Async RAG ingest — fire and forget, never fail the request
  ingestMessage({ id: r.lastInsertRowid, ts, from_agent: from, text, channel }).catch(err =>
    console.warn('[squirrelchat] ingest failed:', err.message)
  );

  res.json({ ok: true, message, botReply });
});

// Edit message
app.patch('/api/messages/:id', auth, (req, res) => {
  const { text } = req.body;
  if (!text) return res.status(400).json({ error: 'text required' });
  const msg = db.prepare('SELECT * FROM messages WHERE id = ?').get(req.params.id);
  if (!msg) return res.status(404).json({ error: 'Not found' });
  const editedAt = Date.now();
  db.prepare('UPDATE messages SET text = ?, edited_at = ? WHERE id = ?').run(text, editedAt, req.params.id);
  // Broadcast edit via SSE
  const payload = JSON.stringify({ type: 'message_edit', data: { id: String(msg.id), text, edited_at: editedAt } });
  for (const client of sseClients) client.write(`data: ${payload}\n\n`);
  res.json({ ok: true });
});

// Delete message
app.delete('/api/messages/:id', auth, (req, res) => {
  const msg = db.prepare('SELECT * FROM messages WHERE id = ?').get(req.params.id);
  if (!msg) return res.status(404).json({ error: 'Not found' });
  db.prepare('DELETE FROM reactions WHERE message_id = ?').run(req.params.id);
  db.prepare('DELETE FROM messages WHERE id = ?').run(req.params.id);
  // Broadcast delete via SSE
  const payload = JSON.stringify({ type: 'message_delete', data: { id: String(msg.id) } });
  for (const client of sseClients) client.write(`data: ${payload}\n\n`);
  res.json({ ok: true });
});

// Reactions — toggle
app.post('/api/messages/:id/react', auth, (req, res) => {
  const { emoji, user_id } = req.body;
  if (!emoji || !user_id) return res.status(400).json({ error: 'emoji and user_id required' });
  const msgId = parseInt(req.params.id);
  const msg = db.prepare('SELECT id FROM messages WHERE id = ?').get(msgId);
  if (!msg) return res.status(404).json({ error: 'Message not found' });

  const existing = db.prepare('SELECT 1 FROM reactions WHERE message_id = ? AND user_id = ? AND emoji = ?').get(msgId, user_id, emoji);
  let action;
  if (existing) {
    db.prepare('DELETE FROM reactions WHERE message_id = ? AND user_id = ? AND emoji = ?').run(msgId, user_id, emoji);
    action = 'removed';
  } else {
    db.prepare('INSERT INTO reactions (message_id, user_id, emoji) VALUES (?, ?, ?)').run(msgId, user_id, emoji);
    action = 'added';
  }

  const reactions = getReactionsMap(msgId);
  // Broadcast reaction event via SSE
  const payload = JSON.stringify({ type: 'reaction', data: { message_id: String(msgId), emoji, user: user_id, action } });
  for (const client of sseClients) client.write(`data: ${payload}\n\n`);
  res.json({ ok: true, action, reactions });
});

// Get reactions for a message
app.get('/api/messages/:id/reactions', (req, res) => {
  const reactions = getReactionsMap(parseInt(req.params.id));
  res.json({ reactions });
});

// Users
app.get('/api/users', (req, res) => {
  const users = db.prepare('SELECT id, name, COALESCE(role, \'user\') as role, COALESCE(avatar_url, \'\') as avatar_url, status, last_seen FROM users ORDER BY name ASC').all();
  res.json(users);
});

app.post('/api/users/register', auth, (req, res) => {
  const { id, name, avatar_url, role = 'user' } = req.body;
  if (!id || !name) return res.status(400).json({ error: 'id and name required' });
  try {
    db.prepare('INSERT OR REPLACE INTO users (id, name, role, avatar_url) VALUES (?, ?, ?, ?)').run(id, name, role, avatar_url || null);
    _userNameCache[id] = name;
  } catch {
    // Fallback if avatar_url col doesn't exist yet
    db.prepare('INSERT OR REPLACE INTO users (id, name, role) VALUES (?, ?, ?)').run(id, name, role);
    _userNameCache[id] = name;
  }
  res.json({ ok: true });
});

app.post('/api/users/presence', auth, (req, res) => {
  const { user_id, status = 'online' } = req.body;
  if (!user_id) return res.status(400).json({ error: 'user_id required' });
  db.prepare('UPDATE users SET status = ?, last_seen = ? WHERE id = ?').run(status, Date.now(), user_id);
  // Broadcast presence via SSE
  const payload = JSON.stringify({ type: 'presence', data: { user: user_id, status } });
  for (const client of sseClients) client.write(`data: ${payload}\n\n`);
  res.json({ ok: true });
});

// Search (FTS5)
app.get('/api/search', (req, res) => {
  const q = req.query.q;
  if (!q) return res.status(400).json({ error: 'q parameter required' });
  const channel = req.query.channel || null;
  const limit = Math.min(parseInt(req.query.limit) || 20, 100);

  let rows;
  if (channel) {
    rows = db.prepare(`
      SELECT m.*, rank FROM messages m
      JOIN messages_fts ON messages_fts.rowid = m.id
      WHERE messages_fts MATCH ? AND m.channel = ?
      ORDER BY rank LIMIT ?
    `).all(q, channel, limit);
  } else {
    rows = db.prepare(`
      SELECT m.*, rank FROM messages m
      JOIN messages_fts ON messages_fts.rowid = m.id
      WHERE messages_fts MATCH ?
      ORDER BY rank LIMIT ?
    `).all(q, limit);
  }
  const results = rows.map(r => ({ ...formatMessage(r), score: r.rank }));
  res.json(results);
});

// Projects
app.get('/api/projects', (req, res) => {
  const projects = db.prepare('SELECT * FROM projects ORDER BY created_at DESC').all().map(p => ({
    ...p, tags: p.tags ? JSON.parse(p.tags) : []
  }));
  res.json(projects);
});

app.post('/api/projects', auth, (req, res) => {
  const { name, description, tags, assignee, status = 'active' } = req.body;
  if (!name) return res.status(400).json({ error: 'name required' });
  const id = name.toLowerCase().replace(/[^a-z0-9]+/g, '-') + '-' + Date.now().toString(36);
  db.prepare('INSERT INTO projects (id, name, description, tags, assignee, status) VALUES (?, ?, ?, ?, ?, ?)')
    .run(id, name, description || '', tags ? JSON.stringify(tags) : null, assignee || null, status);
  res.json({ ok: true, id });
});

app.get('/api/projects/:id', (req, res) => {
  const p = db.prepare('SELECT * FROM projects WHERE id = ?').get(req.params.id);
  if (!p) return res.status(404).json({ error: 'Not found' });
  res.json({ ...p, tags: p.tags ? JSON.parse(p.tags) : [] });
});

app.patch('/api/projects/:id', auth, (req, res) => {
  const { name, description, tags, assignee, status } = req.body;
  const p = db.prepare('SELECT * FROM projects WHERE id = ?').get(req.params.id);
  if (!p) return res.status(404).json({ error: 'Not found' });
  const updated = {
    name: name ?? p.name,
    description: description ?? p.description,
    tags: tags !== undefined ? JSON.stringify(tags) : p.tags,
    assignee: assignee ?? p.assignee,
    status: status ?? p.status,
    updated_at: Date.now(),
  };
  db.prepare('UPDATE projects SET name=?, description=?, tags=?, assignee=?, status=?, updated_at=? WHERE id=?')
    .run(updated.name, updated.description, updated.tags, updated.assignee, updated.status, updated.updated_at, req.params.id);
  res.json({ ok: true });
});

app.delete('/api/projects/:id', auth, (req, res) => {
  db.prepare('DELETE FROM projects WHERE id = ?').run(req.params.id);
  db.prepare('DELETE FROM project_files WHERE project_id = ?').run(req.params.id);
  res.json({ ok: true });
});

// Project files
app.get('/api/projects/:id/files', (req, res) => {
  const files = db.prepare('SELECT id, project_id, filename, encoding, size, created_at FROM project_files WHERE project_id = ?').all(req.params.id);
  res.json(files);
});

app.post('/api/projects/:id/files', auth, (req, res) => {
  const { filename, content, encoding = 'utf8' } = req.body;
  if (!filename || content === undefined) return res.status(400).json({ error: 'filename and content required' });
  const buf = encoding === 'base64' ? Buffer.from(content, 'base64') : Buffer.from(content, 'utf8');
  db.prepare('INSERT OR REPLACE INTO project_files (project_id, filename, content, encoding, size) VALUES (?, ?, ?, ?, ?)')
    .run(req.params.id, filename, buf, encoding, buf.length);
  res.json({ ok: true });
});

app.get('/api/projects/:id/files/:filename', (req, res) => {
  const file = db.prepare('SELECT * FROM project_files WHERE project_id = ? AND filename = ?').get(req.params.id, req.params.filename);
  if (!file) return res.status(404).json({ error: 'Not found' });
  res.set('Content-Disposition', `attachment; filename="${file.filename}"`);
  res.send(file.content);
});

// Project download as tar.gz
app.get('/api/projects/:id/download', (req, res) => {
  const project = db.prepare('SELECT * FROM projects WHERE id = ?').get(req.params.id);
  if (!project) return res.status(404).json({ error: 'Not found' });
  const files = db.prepare('SELECT * FROM project_files WHERE project_id = ?').all(req.params.id);

  res.set('Content-Type', 'application/gzip');
  res.set('Content-Disposition', `attachment; filename="${project.id}.tar.gz"`);

  const archive = archiver('tar', { gzip: true });
  archive.pipe(res);
  for (const f of files) {
    archive.append(f.content, { name: f.filename });
  }
  archive.finalize();
});

// ── Agent status / activity feed ───────────────────────────────────────────

// In-memory store: last 5 freeform status posts per agent
const agentStatusStore = {};

// GET /api/agent-status — proxy RCC activity-feed (public)
app.get('/api/agent-status', async (req, res) => {
  try {
    const r = await rccFetch('/api/queue/activity-feed');
    if (r.ok && r.data?.agents) {
      // Merge freeform status text if available
      const agents = r.data.agents.map(a => ({
        ...a,
        freeStatus: (agentStatusStore[a.name] || []).slice(-1)[0] || null,
      }));
      return res.json({ ok: true, agents, ts: r.data.ts });
    }
  } catch (err) {
    console.warn('[agent-status] RCC fetch error:', err.message);
  }
  // Graceful degradation — return unknown status for all known agents
  const KNOWN = ['natasha','rocky','boris','bullwinkle','peabody','sherman','snidely','dudley'];
  return res.json({
    ok: false,
    agents: KNOWN.map(name => ({ name, status: 'unknown', currentTask: null, lastSeen: null })),
  });
});

// POST /api/agent-status — agents push freeform status text
app.post('/api/agent-status', async (req, res) => {
  const { agent, text, status } = req.body || {};
  if (!agent) return res.status(400).json({ error: 'agent required' });
  if (!agentStatusStore[agent]) agentStatusStore[agent] = [];
  agentStatusStore[agent].push({ text: text || '', status: status || 'online', ts: new Date().toISOString() });
  if (agentStatusStore[agent].length > 5) agentStatusStore[agent].shift();
  return res.json({ ok: true });
});

// Agents (proxy RCC heartbeats)
app.get('/api/agents', async (req, res) => {
  try {
    const r = await rccFetch('/api/heartbeats');
    if (r.ok) {
      const now = Date.now();
      const agents = (Array.isArray(r.data) ? r.data : Object.values(r.data || {})).map(a => ({
        ...a,
        online: a.ts ? (now - a.ts) < 5 * 60 * 1000 : false,
      }));
      return res.json(agents);
    }
  } catch {}
  res.json([]);
});

// ── Phase 5.5: Pinned messages ─────────────────────────────────────────────

// GET all pins for a channel (returns full message objects)
app.get('/api/channels/:id/pins', (req, res) => {
  const pins = db.prepare(`
    SELECT m.* FROM messages m
    INNER JOIN pins p ON p.message_id = m.id
    WHERE p.channel_id = ?
    ORDER BY p.pinned_at DESC
  `).all(req.params.id);
  res.json(pins.map(m => ({
    id: String(m.id),
    channel: m.channel,
    from_agent: m.from_agent,
    text: m.text,
    ts: m.ts,
    edited_at: m.edited_at,
  })));
});

// POST pin a message
app.post('/api/channels/:id/pins/:msgId', auth, (req, res) => {
  const { id: channelId, msgId } = req.params;
  const pinned_by = req.body?.from || 'unknown';
  try {
    db.prepare('INSERT OR IGNORE INTO pins (channel_id, message_id, pinned_by) VALUES (?, ?, ?)').run(channelId, msgId, pinned_by);
    res.json({ ok: true });
  } catch (e) {
    res.status(500).json({ error: e.message });
  }
});

// DELETE unpin a message
app.delete('/api/channels/:id/pins/:msgId', auth, (req, res) => {
  db.prepare('DELETE FROM pins WHERE channel_id = ? AND message_id = ?').run(req.params.id, req.params.msgId);
  res.json({ ok: true });
});

// ── Voice: STT transcription proxy ────────────────────────────────────────────

app.post('/api/voice/transcribe', upload.single('audio'), async (req, res) => {
  if (!req.file) return res.status(400).json({ error: 'audio file required (field: audio)' });

  try {
    const formData = new FormData();
    const mimeType = req.file.mimetype || 'audio/webm';
    formData.append('file', new Blob([req.file.buffer], { type: mimeType }), req.file.originalname || 'audio.webm');

    const whisperRes = await fetch('http://sparky.tail407856.ts.net:8792/inference', {
      method: 'POST',
      body: formData,
      signal: AbortSignal.timeout(30000),
    });

    if (!whisperRes.ok) {
      return res.status(503).json({ error: 'STT unavailable', reason: `Whisper returned ${whisperRes.status}` });
    }

    const data = await whisperRes.json();
    return res.json({ text: data.text || '' });
  } catch (err) {
    console.warn('[voice/transcribe] error:', err.message);
    return res.status(503).json({ error: 'STT unavailable', reason: err.message });
  }
});

// ── Voice: TTS synthesis ───────────────────────────────────────────────────────

app.post('/api/voice/synthesize', async (req, res) => {
  const { text, voice = 'Rachel' } = req.body;
  if (!text) return res.status(400).json({ error: 'text required' });

  const apiKey = process.env.ELEVENLABS_API_KEY;

  // Try ElevenLabs first
  if (apiKey) {
    try {
      const voiceId = EL_VOICES[voice] || EL_VOICES.Rachel;
      const elRes = await fetch(`https://api.elevenlabs.io/v1/text-to-speech/${voiceId}`, {
        method: 'POST',
        headers: {
          'xi-api-key': apiKey,
          'Content-Type': 'application/json',
          'Accept': 'audio/mpeg',
        },
        body: JSON.stringify({
          text,
          model_id: 'eleven_monolingual_v1',
          voice_settings: { stability: 0.5, similarity_boost: 0.5 },
        }),
        signal: AbortSignal.timeout(30000),
      });

      if (elRes.ok) {
        res.set('Content-Type', 'audio/mpeg');
        return res.send(Buffer.from(await elRes.arrayBuffer()));
      }
      console.warn('[voice/synthesize] ElevenLabs returned', elRes.status);
    } catch (err) {
      console.warn('[voice/synthesize] ElevenLabs error:', err.message);
    }
  }

  // Fallback: sparky piper TTS
  try {
    const piperRes = await fetch('http://sparky.tail407856.ts.net:8792/tts', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ text }),
      signal: AbortSignal.timeout(30000),
    });

    if (piperRes.ok) {
      const contentType = piperRes.headers.get('content-type') || 'audio/wav';
      res.set('Content-Type', contentType);
      return res.send(Buffer.from(await piperRes.arrayBuffer()));
    }
    console.warn('[voice/synthesize] piper returned', piperRes.status);
  } catch (err) {
    console.warn('[voice/synthesize] piper error:', err.message);
  }

  return res.status(404).json({ error: 'TTS unavailable', reason: 'No TTS backend reachable' });
});

// ── #worklog: auto-post agent task completions ──────────────────────────────
//
// Polls the RCC queue for newly completed items every 30 seconds.
// When a completion is detected, posts a formatted entry to #worklog.
// Posts are prefixed with a daily date header (posted once per day).
//
// Message format:
//   ✅ **agent** — task title
//   > result summary (if present)
//   `commit-hash`  (if present in result)

const WORKLOG_CHANNEL = 'worklog';
const WORKLOG_POLL_MS = 30_000;

// Track highest completed item timestamp seen so we don't re-post
let worklogLastSeenTs = Date.now();
let worklogLastDateHeader = '';

function extractCommitHash(text) {
  if (!text) return null;
  const m = text.match(/\b([0-9a-f]{7,40})\b/);
  return m ? m[1] : null;
}

function worklogDateHeader() {
  return new Date().toISOString().slice(0, 10); // YYYY-MM-DD
}

async function ensureWorklogChannel() {
  try {
    const r = await rccFetch('/api/channels');
    // Check if worklog channel exists in squirrelchat DB
    const existing = db.prepare('SELECT id FROM channels WHERE id = ?').get(WORKLOG_CHANNEL);
    if (!existing) {
      db.prepare('INSERT OR IGNORE INTO channels (id, name, description, type) VALUES (?, ?, ?, ?)')
        .run(WORKLOG_CHANNEL, '#worklog', 'Automated fleet build journal — task completions', 'channel');
      console.log('[worklog] Created #worklog channel');
    }
  } catch (err) {
    console.warn('[worklog] ensureWorklogChannel error:', err.message);
  }
}

async function postWorklogMessage(text) {
  const ts = Date.now();
  const stmt = db.prepare(
    'INSERT INTO messages (ts, from_agent, text, channel) VALUES (?, ?, ?, ?)'
  );
  const r = stmt.run(ts, 'worklogbot', text, WORKLOG_CHANNEL);
  db.prepare('UPDATE channels SET last_message_at = ? WHERE id = ?').run(ts, WORKLOG_CHANNEL);
  const message = formatMessage({
    id: r.lastInsertRowid, ts, from_agent: 'worklogbot', text,
    channel: WORKLOG_CHANNEL, mentions: null, thread_id: null,
    edited_at: null, created_at: ts, slash_result: null,
  });
  const payload = JSON.stringify({ type: 'message', data: message });
  for (const client of sseClients) client.write(`data: ${payload}\n\n`);
}

async function pollWorklog() {
  try {
    const r = await rccFetch('/api/queue');
    if (!r.ok || !r.data) return;

    const completed = r.data.completed || [];
    // Find items completed after our last seen ts
    const newItems = completed.filter(item =>
      item && typeof item === 'object' &&
      item.completedAt &&
      new Date(item.completedAt).getTime() > worklogLastSeenTs
    );

    if (newItems.length === 0) return;

    // Sort by completedAt ascending
    newItems.sort((a, b) =>
      new Date(a.completedAt).getTime() - new Date(b.completedAt).getTime()
    );

    // Update our watermark
    worklogLastSeenTs = new Date(newItems[newItems.length - 1].completedAt).getTime();

    // Post daily date header if needed
    const today = worklogDateHeader();
    if (today !== worklogLastDateHeader) {
      worklogLastDateHeader = today;
      await postWorklogMessage(`📅 **${today}**`);
    }

    // Post each completion
    for (const item of newItems) {
      const agent   = item.claimedBy || item.source || 'fleet';
      const title   = item.title || item.id || 'unknown task';
      const result  = item.result || '';
      const commit  = extractCommitHash(result);
      const time    = new Date(item.completedAt).toISOString().slice(11, 16) + ' UTC';

      let msg = `✅ **${agent}** — ${title}`;
      if (result && result.length < 200) {
        msg += `\n> ${result.slice(0, 180)}`;
      }
      if (commit) {
        msg += `\n\`${commit}\``;
      }
      msg += `  ·  _${time}_`;

      await postWorklogMessage(msg);
      console.log(`[worklog] Posted: ${agent} — ${title.slice(0, 60)}`);
    }
  } catch (err) {
    console.warn('[worklog] poll error:', err.message);
  }
}

// Bootstrap #worklog channel and start polling when server starts
(async () => {
  await ensureWorklogChannel();
  // Initial poll slightly delayed to let server fully start
  setTimeout(async () => {
    await pollWorklog();
    setInterval(pollWorklog, WORKLOG_POLL_MS);
  }, 5000);
})();

// SSE stream
app.get('/api/stream', (req, res) => {
  res.set({
    'Content-Type': 'text/event-stream',
    'Cache-Control': 'no-cache',
    'Connection': 'keep-alive',
    'X-Accel-Buffering': 'no',
  });
  res.flushHeaders();
  res.write(`data: ${JSON.stringify({ type: 'connected' })}\n\n`);

  sseClients.add(res);
  const keepalive = setInterval(() => res.write(': ping\n\n'), 25000);

  req.on('close', () => {
    sseClients.delete(res);
    clearInterval(keepalive);
  });
});

// ── #worklog REST endpoint ────────────────────────────────────────────────────
// POST /sc/api/worklog/post
// Body: { agent, title, result, commit? }
// Posts a formatted task-completion entry to the #worklog channel, threaded
// under a "📅 YYYY-MM-DD" daily date-header message.
// No auth token required — agents post directly without a session.

function worklogDateLabel(ts) {
  const d = new Date(ts);
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, '0');
  const day = String(d.getDate()).padStart(2, '0');
  return `📅 ${y}-${m}-${day}`;
}

function worklogInsertMessage(from, text, channel, threadId) {
  const ts = Date.now();
  const r = db.prepare(
    'INSERT INTO messages (ts, from_agent, text, channel, thread_id) VALUES (?, ?, ?, ?, ?)'
  ).run(ts, from, text, channel, threadId || null);
  db.prepare('UPDATE channels SET last_message_at = ? WHERE id = ?').run(ts, channel);
  const msg = formatMessage({
    id: r.lastInsertRowid, ts, from_agent: from, text, channel,
    mentions: null, thread_id: threadId || null, edited_at: null,
    created_at: ts, slash_result: null,
  });
  const payload = JSON.stringify({ type: 'message', data: msg });
  for (const client of sseClients) client.write(`data: ${payload}\n\n`);
  return msg;
}

app.post('/sc/api/worklog/post', (req, res) => {
  const { agent, title, result, commit } = req.body || {};
  if (!agent || !title) {
    return res.status(400).json({ error: 'agent and title are required' });
  }

  const todayLabel = worklogDateLabel(Date.now());

  // Find or create today's date-header message in #worklog
  let dateHeader = db.prepare(
    "SELECT * FROM messages WHERE channel = 'worklog' AND from_agent = 'worklog-system' AND text = ? AND thread_id IS NULL ORDER BY ts DESC LIMIT 1"
  ).get(todayLabel);

  if (!dateHeader) {
    dateHeader = worklogInsertMessage('worklog-system', todayLabel, 'worklog', null);
  }

  // Format the task-completion entry
  const truncated = result ? String(result).slice(0, 200) : '';
  const commitPart = commit ? ` (${String(commit).slice(0, 10)})` : '';
  const text = `✅ **${agent}** — *${title}*${commitPart}\n> ${truncated}`;

  const msg = worklogInsertMessage('worklog-system', text, 'worklog', dateHeader.id);

  res.json({ ok: true, message: msg, date_header_id: dateHeader.id });
});

// SPA catch-all — serve index.html for non-API routes
app.get('/{*path}', (req, res, next) => {
  if (req.path.startsWith('/api/')) return next();
  res.sendFile(join(__dirname, 'public', 'index.html'));
});

const server = createServer(app);
server.listen(PORT, () => {
  console.log(`SquirrelChat running on http://localhost:${PORT}`);
});
