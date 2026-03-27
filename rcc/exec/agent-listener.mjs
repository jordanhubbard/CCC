/**
 * rcc/exec/agent-listener.mjs — SquirrelBus subscriber + remote code executor
 *
 * Listens on SquirrelBus for `rcc.exec` messages, verifies HMAC signatures,
 * executes code in a sandboxed vm.runInNewContext() with a 10s timeout,
 * captures output, and POSTs results back to the RCC API.
 *
 * Security rules (NON-NEGOTIABLE):
 * - NEVER execute unsigned or tampered code
 * - NEVER use eval() — only vm.runInNewContext()
 * - ALWAYS log every execution attempt to ~/.rcc/logs/remote-exec.jsonl
 * - 10 second hard timeout
 *
 * Environment variables:
 *   SQUIRRELBUS_TOKEN  — shared secret for HMAC verification (required)
 *   SQUIRRELBUS_URL    — bus URL (default: http://localhost:8788)
 *   RCC_URL            — RCC API base URL (default: http://localhost:8789)
 *   RCC_AUTH_TOKEN     — bearer token for RCC API (required)
 *   AGENT_NAME         — agent identifier (default: 'unknown')
 */

import vm from 'vm';
import { mkdir, appendFile } from 'fs/promises';
import { homedir } from 'os';
import { join } from 'path';
import { randomUUID } from 'crypto';
import { verifyPayload } from './index.mjs';

// ── Config ─────────────────────────────────────────────────────────────────
const SQUIRRELBUS_URL = process.env.SQUIRRELBUS_URL || 'http://localhost:8788';
const RCC_URL         = process.env.RCC_URL         || 'http://localhost:8789';
const RCC_AUTH_TOKEN  = process.env.RCC_AUTH_TOKEN  || process.env.RCC_AUTH_TOKENS?.split(',')[0] || '';
const AGENT_NAME      = process.env.AGENT_NAME      || 'unknown';
const BUS_TOKEN       = process.env.SQUIRRELBUS_TOKEN || '';
const EXEC_TIMEOUT_MS = 10_000;

const LOG_DIR  = join(homedir(), '.rcc', 'logs');
const LOG_FILE = join(LOG_DIR, 'remote-exec.jsonl');

// ── Logging ────────────────────────────────────────────────────────────────
async function logExecution(record) {
  await mkdir(LOG_DIR, { recursive: true });
  await appendFile(LOG_FILE, JSON.stringify(record) + '\n', 'utf8');
}

// ── Safe executor ──────────────────────────────────────────────────────────
function executeCode(code) {
  const output = [];
  const context = {
    console: {
      log:   (...args) => output.push(args.map(String).join(' ')),
      error: (...args) => output.push('[error] ' + args.map(String).join(' ')),
      warn:  (...args) => output.push('[warn] '  + args.map(String).join(' ')),
      info:  (...args) => output.push(args.map(String).join(' ')),
    },
    // Minimal safe globals
    Math,
    Date,
    JSON,
    parseInt,
    parseFloat,
    isNaN,
    isFinite,
    encodeURIComponent,
    decodeURIComponent,
    String,
    Number,
    Boolean,
    Array,
    Object,
    Error,
  };

  vm.createContext(context);

  let result;
  try {
    result = vm.runInContext(code, context, {
      timeout:    EXEC_TIMEOUT_MS,
      displayErrors: true,
    });
  } catch (err) {
    return { ok: false, error: err.message, output: output.join('\n') };
  }

  return {
    ok:     true,
    result: result !== undefined ? String(result) : undefined,
    output: output.join('\n'),
  };
}

// ── Handle a single rcc.exec message ──────────────────────────────────────
async function handleExecMessage(message) {
  // Parse body
  let envelope;
  try {
    envelope = typeof message.body === 'string' ? JSON.parse(message.body) : message.body;
  } catch (err) {
    console.warn('[exec-listener] Failed to parse exec envelope:', err.message);
    return;
  }

  const execId   = envelope.execId   || message.id || randomUUID();
  const target   = envelope.target   || 'all';
  const code     = envelope.code     || '';
  const replyTo  = envelope.replyTo  || null;

  // ── Target filter ─────────────────────────────────────────────────────
  if (target !== 'all' && target !== AGENT_NAME) {
    // Not for us — ignore silently
    return;
  }

  const ts = new Date().toISOString();

  // ── Verify HMAC signature ─────────────────────────────────────────────
  if (!BUS_TOKEN) {
    console.error('[exec-listener] SQUIRRELBUS_TOKEN not set — cannot verify exec payload');
    await logExecution({
      ts, execId, agent: AGENT_NAME, target, status: 'rejected',
      reason: 'no_secret', code: code.slice(0, 200),
    });
    return;
  }

  const valid = verifyPayload(envelope, BUS_TOKEN);
  if (!valid) {
    // Bad sig: log and drop silently (no error response to sender)
    console.warn(`[exec-listener] Bad signature on exec ${execId} — dropping`);
    await logExecution({
      ts, execId, agent: AGENT_NAME, target, status: 'rejected',
      reason: 'bad_signature', code: code.slice(0, 200),
    });
    return;
  }

  // ── Log attempt (before execution) ───────────────────────────────────
  console.log(`[exec-listener] Executing ${execId} (${code.length} bytes)`);
  if (replyTo) {
    console.log(`[exec-listener] replyTo: ${replyTo}`);
  }

  // ── Execute ───────────────────────────────────────────────────────────
  const startMs = Date.now();
  const execResult = executeCode(code);
  const durationMs = Date.now() - startMs;

  // ── Log result ────────────────────────────────────────────────────────
  await logExecution({
    ts,
    execId,
    agent:      AGENT_NAME,
    target,
    status:     execResult.ok ? 'ok' : 'error',
    durationMs,
    output:     execResult.output,
    result:     execResult.result,
    error:      execResult.error || null,
    codeLen:    code.length,
    replyTo,
  });

  // ── POST result to RCC API ────────────────────────────────────────────
  const resultPayload = {
    agent:      AGENT_NAME,
    execId,
    ts,
    ok:         execResult.ok,
    output:     execResult.output || '',
    result:     execResult.result || null,
    error:      execResult.error  || null,
    durationMs,
  };

  try {
    const resp = await fetch(`${RCC_URL}/api/exec/${execId}/result`, {
      method: 'POST',
      headers: {
        'Authorization':  `Bearer ${RCC_AUTH_TOKEN}`,
        'Content-Type':   'application/json',
      },
      body: JSON.stringify(resultPayload),
    });
    if (!resp.ok) {
      console.warn(`[exec-listener] Result POST failed: ${resp.status}`);
    }
  } catch (err) {
    console.warn(`[exec-listener] Could not POST result: ${err.message}`);
  }
}

// ── Subscribe to SquirrelBus via SSE stream ────────────────────────────────
async function subscribe() {
  console.log(`[exec-listener] Connecting to SquirrelBus at ${SQUIRRELBUS_URL}/bus/stream`);

  while (true) {
    try {
      const resp = await fetch(`${SQUIRRELBUS_URL}/bus/stream`, {
        headers: { 'Accept': 'text/event-stream' },
      });

      if (!resp.ok) {
        throw new Error(`SSE connect failed: ${resp.status}`);
      }

      console.log('[exec-listener] Connected to SquirrelBus SSE stream');

      // Read SSE stream line by line
      const reader = resp.body.getReader();
      const decoder = new TextDecoder();
      let buf = '';

      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        buf += decoder.decode(value, { stream: true });

        const lines = buf.split('\n');
        buf = lines.pop(); // keep incomplete line

        for (const line of lines) {
          if (!line.startsWith('data: ')) continue;
          const data = line.slice(6).trim();
          if (!data || data === '[DONE]') continue;
          try {
            const message = JSON.parse(data);
            if (message.type === 'rcc.exec') {
              handleExecMessage(message).catch(err =>
                console.error('[exec-listener] Handler error:', err.message)
              );
            }
          } catch {
            // ignore parse errors on SSE frames
          }
        }
      }

      console.log('[exec-listener] SSE stream ended — reconnecting in 5s');
    } catch (err) {
      console.warn(`[exec-listener] SSE error: ${err.message} — reconnecting in 5s`);
    }

    await new Promise(r => setTimeout(r, 5000));
  }
}

// ── Main ───────────────────────────────────────────────────────────────────
if (!BUS_TOKEN) {
  console.error('[exec-listener] FATAL: SQUIRRELBUS_TOKEN is not set. Refusing to start.');
  process.exit(1);
}

console.log(`[exec-listener] Starting agent=${AGENT_NAME} rcc=${RCC_URL} bus=${SQUIRRELBUS_URL}`);
subscribe().catch(err => {
  console.error('[exec-listener] Fatal error:', err.message);
  process.exit(1);
});

process.on('SIGTERM', () => process.exit(0));
process.on('SIGINT',  () => process.exit(0));
