/**
 * rcc/issues — GitHub Issues ↔ SQLite sync
 *
 * Source of truth: GitHub Issues API
 * Local cache: SQLite (node:sqlite, Node 22+)
 * Schema: doltlite-compatible (can migrate to Dolt later)
 *
 * Exports: syncIssues, syncAllProjects, getIssues, getIssue,
 *          linkIssue, createIssueFromWQ
 */

import { DatabaseSync } from 'node:sqlite';
import { readFile, mkdir } from 'fs/promises';
import { existsSync } from 'fs';
import { execFile } from 'child_process';
import { promisify } from 'util';
import { dirname, join } from 'path';
import { fileURLToPath } from 'url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const execFileP = promisify(execFile);

// ── Config ────────────────────────────────────────────────────────────────
const DB_PATH       = process.env.ISSUES_DB_PATH || join(__dirname, '../data/issues.db');
const SECRETS_PATH  = process.env.SECRETS_PATH   || join(__dirname, '../data/secrets.json');
const PROJECTS_PATH = process.env.PROJECTS_PATH  || join(__dirname, '../api/projects.json');
const GH_API_BASE   = 'https://api.github.com';

// ── GitHub token ──────────────────────────────────────────────────────────
let _ghToken = null;

async function getGhToken() {
  if (_ghToken) return _ghToken;

  // 1. Try env
  if (process.env.GITHUB_TOKEN) {
    _ghToken = process.env.GITHUB_TOKEN;
    return _ghToken;
  }

  // 2. Try secrets.json
  try {
    if (existsSync(SECRETS_PATH)) {
      const secrets = JSON.parse(await readFile(SECRETS_PATH, 'utf8'));
      const tok = secrets['GITHUB_TOKEN'] || secrets['github']?.GITHUB_TOKEN;
      if (tok) { _ghToken = tok; return _ghToken; }
    }
  } catch {}

  // 3. Try gh CLI
  try {
    const { stdout } = await execFileP('gh', ['auth', 'token']);
    const tok = stdout.trim();
    if (tok) { _ghToken = tok; return _ghToken; }
  } catch {}

  throw new Error('No GitHub token found (set GITHUB_TOKEN, or gh auth login)');
}

// ── SQLite DB ─────────────────────────────────────────────────────────────
let _db = null;

function getDb() {
  if (_db) return _db;

  // Ensure data dir exists (sync — called during startup)
  const dir = dirname(DB_PATH);
  if (!existsSync(dir)) {
    import('fs').then(fs => fs.mkdirSync(dir, { recursive: true }));
  }

  _db = new DatabaseSync(DB_PATH);

  _db.exec(`
    CREATE TABLE IF NOT EXISTS issues (
      id          INTEGER NOT NULL,
      repo        TEXT    NOT NULL,
      title       TEXT    NOT NULL,
      state       TEXT    NOT NULL,
      body        TEXT,
      labels      TEXT,
      assignees   TEXT,
      milestone   TEXT,
      url         TEXT,
      author      TEXT,
      created_at  TEXT,
      updated_at  TEXT,
      closed_at   TEXT,
      wq_id       TEXT,
      synced_at   TEXT NOT NULL,
      PRIMARY KEY (id, repo)
    );
    CREATE INDEX IF NOT EXISTS idx_issues_repo  ON issues(repo);
    CREATE INDEX IF NOT EXISTS idx_issues_state ON issues(state);
    CREATE INDEX IF NOT EXISTS idx_issues_wq    ON issues(wq_id);

    CREATE TABLE IF NOT EXISTS sync_log (
      id         INTEGER PRIMARY KEY AUTOINCREMENT,
      repo       TEXT    NOT NULL,
      synced_at  TEXT    NOT NULL,
      count      INTEGER NOT NULL,
      status     TEXT    NOT NULL,
      error      TEXT
    );
  `);

  return _db;
}

// ── GitHub API helpers ────────────────────────────────────────────────────
async function ghFetch(path, opts = {}) {
  const token = await getGhToken();
  const url = path.startsWith('http') ? path : `${GH_API_BASE}${path}`;
  const resp = await fetch(url, {
    ...opts,
    headers: {
      'Authorization': `Bearer ${token}`,
      'Accept': 'application/vnd.github+json',
      'X-GitHub-Api-Version': '2022-11-28',
      'Content-Type': 'application/json',
      ...(opts.headers || {}),
    },
  });

  if (!resp.ok) {
    const body = await resp.text().catch(() => '');
    throw new Error(`GitHub API ${resp.status}: ${body.slice(0, 200)}`);
  }

  return resp.json();
}

async function ghFetchAllPages(path) {
  const results = [];
  let url = `${path}${path.includes('?') ? '&' : '?'}per_page=100&state=all`;

  while (url) {
    const token = await getGhToken();
    const resp = await fetch(url.startsWith('http') ? url : `${GH_API_BASE}${url}`, {
      headers: {
        'Authorization': `Bearer ${token}`,
        'Accept': 'application/vnd.github+json',
        'X-GitHub-Api-Version': '2022-11-28',
      },
    });

    if (!resp.ok) {
      const body = await resp.text().catch(() => '');
      throw new Error(`GitHub API ${resp.status}: ${body.slice(0, 200)}`);
    }

    const page = await resp.json();
    results.push(...page);

    // Follow Link: header for pagination
    const link = resp.headers.get('link') || '';
    const nextMatch = link.match(/<([^>]+)>;\s*rel="next"/);
    url = nextMatch ? nextMatch[1] : null;
  }

  return results;
}

// ── Sync ──────────────────────────────────────────────────────────────────
export async function syncIssues(repo, { state = 'all' } = {}) {
  const db = getDb();
  const now = new Date().toISOString();

  console.log(`[issues] Syncing ${repo} (state=${state})…`);

  let count = 0;
  let error = null;

  try {
    const issues = await ghFetchAllPages(`/repos/${repo}/issues?state=${state}`);

    const upsert = db.prepare(`
      INSERT INTO issues (id, repo, title, state, body, labels, assignees, milestone, url, author, created_at, updated_at, closed_at, synced_at)
      VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
      ON CONFLICT(id, repo) DO UPDATE SET
        title      = excluded.title,
        state      = excluded.state,
        body       = excluded.body,
        labels     = excluded.labels,
        assignees  = excluded.assignees,
        milestone  = excluded.milestone,
        url        = excluded.url,
        author     = excluded.author,
        updated_at = excluded.updated_at,
        closed_at  = excluded.closed_at,
        synced_at  = excluded.synced_at
    `);

    db.exec('BEGIN');
    try {
      for (const issue of issues) {
        // Skip pull requests (they appear in issues API)
        if (issue.pull_request) continue;

        upsert.run(
          issue.number,
          repo,
          issue.title,
          issue.state,
          issue.body || null,
          JSON.stringify((issue.labels || []).map(l => l.name)),
          JSON.stringify((issue.assignees || []).map(a => a.login)),
          issue.milestone?.title || null,
          issue.html_url,
          issue.user?.login || null,
          issue.created_at,
          issue.updated_at,
          issue.closed_at || null,
          now
        );
        count++;
      }
      db.exec('COMMIT');
    } catch (txErr) {
      db.exec('ROLLBACK');
      throw txErr;
    }

    console.log(`[issues] Synced ${count} issues from ${repo}`);
  } catch (err) {
    error = err.message;
    console.error(`[issues] Sync failed for ${repo}: ${err.message}`);
  }

  // Log the sync
  db.prepare(`
    INSERT INTO sync_log (repo, synced_at, count, status, error)
    VALUES (?, ?, ?, ?, ?)
  `).run(repo, now, count, error ? 'error' : 'ok', error || null);

  return { repo, count, error, synced_at: now };
}

export async function syncAllProjects({ state = 'all' } = {}) {
  let projects = [];
  try {
    const raw = await readFile(PROJECTS_PATH, 'utf8');
    projects = JSON.parse(raw);
  } catch {
    return { error: 'Failed to read projects.json' };
  }

  const results = [];
  for (const proj of projects) {
    if (!proj.id || !proj.enabled) continue;
    try {
      const result = await syncIssues(proj.id, { state });
      results.push(result);
    } catch (err) {
      results.push({ repo: proj.id, error: err.message });
    }
  }

  return results;
}

// ── Query ─────────────────────────────────────────────────────────────────
export function getIssues({ repo, state, limit = 50, offset = 0 } = {}) {
  const db = getDb();
  const conditions = [];
  const params = [];

  if (repo) { conditions.push('repo = ?'); params.push(repo); }
  if (state && state !== 'all') { conditions.push('state = ?'); params.push(state); }

  const where = conditions.length ? `WHERE ${conditions.join(' AND ')}` : '';
  const sql = `SELECT * FROM issues ${where} ORDER BY updated_at DESC LIMIT ? OFFSET ?`;
  params.push(limit, offset);

  return db.prepare(sql).all(...params);
}

export function getIssue(id, repo) {
  const db = getDb();
  if (repo) {
    return db.prepare('SELECT * FROM issues WHERE id = ? AND repo = ?').get(id, repo);
  }
  return db.prepare('SELECT * FROM issues WHERE id = ?').get(id);
}

export function getLastSync(repo) {
  const db = getDb();
  return db.prepare(
    'SELECT * FROM sync_log WHERE repo = ? ORDER BY synced_at DESC LIMIT 1'
  ).get(repo);
}

// ── Link issue to WQ item ────────────────────────────────────────────────
export function linkIssue(issueId, repo, wqId) {
  const db = getDb();
  const result = db.prepare(
    'UPDATE issues SET wq_id = ? WHERE id = ? AND repo = ?'
  ).run(wqId, issueId, repo);

  if (result.changes === 0) {
    throw new Error(`Issue #${issueId} in ${repo} not found`);
  }

  return { ok: true, issueId, repo, wqId };
}

// ── Create GH issue from WQ item ─────────────────────────────────────────
export async function createIssueFromWQ(wqItem, repo) {
  if (!repo) throw new Error('repo required');

  const labels = [];
  if (wqItem.priority === 'high' || wqItem.priority === 'urgent') labels.push('priority: high');
  if (wqItem.assignee && wqItem.assignee !== 'all') labels.push(`agent: ${wqItem.assignee}`);

  const body = [
    wqItem.description || wqItem.title,
    '',
    `---`,
    `*Imported from CCC workqueue item \`${wqItem.id}\`*`,
    wqItem.assignee ? `*Assigned to: ${wqItem.assignee}*` : '',
  ].filter(l => l !== undefined).join('\n');

  const issue = await ghFetch(`/repos/${repo}/issues`, {
    method: 'POST',
    body: JSON.stringify({
      title: wqItem.title,
      body,
      labels,
    }),
  });

  // Sync back to local DB
  const db = getDb();
  const now = new Date().toISOString();
  db.prepare(`
    INSERT INTO issues (id, repo, title, state, body, labels, assignees, milestone, url, author, created_at, updated_at, closed_at, wq_id, synced_at)
    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
    ON CONFLICT(id, repo) DO UPDATE SET
      title = excluded.title, state = excluded.state, body = excluded.body,
      wq_id = excluded.wq_id, synced_at = excluded.synced_at
  `).run(
    issue.number, repo, issue.title, issue.state,
    issue.body || null,
    JSON.stringify((issue.labels || []).map(l => l.name)),
    JSON.stringify([]),
    null, issue.html_url, issue.user?.login || null,
    issue.created_at, issue.updated_at, null,
    wqItem.id, now
  );

  // Link back
  linkIssue(issue.number, repo, wqItem.id);

  return { ok: true, issue: { number: issue.number, url: issue.html_url, title: issue.title }, wqId: wqItem.id };
}

// ── Periodic sync (call this on startup in CCC API) ───────────────────────
let _syncInterval = null;

export function startPeriodicSync(intervalMs = 15 * 60 * 1000) {
  if (_syncInterval) return;
  _syncInterval = setInterval(async () => {
    try {
      console.log('[issues] Periodic sync triggered');
      await syncAllProjects({ state: 'all' });
    } catch (err) {
      console.error('[issues] Periodic sync error:', err.message);
    }
  }, intervalMs);
  _syncInterval.unref?.(); // Don't block process exit
  console.log(`[issues] Periodic sync started (every ${intervalMs / 60000}min)`);
}
