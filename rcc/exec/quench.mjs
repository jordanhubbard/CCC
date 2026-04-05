/**
 * rcc/exec/quench.mjs — Agent quench/pause mechanism via ClawBus
 *
 * Agents call checkQuench() between work units. If a quench signal has been
 * received (via rcc.quench ClawBus message), checkQuench() blocks for the
 * remaining pause duration before returning. No work is interrupted mid-task.
 *
 * ClawBus message schema (type: "rcc.quench"):
 *   {
 *     "type":             "rcc.quench",
 *     "from":             "<sender-agent>",
 *     "body": {
 *       "target":           "<agent-name> | all",
 *       "duration_minutes": <number 1–30>,
 *       "reason":           "<optional human note>"
 *     }
 *   }
 *
 * Hard constraints:
 *   - duration_minutes is capped at MAX_QUENCH_MINUTES (30)
 *   - A subsequent quench signal does NOT extend an already-active quench
 *     (first quench wins; sender must wait for it to expire)
 *   - Quench is cleared automatically on expiry; agents never need manual resume
 *
 * Environment variables:
 *   SQUIRRELBUS_URL  — bus URL (default: http://localhost:8788)
 *   SQUIRRELBUS_TOKEN — shared secret (required for send; optional for receive)
 *   AGENT_NAME       — this agent's name (default: 'unknown')
 *   CCC_AUTH_TOKEN   — bearer token for ClawBus auth (required for send)
 */

import { homedir } from 'os';
import { join }    from 'path';
import { mkdir, appendFile } from 'fs/promises';

// ── Constants ───────────────────────────────────────────────────────────────
export const MAX_QUENCH_MINUTES = 30;

const AGENT_NAME = process.env.AGENT_NAME || 'unknown';
const LOG_DIR    = join(homedir(), '.rcc', 'logs');
const LOG_FILE   = join(LOG_DIR, 'quench.jsonl');

// ── State (module-level singleton) ──────────────────────────────────────────
let _quenchUntil = null;   // Date | null
let _quenchFrom  = null;   // string | null — who sent it
let _quenchReason = null;  // string | null

// ── Logging ─────────────────────────────────────────────────────────────────
async function log(record) {
  try {
    await mkdir(LOG_DIR, { recursive: true });
    await appendFile(LOG_FILE, JSON.stringify(record) + '\n', 'utf8');
  } catch { /* non-fatal */ }
}

// ── Internal helpers ─────────────────────────────────────────────────────────

/** True if a quench is currently active. */
export function isQuenched() {
  if (!_quenchUntil) return false;
  if (Date.now() >= _quenchUntil.getTime()) {
    _quenchUntil = null;
    _quenchFrom  = null;
    _quenchReason = null;
    return false;
  }
  return true;
}

/** Remaining quench duration in milliseconds (0 if not quenched). */
export function quenchRemainingMs() {
  if (!_quenchUntil) return 0;
  return Math.max(0, _quenchUntil.getTime() - Date.now());
}

/** Return current quench info (null if not active). */
export function quenchStatus() {
  if (!isQuenched()) return null;
  return {
    until:  _quenchUntil.toISOString(),
    from:   _quenchFrom,
    reason: _quenchReason,
    remainingMs: quenchRemainingMs(),
  };
}

// ── Apply a quench signal ────────────────────────────────────────────────────
/**
 * Apply a quench signal received from the ClawBus.
 * Ignores the signal if a quench is already active (first wins).
 */
export async function applyQuench({ target, duration_minutes, from, reason }) {
  const targetName = (target || 'all').toLowerCase();

  // Target filter
  if (targetName !== 'all' && targetName !== AGENT_NAME.toLowerCase()) {
    return; // not for us
  }

  // Already quenched — first quench wins
  if (isQuenched()) {
    console.log(`[quench] Ignoring quench from ${from}: already quenched until ${_quenchUntil.toISOString()}`);
    await log({
      ts: new Date().toISOString(), agent: AGENT_NAME, event: 'ignored',
      from, target, duration_minutes, reason, activeUntil: _quenchUntil.toISOString(),
    });
    return;
  }

  // Clamp duration
  const mins    = Math.min(Math.max(1, Math.round(duration_minutes || 1)), MAX_QUENCH_MINUTES);
  const until   = new Date(Date.now() + mins * 60_000);

  _quenchUntil  = until;
  _quenchFrom   = from   || 'unknown';
  _quenchReason = reason || null;

  const ts = new Date().toISOString();
  console.log(`[quench] QUENCHED by ${_quenchFrom} for ${mins}m (until ${until.toISOString()})${reason ? ': ' + reason : ''}`);

  await log({
    ts, agent: AGENT_NAME, event: 'quenched',
    from: _quenchFrom, target, duration_minutes: mins,
    until: until.toISOString(), reason: _quenchReason,
  });
}

// ── checkQuench — call between work units ────────────────────────────────────
/**
 * Call this between work units in any agent loop.
 * If a quench is active, blocks (await) until it expires, then returns.
 * Safe to call even if no quench is active (returns immediately).
 */
export async function checkQuench() {
  if (!isQuenched()) return;

  const remainMs = quenchRemainingMs();
  const until    = _quenchUntil.toISOString();
  const from     = _quenchFrom;

  console.log(`[quench] Pausing work for ${Math.ceil(remainMs / 1000)}s (until ${until}, requested by ${from})`);

  await log({
    ts: new Date().toISOString(), agent: AGENT_NAME, event: 'pausing',
    remainMs, until, from,
  });

  // Poll every second so expiry is detected promptly
  while (isQuenched()) {
    await new Promise(r => setTimeout(r, 1000));
  }

  console.log('[quench] Quench expired — resuming work');
  await log({
    ts: new Date().toISOString(), agent: AGENT_NAME, event: 'resumed',
  });
}

// ── ClawBus listener integration ─────────────────────────────────────────────
/**
 * Handle an rcc.quench SSE message from ClawBus.
 * Call this from agent-listener.mjs when message.type === 'rcc.quench'.
 */
export async function handleQuenchMessage(message) {
  let body;
  try {
    body = typeof message.body === 'string' ? JSON.parse(message.body) : (message.body || {});
  } catch {
    body = {};
  }

  const { target, duration_minutes, reason } = body;
  const from = message.from || body.from || 'unknown';

  await applyQuench({ target, duration_minutes, from, reason });
}

// ── Send a quench signal to ClawBus ──────────────────────────────────────────
/**
 * Send a quench signal via the ClawBus API.
 * Can be called by any agent (or the send-quench.mjs CLI).
 *
 * @param {object} opts
 * @param {string} opts.target           — agent name or 'all'
 * @param {number} opts.duration_minutes — 1–30
 * @param {string} [opts.reason]         — optional human note
 * @param {string} [opts.busUrl]         — overrides SQUIRRELBUS_URL
 * @param {string} [opts.authToken]      — overrides CCC_AUTH_TOKEN
 */
export async function sendQuench({ target, duration_minutes, reason, busUrl, authToken } = {}) {
  const url   = (busUrl   || process.env.SQUIRRELBUS_URL || 'http://localhost:8788') + '/api/bus/send';
  const token = authToken || process.env.CCC_AUTH_TOKEN  || '';

  const mins = Math.min(Math.max(1, Math.round(duration_minutes || 1)), MAX_QUENCH_MINUTES);

  const payload = {
    from: AGENT_NAME,
    to:   target || 'all',
    type: 'rcc.quench',
    mime: 'application/json',
    body: JSON.stringify({ target: target || 'all', duration_minutes: mins, reason }),
  };

  const resp = await fetch(url, {
    method:  'POST',
    headers: {
      'Authorization': `Bearer ${token}`,
      'Content-Type':  'application/json',
    },
    body: JSON.stringify(payload),
  });

  if (!resp.ok) {
    const text = await resp.text();
    throw new Error(`ClawBus send failed ${resp.status}: ${text}`);
  }

  const result = await resp.json();
  console.log(`[quench] Signal sent → target=${target || 'all'} duration=${mins}m`);
  return result;
}
