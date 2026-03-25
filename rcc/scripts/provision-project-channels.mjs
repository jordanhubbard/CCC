#!/usr/bin/env node
/**
 * provision-project-channels.mjs
 *
 * Creates and configures Slack project channels for repos registered in the RCC.
 * Each channel becomes a first-class project context: channel = project.
 *
 * Usage:
 *   node provision-project-channels.mjs [--repo owner/name] [--workspace workspace1|workspace2|all] [--dry-run]
 *
 * What it does per channel:
 *   1. Creates the channel if it doesn't exist
 *   2. Sets topic with repo URL + description
 *   3. Adds RCC bookmark (links to project page in command center)
 *   4. Adds GitHub bookmark
 *   5. Posts a pinned welcome card
 *   6. Writes the Slack channel ID back to repos.json (ownership.slack_channel)
 */

import { readFile, writeFile } from 'fs/promises';
import { existsSync } from 'fs';
import { fileURLToPath } from 'url';
import { dirname, join, resolve } from 'path';

const __dir = dirname(fileURLToPath(import.meta.url));
const REPOS_PATH = process.env.REPOS_PATH || resolve(__dir, '../api/repos.json');
const RCC_API    = process.env.RCC_API    || 'http://localhost:8789';
const RCC_TOKEN  = process.env.RCC_TOKEN  || '';
const RCC_PUBLIC = process.env.RCC_PUBLIC || 'http://localhost:8788';

// Slack tokens per workspace
const SLACK_TOKENS = {
  workspace1: {
    bot:  process.env.OFFTERA_BOT  || '',
    team: 'THJ9A47K3',
    url:  process.env.SLACK_WORKSPACE1_URL || '',
  },
  workspace2: {
    bot:  process.env.OMGJKH_BOT   || '',
    team: 'TE0V8MBEJ',
    url:  process.env.SLACK_WORKSPACE2_URL || '',
  },
};

// ── Parse args ──────────────────────────────────────────────────────────────
const args      = process.argv.slice(2);
const dryRun    = args.includes('--dry-run');
const force     = args.includes('--force');
const repoIdx   = args.indexOf('--repo');
const repoArg   = repoIdx !== -1 ? args[repoIdx + 1] || null : null;
const wsIdx     = args.indexOf('--workspace');
const wsArg     = wsIdx !== -1 ? args[wsIdx + 1] || 'all' : 'all';

// ── Slack helpers ───────────────────────────────────────────────────────────
async function slack(workspace, method, endpoint, body = null) {
  const ws    = SLACK_TOKENS[workspace];
  const url   = `https://slack.com/api/${endpoint}`;
  const opts  = {
    method,
    headers: {
      'Authorization': `Bearer ${ws.bot}`,
      'Content-Type':  'application/json; charset=utf-8',
    },
  };
  if (body) opts.body = JSON.stringify(body);
  const res  = await fetch(url, opts);
  const data = await res.json();
  if (!data.ok) {
    // channel_already_exists is fine — not an error
    if (data.error === 'channel_already_exists') return { ok: true, exists: true };
    throw new Error(`Slack ${endpoint} [${workspace}]: ${data.error} (needed: ${data.needed || 'n/a'})`);
  }
  return data;
}

async function getOrCreateChannel(workspace, name) {
  // Try to find existing
  let cursor = '';
  do {
    const params = new URLSearchParams({ types: 'public_channel', limit: '200', exclude_archived: 'true' });
    if (cursor) params.set('cursor', cursor);
    const ws  = SLACK_TOKENS[workspace];
    const res = await fetch(`https://slack.com/api/conversations.list?${params}`, {
      headers: { 'Authorization': `Bearer ${ws.bot}` },
    });
    const data = await res.json();
    if (!data.ok) throw new Error(`conversations.list [${workspace}]: ${data.error}`);
    const found = (data.channels || []).find(c => c.name === name);
    if (found) return { id: found.id, existed: true };
    cursor = data.response_metadata?.next_cursor || '';
  } while (cursor);

  // Create it
  if (dryRun) {
    console.log(`  [DRY-RUN] Would create #${name} in ${workspace}`);
    return { id: `DRY-${workspace}-${name}`, existed: false };
  }
  const created = await slack(workspace, 'POST', 'conversations.create', {
    name,
    is_private: false,
  });
  return { id: created.channel.id, existed: false };
}

function channelName(repo) {
  // e.g. "yourorg/myproject" → "project-myproject"
  const base = repo.full_name.split('/')[1].toLowerCase().replace(/[^a-z0-9-]/g, '-');
  return `project-${base}`;
}

function repoGithubUrl(repo) {
  return `https://github.com/${repo.full_name}`;
}

function rccProjectUrl(repo) {
  // Future: when RCC has project pages, deep-link there
  // For now: dashboard root (projects page will be added)
  const slug = repo.full_name.replace('/', '--');
  return `${RCC_PUBLIC}/projects/${encodeURIComponent(repo.full_name)}`;
}

function buildTopic(repo) {
  const parts = [];
  if (repo.description) parts.push(repo.description);
  parts.push(`GitHub: ${repoGithubUrl(repo)}`);
  if (repo.issue_tracker_url) parts.push(`Issues: https://${repo.issue_tracker_url}`);
  parts.push(`RCC: ${rccProjectUrl(repo)}`);
  // Slack topic max 250 chars
  let topic = parts.join(' | ');
  if (topic.length > 250) topic = topic.slice(0, 247) + '…';
  return topic;
}

function buildWelcomeMessage(repo, channelName, workspace) {
  const ws    = SLACK_TOKENS[workspace];
  const lines = [
    `🐿️ *${repo.display_name || repo.full_name}* — project channel`,
    '',
    `*What this channel is:* Post requests here with no project context needed. ` +
    `"Address current merge requests" or "what's the CI status?" — Rocky knows this channel = this project.`,
    '',
    `*GitHub:* ${repoGithubUrl(repo)}`,
  ];
  if (repo.issue_tracker_url) lines.push(`*Issue tracker:* https://${repo.issue_tracker_url}`);
  lines.push(`*RCC dashboard:* ${rccProjectUrl(repo)}`);
  if (repo.description) lines.push('', `_${repo.description}_`);
  const contributors = repo.ownership?.contributors;
  if (Array.isArray(contributors) && contributors.length > 0) {
    const names = contributors.slice(0, 5).map(c => typeof c === 'string' ? c : c.github).join(', ');
    const more  = contributors.length > 5 ? ` +${contributors.length - 5} more` : '';
    lines.push('', `*Contributors:* ${names}${more}`);
  }
  return lines.join('\n');
}

// ── Provision one repo on one workspace ─────────────────────────────────────
async function provisionRepo(repo, workspace) {
  const name = channelName(repo);
  console.log(`\n▶ [${workspace}] #${name} (${repo.full_name})`);

  // 1. Get or create channel
  const { id, existed } = await getOrCreateChannel(workspace, name);
  console.log(`  Channel ${existed ? 'existed' : 'created'}: ${id}`);
  if (dryRun && id.startsWith('DRY-')) {
    console.log('  [DRY-RUN] Skipping remaining steps');
    return { channelId: null, workspace };
  }

  // 2. Set topic
  if (!dryRun) {
    await slack(workspace, 'POST', 'conversations.setTopic', {
      channel: id,
      topic:   buildTopic(repo),
    });
    console.log('  ✓ Topic set');

    // 3. Set purpose
    await slack(workspace, 'POST', 'conversations.setPurpose', {
      channel: id,
      purpose: `${repo.display_name || repo.full_name} project channel. Post requests here — channel context = project context.`,
    });
    console.log('  ✓ Purpose set');

    // 4. Clear existing bookmarks, add fresh ones
    const existingBms = await slack(workspace, 'POST', 'bookmarks.list', { channel_id: id });
    for (const bm of (existingBms.bookmarks || [])) {
      try {
        await slack(workspace, 'POST', 'bookmarks.remove', { channel_id: id, bookmark_id: bm.id });
      } catch {}
    }

    // Add RCC project bookmark
    await slack(workspace, 'POST', 'bookmarks.add', {
      channel_id: id,
      title:      '🐿️ RCC Dashboard',
      link:       rccProjectUrl(repo),
      type:       'link',
    });
    // Add GitHub bookmark
    await slack(workspace, 'POST', 'bookmarks.add', {
      channel_id: id,
      title:      '🐙 GitHub',
      link:       repoGithubUrl(repo),
      type:       'link',
    });
    if (repo.issue_tracker_url) {
      await slack(workspace, 'POST', 'bookmarks.add', {
        channel_id: id,
        title:      '🎫 Issue Tracker',
        link:       `https://${repo.issue_tracker_url}`,
        type:       'link',
      });
    }
    console.log('  ✓ Bookmarks set');

    // 5. Post welcome card (only if channel is new, or forced)
    if (!existed) {
      const msg = await slack(workspace, 'POST', 'chat.postMessage', {
        channel: id,
        text:    buildWelcomeMessage(repo, name, workspace),
      });
      // Pin it
      if (msg.ts) {
        await slack(workspace, 'POST', 'pins.add', {
          channel:   id,
          timestamp: msg.ts,
        });
        console.log('  ✓ Welcome card posted + pinned');
      }
    } else if (force) {
      const msg = await slack(workspace, 'POST', 'chat.postMessage', {
        channel: id,
        text:    buildWelcomeMessage(repo, name, workspace),
      });
      if (msg.ts) {
        await slack(workspace, 'POST', 'pins.add', { channel: id, timestamp: msg.ts });
        console.log('  ✓ Welcome card reposted + pinned (--force)');
      }
    } else {
      console.log('  ↷ Channel existed, skipping welcome card (use --force to repost)');
    }
  }

  return { channelId: id, workspace };
}

// ── Main ─────────────────────────────────────────────────────────────────────
async function main() {
  console.log(`🐿️ Project Channel Provisioner ${dryRun ? '[DRY-RUN] ' : ''}starting…`);

  // Load repos
  const repos = JSON.parse(await readFile(REPOS_PATH, 'utf8'));

  // Filter
  const targets = repoArg
    ? repos.filter(r => r.full_name === repoArg || channelName(r) === repoArg)
    : repos.filter(r => r.enabled !== false);

  if (!targets.length) {
    console.error('No matching repos found.');
    process.exit(1);
  }

  const workspaces = wsArg === 'all' ? Object.keys(SLACK_TOKENS) : [wsArg];

  console.log(`Repos: ${targets.map(r => r.full_name).join(', ')}`);
  console.log(`Workspaces: ${workspaces.join(', ')}`);

  const results = [];
  for (const repo of targets) {
    for (const ws of workspaces) {
      try {
        const r = await provisionRepo(repo, ws);
        results.push({ repo: repo.full_name, ...r, ok: true });
      } catch (err) {
        console.error(`  ✗ Error: ${err.message}`);
        results.push({ repo: repo.full_name, workspace: ws, ok: false, error: err.message });
      }
    }

    // Write back the primary channel ID to repos.json
    const primaryResult = results.find(r => r.repo === repo.full_name && r.workspace === 'workspace1' && r.ok && r.channelId);
    if (primaryResult && !dryRun) {
      if (!repo.ownership) repo.ownership = {};
      repo.ownership.slack_channel   = primaryResult.channelId;
      repo.ownership.slack_workspace = 'workspace1';
      console.log(`  ✓ Wrote channel ID ${primaryResult.channelId} → repos.json`);
    }
  }

  if (!dryRun) {
    await writeFile(REPOS_PATH, JSON.stringify(repos, null, 4));
    console.log('\n✓ repos.json updated');
  }

  console.log('\n── Summary ────────────────────────────────────────────────');
  for (const r of results) {
    const icon = r.ok ? '✓' : '✗';
    console.log(`${icon} [${r.workspace}] ${r.repo} → ${r.channelId || r.error || 'dry-run'}`);
  }
}

main().catch(err => {
  console.error('Fatal:', err);
  process.exit(1);
});
