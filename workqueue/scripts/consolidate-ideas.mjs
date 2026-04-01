#!/usr/bin/env node
/**
 * consolidate-ideas.mjs — wq-B-002
 *
 * Groups idea-priority items into epics based on keyword/tag overlap.
 * Produces a consolidated view written to workqueue/ideas-consolidated.md
 * and agents/shared/ideas-consolidated.json on MinIO.
 *
 * Does NOT delete or merge items — just groups them for visibility.
 * Agents can use this to spot overlapping ideas before proposing new ones.
 *
 * Usage: node consolidate-ideas.mjs [queue.json path] [--dry-run]
 */

import { readFileSync, writeFileSync } from 'fs';
import { resolve, dirname } from 'path';
import { createHmac, createHash } from 'crypto';
import http from 'http';

const DRY_RUN = process.argv.includes('--dry-run');
const QUEUE_PATH = resolve(
  process.argv.find(a => a.endsWith('.json')) ||
  new URL('../queue.json', import.meta.url).pathname
);
const OUTPUT_PATH = resolve(dirname(QUEUE_PATH), 'ideas-consolidated.md');
const AGENT_NAME = process.env.AGENT_NAME || 'natasha';
const NOW = new Date().toISOString();

// MinIO
const MINIO_HOST = '100.89.199.14';
const MINIO_PORT = 9000;
const MINIO_ACCESS_KEY = 'rockymoose4810f4cc7d28916f';
const MINIO_SECRET_KEY = '<MINIO_SECRET_KEY>';

function log(...args) { console.log(new Date().toISOString(), '[consolidate-ideas]', ...args); }

// Epic keyword clusters — items matching these keywords group together
const EPIC_CLUSTERS = [
  {
    epic: 'workqueue-ops',
    label: 'Workqueue Operations',
    keywords: ['workqueue', 'queue', 'sync', 'archive', 'voting', 'escalat', 'claim', 'epic', 'consolidat'],
  },
  {
    epic: 'agent-infra',
    label: 'Agent Infrastructure',
    keywords: ['session', 'continuity', 'heartbeat', 'budget', 'offline', 'peer', 'status', 'alert'],
  },
  {
    epic: 'gpu-render',
    label: 'GPU / Render',
    keywords: ['render', 'blender', 'gpu', 'rtx', 'cuda', 'overnight', 'blend'],
  },
  {
    epic: 'agent-ux',
    label: 'Agent UX / Behaviour',
    keywords: ['quiet', 'hours', 'do-not-disturb', 'notify', 'verbose', 'briefing', 'digest'],
  },
];

function scoreItem(item, cluster) {
  const text = `${item.title} ${item.description || ''} ${(item.tags || []).join(' ')} ${item.epic || ''}`.toLowerCase();
  return cluster.keywords.filter(kw => text.includes(kw)).length;
}

function clusterItems(items) {
  const grouped = {};
  const unclaimed = [];

  for (const item of items) {
    let bestCluster = null;
    let bestScore = 0;
    for (const cluster of EPIC_CLUSTERS) {
      const score = scoreItem(item, cluster);
      if (score > bestScore) { bestScore = score; bestCluster = cluster; }
    }
    if (bestCluster && bestScore > 0) {
      if (!grouped[bestCluster.epic]) grouped[bestCluster.epic] = { ...bestCluster, items: [] };
      grouped[bestCluster.epic].items.push(item);
    } else {
      unclaimed.push(item);
    }
  }

  return { grouped, unclaimed };
}

function renderMarkdown(grouped, unclaimed, allIdeas) {
  const lines = [
    `# Workqueue Idea Backlog — Consolidated View`,
    ``,
    `_Generated ${NOW} by ${AGENT_NAME}. ${allIdeas.length} ideas total._`,
    ``,
    `> This file groups idea-priority items by theme. It does not replace queue.json — use it to spot overlaps before proposing new items.`,
    ``,
  ];

  for (const cluster of Object.values(grouped)) {
    lines.push(`## ${cluster.label} (\`${cluster.epic}\`)`);
    lines.push('');
    for (const item of cluster.items) {
      const votes = (item.votes || []).length;
      const voteStr = votes > 0 ? ` — ${votes} vote${votes > 1 ? 's' : ''} (${item.votes.join(', ')})` : '';
      lines.push(`- **[${item.id}]** ${item.title}${voteStr}`);
      if (item.description) lines.push(`  _${item.description}_`);
    }
    lines.push('');
  }

  if (unclaimed.length > 0) {
    lines.push('## Unclustered');
    lines.push('');
    for (const item of unclaimed) {
      lines.push(`- **[${item.id}]** ${item.title}`);
    }
    lines.push('');
  }

  lines.push('---');
  lines.push(`_To promote an idea: add your name to its \`votes[]\` in your next WORKQUEUE_SYNC. See [VOTING_PROTOCOL.md](VOTING_PROTOCOL.md)._`);

  return lines.join('\n');
}

async function writeToMinIO(key, body, ct = 'application/json') {
  const bodyBytes = Buffer.from(body);
  const dateStr = new Date().toISOString().slice(0,10).replace(/-/g,'');
  const datetimeStr = new Date().toISOString().replace(/[:\-]|\.\d{3}/g,'').slice(0,15)+'Z';
  const region='us-east-1', service='s3', host=`${MINIO_HOST}:${MINIO_PORT}`;
  const bodyHash = createHash('sha256').update(bodyBytes).digest('hex');
  const hdrs = { host, 'x-amz-date': datetimeStr, 'x-amz-content-sha256': bodyHash, 'content-type': ct };
  const sh = Object.keys(hdrs).sort().join(';');
  const ch = Object.keys(hdrs).sort().map(k=>`${k}:${hdrs[k]}\n`).join('');
  const cr = ['PUT', '/'+key, '', ch, sh, bodyHash].join('\n');
  const cs = `${dateStr}/${region}/${service}/aws4_request`;
  const s2s = ['AWS4-HMAC-SHA256', datetimeStr, cs, createHash('sha256').update(cr).digest('hex')].join('\n');
  function hmac(k,d){return createHmac('sha256',k).update(d).digest();}
  const sk = hmac(hmac(hmac(hmac(`AWS4${MINIO_SECRET_KEY}`,dateStr),region),service),'aws4_request');
  const sig = createHmac('sha256',sk).update(s2s).digest('hex');
  const auth = `AWS4-HMAC-SHA256 Credential=${MINIO_ACCESS_KEY}/${cs}, SignedHeaders=${sh}, Signature=${sig}`;

  return new Promise((resolve, reject) => {
    const req = http.request({ hostname: MINIO_HOST, port: MINIO_PORT, path: '/'+key,
      method: 'PUT', headers: { ...hdrs, Authorization: auth, 'Content-Length': bodyBytes.length }
    }, res => { res.resume(); resolve(res.statusCode); });
    req.on('error', reject);
    req.write(bodyBytes); req.end();
  });
}

async function main() {
  let queue;
  try { queue = JSON.parse(readFileSync(QUEUE_PATH, 'utf8')); }
  catch (e) { log('ERROR:', e.message); process.exit(1); }

  const allIdeas = (queue.items || []).filter(i =>
    i.priority === 'idea' || i.priority === 'low'
  );

  log(`Found ${allIdeas.length} idea-priority items`);

  const { grouped, unclaimed } = clusterItems(allIdeas);

  // Summary
  for (const [epic, cluster] of Object.entries(grouped)) {
    log(`  ${cluster.label}: ${cluster.items.map(i=>i.id).join(', ')}`);
  }
  if (unclaimed.length) log(`  Unclustered: ${unclaimed.map(i=>i.id).join(', ')}`);

  const markdown = renderMarkdown(grouped, unclaimed, allIdeas);
  const jsonOutput = { generatedAt: NOW, generatedBy: AGENT_NAME, totalIdeas: allIdeas.length,
    clusters: Object.values(grouped).map(c => ({
      epic: c.epic, label: c.label,
      items: c.items.map(i => ({ id: i.id, title: i.title, votes: i.votes || [] }))
    })),
    unclustered: unclaimed.map(i => ({ id: i.id, title: i.title }))
  };

  if (!DRY_RUN) {
    writeFileSync(OUTPUT_PATH, markdown);
    log(`Written: ${OUTPUT_PATH}`);
    const s = await writeToMinIO('agents/shared/ideas-consolidated.json', JSON.stringify(jsonOutput, null, 2));
    log(`MinIO write: ${s}`);
  } else {
    log('[dry-run] Output:\n' + markdown);
  }

  console.log('CLUSTERS:', Object.keys(grouped).join(', ') || 'none');
}

main().catch(e => { console.error('Fatal:', e); process.exit(1); });
