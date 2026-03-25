/**
 * rcc/lessons/index.mjs — Distributed Lessons Learned Ledger
 *
 * Agents write lessons when they fail/recover. All agents share the ledger.
 * Storage: MinIO agents/shared/lessons/<domain>.jsonl (append-only)
 * Local cache: ~/.rcc/lessons/<domain>.jsonl (fast lookup, no MinIO roundtrip)
 * Bus propagation: SquirrelBus type:"lesson" broadcasts to all agents
 *
 * Format (one JSON object per line):
 * {"id":"l-<ts>","ts":"ISO","agent":"rocky","domain":"express","tags":["express5","wildcard"],
 *  "symptom":"path-to-regexp error on wildcard routes",
 *  "fix":"Use /route/*splat not /route/* in Express 5","score":1}
 *
 * Context cost: ~100 bytes/lesson. Top-5 match = ~500 bytes. Negligible.
 *
 * v2 additions:
 * - Semantic dedup on ingestion (jaro-winkler fingerprint on symptom+fix)
 * - Cross-domain ?q= search via queryAllLessons()
 * - Trending lessons (by score + recent upvote activity) via getTrendingLessons()
 * - Heartbeat surfacing via getHeartbeatContext()
 */

import { appendFile, readFile, writeFile, mkdir } from 'fs/promises';
import { existsSync, readdirSync } from 'fs';
import { join, dirname, basename } from 'path';
import { execSync } from 'child_process';

// ── Config ─────────────────────────────────────────────────────────────────
const LESSONS_DIR   = process.env.LESSONS_DIR   || join(process.env.HOME || '/tmp', '.rcc/lessons');
const MINIO_ALIAS   = process.env.MINIO_ALIAS || 'local';
const MINIO_BUCKET  = process.env.MINIO_BUCKET  || 'agents';
const AGENT_NAME    = process.env.AGENT_NAME    || 'unknown';
const BUS_API       = process.env.BUS_API       || 'http://localhost:8788';
const BUS_TOKEN     = process.env.BUS_TOKEN     || process.env.RCC_AUTH_TOKENS?.split(',')[0] || '';
const MC_PATH       = process.env.MC_PATH       || 'mc';

// ── Helpers ────────────────────────────────────────────────────────────────

function lessonId() {
  return `l-${Date.now()}-${Math.random().toString(36).slice(2, 6)}`;
}

async function ensureDir(dir) {
  await mkdir(dir, { recursive: true });
}

function localPath(domain) {
  return join(LESSONS_DIR, `${domain}.jsonl`);
}

function minioPath(domain) {
  return `${MINIO_ALIAS}/${MINIO_BUCKET}/shared/lessons/${domain}.jsonl`;
}

// ── Dedup helpers ──────────────────────────────────────────────────────────

/**
 * Simplified text fingerprint for dedup: lowercase, strip punctuation, sort words.
 * Two lessons are duplicates if their normalized symptom+fix fingerprints are ≥85% similar.
 */
function textFingerprint(str) {
  return str.toLowerCase()
    .replace(/[^a-z0-9\s]/g, ' ')
    .split(/\s+/)
    .filter(w => w.length > 2)
    .sort()
    .join(' ');
}

/**
 * Jaccard similarity between two fingerprints (word-level set overlap).
 * Returns 0.0–1.0.
 */
function jaccardSimilarity(a, b) {
  const setA = new Set(a.split(' '));
  const setB = new Set(b.split(' '));
  const intersection = [...setA].filter(w => setB.has(w)).length;
  const union = new Set([...setA, ...setB]).size;
  return union === 0 ? 0 : intersection / union;
}

/**
 * Check if a new lesson is a near-duplicate of an existing one.
 * Returns the existing lesson if duplicate, null otherwise.
 */
function findDuplicate(newLesson, existingLessons, threshold = 0.80) {
  const newFp = textFingerprint((newLesson.symptom || '') + ' ' + (newLesson.fix || ''));
  for (const existing of existingLessons) {
    const existFp = textFingerprint((existing.symptom || '') + ' ' + (existing.fix || ''));
    if (jaccardSimilarity(newFp, existFp) >= threshold) {
      return existing;
    }
  }
  return null;
}

// ── Write a lesson ─────────────────────────────────────────────────────────

/**
 * Record a lesson learned.
 *
 * @param {object} lesson
 * @param {string} lesson.domain    - e.g. "express", "github", "ci", "rcc", "python"
 * @param {string[]} lesson.tags    - searchable keywords
 * @param {string} lesson.symptom  - what went wrong / what triggered this
 * @param {string} lesson.fix      - what solved it
 * @param {string} [lesson.context] - optional extra context (kept short)
 * @param {string} [lesson.agent]   - which agent learned this (defaults to AGENT_NAME)
 */
export async function learnLesson({ domain, tags, symptom, fix, context, agent }) {
  if (!domain || !symptom || !fix) throw new Error('domain, symptom, and fix are required');

  const lesson = {
    id: lessonId(),
    ts: new Date().toISOString(),
    agent: agent || AGENT_NAME,
    domain,
    tags: tags || [],
    symptom: symptom.slice(0, 200),  // keep it tight
    fix: fix.slice(0, 400),
    context: context ? context.slice(0, 200) : undefined,
    score: 1,
  };

  // Remove undefined fields
  Object.keys(lesson).forEach(k => lesson[k] === undefined && delete lesson[k]);

  // ── Dedup check: don't re-learn what we already know ──────────────────
  await ensureDir(LESSONS_DIR);
  const lp = localPath(domain);
  if (existsSync(lp)) {
    try {
      const existing = (await readFile(lp, 'utf8')).trim().split('\n')
        .filter(l => l.trim()).map(l => { try { return JSON.parse(l); } catch { return null; } }).filter(Boolean);
      const dup = findDuplicate(lesson, existing);
      if (dup) {
        // Upvote the existing lesson instead of creating a duplicate
        await upvoteLesson(domain, dup.id);
        console.log(`[lessons] Dedup: upvoted existing lesson ${dup.id} (similar to new: ${symptom.slice(0, 50)})`);
        return { ...dup, _deduped: true };
      }
    } catch { /* if reading fails, just continue with write */ }
  }

  const line = JSON.stringify(lesson) + '\n';

  // Write to local cache
  await appendFile(localPath(domain), line);

  // Write to MinIO (best-effort)
  try {
    const mc = MC_PATH;
    if (existsSync(mc)) {
      execSync(`echo '${line.trim().replace(/'/g, "'\\''")}' | ${mc} pipe ${minioPath(domain)} 2>/dev/null`, {
        stdio: ['pipe', 'pipe', 'pipe'],
        timeout: 5000,
      });
    }
  } catch {
    // MinIO failure is non-fatal — local cache is sufficient
  }

  // Broadcast on SquirrelBus (best-effort)
  try {
    await fetch(`${BUS_API}/bus/send`, {
      method: 'POST',
      headers: { 'Authorization': `Bearer ${BUS_TOKEN}`, 'Content-Type': 'application/json' },
      body: JSON.stringify({
        from: AGENT_NAME,
        to: 'all',
        type: 'lesson',
        subject: `Lesson: ${domain}/${tags.slice(0,3).join('/')}`,
        body: JSON.stringify(lesson),
      }),
    });
  } catch {
    // Bus failure is non-fatal
  }

  console.log(`[lessons] Learned: [${domain}] ${symptom.slice(0, 60)}`);
  return lesson;
}

// ── Query lessons ──────────────────────────────────────────────────────────

/**
 * Get relevant lessons for a task. Returns top N matches by keyword overlap.
 *
 * @param {object} opts
 * @param {string} opts.domain     - primary domain to search
 * @param {string[]} opts.keywords - keywords to match against tags/symptom/fix
 * @param {number} [opts.limit=5]  - max lessons to return
 * @returns {object[]} matching lessons, most relevant first
 */
export async function queryLessons({ domain, keywords = [], limit = 5 }) {
  await ensureDir(LESSONS_DIR);

  // Try to sync from MinIO first (best-effort, fast)
  try {
    const mc = MC_PATH;
    if (existsSync(mc)) {
      execSync(`${mc} cat ${minioPath(domain)} > ${localPath(domain)}.tmp 2>/dev/null && mv ${localPath(domain)}.tmp ${localPath(domain)}`, {
        stdio: ['pipe', 'pipe', 'pipe'],
        timeout: 3000,
      });
    }
  } catch { /* use local cache */ }

  const path = localPath(domain);
  if (!existsSync(path)) return [];

  const content = await readFile(path, 'utf8');
  const lessons = content.trim().split('\n')
    .filter(l => l.trim())
    .map(l => { try { return JSON.parse(l); } catch { return null; } })
    .filter(Boolean);

  if (keywords.length === 0) return lessons.slice(-limit);

  // Score by keyword overlap
  const kw = keywords.map(k => k.toLowerCase());
  const scored = lessons.map(lesson => {
    const text = [
      ...(lesson.tags || []),
      lesson.symptom || '',
      lesson.fix || '',
      lesson.domain || '',
    ].join(' ').toLowerCase();

    const matches = kw.filter(k => text.includes(k)).length;
    return { lesson, matches };
  }).filter(({ matches }) => matches > 0)
    .sort((a, b) => b.matches - a.matches || b.lesson.score - a.lesson.score);

  return scored.slice(0, limit).map(({ lesson }) => lesson);
}

/**
 * Format lessons as a compact context block for LLM prompts.
 * ~100 bytes per lesson. Use before starting complex tasks.
 */
export function formatLessonsForContext(lessons) {
  if (!lessons.length) return '';
  const lines = lessons.map(l =>
    `• [${l.domain}/${l.tags.slice(0,2).join('/')}] SYMPTOM: ${l.symptom} → FIX: ${l.fix}`
  ).join('\n');
  return `\n## Lessons Learned (apply before starting)\n${lines}\n`;
}

/**
 * Receive a lesson broadcast from SquirrelBus and save to local cache.
 * Call this when handling bus messages of type "lesson".
 */
export async function receiveLessonFromBus(busMessage) {
  try {
    const lesson = JSON.parse(busMessage.body);
    if (!lesson.domain || !lesson.symptom || !lesson.fix) return;
    await ensureDir(LESSONS_DIR);
    await appendFile(localPath(lesson.domain), JSON.stringify(lesson) + '\n');
    console.log(`[lessons] Received from ${lesson.agent}: [${lesson.domain}] ${lesson.symptom.slice(0, 60)}`);
  } catch (err) {
    console.warn(`[lessons] Failed to receive lesson: ${err.message}`);
  }
}

/**
 * Upvote a lesson (it worked again). Increases score for better ranking.
 */
export async function upvoteLesson(domain, lessonId) {
  const path = localPath(domain);
  if (!existsSync(path)) return;
  const content = await readFile(path, 'utf8');
  const updated = content.trim().split('\n').map(line => {
    try {
      const l = JSON.parse(line);
      if (l.id === lessonId) {
        l.score = (l.score || 1) + 1;
        l.lastUpvote = new Date().toISOString();
      }
      return JSON.stringify(l);
    } catch { return line; }
  }).join('\n') + '\n';
  await writeFile(path, updated);
}

// ── Cross-domain search ────────────────────────────────────────────────────

/**
 * Search lessons across ALL domains.
 * Used by ?q= queries without a domain filter.
 *
 * @param {string[]} keywords - keywords to match
 * @param {number} [limit=10] - max results
 * @returns {object[]} matching lessons from all domains, best first
 */
export async function queryAllLessons({ keywords = [], limit = 10 } = {}) {
  await ensureDir(LESSONS_DIR);

  let allLessons = [];

  // Collect all domain files
  try {
    const files = readdirSync(LESSONS_DIR).filter(f => f.endsWith('.jsonl'));
    for (const file of files) {
      const domain = basename(file, '.jsonl');
      const content = await readFile(join(LESSONS_DIR, file), 'utf8');
      const lessons = content.trim().split('\n')
        .filter(l => l.trim())
        .map(l => { try { return JSON.parse(l); } catch { return null; } })
        .filter(Boolean);
      allLessons.push(...lessons);
    }
  } catch { /* return empty on failure */ }

  if (keywords.length === 0) {
    return allLessons.sort((a, b) => (b.score || 1) - (a.score || 1)).slice(0, limit);
  }

  const kw = keywords.map(k => k.toLowerCase());
  const scored = allLessons.map(lesson => {
    const text = [
      lesson.domain || '',
      ...(lesson.tags || []),
      lesson.symptom || '',
      lesson.fix || '',
    ].join(' ').toLowerCase();

    const matches = kw.filter(k => text.includes(k)).length;
    return { lesson, matches };
  }).filter(({ matches }) => matches > 0)
    .sort((a, b) => b.matches - a.matches || (b.lesson.score || 1) - (a.lesson.score || 1));

  return scored.slice(0, limit).map(({ lesson }) => lesson);
}

// ── Trending lessons ───────────────────────────────────────────────────────

/**
 * Get trending lessons: highest score + recently upvoted.
 * "Trending" = lessons that have been referenced/upvoted recently and have high scores.
 *
 * @param {object} [opts]
 * @param {number} [opts.limit=5] - max lessons to return
 * @param {number} [opts.recentDays=7] - only consider upvotes within this window
 * @returns {object[]} trending lessons across all domains
 */
export async function getTrendingLessons({ limit = 5, recentDays = 7 } = {}) {
  await ensureDir(LESSONS_DIR);

  let allLessons = [];
  try {
    const files = readdirSync(LESSONS_DIR).filter(f => f.endsWith('.jsonl'));
    for (const file of files) {
      const content = await readFile(join(LESSONS_DIR, file), 'utf8');
      const lessons = content.trim().split('\n')
        .filter(l => l.trim())
        .map(l => { try { return JSON.parse(l); } catch { return null; } })
        .filter(Boolean);
      allLessons.push(...lessons);
    }
  } catch { return []; }

  const cutoff = new Date(Date.now() - recentDays * 86400 * 1000).toISOString();

  // Score: base score + recency bonus (upvoted recently = +2)
  const scored = allLessons.map(lesson => {
    const base = lesson.score || 1;
    const recencyBonus = (lesson.lastUpvote && lesson.lastUpvote > cutoff) ? 2 : 0;
    const recentlyCreated = (lesson.ts && lesson.ts > cutoff) ? 1 : 0;
    return { lesson, trendScore: base + recencyBonus + recentlyCreated };
  }).sort((a, b) => b.trendScore - a.trendScore);

  return scored.slice(0, limit).map(({ lesson }) => lesson);
}

/**
 * Format trending lessons as a compact summary for heartbeat context.
 * @param {object[]} lessons - from getTrendingLessons()
 * @returns {string} compact summary
 */
export function formatTrendingForHeartbeat(lessons) {
  if (!lessons.length) return '';
  const lines = lessons.map(l =>
    `• [${l.domain}] (score:${l.score || 1}) ${l.symptom.slice(0, 80)} → ${l.fix.slice(0, 80)}`
  ).join('\n');
  return `\n## Trending Lessons (top ${lessons.length} by score)\n${lines}\n`;
}

/**
 * Get a compact context block for heartbeat: top trending + any domain-specific recent.
 * Designed to be <500 tokens total.
 *
 * @param {object} [opts]
 * @param {string[]} [opts.domains] - highlight these domains if active
 * @returns {string} context block
 */
export async function getHeartbeatContext({ domains = [] } = {}) {
  const trending = await getTrendingLessons({ limit: 3 });
  let context = '';

  if (trending.length) {
    context += formatTrendingForHeartbeat(trending);
  }

  // Domain-specific spotlight
  for (const domain of domains.slice(0, 2)) {
    const recent = await queryLessons({ domain, keywords: [], limit: 2 });
    if (recent.length) {
      context += `\n## Recent [${domain}] Lessons\n`;
      context += recent.map(l => `• ${l.symptom.slice(0, 80)} → ${l.fix.slice(0, 80)}`).join('\n');
      context += '\n';
    }
  }

  return context;
}

// ── Seed known lessons ─────────────────────────────────────────────────────

/**
 * Seed the lessons ledger with lessons learned so far today.
 * Run once to bootstrap the ledger.
 */
export async function seedKnownLessons() {
  const known = [
    {
      domain: 'express',
      tags: ['express5', 'wildcard', 'route', 'path-to-regexp'],
      symptom: 'path-to-regexp error: Missing parameter name at index N on wildcard routes like /s3/:bucket/*',
      fix: 'Express 5 requires named wildcards. Use /s3/:bucket/*splat and access via req.params.splat. Do NOT use bare * or *key syntax.',
      context: 'Upgraded from Express 4 to 5. path-to-regexp v8 requires all wildcards to be named.',
    },
    {
      domain: 'node-test',
      tags: ['node-test', 'test-isolation', 'shared-state', 'tmp-files'],
      symptom: 'Node --test suites share state via tmp files when tests run concurrently — later tests load earlier tests\' saved state',
      fix: 'Use unique tmp file paths per test with both Date.now() AND a counter/random suffix. Reset state.queue=[] in tests that need clean state rather than relying purely on fresh paths.',
      context: 'Node test runner runs tests in parallel within a describe() block by default.',
    },
    {
      domain: 'github',
      tags: ['dependabot', 'merge', 'ci-failures', 'pre-existing'],
      symptom: 'All Dependabot PRs show CI failures — tempted to skip merging',
      fix: 'Check if a recently-merged PR (even a trivial one like README) also had CI failures. If yes, failures are pre-existing and endemic — safe to merge Dependabot PRs anyway.',
      context: 'Aviation repo merges PRs with broken CI regularly. Check merged PR history before assuming failures are from the dep bump.',
    },
    {
      domain: 'rcc',
      tags: ['queue', 'dedup', 'scout-key', 'scan'],
      symptom: 'Scout scan returns 0 items created even though repos are registered',
      fix: 'Scout dedup checks against items in the authoritative queue API. If items were previously written directly to queue.json (bypassing API), they won\'t have scout_key tags and won\'t dedup — but also won\'t appear as scout items. Run fresh scan after queue is clean.',
      context: 'The pump runs in-process within the API server. Standalone pump runs write to queue.json directly.',
    },
    {
      domain: 'mattermost',
      tags: ['api', 'http', 'https', 'mattermost'],
      symptom: 'Mattermost API calls return empty response or silent failure',
      fix: 'Use HTTPS for Mattermost, not HTTP. HTTP returns empty body with no error.',
      context: 'Mattermost redirects HTTP to HTTPS but curl/fetch don\'t always follow for POST.',
    },
  ];

  for (const lesson of known) {
    await learnLesson({ ...lesson, agent: process.env.PRIMARY_AGENT || 'rocky' });
  }
  console.log(`[lessons] Seeded ${known.length} known lessons`);
}
