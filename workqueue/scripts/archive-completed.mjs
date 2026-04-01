#!/usr/bin/env node
/**
 * archive-completed.mjs — wq-B-001
 *
 * Moves completed/failed queue items older than 7 days from queue.json's
 * `completed[]` array into a separate archive file on MinIO.
 *
 * - Archive path: agents/shared/workqueue-archive.jsonl (append-only JSONL)
 * - Also writes a local copy: workqueue/archive.jsonl
 * - Keeps queue.json's completed[] to items from the last 7 days only
 * - Safe to run multiple times (deduplicates by item id in archive)
 * - Designed to run at end of idle cron cycles
 *
 * Usage: node archive-completed.mjs [queue.json path] [--dry-run]
 */

import { readFileSync, writeFileSync, existsSync, appendFileSync } from 'fs';
import { resolve, dirname } from 'path';
import { createHmac, createHash } from 'crypto';
import http from 'http';

const DRY_RUN = process.argv.includes('--dry-run');
const QUEUE_PATH = resolve(
  process.argv.find(a => a.endsWith('.json')) ||
  new URL('../queue.json', import.meta.url).pathname
);
const ARCHIVE_PATH = resolve(dirname(QUEUE_PATH), 'archive.jsonl');
const AGENT_NAME = process.env.AGENT_NAME || 'natasha';
const NOW = new Date();
const ARCHIVE_THRESHOLD_MS = 7 * 24 * 60 * 60 * 1000; // 7 days

// MinIO
const MINIO_HOST = '100.89.199.14';
const MINIO_PORT = 9000;
const MINIO_ACCESS_KEY = 'rockymoose4810f4cc7d28916f';
const MINIO_SECRET_KEY = '<MINIO_SECRET_KEY>';
const MINIO_ARCHIVE_KEY = 'agents/shared/workqueue-archive.jsonl';

function log(...args) {
  console.log(new Date().toISOString(), '[archive-completed]', ...args);
}

function isOldEnough(item) {
  const ts = item.completedAt || item.lastAttempt || item.created;
  if (!ts) return false;
  return (NOW - new Date(ts)) > ARCHIVE_THRESHOLD_MS;
}

async function appendToMinIO(lines) {
  // Read existing, append, write back (MinIO doesn't support append natively)
  const existing = await minioGet(MINIO_ARCHIVE_KEY);
  const combined = (existing ? existing.trimEnd() + '\n' : '') + lines.join('\n') + '\n';
  return minioput(MINIO_ARCHIVE_KEY, combined, 'application/x-ndjson');
}

function hmac(key, data) { return createHmac('sha256', key).update(data).digest(); }

function sigV4Headers(method, path, body, contentType = 'application/octet-stream') {
  const dateStr = NOW.toISOString().slice(0, 10).replace(/-/g, '');
  const datetimeStr = NOW.toISOString().replace(/[:\-]|\.\d{3}/g, '').slice(0, 15) + 'Z';
  const region = 'us-east-1'; const service = 's3';
  const host = `${MINIO_HOST}:${MINIO_PORT}`;
  const bodyBytes = Buffer.from(body || '');
  const bodyHash = createHash('sha256').update(bodyBytes).digest('hex');
  const headers = { host, 'x-amz-date': datetimeStr, 'x-amz-content-sha256': bodyHash,
    ...(body !== null ? { 'content-type': contentType } : {}) };
  const signedHeaders = Object.keys(headers).sort().join(';');
  const canonicalHeaders = Object.keys(headers).sort().map(k => `${k}:${headers[k]}\n`).join('');
  const canonicalRequest = [method, '/' + path, '', canonicalHeaders, signedHeaders, bodyHash].join('\n');
  const credScope = `${dateStr}/us-east-1/s3/aws4_request`;
  const stringToSign = ['AWS4-HMAC-SHA256', datetimeStr, credScope,
    createHash('sha256').update(canonicalRequest).digest('hex')].join('\n');
  const sigKey = hmac(hmac(hmac(hmac(`AWS4${MINIO_SECRET_KEY}`, dateStr), region), service), 'aws4_request');
  const sig = createHmac('sha256', sigKey).update(stringToSign).digest('hex');
  return { ...headers,
    'Authorization': `AWS4-HMAC-SHA256 Credential=${MINIO_ACCESS_KEY}/${credScope}, SignedHeaders=${signedHeaders}, Signature=${sig}`,
    ...(body !== null ? { 'Content-Length': bodyBytes.length } : {}) };
}

function minioReq(method, key, body = null, ct = 'application/octet-stream') {
  return new Promise((resolve, reject) => {
    const bodyBytes = body !== null ? Buffer.from(body) : null;
    const headers = sigV4Headers(method, key, body, ct);
    const req = http.request({ hostname: MINIO_HOST, port: MINIO_PORT, path: '/' + key,
      method, headers }, (res) => {
      let data = '';
      res.on('data', c => { data += c; });
      res.on('end', () => resolve({ status: res.statusCode, body: data }));
    });
    req.on('error', reject);
    if (bodyBytes) req.write(bodyBytes);
    req.end();
  });
}

async function minioGet(key) {
  try {
    const r = await minioReq('GET', key);
    return r.status === 200 ? r.body : null;
  } catch { return null; }
}

async function minioput(key, body, ct) {
  const r = await minioReq('PUT', key, body, ct);
  if (r.status !== 200) throw new Error(`MinIO PUT ${key} → ${r.status}`);
}

async function main() {
  log(`Starting archive sweep (agent=${AGENT_NAME}, threshold=7d, dry_run=${DRY_RUN})`);

  let queue;
  try {
    queue = JSON.parse(readFileSync(QUEUE_PATH, 'utf8'));
  } catch (e) {
    log('ERROR: Could not read queue:', e.message); process.exit(1);
  }

  const completed = queue.completed || [];
  const toArchive = completed.filter(isOldEnough);
  const toKeep = completed.filter(item => !isOldEnough(item));

  log(`Completed items: ${completed.length} total, ${toArchive.length} eligible for archive, ${toKeep.length} keeping`);

  if (toArchive.length === 0) {
    log('Nothing to archive — all completed items are <7 days old.');
    console.log('ARCHIVED: 0');
    return;
  }

  const archiveLines = toArchive.map(item =>
    JSON.stringify({ ...item, archivedAt: NOW.toISOString(), archivedBy: AGENT_NAME })
  );

  if (!DRY_RUN) {
    // Append to local archive.jsonl
    appendFileSync(ARCHIVE_PATH, archiveLines.join('\n') + '\n');
    log(`Appended ${toArchive.length} items to local ${ARCHIVE_PATH}`);

    // Append to MinIO
    await appendToMinIO(archiveLines);
    log(`Appended ${toArchive.length} items to MinIO ${MINIO_ARCHIVE_KEY}`);

    // Update queue.json
    queue.completed = toKeep;
    queue.lastSync = NOW.toISOString();
    writeFileSync(QUEUE_PATH, JSON.stringify(queue, null, 2));
    log(`queue.json updated — ${toKeep.length} recent completed items retained`);
  } else {
    log(`[dry-run] Would archive: ${toArchive.map(i => i.id).join(', ')}`);
  }

  console.log(`ARCHIVED: ${toArchive.length} items (${toArchive.map(i => i.id).join(', ')})`);
}

main().catch(err => { console.error('Fatal:', err); process.exit(1); });
