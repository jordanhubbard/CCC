#!/usr/bin/env node
/**
 * publish-capabilities.mjs — Publish agent capabilities to RCC at startup
 *
 * Each agent runs this script on startup to register its capability manifest
 * with the RCC API. The script reads from rcc/agents/<name>.capabilities.json
 * and POSTs it to POST /api/agents/:name.
 *
 * Usage:
 *   AGENT_NAME=rocky RCC_API=http://localhost:8789 RCC_AUTH_TOKEN=xxx \
 *     node rcc/scripts/publish-capabilities.mjs
 *
 *   # Or pass the capabilities file directly:
 *   node rcc/scripts/publish-capabilities.mjs /path/to/rocky.capabilities.json
 *
 * Environment:
 *   AGENT_NAME        — agent name (overrides "name" field in caps file)
 *   RCC_API           — RCC base URL (default: http://localhost:8789)
 *   RCC_AUTH_TOKEN    — auth token (or RCC_AUTH_TOKENS comma-separated)
 *   CAPABILITIES_FILE — explicit path to capabilities JSON file
 */

import { readFile } from 'fs/promises';
import { resolve, dirname } from 'path';
import { fileURLToPath } from 'url';

const __dirname = dirname(fileURLToPath(import.meta.url));

const AGENT_NAME  = process.env.AGENT_NAME || process.env.RCC_AGENT_NAME;
const RCC_API     = process.env.RCC_API || process.env.RCC_PUBLIC_URL || 'http://localhost:8789';
const AUTH_TOKEN  = process.env.RCC_AUTH_TOKEN || process.env.RCC_AUTH_TOKENS?.split(',')[0] || '';

// Resolve capabilities file: arg > env > by agent name
let capsFile = process.argv[2]
  || process.env.CAPABILITIES_FILE
  || (AGENT_NAME ? resolve(__dirname, '../agents', `${AGENT_NAME}.capabilities.json`) : null);

if (!capsFile) {
  console.error('[publish-capabilities] Error: set AGENT_NAME or pass a capabilities file path');
  console.error('  Usage: AGENT_NAME=rocky node publish-capabilities.mjs');
  process.exit(1);
}

let caps;
try {
  caps = JSON.parse(await readFile(capsFile, 'utf8'));
} catch (err) {
  console.error(`[publish-capabilities] Cannot read ${capsFile}: ${err.message}`);
  process.exit(1);
}

const name = AGENT_NAME || caps.name;
if (!name) {
  console.error('[publish-capabilities] Error: no agent name — set AGENT_NAME or add "name" to the caps file');
  process.exit(1);
}

// Strip meta fields from the capabilities payload
const { name: _n, ...capabilities } = caps;

const url = `${RCC_API}/api/agents/${encodeURIComponent(name)}`;
const payload = {
  capabilities,
  host: process.env.HOSTNAME || process.env.HOST || 'unknown',
};

try {
  const res = await fetch(url, {
    method: 'POST',
    headers: {
      'Authorization': `Bearer ${AUTH_TOKEN}`,
      'Content-Type':  'application/json',
    },
    body: JSON.stringify(payload),
  });

  const data = await res.json().catch(() => ({}));

  if (res.ok) {
    console.log(`[publish-capabilities] ✓ ${name} capabilities published to ${RCC_API}`);
    if (data.token) {
      console.log(`[publish-capabilities]   agent token: ${data.token}`);
    }
  } else {
    console.error(`[publish-capabilities] RCC returned ${res.status}: ${JSON.stringify(data)}`);
    process.exit(1);
  }
} catch (err) {
  console.error(`[publish-capabilities] Cannot reach RCC at ${RCC_API}: ${err.message}`);
  process.exit(1);
}
