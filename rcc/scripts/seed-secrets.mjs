#!/usr/bin/env node
/**
 * seed-secrets.mjs — Seed the RCC secrets store from the live ~/.rcc/.env
 *
 * Reads secrets that agents need but shouldn't store locally, and POSTs them
 * to the RCC API's secrets store via admin auth.
 *
 * Usage:
 *   node rcc/scripts/seed-secrets.mjs [--dry-run]
 *
 * Requires: RCC_URL and RCC_ADMIN_TOKEN (or RCC_AUTH_TOKENS first token) in ~/.rcc/.env
 */

import { readFile } from 'fs/promises';
import { existsSync } from 'fs';
import { homedir } from 'os';
import { join } from 'path';

const DRY_RUN = process.argv.includes('--dry-run');

// ── Load .env ───────────────────────────────────────────────────────────────
function parseEnv(text) {
  const out = {};
  for (const line of text.split('\n')) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith('#')) continue;
    const eq = trimmed.indexOf('=');
    if (eq < 0) continue;
    const key = trimmed.slice(0, eq).trim();
    const val = trimmed.slice(eq + 1).trim();
    out[key] = val;
  }
  return out;
}

const envPath = join(homedir(), '.rcc', '.env');
if (!existsSync(envPath)) {
  console.error('No ~/.rcc/.env found');
  process.exit(1);
}
const env = parseEnv(await readFile(envPath, 'utf8'));

const RCC_URL = env.RCC_URL || 'http://localhost:8789';
// Admin token: RCC_ADMIN_TOKEN if set; otherwise fallback to first token in RCC_AUTH_TOKENS
const ADMIN_TOKEN = env.RCC_ADMIN_TOKEN || (env.RCC_AUTH_TOKENS || '').split(',')[0]?.trim();

if (!ADMIN_TOKEN) {
  console.error('No admin token found (RCC_ADMIN_TOKEN or RCC_AUTH_TOKENS)');
  process.exit(1);
}

// ── Define which secrets to seed ───────────────────────────────────────────
// Key => env var(s) to read. Objects allow multiple sub-keys.
const SECRETS_MAP = {
  // Slack
  'slack/bot_token_omgjkh':      env.OMGJKH_BOT,
  'slack/bot_token_offtera':     env.OFFTERA_BOT,
  'slack/app_token':             env.SLACK_APP_TOKEN,
  'slack/signing_secret':        env.SLACK_SIGNING_SECRET,
  'slack/omgjkh_user_token':     env.OMGJKH_USER_TOKEN,
  'slack/omgjkh_webhook':        env.OMGJKH_WEBHOOK,
  'slack/watch_channel':         env.WATCH_CHANNEL,

  // Mattermost
  // mattermost/token and mattermost/url retired 2026-04-01

  // NVIDIA (direct gateway — kept for fallback / legacy consumers)
  'nvidia/api_key':              env.NVIDIA_API_KEY,
  'nvidia/api_base':             env.NVIDIA_API_BASE,

  // TokenHub (preferred inference router — aggregates Boris/Sweden + NVIDIA NIM)
  'tokenhub/url':                env.TOKENHUB_URL,
  'tokenhub/agent_key':          env.TOKENHUB_AGENT_KEY,
  'tokenhub/admin_token':        env.TOKENHUB_ADMIN_TOKEN,

  // MinIO
  'minio/endpoint':              env.MINIO_ENDPOINT,
  'minio/access_key':            env.MINIO_ACCESS_KEY,
  'minio/secret_key':            env.MINIO_SECRET_KEY,
  'minio/bucket':                env.MINIO_BUCKET,

  // Azure
  'azure/blob_public_url':       env.AZURE_BLOB_PUBLIC_URL,

  // Milvus
  'milvus/address':              env.MILVUS_ADDRESS,
  'milvus/embed_model':          env.EMBED_MODEL,
  'milvus/embed_dim':            env.EMBED_DIM,
  'milvus/nvidia_embed_url':     env.NVIDIA_EMBED_URL,

  // Peer agent URLs (these are internal network addresses, not really secrets,
  // but centralizing them means agents don't hardcode Tailscale IPs)
  'peers/bullwinkle_url':        env.BULLWINKLE_URL,
  'peers/natasha_url':           env.NATASHA_URL,
};

// ── Seed ───────────────────────────────────────────────────────────────────
let ok = 0, skipped = 0, errors = 0;

for (const [key, value] of Object.entries(SECRETS_MAP)) {
  if (!value) {
    console.log(`  SKIP  ${key}  (no value in .env)`);
    skipped++;
    continue;
  }

  if (DRY_RUN) {
    console.log(`  DRY   ${key}  = ${value.slice(0, 8)}...`);
    ok++;
    continue;
  }

  try {
    const res = await fetch(`${RCC_URL}/api/secrets/${key}`, {
      method: 'POST',
      headers: {
        'Authorization': `Bearer ${ADMIN_TOKEN}`,
        'Content-Type': 'application/json',
      },
      body: JSON.stringify({ value }),
    });
    const data = await res.json();
    if (res.ok) {
      console.log(`  OK    ${key}`);
      ok++;
    } else {
      console.error(`  ERROR ${key}: ${data.error}`);
      errors++;
    }
  } catch (e) {
    console.error(`  ERROR ${key}: ${e.message}`);
    errors++;
  }
}

console.log(`\nDone. ${ok} seeded, ${skipped} skipped, ${errors} errors.`);
if (DRY_RUN) console.log('(dry run — no writes made)');
