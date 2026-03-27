import express from 'express';
import { ingestMessage } from '../../../.openclaw/workspace/rcc/vector/ingest.mjs';
import { DatabaseSync as Database } from 'node:sqlite';
import archiver from 'archiver';
import { createServer } from 'http';
import { fileURLToPath } from 'url';
import { dirname, join } from 'path';


const __dirname = dirname(fileURLToPath(import.meta.url));
const app = express();
const PORT = process.env.PORT || 8790;
const ADMIN_TOKEN = 'sc-squirrelchat-admin-2026';
const RCC_BASE = 'http://localhost:8789';

// DB setup
const db = new Database('./squirrelchat.db');
db.exec(`
  CREATE TABLE IF NOT EXISTS messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ts INTEGER NOT NULL,
    from_agent TEXT NOT NULL,
    text TEXT NOT NULL,
    channel TEXT NOT NULL DEFAULT 'chat',
    mentions TEXT,
    slash_result TEXT,
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
`);

// SSE clients
const sseClients = new Set();

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

// Auth middleware
function auth(req, res, next) {
  const token = (req.headers.authorization || '').replace('Bearer ', '');
  if (token !== ADMIN_TOKEN) return res.status(401).json({ error: 'Unauthorized' });
  next();
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
      headers: { 'Content-Type': 'application/json', ...(options.headers || {}) },
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

// Messages
app.get('/api/messages', (req, res) => {
  const since = parseInt(req.query.since) || 0;
  const channel = req.query.channel || 'chat';
  const limit = Math.min(parseInt(req.query.limit) || 50, 200);

  let query, params;
  if (channel === 'all') {
    query = 'SELECT * FROM messages WHERE ts > ? ORDER BY ts DESC LIMIT ?';
    params = [since, limit];
  } else {
    query = 'SELECT * FROM messages WHERE ts > ? AND channel = ? ORDER BY ts DESC LIMIT ?';
    params = [since, channel, limit];
  }

  const rows = db.prepare(query).all(...params).reverse();
  const messages = rows.map(r => ({
    ...r,
    mentions: r.mentions ? JSON.parse(r.mentions) : [],
    slash_result: r.slash_result || null,
  }));
  res.json(messages);
});

app.post('/api/messages', auth, async (req, res) => {
  const { from, text, channel = 'chat', mentions } = req.body;
  if (!from || !text) return res.status(400).json({ error: 'from and text required' });

  const ts = Date.now();
  const stmt = db.prepare('INSERT INTO messages (ts, from_agent, text, channel, mentions) VALUES (?, ?, ?, ?, ?)');
  const r = stmt.run(ts, from, text, channel, mentions ? JSON.stringify(mentions) : null);
  const message = { id: r.lastInsertRowid, ts, from_agent: from, text, channel, mentions: mentions || [] };

  // Broadcast via SSE
  const payload = JSON.stringify({ type: 'message', message });
  for (const client of sseClients) {
    client.write(`data: ${payload}\n\n`);
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
      botReply = { id: br.lastInsertRowid, ts: bts, from_agent: 'squirrelbot', text: slashResult, channel };

      // Update original message with slash_result
      db.prepare('UPDATE messages SET slash_result = ? WHERE id = ?').run(trimmed, r.lastInsertRowid);

      const botPayload = JSON.stringify({ type: 'message', message: botReply });
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

const server = createServer(app);
server.listen(PORT, () => {
  console.log(`SquirrelChat running on http://localhost:${PORT}`);
});
