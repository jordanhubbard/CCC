/**
 * rcc/api/routes/memory.mjs — Memory, vector, lessons, ideation, issues, conversations routes
 * Extracted from api/index.mjs (structural refactor only — no logic changes)
 */

export default function registerRoutes(app, state) {
  const {
    json, readBody, isAuthed,
    readQueue, writeQueue,
    readConversations, writeConversations,
    learnLesson, queryLessons, queryAllLessons, formatLessonsForContext,
    getTrendingLessons, formatTrendingForHeartbeat, getHeartbeatContext,
    generateIdea,
    issuesModule,
    vectorUpsert, vectorSearch, vectorSearchAll, channelMemoryIngest, channelMemoryRecall, collectionStats,
  } = state;

  // ── POST /api/lessons — record a lesson ─────────────────────────────────────
  app.on('POST', '/api/lessons', async (req, res) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const body = await readBody(req);
    if (!body.domain || !body.symptom || !body.fix) return json(res, 400, { error: 'domain, symptom, fix required' });
    const lesson = await learnLesson({ ...body, agent: body.agent || 'api' });
    return json(res, 201, { ok: true, lesson });
  });

  // ── GET /api/lessons/trending ────────────────────────────────────────────────
  app.on('GET', '/api/lessons/trending', async (req, res, _m, url) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const limit = parseInt(url.searchParams.get('limit') || '5', 10);
    const recentDays = parseInt(url.searchParams.get('days') || '7', 10);
    const lessons = await getTrendingLessons({ limit, recentDays });
    const context = url.searchParams.get('format') === 'context' ? formatTrendingForHeartbeat(lessons) : null;
    return json(res, 200, { lessons, context, count: lessons.length });
  });

  // ── GET /api/lessons/heartbeat ───────────────────────────────────────────────
  app.on('GET', '/api/lessons/heartbeat', async (req, res, _m, url) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const domains = (url.searchParams.get('domains') || '').split(',').filter(Boolean);
    const context = await getHeartbeatContext({ domains });
    return json(res, 200, { context });
  });

  // ── GET /api/lessons ─────────────────────────────────────────────────────────
  app.on('GET', /^\/api\/lessons/, async (req, res, _m, url) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const domain = url.searchParams.get('domain');
    const q = (url.searchParams.get('q') || '').split(/\s+/).filter(Boolean);
    const limit = parseInt(url.searchParams.get('limit') || '5', 10);

    let lessons;
    if (!domain) {
      lessons = await queryAllLessons({ keywords: q, limit });
    } else {
      lessons = await queryLessons({ domain, keywords: q, limit });
    }
    const context = url.searchParams.get('format') === 'context' ? formatLessonsForContext(lessons) : null;
    return json(res, 200, { lessons, context, count: lessons.length });
  });

  // ── GET /api/vector/health ───────────────────────────────────────────────────
  app.on('GET', '/api/vector/health', async (req, res) => {
    try {
      const collections = await collectionStats();
      return json(res, 200, { ok: true, collections });
    } catch (err) {
      return json(res, 500, { ok: false, error: err.message });
    }
  });

  // ── GET /api/vector/search ───────────────────────────────────────────────────
  app.on('GET', '/api/vector/search', async (req, res, _m, url) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const q = url.searchParams.get('q') || '';
    if (!q) return json(res, 400, { error: 'Missing query parameter q' });
    const k = parseInt(url.searchParams.get('k') || '10', 10);
    const collections = url.searchParams.get('collections') || 'all';
    try {
      let results;
      if (collections === 'all') {
        results = await vectorSearchAll(q, { k });
      } else {
        results = await vectorSearch(collections, q, { k });
        results = results.map(r => ({ collection: collections, ...r }));
      }
      return json(res, 200, { ok: true, query: q, results });
    } catch (err) {
      return json(res, 500, { ok: false, error: err.message });
    }
  });

  // ── POST /api/vector/upsert ──────────────────────────────────────────────────
  app.on('POST', '/api/vector/upsert', async (req, res) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const body = await readBody(req);
    const { collection, id, text, metadata } = body || {};
    if (!collection || !id || !text) return json(res, 400, { error: 'Missing required fields: collection, id, text' });
    try {
      await vectorUpsert(collection, { id, text, metadata: metadata || {} });
      return json(res, 200, { ok: true });
    } catch (err) {
      return json(res, 500, { ok: false, error: err.message });
    }
  });

  // ── GET /api/vector/context ──────────────────────────────────────────────────
  app.on('GET', '/api/vector/context', async (req, res, _m, url) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const q = url.searchParams.get('q') || '';
    if (!q) return json(res, 400, { error: 'Missing query parameter q' });
    const k = parseInt(url.searchParams.get('k') || '10', 10);
    const collectionsParam = url.searchParams.get('collections') || 'all';
    try {
      let results;
      if (collectionsParam === 'all') {
        results = await vectorSearchAll(q, { k });
      } else {
        const cols = collectionsParam.split(',').map(c => c.trim()).filter(Boolean);
        const searches = await Promise.all(
          cols.map(async col => {
            const hits = await vectorSearch(col, q, { k });
            return hits.map(r => ({ collection: col, ...r }));
          })
        );
        results = searches.flat().sort((a, b) => b.score - a.score).slice(0, k);
      }
      return json(res, 200, { ok: true, results });
    } catch (err) {
      return json(res, 500, { ok: false, error: err.message });
    }
  });

  // ── POST /api/memory/ingest ──────────────────────────────────────────────────
  app.on('POST', '/api/memory/ingest', async (req, res) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const body = await readBody(req);
    const { id, text, platform, workspace_id, channel_id, user_id, conv_id, agent, source } = body || {};
    if (!id || !text) return json(res, 400, { error: 'Missing required fields: id, text' });
    try {
      await channelMemoryIngest(id, text, { platform, workspace_id, channel_id, user_id, conv_id, agent, source });
      return json(res, 200, { ok: true, id });
    } catch (err) {
      return json(res, 500, { ok: false, error: err.message });
    }
  });

  // ── GET /api/memory/recall ───────────────────────────────────────────────────
  app.on('GET', '/api/memory/recall', async (req, res, _m, url) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const q = url.searchParams.get('q') || '';
    if (!q) return json(res, 400, { error: 'Missing query parameter q' });
    const scope = {
      platform:     url.searchParams.get('platform')     || '',
      workspace_id: url.searchParams.get('workspace_id') || '',
      channel_id:   url.searchParams.get('channel_id')   || '',
      user_id:      url.searchParams.get('user_id')      || '',
    };
    const k = parseInt(url.searchParams.get('k') || '8', 10);
    try {
      const result = await channelMemoryRecall(q, scope, { k });
      return json(res, 200, { ok: true, query: q, scope, ...result });
    } catch (err) {
      return json(res, 500, { ok: false, error: err.message });
    }
  });

  // ── POST /api/ideation/generate ─────────────────────────────────────────────
  app.on('POST', '/api/ideation/generate', async (req, res) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const body = await readBody(req);
    const agentName = body.agent || 'unknown';
    const count = Math.min(parseInt(body.count || '1', 10), 3);

    const q = await readQueue();
    const recentQueue = (q.items || []).slice(-20);
    const recentLessons = await queryAllLessons('').catch(() => []);

    const context = { recentQueue, recentLessons, agentName };
    const ideas = [];

    for (let i = 0; i < count; i++) {
      const idea = await generateIdea(context);
      const itemId = `wq-IDEA-${Date.now()}-${i}`;
      const item = {
        id: itemId,
        itemVersion: 1,
        created: new Date().toISOString(),
        source: agentName,
        assignee: 'all',
        priority: 'normal',
        status: 'idea',
        title: idea.title,
        description: idea.description,
        notes: idea.rationale,
        preferred_executor: 'claude_cli',
        journal: [],
        choices: [],
        choiceRecorded: null,
        votes: [],
        attempts: 0,
        maxAttempts: 3,
        claimedBy: null,
        claimedAt: null,
        completedAt: null,
        result: null,
        tags: ['idea', 'auto-generated', ...(idea.tags || [])],
        scout_key: null,
        repo: null,
        ideaMeta: { difficulty: idea.difficulty, rationale: idea.rationale },
      };
      if (!q.items) q.items = [];
      q.items.push(item);
      ideas.push({ id: itemId, title: idea.title });
    }

    await writeQueue(q);
    return json(res, 201, { ok: true, ideas });
  });

  // ── GET /api/ideation/pending ────────────────────────────────────────────────
  app.on('GET', '/api/ideation/pending', async (req, res) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const q = await readQueue();
    const ideas = (q.items || []).filter(i =>
      i.status === 'idea' || (i.tags || []).includes('idea')
    );
    return json(res, 200, { ok: true, ideas });
  });

  // ── POST /api/ideation/:id/promote ──────────────────────────────────────────
  app.on('POST', /^\/api\/ideation\/([^/]+)\/promote$/, async (req, res, m) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const id = decodeURIComponent(m[1]);
    const q = await readQueue();
    const item = (q.items || []).find(i => i.id === id);
    if (!item) return json(res, 404, { error: 'Idea not found' });
    if (!item.claimedBy && (!item.votes || item.votes.length < 1)) {
      return json(res, 400, { error: 'Idea needs at least 1 vote or a claimedBy to promote' });
    }
    item.status = 'pending';
    item.tags = (item.tags || []).filter(t => t !== 'idea');
    item.tags.push('promoted-idea');
    item.journal = item.journal || [];
    item.journal.push({ ts: new Date().toISOString(), type: 'promote', text: 'Promoted from idea to pending' });
    await writeQueue(q);
    return json(res, 200, { ok: true, item });
  });

  // ── GET /api/issues ──────────────────────────────────────────────────────────
  app.on('GET', '/api/issues', async (req, res, _m, url) => {
    const repo  = url.searchParams.get('repo')   || undefined;
    const status = url.searchParams.get('state')  || 'open';
    const limit = parseInt(url.searchParams.get('limit') || '50', 10);
    const offset = parseInt(url.searchParams.get('offset') || '0', 10);
    try {
      const issues = issuesModule.getIssues({ repo, state: status === 'all' ? undefined : status, limit, offset });
      const lastSync = repo ? issuesModule.getLastSync(repo) : null;
      return json(res, 200, { ok: true, issues, count: issues.length, lastSync });
    } catch (err) {
      return json(res, 500, { error: err.message });
    }
  });

  // ── GET /api/issues/:id ──────────────────────────────────────────────────────
  app.on('GET', /^\/api\/issues\/(\d+)$/, async (req, res, m, url) => {
    const id   = parseInt(m[1], 10);
    const repo = url.searchParams.get('repo') || undefined;
    try {
      const issue = issuesModule.getIssue(id, repo);
      if (!issue) return json(res, 404, { error: 'Issue not found' });
      return json(res, 200, { ok: true, issue });
    } catch (err) {
      return json(res, 500, { error: err.message });
    }
  });

  // ── POST /api/issues/sync ────────────────────────────────────────────────────
  app.on('POST', '/api/issues/sync', async (req, res) => {
    const body = await readBody(req);
    const repo = body.repo || null;
    try {
      const result = repo
        ? await issuesModule.syncIssues(repo, { state: body.state || 'all' })
        : await issuesModule.syncAllProjects({ state: body.state || 'all' });
      return json(res, 200, { ok: true, result });
    } catch (err) {
      return json(res, 500, { error: err.message });
    }
  });

  // ── POST /api/issues/:id/link ────────────────────────────────────────────────
  app.on('POST', /^\/api\/issues\/(\d+)\/link$/, async (req, res, m) => {
    const id   = parseInt(m[1], 10);
    const body = await readBody(req);
    const repo  = body.repo;
    const wqId  = body.wq_id;
    if (!repo || !wqId) return json(res, 400, { error: 'repo and wq_id required' });
    try {
      const result = issuesModule.linkIssue(id, repo, wqId);
      return json(res, 200, result);
    } catch (err) {
      return json(res, 500, { error: err.message });
    }
  });

  // ── POST /api/issues/create-from-wq ─────────────────────────────────────────
  app.on('POST', '/api/issues/create-from-wq', async (req, res) => {
    const body = await readBody(req);
    const wqId = body.wq_id;
    const repo = body.repo;
    if (!wqId || !repo) return json(res, 400, { error: 'wq_id and repo required' });
    try {
      const q = await readQueue();
      const item = [...(q.items || []), ...(q.completed || [])].find(i => i.id === wqId);
      if (!item) return json(res, 404, { error: `WQ item ${wqId} not found` });
      const result = await issuesModule.createIssueFromWQ(item, repo);
      return json(res, 201, result);
    } catch (err) {
      return json(res, 500, { error: err.message });
    }
  });

  // ── GET /api/conversations ───────────────────────────────────────────────────
  app.on('GET', '/api/conversations', async (req, res, _m, url) => {
    const convs = await readConversations();
    const { project, agent, channel, since } = Object.fromEntries(url.searchParams);
    let result = convs;
    if (project) result = result.filter(c => c.projectId === project);
    if (agent)   result = result.filter(c => (c.participants || []).includes(agent));
    if (channel) result = result.filter(c => c.channel === channel);
    if (since)   result = result.filter(c => c.createdAt >= since);
    return json(res, 200, result);
  });

  // ── POST /api/conversations ──────────────────────────────────────────────────
  app.on('POST', '/api/conversations', async (req, res) => {
    const body = await readBody(req);
    const convs = await readConversations();
    const conv = {
      id: `conv-${Date.now()}`,
      participants: body.participants || [],
      channel: body.channel || null,
      projectId: body.projectId || null,
      messages: body.messages || [],
      tags: body.tags || [],
      createdAt: new Date().toISOString(),
      updatedAt: new Date().toISOString(),
    };
    convs.push(conv);
    await writeConversations(convs);
    return json(res, 201, { ok: true, conversation: conv });
  });

  // ── GET /api/conversations/:id ───────────────────────────────────────────────
  app.on('GET', /^\/api\/conversations\/([^/]+)$/, async (req, res, m) => {
    const id = decodeURIComponent(m[1]);
    const convs = await readConversations();
    const conv = convs.find(c => c.id === id);
    if (!conv) return json(res, 404, { error: 'Conversation not found' });
    return json(res, 200, conv);
  });

  // ── POST /api/conversations/:id/messages ─────────────────────────────────────
  app.on('POST', /^\/api\/conversations\/([^/]+)\/messages$/, async (req, res, m) => {
    const id = decodeURIComponent(m[1]);
    const body = await readBody(req);
    if (!body.author || !body.text) return json(res, 400, { error: 'author and text required' });
    const convs = await readConversations();
    const idx = convs.findIndex(c => c.id === id);
    if (idx === -1) return json(res, 404, { error: 'Conversation not found' });
    const message = { ts: new Date().toISOString(), author: body.author, text: body.text };
    if (!convs[idx].messages) convs[idx].messages = [];
    convs[idx].messages.push(message);
    convs[idx].updatedAt = new Date().toISOString();
    await writeConversations(convs);
    return json(res, 201, { ok: true, message });
  });
}
