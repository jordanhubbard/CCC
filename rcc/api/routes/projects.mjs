/**
 * rcc/api/routes/projects.mjs — Project CRUD route handlers
 * Extracted from api/index.mjs (structural refactor only — no logic changes)
 */

export default function registerRoutes(app, state) {
  const { json, readBody, isAuthed } = state;

  // ── Public: GET /api/projects — list all projects ─────────────────────
  app.on('GET', '/api/projects', async (req, res) => {
    const repos    = await state.getPump().listRepos();
    const projects = await state.readProjects();
    const projectMap = new Map(projects.map(p => [p.id, p]));
    const result = repos
      .filter(r => r.enabled !== false)
      .map(r => {
        const base    = state.buildProjectFromRepo(r);
        const overlay = projectMap.get(r.full_name) || {};
        return { ...base, ...overlay };
      });
    return json(res, 200, result);
  });

  // ── Public: GET /api/projects/:owner/:repo/github — live issues + PRs ─
  // Must be registered before the detail route (which would eat the /github suffix)
  app.on('GET', /^\/api\/projects\/([^/]+(?:\/[^/]+|%2F[^/]+))\/github$/i, async (req, res, m, url) => {
    const fullName = decodeURIComponent(m[1]);
    if (!state._githubCache) state._githubCache = new Map();
    const cached = state._githubCache.get(fullName);
    const bustCache = url.searchParams.get('refresh') === '1';
    if (cached && !bustCache && (Date.now() - cached.ts) < 5 * 60 * 1000) {
      return json(res, 200, cached.data);
    }
    const { execSync } = await import('child_process');
    function ghq(args, fields) {
      try {
        const out = execSync(`gh ${args} --json ${fields}`, { encoding: 'utf8', stdio: ['pipe','pipe','pipe'] });
        return JSON.parse(out);
      } catch { return null; }
    }
    const issues = ghq(`issue list --repo ${fullName} --state open --limit 50`,
      'number,title,labels,url,author,createdAt,updatedAt,comments') || [];
    const prs = ghq(`pr list --repo ${fullName} --state open --limit 30`,
      'number,title,author,url,isDraft,reviewDecision,mergeable,createdAt,updatedAt,labels') || [];
    const result = {
      repo: fullName,
      fetchedAt: new Date().toISOString(),
      issues: issues.map(i => ({
        number: i.number, title: i.title, url: i.url,
        labels: (i.labels || []).map(l => ({ name: l.name, color: l.color })),
        author: i.author?.login || i.author,
        createdAt: i.createdAt, updatedAt: i.updatedAt,
        commentCount: (i.comments || []).length,
      })),
      prs: (prs || []).map(p => ({
        number: p.number, title: p.title, url: p.url,
        author: p.author?.login || p.author,
        isDraft: p.isDraft || false,
        reviewDecision: p.reviewDecision || null,
        mergeable: p.mergeable || null,
        createdAt: p.createdAt, updatedAt: p.updatedAt,
        labels: (p.labels || []).map(l => ({ name: l.name, color: l.color })),
      })),
    };
    state._githubCache.set(fullName, { ts: Date.now(), data: result });
    return json(res, 200, result);
  });

  // ── Public: GET /api/projects/:owner/:repo — single project ───────────
  app.on('GET', /^\/api\/projects\/([^/]+(?:\/[^/]+|%2F[^/]+))$/i, async (req, res, m) => {
    const fullName = decodeURIComponent(m[1]);
    const repos    = await state.getPump().listRepos();
    const repo     = repos.find(r => r.full_name === fullName);
    if (!repo) return json(res, 404, { error: 'Project not found' });
    const projects = await state.readProjects();
    const overlay  = projects.find(p => p.id === fullName) || {};
    const base     = state.buildProjectFromRepo(repo);
    return json(res, 200, { ...base, ...overlay });
  });

  // ── POST /api/projects — create project ───────────────────────────────
  app.on('POST', '/api/projects', async (req, res) => {
    const body = await readBody(req);
    if (!body.name) return json(res, 400, { error: 'name required' });
    const projects = await state.readProjects();
    const id = `proj-${Date.now()}`;
    const project = {
      id,
      name: body.name,
      description: body.description || '',
      repoUrl: body.repoUrl || null,
      slackChannels: body.slackChannels || [],
      tags: body.tags || [],
      status: body.status || 'active',
      createdAt: new Date().toISOString(),
      updatedAt: new Date().toISOString(),
    };
    projects.push(project);
    await state.writeProjects(projects);
    return json(res, 201, { ok: true, project });
  });

  // ── POST /api/projects/:owner/:repo/channel — register Slack channel ──
  app.on('POST', /^\/api\/projects\/([^/]+(?:\/[^/]+|%2F[^/]+))\/channel$/i, async (req, res, m) => {
    const fullName = decodeURIComponent(m[1]);
    const body     = await readBody(req);
    if (!body.channel_id || !body.workspace) return json(res, 400, { error: 'channel_id and workspace required' });
    const projects = await state.readProjects();
    let project    = projects.find(p => p.id === fullName);
    if (!project) {
      const repos = await state.getPump().listRepos();
      const repo  = repos.find(r => r.full_name === fullName);
      if (!repo) return json(res, 404, { error: 'Project not found' });
      project = state.buildProjectFromRepo(repo);
      projects.push(project);
    }
    if (!project.slack_channels) project.slack_channels = [];
    // Upsert by workspace
    const existing = project.slack_channels.find(c => c.workspace === body.workspace);
    if (existing) {
      existing.channel_id = body.channel_id;
      existing.channel_name = body.channel_name || existing.channel_name;
      existing.updatedAt  = new Date().toISOString();
    } else {
      project.slack_channels.push({
        workspace:    body.workspace,
        channel_id:   body.channel_id,
        channel_name: body.channel_name || null,
        addedAt:      new Date().toISOString(),
      });
    }
    project.updatedAt = new Date().toISOString();
    await state.writeProjects(projects);
    // Also update repos.json for the primary workspace
    const pump = state.getPump();
    const repos = await pump.listRepos();
    const repo  = repos.find(r => r.full_name === fullName);
    if (repo) {
      if (!repo.ownership) repo.ownership = {};
      if (!repo.ownership.slack_channel || body.workspace === 'omgjkh') {
        repo.ownership.slack_channel   = body.channel_id;
        repo.ownership.slack_workspace = body.workspace;
        await pump.patchRepo(fullName, { ownership: repo.ownership });
      }
    }
    // Set channel topic and description to reflect project metadata
    await state.setSlackChannelMeta(body.channel_id, project).catch(e =>
      console.warn(`[rcc-api] setSlackChannelMeta ${body.channel_id}: ${e.message}`));
    return json(res, 200, { ok: true, project });
  });

  // ── PATCH /api/projects/:id — update project ──────────────────────────
  app.on('PATCH', /^\/api\/projects\/([^/]+)$/, async (req, res, m) => {
    const id = decodeURIComponent(m[1]);
    const body = await readBody(req);
    const projects = await state.readProjects();
    const idx = projects.findIndex(p => p.id === id);
    if (idx === -1) return json(res, 404, { error: 'Project not found' });
    const allowed = ['name','description','repoUrl','slackChannels','tags','status'];
    for (const field of allowed) {
      if (body[field] !== undefined) projects[idx][field] = body[field];
    }
    projects[idx].updatedAt = new Date().toISOString();
    await state.writeProjects(projects);
    return json(res, 200, { ok: true, project: projects[idx] });
  });

  // ── DELETE /api/projects/:id — soft-delete (archive) ─────────────────
  app.on('DELETE', /^\/api\/projects\/([^/]+)$/, async (req, res, m) => {
    const id = decodeURIComponent(m[1]);
    const projects = await state.readProjects();
    const idx = projects.findIndex(p => p.id === id);
    if (idx === -1) return json(res, 404, { error: 'Project not found' });
    projects[idx].status = 'archived';
    projects[idx].updatedAt = new Date().toISOString();
    await state.writeProjects(projects);
    return json(res, 200, { ok: true, project: projects[idx] });
  });
}
