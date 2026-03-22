/**
 * rcc/scout/pump.mjs — The Work Pump
 *
 * Keeps all agents busy 24/7:
 * 1. Periodically scans registered repos for work
 * 2. When queue goes idle (below threshold), fires emergency scan
 * 3. If still idle after scan, asks agents to self-audit for more work
 * 4. Verifies completed items (did CI pass? PR merged?)
 *
 * Runs as part of rcc-api or standalone.
 */

import { scout } from './github.mjs';
import { readFile, writeFile } from 'fs/promises';
import { existsSync } from 'fs';

// ── Config ─────────────────────────────────────────────────────────────────
const REPOS_PATH         = process.env.REPOS_PATH || './repos.json';
const QUEUE_PATH         = process.env.QUEUE_PATH || '../../workqueue/queue.json';
const RCC_API            = process.env.RCC_API_INTERNAL || 'http://localhost:8789';
const RCC_TOKEN          = process.env.RCC_AUTH_TOKENS?.split(',')[0] || '';
const IDLE_THRESHOLD     = parseInt(process.env.PUMP_IDLE_THRESHOLD || '3', 10);  // items per agent
const SCAN_INTERVAL_MS   = parseInt(process.env.PUMP_SCAN_INTERVAL_MS || String(6 * 60 * 60 * 1000), 10); // 6h
const IDLE_CHECK_MS      = parseInt(process.env.PUMP_IDLE_CHECK_MS || String(30 * 60 * 1000), 10); // 30min

// ── Repo registry I/O ──────────────────────────────────────────────────────
async function readRepos() {
  if (!existsSync(REPOS_PATH)) return [];
  return JSON.parse(await readFile(REPOS_PATH, 'utf8'));
}

async function writeRepos(repos) {
  await writeFile(REPOS_PATH, JSON.stringify(repos, null, 2));
}

// ── RCC API helpers ────────────────────────────────────────────────────────
async function rccGet(path) {
  const r = await fetch(`${RCC_API}${path}`);
  return r.json();
}

async function rccPost(path, body) {
  const r = await fetch(`${RCC_API}${path}`, {
    method: 'POST',
    headers: { 'Authorization': `Bearer ${RCC_TOKEN}`, 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  });
  return r.json();
}

// ── Core pump logic ────────────────────────────────────────────────────────

async function runScan() {
  const repos = await readRepos();
  if (repos.length === 0) {
    console.log('[pump] No repos registered — nothing to scan');
    return;
  }

  const repoNames = repos.filter(r => r.enabled !== false).map(r => r.full_name);
  console.log(`[pump] Scanning ${repoNames.length} repos: ${repoNames.join(', ')}`);

  // Get current queue for dedup
  const { items = [] } = await rccGet('/api/queue');
  const allItems = [...items];

  // Run scouts
  const newItems = await scout(repoNames, allItems);
  console.log(`[pump] Scout found ${newItems.length} new items`);

  // Create items via API
  let created = 0;
  for (const item of newItems) {
    try {
      await rccPost('/api/queue', item);
      created++;
    } catch (err) {
      console.error(`[pump] Failed to create item "${item.title}": ${err.message}`);
    }
  }

  console.log(`[pump] Created ${created} new work items`);
  return created;
}

async function checkIdle() {
  const { items = [] } = await rccGet('/api/queue');
  const agents = await rccGet('/api/agents');
  const agentCount = Math.max(agents.length, 1);

  const activeItems = items.filter(i => !['completed','cancelled'].includes(i.status));
  const itemsPerAgent = activeItems.length / agentCount;

  console.log(`[pump] Queue check: ${activeItems.length} active items / ${agentCount} agents = ${itemsPerAgent.toFixed(1)} per agent (threshold: ${IDLE_THRESHOLD})`);

  if (itemsPerAgent < IDLE_THRESHOLD) {
    console.log('[pump] Queue below idle threshold — triggering emergency scan');
    const created = await runScan();

    if (created === 0) {
      // Still nothing — ask agents to self-audit
      console.log('[pump] Still idle after scan — requesting agent self-audit');
      await rccPost('/api/queue', {
        title: 'Agent self-audit: find new work in registered repos',
        description: `The work queue is running low (${activeItems.length} items for ${agentCount} agents). All registered repos have been scanned and nothing new was found automatically.\n\nEach agent should:\n1. Browse their assigned repos for improvement opportunities\n2. Look for undocumented features, missing tests, performance issues, UX problems\n3. File new work items for anything worth doing\n4. Suggest new repos to register if relevant work exists elsewhere`,
        assignee: 'all',
        priority: 'low',
        preferred_executor: 'claude_cli',
        source: 'pump:idle',
        tags: ['meta', 'self-audit', 'idle-trigger'],
      });
    }
  }
}

// ── Completion verification ────────────────────────────────────────────────

async function verifyCompletions() {
  const { completed = [] } = await rccGet('/api/queue');

  // Check recently completed GitHub items
  const recentGitHub = completed.filter(i => {
    if (!i.tags?.includes('github')) return false;
    if (!i.completedAt) return false;
    const age = Date.now() - new Date(i.completedAt).getTime();
    return age < 24 * 60 * 60 * 1000; // within last 24h
  });

  for (const item of recentGitHub) {
    // Extract issue/PR number from scout_key or tags
    const prTag = item.tags?.find(t => t.includes(':pr:'));
    const issueTag = item.tags?.find(t => t.includes(':issue:'));

    if (prTag) {
      // scout:owner/repo:pr:123
      const [, repoPath, , num] = prTag.split(':');
      try {
        const state = execSync(`gh pr view ${num} --repo ${repoPath} --json state --jq .state`, { encoding: 'utf8' }).trim();
        if (state !== 'MERGED') {
          console.log(`[pump] Verification: PR #${num} in ${repoPath} is ${state}, not MERGED — reopening item`);
          // TODO: reopen item with verification failure note
        }
      } catch { /* ignore */ }
    }
  }
}

// ── Public API ─────────────────────────────────────────────────────────────

export class Pump {
  constructor() {
    this.running = false;
    this._scanTimer = null;
    this._idleTimer = null;
  }

  async registerRepo(repoSpec) {
    // repoSpec: { full_name, platform, scouts, enabled, notes }
    const repos = await readRepos();
    const existing = repos.findIndex(r => r.full_name === repoSpec.full_name);
    if (existing >= 0) {
      repos[existing] = { ...repos[existing], ...repoSpec };
    } else {
      repos.push({
        full_name: repoSpec.full_name,
        platform: repoSpec.platform || 'github',
        scouts: repoSpec.scouts || ['issues', 'prs', 'ci', 'deps', 'analysis'],
        enabled: repoSpec.enabled !== false,
        registeredAt: new Date().toISOString(),
        notes: repoSpec.notes || '',
      });
    }
    await writeRepos(repos);
    return repos.find(r => r.full_name === repoSpec.full_name);
  }

  async listRepos() {
    return readRepos();
  }

  async scan() {
    return runScan();
  }

  start() {
    if (this.running) return;
    this.running = true;

    // Initial scan after 30s (let API settle)
    setTimeout(() => runScan(), 30000);

    // Periodic full scan
    this._scanTimer = setInterval(runScan, SCAN_INTERVAL_MS);

    // Idle check
    this._idleTimer = setInterval(checkIdle, IDLE_CHECK_MS);

    console.log(`[pump] Started. Scan interval: ${SCAN_INTERVAL_MS/3600000}h, Idle check: ${IDLE_CHECK_MS/60000}min`);
  }

  stop() {
    this.running = false;
    clearInterval(this._scanTimer);
    clearInterval(this._idleTimer);
    console.log('[pump] Stopped.');
  }
}

// ── CLI ────────────────────────────────────────────────────────────────────

if (process.argv[1] === new URL(import.meta.url).pathname) {
  const cmd = process.argv[2];

  if (cmd === 'scan') {
    await runScan();
  } else if (cmd === 'idle-check') {
    await checkIdle();
  } else if (cmd === 'register') {
    const pump = new Pump();
    const repo = await pump.registerRepo({
      full_name: process.argv[3],
      platform: 'github',
    });
    console.log('Registered:', repo);
  } else if (cmd === 'list') {
    const repos = await readRepos();
    console.log(JSON.stringify(repos, null, 2));
  } else {
    console.log('Usage: node rcc/scout/pump.mjs scan|idle-check|register <repo>|list');
  }
}
