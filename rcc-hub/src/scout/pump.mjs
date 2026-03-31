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
const SLACK_TOKEN        = process.env.SLACK_TOKEN || '';
const WATCH_CHANNEL      = process.env.WATCH_CHANNEL || ''; // set WATCH_CHANNEL to your Slack channel ID

// ── Slack posting ──────────────────────────────────────────────────────────
async function slackPost(channel, text, blocks) {
  if (!SLACK_TOKEN) return; // no-op if token not configured
  try {
    const body = { channel, text };
    if (blocks) body.blocks = blocks;
    const r = await fetch('https://slack.com/api/chat.postMessage', {
      method: 'POST',
      headers: { 'Authorization': `Bearer ${SLACK_TOKEN}`, 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
    });
    const j = await r.json();
    if (!j.ok) console.error('[pump] Slack post failed:', j.error);
  } catch (err) {
    console.error('[pump] Slack post error:', err.message);
  }
}

// Format a batch of new work items for Slack
function formatNewItemsForSlack(newItems, repoMap) {
  if (newItems.length === 0) return null;

  // Group by repo
  const byRepo = {};
  for (const item of newItems) {
    const repo = item.repo || item.tags?.find(t => t.includes('/')) || 'unknown';
    if (!byRepo[repo]) byRepo[repo] = [];
    byRepo[repo].push(item);
  }

  const lines = [`🔍 *Scout found ${newItems.length} new work item${newItems.length > 1 ? 's' : ''}*`];
  for (const [repo, items] of Object.entries(byRepo)) {
    const repoInfo = repoMap[repo];
    const kind = repoInfo?.kind === 'team' ? '👥' : '👤';
    lines.push(`\n${kind} *${repo}*`);
    for (const item of items.slice(0, 5)) {
      const pri = item.priority === 'high' ? '🔴' : item.priority === 'medium' ? '🟡' : '⚪';
      lines.push(`  ${pri} ${item.title}`);
    }
    if (items.length > 5) lines.push(`  _…and ${items.length - 5} more_`);
  }
  return lines.join('\n');
}

// Format CI failure alert
function formatCIAlert(repo, failedJobs) {
  return `🚨 *CI failure* in \`${repo}\`\nFailed: ${failedJobs.map(j => `\`${j}\``).join(', ')}\n<https://github.com/${repo}/actions|View runs>`;
}

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
    return 0;
  }

  const enabledRepos = repos.filter(r => r.enabled !== false);
  const repoNames = enabledRepos.map(r => r.full_name);
  const repoMap = Object.fromEntries(enabledRepos.map(r => [r.full_name, r]));
  console.log(`[pump] Scanning ${repoNames.length} repos: ${repoNames.join(', ')}`);

  // Get current queue for dedup — include completed[] so scout doesn't re-create finished work
  const { items = [], completed = [] } = await rccGet('/api/queue');
  const allItems = [...items, ...completed];

  // Run scouts
  const newItems = await scout(repoNames, allItems);
  console.log(`[pump] Scout found ${newItems.length} new items`);

  // Create items via API
  let created = 0;
  let deduped = 0;
  const createdItems = [];
  for (const item of newItems) {
    try {
      const result = await rccPost('/api/queue', item);
      if (result.ok) { created++; createdItems.push(item); }
      else if (result.duplicate) { deduped++; } // server-side scout_key dedup
      else { console.warn(`[pump] Item not created (ok=false): "${item.title}"`, result); }
    } catch (err) {
      console.error(`[pump] Failed to create item "${item.title}": ${err.message}`);
    }
  }
  if (deduped > 0) console.log(`[pump] Suppressed ${deduped} duplicate scout items (server-side dedup)`);

  console.log(`[pump] Created ${created} new work items`);

  // Post to Slack #watch-projects if anything interesting found
  if (created > 0) {
    const msg = formatNewItemsForSlack(createdItems, repoMap);
    if (msg) await slackPost(WATCH_CHANNEL, msg);
  }

  // Separately alert on CI failures (high-priority items)
  const ciFailures = createdItems.filter(i => i.tags?.includes('ci-failure') || i.title?.includes('CI fail'));
  for (const item of ciFailures) {
    const repo = item.repo || item.tags?.find(t => t.includes('/')) || 'unknown';
    await slackPost(WATCH_CHANNEL, formatCIAlert(repo, [item.title]));
  }

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
    // repoSpec: full project object — see repos.json schema
    // Required: full_name
    // Optional: kind (personal|team|org), display_name, description,
    //           ownership { model, owner, contributors, slack_channel, triaging_agent },
    //           issue_tracker (github|beads|jira|linear|none),
    //           scouts, enabled, notes
    const repos = await readRepos();
    const existing = repos.findIndex(r => r.full_name === repoSpec.full_name);

    const defaults = {
      full_name: repoSpec.full_name,
      platform: repoSpec.platform || 'github',
      kind: repoSpec.kind || 'personal',
      display_name: repoSpec.display_name || repoSpec.full_name.split('/')[1],
      description: repoSpec.description || '',
      ownership: repoSpec.ownership || {
        model: 'sole',
        owner: repoSpec.full_name.split('/')[0],
        contributors: [repoSpec.full_name.split('/')[0]],
        slack_channel: null,
        triaging_agent: process.env.PRIMARY_AGENT || 'rocky',
      },
      issue_tracker: repoSpec.issue_tracker || 'github',
      scouts: repoSpec.scouts || ['issues', 'prs', 'ci', 'deps', 'analysis'],
      enabled: repoSpec.enabled !== false,
      registeredAt: new Date().toISOString(),
      notes: repoSpec.notes || '',
    };

    if (existing >= 0) {
      repos[existing] = { ...repos[existing], ...repoSpec };
    } else {
      repos.push(defaults);
    }
    await writeRepos(repos);
    return repos.find(r => r.full_name === repoSpec.full_name);
  }

  // Update a single field or sub-object on an existing repo
  async patchRepo(fullName, patch) {
    const repos = await readRepos();
    const idx = repos.findIndex(r => r.full_name === fullName);
    if (idx < 0) throw new Error(`Repo not found: ${fullName}`);
    // Deep-merge ownership if provided
    if (patch.ownership) {
      patch.ownership = { ...repos[idx].ownership, ...patch.ownership };
    }
    repos[idx] = { ...repos[idx], ...patch, updatedAt: new Date().toISOString() };
    await writeRepos(repos);
    return repos[idx];
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
