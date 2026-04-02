#!/usr/bin/env node
/**
 * peer-status-check.mjs — wq-20260319-007
 *
 * Called during workqueue sync cycles. For each known peer:
 *   1. Attempts a health/reachability check
 *   2. Writes result to MinIO agents/shared/peer-status.json
 *   3. If a peer has been offline >30 min, notifies jkh via Slack DM
 *
 * Usage: node peer-status-check.mjs [--dry-run]
 *
 * Designed to be called by any agent (natasha/rocky/bullwinkle).
 * Each agent writes its own view; CCC dashboard can aggregate.
 */

import https from 'https';
import http from 'http';

const DRY_RUN = process.argv.includes('--dry-run');
const AGENT_NAME = process.env.AGENT_NAME || 'natasha';

// MinIO config
const MINIO_HOST = '100.89.199.14';
const MINIO_PORT = 9000;
const MINIO_ACCESS_KEY = 'rockymoose4810f4cc7d28916f';
const MINIO_SECRET_KEY = '<MINIO_SECRET_KEY>';
const MINIO_BUCKET = 'agents';
const PEER_STATUS_KEY = 'agents/shared/peer-status.json';

// Slack config (for jkh DM alerts)
const SLACK_JKH_USER = 'UDYR7H4SC';

// Known peers
const PEERS = {
  rocky: {
    healthUrl: 'http://146.190.134.110:8788/api/health',
    fallbackUrl: 'http://100.89.199.14:8788/api/health',
    label: 'Rocky (do-host1)',
  },
  bullwinkle: {
    healthUrl: 'https://puck.tail407856.ts.net/v1/chat/completions',
    method: 'OPTIONS',
    label: 'Bullwinkle (puck)',
  },
};

const OFFLINE_ALERT_THRESHOLD_MS = 30 * 60 * 1000; // 30 minutes

function log(...args) {
  console.log(new Date().toISOString(), '[peer-status]', ...args);
}

// Simple HTTP GET with timeout
function httpGet(url, timeoutMs = 5000) {
  return new Promise((resolve) => {
    const lib = url.startsWith('https') ? https : http;
    const req = lib.get(url, { timeout: timeoutMs }, (res) => {
      resolve({ ok: res.statusCode < 500, status: res.statusCode });
    });
    req.on('error', (e) => resolve({ ok: false, error: e.message }));
    req.on('timeout', () => { req.destroy(); resolve({ ok: false, error: 'timeout' }); });
  });
}

// AWS SigV4 signing for MinIO
async function minioRequest(method, key, body = null) {
  const { createHmac, createHash } = await import('crypto');

  const now = new Date();
  const dateStr = now.toISOString().slice(0, 10).replace(/-/g, '');
  const datetimeStr = now.toISOString().replace(/[:\-]|\.\d{3}/g, '').slice(0, 15) + 'Z';
  const region = 'us-east-1';
  const service = 's3';
  const host = `${MINIO_HOST}:${MINIO_PORT}`;
  const path = `/${key}`;
  const bodyBytes = body ? Buffer.from(body) : Buffer.alloc(0);
  const bodyHash = createHash('sha256').update(bodyBytes).digest('hex');

  const headers = {
    host,
    'x-amz-date': datetimeStr,
    'x-amz-content-sha256': bodyHash,
    ...(body ? { 'content-type': 'application/json' } : {}),
  };

  const signedHeaders = Object.keys(headers).sort().join(';');
  const canonicalHeaders = Object.keys(headers).sort().map(k => `${k}:${headers[k]}\n`).join('');
  const canonicalRequest = [method, path, '', canonicalHeaders, signedHeaders, bodyHash].join('\n');
  const credentialScope = `${dateStr}/${region}/${service}/aws4_request`;
  const stringToSign = ['AWS4-HMAC-SHA256', datetimeStr, credentialScope,
    createHash('sha256').update(canonicalRequest).digest('hex')].join('\n');

  function hmac(key, data) { return createHmac('sha256', key).update(data).digest(); }
  const signingKey = hmac(hmac(hmac(hmac(`AWS4${MINIO_SECRET_KEY}`, dateStr), region), service), 'aws4_request');
  const signature = createHmac('sha256', signingKey).update(stringToSign).digest('hex');

  const authHeader = `AWS4-HMAC-SHA256 Credential=${MINIO_ACCESS_KEY}/${credentialScope}, SignedHeaders=${signedHeaders}, Signature=${signature}`;

  return new Promise((resolve, reject) => {
    const options = {
      hostname: MINIO_HOST,
      port: MINIO_PORT,
      path,
      method,
      headers: {
        ...headers,
        Authorization: authHeader,
        ...(body ? { 'Content-Length': bodyBytes.length } : {}),
      },
    };

    const req = http.request(options, (res) => {
      let data = '';
      res.on('data', chunk => { data += chunk; });
      res.on('end', () => resolve({ status: res.statusCode, body: data }));
    });
    req.on('error', reject);
    if (body) req.write(bodyBytes);
    req.end();
  });
}

async function readPeerStatus() {
  try {
    const res = await minioRequest('GET', PEER_STATUS_KEY);
    if (res.status === 200) return JSON.parse(res.body);
  } catch (_) {}
  return { peers: {}, lastUpdated: null };
}

async function writePeerStatus(data) {
  if (DRY_RUN) { log('[dry-run] would write peer-status.json:', JSON.stringify(data)); return; }
  const body = JSON.stringify(data, null, 2);
  const res = await minioRequest('PUT', PEER_STATUS_KEY, body);
  if (res.status !== 200) log('WARN: MinIO write returned', res.status);
}

async function sendSlackDM(userId, text) {
  if (DRY_RUN) { log('[dry-run] would Slack DM', userId, ':', text); return; }
  // Use the OpenClaw gateway to deliver slack message
  const payload = JSON.stringify({
    model: 'openclaw:main',
    messages: [{
      role: 'user',
      content: `[SYSTEM ALERT - peer-status-check] Please send this Slack DM to jkh (user ${userId}): ${text}`
    }],
    stream: false,
  });

  return new Promise((resolve) => {
    const req = http.request({
      hostname: '127.0.0.1',
      port: 18789,
      path: '/v1/chat/completions',
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        'Authorization': process.env.TOKENHUB_API_KEY || '<YOUR_TOKENHUB_KEY>',
        'Content-Length': Buffer.byteLength(payload),
      },
    }, (res) => {
      res.resume();
      resolve({ status: res.statusCode });
    });
    req.on('error', (e) => resolve({ error: e.message }));
    req.write(payload);
    req.end();
  });
}

async function main() {
  const now = new Date();
  log(`Starting peer status check (agent=${AGENT_NAME}, dry_run=${DRY_RUN})`);

  // Load current status from MinIO
  const statusData = await readPeerStatus();
  if (!statusData.peers) statusData.peers = {};

  const alerts = [];

  for (const [peerId, peer] of Object.entries(PEERS)) {
    log(`Checking ${peerId} (${peer.label})...`);

    let reachable = false;
    let errorMsg = null;

    // Try primary URL
    const result = await httpGet(peer.healthUrl);
    if (result.ok) {
      reachable = true;
    } else {
      errorMsg = result.error || `HTTP ${result.status}`;
      // Try fallback if available
      if (peer.fallbackUrl) {
        const fb = await httpGet(peer.fallbackUrl);
        if (fb.ok) { reachable = true; errorMsg = null; }
      }
    }

    const existing = statusData.peers[peerId] || {};
    const wasOnline = existing.status === 'online';
    const offlineSince = existing.offlineSince || null;

    if (reachable) {
      statusData.peers[peerId] = {
        status: 'online',
        label: peer.label,
        lastSeen: now.toISOString(),
        checkedBy: AGENT_NAME,
        offlineSince: null,
        alertSent: false,
      };
      log(`  ${peerId}: online ✅`);
    } else {
      const newOfflineSince = offlineSince || now.toISOString();
      const offlineDurationMs = now - new Date(newOfflineSince);
      const shouldAlert = offlineDurationMs > OFFLINE_ALERT_THRESHOLD_MS && !existing.alertSent;

      statusData.peers[peerId] = {
        status: 'offline',
        label: peer.label,
        lastSeen: existing.lastSeen || null,
        checkedBy: AGENT_NAME,
        offlineSince: newOfflineSince,
        error: errorMsg,
        alertSent: existing.alertSent || false,
      };

      const offlineMinutes = Math.round(offlineDurationMs / 60000);
      log(`  ${peerId}: offline ❌ (since ${newOfflineSince}, ${offlineMinutes}m, error: ${errorMsg})`);

      if (shouldAlert) {
        alerts.push({ peerId, peer, offlineMinutes, newOfflineSince });
        statusData.peers[peerId].alertSent = true;
      }
    }
  }

  statusData.lastUpdated = now.toISOString();
  statusData.lastCheckedBy = AGENT_NAME;

  await writePeerStatus(statusData);
  log('peer-status.json written to MinIO');

  // Send alerts
  for (const { peerId, peer, offlineMinutes, newOfflineSince } of alerts) {
    const msg = `⚠️ Agent offline alert: *${peer.label}* (${peerId}) has been unreachable for ${offlineMinutes} minutes (since ${new Date(newOfflineSince).toLocaleString('en-US', { timeZone: 'America/Los_Angeles' })} PT). Check the dashboard: http://146.190.134.110:8788/`;
    log(`Sending alert for ${peerId}: ${msg}`);
    await sendSlackDM(SLACK_JKH_USER, msg);
  }

  log(`Done. ${Object.keys(PEERS).length} peers checked, ${alerts.length} alerts sent.`);

  // Output summary for workqueue result
  const summary = Object.entries(statusData.peers).map(([id, s]) =>
    `${id}: ${s.status} (checked by ${s.checkedBy})`
  ).join(', ');
  console.log(`RESULT: ${summary}`);
}

main().catch(err => { console.error('Fatal:', err); process.exit(1); });
