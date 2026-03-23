#!/usr/bin/env node
/**
 * claude-worker.mjs
 * Reusable module for delegating tasks to a local Claude Code tmux session.
 *
 * Usable by any OpenClaw agent (any OpenClaw agent).
 *
 * Exports:
 *   sendTask(sessionName, task, opts)      — send task, wait for prompt, return output
 *   sendTaskBackground(sessionName, task)  — fire-and-forget with & prefix
 *   detectSession()                        — find first tmux session running claude
 *   pollUntilDone(sessionName, timeoutMs)  — poll pane until idle prompt detected
 */

import { execFileSync, execSync } from 'child_process';

// ── Prompt detection ───────────────────────────────────────────────────────────
// Claude Code shows one of these when idle and ready for input:
//   • "❯" at the start of the last non-empty line  (input prompt)
//   • "? for shortcuts" anywhere in the visible pane output
//   • The esc-code stripped line starts with ">" (ASCII fallback)
const IDLE_PATTERNS = [
  /^❯\s/m,
  /\? for shortcuts/,
  /^>\s/m,
];

// Spinner chars Claude Code uses while processing
const SPINNER_CHARS = new Set(['⠋','⠙','⠹','⠸','⠼','⠴','⠦','⠧','⠇','⠏']);

// Strip ANSI/VT escape codes from captured pane text
function stripAnsi(str) {
  // eslint-disable-next-line no-control-regex
  return str.replace(/\x1b\[[0-9;?]*[a-zA-Z]/g, '')
            .replace(/\x1b\][^\x07]*\x07/g, '')
            .replace(/\x1b[()][AB012]/g, '')
            .replace(/\x1b./g, '');
}

// ── Low-level tmux helpers ─────────────────────────────────────────────────────

/**
 * Capture visible pane content for a session (raw, with ANSI).
 * @param {string} target  tmux target: "session", "session:window", or "session:window.pane"
 * @returns {string}
 */
function capturePane(target) {
  try {
    return execFileSync('tmux', ['capture-pane', '-t', target, '-p', '-e'], {
      encoding: 'utf8',
      stdio: ['pipe', 'pipe', 'pipe'],
    });
  } catch {
    // Fall back without -e (some older tmux builds)
    try {
      return execFileSync('tmux', ['capture-pane', '-t', target, '-p'], {
        encoding: 'utf8',
        stdio: ['pipe', 'pipe', 'pipe'],
      });
    } catch {
      return '';
    }
  }
}

/**
 * Send keys to a tmux target.
 */
function sendKeys(target, text, { pressEnter = true } = {}) {
  const args = ['send-keys', '-t', target, text];
  if (pressEnter) args.push('Enter');
  execFileSync('tmux', args, { stdio: ['pipe', 'pipe', 'pipe'] });
}

/**
 * Sleep for ms milliseconds.
 */
function sleep(ms) {
  return new Promise(resolve => setTimeout(resolve, ms));
}

// ── Prompt detection helpers ───────────────────────────────────────────────────

/**
 * Returns true if the pane output looks idle (prompt visible, no spinner).
 */
function isIdle(paneText) {
  const clean = stripAnsi(paneText);
  // Check for spinner in last few lines — if present, still working
  const lastLines = clean.split('\n').slice(-6).join('');
  for (const ch of SPINNER_CHARS) {
    if (lastLines.includes(ch)) return false;
  }
  return IDLE_PATTERNS.some(re => re.test(clean));
}

/**
 * Returns true if the pane has changed meaningfully from a baseline snapshot.
 * Used to confirm the task actually started before we begin waiting for done.
 */
function hasChanged(before, after) {
  return stripAnsi(before).trim() !== stripAnsi(after).trim();
}

// ── Public API ─────────────────────────────────────────────────────────────────

/**
 * detectSession() — scan tmux ls and return the first session that looks like
 * it's running claude (heuristic: pane current-command contains "claude" or
 * pane visible output contains the claude prompt).
 *
 * @returns {string|null}  session name, or null if none found
 */
export function detectSession() {
  let paneList;
  try {
    paneList = execFileSync(
      'tmux',
      ['list-panes', '-a', '-F', '#{session_name}|||#{pane_current_command}|||#{pane_id}'],
      { encoding: 'utf8', stdio: ['pipe', 'pipe', 'pipe'] }
    );
  } catch {
    return null;
  }

  // Note: we use ||| as separator instead of \t because tmux 3.6a on macOS
  // does not reliably pass \t through execFileSync format strings.
  for (const line of paneList.trim().split('\n')) {
    if (!line.trim()) continue;
    const [session, cmd] = line.split('|||');
    if (!session) continue;

    // Fast path: current command is literally "claude"
    if (cmd && cmd.trim().toLowerCase() === 'claude') return session.trim();

    // Slower path: check pane output for claude prompt markers
    const paneText = capturePane(session.trim());
    const clean = stripAnsi(paneText);
    if (clean.includes('? for shortcuts') || /^❯/m.test(clean)) {
      return session.trim();
    }
  }
  return null;
}

/**
 * pollUntilDone(sessionName, timeoutMs) — poll pane output every pollIntervalMs
 * until the idle prompt is detected, or until timeoutMs elapses.
 *
 * @param {string} sessionName
 * @param {number} timeoutMs        default 120_000 (2 min)
 * @param {number} pollIntervalMs   default 800ms
 * @returns {Promise<{done: boolean, output: string}>}
 */
export async function pollUntilDone(sessionName, timeoutMs = 120_000, pollIntervalMs = 800) {
  const deadline = Date.now() + timeoutMs;
  let lastOutput = '';

  while (Date.now() < deadline) {
    await sleep(pollIntervalMs);
    const raw = capturePane(sessionName);
    lastOutput = raw;
    if (isIdle(raw)) {
      return { done: true, output: stripAnsi(raw) };
    }
  }
  return { done: false, output: stripAnsi(lastOutput) };
}

/**
 * sendTask(sessionName, task, opts) — send a task to a Claude tmux session,
 * wait for the prompt to return, and return the captured output text.
 *
 * @param {string} sessionName   tmux session name (e.g. "auth3", "claude-puck")
 * @param {string} task          task string to type into the session
 * @param {object} opts
 *   @param {number}  opts.timeoutMs      total timeout in ms (default 120_000)
 *   @param {number}  opts.startupWaitMs  wait after sending before polling (default 1_500)
 *   @param {number}  opts.pollIntervalMs polling interval in ms (default 800)
 *   @param {boolean} opts.debug          print debug lines to stderr
 * @returns {Promise<{done: boolean, output: string, elapsed: number}>}
 */
export async function sendTask(sessionName, task, opts = {}) {
  const {
    timeoutMs      = 120_000,
    startupWaitMs  = 1_500,
    pollIntervalMs = 800,
    debug          = false,
  } = opts;

  const dbg = debug ? (...a) => console.error('[claude-worker]', ...a) : () => {};

  const baseline = capturePane(sessionName);
  dbg(`baseline captured (${baseline.length} chars)`);

  sendKeys(sessionName, task, { pressEnter: true });
  dbg(`task sent to session "${sessionName}"`);

  // Wait for Claude to start processing (spinner appears)
  await sleep(startupWaitMs);

  // Verify the pane actually changed (task was accepted)
  const afterSend = capturePane(sessionName);
  if (!hasChanged(baseline, afterSend)) {
    dbg('WARNING: pane did not change after sending task — session may be unresponsive');
  }

  const start = Date.now();
  const deadline = start + timeoutMs;
  let lastOutput = afterSend;

  while (Date.now() < deadline) {
    await sleep(pollIntervalMs);
    const raw = capturePane(sessionName);
    lastOutput = raw;

    if (isIdle(raw)) {
      const elapsed = Date.now() - start;
      dbg(`done in ${elapsed}ms`);
      return { done: true, output: stripAnsi(raw), elapsed };
    }
    dbg(`still running... (${Math.round((Date.now() - start) / 1000)}s)`);
  }

  return { done: false, output: stripAnsi(lastOutput), elapsed: timeoutMs };
}

/**
 * sendTaskBackground(sessionName, task) — send a task prefixed with "& " so
 * Claude treats it as a background/non-blocking operation, then return immediately
 * without waiting for completion.
 *
 * @param {string} sessionName
 * @param {string} task
 * @returns {void}
 */
export function sendTaskBackground(sessionName, task) {
  // Prepend & so the task is dispatched as background work
  const bgTask = task.trimStart().startsWith('&') ? task : `& ${task}`;
  sendKeys(sessionName, bgTask, { pressEnter: true });
}

// ── CLI self-test (run directly: node claude-worker.mjs --test) ───────────────
if (process.argv[1] && process.argv[1].endsWith('claude-worker.mjs')) {
  if (process.argv.includes('--test')) {
    (async () => {
      console.log('[claude-worker] Running self-test...');
      const session = detectSession();
      if (!session) {
        console.error('[claude-worker] No Claude session detected. Start one with: claude');
        process.exit(1);
      }
      console.log(`[claude-worker] Detected session: "${session}"`);

      const result = await sendTask(session, 'echo hello from claude-worker self-test', {
        timeoutMs: 30_000,
        debug: true,
      });

      console.log(`[claude-worker] done=${result.done}  elapsed=${result.elapsed}ms`);
      console.log('--- output (last 20 lines) ---');
      console.log(result.output.split('\n').slice(-20).join('\n'));
      process.exit(result.done ? 0 : 1);
    })();
  }
}
