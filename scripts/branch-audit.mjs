#!/usr/bin/env node
// branch-audit.mjs — daily branch lifecycle audit
// Runs daily at 09:00 PT (configured in Rocky crontab)
// Checks all remote branches not merged into main, flags orphans, escalates old ones.

import { execSync } from 'child_process';

const CCC_URL = process.env.CCC_URL || 'https://api.yourmom.photos';
const RCC_TOKEN = process.env.CCC_AGENT_TOKEN || process.env.BUS_TOKEN || '';
const SLACK_TOKEN = process.env.SLACK_OMGJKH_TOKEN || process.env.SLACK_BOT_TOKEN || '';
const SLACK_CHANNEL = process.env.SLACK_AGENT_CHANNEL || '#rockyandfriends';
const SLACK_API = 'https://slack.com/api';
const ORPHAN_TTL_HOURS = 72;    // file queue item
const ESCALATE_TTL_HOURS = 168; // 7 days → Slack escalation
const DRY_RUN = process.argv.includes('--dry-run');

function git(cmd) {
  try {
    return execSync(`git -C ${process.cwd()} ${cmd}`, { encoding: 'utf8' }).trim();
  } catch (e) {
    return '';
  }
}

async function rccGet(path) {
  const r = await fetch(`${CCC_URL}${path}`, {
    headers: { 'Authorization': `Bearer ${RCC_TOKEN}` }
  });
  return r.json();
}

async function rccPost(path, body) {
  if (DRY_RUN) { console.log('[DRY-RUN] POST', path, JSON.stringify(body).slice(0, 200)); return { ok: true }; }
  const r = await fetch(`${CCC_URL}${path}`, {
    method: 'POST',
    headers: { 'Authorization': `Bearer ${RCC_TOKEN}`, 'Content-Type': 'application/json' },
    body: JSON.stringify(body)
  });
  return r.json();
}

async function slackPost(text) {
  if (DRY_RUN) { console.log('[DRY-RUN] SLACK:', text); return; }
  if (!SLACK_TOKEN) { console.warn('[branch-audit] No Slack token — skipping Slack post'); return; }
  await fetch(`${SLACK_API}/chat.postMessage`, {
    method: 'POST',
    headers: { 'Authorization': `Bearer ${SLACK_TOKEN}`, 'Content-Type': 'application/json' },
    body: JSON.stringify({ channel: SLACK_CHANNEL, text })
  });
}

async function main() {
  console.log('[branch-audit] Starting daily branch audit', new Date().toISOString());

  // Fetch and prune remote branches
  git('fetch --prune origin');
  
  const remotes = git('branch -r').split('\n')
    .map(b => b.trim())
    .filter(b => b && !b.includes('HEAD') && !b.includes('origin/main') && !b.includes('origin/master'));

  if (remotes.length === 0) {
    console.log('[branch-audit] No stale branches found — all clear!');
    return;
  }

  // Get existing queue items to check for branch-orphan dedup keys
  const q = await rccGet('/api/queue');
  const allItems = [...(q.items || []), ...(q.completed || [])];

  const report = [];
  const now = Date.now();

  for (const remote of remotes) {
    const branch = remote.replace('origin/', '');
    
    // Commits ahead of main
    const aheadCount = parseInt(git(`rev-list --count main..${remote}`) || '0', 10);
    
    // Age of last commit on branch
    const lastCommitTs = git(`log -1 --format=%ct ${remote}`);
    const lastCommitMs = lastCommitTs ? parseInt(lastCommitTs, 10) * 1000 : null;
    const ageHours = lastCommitMs ? (now - lastCommitMs) / 3600000 : Infinity;
    
    // Last commit message
    const lastMsg = git(`log -1 --format=%s ${remote}`).slice(0, 80);
    
    console.log(`[branch-audit] Branch: ${branch} | +${aheadCount} commits | age: ${Math.round(ageHours)}h | ${lastMsg}`);

    if (aheadCount === 0) {
      // Fully merged — delete it
      console.log(`[branch-audit] Deleting fully-merged branch: ${branch}`);
      if (!DRY_RUN) {
        try {
          git(`push origin --delete ${branch}`);
          report.push(`🗑️ Deleted fully-merged branch: \`${branch}\``);
        } catch (e) {
          console.warn(`[branch-audit] Could not delete ${branch}:`, e.message);
        }
      } else {
        report.push(`🗑️ [DRY-RUN] Would delete fully-merged branch: \`${branch}\``);
      }
      continue;
    }

    // Unmerged branch — check age
    const scoutKey = `branch-orphan-${branch.replace(/[^a-z0-9-]/gi, '-')}`;
    const existingAlert = allItems.find(i => i.scout_key === scoutKey && 
      !['completed', 'cancelled'].includes(i.status));

    if (ageHours > ESCALATE_TTL_HOURS) {
      // 7+ days unmerged with no activity — Slack escalate
      report.push(`🚨 *Orphaned >7d:* \`${branch}\` (+${aheadCount} commits, ${Math.round(ageHours/24)}d old) — \`${lastMsg}\``);
      if (!existingAlert) {
        await rccPost('/api/queue', {
          title: `Orphaned branch: ${branch} (>7d unmerged)`,
          description: `Branch ${branch} has ${aheadCount} unmerged commits and is ${Math.round(ageHours/24)} days old. Last: ${lastMsg}. Action required: merge to main or delete.`,
          assignee: 'jkh',
          priority: 'high',
          needsHuman: true,
          tags: ['branch-orphan', 'git', 'maintenance'],
          scout_key: scoutKey,
          source: 'branch-audit'
        });
      }
    } else if (ageHours > ORPHAN_TTL_HOURS) {
      // 72h+ — file a queue item (once, deduped)
      report.push(`⚠️ Orphaned >72h: \`${branch}\` (+${aheadCount} commits, ${Math.round(ageHours)}h old) — \`${lastMsg}\``);
      if (!existingAlert) {
        await rccPost('/api/queue', {
          title: `Orphaned branch: ${branch} — merge or delete?`,
          description: `Branch ${branch} has ${aheadCount} unmerged commits and hasn't been touched in ${Math.round(ageHours)}h. Last commit: ${lastMsg}. Merge to main or delete.`,
          assignee: 'all',
          priority: 'medium',
          needsHuman: true,
          tags: ['branch-orphan', 'git', 'maintenance'],
          scout_key: scoutKey,
          source: 'branch-audit'
        });
      }
    } else {
      // Active branch — just report
      report.push(`🔄 Active branch: \`${branch}\` (+${aheadCount} commits, ${Math.round(ageHours)}h old) — \`${lastMsg}\``);
    }
  }

  if (report.length > 0) {
    const summary = `*Branch Audit — ${new Date().toLocaleDateString('en-US', { timeZone: 'America/Los_Angeles' })}*\n${report.join('\n')}`;
    console.log('\n' + summary);
    await slackPost(summary);
  } else {
    console.log('[branch-audit] Nothing to report.');
  }

  console.log('[branch-audit] Done.');
}

main().catch(e => {
  console.error('[branch-audit] Fatal:', e);
  process.exit(1);
});
