/**
 * rcc/api/routes/services.mjs — Health, services, providers, users, requests, calendar,
 *   repos, secrets, keys, exec, slack, sbom, pkg routes
 * Extracted from api/index.mjs (structural refactor only — no logic changes)
 */

import { existsSync } from 'fs';
import { readFile, writeFile, mkdir, chmod, appendFile, readdir } from 'fs/promises';
import { dirname, join as pathJoin } from 'path';
import { randomUUID } from 'crypto';
import * as _https from 'https';

export default function registerRoutes(app, state) {
  const {
    json, readBody, isAuthed, isAdminAuthed,
    readQueue, writeQueue,
    readAgents, writeAgents,
    readRequests, writeRequests,
    readCalendar, writeCalendar,
    readUsers, writeUsers,
    readSecrets, writeSecrets,
    readJsonFile, writeJsonFile,
    readProjects, writeProjects, buildProjectFromRepo,
    getPump,
    getBrain, createRequest,
    getServicesStatus,
    heartbeats,
    slackPost, readRawBody, verifySlackSignature, formatAgentStatus, formatQueueSummary,
    setSlackChannelMeta,
    broadcastGeekEvent,
    llmRegistry,
    START_TIME,
    RCC_PUBLIC_URL,
    TUNNEL_STATE_PATH, TUNNEL_PORT_START,
    TUNNEL_AUTH_KEYS, TUNNEL_USER,
    EXEC_LOG_PATH,
    PROVIDERS_PATH,
    SBOM_DIR,
  } = state;

  // ── GET /health ──────────────────────────────────────────────────────────────
  app.on('GET', '/health', async (req, res) => {
    const b = state.brain;
    const q = await readQueue();
    const llmEndpoints = llmRegistry.serialize();
    return json(res, 200, {
      ok: true,
      uptime: Math.floor((Date.now() - START_TIME) / 1000),
      agentCount: Object.keys(heartbeats).length,
      queueDepth: (q.items || []).filter(i => !['completed','cancelled'].includes(i.status)).length,
      lastBrainTick: b?.state?.lastTick || null,
      version: '0.1.0',
      llm: {
        endpointCount: llmEndpoints.length,
        freshCount: llmEndpoints.filter(e => e.fresh).length,
        modelCount: llmEndpoints.reduce((s, e) => s + e.models.length, 0),
      },
    });
  });

  // ── GET /api/services/status ─────────────────────────────────────────────────
  app.on('GET', '/api/services/status', async (req, res) => {
    const statuses = await getServicesStatus();
    return json(res, 200, statuses);
  });

  // ── GET /api/presence ────────────────────────────────────────────────────────
  app.on('GET', '/api/presence', async (req, res) => {
    const PRESENCE_ONLINE_MS  =  3 * 60 * 1000;
    const PRESENCE_AWAY_MS    = 15 * 60 * 1000;
    const PRESENCE_CACHE_TTL  = 30 * 1000;
    if (!state._presenceCache) state._presenceCache = { data: null, ts: 0 };
    const pc = state._presenceCache;
    if (pc.data && (Date.now() - pc.ts) < PRESENCE_CACHE_TTL) {
      return json(res, 200, pc.data);
    }
    const agents = await readAgents().catch(() => ({}));
    const now = Date.now();
    const presence = {};
    const allNames = new Set([...Object.keys(agents), ...Object.keys(heartbeats)]);
    for (const name of allNames) {
      const hb = heartbeats[name] || null;
      const agent = agents[name] || {};
      if (agent.decommissioned) continue;
      const lastSeen = hb?.ts || agent.lastSeen || null;
      const gapMs = lastSeen ? now - new Date(lastSeen).getTime() : null;
      let status = 'unknown';
      if (gapMs !== null) {
        if (gapMs <= PRESENCE_ONLINE_MS)      status = 'online';
        else if (gapMs <= PRESENCE_AWAY_MS)   status = 'away';
        else                                   status = 'offline';
      }
      const caps = agent.capabilities || {};
      const gpu = caps.gpu || hb?.gpu || null;
      const task = hb?.currentTask || agent.currentTask || null;
      let statusText = status === 'online'
        ? (task ? `busy: ${String(task).slice(0, 40)}` : 'idle')
        : status === 'away' ? 'away'
        : status === 'offline' ? 'offline'
        : 'unknown';
      if (status === 'online' && gpu) statusText = `${statusText} · ${gpu}`;
      presence[name] = {
        status,
        statusText,
        since: lastSeen,
        host: hb?.host || agent.host || null,
        gpu,
        gap_minutes: gapMs !== null ? Math.round(gapMs / 60000) : null,
      };
    }
    const result = { ok: true, agents: presence, ts: new Date().toISOString() };
    pc.data = result;
    pc.ts = Date.now();
    return json(res, 200, result);
  });

  // ── GET /api/queue/activity-feed ─────────────────────────────────────────────
  app.on('GET', '/api/queue/activity-feed', async (req, res) => {
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
      const hb = heartbeats[name] || {};
      const lastSeen = hb.ts || null;
      const isOffline = !lastSeen || (Date.now() - new Date(lastSeen).getTime() > 10 * 60 * 1000);
      const currentTask = activeClaims[name] || null;
      let status = 'offline';
      if (!isOffline) status = currentTask ? 'working' : 'idle';
      return { name, status, currentTask, lastSeen };
    });
    return json(res, 200, { ok: true, agents: agentList, ts: new Date().toISOString() });
  });

  // ── GET /api/repos ───────────────────────────────────────────────────────────
  app.on('GET', '/api/repos', async (req, res) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const repos = await getPump().listRepos();
    const enriched = repos.map(r => ({
      ...r,
      kind: r.kind || 'personal',
      ownership_summary: state.repoOwnershipSummary(r),
    }));
    return json(res, 200, enriched);
  });

  // ── GET /api/repos/:owner/:repo ──────────────────────────────────────────────
  app.on('GET', /^\/api\/repos\/([^/]+\/[^/]+)$/, async (req, res, m) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const fullName = decodeURIComponent(m[1]);
    const repos = await getPump().listRepos();
    const repo = repos.find(r => r.full_name === fullName);
    if (!repo) return json(res, 404, { error: 'Repo not found' });
    return json(res, 200, { ...repo, ownership_summary: state.repoOwnershipSummary(repo) });
  });

  // ── PATCH /api/repos/:owner/:repo ────────────────────────────────────────────
  app.on('PATCH', /^\/api\/repos\/([^/]+\/[^/]+)$/, async (req, res, m) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const fullName = decodeURIComponent(m[1]);
    const body = await readBody(req);
    const repo = await getPump().patchRepo(fullName, body);
    return json(res, 200, { ok: true, repo });
  });

  // ── POST /api/repos/register ─────────────────────────────────────────────────
  app.on('POST', '/api/repos/register', async (req, res) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const body = await readBody(req);
    if (!body.full_name) return json(res, 400, { error: 'full_name required (e.g. owner/repo)' });
    const repo = await getPump().registerRepo(body);
    return json(res, 201, { ok: true, repo });
  });

  // ── POST /api/repos/scan ─────────────────────────────────────────────────────
  app.on('POST', '/api/repos/scan', async (req, res) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const created = await getPump().scan();
    return json(res, 200, { ok: true, itemsCreated: created });
  });

  // ── GET /api/context ─────────────────────────────────────────────────────────
  app.on('GET', '/api/context', async (req, res, _m, url) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const channelId = url.searchParams.get('channel');
    if (!channelId) return json(res, 400, { error: 'channel query param required' });
    const repos    = await getPump().listRepos();
    const projects = await readProjects();
    let repo = repos.find(r => r.ownership?.slack_channel === channelId);
    if (!repo) {
      const projectEntry = projects.find(p =>
        (p.slack_channels || []).some(c => c.channel_id === channelId)
      );
      if (projectEntry) repo = repos.find(r => r.full_name === projectEntry.id);
    }
    if (!repo) return json(res, 404, { error: 'No project associated with this channel' });
    const overlay  = projects.find(p => p.id === repo.full_name) || {};
    const project  = { ...buildProjectFromRepo(repo), ...overlay };
    const q        = await readQueue();
    const repoItems = (q.items || []).filter(i =>
      i.tags?.includes(repo.full_name) ||
      i.title?.toLowerCase().includes(repo.full_name.split('/')[1].toLowerCase())
    ).slice(-10);
    return json(res, 200, { project, recentItems: repoItems });
  });

  // ── GET /api/users ───────────────────────────────────────────────────────────
  app.on('GET', '/api/users', async (req, res) => {
    const users = await readUsers();
    return json(res, 200, users);
  });

  // ── POST /api/users ──────────────────────────────────────────────────────────
  app.on('POST', '/api/users', async (req, res) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const body = await readBody(req);
    if (!body.handle) return json(res, 400, { error: 'handle required' });
    const users = await readUsers();
    if (users.find(u => u.handle === body.handle)) return json(res, 409, { error: 'handle already exists' });
    const user = {
      id: `user-${Date.now()}`,
      name: body.name || body.handle,
      handle: body.handle,
      channels: body.channels || {},
      role: body.role || 'human',
      createdAt: new Date().toISOString(),
      updatedAt: new Date().toISOString(),
    };
    users.push(user);
    await writeUsers(users);
    return json(res, 201, { ok: true, user });
  });

  // ── PATCH /api/users/:id ─────────────────────────────────────────────────────
  app.on('PATCH', /^\/api\/users\/([^/]+)$/, async (req, res, m) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const id = decodeURIComponent(m[1]);
    const body = await readBody(req);
    const users = await readUsers();
    const idx = users.findIndex(u => u.id === id);
    if (idx === -1) return json(res, 404, { error: 'User not found' });
    const allowed = ['name','handle','channels','role'];
    for (const field of allowed) {
      if (body[field] !== undefined) users[idx][field] = body[field];
    }
    users[idx].updatedAt = new Date().toISOString();
    await writeUsers(users);
    return json(res, 200, { ok: true, user: users[idx] });
  });

  // ── GET /api/requests ────────────────────────────────────────────────────────
  app.on('GET', '/api/requests', async (req, res, _m, url) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    let reqs = await readRequests();
    const ownerFilter = url.searchParams.get('owner');
    const statusFilter = url.searchParams.get('status');
    const requesterFilter = url.searchParams.get('requester');
    if (ownerFilter) reqs = reqs.filter(r => r.owner === ownerFilter);
    if (statusFilter) {
      const statuses = statusFilter.split(',');
      reqs = reqs.filter(r => statuses.includes(r.status));
    }
    if (requesterFilter) reqs = reqs.filter(r => r.requester?.id === requesterFilter);
    return json(res, 200, reqs);
  });

  // ── POST /api/requests ───────────────────────────────────────────────────────
  app.on('POST', '/api/requests', async (req, res) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const body = await readBody(req);
    if (!body.summary) return json(res, 400, { error: 'summary required' });
    const ticket = {
      id: `req-${Date.now()}`,
      created: new Date().toISOString(),
      requester: body.requester || { type: 'human', id: 'jkh', channel: 'telegram' },
      summary: body.summary,
      status: 'open',
      owner: body.owner || 'rocky',
      delegations: [],
      resolution: null,
      notifiedRequesterAt: null,
      closedAt: null,
    };
    const reqs = await readRequests();
    reqs.push(ticket);
    await writeRequests(reqs);
    return json(res, 201, { ok: true, ticket });
  });

  // ── GET /api/requests/:id ────────────────────────────────────────────────────
  app.on('GET', /^\/api\/requests\/([^/]+)$/, async (req, res, m) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const id = decodeURIComponent(m[1]);
    const reqs = await readRequests();
    const ticket = reqs.find(r => r.id === id);
    if (!ticket) return json(res, 404, { error: 'Ticket not found' });
    return json(res, 200, ticket);
  });

  // ── PATCH /api/requests/:id ──────────────────────────────────────────────────
  app.on('PATCH', /^\/api\/requests\/([^/]+)$/, async (req, res, m) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const id = decodeURIComponent(m[1]);
    const body = await readBody(req);
    const reqs = await readRequests();
    const ticket = reqs.find(r => r.id === id);
    if (!ticket) return json(res, 404, { error: 'Ticket not found' });
    const allowed = ['summary', 'status', 'owner', 'resolution', 'notifiedRequesterAt'];
    for (const k of allowed) { if (k in body) ticket[k] = body[k]; }
    await writeRequests(reqs);
    return json(res, 200, { ok: true, ticket });
  });

  // ── POST /api/requests/:id/delegate ─────────────────────────────────────────
  app.on('POST', /^\/api\/requests\/([^/]+)\/delegate$/, async (req, res, m) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const id = decodeURIComponent(m[1]);
    const body = await readBody(req);
    if (!body.to || !body.summary) return json(res, 400, { error: 'to and summary required' });
    const reqs = await readRequests();
    const ticket = reqs.find(r => r.id === id);
    if (!ticket) return json(res, 404, { error: 'Ticket not found' });
    const delegation = {
      to: body.to,
      at: new Date().toISOString(),
      summary: body.summary,
      queueItemId: body.queueItemId || null,
      resolvedAt: null,
      outcome: null,
    };
    ticket.delegations.push(delegation);
    if (ticket.status === 'open') ticket.status = 'delegated';
    await writeRequests(reqs);
    return json(res, 201, { ok: true, delegation, delegationIndex: ticket.delegations.length - 1 });
  });

  // ── PATCH /api/requests/:id/delegations/:idx ─────────────────────────────────
  app.on('PATCH', /^\/api\/requests\/([^/]+)\/delegations\/(\d+)$/, async (req, res, m) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const id = decodeURIComponent(m[1]);
    const idx = parseInt(m[2], 10);
    const body = await readBody(req);
    const reqs = await readRequests();
    const ticket = reqs.find(r => r.id === id);
    if (!ticket) return json(res, 404, { error: 'Ticket not found' });
    if (!ticket.delegations[idx]) return json(res, 404, { error: 'Delegation not found' });
    ticket.delegations[idx].resolvedAt = new Date().toISOString();
    ticket.delegations[idx].outcome = body.outcome || '';
    if (ticket.delegations.every(d => d.resolvedAt) && ticket.status === 'delegated') {
      ticket.status = 'resolved';
      if (body.outcome) ticket.resolution = body.outcome;
    }
    await writeRequests(reqs);
    return json(res, 200, { ok: true, ticket });
  });

  // ── POST /api/requests/:id/close ─────────────────────────────────────────────
  app.on('POST', /^\/api\/requests\/([^/]+)\/close$/, async (req, res, m) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const id = decodeURIComponent(m[1]);
    const body = await readBody(req);
    const reqs = await readRequests();
    const ticket = reqs.find(r => r.id === id);
    if (!ticket) return json(res, 404, { error: 'Ticket not found' });
    const now = new Date().toISOString();
    ticket.notifiedRequesterAt = now;
    ticket.closedAt = now;
    ticket.status = 'closed';
    if (body?.resolution) ticket.resolution = body.resolution;
    await writeRequests(reqs);
    return json(res, 200, { ok: true, ticket });
  });

  // ── GET /api/calendar ────────────────────────────────────────────────────────
  app.on('GET', '/api/calendar', async (req, res, _m, url) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    let events = await readCalendar();
    const start = url.searchParams.get('start');
    const end   = url.searchParams.get('end');
    const resource = url.searchParams.get('resource');
    if (start) events = events.filter(e => e.end >= start);
    if (end)   events = events.filter(e => e.start <= end);
    if (resource) events = events.filter(e => e.resource === resource);
    return json(res, 200, events);
  });

  // ── POST /api/calendar ───────────────────────────────────────────────────────
  app.on('POST', '/api/calendar', async (req, res) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const body = await readBody(req);
    if (!body.title || !body.start || !body.end)
      return json(res, 400, { error: 'title, start, end required' });
    const events = await readCalendar();
    const event = {
      id: randomUUID(),
      title: body.title,
      start: body.start,
      end: body.end,
      allDay: body.allDay || false,
      tags: body.tags || [],
      description: body.description || '',
      owner: body.owner || null,
      type: body.type || 'event',
      resource: body.resource || null,
    };
    events.push(event);
    await writeCalendar(events);
    return json(res, 201, { ok: true, event });
  });

  // ── DELETE /api/calendar/:id ─────────────────────────────────────────────────
  app.on('DELETE', /^\/api\/calendar\/([^/]+)$/, async (req, res, m) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const id = decodeURIComponent(m[1]);
    const events = await readCalendar();
    const idx = events.findIndex(e => e.id === id);
    if (idx === -1) return json(res, 404, { error: 'Event not found' });
    const event = events[idx];
    const auth = req.headers['authorization'] || '';
    const token = auth.replace(/^Bearer\s+/i, '').trim();
    const agents = await readAgents();
    const callerAgent = Object.entries(agents).find(([, a]) => a.token === token)?.[0] || null;
    if (event.owner !== 'rocky' && callerAgent !== event.owner && callerAgent !== 'rocky') {
      return json(res, 403, { error: 'Only the event owner or Rocky (CCC) may delete this event' });
    }
    events.splice(idx, 1);
    await writeCalendar(events);
    return json(res, 200, { ok: true });
  });

  // ── PATCH /api/calendar/:id ──────────────────────────────────────────────────
  app.on('PATCH', /^\/api\/calendar\/([^/]+)$/, async (req, res, m) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const id = decodeURIComponent(m[1]);
    const body = await readBody(req);
    const events = await readCalendar();
    const idx = events.findIndex(e => e.id === id);
    if (idx === -1) return json(res, 404, { error: 'Event not found' });
    events[idx] = { ...events[idx], ...body, id };
    await writeCalendar(events);
    return json(res, 200, { ok: true, event: events[idx] });
  });

  // ── GET /api/providers ───────────────────────────────────────────────────────
  app.on('GET', '/api/providers', async (req, res) => {
    const providers = await readJsonFile(state.PROVIDERS_PATH, {});
    return json(res, 200, Object.values(providers));
  });

  // ── GET /api/providers/:id ───────────────────────────────────────────────────
  app.on('GET', /^\/api\/providers\/([^/]+)$/, async (req, res, m) => {
    const providers = await readJsonFile(state.PROVIDERS_PATH, {});
    const id = decodeURIComponent(m[1]);
    const p = providers[id];
    if (!p) return json(res, 404, { error: 'Provider not found' });
    return json(res, 200, p);
  });

  // ── PUT /api/providers/:id ───────────────────────────────────────────────────
  app.on('PUT', /^\/api\/providers\/([^/]+)$/, async (req, res, m) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const id = decodeURIComponent(m[1]);
    const body = await readBody(req);
    if (!body) return json(res, 400, { error: 'Body required' });
    const providers = await readJsonFile(state.PROVIDERS_PATH, {});
    const existing = providers[id] || {};
    providers[id] = {
      id,
      model:        body.model       || existing.model       || null,
      baseUrl:      body.baseUrl     || existing.baseUrl     || null,
      local_port:   body.local_port  || existing.local_port  || null,
      status:       body.status      || 'online',
      owner:        body.owner       || existing.owner       || null,
      context_len:  body.context_len || existing.context_len || null,
      tags:         body.tags        || existing.tags        || [],
      createdAt:    existing.createdAt || new Date().toISOString(),
      updatedAt:    new Date().toISOString(),
    };
    await writeJsonFile(state.PROVIDERS_PATH, providers);
    return json(res, 200, { ok: true, provider: providers[id] });
  });

  // ── POST /api/providers ──────────────────────────────────────────────────────
  app.on('POST', '/api/providers', async (req, res) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const body = await readBody(req);
    if (!body) return json(res, 400, { error: 'Body required' });
    if (!body.model) return json(res, 400, { error: 'model required' });
    const providers = await readJsonFile(state.PROVIDERS_PATH, {});
    const id = body.id || `provider-${randomUUID().slice(0, 8)}`;
    providers[id] = {
      id,
      model:       body.model,
      baseUrl:     body.baseUrl     || null,
      local_port:  body.local_port  || null,
      status:      body.status      || 'online',
      owner:       body.owner       || null,
      context_len: body.context_len || null,
      tags:        body.tags        || [],
      createdAt:   new Date().toISOString(),
      updatedAt:   new Date().toISOString(),
    };
    await writeJsonFile(state.PROVIDERS_PATH, providers);
    return json(res, 201, { ok: true, id, provider: providers[id] });
  });

  // ── PATCH /api/providers/:id ─────────────────────────────────────────────────
  app.on('PATCH', /^\/api\/providers\/([^/]+)$/, async (req, res, m) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const id = decodeURIComponent(m[1]);
    const body = await readBody(req);
    const providers = await readJsonFile(state.PROVIDERS_PATH, {});
    if (!providers[id]) return json(res, 404, { error: 'Provider not found' });
    providers[id] = { ...providers[id], ...body, id, updatedAt: new Date().toISOString() };
    await writeJsonFile(state.PROVIDERS_PATH, providers);
    return json(res, 200, { ok: true, provider: providers[id] });
  });

  // ── DELETE /api/providers/:id ────────────────────────────────────────────────
  app.on('DELETE', /^\/api\/providers\/([^/]+)$/, async (req, res, m) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const id = decodeURIComponent(m[1]);
    const providers = await readJsonFile(state.PROVIDERS_PATH, {});
    if (!providers[id]) return json(res, 404, { error: 'Provider not found' });
    delete providers[id];
    await writeJsonFile(state.PROVIDERS_PATH, providers);
    return json(res, 200, { ok: true });
  });

  // ── GET /api/secrets ─────────────────────────────────────────────────────────
  app.on('GET', '/api/secrets', async (req, res) => {
    if (!isAdminAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const secrets = await readSecrets();
    return json(res, 200, { ok: true, keys: Object.keys(secrets) });
  });

  // ── GET /api/secrets/:key ────────────────────────────────────────────────────
  app.on('GET', /^\/api\/secrets\/([^/]+)$/, async (req, res, m) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const key = decodeURIComponent(m[1]);
    const secrets = await readSecrets();
    if (!(key in secrets)) return json(res, 404, { error: `Secret '${key}' not found` });
    const value = secrets[key];
    if (typeof value === 'object' && value !== null) {
      return json(res, 200, { ok: true, key, secrets: value });
    }
    return json(res, 200, { ok: true, key, value });
  });

  // ── POST /api/secrets/:key ───────────────────────────────────────────────────
  app.on('POST', /^\/api\/secrets\/([^/]+)$/, async (req, res, m) => {
    if (!isAdminAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const key = decodeURIComponent(m[1]);
    const body = await readBody(req);
    if (body.value === undefined && body.secrets === undefined) {
      return json(res, 400, { error: 'body must include "value" (scalar) or "secrets" (object)' });
    }
    const secrets = await readSecrets();
    secrets[key] = body.secrets !== undefined ? body.secrets : body.value;
    await writeSecrets(secrets);
    return json(res, 200, { ok: true, key });
  });

  // ── DELETE /api/secrets/:key ─────────────────────────────────────────────────
  app.on('DELETE', /^\/api\/secrets\/(.+)$/, async (req, res, m) => {
    if (!isAdminAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const key = decodeURIComponent(m[1]);
    const secrets = await readSecrets();
    if (!(key in secrets)) return json(res, 404, { error: 'Secret not found' });
    delete secrets[key];
    await writeSecrets(secrets);
    console.log(`[rcc-api] Secret '${key}' deleted by admin`);
    return json(res, 200, { ok: true, key, deleted: true });
  });

  // ── POST /api/keys/github ────────────────────────────────────────────────────
  app.on('POST', '/api/keys/github', async (req, res) => {
    if (!isAdminAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const body = await readBody(req);
    if (!body.repoUrl || !body.deployKey) return json(res, 400, { error: 'repoUrl and deployKey required' });
    const keyPath = new URL('../../data/github-key.json', import.meta.url).pathname;
    const record = { repoUrl: body.repoUrl, deployKey: body.deployKey, label: body.label || '', registeredAt: new Date().toISOString() };
    await writeFile(keyPath, JSON.stringify(record, null, 2));
    await chmod(keyPath, 0o600);
    return json(res, 200, { ok: true, keyId: 'default' });
  });

  // ── GET /api/keys/github ─────────────────────────────────────────────────────
  app.on('GET', '/api/keys/github', async (req, res) => {
    if (!isAdminAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const keyPath = new URL('../../data/github-key.json', import.meta.url).pathname;
    if (!existsSync(keyPath)) return json(res, 404, { error: 'No deploy key registered' });
    const record = JSON.parse(await readFile(keyPath, 'utf8'));
    return json(res, 200, record);
  });

  // ── POST /api/exec ───────────────────────────────────────────────────────────
  app.on('POST', '/api/exec', async (req, res) => {
    if (!isAdminAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const body = await readBody(req);
    if (!body.code) return json(res, 400, { error: 'code required' });

    const SQUIRRELBUS_TOKEN = process.env.CLAWBUS_TOKEN || process.env.SQUIRRELBUS_TOKEN || '';
    if (!SQUIRRELBUS_TOKEN) return json(res, 500, { error: 'CLAWBUS_TOKEN not configured' });

    const { signPayload } = await import('../exec/index.mjs');

    const execId = `exec-${randomUUID()}`;
    const payload = {
      execId,
      code:    body.code,
      target:  body.target  || 'all',
      replyTo: body.replyTo || null,
      ts:      new Date().toISOString(),
    };

    const sig = signPayload(payload, SQUIRRELBUS_TOKEN);
    const envelope = { ...payload, sig };

    const BUS_URL   = process.env.CLAWBUS_URL || process.env.SQUIRRELBUS_URL || `http://localhost:${process.env.RCC_PORT || 8789}`;
    const BUS_TOKEN = process.env.RCC_AGENT_TOKEN || SQUIRRELBUS_TOKEN;
    let busSent = false;
    try {
      const busResp = await fetch(`${BUS_URL}/bus/send`, {
        method: 'POST',
        headers: { 'Authorization': `Bearer ${BUS_TOKEN}`, 'Content-Type': 'application/json' },
        body: JSON.stringify({
          from:    'rocky',
          to:      body.target || 'all',
          type:    'rcc.exec',
          subject: `rcc.exec:${execId}`,
          body:    JSON.stringify(envelope),
        }),
      });
      busSent = busResp.ok;
    } catch (busErr) {
      console.warn('[rcc-api] ClawBus broadcast failed:', busErr.message);
    }

    const logRecord = {
      execId,
      ts:      payload.ts,
      code:    body.code,
      target:  payload.target,
      replyTo: payload.replyTo,
      results: [],
      busSent,
      requestedBy: 'admin',
    };
    const logPath = state.EXEC_LOG_PATH_ABS;
    await mkdir(new URL('../../data', import.meta.url).pathname, { recursive: true });
    await appendFile(logPath, JSON.stringify(logRecord) + '\n', 'utf8');

    return json(res, 200, { ok: true, execId, busSent });
  });

  // ── GET /api/exec/:id ────────────────────────────────────────────────────────
  app.on('GET', /^\/api\/exec\/([^/]+)$/, async (req, res, m) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const execId = decodeURIComponent(m[1]);
    const logPath = state.EXEC_LOG_PATH_ABS;
    if (!existsSync(logPath)) return json(res, 404, { error: 'Exec record not found' });
    const lines = (await readFile(logPath, 'utf8')).trim().split('\n').filter(Boolean);
    const record = lines.map(l => { try { return JSON.parse(l); } catch { return null; } })
      .filter(Boolean)
      .find(r => r.execId === execId);
    if (!record) return json(res, 404, { error: 'Exec record not found' });
    return json(res, 200, record);
  });

  // ── POST /api/exec/:id/result ────────────────────────────────────────────────
  app.on('POST', /^\/api\/exec\/([^/]+)\/result$/, async (req, res, m) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const execId = decodeURIComponent(m[1]);
    const body = await readBody(req);
    const logPath = state.EXEC_LOG_PATH_ABS;
    await mkdir(new URL('../../data', import.meta.url).pathname, { recursive: true });

    let records = [];
    if (existsSync(logPath)) {
      const lines = (await readFile(logPath, 'utf8')).trim().split('\n').filter(Boolean);
      records = lines.map(l => { try { return JSON.parse(l); } catch { return null; } }).filter(Boolean);
    }
    const idx = records.findIndex(r => r.execId === execId);
    if (idx === -1) {
      records.push({
        execId,
        ts:      new Date().toISOString(),
        results: [{ ...body, ts: new Date().toISOString() }],
        stub:    true,
      });
    } else {
      if (!records[idx].results) records[idx].results = [];
      records[idx].results.push({ ...body, ts: new Date().toISOString() });
    }
    await writeFile(logPath, records.map(r => JSON.stringify(r)).join('\n') + '\n', 'utf8');

    return json(res, 200, { ok: true, execId });
  });

  // ── POST /api/slack/send ─────────────────────────────────────────────────────
  app.on('POST', '/api/slack/send', async (req, res) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const body = await readBody(req);
    if (!body.channel || !body.text) return json(res, 400, { error: 'channel and text required' });
    const result = await slackPost('chat.postMessage', {
      channel:   body.channel,
      text:      body.text,
      thread_ts: body.thread_ts,
      mrkdwn:    true,
    });
    return json(res, 200, { ok: result.ok, ts: result.ts, error: result.error });
  });

  // ── POST /api/slack/events ───────────────────────────────────────────────────
  app.on('POST', '/api/slack/events', async (req, res) => {
    const rawBody = await readRawBody(req);
    if (!verifySlackSignature(req, rawBody)) {
      return json(res, 401, { error: 'Invalid Slack signature' });
    }
    let body;
    try { body = JSON.parse(rawBody.toString('utf8')); } catch { return json(res, 400, { error: 'Invalid JSON' }); }

    if (body.type === 'url_verification') {
      return json(res, 200, { challenge: body.challenge });
    }

    const event = body.event || {};
    json(res, 200, { ok: true });

    if (event.type === 'app_mention' || (event.type === 'message' && event.channel_type === 'im' && !event.bot_id)) {
      const text = (event.text || '').replace(/<@[A-Z0-9]+>/g, '').trim();
      if (!text) return;
      try {
        const b = await getBrain();
        const request = createRequest({
          role: 'user',
          content: text,
          context: { slack_user: event.user, slack_channel: event.channel, source: 'slack' },
        });
        const reply = await b.process(request);
        const replyText = typeof reply === 'string' ? reply : reply?.content || reply?.text || JSON.stringify(reply);
        await slackPost('chat.postMessage', {
          channel:   event.channel,
          text:      replyText,
          thread_ts: event.ts,
          mrkdwn:    true,
        });
      } catch (e) {
        console.error('[rcc-api] Slack event brain error:', e.message);
        await slackPost('chat.postMessage', {
          channel:   event.channel,
          text:      `⚠️ Error: ${e.message}`,
          thread_ts: event.ts,
        }).catch(() => {});
      }
    }
    return;
  });

  // ── POST /api/slack/commands ─────────────────────────────────────────────────
  app.on('POST', '/api/slack/commands', async (req, res) => {
    const rawBody = await readRawBody(req);
    if (!verifySlackSignature(req, rawBody)) {
      return json(res, 401, { error: 'Invalid Slack signature' });
    }
    const params = Object.fromEntries(new URLSearchParams(rawBody.toString('utf8')));
    const cmdText = (params.text || '').trim().toLowerCase();
    const channel  = params.channel_id;
    const responseUrl = params.response_url;

    const slackRespond = async (text) => {
      if (!responseUrl) return;
      await fetch(responseUrl, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ text, response_type: 'in_channel', mrkdwn: true }),
      }).catch(() => {});
    };

    const ack = { text: '⏳ Working on it...', response_type: 'ephemeral' };

    if (cmdText === 'status' || cmdText === '') {
      json(res, 200, ack);
      const statusText = await formatAgentStatus().catch(e => `Error: ${e.message}`);
      await slackRespond(`*🐾 CCC Agent Status*\n${statusText}`);
      return;
    }

    if (cmdText === 'queue') {
      json(res, 200, ack);
      const queueText = await formatQueueSummary().catch(e => `Error: ${e.message}`);
      await slackRespond(`*📋 CCC Queue*\n${queueText}`);
      return;
    }

    if (cmdText.startsWith('ask ')) {
      const question = cmdText.slice(4).trim();
      json(res, 200, ack);
      try {
        const b = await getBrain();
        const request = createRequest({
          role: 'user',
          content: question,
          context: { slack_channel: channel, source: 'slack_command' },
        });
        const reply = await b.process(request);
        const replyText = typeof reply === 'string' ? reply : reply?.content || reply?.text || JSON.stringify(reply);
        await slackRespond(`*🧠 CCC Brain:* ${replyText}`);
      } catch (e) {
        await slackRespond(`⚠️ Error: ${e.message}`);
      }
      return;
    }

    return json(res, 200, {
      text: '*CCC Slash Commands*\n`/ccc status` — agent heartbeat status\n`/ccc queue` — pending work items\n`/ccc ask <question>` — ask the RCC brain',
      response_type: 'ephemeral',
    });
  });

  // ── POST /api/tunnel/request ─────────────────────────────────────────────────
  app.on('POST', '/api/tunnel/request', async (req, res) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const body = await readBody(req);
    if (!body?.pubkey) return json(res, 400, { error: 'pubkey required' });
    const pubkeyTrimmed = body.pubkey.trim();
    if (!/^(ssh-|ecdsa-sha2)/.test(pubkeyTrimmed)) {
      return json(res, 400, { error: 'Invalid pubkey format' });
    }

    const agent = (body.agent || body.label || '').replace(/[^a-z0-9_-]/gi, '').toLowerCase();
    if (!agent) return json(res, 400, { error: 'agent required' });
    const label = body.label || agent;

    const SHELL_TUNNEL_STATE_PATH = process.env.SHELL_TUNNEL_STATE_PATH ||
      state.TUNNEL_STATE_PATH.replace('tunnel-state.json', 'shell-tunnel-state.json');
    let shellState = await readJsonFile(SHELL_TUNNEL_STATE_PATH, { nextPort: state.TUNNEL_PORT_START + 100, tunnels: {} });

    let assigned = shellState.tunnels[agent];
    let alreadyExisted = !!assigned;
    let keyWritten = false;

    if (!assigned) {
      const port = shellState.nextPort;
      shellState.nextPort = port + 1;
      assigned = { agent, port, pubkey: pubkeyTrimmed, label, assignedAt: new Date().toISOString() };
      shellState.tunnels[agent] = assigned;
      await writeJsonFile(SHELL_TUNNEL_STATE_PATH, shellState);
      const comment = `rcc-shell-tunnel-${label}`;
      const authKeyEntry = `no-pty,no-agent-forwarding,no-X11-forwarding,permitopen="localhost:${assigned.port}" ${pubkeyTrimmed} ${comment}\n`;
      try {
        await appendFile(state.TUNNEL_AUTH_KEYS, authKeyEntry, 'utf8');
        try {
          const { execSync } = await import('child_process');
          execSync(`sudo chown tunnel:tunnel ${state.TUNNEL_AUTH_KEYS} && sudo chmod 600 ${state.TUNNEL_AUTH_KEYS}`, { stdio: 'ignore' });
        } catch {}
        keyWritten = true;
        console.log(`[rcc-api] Shell tunnel key added for ${agent} on port ${assigned.port}`);
      } catch (authErr) {
        keyWritten = false;
        console.warn(`[rcc-api] Could not write shell tunnel authorized_keys for ${agent}: ${authErr.message}`);
      }
    } else if (assigned.pubkey !== pubkeyTrimmed) {
      assigned.pubkey = pubkeyTrimmed;
      assigned.updatedAt = new Date().toISOString();
      shellState.tunnels[agent] = assigned;
      await writeJsonFile(SHELL_TUNNEL_STATE_PATH, shellState);
      const comment2 = `rcc-shell-tunnel-${label}`;
      const authKeyEntry2 = `no-pty,no-agent-forwarding,no-X11-forwarding,permitopen="localhost:${assigned.port}" ${pubkeyTrimmed} ${comment2}\n`;
      try {
        await appendFile(state.TUNNEL_AUTH_KEYS, authKeyEntry2, 'utf8');
        try {
          const { execSync } = await import('child_process');
          execSync(`sudo chown tunnel:tunnel ${state.TUNNEL_AUTH_KEYS} && sudo chmod 600 ${state.TUNNEL_AUTH_KEYS}`, { stdio: 'ignore' });
        } catch {}
        keyWritten = true;
        console.log(`[rcc-api] Shell tunnel key rotated for ${agent} on port ${assigned.port}`);
      } catch (authErr) {
        keyWritten = false;
        console.warn(`[rcc-api] Could not rotate shell tunnel authorized_keys for ${agent}: ${authErr.message}`);
      }
    }

    const publicHost = (state.RCC_PUBLIC_URL.replace(/^https?:\/\//, '').split(':')[0]) || '146.190.134.110';
    return json(res, 200, {
      ok: true,
      port: assigned.port,
      user: state.TUNNEL_USER,
      host: publicHost,
      agent: assigned.agent,
      keyWritten,
      alreadyExisted,
      connect: `ssh -p ${assigned.port} horde@localhost  # from do-host1`,
      warning: keyWritten ? null : 'authorized_keys write failed — admin must add key manually',
    });
  });

  // ── POST /api/tunnel/verify ──────────────────────────────────────────────────
  app.on('POST', '/api/tunnel/verify', async (req, res) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const body = await readBody(req);
    if (!body?.agent) return json(res, 400, { error: 'agent required' });
    const tunnelState = await readJsonFile(state.TUNNEL_STATE_PATH, { tunnels: {} });
    const agentName = body.agent.toLowerCase();
    const tunnel = tunnelState.tunnels[agentName]
      || Object.values(tunnelState.tunnels).find(t => t.agent?.toLowerCase() === agentName);
    if (!tunnel?.port) {
      return json(res, 200, { ok: true, tunnelUp: false, port: null, message: 'No tunnel assigned for this agent' });
    }
    const { createConnection } = (await import('net'));
    const tunnelUp = await new Promise(resolve => {
      const sock = createConnection({ host: '127.0.0.1', port: tunnel.port }, () => {
        sock.destroy();
        resolve(true);
      });
      sock.on('error', () => resolve(false));
      sock.setTimeout(2000, () => { sock.destroy(); resolve(false); });
    }).catch(() => false);
    return json(res, 200, {
      ok: true,
      tunnelUp,
      port: tunnel.port,
      message: tunnelUp
        ? `Tunnel active on port ${tunnel.port}`
        : `Port ${tunnel.port} not responding — tunnel may not be connected yet`,
    });
  });

  // ── GET /api/tunnel/list ─────────────────────────────────────────────────────
  app.on('GET', '/api/tunnel/list', async (req, res) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const tunnelState = await readJsonFile(state.TUNNEL_STATE_PATH, { nextPort: state.TUNNEL_PORT_START, tunnels: {} });
    return json(res, 200, Object.values(tunnelState.tunnels));
  });

  // ── GET /api/sbom ────────────────────────────────────────────────────────────
  app.on('GET', '/api/sbom', async (req, res) => {
    try {
      const sbomDir = new URL('../../sbom', import.meta.url).pathname;
      const files = await readdir(sbomDir).catch(() => []);
      const sboms = [];
      for (const f of files) {
        if (!f.endsWith('.sbom.json')) continue;
        try {
          const raw = await readFile(pathJoin(sbomDir, f), 'utf8');
          sboms.push(JSON.parse(raw));
        } catch {}
      }
      return json(res, 200, { ok: true, count: sboms.length, sboms });
    } catch (e) {
      return json(res, 500, { error: e.message });
    }
  });

  // ── GET /api/sbom/:agent/install ─────────────────────────────────────────────
  app.on('GET', /^\/api\/sbom\/[^/]+\/install$/, async (req, res, m, _url, path) => {
    const agentName = (path || req.url.split('?')[0]).split('/')[3].replace(/[^a-z0-9_-]/gi, '');
    try {
      const sbomDir = new URL('../../sbom', import.meta.url).pathname;
      const script = await readFile(pathJoin(sbomDir, 'install-sbom.sh'), 'utf8').catch(() => null);
      if (!script) return json(res, 404, { error: 'install-sbom.sh not found' });
      res.writeHead(200, { 'Content-Type': 'text/plain; charset=utf-8' });
      res.end(`AGENT_NAME=${agentName} RCC_URL=${process.env.RCC_EXTERNAL_URL || 'http://localhost:8789'}\n${script}`);
      return;
    } catch (e) {
      return json(res, 500, { error: e.message });
    }
  });

  // ── GET /api/sbom/:agent (without /install) ──────────────────────────────────
  app.on('GET', /^\/api\/sbom\/([a-zA-Z0-9_-]+)$/, async (req, res, m) => {
    const agentName = m[1];
    try {
      const sbomDir = new URL('../../sbom', import.meta.url).pathname;
      const sbomFile = pathJoin(sbomDir, `${agentName}.sbom.json`);
      const raw = await readFile(sbomFile, 'utf8').catch(() => null);
      if (!raw) return json(res, 404, { error: `No SBOM found for agent: ${agentName}` });
      return json(res, 200, JSON.parse(raw));
    } catch (e) {
      return json(res, 500, { error: e.message });
    }
  });

  // ── PUT /api/sbom/:agent ─────────────────────────────────────────────────────
  app.on('PUT', /^\/api\/sbom\/([a-zA-Z0-9_-]+)$/, async (req, res, m) => {
    const agentName = m[1];
    try {
      const body = await readBody(req);
      const sbomData = typeof body === 'string' ? JSON.parse(body) : body;
      if (!sbomData.agent) sbomData.agent = agentName;
      sbomData.updated = new Date().toISOString();
      const sbomDir = new URL('../../sbom', import.meta.url).pathname;
      const sbomFile = pathJoin(sbomDir, `${agentName}.sbom.json`);
      await writeFile(sbomFile, JSON.stringify(sbomData, null, 2) + '\n', 'utf8');
      const { exec: execChild } = await import('child_process');
      const { promisify } = await import('util');
      const execAsync = promisify(execChild);
      await execAsync(`git add rcc/sbom/${agentName}.sbom.json && git commit -m "sbom: update ${agentName}"`, { cwd: process.env.WORKSPACE_DIR || '/home/jkh/.openclaw/workspace' }).catch(() => {});
      return json(res, 200, { ok: true, agent: agentName, updated: sbomData.updated });
    } catch (e) {
      return json(res, 500, { error: e.message });
    }
  });

  // ── POST /api/sbom/:agent/propose ────────────────────────────────────────────
  app.on('POST', /^\/api\/sbom\/([a-zA-Z0-9_-]+)\/propose$/, async (req, res, m) => {
    const agentName = m[1];
    try {
      const body = await readBody(req);
      const proposal = typeof body === 'string' ? JSON.parse(body) : body;
      const { package_type, name, reason } = proposal;
      if (!package_type || !name) return json(res, 400, { error: 'package_type and name required' });

      const sbomDir = new URL('../../sbom', import.meta.url).pathname;
      const sbomFile = pathJoin(sbomDir, `${agentName}.sbom.json`);
      const raw = await readFile(sbomFile, 'utf8').catch(() => null);
      let sbomData = raw ? JSON.parse(raw) : { agent: agentName, version: '1.0.0', packages: {}, skills: [], env_required: [], env_optional: [] };

      if (package_type === 'skill') {
        sbomData.skills = sbomData.skills || [];
        if (!sbomData.skills.includes(name)) sbomData.skills.push(name);
      } else {
        sbomData.packages = sbomData.packages || {};
        sbomData.packages[package_type] = sbomData.packages[package_type] || [];
        if (!sbomData.packages[package_type].includes(name)) {
          sbomData.packages[package_type].push(name);
        }
      }
      sbomData.updated = new Date().toISOString();

      await writeFile(sbomFile, JSON.stringify(sbomData, null, 2) + '\n', 'utf8');
      const { exec: execChild } = await import('child_process');
      const { promisify } = await import('util');
      const execAsync = promisify(execChild);
      await execAsync(`git add rcc/sbom/${agentName}.sbom.json && git commit -m "sbom: ${agentName} propose ${package_type}/${name} — ${reason || 'no reason'}"`, { cwd: process.env.WORKSPACE_DIR || '/home/jkh/.openclaw/workspace' }).catch(() => {});
      return json(res, 200, { ok: true, agent: agentName, added: { type: package_type, name }, reason });
    } catch (e) {
      return json(res, 500, { error: e.message });
    }
  });

  // ── GET /api/pkg ─────────────────────────────────────────────────────────────
  app.on('GET', /^\/api\/pkg/, async (req, res) => {
    const PKG_CACHE_TTL = 5 * 60 * 1000;
    if (!state._pkgCache || Date.now() - state._pkgCache.ts > PKG_CACHE_TTL) {
      try {
        const listing = await new Promise((resolve, reject) => {
          _https.get({
            hostname: 'api.github.com',
            path: '/repos/jordanhubbard/nano-packages/contents',
            headers: { 'User-Agent': 'rcc-api/1.0', 'Accept': 'application/vnd.github.v3+json' },
          }, (r) => {
            let buf = '';
            r.on('data', d => buf += d);
            r.on('end', () => { try { resolve(JSON.parse(buf)); } catch(e) { reject(e); } });
          }).on('error', reject);
        });
        const dirs = Array.isArray(listing) ? listing.filter(e => e.type === 'dir') : [];
        const packages = [];
        for (const dir of dirs) {
          try {
            const manifest = await new Promise((resolve, reject) => {
              const u = new URL(`https://raw.githubusercontent.com/jordanhubbard/nano-packages/main/${dir.name}/nano.toml`);
              _https.get({ hostname: u.hostname, path: u.pathname, headers: { 'User-Agent': 'rcc-api/1.0' } }, (r) => {
                let buf = '';
                r.on('data', d => buf += d);
                r.on('end', () => resolve(buf));
              }).on('error', reject);
            });
            const getField = (section, key) => {
              const secMatch = manifest.match(new RegExp('\\[' + section + '\\]([^]*?)(?=\\n\\[|$)'));
              if (!secMatch) return '';
              const kMatch = secMatch[1].match(new RegExp('^' + key + '\\s*=\\s*["\']?([^"\'\\n]+)["\']?', 'm'));
              return kMatch ? kMatch[1].trim() : '';
            };
            const getDeps = () => {
              const depSection = manifest.match(/\[dependencies\]([^]*?)(?=\n\[|$)/);
              if (!depSection) return {};
              const deps = {};
              for (const line of depSection[1].split('\n')) {
                const m2 = line.match(/^\s*(\w[\w_-]*)\s*=\s*["']?([^"'\n]+)["']?/);
                if (m2) deps[m2[1]] = m2[2].trim();
              }
              return deps;
            };
            packages.push({
              name: getField('package', 'name') || dir.name,
              version: getField('package', 'version'),
              description: getField('package', 'description'),
              author: getField('package', 'author'),
              license: getField('package', 'license'),
              homepage: getField('package', 'homepage'),
              repository: `https://github.com/jordanhubbard/nano-packages/tree/main/${dir.name}`,
              dependencies: getDeps(),
            });
          } catch (_) { /* skip unparseable */ }
        }
        state._pkgCache = { packages, ts: Date.now() };
      } catch (err) {
        if (!state._pkgCache) state._pkgCache = { packages: [], ts: 0 };
        console.warn('[rcc-api] /api/pkg fetch error:', err.message);
      }
    }
    const cache = state._pkgCache;
    return json(res, 200, {
      ok: true,
      count: cache.packages.length,
      packages: cache.packages,
      cached: cache.ts > 0 && (Date.now() - cache.ts) < 5 * 60 * 1000,
      fetchedAt: cache.ts || null,
      registry: 'https://github.com/jordanhubbard/nano-packages',
    });
  });
}
