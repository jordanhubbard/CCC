#!/usr/bin/env node
/**
 * openclaw-register.mjs — Register this agent in CCC and send periodic heartbeats.
 *
 * On startup: POST /api/agents/register with name, host, capabilities, version, vllm_port.
 * Then loops every 60s: POST /api/agents/:name/heartbeat
 *
 * Usage:
 *   node rcc/scripts/openclaw-register.mjs
 *   node rcc/scripts/openclaw-register.mjs --once    # register once and exit
 *
 * Environment:
 *   AGENT_NAME      — agent name (default: "rocky")
 *   AGENT_HOST      — agent hostname (default: os.hostname())
 *   CCC_URL         — CCC base URL (default: http://localhost:8789)
 *   CCC_AUTH_TOKEN  — optional auth token
 *   VLLM_PORT       — if set, included in registration
 *   AGENT_VERSION   — optional version string
 *   SLACK_ID        — optional Slack member ID
 */

import os from 'os';

const AGENT_NAME    = process.env.AGENT_NAME    || 'rocky';
const AGENT_HOST    = process.env.AGENT_HOST    || os.hostname();
const CCC_URL       = (process.env.CCC_URL      || 'http://localhost:8789').replace(/\/$/, '');
const AUTH_TOKEN    = process.env.CCC_AUTH_TOKEN || process.env.CCC_AGENT_TOKEN || '';
const VLLM_PORT     = process.env.VLLM_PORT ? parseInt(process.env.VLLM_PORT, 10) : undefined;
const AGENT_VERSION = process.env.AGENT_VERSION || undefined;
const SLACK_ID      = process.env.SLACK_ID      || undefined;
const ONCE          = process.argv.includes('--once');
const HEARTBEAT_MS  = 60_000;

function authHeaders() {
  const h = { 'Content-Type': 'application/json' };
  if (AUTH_TOKEN) h['Authorization'] = `Bearer ${AUTH_TOKEN}`;
  return h;
}

function buildCapabilities() {
  const caps = ['openclaw', 'claude'];
  if (VLLM_PORT) caps.push('vllm');
  return caps;
}

async function register() {
  const payload = {
    name:         AGENT_NAME,
    host:         AGENT_HOST,
    capabilities: buildCapabilities(),
  };
  if (VLLM_PORT)     payload.vllm_port = VLLM_PORT;
  if (AGENT_VERSION) payload.version   = AGENT_VERSION;
  if (SLACK_ID)      payload.slack_id  = SLACK_ID;

  const res = await fetch(`${CCC_URL}/api/agents/register`, {
    method:  'POST',
    headers: authHeaders(),
    body:    JSON.stringify(payload),
  });

  if (!res.ok) {
    const text = await res.text().catch(() => '');
    throw new Error(`register failed: ${res.status} ${text}`);
  }

  const data = await res.json();
  console.log(`[openclaw-register] registered ${AGENT_NAME} at ${AGENT_HOST} (caps: ${buildCapabilities().join(', ')})`);
  return data;
}

async function heartbeat() {
  const res = await fetch(`${CCC_URL}/api/agents/${encodeURIComponent(AGENT_NAME)}/heartbeat`, {
    method:  'POST',
    headers: authHeaders(),
    body:    JSON.stringify({ host: AGENT_HOST }),
  });

  if (!res.ok) {
    const text = await res.text().catch(() => '');
    console.error(`[openclaw-register] heartbeat failed: ${res.status} ${text}`);
    return false;
  }
  return true;
}

// Main
try {
  await register();
} catch (err) {
  console.error(`[openclaw-register] ERROR: ${err.message}`);
  process.exit(1);
}

if (ONCE) {
  console.log('[openclaw-register] --once: done.');
  process.exit(0);
}

console.log(`[openclaw-register] heartbeat loop started (every ${HEARTBEAT_MS / 1000}s)`);

setInterval(async () => {
  try {
    const ok = await heartbeat();
    if (ok) {
      console.log(`[openclaw-register] heartbeat ok (${new Date().toISOString()})`);
    }
  } catch (err) {
    console.error(`[openclaw-register] heartbeat error: ${err.message}`);
  }
}, HEARTBEAT_MS);
