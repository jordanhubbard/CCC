#!/usr/bin/env node
/**
 * agent-shared-archiver.mjs
 * wq-R-005: Archive Mattermost #agent-shared channel to MinIO
 *
 * Reads the last 100 messages from #agent-shared (channel ID: sjkptm1ot3naijobynuqdcexyh)
 * on the Mattermost server and uploads the archive as JSON to MinIO at:
 *   agents/shared/agent-shared-archive-YYYY-MM-DD.json
 *
 * Usage: node agent-shared-archiver.mjs [--date YYYY-MM-DD]
 */

import { execSync, execFileSync } from 'child_process';
import { writeFileSync, mkdirSync, existsSync, unlinkSync } from 'fs';
import { join, dirname } from 'path';
import { fileURLToPath } from 'url';
import https from 'https';

const __dirname = dirname(fileURLToPath(import.meta.url));
const WORKSPACE = join(__dirname, '../../');
const MC = process.env.MC_BIN || '/home/jkh/.local/bin/mc';
const MINIO_ALIAS = process.env.MINIO_ALIAS || 'local';
const SHARED_PREFIX = `${MINIO_ALIAS}/agents/shared`;

const MM_SERVER = 'https://chat.yourmom.photos';
const MM_TOKEN  = process.env.MATTERMOST_TOKEN || '';
const CHANNEL_ID = 'sjkptm1ot3naijobynuqdcexyh';
const PER_PAGE = 100;

function getDate() {
  const dateArg = process.argv.find(a => a.startsWith('--date'));
  if (dateArg) {
    const d = dateArg.split('=')[1] || process.argv[process.argv.indexOf(dateArg) + 1];
    if (d && /^\d{4}-\d{2}-\d{2}$/.test(d)) return d;
  }
  return new Date().toLocaleDateString('en-CA', { timeZone: 'America/Los_Angeles' });
}

function mmGet(path) {
  return new Promise((resolve, reject) => {
    const url = new URL(path, MM_SERVER);
    const options = {
      hostname: url.hostname,
      port: url.port || 443,
      path: url.pathname + url.search,
      method: 'GET',
      headers: {
        'Authorization': `Bearer ${MM_TOKEN}`,
        'Content-Type': 'application/json'
      }
    };
    const req = https.request(options, res => {
      let data = '';
      res.on('data', chunk => data += chunk);
      res.on('end', () => {
        try {
          resolve({ status: res.statusCode, body: JSON.parse(data) });
        } catch (e) {
          resolve({ status: res.statusCode, body: data });
        }
      });
    });
    req.on('error', reject);
    req.end();
  });
}

function mcPut(localPath, remotePath) {
  try {
    execFileSync(MC, ['cp', localPath, remotePath], { stdio: ['pipe', 'pipe', 'pipe'] });
    return true;
  } catch (e) {
    console.error('[minio] upload failed:', e.message);
    return false;
  }
}

async function main() {
  const today = getDate();
  const now = new Date().toISOString();

  console.log(`[agent-shared-archiver] Fetching last ${PER_PAGE} messages from #agent-shared...`);

  // Fetch posts from Mattermost
  const postsResp = await mmGet(
    `/api/v4/channels/${CHANNEL_ID}/posts?page=0&per_page=${PER_PAGE}`
  );

  if (postsResp.status !== 200) {
    console.error(`[agent-shared-archiver] ERROR: Mattermost API returned ${postsResp.status}:`, postsResp.body);
    process.exit(1);
  }

  const postsData = postsResp.body;

  // postsData.order is array of post IDs in reverse-chronological order
  // postsData.posts is a map of id → post object
  const order = postsData.order || [];
  const postsMap = postsData.posts || {};

  // Build sorted array (oldest first)
  const messages = order
    .map(id => postsMap[id])
    .filter(Boolean)
    .sort((a, b) => a.create_at - b.create_at)
    .map(p => ({
      id: p.id,
      create_at: p.create_at,
      create_at_iso: new Date(p.create_at).toISOString(),
      user_id: p.user_id,
      channel_id: p.channel_id,
      message: p.message,
      type: p.type || '',
      props: p.props || {}
    }));

  const archive = {
    archived_at: now,
    archive_date: today,
    channel_id: CHANNEL_ID,
    server: MM_SERVER,
    message_count: messages.length,
    messages
  };

  // Write to temp file
  const tmpPath = `/tmp/agent-shared-archive-${today}.json`;
  writeFileSync(tmpPath, JSON.stringify(archive, null, 2), 'utf8');
  console.log(`[agent-shared-archiver] Wrote ${messages.length} messages to ${tmpPath}`);

  // Upload to MinIO
  const remotePath = `${SHARED_PREFIX}/agent-shared-archive-${today}.json`;
  const ok = mcPut(tmpPath, remotePath);

  if (ok) {
    console.log(`[agent-shared-archiver] Uploaded to MinIO: agents/shared/agent-shared-archive-${today}.json`);
  } else {
    console.error(`[agent-shared-archiver] WARNING: MinIO upload failed — local copy at ${tmpPath}`);
    process.exit(1);
  }

  // Clean up temp file
  try { unlinkSync(tmpPath); } catch {}

  console.log(`[agent-shared-archiver] Done. ${messages.length} messages archived for ${today}.`);
  process.exit(0);
}

main().catch(e => {
  console.error('[agent-shared-archiver] FATAL:', e.message);
  process.exit(1);
});
