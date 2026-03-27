#!/usr/bin/env node
/**
 * agent-status-digest.mjs
 * Produces a concise "what has everyone been up to?" digest.
 * Can be run standalone (prints to stdout) or imported for use in server.mjs.
 */

import { readFile } from 'fs/promises';
import { execFile } from 'child_process';
import { promisify } from 'util';

const execFileP = promisify(execFile);

const MC_PATH     = process.env.MC_PATH     || '/home/jkh/.local/bin/mc';
const MINIO_ALIAS = process.env.MINIO_ALIAS || 'local';
const QUEUE_PATH  = process.env.QUEUE_PATH  || '/home/jkh/.openclaw/workspace/workqueue/queue.json';

const AGENTS = [
  { name: 'rocky',     emoji: '🐿️',  healthFile: 'agent-health-rocky.json' },
  { name: 'bullwinkle',emoji: '🫎',   healthFile: 'agent-health-bullwinkle.json' },
  { name: 'natasha',   emoji: '🕵️‍♀️', healthFile: 'agent-health-natasha.json' },
  { name: 'boris',     emoji: '🕵️‍♂️', healthFile: 'agent-heartbeat-boris.json' },
];

// Fetch a JSON file from MinIO, return parsed object or null
async function fetchMinIO(path) {
  try {
    const { stdout } = await execFileP(MC_PATH, ['cat', `${MINIO_ALIAS}/${path}`], { timeout: 6000 });
    return JSON.parse(stdout);
  } catch {
    return null;
  }
}

// Determine online status from health/heartbeat data
function onlineStatus(health) {
  if (!health) return 'unknown';
  const tsField = health.ts || health.timestamp || health.lastTs || null;
  if (!tsField) return 'unknown';
  const ageMs = Date.now() - new Date(tsField).getTime();
  if (ageMs < 40 * 60 * 1000) return 'online';
  if (ageMs < 4 * 60 * 60 * 1000) return 'idle';
  return 'offline';
}

export async function buildDigest() {
  // Load queue
  let queueData = { items: [], completed: [] };
  try {
    const raw = await readFile(QUEUE_PATH, 'utf8');
    queueData = JSON.parse(raw);
  } catch { /* queue unavailable */ }

  const items     = queueData.items     || [];
  const completed = queueData.completed || [];

  const now      = Date.now();
  const day24h   = now - 24 * 60 * 60 * 1000;
  const day7d    = now - 7  * 24 * 60 * 60 * 1000;

  // Build per-agent stats from queue
  function agentStats(agentName) {
    const isAssigned = item =>
      item.assignee === agentName ||
      item.assignee === 'all' ||
      item.claimedBy === agentName;

    const claimedBy = item => item.claimedBy === agentName;

    const done24h = completed.filter(i =>
      claimedBy(i) && i.completedAt && new Date(i.completedAt).getTime() > day24h
    ).length;

    const done7d = completed.filter(i =>
      claimedBy(i) && i.completedAt && new Date(i.completedAt).getTime() > day7d
    ).length;

    const inProgress = items.filter(i =>
      (i.status === 'claimed' || i.status === 'in-progress') && claimedBy(i)
    );

    const pending = items.filter(i =>
      i.status === 'pending' && isAssigned(i)
    );

    return { done24h, done7d, inProgress, pending };
  }

  // Fetch health data for all agents in parallel
  const healthResults = await Promise.all(
    AGENTS.map(a => fetchMinIO(`agents/shared/${a.healthFile}`))
  );

  const agentData = AGENTS.map((a, i) => ({
    ...a,
    health:  healthResults[i],
    status:  onlineStatus(healthResults[i]),
    stats:   agentStats(a.name),
  }));

  // Queue stats
  const totalPending   = items.filter(i => i.status === 'pending').length;
  const totalClaimed   = items.filter(i => i.status === 'claimed' || i.status === 'in-progress').length;
  const totalCompleted = completed.length;
  const totalIdeas     = items.filter(i => i.status === 'idea').length;

  // Format digest text
  const ts = new Date().toISOString().replace('T', ' ').slice(0, 19) + ' UTC';
  const lines = [`📊 Agent Status Digest — ${ts}`, ''];

  for (const a of agentData) {
    const { done24h, done7d, inProgress, pending } = a.stats;
    const statusLabel = a.status === 'online' ? 'online' : a.status === 'idle' ? 'idle' : a.status === 'offline' ? 'offline' : 'unknown';
    lines.push(`${a.emoji} ${a.name.charAt(0).toUpperCase() + a.name.slice(1)} (${statusLabel}): ${done24h} done today, ${done7d} this week`);

    if (inProgress.length > 0) {
      inProgress.forEach(i => lines.push(`  ▸ In progress: [${i.id}] ${i.title}`));
    }
    if (pending.length > 0) {
      const shown = pending.slice(0, 3);
      shown.forEach(i => lines.push(`  ▸ Pending: [${i.id}] ${i.title}`));
      if (pending.length > 3) lines.push(`  ▸ … and ${pending.length - 3} more pending`);
    }
    if (inProgress.length === 0 && pending.length === 0) {
      lines.push(`  ▸ Nothing assigned`);
    }
    lines.push('');
  }

  lines.push(`Queue: ${totalPending} pending, ${totalClaimed} in-progress, ${totalIdeas} ideas, ${totalCompleted} completed total`);

  const digest = lines.join('\n');

  // Structured agents object
  const agents = {};
  for (const a of agentData) {
    agents[a.name] = {
      status:   a.status,
      done24h:  a.stats.done24h,
      done7d:   a.stats.done7d,
      inProgress: a.stats.inProgress.map(i => ({ id: i.id, title: i.title })),
      pending:    a.stats.pending.slice(0, 5).map(i => ({ id: i.id, title: i.title })),
    };
  }

  return {
    digest,
    agents,
    queueStats: { totalPending, totalClaimed, totalIdeas, totalCompleted },
    ts,
  };
}

// Standalone usage
if (process.argv[1] && process.argv[1].endsWith('agent-status-digest.mjs')) {
  buildDigest().then(result => {
    console.log(result.digest);
  }).catch(err => {
    console.error('Digest error:', err.message);
    process.exit(1);
  });
}
