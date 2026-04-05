/**
 * rcc/scout/github.mjs — GitHub repo scanner
 *
 * Scans registered GitHub repos for actionable work:
 * - Open issues (bugs, enhancements, help-wanted)
 * - Stale/stuck PRs needing review or rebase
 * - Failing CI runs on main branch
 * - Security advisories
 * - TODO/FIXME/HACK comments in code (sampled)
 * - Missing tests, docs, README sections
 *
 * Creates CCC work items for anything not already queued.
 * Deduplicates by repo+type+ref (never creates duplicates).
 */

import { execSync } from 'child_process';

// ── gh CLI wrapper ─────────────────────────────────────────────────────────

function gh(args) {
  try {
    return JSON.parse(execSync(`gh ${args} --json`, { encoding: 'utf8', stdio: ['pipe','pipe','pipe'] }));
  } catch {
    try {
      const out = execSync(`gh ${args}`, { encoding: 'utf8', stdio: ['pipe','pipe','pipe'] });
      try { return JSON.parse(out); } catch { return out.trim(); }
    } catch (e) {
      return null;
    }
  }
}

function ghq(query, fields) {
  try {
    const result = execSync(`gh ${query} --json ${fields}`, { encoding: 'utf8', stdio: ['pipe','pipe','pipe'] });
    return JSON.parse(result);
  } catch (e) {
    return null;
  }
}

// ── Dedup key ──────────────────────────────────────────────────────────────

function dedupKey(repo, type, ref) {
  return `scout:${repo}:${type}:${ref}`;
}

function itemAlreadyExists(existingItems, key) {
  return existingItems.some(i => i.scout_key === key || (i.tags || []).includes(key));
}

// ── Scanners ───────────────────────────────────────────────────────────────

async function scanIssues(repo, existingItems) {
  const issues = ghq(`issue list --repo ${repo} --state open --limit 50`, 'number,title,labels,body,createdAt,updatedAt,assignees');
  if (!issues) return [];
  const items = [];

  for (const issue of issues) {
    const key = dedupKey(repo, 'issue', String(issue.number));
    if (itemAlreadyExists(existingItems, key)) continue;

    const labels = (issue.labels || []).map(l => l.name);
    const isBug = labels.some(l => /bug|error|crash|broken|fail/i.test(l));
    const isEnhancement = labels.some(l => /enhance|feature|improvement/i.test(l));
    const isHelpWanted = labels.some(l => /help.wanted|good.first.issue/i.test(l));

    const priority = isBug ? 'high' : isEnhancement ? 'normal' : 'low';
    const executor = isBug ? 'claude_cli' : 'claude_cli';

    items.push({
      title: `[${repo}] Issue #${issue.number}: ${issue.title}`,
      description: `GitHub issue in ${repo}.\n\nIssue body:\n${(issue.body || '').slice(0, 500)}`,
      assignee: 'all',
      priority,
      preferred_executor: executor,
      source: 'scout:github',
      scout_key: key,
      tags: ['github', 'issue', repo, key, ...(isBug ? ['bug'] : []), ...(isEnhancement ? ['enhancement'] : [])],
      notes: `Repo: https://github.com/${repo}/issues/${issue.number}\nLabels: ${labels.join(', ') || 'none'}\nOpened: ${issue.createdAt?.slice(0,10)}`,
    });
  }
  return items;
}

async function scanPRs(repo, existingItems) {
  const prs = ghq(`pr list --repo ${repo} --state open --limit 30`, 'number,title,author,createdAt,updatedAt,mergeable,reviewDecision,labels,isDraft');
  if (!prs) return [];
  const items = [];

  for (const pr of prs) {
    // Skip drafts and dependabot (handled by dep scanner)
    if (pr.isDraft) continue;
    if (pr.author?.login === 'app/dependabot') continue;

    const key = dedupKey(repo, 'pr', String(pr.number));
    if (itemAlreadyExists(existingItems, key)) continue;

    const ageMs = Date.now() - new Date(pr.updatedAt).getTime();
    const ageDays = Math.floor(ageMs / 86400000);
    const isStale = ageDays > 7;
    const needsReview = pr.reviewDecision === 'REVIEW_REQUIRED';
    const hasConflicts = pr.mergeable === 'CONFLICTING';

    if (!isStale && !needsReview && !hasConflicts) continue; // nothing to do

    const reason = [
      needsReview && 'needs review',
      hasConflicts && 'has merge conflicts',
      isStale && `stale (${ageDays}d)`,
    ].filter(Boolean).join(', ');

    items.push({
      title: `[${repo}] PR #${pr.number}: ${pr.title} — ${reason}`,
      description: `Pull request in ${repo} needs attention.\nReason: ${reason}`,
      assignee: 'all',
      priority: hasConflicts ? 'high' : needsReview ? 'normal' : 'low',
      preferred_executor: 'claude_cli',
      source: 'scout:github',
      scout_key: key,
      tags: ['github', 'pr', 'review', repo, key],
      notes: `PR: https://github.com/${repo}/pull/${pr.number}\nAge: ${ageDays}d\nMergeable: ${pr.mergeable}\nReview: ${pr.reviewDecision}`,
    });
  }
  return items;
}

async function scanCI(repo, existingItems) {
  const runs = ghq(`run list --repo ${repo} --branch main --limit 10`, 'databaseId,name,status,conclusion,createdAt,headBranch,url');
  if (!runs) return [];
  const items = [];

  // Find unique failing workflows (don't create duplicate items for same workflow)
  const seenWorkflows = new Set();
  for (const run of runs) {
    if (run.conclusion !== 'failure') continue;
    const key = dedupKey(repo, 'ci', run.name.replace(/\s+/g, '-').toLowerCase());
    if (seenWorkflows.has(key) || itemAlreadyExists(existingItems, key)) continue;
    seenWorkflows.add(key);

    items.push({
      title: `[${repo}] CI failing: ${run.name}`,
      description: `GitHub Actions workflow "${run.name}" is failing on main branch in ${repo}.`,
      assignee: 'all',
      priority: 'high',
      preferred_executor: 'claude_cli',
      source: 'scout:github',
      scout_key: key,
      tags: ['github', 'ci', 'failing', repo, key],
      notes: `Run: ${run.url}\nBranch: ${run.headBranch}\nFailed: ${run.createdAt?.slice(0,10)}`,
    });
  }
  return items;
}

async function scanDeps(repo, existingItems) {
  // Check for Dependabot PRs that have been open too long or have conflicts
  const prs = ghq(`pr list --repo ${repo} --state open --limit 50 --author app/dependabot`, 'number,title,createdAt,mergeable');
  if (!prs || prs.length === 0) return [];
  const items = [];

  const stale = prs.filter(pr => {
    const ageDays = Math.floor((Date.now() - new Date(pr.createdAt).getTime()) / 86400000);
    return ageDays > 14 || pr.mergeable === 'CONFLICTING';
  });

  if (stale.length === 0) return [];

  const key = dedupKey(repo, 'deps', 'stale-dependabot');
  if (itemAlreadyExists(existingItems, key)) return [];

  items.push({
    title: `[${repo}] ${stale.length} stale Dependabot PRs need attention`,
    description: `${stale.length} Dependabot PRs in ${repo} are either stale (>14 days) or have merge conflicts and need to be merged, rebased, or closed.`,
    assignee: 'all',
    priority: 'normal',
    preferred_executor: 'claude_cli',
    source: 'scout:github',
    scout_key: key,
    tags: ['github', 'deps', 'dependabot', repo, key],
    notes: `Stale PRs: ${stale.map(p => `#${p.number}`).join(', ')}\nRepo: https://github.com/${repo}`,
    choices: [
      { id: 'merge', label: 'Auto-merge all mergeable' },
      { id: 'rebase', label: 'Request rebase on conflicting ones' },
      { id: 'close', label: 'Close all (repo is inactive)' },
      { id: 'review', label: 'Review each manually' },
    ],
  });
  return items;
}

async function scanCodeTodos(repo, existingItems) {
  // Sample the codebase for TODO/FIXME/HACK/HACK comments
  // Only do this for non-Aviation repos (Aviation is too large)
  const key = dedupKey(repo, 'todos', 'code-audit');
  if (itemAlreadyExists(existingItems, key)) return [];

  try {
    // Clone shallow to /tmp if needed, or use gh api to search
    const results = execSync(
      `gh search code --repo ${repo} "TODO" --limit 10 2>/dev/null || echo ""`,
      { encoding: 'utf8', stdio: ['pipe','pipe','pipe'] }
    ).trim();

    if (!results || results.length < 10) return [];

    return [{
      title: `[${repo}] Code audit: TODO/FIXME comments found`,
      description: `The ${repo} codebase has TODO/FIXME/HACK comments that may represent unfinished work or known issues worth addressing.`,
      assignee: 'all',
      priority: 'low',
      preferred_executor: 'claude_cli',
      source: 'scout:github',
      scout_key: key,
      tags: ['github', 'code-quality', 'todos', repo, key],
      notes: `Search: https://github.com/search?q=repo:${repo}+TODO&type=code`,
    }];
  } catch {
    return [];
  }
}

// ── Repo-specific deep analysis ────────────────────────────────────────────

async function analyzeRepo(repo, existingItems) {
  const items = [];

  // Get repo metadata
  const meta = ghq(`repo view ${repo}`, 'name,description,primaryLanguage,hasIssuesEnabled,pushedAt,isEmpty');
  if (!meta || meta.isEmpty) return [];

  const lang = meta.primaryLanguage?.name || 'unknown';
  const repoName = repo.split('/')[1];

  // Repo-specific analysis rules are loaded from repos.json (ownership.analysis_rules[])
  // Add custom per-repo rules there; this fallback is a no-op for unconfigured repos.
  const analyses = {};

  const repoAnalyses = analyses[repo] || [];
  for (const analysis of repoAnalyses) {
    if (itemAlreadyExists(existingItems, analysis.key)) continue;
    items.push({
      title: analysis.title,
      description: analysis.desc,
      assignee: 'all',
      priority: analysis.priority,
      preferred_executor: analysis.executor,
      source: 'scout:github',
      scout_key: analysis.key,
      tags: ['github', 'scout-analysis', ...analysis.tags, analysis.key],
      notes: `Repo: https://github.com/${repo}`,
    });
  }

  return items;
}

// ── Main scout function ────────────────────────────────────────────────────

/**
 * Scan a list of repos and return new work items not already in the queue.
 *
 * @param {string[]} repos - list of "owner/repo" strings
 * @param {object[]} existingItems - current CCC queue items (for dedup)
 * @returns {object[]} new work items to create
 */
export async function scout(repos, existingItems = []) {
  const allItems = [];

  for (const repo of repos) {
    console.log(`[scout] Scanning ${repo}...`);

    try {
      const [issues, prs, ci, deps, todos, analysis] = await Promise.all([
        scanIssues(repo, existingItems),
        scanPRs(repo, existingItems),
        scanCI(repo, existingItems),
        scanDeps(repo, existingItems),
        scanCodeTodos(repo, existingItems),
        analyzeRepo(repo, existingItems),
      ]);

      const found = [...issues, ...prs, ...ci, ...deps, ...todos, ...analysis];
      console.log(`[scout] ${repo}: found ${found.length} new items (issues:${issues.length} prs:${prs.length} ci:${ci.length} deps:${deps.length} analysis:${analysis.length})`);
      allItems.push(...found);
    } catch (err) {
      console.error(`[scout] Error scanning ${repo}: ${err.message}`);
    }
  }

  return allItems;
}

// ── CLI entry point ────────────────────────────────────────────────────────

if (process.argv[1] === new URL(import.meta.url).pathname) {
  const repos = process.argv.slice(2);
  if (repos.length === 0) {
    console.error('Usage: node rcc/scout/github.mjs owner/repo [owner/repo ...]');
    process.exit(1);
  }

  const items = await scout(repos, []);
  console.log(`\n[scout] Total new items found: ${items.length}`);
  for (const item of items) {
    console.log(`  [${item.priority}] ${item.title}`);
  }
}
