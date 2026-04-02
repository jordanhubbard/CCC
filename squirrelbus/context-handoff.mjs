/**
 * context-handoff.mjs — Smart Context Handoff via ClawBus
 *
 * When jkh says "tell Rocky about X" or "brief Bullwinkle on the storage topology",
 * any agent can call this to package up context and relay it via ClawBus.
 *
 * Usage:
 *   node context-handoff.mjs --to rocky --topic "storage topology"
 *   node context-handoff.mjs --to bullwinkle --topic "Taiwan render specs" --wq RENDER-002
 *   node context-handoff.mjs --to all --topic "jkh wants 24/7 mode"
 *
 * wq-B-013 implementation by Natasha (sparky), 2026-03-21
 */

import { readFileSync, existsSync } from 'fs';
import { createHmac, createHash } from 'crypto';

const BUS_SEND_URL = 'http://100.89.199.14:8788/bus/send';
const BUS_TOKEN = '<YOUR_BUS_TOKEN>';
const MINIO_HOST = '100.89.199.14:9000';
const MINIO_KEY = 'rockymoose4810f4cc7d28916f';
const MINIO_SECRET = '<MINIO_SECRET_KEY>';
const QUEUE_API = 'http://146.190.134.110:8788/api/queue';

const AGENT_ENDPOINTS = {
  rocky: 'https://do-host1.tail407856.ts.net/bus/receive',
  bullwinkle: 'https://puck.tail407856.ts.net/bus/receive',
  boris: null, // Boris has no Tailscale — routed via Rocky dashboard push
};

// Parse CLI args
const args = process.argv.slice(2);
const getArg = (flag) => {
  const idx = args.indexOf(flag);
  return idx !== -1 ? args[idx + 1] : null;
};

const to = getArg('--to');
const topic = getArg('--topic');
const wqId = getArg('--wq');
const fromAgent = getArg('--from') || 'natasha';
const extraContext = getArg('--context');

if (!to || !topic) {
  console.error('Usage: node context-handoff.mjs --to <agent|all> --topic "<topic>" [--wq <item-id>] [--context "<extra>"] [--from <agent>]');
  process.exit(1);
}

// ---- MinIO SigV4 helper ----
function hmac(key, data) {
  return createHmac('sha256', key).update(data).digest();
}
function hmacHex(key, data) {
  return createHmac('sha256', key).update(data).digest('hex');
}
function sha256Hex(data) {
  return createHash('sha256').update(data).digest('hex');
}

async function minioGet(bucket, key) {
  const method = 'GET';
  const now = new Date();
  const datestamp = now.toISOString().slice(0, 10).replace(/-/g, '');
  const amzdate = now.toISOString().replace(/[:\-]|\.\d{3}/g, '').slice(0, 15) + 'Z';
  const region = 'us-east-1';
  const service = 's3';
  const host = MINIO_HOST;
  const uri = `/${bucket}/${key}`;

  const payloadHash = sha256Hex('');
  const canonicalHeaders = `host:${host}\nx-amz-content-sha256:${payloadHash}\nx-amz-date:${amzdate}\n`;
  const signedHeaders = 'host;x-amz-content-sha256;x-amz-date';
  const canonicalRequest = `${method}\n${uri}\n\n${canonicalHeaders}\n${signedHeaders}\n${payloadHash}`;

  const credentialScope = `${datestamp}/${region}/${service}/aws4_request`;
  const stringToSign = `AWS4-HMAC-SHA256\n${amzdate}\n${credentialScope}\n${sha256Hex(canonicalRequest)}`;

  const signingKey = hmac(hmac(hmac(hmac(`AWS4${MINIO_SECRET}`, datestamp), region), service), 'aws4_request');
  const signature = hmacHex(signingKey, stringToSign);
  const authHeader = `AWS4-HMAC-SHA256 Credential=${MINIO_KEY}/${credentialScope}, SignedHeaders=${signedHeaders}, Signature=${signature}`;

  const resp = await fetch(`http://${MINIO_HOST}${uri}`, {
    headers: {
      'Authorization': authHeader,
      'x-amz-date': amzdate,
      'x-amz-content-sha256': payloadHash,
    },
  });

  if (!resp.ok) throw new Error(`MinIO GET ${uri} → ${resp.status}`);
  return resp.json();
}

// ---- Gather context ----
async function gatherContext(topic, wqId) {
  const sections = [];

  // 1. WorkQueue item context (if specified)
  if (wqId) {
    try {
      const resp = await fetch(QUEUE_API);
      const data = await resp.json();
      const item = data.items?.find(i => i.id === wqId);
      if (item) {
        sections.push(`## WorkQueue Item: ${item.id}\n**${item.title}**\nStatus: ${item.status} | Priority: ${item.priority}\n\n${item.description || ''}\n\nNotes: ${item.notes || 'none'}\nResult: ${item.result || 'pending'}`);
      }
    } catch (e) {
      sections.push(`(Could not fetch workqueue item ${wqId}: ${e.message})`);
    }
  }

  // 2. Recent completed items relevant to topic (keyword match)
  try {
    const resp = await fetch(QUEUE_API);
    const data = await resp.json();
    const keywords = topic.toLowerCase().split(/\s+/).filter(w => w.length > 3);
    const relevant = (data.items || [])
      .filter(i => i.status === 'completed' && !i.id.startsWith('wq-crash'))
      .filter(i => {
        const text = `${i.title} ${i.description || ''} ${i.tags?.join(' ') || ''}`.toLowerCase();
        return keywords.some(k => text.includes(k));
      })
      .slice(0, 3);

    if (relevant.length > 0) {
      const summary = relevant.map(i =>
        `- **${i.id}** (${i.claimedBy || 'unclaimed'}): ${i.title} → ${(i.result || '').slice(0, 100)}`
      ).join('\n');
      sections.push(`## Related Completed Work\n${summary}`);
    }
  } catch (e) {
    // non-fatal
  }

  // 3. jkh intents
  try {
    const intents = await minioGet('agents', 'shared/jkh-intents.json');
    const keywords = topic.toLowerCase().split(/\s+/).filter(w => w.length > 3);
    const relevant = (intents.intents || [])
      .filter(i => {
        const text = `${i.intent} ${i.phrase} ${i.tags?.join(' ') || ''}`.toLowerCase();
        return keywords.some(k => text.includes(k));
      })
      .slice(0, 2);

    if (relevant.length > 0) {
      const summary = relevant.map(i => `- "${i.phrase}" → ${i.intent}`).join('\n');
      sections.push(`## jkh Intents (related)\n${summary}`);
    }
  } catch (e) {
    // non-fatal
  }

  // 4. Extra context from caller
  if (extraContext) {
    sections.push(`## Additional Context\n${extraContext}`);
  }

  return sections.join('\n\n');
}

// ---- Send via ClawBus ----
async function sendViaBus(targetAgent, subject, body) {
  const payload = {
    from: fromAgent,
    to: targetAgent,
    type: 'context-handoff',
    subject,
    body,
    ts: new Date().toISOString(),
  };

  // Post to Rocky's bus (which fan-outs to other agents)
  const resp = await fetch(BUS_SEND_URL, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      'Authorization': `Bearer ${BUS_TOKEN}`,
    },
    body: JSON.stringify(payload),
  });

  if (!resp.ok) {
    const text = await resp.text();
    throw new Error(`Bus send failed: ${resp.status} ${text}`);
  }

  return resp.json();
}

// ---- Main ----
async function main() {
  console.log(`📦 Context handoff: from=${fromAgent} to=${to} topic="${topic}"`);

  const contextBody = await gatherContext(topic, wqId);

  const subject = `Context handoff: ${topic}`;
  const body = `# Context Handoff from ${fromAgent}\n\n**Topic:** ${topic}\n\n${contextBody}`;

  const targets = to === 'all'
    ? Object.keys(AGENT_ENDPOINTS).filter(a => a !== fromAgent)
    : [to];

  for (const target of targets) {
    try {
      await sendViaBus(target, subject, body);
      console.log(`✅ Sent to ${target}`);
    } catch (e) {
      console.error(`❌ Failed to send to ${target}: ${e.message}`);
    }
  }

  console.log('Done.');
}

main().catch(e => {
  console.error('Fatal:', e.message);
  process.exit(1);
});
