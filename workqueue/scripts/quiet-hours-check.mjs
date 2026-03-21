#!/usr/bin/env node
/**
 * quiet-hours-check.mjs
 * wq-20260319-015: Quiet hours / do-not-disturb protocol for OpenClaw agents.
 *
 * Quiet hours: 23:00–08:00 America/Los_Angeles (PT).
 *
 * Exports:
 *   isQuietHours()                  — true if current PT time is in quiet window
 *   shouldSkipAction(actionType)    — true if the given action should be skipped now
 */

const QUIET_START = 23; // 23:00 PT
const QUIET_END   =  8; //  08:00 PT
const TZ          = 'America/Los_Angeles';

/**
 * Returns the current hour (0–23) in PT.
 */
function currentHourPT() {
  const now = new Date();
  // toLocaleString with hour12:false gives "HH:MM:SS" we can parse
  const timeStr = now.toLocaleString('en-US', { timeZone: TZ, hour: '2-digit', hour12: false });
  return parseInt(timeStr, 10);
}

/**
 * isQuietHours() — true if PT hour is in [23:00, 08:00) window.
 * The window wraps midnight: quiet when hour >= 23 OR hour < 8.
 */
export function isQuietHours() {
  const hour = currentHourPT();
  return hour >= QUIET_START || hour < QUIET_END;
}

/**
 * shouldSkipAction(actionType) — true if this action should be suppressed now.
 *
 * Action types:
 *   'gpu_task'        — GPU-intensive compute jobs
 *   'slack_ping'      — Slack DMs / channel pings to humans
 *   'noisy_external'  — External API calls, email, loud webhooks
 *
 * During quiet hours all three return true (skip).
 * Outside quiet hours all return false (proceed).
 */
export function shouldSkipAction(actionType) {
  const QUIET_SENSITIVE = new Set(['gpu_task', 'slack_ping', 'noisy_external']);
  if (!QUIET_SENSITIVE.has(actionType)) return false;
  return isQuietHours();
}

// ── CLI self-test ─────────────────────────────────────────────────────────────
if (process.argv[1]?.endsWith('quiet-hours-check.mjs')) {
  const hour   = currentHourPT();
  const quiet  = isQuietHours();
  const types  = ['gpu_task', 'slack_ping', 'noisy_external'];

  console.log(`PT hour     : ${hour}:xx`);
  console.log(`quietHours  : ${quiet}  (window: 23:00–08:00 PT)`);
  console.log('');
  console.log('shouldSkipAction results:');
  for (const t of types) {
    console.log(`  ${t.padEnd(18)} → ${shouldSkipAction(t)}`);
  }
  console.log(`  ${'unknown_type'.padEnd(18)} → ${shouldSkipAction('unknown_type')}`);
}
