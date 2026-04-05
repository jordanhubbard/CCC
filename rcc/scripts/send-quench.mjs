#!/usr/bin/env node
/**
 * rcc/scripts/send-quench.mjs — CLI to send an agent quench/pause signal via ClawBus
 *
 * Usage:
 *   node send-quench.mjs <target> <duration_minutes> [reason]
 *   node send-quench.mjs all 10 "deploying new vLLM model"
 *   node send-quench.mjs peabody 5
 *
 * Arguments:
 *   target           — agent name or 'all' (required)
 *   duration_minutes — 1–30 (required)
 *   reason           — optional human note (quoted string)
 *
 * Environment:
 *   SQUIRRELBUS_URL  — bus URL (default: http://localhost:8788)
 *   CCC_AUTH_TOKEN   — bearer token for auth (required)
 *   AGENT_NAME       — sender identity (default: 'cli')
 */

import { sendQuench, MAX_QUENCH_MINUTES } from '../exec/quench.mjs';

const args = process.argv.slice(2);

if (args.length < 2 || args[0] === '--help' || args[0] === '-h') {
  console.log(`Usage: node send-quench.mjs <target> <duration_minutes> [reason]

  target           — agent name or 'all'
  duration_minutes — 1–${MAX_QUENCH_MINUTES} (hard cap enforced server-side)
  reason           — optional human note

Examples:
  node send-quench.mjs all 10 "deploying new model"
  node send-quench.mjs peabody 5
  node send-quench.mjs boris 2 "rebooting vLLM"

Environment:
  SQUIRRELBUS_URL   bus URL (default: http://localhost:8788)
  CCC_AUTH_TOKEN    bearer token (required)
  AGENT_NAME        sender name (default: cli)
`);
  process.exit(0);
}

const [target, durationStr, ...reasonParts] = args;
const duration_minutes = parseInt(durationStr, 10);
const reason = reasonParts.join(' ') || undefined;

if (isNaN(duration_minutes) || duration_minutes < 1) {
  console.error(`Error: duration_minutes must be a positive integer (got: ${durationStr})`);
  process.exit(1);
}

if (duration_minutes > MAX_QUENCH_MINUTES) {
  console.error(`Error: duration_minutes ${duration_minutes} exceeds hard cap of ${MAX_QUENCH_MINUTES}`);
  process.exit(1);
}

if (!process.env.AGENT_NAME) {
  process.env.AGENT_NAME = 'cli';
}

try {
  const result = await sendQuench({ target, duration_minutes, reason });
  console.log('Quench signal sent:', JSON.stringify(result, null, 2));
} catch (err) {
  console.error('Failed to send quench:', err.message);
  process.exit(1);
}
