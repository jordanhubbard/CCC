/**
 * rcc/api — Rocky Command Center REST API
 *
 * Single source of truth for the work queue, agent registry, and heartbeats.
 * Agents talk to this instead of maintaining local queue copies.
 *
 * Port: RCC_PORT env var (default 8789)
 * Auth: Bearer token — must be in RCC_AUTH_TOKENS (comma-separated)
 */

import { createServer } from 'http';
import { readFile, writeFile, mkdir } from 'fs/promises';
import { existsSync } from 'fs';
import { dirname } from 'path';
import { createHmac, timingSafeEqual, randomUUID } from 'crypto';
import { Brain, createRequest } from '../brain/index.mjs';
import { Pump } from '../scout/pump.mjs';
import { learnLesson, queryLessons, queryAllLessons, formatLessonsForContext, getTrendingLessons, formatTrendingForHeartbeat, getHeartbeatContext, receiveLessonFromBus, seedKnownLessons } from '../lessons/index.mjs';

// ── Config ─────────────────────────────────────────────────────────────────
const PORT            = parseInt(process.env.RCC_PORT || '8789', 10);
const QUEUE_PATH      = process.env.QUEUE_PATH    || '../../workqueue/queue.json';
const AGENTS_PATH        = process.env.AGENTS_PATH        || './agents.json';
const CAPABILITIES_PATH  = process.env.CAPABILITIES_PATH  || './data/agent-capabilities.json';
const REPOS_PATH      = process.env.REPOS_PATH    || './repos.json';
const PROJECTS_PATH   = process.env.PROJECTS_PATH || './projects.json';
const RCC_PUBLIC_URL  = process.env.RCC_PUBLIC_URL || 'http://localhost:8789';
const AUTH_TOKENS  = new Set((process.env.RCC_AUTH_TOKENS || '').split(',').map(t => t.trim()).filter(Boolean));
const START_TIME   = Date.now();
const CALENDAR_PATH   = process.env.CALENDAR_PATH   || './data/calendar.json';
const REQUESTS_PATH   = process.env.REQUESTS_PATH   || './data/requests.json';

// ── Slack config ───────────────────────────────────────────────────────────
const SLACK_SIGNING_SECRET = process.env.SLACK_SIGNING_SECRET || '';
const SLACK_BOT_TOKEN      = process.env.SLACK_BOT_TOKEN      || '';
const SLACK_API            = 'https://slack.com/api';

// ── Stale claim thresholds (ms) by executor type ───────────────────────────
// claude_cli: real coding agents, can run 60-90min on complex tasks
// gpu: render jobs, can run hours
// inference_key: fast LLM calls, should finish in minutes
const STALE_THRESHOLDS = {
  claude_cli:    parseInt(process.env.STALE_CLAUDE_MS    || String(120 * 60 * 1000), 10), // 2h
  gpu:           parseInt(process.env.STALE_GPU_MS       || String(6  * 60 * 60 * 1000), 10), // 6h
  inference_key: parseInt(process.env.STALE_INFERENCE_MS || String(30 * 60 * 1000), 10), // 30min
  default:       parseInt(process.env.STALE_DEFAULT_MS   || String(60 * 60 * 1000), 10), // 1h
};

// ── In-memory heartbeats ───────────────────────────────────────────────────
const heartbeats = {};
const heartbeatHistory = {};
const cronStatus = {};
const providerHealth = {};
const geekSseClients = new Set();

// ── Projects I/O ─────────────────────────────────────────────────────────
async function readProjects() {
  const p = new URL(PROJECTS_PATH, import.meta.url).pathname;
  if (!existsSync(p)) return [];
  return JSON.parse(await readFile(p, 'utf8'));
}

async function writeProjects(data) {
  const p = new URL(PROJECTS_PATH, import.meta.url).pathname;
  await writeFile(p, JSON.stringify(data, null, 2));
}

function projectUrl(fullName) {
  return `${RCC_PUBLIC_URL}/api/projects/${encodeURIComponent(fullName)}`;
}

function buildProjectFromRepo(repo) {
  return {
    id:            repo.full_name,
    display_name:  repo.display_name || repo.full_name.split('/')[1],
    description:   repo.description || '',
    github_url:    `https://github.com/${repo.full_name}`,
    rcc_url:       projectUrl(repo.full_name),
    issue_tracker: repo.issue_tracker_url ? `https://${repo.issue_tracker_url}` : `https://github.com/${repo.full_name}/issues`,
    slack_channels: repo.ownership?.slack_channel
      ? [{ workspace: repo.ownership.slack_workspace || 'omgjkh', channel_id: repo.ownership.slack_channel }]
      : [],
    triaging_agent: repo.ownership?.triaging_agent || process.env.DEFAULT_TRIAGING_AGENT || '',
    enabled:        repo.enabled !== false,
    kind:           repo.kind || 'personal',
    scouts:         repo.scouts || [],
    notes:          repo.notes || '',
    registeredAt:   repo.registeredAt || new Date().toISOString(),
    updatedAt:      repo.updatedAt || new Date().toISOString(),
  };
}

// ── Repo helpers ───────────────────────────────────────────────────────────
function repoOwnershipSummary(repo) {
  if (!repo.ownership) return { kind: repo.kind || 'personal', label: repo.full_name.split('/')[0] };
  const o = repo.ownership;
  if (o.model === 'sole') {
    return { kind: 'personal', label: o.owner || repo.full_name.split('/')[0], sole: true };
  }
  // team/org: list contributor logins
  const contributors = Array.isArray(o.contributors)
    ? o.contributors.map(c => typeof c === 'string' ? c : c.github)
    : [];
  return {
    kind: repo.kind || 'team',
    label: contributors.slice(0, 3).join(', ') + (contributors.length > 3 ? ` +${contributors.length - 3}` : ''),
    contributors,
    slack_channel: o.slack_channel || null,
  };
}

// ── Brain (lazy init) ─────────────────────────────────────────────────────
let brain = null;
async function getBrain() {
  if (!brain) {
    brain = new Brain();
    await brain.init();
    brain.start();
  }
  return brain;
}

// ── Pump (lazy init) ──────────────────────────────────────────────────────
let pump = null;
function getPump() {
  if (!pump) {
    pump = new Pump();
    pump.start();
  }
  return pump;
}

// ── Auth ───────────────────────────────────────────────────────────────────
function isAuthed(req) {
  if (AUTH_TOKENS.size === 0) return true; // no tokens configured = open (dev mode)
  const auth = req.headers['authorization'] || '';
  const token = auth.replace(/^Bearer\s+/i, '').trim();
  return AUTH_TOKENS.has(token);
}

// ── Queue I/O ──────────────────────────────────────────────────────────────
async function readQueue() {
  const p = new URL(QUEUE_PATH, import.meta.url).pathname;
  if (!existsSync(p)) return { items: [], completed: [] };
  return JSON.parse(await readFile(p, 'utf8'));
}

async function writeQueue(data) {
  const p = new URL(QUEUE_PATH, import.meta.url).pathname;
  await writeFile(p, JSON.stringify(data, null, 2));
}

// ── Request tickets I/O ───────────────────────────────────────────────────
async function readRequests() {
  const p = new URL(REQUESTS_PATH, import.meta.url).pathname;
  if (!existsSync(p)) return [];
  return JSON.parse(await readFile(p, 'utf8'));
}

async function writeRequests(data) {
  const p = new URL(REQUESTS_PATH, import.meta.url).pathname;
  await writeFile(p, JSON.stringify(data, null, 2));
}

// ── Agent registry I/O ────────────────────────────────────────────────────
async function readAgents() {
  const p = new URL(AGENTS_PATH, import.meta.url).pathname;
  if (!existsSync(p)) return {};
  return JSON.parse(await readFile(p, 'utf8'));
}

async function writeAgents(data) {
  const p = new URL(AGENTS_PATH, import.meta.url).pathname;
  await writeFile(p, JSON.stringify(data, null, 2));
}

// ── Agent capabilities I/O ────────────────────────────────────────────────
async function readCapabilities() {
  const p = new URL(CAPABILITIES_PATH, import.meta.url).pathname;
  if (!existsSync(p)) return {};
  return JSON.parse(await readFile(p, 'utf8'));
}

async function writeCapabilities(data) {
  const p = new URL(CAPABILITIES_PATH, import.meta.url).pathname;
  await mkdir(dirname(p), { recursive: true });
  await writeFile(p, JSON.stringify(data, null, 2));
}

// ── Calendar I/O ───────────────────────────────────────────────────────────
async function readCalendar() {
  const p = new URL(CALENDAR_PATH, import.meta.url).pathname;
  if (!existsSync(p)) return [];
  return JSON.parse(await readFile(p, 'utf8'));
}

async function writeCalendar(data) {
  const p = new URL(CALENDAR_PATH, import.meta.url).pathname;
  await mkdir(dirname(p), { recursive: true });
  await writeFile(p, JSON.stringify(data, null, 2));
}

// ── HTTP helpers ───────────────────────────────────────────────────────────
function json(res, status, body) {
  const payload = JSON.stringify(body);
  res.writeHead(status, { 'Content-Type': 'application/json', 'Access-Control-Allow-Origin': '*' });
  res.end(payload);
}

function readBody(req) {
  return new Promise((resolve, reject) => {
    let body = '';
    req.on('data', chunk => { body += chunk; if (body.length > 1e6) reject(new Error('Body too large')); });
    req.on('end', () => { try { resolve(body ? JSON.parse(body) : {}); } catch { reject(new Error('Invalid JSON')); } });
    req.on('error', reject);
  });
}

// ── Geek SSE broadcast ─────────────────────────────────────────────────────
function broadcastGeekEvent(type, from, to, label) {
  if (geekSseClients.size === 0) return;
  const data = JSON.stringify({ type, from, to, label, ts: new Date().toISOString() });
  const msg = `data: ${data}\n\n`;
  for (const client of geekSseClients) {
    try { client.write(msg); } catch { geekSseClients.delete(client); }
  }
}

// ── HTML UI helpers ────────────────────────────────────────────────────────
const HTML_STYLE = `
  <meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
  <style>
    *{box-sizing:border-box;margin:0;padding:0}
    body{font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',sans-serif;background:#0d1117;color:#e6edf3;min-height:100vh;padding:2rem}
    a{color:#58a6ff;text-decoration:none}a:hover{text-decoration:underline}
    .nav{font-size:.85rem;color:#8b949e;margin-bottom:1.5rem}
    .nav a{color:#8b949e}
    h1{font-size:1.8rem;font-weight:700;margin-bottom:.4rem}
    .subtitle{color:#8b949e;font-size:.95rem;margin-bottom:1.5rem}
    .card{background:#161b22;border:1px solid #30363d;border-radius:8px;padding:1.25rem 1.5rem;margin-bottom:1rem}
    .card h2{font-size:1rem;font-weight:600;margin-bottom:.5rem}
    .meta{display:flex;flex-wrap:wrap;gap:.5rem 1.5rem;font-size:.85rem;color:#8b949e;margin-bottom:.75rem}
    .meta span{display:flex;align-items:center;gap:.3rem}
    .badge{display:inline-block;padding:.15rem .55rem;border-radius:999px;font-size:.75rem;font-weight:600;background:#21262d;border:1px solid #30363d;color:#8b949e}
    .badge.team{border-color:#388bfd55;color:#58a6ff}
    .badge.personal{border-color:#3fb95055;color:#3fb950}
    .scouts{display:flex;flex-wrap:wrap;gap:.35rem;margin-top:.75rem}
    .scout-tag{background:#21262d;border:1px solid #30363d;border-radius:4px;padding:.1rem .5rem;font-size:.75rem;color:#8b949e}
    .notes{color:#c9d1d9;font-size:.875rem;margin-top:.75rem;line-height:1.5;border-top:1px solid #21262d;padding-top:.75rem}
    .links{display:flex;flex-wrap:wrap;gap:.5rem 1.5rem;margin-top:.75rem;font-size:.85rem}
    .project-grid{display:grid;grid-template-columns:repeat(auto-fill,minmax(340px,1fr));gap:1rem}
    .project-card{background:#161b22;border:1px solid #30363d;border-radius:8px;padding:1.25rem;cursor:pointer;transition:border-color .15s}
    .project-card:hover{border-color:#58a6ff}
    .project-card h3{font-size:1rem;font-weight:600;margin-bottom:.35rem}
    .project-card .desc{font-size:.85rem;color:#8b949e;line-height:1.45;display:-webkit-box;-webkit-line-clamp:2;-webkit-box-orient:vertical;overflow:hidden}
    .error{color:#f85149;margin-top:2rem;font-size:1rem}
    .spinner{color:#8b949e;margin-top:2rem}
    .detail-header{margin-bottom:1.5rem}
    .detail-header h1{margin-bottom:.3rem}
    .queue-section h2{font-size:1.1rem;font-weight:600;margin-bottom:.75rem}
    .queue-item{background:#161b22;border:1px solid #30363d;border-radius:6px;padding:.75rem 1rem;margin-bottom:.5rem;font-size:.875rem}
    .queue-item .qi-title{font-weight:600;margin-bottom:.25rem}
    .qi-meta{font-size:.78rem;color:#8b949e;display:flex;gap:.75rem;flex-wrap:wrap}
    .status-badge{display:inline-block;padding:.1rem .45rem;border-radius:4px;font-size:.72rem;font-weight:600;text-transform:uppercase}
    .status-pending{background:#1f2d3d;color:#58a6ff;border:1px solid #388bfd55}
    .status-active{background:#1a2f1a;color:#3fb950;border:1px solid #3fb95055}
    .status-completed{background:#1c1c1c;color:#8b949e;border:1px solid #30363d}
    .status-cancelled{background:#1c1c1c;color:#8b949e;border:1px solid #30363d}
    .status-failed{background:#2d1a1a;color:#f85149;border:1px solid #f8514955}
    .gh-panel{margin-top:1rem}
    .gh-columns{display:grid;grid-template-columns:1fr 1fr;gap:1rem}
    @media(max-width:680px){.gh-columns{grid-template-columns:1fr}}
    .gh-col-header{font-size:.95rem;font-weight:600;margin-bottom:.6rem;display:flex;align-items:center;gap:.5rem}
    .gh-item{background:#0d1117;border:1px solid #21262d;border-radius:6px;padding:.65rem .85rem;margin-bottom:.45rem;font-size:.835rem;transition:border-color .15s}
    .gh-item:hover{border-color:#388bfd55}
    .gh-item-title{font-weight:500;line-height:1.35;margin-bottom:.3rem}
    .gh-item-title a{color:#e6edf3}.gh-item-title a:hover{color:#58a6ff}
    .gh-meta{display:flex;flex-wrap:wrap;align-items:center;gap:.3rem .6rem;font-size:.75rem;color:#8b949e}
    .gh-num{color:#6e7681;font-size:.78rem;margin-right:.2rem}
    .label-chip{display:inline-block;padding:.1rem .42rem;border-radius:999px;font-size:.7rem;font-weight:600;border:1px solid transparent;line-height:1.4}
    .draft-badge{background:#21262d;color:#8b949e;border:1px solid #30363d;padding:.1rem .4rem;border-radius:4px;font-size:.7rem;font-weight:600;margin-right:.2rem}
    .review-approved{color:#3fb950;font-weight:600}.review-changes{color:#f85149;font-weight:600}.review-pending{color:#d29922}
    .merge-ok{color:#a371f7;font-weight:600}.merge-conflict{color:#f85149}
    .gh-empty{color:#8b949e;font-size:.85rem;padding:.4rem 0}
    .gh-refresh-btn{background:transparent;border:1px solid #30363d;color:#8b949e;border-radius:4px;padding:.15rem .55rem;font-size:.75rem;cursor:pointer;transition:border-color .15s,color .15s;margin-left:.5rem}
    .gh-refresh-btn:hover{border-color:#58a6ff;color:#58a6ff}
    .gh-fetched{font-size:.72rem;color:#484f58}
    .gh-error{color:#f85149;font-size:.82rem;padding:.4rem 0}
  </style>`;

function projectsListHtml() {
  return `<!DOCTYPE html><html lang="en"><head>${HTML_STYLE}<title>Projects — RCC</title></head><body>
  <div class="nav"><a href="/">← RCC</a></div>
  <h1>Projects</h1>
  <p class="subtitle">All registered projects tracked by Rocky Command Center</p>
  <div id="root"><p class="spinner">Loading…</p></div>
  <script>
    fetch('/api/projects').then(r=>r.json()).then(projects=>{
      const root=document.getElementById('root');
      if(!projects.length){root.innerHTML='<p class="error">No projects found.</p>';return;}
      const byKind=(k)=>projects.filter(p=>p.kind===k);
      const renderCard=(p)=>\`<a href="/projects/\${encodeURIComponent(p.id)}" style="text-decoration:none">
        <div class="project-card">
          <div style="display:flex;align-items:center;justify-content:space-between;margin-bottom:.4rem">
            <h3>\${p.display_name||p.id}</h3>
            <span class="badge \${p.kind||''}">\${p.kind||'project'}</span>
          </div>
          <div class="desc">\${p.description||''}</div>
        </div></a>\`;
      const sections=[];
      const team=byKind('team'), personal=byKind('personal'), other=projects.filter(p=>p.kind!=='team'&&p.kind!=='personal');
      if(team.length) sections.push(\`<h2 style="font-size:1rem;font-weight:600;color:#8b949e;margin:1.25rem 0 .6rem">Team Projects</h2><div class="project-grid">\${team.map(renderCard).join('')}</div>\`);
      if(personal.length) sections.push(\`<h2 style="font-size:1rem;font-weight:600;color:#8b949e;margin:1.25rem 0 .6rem">Personal Projects</h2><div class="project-grid">\${personal.map(renderCard).join('')}</div>\`);
      if(other.length) sections.push(\`<div class="project-grid">\${other.map(renderCard).join('')}</div>\`);
      root.innerHTML=sections.join('');
    }).catch(e=>{document.getElementById('root').innerHTML='<p class="error">Failed to load projects: '+e.message+'</p>';});
  </script></body></html>`;
}

function projectDetailHtml(projectId) {
  const encodedId = encodeURIComponent(projectId);
  return `<!DOCTYPE html><html lang="en"><head>${HTML_STYLE}<title>${projectId} — RCC</title></head><body>
  <div class="nav"><a href="/projects">← Projects</a></div>
  <div id="root"><p class="spinner">Loading…</p></div>
  <script>
    const projectId=${JSON.stringify(projectId)};
    const encodedId=${JSON.stringify(encodedId)};
    function timeAgo(ds){if(!ds)return'';const s=Math.floor((Date.now()-new Date(ds))/1000);if(s<60)return s+'s ago';if(s<3600)return Math.floor(s/60)+'m ago';if(s<86400)return Math.floor(s/3600)+'h ago';return Math.floor(s/86400)+'d ago';}
    function labelFg(hex){if(!hex||hex==='000000')return'#8b949e';const r=parseInt(hex.slice(0,2),16),g=parseInt(hex.slice(2,4),16),b=parseInt(hex.slice(4,6),16);return(r*299+g*587+b*114)/1000>128?'#0d1117':'#f0f6fc';}
    function labelChip(l){const bg='#'+((l.color&&l.color!=='000000')?l.color:'333');const fg=labelFg(l.color);return\`<span class="label-chip" style="background:\${bg}33;border-color:\${bg}88;color:\${fg}">\${esc(l.name||'')}</span>\`;}
    function renderIssue(i){return\`<div class="gh-item"><div class="gh-item-title"><span class="gh-num">#\${i.number}</span><a href="\${i.url}" target="_blank">\${esc(i.title||'')}</a></div><div class="gh-meta">\${(i.labels||[]).map(labelChip).join('')}<span>\${esc(i.author||'')}</span><span title="\${i.createdAt||''}">\${timeAgo(i.createdAt)}</span>\${i.commentCount?\`<span>💬 \${i.commentCount}</span>\`:''}</div></div>\`;}
    function renderPR(pr){const rc=pr.reviewDecision==='APPROVED'?'review-approved':pr.reviewDecision==='CHANGES_REQUESTED'?'review-changes':'review-pending';const rl=pr.reviewDecision==='APPROVED'?'✓ approved':pr.reviewDecision==='CHANGES_REQUESTED'?'✗ changes req':'⏳ pending review';const mc=pr.mergeable==='MERGEABLE'?'merge-ok':pr.mergeable==='CONFLICTING'?'merge-conflict':'';const ml=pr.mergeable==='MERGEABLE'?'mergeable':pr.mergeable==='CONFLICTING'?'⚠ conflicts':'';return\`<div class="gh-item"><div class="gh-item-title"><span class="gh-num">#\${pr.number}</span>\${pr.isDraft?'<span class="draft-badge">draft</span>':''}<a href="\${pr.url}" target="_blank">\${esc(pr.title||'')}</a></div><div class="gh-meta">\${(pr.labels||[]).map(labelChip).join('')}<span>\${esc(pr.author||'')}</span><span class="\${rc}">\${rl}</span>\${ml?\`<span class="\${mc}">\${ml}</span>\`:''}<span title="\${pr.createdAt||''}">\${timeAgo(pr.createdAt)}</span></div></div>\`;}
    function renderGitHub(ghData){if(!ghData)return'';if(ghData.error)return\`<div class="card gh-panel"><p class="gh-error">GitHub data unavailable: \${esc(ghData.error)}</p></div>\`;const issues=ghData.issues||[];const prs=ghData.prs||[];return\`<div class="card gh-panel"><div style="display:flex;align-items:center;justify-content:space-between;margin-bottom:.85rem"><h2 style="font-size:1.05rem;font-weight:600">🐙 GitHub</h2><span><span class="gh-fetched">fetched \${timeAgo(ghData.fetchedAt)}</span><button class="gh-refresh-btn" onclick="refreshGitHub()">↻ Refresh</button></span></div><div class="gh-columns"><div><div class="gh-col-header">🔴 Issues <span style="color:#8b949e;font-size:.82rem;font-weight:400">\${issues.length} open</span></div>\${issues.length?issues.map(renderIssue).join(''):'<p class="gh-empty">No open issues ✓</p>'}</div><div><div class="gh-col-header">🟣 Pull Requests <span style="color:#8b949e;font-size:.82rem;font-weight:400">\${prs.length} open</span></div>\${prs.length?prs.map(renderPR).join(''):'<p class="gh-empty">No open PRs ✓</p>'}</div></div></div>\`;}
    function refreshGitHub(){const panel=document.querySelector('.gh-panel');if(panel)panel.style.opacity='0.5';fetch('/api/projects/'+encodedId+'/github?refresh=1').then(()=>location.reload()).catch(()=>{if(panel)panel.style.opacity='1';});}
    Promise.all([
      fetch('/api/projects/'+encodedId).then(r=>r.json()),
      fetch('/api/queue').then(r=>r.json()),
      fetch('/api/projects/'+encodedId+'/github').then(r=>r.json()).catch(()=>null),
    ]).then(([p, qdata, ghData])=>{
      if(p.error){document.getElementById('root').innerHTML='<p class="error">'+p.error+'</p>';return;}
      const items=[...(qdata.items||[]),...(qdata.completed||[])].filter(i=>i.project===projectId||i.repo===projectId||(i.slack_channels||[]).some(c=>c===projectId));
      const active=items.filter(i=>!['completed','cancelled'].includes(i.status));
      const done=items.filter(i=>['completed','cancelled'].includes(i.status)).slice(0,10);
      const statusBadge=(s)=>\`<span class="status-badge status-\${s||'pending'}">\${s||'pending'}</span>\`;
      const renderItem=(i)=>\`<div class="queue-item">
        <div class="qi-title">\${i.title||'Untitled'}</div>
        <div class="qi-meta">
          \${statusBadge(i.status)}
          \${i.preferred_executor?'<span>'+i.preferred_executor+'</span>':''}
          \${i.assignedTo?'<span>→ '+i.assignedTo+'</span>':''}
          <span>\${new Date(i.completedAt||i.createdAt||i.created||i.ts||null).toLocaleDateString()}</span>
        </div>
      </div>\`;
      const scoutTags=(p.scouts||[]).map(s=>'<span class="scout-tag">'+s+'</span>').join('');
      const channelLinks=(p.slack_channels||[]).map(c=>'<span>Slack #'+c.channel_id+(c.workspace?' ('+c.workspace+')':'')+'</span>').join('');
      document.getElementById('root').innerHTML=\`
        <div class="detail-header">
          <div style="display:flex;align-items:center;gap:.75rem;margin-bottom:.3rem">
            <h1>\${p.display_name||p.id}</h1>
            <span class="badge \${p.kind||''}">\${p.kind||'project'}</span>
          </div>
          <p class="subtitle">\${p.description||''}</p>
          <div class="links">
            \${p.github_url?'<a href="'+p.github_url+'" target="_blank">GitHub →</a>':''}
            \${p.issue_tracker&&p.issue_tracker!==p.github_url+'/issues'?'<a href="'+p.issue_tracker+'" target="_blank">Issues →</a>':''}
            \${channelLinks}
          </div>
          \${scoutTags?'<div class="scouts">'+scoutTags+'</div>':''}
          \${p.notes?'<div class="notes">'+p.notes+'</div>':''}
        </div>
        \${active.length?'<div class="queue-section card"><h2>Active Work ('+active.length+')</h2>'+active.map(renderItem).join('')+'</div>':''}
        \${done.length?'<div class="queue-section card" style="margin-top:.5rem"><h2>Recent Completed</h2>'+done.map(renderItem).join('')+'</div>':''}
        \${!active.length&&!done.length?'<div class="card"><p style="color:#8b949e;font-size:.875rem">No queue items for this project yet.</p></div>':''}
        \${renderGitHub(ghData)}
      \`
    }).catch(e=>{document.getElementById('root').innerHTML='<p class="error">Failed to load: '+e.message+'</p>';});
  </script></body></html>`;
}

// ── Slack helpers ──────────────────────────────────────────────────────────

/** Read raw body bytes (needed for Slack signature verification) */
function readRawBody(req) {
  return new Promise((resolve, reject) => {
    const chunks = [];
    req.on('data', c => chunks.push(c));
    req.on('end', () => resolve(Buffer.concat(chunks)));
    req.on('error', reject);
  });
}

/** Verify Slack request signature — returns true if valid */
function verifySlackSignature(req, rawBody) {
  if (!SLACK_SIGNING_SECRET) return true; // dev mode — no secret configured
  const ts = req.headers['x-slack-request-timestamp'];
  const sig = req.headers['x-slack-signature'];
  if (!ts || !sig) return false;
  // Replay attack: reject if >5 minutes old
  if (Math.abs(Date.now() / 1000 - parseInt(ts, 10)) > 300) return false;
  const baseString = `v0:${ts}:${rawBody.toString('utf8')}`;
  const hmac = createHmac('sha256', SLACK_SIGNING_SECRET).update(baseString).digest('hex');
  const computed = Buffer.from(`v0=${hmac}`);
  const provided  = Buffer.from(sig);
  if (computed.length !== provided.length) return false;
  return timingSafeEqual(computed, provided);
}

/** Post a message to Slack */
async function slackPost(endpoint, payload) {
  if (!SLACK_BOT_TOKEN) throw new Error('SLACK_BOT_TOKEN not configured');
  const resp = await fetch(`${SLACK_API}/${endpoint}`, {
    method: 'POST',
    headers: {
      'Authorization': `Bearer ${SLACK_BOT_TOKEN}`,
      'Content-Type': 'application/json; charset=utf-8',
    },
    body: JSON.stringify(payload),
  });
  return resp.json();
}

/** Format queue summary for Slack */
async function formatQueueSummary() {
  const qdata = await readQueue();
  const pending = (qdata.items || []).filter(i => i.status === 'pending');
  const inProgress = (qdata.items || []).filter(i => i.status === 'in-progress');
  const top = pending
    .sort((a, b) => {
      const pri = { urgent: 0, high: 1, medium: 2, normal: 2, low: 3, idea: 4 };
      return (pri[a.priority] ?? 2) - (pri[b.priority] ?? 2);
    })
    .slice(0, 3);
  let text = `*Queue status:* ${pending.length} pending, ${inProgress.length} in-progress`;
  if (top.length) {
    text += '\n*Top items:*\n' + top.map(i =>
      `• [${i.priority}] ${i.title?.slice(0, 80) ?? i.id} _(${i.assignee})_`
    ).join('\n');
  }
  return text;
}

/** Format heartbeat/agent status for Slack */
async function formatAgentStatus() {
  const agentsData = await readAgents().catch(() => []);
  const aList = Array.isArray(agentsData) ? agentsData : (agentsData.agents || []);
  const agents = aList.map(a => {
    const mins = a.lastSeen ? Math.round((Date.now() - new Date(a.lastSeen).getTime()) / 60000) : null;
    const status = mins === null ? '?' : mins < 5 ? '🟢' : mins < 30 ? '🟡' : '🔴';
    return `${status} *${a.name || a.id}* — ${mins === null ? 'never' : `${mins}m ago`} (${a.host || 'unknown host'})`;
  });
  return agents.length ? agents.join('\n') : '_No agents registered_';
}

// ── Router ─────────────────────────────────────────────────────────────────
async function handleRequest(req, res) {
  const url = new URL(req.url, `http://localhost`);
  const path = url.pathname;
  const method = req.method;

  // CORS preflight
  if (method === 'OPTIONS') {
    res.writeHead(204, { 'Access-Control-Allow-Origin': '*', 'Access-Control-Allow-Headers': 'Authorization, Content-Type', 'Access-Control-Allow-Methods': 'GET, POST, PATCH, DELETE, OPTIONS' });
    return res.end();
  }

  try {
    // ── Public endpoints ────────────────────────────────────────────────

    if (method === 'GET' && path === '/health') {
      const b = brain;
      const q = await readQueue();
      return json(res, 200, {
        ok: true,
        uptime: Math.floor((Date.now() - START_TIME) / 1000),
        agentCount: Object.keys(heartbeats).length,
        queueDepth: (q.items || []).filter(i => !['completed','cancelled'].includes(i.status)).length,
        lastBrainTick: b?.state?.lastTick || null,
        version: '0.1.0',
      });
    }

    if (method === 'GET' && path === '/api/queue') {
      const q = await readQueue();
      return json(res, 200, { items: q.items || [], completed: q.completed || [] });
    }

    if (method === 'GET' && path === '/api/agents') {
      const agents = await readAgents();
      const caps   = await readCapabilities();
      const result = Object.entries(agents).map(([name, agent]) => ({
        ...agent,
        capabilities: { ...(agent.capabilities || {}), ...(caps[name] || {}) },
        heartbeat: heartbeats[name] || null,
      }));
      return json(res, 200, result);
    }

    // ── GET /api/agents/best?task=X — capability-based routing ───────────
    if (method === 'GET' && path === '/api/agents/best') {
      const task = url.searchParams.get('task') || '';
      const agents = await readAgents();
      const caps   = await readCapabilities();
      const GPU_TASKS    = new Set(['gpu', 'render', 'training', 'inference']);
      const CLAUDE_TASKS = new Set(['claude', 'code', 'review', 'debug', 'triage']);
      const CTX_PRIORITY = { large: 3, medium: 2, small: 1 };

      const candidates = Object.entries(agents).map(([name, agent]) => ({
        name,
        ...agent,
        capabilities: { ...(agent.capabilities || {}), ...(caps[name] || {}) },
        heartbeat: heartbeats[name] || null,
      }));

      // prefer online agents (heartbeat within last 10 min), fall back to all
      const onlineCutoff = Date.now() - 10 * 60 * 1000;
      const online = candidates.filter(a => a.heartbeat && new Date(a.heartbeat.ts).getTime() > onlineCutoff);
      const pool   = online.length > 0 ? online : candidates;

      let best = null;

      if (GPU_TASKS.has(task)) {
        const gpu = pool.filter(a => a.capabilities?.gpu);
        if (gpu.length) best = gpu.sort((a, b) => (b.capabilities.gpu_vram_gb || 0) - (a.capabilities.gpu_vram_gb || 0))[0];
      } else if (CLAUDE_TASKS.has(task)) {
        const cli = pool.filter(a => a.capabilities?.claude_cli);
        if (cli.length) best = cli.sort((a, b) => (CTX_PRIORITY[b.capabilities.context_size] || 0) - (CTX_PRIORITY[a.capabilities.context_size] || 0))[0];
      }

      if (!best) {
        // match preferred_tasks
        const byPref = pool.filter(a => (a.capabilities?.preferred_tasks || []).includes(task));
        if (byPref.length) best = byPref[0];
      }

      if (!best && pool.length) best = pool[0];
      if (!best) return json(res, 404, { error: 'No agents available' });
      return json(res, 200, { agent: best, task });
    }

    if (method === 'GET' && path === '/api/heartbeats') {
      return json(res, 200, heartbeats);
    }

    if (method === 'GET' && path === '/api/brain/status') {
      const b = brain;
      if (!b) return json(res, 200, { ok: true, status: 'not started' });
      return json(res, 200, b.getStatus());
    }

    // ── Item detail (public read) ─────────────────────────────────────────
    const itemDetailMatch = path.match(/^\/api\/item\/([^/]+)$/);
    if (method === 'GET' && itemDetailMatch) {
      const id = decodeURIComponent(itemDetailMatch[1]);
      const q = await readQueue();
      const item = [...(q.items||[]), ...(q.completed||[])].find(i => i.id === id);
      if (!item) return json(res, 404, { error: 'Item not found' });
      return json(res, 200, item);
    }

    // ── Public: GET /api/projects list + detail ──────────────────────────
    if (method === 'GET' && path === '/api/projects') {
      const repos    = await getPump().listRepos();
      const projects = await readProjects();
      const projectMap = new Map(projects.map(p => [p.id, p]));
      const result = repos
        .filter(r => r.enabled !== false)
        .map(r => {
          const base    = buildProjectFromRepo(r);
          const overlay = projectMap.get(r.full_name) || {};
          return { ...base, ...overlay };
        });
      return json(res, 200, result);
    }
    // ── GET /api/projects/:owner/:repo/github — live issues + PRs (public) ─
    // Must be before projectPublicDetailMatch (which would otherwise eat the /github suffix)
    if (method === 'GET' && path.endsWith('/github')) {
      const githubSubMatch = path.match(/^\/api\/projects\/([^/]+(?:\/[^/]+|%2F[^/]+))\/github$/i);
      if (githubSubMatch) {
        const fullName = decodeURIComponent(githubSubMatch[1]);
        if (!globalThis._githubCache) globalThis._githubCache = new Map();
        const cached = globalThis._githubCache.get(fullName);
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
        globalThis._githubCache.set(fullName, { ts: Date.now(), data: result });
        return json(res, 200, result);
      }
    }

    const projectPublicDetailMatch = path.match(/^\/api\/projects\/([^/]+(?:\/[^/]+|%2F[^/]+))$/i);
    if (method === 'GET' && projectPublicDetailMatch) {
      const fullName = decodeURIComponent(projectPublicDetailMatch[1]);
      const repos    = await getPump().listRepos();
      const repo     = repos.find(r => r.full_name === fullName);
      if (!repo) return json(res, 404, { error: 'Project not found' });
      const projects = await readProjects();
      const overlay  = projects.find(p => p.id === fullName) || {};
      const base     = buildProjectFromRepo(repo);
      return json(res, 200, { ...base, ...overlay });
    }

    // ── UI: GET /projects — project list page ────────────────────────────
    if (method === 'GET' && path === '/projects') {
      res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8', 'Access-Control-Allow-Origin': '*' });
      res.end(projectsListHtml());
      return;
    }
    // ── UI: GET /projects/:owner/:repo — project detail page ─────────────
    const projectUiMatch = path.match(/^\/projects\/([^/]+(?:\/[^/]+|%2F[^/]+))$/i);
    if (method === 'GET' && projectUiMatch) {
      res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8', 'Access-Control-Allow-Origin': '*' });
      res.end(projectDetailHtml(decodeURIComponent(projectUiMatch[1])));
      return;
    }

    // ── Auth-required endpoints ───────────────────────────────────────────
    if (!isAuthed(req)) {
      return json(res, 401, { error: 'Unauthorized' });
    }

    // ── POST /api/queue — create item ─────────────────────────────────────
    if (method === 'POST' && path === '/api/queue') {
      const body = await readBody(req);
      if (!body.title) return json(res, 400, { error: 'title required' });
      const q = await readQueue();

      // Scout dedup: if a scout_key is provided, reject if it already exists
      // anywhere in the queue (active OR completed) to prevent hourly re-filing.
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

      // Infer preferred_executor if not specified
      const inferExecutor = (b) => {
        if (b.preferred_executor) return b.preferred_executor;
        const tags = b.tags || [];
        if (tags.includes('gpu') || tags.includes('render') || tags.includes('simulation')) return 'gpu';
        if (tags.includes('reasoning') || tags.includes('code') || tags.includes('complex')) return 'claude_cli';
        if (tags.includes('heartbeat') || tags.includes('status') || tags.includes('poll')) return 'inference_key';
        // Default: claude_cli for assignee-specific tasks, inference_key for housekeeping
        return (b.assignee && b.assignee !== 'all') ? 'claude_cli' : 'inference_key';
      };

      // Prevent ID collisions — if a caller supplies an ID that already exists
      // (in either active items or completed), generate a fresh one instead.
      const allIds = new Set([...(q.items||[]), ...(q.completed||[])].map(i => i.id));
      let itemId = body.id || `wq-API-${Date.now()}`;
      if (body.id && allIds.has(body.id)) {
        itemId = `wq-API-${Date.now()}`;
        console.warn(`[rcc-api] ID collision on "${body.id}" — reassigned to "${itemId}"`);
      }

      const item = {
        id: itemId,
        itemVersion: 1,
        created: new Date().toISOString(),
        source: body.source || 'api',
        assignee: body.assignee || 'all',
        priority: body.priority || 'normal',
        status: 'pending',
        title: body.title,
        description: body.description || '',
        notes: body.notes || '',
        preferred_executor: inferExecutor(body),  // claude_cli | inference_key | gpu
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
        // Scout dedup key — preserved for itemAlreadyExists() checks
        scout_key: body.scout_key || null,
        repo: body.repo || null,
      };
      if (!q.items) q.items = [];
      q.items.push(item);
      await writeQueue(q);
      return json(res, 201, { ok: true, item });
    }

    // ── GET /api/queue/stale — list stale claims ──────────────────────────
    if (method === 'GET' && path === '/api/queue/stale') {
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
    }

    // ── POST /api/queue/expire-stale — server-side stale reset ───────────
    if (method === 'POST' && path === '/api/queue/expire-stale') {
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
    }

    // ── PATCH /api/item/:id ───────────────────────────────────────────────
    const patchMatch = path.match(/^\/api\/item\/([^/]+)$/);
    if (method === 'PATCH' && patchMatch) {
      const id = decodeURIComponent(patchMatch[1]);
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
        // Auto-archive: move completed/cancelled items from items[] to completed[]
        if (item.status === 'completed' || item.status === 'cancelled') {
          q.items = q.items.filter(i => i.id !== item.id);
          if (!q.completed) q.completed = [];
          q.completed.push(item);
        }
        await writeQueue(q);
      }
      return json(res, 200, { ok: true, item });
    }

    // ── POST /api/item/:id/comment ────────────────────────────────────────
    const commentMatch = path.match(/^\/api\/item\/([^/]+)\/comment$/);
    if (method === 'POST' && commentMatch) {
      const id = decodeURIComponent(commentMatch[1]);
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
      return json(res, 200, { ok: true, entry });
    }

    // ── POST /api/item/:id/choice ─────────────────────────────────────────
    const choiceMatch = path.match(/^\/api\/item\/([^/]+)\/choice$/);
    if (method === 'POST' && choiceMatch) {
      const id = decodeURIComponent(choiceMatch[1]);
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
    }

    // ── POST /api/item/:id/ai-comment ─────────────────────────────────────
    const aiMatch = path.match(/^\/api\/item\/([^/]+)\/ai-comment$/);
    if (method === 'POST' && aiMatch) {
      const id = decodeURIComponent(aiMatch[1]);
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

      // Queue to brain for async processing, or call inline if brain available
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
        // Await completion inline (with timeout)
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
    }

    // ── POST /api/agents/register ─────────────────────────────────────────
    if (method === 'POST' && path === '/api/agents/register') {
      const body = await readBody(req);
      if (!body.name) return json(res, 400, { error: 'name required' });
      const agents = await readAgents();
      const token = `rcc-agent-${body.name}-${Math.random().toString(36).slice(2)}${Date.now().toString(36)}`;
      agents[body.name] = {
        name: body.name,
        host: body.host || 'unknown',
        type: body.type || 'full',           // full | container | local | spark
        token,
        registeredAt: new Date().toISOString(),
        lastSeen: null,
        // Worker capabilities — declared at registration, updated via PATCH /api/agents/:name
        capabilities: {
          claude_cli: body.capabilities?.claude_cli ?? false,
          claude_cli_model: body.capabilities?.claude_cli_model || null,
          inference_key: body.capabilities?.inference_key ?? true,
          inference_provider: body.capabilities?.inference_provider || 'nvidia',
          gpu: body.capabilities?.gpu ?? false,
          gpu_model: body.capabilities?.gpu_model || null,
          gpu_count: body.capabilities?.gpu_count || 0,
          gpu_vram_gb: body.capabilities?.gpu_vram_gb || 0,
        },
        billing: {
          claude_cli: body.billing?.claude_cli || 'fixed',
          inference_key: body.billing?.inference_key || 'metered',
          gpu: body.billing?.gpu || 'fixed',
        },
      };
      await writeAgents(agents);
      AUTH_TOKENS.add(token);
      return json(res, 201, { ok: true, token, agent: { ...agents[body.name], token } });
    }

    // ── POST /api/agents/:name — publish capabilities at startup (upsert) ─
    const agentNameMatch = path.match(/^\/api\/agents\/([^/]+)$/);
    if (method === 'POST' && agentNameMatch) {
      const name = decodeURIComponent(agentNameMatch[1]);
      const body = await readBody(req);
      const agents = await readAgents();
      if (!agents[name]) {
        // auto-register on first capability publish
        const token = `rcc-agent-${name}-${Math.random().toString(36).slice(2)}${Date.now().toString(36)}`;
        agents[name] = {
          name,
          host: body.host || 'unknown',
          type: body.type || 'full',
          token,
          registeredAt: new Date().toISOString(),
          lastSeen: null,
          capabilities: {},
          billing: { claude_cli: 'fixed', inference_key: 'metered', gpu: 'fixed' },
        };
        AUTH_TOKENS.add(token);
      } else {
        if (body.host) agents[name].host = body.host;
        if (body.type) agents[name].type = body.type;
      }
      await writeAgents(agents);
      if (body.capabilities) {
        const caps = await readCapabilities();
        caps[name] = { ...(caps[name] || {}), ...body.capabilities };
        await writeCapabilities(caps);
      }
      return json(res, 200, { ok: true, token: agents[name].token, agent: agents[name] });
    }

    // ── PATCH /api/agents/:name — update capabilities ─────────────────────
    const agentPatchMatch = path.match(/^\/api\/agents\/([^/]+)$/);
    if (method === 'PATCH' && agentPatchMatch) {
      const name = decodeURIComponent(agentPatchMatch[1]);
      const body = await readBody(req);
      const agents = await readAgents();
      if (!agents[name]) return json(res, 404, { error: 'Agent not found' });
      if (body.capabilities) Object.assign(agents[name].capabilities || {}, body.capabilities);
      if (body.billing) Object.assign(agents[name].billing || {}, body.billing);
      if (body.host) agents[name].host = body.host;
      if (body.type) agents[name].type = body.type;
      await writeAgents(agents);
      if (body.capabilities) {
        const caps = await readCapabilities();
        caps[name] = { ...(caps[name] || {}), ...body.capabilities };
        await writeCapabilities(caps);
      }
      return json(res, 200, { ok: true, agent: agents[name] });
    }

    // ── POST /api/heartbeat/:agent ────────────────────────────────────────
    const hbMatch = path.match(/^\/api\/heartbeat\/([^/]+)$/);
    if (method === 'POST' && hbMatch) {
      const agent = decodeURIComponent(hbMatch[1]);
      const body = await readBody(req);
      heartbeats[agent] = { agent, ts: new Date().toISOString(), status: 'online', ...body };
      // Ring buffer for heartbeat history (max 288 entries = 24h at 5-min intervals)
      if (!heartbeatHistory[agent]) heartbeatHistory[agent] = [];
      heartbeatHistory[agent].push({ ts: new Date().toISOString(), status: 'online' });
      if (heartbeatHistory[agent].length > 288) heartbeatHistory[agent].shift();
      // Update agent lastSeen
      const agents = await readAgents();
      if (agents[agent]) {
        agents[agent].lastSeen = heartbeats[agent].ts;
        await writeAgents(agents);
      }
      broadcastGeekEvent('heartbeat', agent, 'rocky', `${agent} heartbeat`);
      return json(res, 200, { ok: true });
    }

    // ── POST /api/complete/:id ────────────────────────────────────────────
    const completeMatch = path.match(/^\/api\/complete\/([^/]+)$/);
    if (method === 'POST' && completeMatch) {
      const id = decodeURIComponent(completeMatch[1]);
      const body = await readBody(req);
      const q = await readQueue();
      const item = q.items?.find(i => i.id === id);
      if (!item) return json(res, 404, { error: 'Item not found' });
      item.status = 'completed';
      item.completedAt = new Date().toISOString();
      item.itemVersion = (item.itemVersion || 0) + 1;
      if (body?.result) item.result = body.result;
      await writeQueue(q);

      // ── requestId linkage: resolve matching delegation on parent ticket ──
      if (item.requestId) {
        try {
          const reqs = await readRequests();
          const ticket = reqs.find(r => r.id === item.requestId);
          if (ticket) {
            const outcome = item.result || `Queue item ${item.id} completed`;
            // Find unresolved delegation matching this queue item
            const delIdx = (ticket.delegations || []).findIndex(d =>
              !d.resolvedAt && (d.queueItemId === id || d.summary?.includes(id) || d.summary?.includes(item.title))
            );
            if (delIdx >= 0) {
              ticket.delegations[delIdx].resolvedAt = new Date().toISOString();
              ticket.delegations[delIdx].outcome = outcome;
            }
            // If all delegations resolved, mark ticket resolved
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
    }

    // ── POST /api/brain/request — submit LLM request to brain ────────────
    if (method === 'POST' && path === '/api/brain/request') {
      const body = await readBody(req);
      if (!body.messages || !Array.isArray(body.messages)) return json(res, 400, { error: 'messages array required' });
      const b = await getBrain();
      const req2 = createRequest({
        messages: body.messages,
        maxTokens: body.maxTokens || 1024,
        priority: body.priority || 'normal',
        callbackUrl: body.callbackUrl || null,
        metadata: body.metadata || {},
      });
      const id = await b.enqueue(req2);
      return json(res, 202, { ok: true, requestId: id, status: 'queued' });
    }

    // ── GET /api/brain/status ─────────────────────────────────────────────
    if (method === 'GET' && path === '/api/brain/status') {
      const b = brain;
      if (!b) return json(res, 200, { ok: true, status: 'not started' });
      return json(res, 200, b.getStatus());
    }

    // ── POST /api/lessons — record a lesson ──────────────────────────────
    if (method === 'POST' && path === '/api/lessons') {
      const body = await readBody(req);
      if (!body.domain || !body.symptom || !body.fix) return json(res, 400, { error: 'domain, symptom, fix required' });
      const lesson = await learnLesson({ ...body, agent: body.agent || 'api' });
      return json(res, 201, { ok: true, lesson });
    }

    // ── GET /api/lessons/trending — top lessons by score + recency ────────
    if (method === 'GET' && path === '/api/lessons/trending') {
      const limit = parseInt(url.searchParams.get('limit') || '5', 10);
      const recentDays = parseInt(url.searchParams.get('days') || '7', 10);
      const lessons = await getTrendingLessons({ limit, recentDays });
      const context = url.searchParams.get('format') === 'context' ? formatTrendingForHeartbeat(lessons) : null;
      return json(res, 200, { lessons, context, count: lessons.length });
    }

    // ── GET /api/lessons/heartbeat — context block for heartbeat ──────────
    if (method === 'GET' && path === '/api/lessons/heartbeat') {
      const domains = (url.searchParams.get('domains') || '').split(',').filter(Boolean);
      const context = await getHeartbeatContext({ domains });
      return json(res, 200, { context });
    }

    // ── GET /api/lessons?domain=X&q=keyword+keyword ───────────────────────
    // If no domain specified but q= is present, search across all domains
    if (method === 'GET' && path.startsWith('/api/lessons')) {
      const domain = url.searchParams.get('domain');
      const q = (url.searchParams.get('q') || '').split(/\s+/).filter(Boolean);
      const limit = parseInt(url.searchParams.get('limit') || '5', 10);

      let lessons;
      if (!domain) {
        // Cross-domain search
        lessons = await queryAllLessons({ keywords: q, limit });
      } else {
        lessons = await queryLessons({ domain, keywords: q, limit });
      }
      const context = url.searchParams.get('format') === 'context' ? formatLessonsForContext(lessons) : null;
      return json(res, 200, { lessons, context, count: lessons.length });
    }

    // ── GET /api/repos ────────────────────────────────────────────────────
    if (method === 'GET' && path === '/api/repos') {
      const repos = await getPump().listRepos();
      // Enrich with kind/ownership summary for dashboard
      const enriched = repos.map(r => ({
        ...r,
        kind: r.kind || 'personal',
        ownership_summary: repoOwnershipSummary(r),
      }));
      return json(res, 200, enriched);
    }

    // ── GET /api/repos/:name or PATCH /api/repos/:name ───────────────────
    const repoSingleMatch = path.match(/^\/api\/repos\/([^/]+\/[^/]+)$/);
    if (repoSingleMatch) {
      const fullName = decodeURIComponent(repoSingleMatch[1]);
      if (method === 'GET') {
        const repos = await getPump().listRepos();
        const repo = repos.find(r => r.full_name === fullName);
        if (!repo) return json(res, 404, { error: 'Repo not found' });
        return json(res, 200, { ...repo, ownership_summary: repoOwnershipSummary(repo) });
      }
      if (method === 'PATCH') {
        const body = await readBody(req);
        const repo = await getPump().patchRepo(fullName, body);
        return json(res, 200, { ok: true, repo });
      }
    }

    // ── POST /api/repos/register ──────────────────────────────────────────
    if (method === 'POST' && path === '/api/repos/register') {
      const body = await readBody(req);
      if (!body.full_name) return json(res, 400, { error: 'full_name required (e.g. owner/repo)' });
      const repo = await getPump().registerRepo(body);
      return json(res, 201, { ok: true, repo });
    }

    // ── GET /api/projects — list all projects (derived from repos + projects.json) ──
    if (method === 'GET' && path === '/api/projects') {
      const repos    = await getPump().listRepos();
      const projects = await readProjects();
      // Merge: repos.json is source of truth; projects.json holds Slack channel overrides
      const projectMap = new Map(projects.map(p => [p.id, p]));
      const result = repos
        .filter(r => r.enabled !== false)
        .map(r => {
          const base    = buildProjectFromRepo(r);
          const overlay = projectMap.get(r.full_name) || {};
          return { ...base, ...overlay };
        });
      return json(res, 200, result);
    }

    // ── GET /api/projects/:owner/:repo/github — live issues + PRs ────────
    // Must be before projectDetailMatch (which would otherwise eat the /github suffix)
    const projectGithubMatch = path.match(/^\/api\/projects\/([^/]+(?:\/[^/]+|%2F[^/]+))\/github$/i);
    if (method === 'GET' && projectGithubMatch) {
      const fullName = decodeURIComponent(projectGithubMatch[1]);
      // 5-minute in-memory cache
      if (!globalThis._githubCache) globalThis._githubCache = new Map();
      const cached = globalThis._githubCache.get(fullName);
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
          number: i.number,
          title: i.title,
          url: i.url,
          labels: (i.labels || []).map(l => ({ name: l.name, color: l.color })),
          author: i.author?.login || i.author,
          createdAt: i.createdAt,
          updatedAt: i.updatedAt,
          commentCount: (i.comments || []).length,
        })),
        prs: (prs || []).map(p => ({
          number: p.number,
          title: p.title,
          url: p.url,
          author: p.author?.login || p.author,
          isDraft: p.isDraft || false,
          reviewDecision: p.reviewDecision || null,
          mergeable: p.mergeable || null,
          createdAt: p.createdAt,
          updatedAt: p.updatedAt,
          labels: (p.labels || []).map(l => ({ name: l.name, color: l.color })),
        })),
      };
      globalThis._githubCache.set(fullName, { ts: Date.now(), data: result });
      return json(res, 200, result);
    }

    // ── GET /api/projects/:owner/:repo — single project ───────────────────
    // Handles both /api/projects/owner/repo and /api/projects/owner%2Frepo
    const projectDetailMatch = path.match(/^\/api\/projects\/([^/]+(?:\/[^/]+|%2F[^/]+))$/i);
    if (method === 'GET' && projectDetailMatch) {
      const fullName = decodeURIComponent(projectDetailMatch[1]);
      const repos    = await getPump().listRepos();
      const repo     = repos.find(r => r.full_name === fullName);
      if (!repo) return json(res, 404, { error: 'Project not found' });
      const projects = await readProjects();
      const overlay  = projects.find(p => p.id === fullName) || {};
      const base     = buildProjectFromRepo(repo);
      return json(res, 200, { ...base, ...overlay });
    }

    // ── POST /api/projects/:owner/:repo/channel — register a Slack channel ─
    const projectChannelMatch = path.match(/^\/api\/projects\/([^/]+(?:\/[^/]+|%2F[^/]+))\/channel$/i);
    if (method === 'POST' && projectChannelMatch) {
      const fullName = decodeURIComponent(projectChannelMatch[1]);
      const body     = await readBody(req);
      if (!body.channel_id || !body.workspace) return json(res, 400, { error: 'channel_id and workspace required' });
      const projects = await readProjects();
      let project    = projects.find(p => p.id === fullName);
      if (!project) {
        const repos = await getPump().listRepos();
        const repo  = repos.find(r => r.full_name === fullName);
        if (!repo) return json(res, 404, { error: 'Project not found' });
        project = buildProjectFromRepo(repo);
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
      await writeProjects(projects);
      // Also update repos.json for the primary workspace
      const pump = getPump();
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
      return json(res, 200, { ok: true, project });
    }

    // ── GET /api/context?channel=CXXXX — get project context for a Slack channel ──
    if (method === 'GET' && path === '/api/context') {
      const channelId = url.searchParams.get('channel');
      if (!channelId) return json(res, 400, { error: 'channel query param required' });
      const repos    = await getPump().listRepos();
      const projects = await readProjects();
      // Search repos.json first
      let repo = repos.find(r =>
        r.ownership?.slack_channel === channelId
      );
      // Then projects.json (may have multiple workspaces)
      if (!repo) {
        const projectEntry = projects.find(p =>
          (p.slack_channels || []).some(c => c.channel_id === channelId)
        );
        if (projectEntry) repo = repos.find(r => r.full_name === projectEntry.id);
      }
      if (!repo) return json(res, 404, { error: 'No project associated with this channel' });
      const overlay  = projects.find(p => p.id === repo.full_name) || {};
      const project  = { ...buildProjectFromRepo(repo), ...overlay };
      // Include recent queue items for this project
      const q        = await readQueue();
      const repoItems = (q.items || []).filter(i =>
        i.tags?.includes(repo.full_name) ||
        i.title?.toLowerCase().includes(repo.full_name.split('/')[1].toLowerCase())
      ).slice(-10);
      return json(res, 200, { project, recentItems: repoItems });
    }

    // ── POST /api/bus/receive — handle incoming SquirrelBus messages ──────
    if (method === 'POST' && path === '/api/bus/receive') {
      const body = await readBody(req);
      broadcastGeekEvent('bus_msg', body.from || 'unknown', body.to || 'all', 'SquirrelBus message');
      if (body.type === 'lesson') {
        await receiveLessonFromBus(body);
        return json(res, 200, { ok: true });
      }
      return json(res, 200, { ok: true, ignored: true });
    }

    // ── POST /api/repos/scan — trigger immediate scan ─────────────────────
    if (method === 'POST' && path === '/api/repos/scan') {
      const created = await getPump().scan();
      return json(res, 200, { ok: true, itemsCreated: created });
    }

    // ── POST /api/slack/send — send a message to Slack ─────────────────────
    if (method === 'POST' && path === '/api/slack/send') {
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
    }

    // ── POST /api/slack/events — Slack Events API (app_mention, message.im) ─
    if (method === 'POST' && path === '/api/slack/events') {
      const rawBody = await readRawBody(req);
      if (!verifySlackSignature(req, rawBody)) {
        return json(res, 401, { error: 'Invalid Slack signature' });
      }
      let body;
      try { body = JSON.parse(rawBody.toString('utf8')); } catch { return json(res, 400, { error: 'Invalid JSON' }); }

      // Slack url_verification challenge (app setup handshake)
      if (body.type === 'url_verification') {
        return json(res, 200, { challenge: body.challenge });
      }

      // Process events asynchronously — Slack requires 200 within 3s
      const event = body.event || {};
      json(res, 200, { ok: true }); // respond immediately

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
      return; // already responded
    }

    // ── POST /api/slack/commands — Slack slash commands (/rcc ...) ─────────
    if (method === 'POST' && path === '/api/slack/commands') {
      const rawBody = await readRawBody(req);
      if (!verifySlackSignature(req, rawBody)) {
        return json(res, 401, { error: 'Invalid Slack signature' });
      }
      // Slack sends slash command payloads as URL-encoded form
      const params = Object.fromEntries(new URLSearchParams(rawBody.toString('utf8')));
      const cmdText = (params.text || '').trim().toLowerCase();
      const channel  = params.channel_id;
      const responseUrl = params.response_url;

      // Helper: send delayed response to Slack response_url
      const slackRespond = async (text) => {
        if (!responseUrl) return;
        await fetch(responseUrl, {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ text, response_type: 'in_channel', mrkdwn: true }),
        }).catch(() => {});
      };

      // Acknowledge immediately (required within 3s)
      const ack = { text: '⏳ Working on it...', response_type: 'ephemeral' };

      if (cmdText === 'status' || cmdText === '') {
        json(res, 200, ack);
        const statusText = await formatAgentStatus().catch(e => `Error: ${e.message}`);
        await slackRespond(`*🐿️ RCC Agent Status*\n${statusText}`);
        return;
      }

      if (cmdText === 'queue') {
        json(res, 200, ack);
        const queueText = await formatQueueSummary().catch(e => `Error: ${e.message}`);
        await slackRespond(`*📋 RCC Queue*\n${queueText}`);
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
          await slackRespond(`*🧠 RCC Brain:* ${replyText}`);
        } catch (e) {
          await slackRespond(`⚠️ Error: ${e.message}`);
        }
        return;
      }

      // Unknown command — show help
      return json(res, 200, {
        text: '*RCC Slash Commands*\n`/rcc status` — agent heartbeat status\n`/rcc queue` — pending work items\n`/rcc ask <question>` — ask the RCC brain',
        response_type: 'ephemeral',
      });
    }

    // ── GET /api/calendar ─────────────────────────────────────────────────
    if (method === 'GET' && path === '/api/calendar') {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      let events = await readCalendar();
      const start = url.searchParams.get('start');
      const end   = url.searchParams.get('end');
      const resource = url.searchParams.get('resource');
      if (start) events = events.filter(e => e.end >= start);
      if (end)   events = events.filter(e => e.start <= end);
      if (resource) events = events.filter(e => e.resource === resource);
      return json(res, 200, events);
    }

    // ── POST /api/calendar ────────────────────────────────────────────────
    if (method === 'POST' && path === '/api/calendar') {
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
    }

    // ── DELETE /api/calendar/:id ──────────────────────────────────────────
    const calDeleteMatch = path.match(/^\/api\/calendar\/([^/]+)$/);
    if (method === 'DELETE' && calDeleteMatch) {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const id = decodeURIComponent(calDeleteMatch[1]);
      const events = await readCalendar();
      const idx = events.findIndex(e => e.id === id);
      if (idx === -1) return json(res, 404, { error: 'Event not found' });
      const event = events[idx];
      // Determine caller identity from token (for owner check)
      const auth = req.headers['authorization'] || '';
      const token = auth.replace(/^Bearer\s+/i, '').trim();
      const agents = await readAgents();
      const callerAgent = Object.entries(agents).find(([, a]) => a.token === token)?.[0] || null;
      if (event.owner !== 'rocky' && callerAgent !== event.owner && callerAgent !== 'rocky') {
        return json(res, 403, { error: 'Only the event owner or Rocky may delete this event' });
      }
      events.splice(idx, 1);
      await writeCalendar(events);
      return json(res, 200, { ok: true });
    }

    // ── PATCH /api/calendar/:id ───────────────────────────────────────────
    const calPatchMatch = path.match(/^\/api\/calendar\/([^/]+)$/);
    if (method === 'PATCH' && calPatchMatch) {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const id = decodeURIComponent(calPatchMatch[1]);
      const body = await readBody(req);
      const events = await readCalendar();
      const idx = events.findIndex(e => e.id === id);
      if (idx === -1) return json(res, 404, { error: 'Event not found' });
      events[idx] = { ...events[idx], ...body, id };
      await writeCalendar(events);
      return json(res, 200, { ok: true, event: events[idx] });
    }

    // ── GET /api/appeal ───────────────────────────────────────────────────
    if (method === 'GET' && path === '/api/appeal') {
      const q = await readQueue();
      const all = [...(q.items || []), ...(q.completed || [])];
      const appeals = all.filter(i => i.needsHuman === true || i.status === 'awaiting-jkh');
      appeals.sort((a, b) => {
        const ta = a.needsHumanAt ? new Date(a.needsHumanAt).getTime() : 0;
        const tb = b.needsHumanAt ? new Date(b.needsHumanAt).getTime() : 0;
        return ta - tb;
      });
      return json(res, 200, appeals);
    }

    // ── POST /api/appeal/:id ──────────────────────────────────────────────
    const appealMatch = path.match(/^\/api\/appeal\/([^/]+)$/);
    if (method === 'POST' && appealMatch) {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const id = decodeURIComponent(appealMatch[1]);
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
        // needsHuman stays true
      }
      item.itemVersion = (item.itemVersion || 0) + 1;
      // Re-archive if completed/cancelled
      if (item.status === 'completed' || item.status === 'cancelled') {
        q.items = (q.items || []).filter(i => i.id !== item.id);
        if (!q.completed) q.completed = [];
        if (!q.completed.find(i => i.id === item.id)) q.completed.push(item);
      }
      await writeQueue(q);
      return json(res, 200, { ok: true, item });
    }

    // ── GET /api/heartbeat/:agent/history ─────────────────────────────────
    const hbHistoryMatch = path.match(/^\/api\/heartbeat\/([^/]+)\/history$/);
    if (method === 'GET' && hbHistoryMatch) {
      const agent = decodeURIComponent(hbHistoryMatch[1]);
      return json(res, 200, heartbeatHistory[agent] || []);
    }

    // ── GET /api/crons ────────────────────────────────────────────────────
    if (method === 'GET' && path === '/api/crons') {
      return json(res, 200, Object.values(cronStatus));
    }

    // ── POST /api/crons/:agent ────────────────────────────────────────────
    const cronMatch = path.match(/^\/api\/crons\/([^/]+)$/);
    if (method === 'POST' && cronMatch) {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const agent = decodeURIComponent(cronMatch[1]);
      const body = await readBody(req);
      if (!body.jobId) return json(res, 400, { error: 'jobId required' });
      const key = `${agent}/${body.jobId}`;
      cronStatus[key] = { ...body, agent, updatedAt: new Date().toISOString() };
      return json(res, 200, { ok: true, key });
    }

    // ── GET /api/provider-health ──────────────────────────────────────────
    if (method === 'GET' && path === '/api/provider-health') {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      return json(res, 200, providerHealth);
    }

    // ── POST /api/provider-health ─────────────────────────────────────────
    if (method === 'POST' && path === '/api/provider-health') {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const body = await readBody(req);
      if (!body.provider) return json(res, 400, { error: 'provider required' });
      providerHealth[body.provider] = { ...body, ts: new Date().toISOString() };
      return json(res, 200, { ok: true });
    }

    // ── POST /api/provider-health/:agent ─────────────────────────────────
    const providerMatch = path.match(/^\/api\/provider-health\/([^/]+)$/);
    if (method === 'POST' && providerMatch) {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const agent = decodeURIComponent(providerMatch[1]);
      const body = await readBody(req);
      providerHealth[agent] = { ...body, agent, updatedAt: new Date().toISOString() };
      return json(res, 200, { ok: true });
    }

    // ── GET /api/geek/topology ────────────────────────────────────────────
    if (method === 'GET' && path === '/api/geek/topology') {
      const nodes = [
        { id: 'rocky',          label: 'Rocky',          type: 'agent',          host: 'do-host1',    chips: ['RCC API :8789','WQ Dashboard :8788','RCC Brain','SquirrelBus hub','Tailscale proxy'] },
        { id: 'bullwinkle',     label: 'Bullwinkle',     type: 'agent',          host: 'puck',        chips: ['OpenClaw :18789','SquirrelBus :8788','launchd crons','disk free','uptime'] },
        { id: 'natasha',        label: 'Natasha',        type: 'agent',          host: 'sparky',      chips: ['OpenClaw :18789','SquirrelBus /bus→:18799','Milvus :19530','CUDA/RTX','Ollama :11434'] },
        { id: 'boris',          label: 'Boris',          type: 'agent',          host: 'l40-sweden',  chips: ['OpenClaw gateway','L40 GPU','Omniverse headless'] },
        { id: 'milvus',         label: 'Milvus',         type: 'shared-service', host: 'do-host1',   port: 19530 },
        { id: 'minio',          label: 'MinIO',          type: 'shared-service', host: 'do-host1',   port: 9000 },
        { id: 'searxng',        label: 'SearXNG',        type: 'shared-service', host: 'do-host1',   port: 8888 },
        { id: 'nvidia-gateway', label: 'NVIDIA Gateway', type: 'external',       url: 'inference-api.nvidia.com' },
        { id: 'github',         label: 'GitHub',         type: 'external',       url: 'api.github.com' },
        { id: 'mattermost',     label: 'Mattermost',     type: 'external',       url: 'chat.yourmom.photos' },
        { id: 'slack-omgjkh',   label: 'Slack (omgjkh)', type: 'external',       url: 'omgjkh.slack.com' },
        { id: 'slack-offtera',  label: 'Slack (offtera)', type: 'external',      url: 'offtera.slack.com' },
        { id: 'telegram',       label: 'Telegram',       type: 'external',       url: 'api.telegram.org' },
        { id: 'squirrelbus',    label: 'SquirrelBus',    type: 'bus',            host: 'do-host1' },
      ];
      const edges = [
        { from: 'rocky',      to: 'rcc-api',        type: 'persistent',  protocol: 'internal' },
        { from: 'bullwinkle', to: 'rocky',           type: 'persistent',  protocol: 'heartbeat/HTTP' },
        { from: 'natasha',    to: 'rocky',           type: 'persistent',  protocol: 'heartbeat/HTTP' },
        { from: 'boris',      to: 'rocky',           type: 'persistent',  protocol: 'heartbeat/HTTP' },
        { from: 'rocky',      to: 'milvus',          type: 'on-demand',   protocol: 'gRPC' },
        { from: 'rocky',      to: 'minio',           type: 'on-demand',   protocol: 'S3/HTTP' },
        { from: 'rocky',      to: 'searxng',         type: 'on-demand',   protocol: 'HTTP' },
        { from: 'rocky',      to: 'squirrelbus',     type: 'persistent',  protocol: 'JSONL/fanout' },
        { from: 'bullwinkle', to: 'squirrelbus',     type: 'on-demand',   protocol: 'HTTP' },
        { from: 'natasha',    to: 'squirrelbus',     type: 'on-demand',   protocol: 'HTTP' },
        { from: 'rocky',      to: 'nvidia-gateway',  type: 'on-demand',   protocol: 'HTTPS/OpenAI' },
        { from: 'bullwinkle', to: 'nvidia-gateway',  type: 'on-demand',   protocol: 'HTTPS/OpenAI' },
        { from: 'natasha',    to: 'nvidia-gateway',  type: 'on-demand',   protocol: 'HTTPS/OpenAI' },
        { from: 'rocky',      to: 'github',          type: 'on-demand',   protocol: 'HTTPS/REST' },
        { from: 'rocky',      to: 'mattermost',      type: 'on-demand',   protocol: 'HTTPS/REST' },
        { from: 'rocky',      to: 'slack-omgjkh',    type: 'persistent',  protocol: 'Socket Mode' },
        { from: 'rocky',      to: 'slack-offtera',   type: 'on-demand',   protocol: 'HTTPS/REST' },
        { from: 'rocky',      to: 'telegram',        type: 'on-demand',   protocol: 'HTTPS/Bot API' },
      ];
      const STALE_MS = 5 * 60 * 1000;
      const now = Date.now();
      const nodesWithStatus = nodes.map(n => {
        if (n.type !== 'agent') return n;
        const hb = heartbeats[n.id];
        if (!hb) return { ...n, status: 'offline', lastSeen: null };
        const age = now - new Date(hb.ts).getTime();
        const status = age < STALE_MS ? 'online' : age < 30 * 60 * 1000 ? 'stale' : 'offline';
        return { ...n, status, lastSeen: hb.ts };
      });
      // Dynamic: registered agents
      const agentsData = await readAgents().catch(() => ({}));
      // Dynamic: recent bus messages (last 50 lines of squirrelbus/bus.jsonl)
      let busMessages = [];
      const busPath = new URL('../../squirrelbus/bus.jsonl', import.meta.url).pathname;
      if (existsSync(busPath)) {
        try {
          const busRaw = await readFile(busPath, 'utf8');
          busMessages = busRaw.trim().split('\n').filter(Boolean).slice(-50).map(l => { try { return JSON.parse(l); } catch { return null; } }).filter(Boolean);
        } catch { /* ignore */ }
      }
      // Dynamic: recent heartbeats summary
      const heartbeatSummary = Object.entries(heartbeats).map(([agent, hb]) => ({ agent, ts: hb.ts, status: hb.status || 'online' }));
      return json(res, 200, { nodes: nodesWithStatus, edges, agents: agentsData, busMessages, heartbeatSummary });
    }

    // ── GET /api/geek/stream — SSE live traffic ───────────────────────────
    if (method === 'GET' && path === '/api/geek/stream') {
      res.writeHead(200, {
        'Content-Type':  'text/event-stream',
        'Cache-Control': 'no-cache',
        'Connection':    'keep-alive',
        'Access-Control-Allow-Origin': '*',
      });
      res.write(`data: ${JSON.stringify({ type: 'connected' })}\n\n`);
      geekSseClients.add(res);
      const keepalive = setInterval(() => {
        try { res.write(': keepalive\n\n'); } catch { clearInterval(keepalive); geekSseClients.delete(res); }
      }, 15000);
      req.on('close', () => { clearInterval(keepalive); geekSseClients.delete(res); });
      return; // don't call res.end()
    }

    // ── GET /api/heartbeat-history ────────────────────────────────────────
    if (method === 'GET' && path === '/api/heartbeat-history') {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      return json(res, 200, heartbeatHistory);
    }

    // ── POST /api/cron-status ─────────────────────────────────────────────
    if (method === 'POST' && path === '/api/cron-status') {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const body = await readBody(req);
      if (!body.name) return json(res, 400, { error: 'name required' });
      cronStatus[body.name] = { ...body, ts: new Date().toISOString() };
      return json(res, 200, { ok: true });
    }

    // ── GET /api/cron-status ──────────────────────────────────────────────
    if (method === 'GET' && path === '/api/cron-status') {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      return json(res, 200, cronStatus);
    }

    // ── POST /api/requests — create request ticket ────────────────────────
    if (method === 'POST' && path === '/api/requests') {
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
    }

    // ── GET /api/requests — list tickets ─────────────────────────────────
    if (method === 'GET' && path === '/api/requests') {
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
    }

    // ── GET /api/requests/:id — get one ticket ────────────────────────────
    const reqIdMatch = path.match(/^\/api\/requests\/([^/]+)$/);
    if (method === 'GET' && reqIdMatch) {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const id = decodeURIComponent(reqIdMatch[1]);
      const reqs = await readRequests();
      const ticket = reqs.find(r => r.id === id);
      if (!ticket) return json(res, 404, { error: 'Ticket not found' });
      return json(res, 200, ticket);
    }

    // ── PATCH /api/requests/:id — update ticket fields ────────────────────
    if (method === 'PATCH' && reqIdMatch) {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const id = decodeURIComponent(reqIdMatch[1]);
      const body = await readBody(req);
      const reqs = await readRequests();
      const ticket = reqs.find(r => r.id === id);
      if (!ticket) return json(res, 404, { error: 'Ticket not found' });
      const allowed = ['summary', 'status', 'owner', 'resolution', 'notifiedRequesterAt'];
      for (const k of allowed) { if (k in body) ticket[k] = body[k]; }
      await writeRequests(reqs);
      return json(res, 200, { ok: true, ticket });
    }

    // ── POST /api/requests/:id/delegate — add delegation ─────────────────
    const delegateMatch = path.match(/^\/api\/requests\/([^/]+)\/delegate$/);
    if (method === 'POST' && delegateMatch) {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const id = decodeURIComponent(delegateMatch[1]);
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
    }

    // ── PATCH /api/requests/:id/delegations/:idx — resolve delegation ─────
    const delegResMatch = path.match(/^\/api\/requests\/([^/]+)\/delegations\/(\d+)$/);
    if (method === 'PATCH' && delegResMatch) {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const id = decodeURIComponent(delegResMatch[1]);
      const idx = parseInt(delegResMatch[2], 10);
      const body = await readBody(req);
      const reqs = await readRequests();
      const ticket = reqs.find(r => r.id === id);
      if (!ticket) return json(res, 404, { error: 'Ticket not found' });
      if (!ticket.delegations[idx]) return json(res, 404, { error: 'Delegation not found' });
      ticket.delegations[idx].resolvedAt = new Date().toISOString();
      ticket.delegations[idx].outcome = body.outcome || '';
      // If all delegations resolved, set status to resolved
      if (ticket.delegations.every(d => d.resolvedAt) && ticket.status === 'delegated') {
        ticket.status = 'resolved';
        if (body.outcome) ticket.resolution = body.outcome;
      }
      await writeRequests(reqs);
      return json(res, 200, { ok: true, ticket });
    }

    // ── POST /api/requests/:id/close — notify requester and close ─────────
    const closeMatch = path.match(/^\/api\/requests\/([^/]+)\/close$/);
    if (method === 'POST' && closeMatch) {
      if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
      const id = decodeURIComponent(closeMatch[1]);
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
    }

    return json(res, 404, { error: 'Not found' });

  } catch (err) {
    console.error('[rcc-api] Error:', err.message);
    json(res, 500, { error: err.message });
  }
}

// ── Start server ───────────────────────────────────────────────────────────
export function startServer(port = PORT) {
  const server = createServer(handleRequest);
  server.listen(port, '0.0.0.0', () => {
    console.log(`[rcc-api] 🐿️ RCC API running on http://0.0.0.0:${port}`);
    console.log(`[rcc-api] Auth: ${AUTH_TOKENS.size > 0 ? `${AUTH_TOKENS.size} token(s) configured` : 'OPEN (no tokens set)'}`);
  });
  return server;
}

if (process.argv[1] === new URL(import.meta.url).pathname) {
  startServer();
  process.on('SIGTERM', () => process.exit(0));
  process.on('SIGINT',  () => process.exit(0));
}
