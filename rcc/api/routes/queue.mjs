/**
 * rcc/api/routes/queue.mjs — Queue-related route handlers
 * Extracted from api/index.mjs (structural refactor only — no logic changes)
 */

export default function registerRoutes(app, state) {
  const {
    json, readBody, readQueue, writeQueue, withQueueLock,
    isAuthed, readRequests, writeRequests,
    notifyJkhCompletion, fanoutToProjectChannels, getBrain, createRequest,
    STALE_THRESHOLDS,
  } = state;

  // ── GET /api/queue ─────────────────────────────────────────────────────
  app.on('GET', '/api/queue', async (req, res) => {
    const q = await readQueue();
    return json(res, 200, { items: q.items || [], completed: q.completed || [] });
  });

  // ── GET /api/queue/stale ───────────────────────────────────────────────
  app.on('GET', '/api/queue/stale', async (req, res) => {
    const q = await readQueue();
    const now = Date.now();
    const stale = (q.items || []).filter(item => {
      if (item.status !== 'in-progress' || !item.claimedAt) return false;
      const threshold = STALE_THRESHOLDS[item.preferred_executor] || STALE_THRESHOLDS.default;
      return (now - new Date(item.claimedAt).getTime()) > threshold;
    }).map(item => {
      const threshold = STALE_THRESHOLDS[item.preferred_executor] || STALE_THRESHOLDS.default;
      const age = now - new Date(item.claimedAt).getTime();
      return { ...item, staleMs: age, thresholdMs: threshold, staleMin: Math.round(age / 60000) };
    });
    return json(res, 200, { stale, count: stale.length, thresholds: STALE_THRESHOLDS });
  });

  // ── POST /api/queue/expire-stale ──────────────────────────────────────
  app.on('POST', '/api/queue/expire-stale', async (req, res) => {
    const q = await readQueue();
    const now = Date.now();
    let reset = 0;
    for (const item of (q.items || [])) {
      if (item.status !== 'in-progress' || !item.claimedAt) continue;
      const threshold = STALE_THRESHOLDS[item.preferred_executor] || STALE_THRESHOLDS.default;
      if ((now - new Date(item.claimedAt).getTime()) > threshold) {
        const prevAgent = item.claimedBy;
        item.status = 'pending';
        item.claimedBy = null;
        item.claimedAt = null;
        item.attempts = (item.attempts || 0) + 1;
        if (!item.journal) item.journal = [];
        item.journal.push({
          ts: new Date().toISOString(),
          author: 'rcc-api',
          type: 'stale-reset',
          text: `Stale claim reset (was ${prevAgent}, threshold: ${threshold/60000}min for ${item.preferred_executor || 'default'})`,
        });
        reset++;
      }
    }
    if (reset > 0) await writeQueue(q);
    return json(res, 200, { ok: true, reset });
  });

  // ── GET /api/queue/activity-feed ──────────────────────────────────────
  app.on('GET', '/api/queue/activity-feed', async (req, res, _m, url) => {
    const KNOWN_AGENTS = ['natasha','rocky','boris','bullwinkle','peabody','sherman','snidely','dudley'];
    const q = await readQueue();
    const activeClaims = {};
    for (const item of (q.items || [])) {
      if (item.status === 'in-progress' && item.claimedBy) {
        const key = item.claimedBy.toLowerCase();
        if (!activeClaims[key] || new Date(item.claimedAt) > new Date(activeClaims[key].claimedAt)) {
          activeClaims[key] = {
            id: item.id, title: item.title, priority: item.priority,
            claimedAt: item.claimedAt, tags: item.tags || [],
          };
        }
      }
    }
    const agentList = KNOWN_AGENTS.map(name => {
      const hb = state.heartbeats[name] || {};
      const lastSeen = hb.ts || null;
      const isOffline = !lastSeen || (Date.now() - new Date(lastSeen).getTime() > 10 * 60 * 1000);
      const currentTask = activeClaims[name] || null;
      let status = 'offline';
      if (!isOffline) status = currentTask ? 'working' : 'idle';
      return { name, status, currentTask, lastSeen };
    });
    return json(res, 200, { ok: true, agents: agentList, ts: new Date().toISOString() });
  });

  // ── GET /api/queue/claimed ─────────────────────────────────────────────
  app.on('GET', '/api/queue/claimed', async (req, res) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const q = await readQueue();
    const claimed = (q.items || []).filter(i => i.status === 'in-progress' && i.claimedBy);
    return json(res, 200, { ok: true, claimed, count: claimed.length });
  });

  // ── POST /api/queue ────────────────────────────────────────────────────
  app.on('POST', '/api/queue', async (req, res) => {
    const body = await readBody(req);
    if (!body.title) return json(res, 400, { error: 'title required' });

    const isIdeaOrSkip = body.priority === 'idea' || body._skip_dedup === true;
    if (!isIdeaOrSkip) {
      const desc = (body.description || '').trim();
      if (desc.length < 20) {
        return json(res, 400, {
          error: 'description_required',
          message: 'description must be at least 20 characters — provide context so the dedup gate can work correctly',
        });
      }
    }

    const q = await readQueue();

    if (!isIdeaOrSkip) {
      const normalizeTitle = (t) => (t || '').trim().toLowerCase().replace(/\s+/g, ' ');
      const incomingTitle = normalizeTitle(body.title);
      const activeStatuses = new Set(['pending', 'in-progress', 'in_progress', 'claimed', 'incubating']);
      const exactDup = (q.items || []).find(i =>
        activeStatuses.has(i.status) && normalizeTitle(i.title) === incomingTitle
      );
      if (exactDup) {
        console.log(`[rcc-api] Exact-title dedup: rejected "${body.title.slice(0,60)}" (matches active item ${exactDup.id})`);
        return json(res, 409, {
          ok: false,
          error: 'duplicate',
          reason: 'exact_title_dedup',
          duplicate_id: exactDup.id,
          duplicate_title: exactDup.title,
        });
      }
    }

    const skipSemanticDedup = isIdeaOrSkip;
    if (!skipSemanticDedup) {
      try {
        const SPARKY_OLLAMA = process.env.SPARKY_OLLAMA_URL || 'http://100.87.229.125:11434';
        const MILVUS_URL    = process.env.MILVUS_URL        || 'http://100.89.199.14:19530';
        const DEDUP_THRESH  = parseFloat(process.env.QUEUE_DEDUP_THRESHOLD || '0.85');
        const EMBED_MODEL   = 'nomic-embed-text';

        const embedText = `${body.title}\n${(body.description || '').slice(0, 300)}`.trim();

        const embedCtrl = new AbortController();
        const embedTimer = setTimeout(() => embedCtrl.abort(), 5000);
        const embedResp = await fetch(`${SPARKY_OLLAMA}/api/embed`, {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ model: EMBED_MODEL, input: embedText }),
          signal: embedCtrl.signal,
        });
        clearTimeout(embedTimer);

        if (embedResp.ok) {
          const embedData = await embedResp.json();
          const vector = embedData?.embeddings?.[0];

          if (vector && vector.length === 768) {
            const searchCtrl = new AbortController();
            const searchTimer = setTimeout(() => searchCtrl.abort(), 4000);
            const searchResp = await fetch(`${MILVUS_URL}/v2/vectordb/entities/search`, {
              method: 'POST',
              headers: { 'Content-Type': 'application/json' },
              body: JSON.stringify({
                collectionName: 'rcc_queue_dedup',
                data: [vector],
                annsField: 'vector',
                limit: 3,
                outputFields: ['id', 'title', 'status'],
                searchParams: { metric_type: 'COSINE', params: { nprobe: 10 } },
              }),
              signal: searchCtrl.signal,
            });
            clearTimeout(searchTimer);

            if (searchResp.ok) {
              const searchData = await searchResp.json();
              const hits = searchData?.data?.[0] || [];
              const activeStatuses = new Set(['pending', 'in-progress', 'in_progress', 'claimed', 'incubating']);
              const duplicate = hits.find(h => h.distance >= DEDUP_THRESH && activeStatuses.has(h.status));
              if (duplicate) {
                console.log(`[rcc-api] Semantic dedup: rejected "${body.title.slice(0,50)}" (similarity=${duplicate.distance.toFixed(3)} ≥ ${DEDUP_THRESH} to "${duplicate.title?.slice(0,50)}" id=${duplicate.id})`);
                return json(res, 409, {
                  ok: false,
                  error: 'duplicate',
                  reason: 'semantic_dedup',
                  similarity: duplicate.distance,
                  threshold: DEDUP_THRESH,
                  duplicate_id: duplicate.id,
                  duplicate_title: duplicate.title,
                });
              }
            }

            const tempId = body.id || `wq-tmp-${Date.now()}`;
            fetch(`${MILVUS_URL}/v2/vectordb/entities/upsert`, {
              method: 'POST',
              headers: { 'Content-Type': 'application/json' },
              body: JSON.stringify({
                collectionName: 'rcc_queue_dedup',
                data: [{ id: tempId, vector, title: body.title.slice(0, 256), status: 'pending' }],
              }),
            }).catch(() => {});
          }
        }
      } catch (err) {
        console.warn('[rcc-api] Semantic dedup gate error (non-fatal):', err.message);
      }
    }

    if (body.scout_key) {
      const allExisting = [...(q.items||[]), ...(q.completed||[])];
      const exists = allExisting.some(i =>
        i.scout_key === body.scout_key ||
        (i.tags || []).includes(body.scout_key)
      );
      if (exists) {
        return json(res, 200, { ok: false, duplicate: true, scout_key: body.scout_key });
      }
    }

    const inferExecutor = (b) => {
      if (b.preferred_executor) return b.preferred_executor;
      const tags = b.tags || [];
      if (tags.includes('gpu') || tags.includes('render') || tags.includes('simulation')) return 'gpu';
      if (tags.includes('reasoning') || tags.includes('code') || tags.includes('complex')) return 'claude_cli';
      if (tags.includes('heartbeat') || tags.includes('status') || tags.includes('poll')) return 'inference_key';
      if (tags.includes('embedding') || tags.includes('local-llm') || tags.includes('peer-llm')) return 'llm_server';
      return (b.assignee && b.assignee !== 'all') ? 'claude_cli' : 'inference_key';
    };

    const allIds = new Set([...(q.items||[]), ...(q.completed||[])].map(i => i.id));
    let itemId = body.id || `wq-API-${Date.now()}`;
    if (body.id && allIds.has(body.id)) {
      itemId = `wq-API-${Date.now()}`;
      console.warn(`[rcc-api] ID collision on "${body.id}" — reassigned to "${itemId}"`);
    }

    const VALID_PRIORITIES = new Set(['critical','high','medium','normal','low','idea']);
    const NUMERIC_PRIORITY_MAP = (n) => n >= 80 ? 'critical' : n >= 60 ? 'high' : n >= 40 ? 'medium' : n >= 20 ? 'low' : 'idea';
    let rawPriority = body.priority ?? 'normal';
    if (typeof rawPriority === 'number') {
      rawPriority = NUMERIC_PRIORITY_MAP(rawPriority);
      console.warn(`[rcc-api] Numeric priority ${body.priority} coerced to "${rawPriority}" for item "${body.title?.slice(0,40)}"`);
    } else if (!VALID_PRIORITIES.has(rawPriority)) {
      console.warn(`[rcc-api] Unknown priority "${rawPriority}" for item "${body.title?.slice(0,40)}" — defaulting to "normal"`);
      rawPriority = 'normal';
    }

    const item = {
      id: itemId,
      itemVersion: 1,
      created: new Date().toISOString(),
      source: body.source || 'api',
      assignee: body.assignee || 'all',
      priority: rawPriority,
      status: 'pending',
      title: body.title,
      description: body.description || '',
      notes: body.notes || '',
      preferred_executor: inferExecutor(body),
      journal: [],
      choices: body.choices || [],
      choiceRecorded: null,
      votes: [],
      attempts: 0,
      maxAttempts: body.maxAttempts || 3,
      claimedBy: null,
      claimedAt: null,
      completedAt: null,
      result: null,
      tags: body.tags || [],
      scout_key: body.scout_key || null,
      repo: body.repo || null,
      project: body.project || body.repo || null,
    };
    if (!q.items) q.items = [];
    q.items.push(item);
    await writeQueue(q);
    if (item.project && item.priority !== 'idea') {
      fanoutToProjectChannels(item.project,
        `📋 New task queued: *${item.title}* (${item.priority})\n${item.description ? item.description.slice(0, 200) : ''}`
      );
    }
    return json(res, 201, { ok: true, item });
  });

  // ── GET /api/item/:id ──────────────────────────────────────────────────
  app.on('GET', /^\/api\/item\/([^/]+)$/, async (req, res, m) => {
    const id = decodeURIComponent(m[1]);
    const q = await readQueue();
    const item = [...(q.items||[]), ...(q.completed||[])].find(i => i.id === id);
    if (!item) return json(res, 404, { error: 'Item not found' });
    return json(res, 200, item);
  });

  // ── POST /api/item/:id/claim ───────────────────────────────────────────
  app.on('POST', /^\/api\/item\/([^/]+)\/claim$/, async (req, res, m) => {
    const id = decodeURIComponent(m[1]);
    const body = await readBody(req);
    const agent = body.agent || body._author;
    if (!agent) return json(res, 400, { error: 'agent required' });
    return withQueueLock(async () => {
      const q = await readQueue();
      const item = q.items?.find(i => i.id === id);
      if (!item) return json(res, 404, { error: 'Item not found' });
      if (item.claimedBy && item.claimedBy !== agent && item.status === 'in-progress') {
        const threshold = STALE_THRESHOLDS[item.preferred_executor] || STALE_THRESHOLDS.default;
        const age = Date.now() - new Date(item.claimedAt).getTime();
        if (age < threshold) {
          return json(res, 409, { error: `Already claimed by ${item.claimedBy}`, claimedBy: item.claimedBy, claimedAt: item.claimedAt });
        }
      }
      if (item.status !== 'pending' && item.status !== 'in-progress') {
        return json(res, 409, { error: `Item is ${item.status}, cannot claim` });
      }
      const now = new Date().toISOString();
      const prevAgent = item.claimedBy;
      item.claimedBy = agent;
      item.claimedAt = now;
      item.keepaliveAt = now;
      item.status = 'in-progress';
      item.attempts = (item.attempts || 0) + 1;
      if (!item.journal) item.journal = [];
      item.journal.push({ ts: now, author: agent, type: 'claim', text: prevAgent ? `Claimed (previous: ${prevAgent})` : 'Claimed' });
      if (!item.events) item.events = [];
      item.events.push({ ts: now, agent, type: 'claim', note: body.note || null });
      item.itemVersion = (item.itemVersion || 0) + 1;
      await writeQueue(q);
      return json(res, 200, { ok: true, item });
    });
  });

  // ── POST /api/item/:id/complete ────────────────────────────────────────
  app.on('POST', /^\/api\/item\/([^/]+)\/complete$/, async (req, res, m) => {
    const id = decodeURIComponent(m[1]);
    const body = await readBody(req);
    const agent = body.agent || body._author;
    const q = await readQueue();
    const item = q.items?.find(i => i.id === id);
    if (!item) return json(res, 404, { error: 'Item not found' });
    const now = new Date().toISOString();
    item.status = 'completed';
    item.completedAt = now;
    if (body.resolution) item.resolution = body.resolution;
    if (body.result) item.result = body.result;
    if (!item.journal) item.journal = [];
    item.journal.push({ ts: now, author: agent || 'api', type: 'complete', text: body.resolution || body.result || 'Completed' });
    if (!item.events) item.events = [];
    item.events.push({ ts: now, agent: agent || 'api', type: 'complete', note: body.resolution || body.result || null });
    item.itemVersion = (item.itemVersion || 0) + 1;
    q.items = q.items.filter(i => i.id !== id);
    if (!q.completed) q.completed = [];
    q.completed.push(item);
    await writeQueue(q);
    notifyJkhCompletion(item, agent);
    if (item.project) {
      const resolution = (item.resolution || item.result || '').slice(0, 200);
      fanoutToProjectChannels(item.project,
        `✅ *${item.title}* — completed by ${agent || 'unknown'}${resolution ? '\n' + resolution : ''}`
      );
    }
    return json(res, 200, { ok: true, item });
  });

  // ── POST /api/item/:id/fail ────────────────────────────────────────────
  app.on('POST', /^\/api\/item\/([^/]+)\/fail$/, async (req, res, m) => {
    const id = decodeURIComponent(m[1]);
    const body = await readBody(req);
    const agent = body.agent || body._author;
    const q = await readQueue();
    const item = q.items?.find(i => i.id === id);
    if (!item) return json(res, 404, { error: 'Item not found' });
    const now = new Date().toISOString();
    const reason = body.reason || 'Agent reported failure';
    item.status = 'pending';
    item.claimedBy = null;
    item.claimedAt = null;
    item.keepaliveAt = null;
    if (!item.journal) item.journal = [];
    item.journal.push({ ts: now, author: agent || 'api', type: 'fail', text: reason });
    if (!item.events) item.events = [];
    item.events.push({ ts: now, agent: agent || 'api', type: 'fail', note: reason });
    item.itemVersion = (item.itemVersion || 0) + 1;
    if (item.attempts >= (item.maxAttempts || 3)) {
      item.status = 'blocked';
      item.blockedReason = `Exceeded maxAttempts (${item.maxAttempts || 3}). Last failure: ${reason}`;
      item.journal.push({ ts: now, author: 'rcc-api', type: 'dlq', text: `Moved to blocked — maxAttempts exceeded` });
    }
    await writeQueue(q);
    return json(res, 200, { ok: true, item });
  });

  // ── POST /api/item/:id/keepalive ───────────────────────────────────────
  app.on('POST', /^\/api\/item\/([^/]+)\/keepalive$/, async (req, res, m) => {
    const id = decodeURIComponent(m[1]);
    const body = await readBody(req);
    const agent = body.agent || body._author;
    const q = await readQueue();
    const item = q.items?.find(i => i.id === id);
    if (!item) return json(res, 404, { error: 'Item not found' });
    if (item.claimedBy && agent && item.claimedBy !== agent) {
      return json(res, 409, { error: `Item claimed by ${item.claimedBy}, not ${agent}` });
    }
    const now = new Date().toISOString();
    item.keepaliveAt = now;
    if (!item.events) item.events = [];
    item.events.push({ ts: now, agent: agent || item.claimedBy || 'api', type: 'keepalive', note: body.note || null });
    item.itemVersion = (item.itemVersion || 0) + 1;
    await writeQueue(q);
    return json(res, 200, { ok: true, keepaliveAt: now });
  });

  // ── PATCH /api/item/:id ────────────────────────────────────────────────
  app.on('PATCH', /^\/api\/item\/([^/]+)$/, async (req, res, m) => {
    const id = decodeURIComponent(m[1]);
    const body = await readBody(req);
    const q = await readQueue();
    const item = q.items?.find(i => i.id === id);
    if (!item) return json(res, 404, { error: 'Item not found' });
    const allowed = ['title','description','priority','assignee','status','notes','choices','claimedBy','claimedAt','result','completedAt','type','blockedBy','blocks','needsHuman','needsHumanAt','needsHumanReason'];
    const now = new Date().toISOString();
    const changed = [];
    for (const field of allowed) {
      if (body[field] !== undefined && body[field] !== item[field]) {
        changed.push(`${field}: ${JSON.stringify(item[field])} → ${JSON.stringify(body[field])}`);
        item[field] = body[field];
      }
    }
    if (changed.length) {
      if (!item.journal) item.journal = [];
      item.journal.push({ ts: now, author: body._author || 'api', type: 'status-change', text: `Updated: ${changed.join('; ')}` });
      item.itemVersion = (item.itemVersion || 0) + 1;
      if (item.status === 'completed' || item.status === 'cancelled') {
        q.items = q.items.filter(i => i.id !== item.id);
        if (!q.completed) q.completed = [];
        q.completed.push(item);
        if (item.status === 'completed') notifyJkhCompletion(item, body._author);
      }
      await writeQueue(q);
    }
    return json(res, 200, { ok: true, item });
  });

  // ── POST /api/item/:id/comment ─────────────────────────────────────────
  app.on('POST', /^\/api\/item\/([^/]+)\/comment$/, async (req, res, m) => {
    const id = decodeURIComponent(m[1]);
    const body = await readBody(req);
    const text = (body.text || '').trim();
    if (!text) return json(res, 400, { error: 'text required' });
    const q = await readQueue();
    const item = q.items?.find(i => i.id === id);
    if (!item) return json(res, 404, { error: 'Item not found' });
    if (!item.journal) item.journal = [];
    const entry = { ts: new Date().toISOString(), author: body.author || 'api', type: 'comment', text };
    item.journal.push(entry);
    item.itemVersion = (item.itemVersion || 0) + 1;
    await writeQueue(q);
    if (item.project && body.author && body.author !== 'api') {
      fanoutToProjectChannels(item.project,
        `💬 *${body.author}* on *${item.title}*: ${text.slice(0, 300)}`
      );
    }
    return json(res, 200, { ok: true, entry });
  });

  // ── POST /api/item/:id/choice ──────────────────────────────────────────
  app.on('POST', /^\/api\/item\/([^/]+)\/choice$/, async (req, res, m) => {
    const id = decodeURIComponent(m[1]);
    const body = await readBody(req);
    if (!body.choice) return json(res, 400, { error: 'choice required' });
    const q = await readQueue();
    const item = q.items?.find(i => i.id === id);
    if (!item) return json(res, 404, { error: 'Item not found' });
    const now = new Date().toISOString();
    if (!item.journal) item.journal = [];
    const entry = { ts: now, author: body.author || 'api', type: 'choice', text: `Choice: [${body.choice}] ${body.choiceLabel || ''}` };
    item.journal.push(entry);
    item.choiceRecorded = { choice: body.choice, label: body.choiceLabel || '', ts: now };
    item.itemVersion = (item.itemVersion || 0) + 1;
    await writeQueue(q);
    return json(res, 200, { ok: true, entry, choiceRecorded: item.choiceRecorded });
  });

  // ── POST /api/item/:id/ai-comment ──────────────────────────────────────
  app.on('POST', /^\/api\/item\/([^/]+)\/ai-comment$/, async (req, res, m) => {
    const id = decodeURIComponent(m[1]);
    const body = await readBody(req);
    const prompt = (body.prompt || '').trim();
    if (!prompt) return json(res, 400, { error: 'prompt required' });
    const q = await readQueue();
    const item = q.items?.find(i => i.id === id);
    if (!item) return json(res, 404, { error: 'Item not found' });
    const now = new Date().toISOString();
    if (!item.journal) item.journal = [];
    const userEntry = { ts: now, author: body.author || 'jkh', type: 'ai', text: `✨ ${prompt}` };
    item.journal.push(userEntry);

    let aiText = '(queued for brain processing)';
    try {
      const b = await getBrain();
      const brainReq = createRequest({
        messages: [
          { role: 'system', content: `You are Rocky, helping with work item "${item.title}". Be concise.` },
          { role: 'user', content: prompt }
        ],
        maxTokens: 500,
        priority: 'normal',
        metadata: { itemId: id },
      });
      const result = await Promise.race([
        new Promise(resolve => {
          const onComplete = (r) => { if (r.id === brainReq.id) { b.off('completed', onComplete); resolve(r.result); } };
          b.on('completed', onComplete);
          b.enqueue(brainReq);
        }),
        new Promise((_, reject) => setTimeout(() => reject(new Error('timeout')), 20000))
      ]);
      aiText = result;
    } catch (e) {
      aiText = `(brain error: ${e.message})`;
    }

    const aiEntry = { ts: new Date().toISOString(), author: '🐿️ Rocky', type: 'ai', text: aiText };
    item.journal.push(aiEntry);
    item.itemVersion = (item.itemVersion || 0) + 1;
    await writeQueue(q);
    return json(res, 200, { ok: true, userEntry, aiEntry });
  });

  // ── DELETE /api/item/:id ───────────────────────────────────────────────
  app.on('DELETE', /^\/api\/item\/([^/]+)$/, async (req, res, m) => {
    const id = decodeURIComponent(m[1]);
    const q = await readQueue();
    const idx = (q.items || []).findIndex(i => i.id === id);
    if (idx === -1) return json(res, 404, { error: 'Item not found' });
    const [item] = q.items.splice(idx, 1);
    item.status = 'deleted';
    item.deletedAt = new Date().toISOString();
    if (!q.deleted) q.deleted = [];
    q.deleted.push(item);
    await writeQueue(q);
    return json(res, 200, { ok: true, item });
  });

  // ── POST /api/complete/:id ─────────────────────────────────────────────
  app.on('POST', /^\/api\/complete\/([^/]+)$/, async (req, res, m) => {
    const id = decodeURIComponent(m[1]);
    const body = await readBody(req);
    const q = await readQueue();
    const item = q.items?.find(i => i.id === id);
    if (!item) return json(res, 404, { error: 'Item not found' });
    item.status = 'completed';
    item.completedAt = new Date().toISOString();
    item.itemVersion = (item.itemVersion || 0) + 1;
    if (body?.result) item.result = body.result;
    await writeQueue(q);
    notifyJkhCompletion(item, body?._author || body?.agent);

    if (item.requestId) {
      try {
        const reqs = await readRequests();
        const ticket = reqs.find(r => r.id === item.requestId);
        if (ticket) {
          const outcome = item.result || `Queue item ${item.id} completed`;
          const delIdx = (ticket.delegations || []).findIndex(d =>
            !d.resolvedAt && (d.queueItemId === id || d.summary?.includes(id) || d.summary?.includes(item.title))
          );
          if (delIdx >= 0) {
            ticket.delegations[delIdx].resolvedAt = new Date().toISOString();
            ticket.delegations[delIdx].outcome = outcome;
          }
          const allResolved = (ticket.delegations || []).every(d => d.resolvedAt);
          if (allResolved && ticket.status === 'delegated') {
            ticket.status = 'resolved';
            ticket.resolution = outcome;
          }
          await writeRequests(reqs);
        }
      } catch (e) {
        console.error('[rcc-api] requestId linkage error:', e.message);
      }
    }

    return json(res, 200, { ok: true, item });
  });

  // ── GET /api/appeal ────────────────────────────────────────────────────
  app.on('GET', '/api/appeal', async (req, res) => {
    const q = await readQueue();
    const all = [...(q.items || []), ...(q.completed || [])];
    const appeals = all.filter(i => i.needsHuman === true || i.status === 'awaiting-jkh');
    appeals.sort((a, b) => {
      const ta = a.needsHumanAt ? new Date(a.needsHumanAt).getTime() : 0;
      const tb = b.needsHumanAt ? new Date(b.needsHumanAt).getTime() : 0;
      return ta - tb;
    });
    return json(res, 200, appeals);
  });

  // ── POST /api/appeal/:id ───────────────────────────────────────────────
  app.on('POST', /^\/api\/appeal\/([^/]+)$/, async (req, res, m) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const id = decodeURIComponent(m[1]);
    const body = await readBody(req);
    const { action, note, assignee } = body;
    if (!['approve','reject','reassign','comment'].includes(action))
      return json(res, 400, { error: 'action must be approve, reject, reassign, or comment' });
    const q = await readQueue();
    const item = [...(q.items || []), ...(q.completed || [])].find(i => i.id === id);
    if (!item) return json(res, 404, { error: 'Item not found' });
    const now = new Date().toISOString();
    if (!item.journal) item.journal = [];
    if (action === 'approve') {
      item.status = 'pending';
      item.needsHuman = false;
      item.journal.push({ ts: now, author: 'jkh', type: 'appeal', text: `Approved${note ? ': ' + note : ''}` });
    } else if (action === 'reject') {
      item.status = 'cancelled';
      item.needsHuman = false;
      item.journal.push({ ts: now, author: 'jkh', type: 'appeal', text: `Rejected${note ? ': ' + note : ''}` });
    } else if (action === 'reassign') {
      if (!assignee) return json(res, 400, { error: 'assignee required for reassign' });
      item.assignee = assignee;
      item.needsHuman = false;
      item.journal.push({ ts: now, author: 'jkh', type: 'appeal', text: `Reassigned to ${assignee}${note ? ': ' + note : ''}` });
    } else if (action === 'comment') {
      item.journal.push({ ts: now, author: 'jkh', type: 'comment', text: note || '' });
    }
    item.itemVersion = (item.itemVersion || 0) + 1;
    if (item.status === 'completed' || item.status === 'cancelled') {
      q.items = (q.items || []).filter(i => i.id !== item.id);
      if (!q.completed) q.completed = [];
      if (!q.completed.find(i => i.id === item.id)) q.completed.push(item);
    }
    await writeQueue(q);
    return json(res, 200, { ok: true, item });
  });

  // ── GET /api/metrics ───────────────────────────────────────────────────
  app.on('GET', '/api/metrics', async (req, res) => {
    const q = await readQueue();
    const items = q.items || [];
    const completed = q.completed || [];
    const now = Date.now();
    const pending = items.filter(i => i.status === 'pending').length;
    const inProgress = items.filter(i => i.status === 'in-progress').length;
    const blocked = items.filter(i => i.status === 'blocked').length;
    const completedToday = completed.filter(i => {
      if (!i.completedAt) return false;
      return (now - new Date(i.completedAt).getTime()) < 24 * 60 * 60 * 1000;
    }).length;
    const agentCounts = {};
    for (const item of items) {
      if (item.claimedBy) agentCounts[item.claimedBy] = (agentCounts[item.claimedBy] || 0) + 1;
    }
    return json(res, 200, {
      ok: true,
      queue: { pending, inProgress, blocked, total: items.length },
      completed: { today: completedToday, total: completed.length },
      agents: agentCounts,
      ts: new Date().toISOString(),
    });
  });

  // ── GET /api/digest ────────────────────────────────────────────────────
  app.on('GET', '/api/digest', async (req, res) => {
    const q = await readQueue();
    const now = Date.now();
    const oneDayAgo = now - 24 * 60 * 60 * 1000;
    const recentCompleted = (q.completed || []).filter(i =>
      i.completedAt && new Date(i.completedAt).getTime() > oneDayAgo
    ).slice(-20);
    const pending = (q.items || [])
      .filter(i => i.status === 'pending')
      .sort((a, b) => {
        const pri = { critical: 0, high: 1, medium: 2, normal: 2, low: 3, idea: 4 };
        return (pri[a.priority] ?? 2) - (pri[b.priority] ?? 2);
      })
      .slice(0, 10);
    return json(res, 200, {
      ok: true,
      generatedAt: new Date().toISOString(),
      recentCompleted: recentCompleted.map(i => ({ id: i.id, title: i.title, completedAt: i.completedAt, agent: i.claimedBy })),
      pending: pending.map(i => ({ id: i.id, title: i.title, priority: i.priority, assignee: i.assignee })),
    });
  });

  // ── GET /api/activity ──────────────────────────────────────────────────
  app.on('GET', '/api/activity', async (req, res, _m, url) => {
    const limit = Math.min(parseInt(url.searchParams.get('limit') || '50', 10), 200);
    const q = await readQueue();
    const events = [];
    for (const item of [...(q.items || []), ...(q.completed || [])]) {
      for (const entry of (item.journal || [])) {
        events.push({ ...entry, itemId: item.id, itemTitle: item.title });
      }
    }
    events.sort((a, b) => new Date(b.ts).getTime() - new Date(a.ts).getTime());
    return json(res, 200, { ok: true, events: events.slice(0, limit), total: events.length });
  });

  // ── GET /api/changelog ─────────────────────────────────────────────────
  app.on('GET', '/api/changelog', async (req, res) => {
    const q = await readQueue();
    const recent = (q.completed || [])
      .filter(i => i.completedAt)
      .sort((a, b) => new Date(b.completedAt).getTime() - new Date(a.completedAt).getTime())
      .slice(0, 30);
    return json(res, 200, {
      ok: true,
      entries: recent.map(i => ({
        id: i.id,
        title: i.title,
        completedAt: i.completedAt,
        agent: i.claimedBy,
        resolution: (i.resolution || i.result || '').slice(0, 200),
        tags: i.tags || [],
        project: i.project || null,
      })),
    });
  });

  // ── POST /api/crash-report ─────────────────────────────────────────────
  app.on('POST', '/api/crash-report', async (req, res) => {
    const body = await readBody(req);
    const report = {
      ts: new Date().toISOString(),
      agent: body.agent || 'unknown',
      error: body.error || body.message || 'Unknown error',
      stack: body.stack || null,
      context: body.context || {},
    };
    console.error(`[rcc-api] Crash report from ${report.agent}:`, report.error);
    return json(res, 200, { ok: true, received: true, ts: report.ts });
  });
}
