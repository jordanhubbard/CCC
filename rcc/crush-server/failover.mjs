/**
 * crush-server/failover.mjs — Claude Code throttle detection + Crush failover
 *
 * When Claude Code hits a rate-limit or token exhaustion, the CCC work queue
 * item should re-route through crush (charmbracelet/crush) instead of erroring.
 *
 * Detection heuristics (any one triggers failover):
 *   • Exit code 1 + stderr contains: "rate limit", "quota", "overloaded",
 *     "too many requests", "529", "529 Overloaded", "usage limit"
 *   • Exit code 2 (claude-code specific auth/quota exit)
 *   • HTTP 429 / 529 in stderr or stdout
 *   • ENV: CLAUDE_THROTTLED=1  (set externally to force crush routing)
 *
 * Usage:
 *   import { shouldFailover, crushFallback, withFailover } from './failover.mjs';
 *
 *   // Wrap a claude-code run with automatic crush fallback:
 *   const result = await withFailover({
 *     prompt,
 *     cwd,
 *     model,          // passed to crush when falling over
 *     onThrottle,     // optional callback(reason) when failover fires
 *   });
 *   // result: { output, sessionId, provider: 'claude-code' | 'crush', exitCode }
 *
 * CCC integration:
 *   POST /run — accepts { prompt, sessionId?, cwd?, model?, yolo? }
 *   The route now attempts claude-code first; on throttle → crush.
 *   SSE events include a provider field: { provider: 'crush' } on failover.
 */

import { spawn } from 'child_process';
import { EventEmitter } from 'events';

// ── Config ─────────────────────────────────────────────────────────────────

const CLAUDE_BIN   = process.env.CLAUDE_BIN   || 'claude';
const CRUSH_BIN    = process.env.CRUSH_BIN    || 'crush';
const CCC_URL      = process.env.CCC_URL      || 'http://localhost:8789';
const RCC_TOKEN    = process.env.CCC_AGENT_TOKEN || '';

// Patterns that indicate Claude Code is throttled / exhausted
const THROTTLE_PATTERNS = [
  /rate[\s_-]?limit/i,
  /too many requests/i,
  /429/,
  /529/,
  /overloaded/i,
  /quota.*exceeded/i,
  /usage.*limit/i,
  /out of tokens/i,
  /context.*window.*exceeded/i,
  /insufficient.*credits/i,
  /token.*exhausted/i,
  /anthropic.*error.*529/i,
];

// Exit codes that indicate throttle (not just a code error)
const THROTTLE_EXIT_CODES = new Set([2, 3]); // claude-code quota/auth exits

// ── Detection ──────────────────────────────────────────────────────────────

/**
 * Determine if claude-code output/exitCode indicates throttling.
 * @param {object} opts
 * @param {number}  opts.exitCode
 * @param {string}  [opts.stdout]
 * @param {string}  [opts.stderr]
 * @returns {{ throttled: boolean, reason: string | null }}
 */
export function shouldFailover({ exitCode, stdout = '', stderr = '' }) {
  if (process.env.CLAUDE_THROTTLED === '1') {
    return { throttled: true, reason: 'CLAUDE_THROTTLED env override' };
  }

  if (THROTTLE_EXIT_CODES.has(exitCode)) {
    return { throttled: true, reason: `claude-code exit ${exitCode} (quota/auth)` };
  }

  const combined = `${stdout}\n${stderr}`;
  for (const pat of THROTTLE_PATTERNS) {
    if (pat.test(combined)) {
      return { throttled: true, reason: `matched pattern: ${pat.toString()}` };
    }
  }

  return { throttled: false, reason: null };
}

// ── CCC notification ───────────────────────────────────────────────────────

/**
 * Notify CCC that a failover happened (writes to ClawBus + logs a lesson).
 */
async function notifyRCC({ reason, prompt, provider }) {
  if (!RCC_TOKEN) return;
  try {
    await fetch(`${CCC_URL}/bus/send`, {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        'Authorization': `Bearer ${RCC_TOKEN}`,
      },
      body: JSON.stringify({
        type:    'failover',
        from:    'crush-server',
        payload: { reason, provider, promptLen: prompt?.length ?? 0, ts: new Date().toISOString() },
      }),
    });
  } catch {
    // Non-fatal — bus notification best-effort
  }
}

// ── Claude Code runner ─────────────────────────────────────────────────────

/**
 * Run claude-code non-interactively, buffering all output.
 * Returns { stdout, stderr, exitCode }.
 */
function runClaude({ prompt, cwd, model }) {
  return new Promise((resolve) => {
    const args = ['--print', '--permission-mode', 'bypassPermissions'];
    if (model) args.push('--model', model);
    args.push(prompt);

    const proc = spawn(CLAUDE_BIN, args, {
      cwd: cwd || process.cwd(),
      env: process.env,
      stdio: ['ignore', 'pipe', 'pipe'],
    });

    let stdout = '';
    let stderr = '';
    proc.stdout.on('data', d => { stdout += d; });
    proc.stderr.on('data', d => { stderr += d; });
    proc.on('error', err => {
      resolve({ stdout, stderr: stderr + '\n' + err.message, exitCode: 1 });
    });
    proc.on('close', exitCode => {
      resolve({ stdout, stderr, exitCode: exitCode ?? 1 });
    });
  });
}

// ── Crush runner (streaming) ───────────────────────────────────────────────

/**
 * Run crush non-interactively, emitting SSE-style events on an EventEmitter.
 * Emits: 'chunk' (text), 'done' ({ exitCode, sessionId }), 'error' (message).
 */
export function runCrushStream({ prompt, sessionId, cwd, model, yolo = false }) {
  const ee = new EventEmitter();

  const args = ['run', '--quiet'];
  if (cwd) args.push('--cwd', cwd);
  if (sessionId) args.push('--session', sessionId);
  if (model) args.push('--model', model);
  if (yolo) args.push('--yolo');
  args.push(prompt);

  const proc = spawn(CRUSH_BIN, args, {
    env: process.env,
    stdio: ['ignore', 'pipe', 'pipe'],
  });

  let sessionIdOut = null;

  proc.stdout.on('data', chunk => {
    const text = chunk.toString();
    if (!sessionIdOut) {
      const m = text.match(/Session:\s*(\S+)/);
      if (m) sessionIdOut = m[1];
    }
    ee.emit('chunk', text);
  });

  proc.stderr.on('data', chunk => {
    const text = chunk.toString().trim();
    if (text) ee.emit('log', text);
  });

  proc.on('close', code => {
    ee.emit('done', { exitCode: code ?? 0, sessionId: sessionIdOut });
  });

  proc.on('error', err => {
    ee.emit('error', err.message);
  });

  // expose kill handle
  ee.kill = (sig = 'SIGTERM') => { if (proc.exitCode === null) proc.kill(sig); };

  return ee;
}

// ── withFailover ───────────────────────────────────────────────────────────

/**
 * Run a coding prompt with automatic crush failover.
 *
 * Non-streaming: attempts claude-code, falls back to crush (buffered).
 *
 * @param {object} opts
 * @param {string}  opts.prompt
 * @param {string}  [opts.cwd]
 * @param {string}  [opts.model]
 * @param {string}  [opts.sessionId]   — passed to crush on failover
 * @param {boolean} [opts.yolo]        — passed to crush
 * @param {Function} [opts.onThrottle] — callback(reason) when failover fires
 * @returns {Promise<{ output: string, sessionId: string|null, provider: string, exitCode: number }>}
 */
export async function withFailover({ prompt, cwd, model, sessionId, yolo, onThrottle } = {}) {
  // 1. Try claude-code first (unless forced)
  if (process.env.CLAUDE_THROTTLED !== '1') {
    const r = await runClaude({ prompt, cwd, model });
    const { throttled, reason } = shouldFailover(r);

    if (!throttled) {
      return {
        output:    r.stdout,
        sessionId: null,
        provider:  'claude-code',
        exitCode:  r.exitCode,
      };
    }

    if (onThrottle) onThrottle(reason);
    await notifyRCC({ reason, prompt, provider: 'crush' });
  }

  // 2. Failover to crush (buffered, non-streaming)
  return new Promise((resolve, reject) => {
    const ee = runCrushStream({ prompt, sessionId, cwd, model, yolo });
    let output = '';
    let sid = null;

    ee.on('chunk', t => { output += t; });
    ee.on('done',  ({ exitCode, sessionId: s }) => {
      sid = s;
      resolve({ output, sessionId: sid, provider: 'crush', exitCode });
    });
    ee.on('error', msg => reject(new Error(msg)));
  });
}

// ── SSE failover helper (for HTTP routes) ──────────────────────────────────

/**
 * Like withFailover but streams SSE events to an Express response.
 * Emits a `provider` SSE event before chunks start so the UI knows the source.
 *
 * @param {object} opts
 * @param {object}  opts.res         — Express response (SSE headers already set)
 * @param {string}  opts.prompt
 * @param {string}  [opts.cwd]
 * @param {string}  [opts.model]
 * @param {string}  [opts.sessionId]
 * @param {boolean} [opts.yolo]
 * @param {Function} [opts.onThrottle]
 */
export async function sseWithFailover({ res, prompt, cwd, model, sessionId, yolo, onThrottle } = {}) {
  const send = (event, data) => {
    const payload = typeof data === 'string' ? data : JSON.stringify(data);
    res.write(`event: ${event}\ndata: ${payload}\n\n`);
  };

  // Try claude-code first
  let useCrush = process.env.CLAUDE_THROTTLED === '1';
  let claudeOut = '';

  if (!useCrush) {
    send('provider', { provider: 'claude-code', status: 'starting' });

    const r = await runClaude({ prompt, cwd, model });
    const { throttled, reason } = shouldFailover(r);

    if (!throttled) {
      // Success — stream buffered output as chunks
      send('provider', { provider: 'claude-code', status: 'ok' });
      const escaped = r.stdout.replace(/\n/g, '\\n');
      send('chunk', escaped);
      send('done', { exitCode: r.exitCode, sessionId: null, provider: 'claude-code' });
      res.end();
      return;
    }

    // Throttled — fall over
    useCrush = true;
    if (onThrottle) onThrottle(reason);
    await notifyRCC({ reason, prompt, provider: 'crush' });
    send('provider', { provider: 'crush', status: 'failover', reason });
  } else {
    send('provider', { provider: 'crush', status: 'forced' });
  }

  // Stream crush output
  const ee = runCrushStream({ prompt, sessionId, cwd, model, yolo });

  ee.on('chunk', text => {
    const escaped = text.replace(/\n/g, '\\n');
    send('chunk', escaped);
  });

  ee.on('log', text => {
    send('log', JSON.stringify(text));
  });

  ee.on('done', ({ exitCode, sessionId: sid }) => {
    send('done', { exitCode, sessionId: sid, provider: 'crush' });
    res.end();
  });

  ee.on('error', msg => {
    send('error', JSON.stringify(msg));
    res.end();
  });

  // Clean up if client disconnects
  res.on('close', () => ee.kill());
}
