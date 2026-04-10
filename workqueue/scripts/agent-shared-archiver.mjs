#!/usr/bin/env node
/**
 * agent-shared-archiver.mjs
 * Archive ClawBus #ops channel messages to MinIO.
 *
 * Reads text messages from CCC /bus/messages (filtered to subject=ops),
 * uploads as JSON to MinIO at: agents/shared/bus-ops-archive-YYYY-MM-DD.json
 *
 * Formerly archived Mattermost #agent-shared; now archives ClawBus #ops.
 * Updated 2026-04-10.
 *
 * Usage: node agent-shared-archiver.mjs [--date YYYY-MM-DD] [--channel ops]
 */

import { execFileSync } from 'child_process';
import { writeFileSync, unlinkSync } from 'fs';

const CCC_URL   = process.env.CCC_URL   || 'http://localhost:8789';
const CCC_TOKEN = process.env.CCC_AGENT_TOKEN || '';
const CHANNEL   = process.argv.find(a => a.startsWith('--channel='))?.split('=')[1] ||
  process.argv[process.argv.indexOf('--channel') + 1] || 'ops';
const MC = process.env.MC_BIN || 'mc';
const MINIO_ALIAS = process.env.MINIO_ALIAS || 'local';

function getDate() {
  const dateArg = process.argv.find(a => a === '--date');
  if (dateArg) {
    const d = process.argv[process.argv.indexOf('--date') + 1];
    if (d && /^\d{4}-\d{2}-\d{2}$/.test(d)) return d;
  }
  return new Date().toLocaleDateString('en-CA', { timeZone: 'America/Los_Angeles' });
}

function mcPut(localPath, remotePath) {
  try {
    execFileSync(MC, ['cp', localPath, remotePath], { stdio: ['pipe', 'pipe', 'pipe'] });
    return true;
  } catch (e) {
    console.error('[archiver] MinIO upload failed:', e.message);
    return false;
  }
}

async function main() {
  if (!CCC_TOKEN) {
    console.error('[archiver] CCC_AGENT_TOKEN not set — cannot fetch messages.');
    process.exit(1);
  }

  const today = getDate();
  const now   = new Date().toISOString();
  const limit = 1000;

  console.log(`[archiver] Fetching ClawBus text messages (subject=${CHANNEL}, limit=${limit})...`);

  const res = await fetch(
    `${CCC_URL}/bus/messages?type=text&limit=${limit}`,
    { headers: { Authorization: `Bearer ${CCC_TOKEN}` } }
  );

  if (!res.ok) {
    console.error(`[archiver] Failed to fetch messages: ${res.status} ${await res.text()}`);
    process.exit(1);
  }

  const allMessages = await res.json();

  // Filter to the target channel (subject field)
  const messages = (Array.isArray(allMessages) ? allMessages : [])
    .filter(m => !CHANNEL || m.subject === CHANNEL || m.subject === `#${CHANNEL}`)
    .map(m => ({
      id:      m.id,
      ts:      m.ts,
      from:    m.from,
      to:      m.to,
      subject: m.subject,
      body:    m.body,
      mime:    m.mime || 'text/plain',
    }));

  const archive = {
    archived_at:   now,
    archive_date:  today,
    channel:       CHANNEL,
    source:        'clawbus',
    ccc_url:       CCC_URL,
    message_count: messages.length,
    messages,
  };

  const tmpPath = `/tmp/bus-${CHANNEL}-archive-${today}.json`;
  writeFileSync(tmpPath, JSON.stringify(archive, null, 2), 'utf8');
  console.log(`[archiver] Wrote ${messages.length} messages to ${tmpPath}`);

  const remotePath = `${MINIO_ALIAS}/agents/shared/bus-${CHANNEL}-archive-${today}.json`;
  const ok = mcPut(tmpPath, remotePath);

  if (ok) {
    console.log(`[archiver] Uploaded to MinIO: agents/shared/bus-${CHANNEL}-archive-${today}.json`);
  } else {
    console.error(`[archiver] WARNING: MinIO upload failed — local copy at ${tmpPath}`);
    process.exit(1);
  }

  try { unlinkSync(tmpPath); } catch {}

  console.log(`[archiver] Done. ${messages.length} messages archived for ${today}.`);
}

main().catch(e => {
  console.error('[archiver] FATAL:', e.message);
  process.exit(1);
});
