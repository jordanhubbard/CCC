/**
 * crush-server — HTTP/SSE bridge for charmbracelet/crush
 *
 * Endpoints:
 *   GET  /health                        — liveness check
 *   GET  /sessions                      — list crush sessions (--json)
 *   GET  /sessions/:id                  — show session details (--json)
 *   DELETE /sessions/:id                — delete a session
 *   GET  /projects                      — list crush projects (--json)
 *   POST /run                           — run a prompt non-interactively (SSE stream)
 *     body: { prompt, sessionId?, cwd?, model?, yolo? }
 *   POST /sessions/:id/rename           — rename session
 *     body: { title }
 *
 * POST /run streams SSE events:
 *   event: chunk  data: <text chunk>
 *   event: done   data: { exitCode }
 *   event: error  data: <message>
 */

import express from 'express';
import cors from 'cors';
import { spawn } from 'child_process';
import { createRequire } from 'module';

const PORT = parseInt(process.env.CRUSH_SERVER_PORT || '8793', 10);
const CRUSH_BIN = process.env.CRUSH_BIN || 'crush';

const app = express();
app.use(cors());
app.use(express.json());

// ── helpers ────────────────────────────────────────────────────────────────

function crushCmd(args, env = {}) {
  return new Promise((resolve, reject) => {
    const proc = spawn(CRUSH_BIN, args, {
      env: { ...process.env, ...env },
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    let stdout = '';
    let stderr = '';
    proc.stdout.on('data', (d) => { stdout += d; });
    proc.stderr.on('data', (d) => { stderr += d; });
    proc.on('close', (code) => {
      if (code !== 0) {
        reject(new Error(`crush ${args[0]} exited ${code}: ${stderr.trim()}`));
      } else {
        resolve(stdout);
      }
    });
    proc.on('error', reject);
  });
}

// ── routes ─────────────────────────────────────────────────────────────────

app.get('/health', (_req, res) => {
  res.json({ ok: true, service: 'crush-server', bin: CRUSH_BIN });
});

app.get('/sessions', async (req, res) => {
  try {
    const raw = await crushCmd(['session', 'list', '--json']);
    const sessions = JSON.parse(raw || '[]');
    res.json(sessions);
  } catch (err) {
    res.status(500).json({ error: err.message });
  }
});

app.get('/sessions/:id', async (req, res) => {
  try {
    const raw = await crushCmd(['session', 'show', req.params.id, '--json']);
    const session = JSON.parse(raw);
    res.json(session);
  } catch (err) {
    res.status(500).json({ error: err.message });
  }
});

app.delete('/sessions/:id', async (req, res) => {
  try {
    await crushCmd(['session', 'delete', req.params.id]);
    res.json({ ok: true });
  } catch (err) {
    res.status(500).json({ error: err.message });
  }
});

app.post('/sessions/:id/rename', async (req, res) => {
  const { title } = req.body;
  if (!title) return res.status(400).json({ error: 'title required' });
  try {
    await crushCmd(['session', 'rename', req.params.id, title]);
    res.json({ ok: true });
  } catch (err) {
    res.status(500).json({ error: err.message });
  }
});

app.get('/projects', async (req, res) => {
  try {
    const raw = await crushCmd(['projects', '--json']);
    const projects = JSON.parse(raw || '[]');
    res.json(projects);
  } catch (err) {
    res.status(500).json({ error: err.message });
  }
});

// POST /run — streaming SSE
app.post('/run', (req, res) => {
  const { prompt, sessionId, cwd, model, yolo } = req.body || {};

  if (!prompt) {
    return res.status(400).json({ error: 'prompt required' });
  }

  // Build crush run args
  const args = ['run', '--quiet'];
  if (sessionId) args.push('--session', sessionId);
  if (model) args.push('--model', model);
  if (yolo) args.push('--yolo');
  args.push(prompt);

  const env = {};
  if (cwd) env.CRUSH_CWD = cwd; // crush uses --cwd flag

  // Rebuild with --cwd if provided
  if (cwd) args.splice(1, 0, '--cwd', cwd);

  // SSE headers
  res.setHeader('Content-Type', 'text/event-stream');
  res.setHeader('Cache-Control', 'no-cache');
  res.setHeader('Connection', 'keep-alive');
  res.flushHeaders();

  const proc = spawn(CRUSH_BIN, args, {
    env: { ...process.env },
    stdio: ['ignore', 'pipe', 'pipe'],
  });

  let sessionIdOut = null;

  proc.stdout.on('data', (chunk) => {
    const text = chunk.toString();
    // Check if crush prints session id on first line (some versions do)
    if (!sessionIdOut && text.startsWith('Session:')) {
      const match = text.match(/Session:\s*(\S+)/);
      if (match) sessionIdOut = match[1];
    }
    // Escape SSE data (newlines → \n literal in SSE data field)
    const escaped = text.replace(/\n/g, '\\n');
    res.write(`event: chunk\ndata: ${escaped}\n\n`);
  });

  proc.stderr.on('data', (chunk) => {
    const text = chunk.toString().trim();
    if (text) {
      res.write(`event: log\ndata: ${JSON.stringify(text)}\n\n`);
    }
  });

  proc.on('close', (code) => {
    const payload = JSON.stringify({ exitCode: code, sessionId: sessionIdOut });
    res.write(`event: done\ndata: ${payload}\n\n`);
    res.end();
  });

  proc.on('error', (err) => {
    res.write(`event: error\ndata: ${JSON.stringify(err.message)}\n\n`);
    res.end();
  });

  // Clean up if client disconnects
  req.on('close', () => {
    if (proc.exitCode === null) proc.kill('SIGTERM');
  });
});

// ── start ──────────────────────────────────────────────────────────────────

app.listen(PORT, '0.0.0.0', () => {
  console.log(`crush-server listening on :${PORT}`);
  console.log(`  crush binary: ${CRUSH_BIN}`);
});
