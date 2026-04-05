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
import { shouldFailover, sseWithFailover, runCrushStream } from './failover.mjs';

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

// GET /failover-status — current routing config snapshot
app.get('/failover-status', (_req, res) => {
  res.json({
    mode:          process.env.CRUSH_ONLY === '1'     ? 'crush-only'
                 : process.env.CLAUDE_THROTTLED === '1' ? 'crush-forced'
                 : 'auto',
    crushBin:      CRUSH_BIN,
    claudeBin:     process.env.CLAUDE_BIN || 'claude',
    rccUrl:        process.env.CCC_URL || 'http://localhost:8789',
    ts:            new Date().toISOString(),
  });
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

// POST /run — streaming SSE with automatic claude-code → crush failover
//
// If claude-code is available (CLAUDE_BIN in PATH), it is tried first.
// On rate-limit / quota exhaustion (429, 529, exit 2, etc.) the request
// transparently falls over to crush and the SSE stream emits:
//   event: provider  data: { provider: "crush", status: "failover", reason: "..." }
// before the first chunk arrives.
//
// Set CLAUDE_THROTTLED=1 to force crush routing without attempting claude-code.
// Set CRUSH_ONLY=1 to skip failover entirely and always use crush directly.
app.post('/run', async (req, res) => {
  const { prompt, sessionId, cwd, model, yolo } = req.body || {};

  if (!prompt) {
    return res.status(400).json({ error: 'prompt required' });
  }

  // SSE headers
  res.setHeader('Content-Type', 'text/event-stream');
  res.setHeader('Cache-Control', 'no-cache');
  res.setHeader('Connection', 'keep-alive');
  res.flushHeaders();

  // CRUSH_ONLY=1 → skip claude-code entirely (direct crush, no failover logic)
  if (process.env.CRUSH_ONLY === '1') {
    const ee = runCrushStream({ prompt, sessionId, cwd, model, yolo });
    ee.on('chunk', text => {
      res.write(`event: chunk\ndata: ${text.replace(/\n/g, '\\n')}\n\n`);
    });
    ee.on('log', text => {
      res.write(`event: log\ndata: ${JSON.stringify(text)}\n\n`);
    });
    ee.on('done', ({ exitCode, sessionId: sid }) => {
      res.write(`event: done\ndata: ${JSON.stringify({ exitCode, sessionId: sid, provider: 'crush' })}\n\n`);
      res.end();
    });
    ee.on('error', msg => {
      res.write(`event: error\ndata: ${JSON.stringify(msg)}\n\n`);
      res.end();
    });
    req.on('close', () => ee.kill());
    return;
  }

  // Default: try claude-code → crush failover
  try {
    await sseWithFailover({
      res, prompt, cwd, model, sessionId, yolo,
      onThrottle: (reason) => {
        console.warn(`[crush-server] claude-code throttled: ${reason} — failing over to crush`);
      },
    });
  } catch (err) {
    if (!res.writableEnded) {
      res.write(`event: error\ndata: ${JSON.stringify(err.message)}\n\n`);
      res.end();
    }
  }
});

// ── start ──────────────────────────────────────────────────────────────────

app.listen(PORT, '0.0.0.0', () => {
  console.log(`crush-server listening on :${PORT}`);
  console.log(`  crush binary: ${CRUSH_BIN}`);
});
