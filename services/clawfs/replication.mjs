/**
 * AgentFS distributed replication subscriber
 *
 * Polls ClawBus for agentos.fs.* events and replicates blobs to/from
 * local MinIO, enabling any mesh node to serve all WASM modules fleet-wide.
 *
 * Enable:  AGENTFS_REPLICATE=1
 * Config:  SQUIRRELBUS_URL, AGENTFS_ORIGIN_URL, AGENTFS_TOKEN, AGENTFS_PEER_URLS
 */

import {
  PutObjectCommand,
  HeadObjectCommand,
  DeleteObjectCommand,
} from '@aws-sdk/client-s3';

function sleep(ms) { return new Promise(r => setTimeout(r, ms)); }

async function fetchWithBackoff(url, opts, retries = 3) {
  let delay = 1000;
  for (let attempt = 0; attempt <= retries; attempt++) {
    try {
      const r = await fetch(url, opts);
      if (!r.ok) throw Object.assign(new Error(`HTTP ${r.status}`), { status: r.status });
      return r;
    } catch (err) {
      if (attempt === retries) throw err;
      console.warn(`[agentfs-repl] fetch attempt ${attempt + 1}/${retries + 1} failed: ${err.message}, retry in ${delay}ms`);
      await sleep(delay);
      delay *= 2;
    }
  }
}

async function replicatePut(s3Client, bucket, payload, ownOrigin, fetchToken) {
  const { hash, origin_url } = payload;
  if (!hash || !origin_url) return;

  if (origin_url === ownOrigin) return; // don't self-replicate

  const key = `modules/${hash}.wasm`;

  // Check if already present locally
  try {
    await s3Client.send(new HeadObjectCommand({ Bucket: bucket, Key: key }));
    return; // already have it
  } catch { /* not found — proceed */ }

  console.log(`[agentfs-repl] PUT hash=${hash} from origin ${origin_url}`);

  const r = await fetchWithBackoff(`${origin_url}/agentfs/modules/${hash}`, {
    headers: { Authorization: `Bearer ${fetchToken}` },
  });
  const buf = Buffer.from(await r.arrayBuffer());

  await s3Client.send(new PutObjectCommand({
    Bucket: bucket,
    Key: key,
    Body: buf,
    ContentType: 'application/wasm',
    ContentLength: buf.length,
    Metadata: { hash, size: String(buf.length), replicated_from: origin_url },
  }));
  console.log(`[agentfs-repl] PUT hash=${hash} stored (${buf.length}B) replicated from ${origin_url}`);
}

async function replicateDelete(s3Client, bucket, payload) {
  const { hash } = payload;
  if (!hash) return;
  const key = `modules/${hash}.wasm`;
  try {
    await s3Client.send(new DeleteObjectCommand({ Bucket: bucket, Key: key }));
    console.log(`[agentfs-repl] DELETE hash=${hash} removed from local store`);
  } catch (err) {
    if (err.name !== 'NoSuchKey' && err.$metadata?.httpStatusCode !== 404) {
      console.warn(`[agentfs-repl] DELETE hash=${hash}: ${err.message}`);
    }
  }
}

/**
 * Start the ClawBus replication subscriber.
 * Polls for agentos.fs.* events and replicates blobs to local MinIO.
 *
 * @param {object} s3Client   - AWS S3Client instance
 * @param {string} bucket     - local MinIO bucket name
 * @param {object} opts       - { squirrelbusUrl, ownOriginUrl, fetchToken, signal }
 */
export async function startReplicationSubscriber(s3Client, bucket, opts = {}) {
  const busUrl     = opts.squirrelbusUrl || process.env.SQUIRRELBUS_URL    || 'http://100.89.199.14:8788/bus';
  const ownOrigin  = opts.ownOriginUrl   || process.env.AGENTFS_ORIGIN_URL || 'http://sparky.tail407856.ts.net:8791';
  const fetchToken = opts.fetchToken     || process.env.AGENTFS_TOKEN      || 'agentfs-dev-token';
  const signal     = opts.signal;

  let since = new Date().toISOString();

  console.log(`[agentfs-repl] subscriber started  bus=${busUrl}  own=${ownOrigin}`);

  (async function poll() {
    while (!signal?.aborted) {
      try {
        const pollUrl = `${busUrl}?filter=agentos.fs&since=${encodeURIComponent(since)}`;
        let r;
        try {
          r = await fetch(pollUrl, { signal: AbortSignal.timeout(30_000) });
        } catch (err) {
          if (!signal?.aborted) {
            await sleep(3000);
          }
          continue;
        }

        if (!r.ok) {
          console.warn(`[agentfs-repl] bus poll HTTP ${r.status}`);
          await sleep(5000);
          continue;
        }

        const data = await r.json();
        const messages = Array.isArray(data) ? data : (data.messages || []);

        for (const msg of messages) {
          const type = msg.type;
          let payload;
          try {
            payload = typeof msg.body === 'string' ? JSON.parse(msg.body) : (msg.body ?? msg.payload ?? {});
          } catch {
            payload = {};
          }
          if (msg.ts) since = msg.ts;

          try {
            if (type === 'agentos.fs.put') {
              await replicatePut(s3Client, bucket, payload, ownOrigin, fetchToken);
            } else if (type === 'agentos.fs.delete') {
              await replicateDelete(s3Client, bucket, payload);
            }
          } catch (err) {
            console.error(`[agentfs-repl] error handling ${type} hash=${payload.hash}: ${err.message}`);
          }
        }

        if (messages.length === 0) await sleep(2000);
      } catch (err) {
        if (!signal?.aborted) {
          console.error(`[agentfs-repl] unexpected poll error: ${err.message}`);
          await sleep(5000);
        }
      }
    }
  })().catch(err => {
    if (!signal?.aborted) console.error(`[agentfs-repl] subscriber fatal: ${err.message}`);
  });
}
