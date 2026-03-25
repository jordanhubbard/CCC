/**
 * rcc/api — Rocky Command Center REST API
 *
 * Single source of truth for the work queue, agent registry, and heartbeats.
 * Agents talk to this instead of maintaining local queue copies.
 *
 * Port: RCC_PORT env var (default 8789)
 * Auth: Bearer token — must be in RCC_AUTH_TOKENS (comma-separated)
 *
 * Slack inbound:
 *   SLACK_SIGNING_SECRET  — Slack app signing secret for verifying event/command payloads
 *   OFFTERA_BOT           — Bot token for workspace THJ9A47K3
 *   OMGJKH_BOT            — Bot token for workspace TE0V8MBEJ
 */

import { createServer } from 'http';
import { readFile, writeFile, mkdir } from 'fs/promises';
import { existsSync } from 'fs';
import { dirname } from 'path';
import { hostname } from 'os';
import { createHmac, timingSafeEqual } from 'crypto';
import { Brain, createRequest } from '../brain/index.mjs';
import { Pump } from '../scout/pump.mjs';
import { learnLesson, queryLessons, queryAllLessons, formatLessonsForContext, getTrendingLessons, formatTrendingForHeartbeat, getHeartbeatContext, receiveLessonFromBus, seedKnownLessons } from '../lessons/index.mjs';
import * as registry from '../capabilities/registry.mjs';

// ── Config ─────────────────────────────────────────────────────────────────
const PORT            = parseInt(process.env.RCC_PORT || '8789', 10);
const QUEUE_PATH      = process.env.QUEUE_PATH    || '../../workqueue/queue.json';
const AGENTS_PATH        = process.env.AGENTS_PATH        || './agents.json';
const CAPABILITIES_PATH  = process.env.CAPABILITIES_PATH  || './data/agent-capabilities.json';
const REGISTRY_PATH      = process.env.REGISTRY_PATH      || './data/capabilities-registry.json';
const REPOS_PATH      = process.env.REPOS_PATH    || './repos.json';
const PROJECTS_PATH   = process.env.PROJECTS_PATH || './projects.json';
const RCC_PUBLIC_URL  = process.env.RCC_PUBLIC_URL || 'http://localhost:8789';
const AUTH_TOKENS  = new Set((process.env.RCC_AUTH_TOKENS || '').split(',').map(t => t.trim()).filter(Boolean));
const START_TIME   = Date.now();

// Configure capability registry persist path
registry.configure({ path: new URL(REGISTRY_PATH, import.meta.url).pathname });

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
      ? [{ workspace: repo.ownership.slack_workspace || '', channel_id: repo.ownership.slack_channel }]
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
    .status-incubating{background:#2a1f3d;color:#d2a8ff;border:1px solid #8957e555}
    .incubator-section{margin-top:1rem}
    .incubator-section h2{font-size:1rem;font-weight:600;margin-bottom:.75rem}
    .incubator-item{background:#161b22;border:1px solid #8957e533;border-radius:8px;padding:.9rem 1rem;margin-bottom:.75rem}
    .incubator-item .inc-title{font-weight:600;font-size:.9rem;margin-bottom:.3rem}
    .inc-desc{font-size:.82rem;color:#8b949e;margin-bottom:.55rem;line-height:1.45}
    .inc-journal{margin:.5rem 0;border-left:2px solid #8957e544;padding-left:.65rem}
    .inc-journal-entry{font-size:.78rem;color:#c9d1d9;margin-bottom:.3rem;line-height:1.4}
    .inc-journal-entry .je-author{color:#d2a8ff;font-weight:600;margin-right:.35rem}
    .inc-journal-entry .je-ts{color:#8b949e;font-size:.72rem;margin-right:.35rem}
    .inc-actions{display:flex;gap:.5rem;margin-top:.6rem;flex-wrap:wrap}
    .inc-comment-form{margin-top:.6rem;display:flex;gap:.5rem}
    .inc-comment-input{flex:1;background:#0d1117;border:1px solid #30363d;border-radius:4px;color:#e6edf3;padding:.3rem .6rem;font-size:.8rem}
    .inc-comment-input:focus{outline:none;border-color:#8957e5}
    .btn-promote{background:#1a2f1a;color:#3fb950;border:1px solid #3fb95055;border-radius:4px;padding:.25rem .65rem;font-size:.78rem;cursor:pointer;font-weight:600}
    .btn-promote:hover{background:#1f3a1f}
    .inc-pri-sel{background:#0d1117;border:1px solid #30363d;color:#8b949e;border-radius:4px;padding:.2rem .4rem;font-size:.75rem;cursor:pointer;transition:border-color .15s}
    .inc-pri-sel:hover,.inc-pri-sel:focus{border-color:#8957e5;outline:none}
    .btn-send-comment{background:#2a1f3d;color:#d2a8ff;border:1px solid #8957e555;border-radius:4px;padding:.25rem .65rem;font-size:.78rem;cursor:pointer}
    .btn-send-comment:hover{background:#321e4f}
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
    function esc(s){const d=document.createElement('div');d.textContent=s||'';return d.innerHTML;}
    function timeAgo(ds){if(!ds)return'';const s=Math.floor((Date.now()-new Date(ds))/1000);if(s<60)return s+'s ago';if(s<3600)return Math.floor(s/60)+'m ago';if(s<86400)return Math.floor(s/3600)+'h ago';return Math.floor(s/86400)+'d ago';}
    function labelFg(hex){if(!hex||hex==='000000')return'#8b949e';const r=parseInt(hex.slice(0,2),16),g=parseInt(hex.slice(2,4),16),b=parseInt(hex.slice(4,6),16);return(r*299+g*587+b*114)/1000>128?'#0d1117':'#f0f6fc';}
    function labelChip(l){const bg='#'+((l.color&&l.color!=='000000')?l.color:'333');const fg=labelFg(l.color);return\`<span class="label-chip" style="background:\${bg}33;border-color:\${bg}88;color:\${fg}">\${esc(l.name||'')}</span>\`;}
    function renderIssue(i){return\`<div class="gh-item"><div class="gh-item-title"><span class="gh-num">#\${i.number}</span><a href="\${i.url}" target="_blank">\${esc(i.title||'')}</a></div><div class="gh-meta">\${(i.labels||[]).map(labelChip).join('')}<span>\${esc(i.author||'')}</span><span title="\${i.createdAt||''}">\${timeAgo(i.createdAt)}</span>\${i.commentCount?\`<span>💬 \${i.commentCount}</span>\`:''}</div></div>\`;}
    function renderPR(pr){const rc=pr.reviewDecision==='APPROVED'?'review-approved':pr.reviewDecision==='CHANGES_REQUESTED'?'review-changes':'review-pending';const rl=pr.reviewDecision==='APPROVED'?'✓ approved':pr.reviewDecision==='CHANGES_REQUESTED'?'✗ changes req':'⏳ pending review';const mc=pr.mergeable==='MERGEABLE'?'merge-ok':pr.mergeable==='CONFLICTING'?'merge-conflict':'';const ml=pr.mergeable==='MERGEABLE'?'mergeable':pr.mergeable==='CONFLICTING'?'⚠ conflicts':'';return\`<div class="gh-item"><div class="gh-item-title"><span class="gh-num">#\${pr.number}</span>\${pr.isDraft?'<span class="draft-badge">draft</span>':''}<a href="\${pr.url}" target="_blank">\${esc(pr.title||'')}</a></div><div class="gh-meta">\${(pr.labels||[]).map(labelChip).join('')}<span>\${esc(pr.author||'')}</span><span class="\${rc}">\${rl}</span>\${ml?\`<span class="\${mc}">\${ml}</span>\`:''}<span title="\${pr.createdAt||''}">\${timeAgo(pr.createdAt)}</span></div></div>\`;}
    function renderGitHub(ghData){if(!ghData)return'';if(ghData.error)return\`<div class="card gh-panel"><p class="gh-error">GitHub data unavailable: \${esc(ghData.error)}</p></div>\`;const issues=ghData.issues||[];const prs=ghData.prs||[];return\`<div class="card gh-panel"><div style="display:flex;align-items:center;justify-content:space-between;margin-bottom:.85rem"><h2 style="font-size:1.05rem;font-weight:600">🐙 GitHub</h2><span><span class="gh-fetched">fetched \${timeAgo(ghData.fetchedAt)}</span><button class="gh-refresh-btn" onclick="refreshGitHub()">↻ Refresh</button></span></div><div class="gh-columns"><div><div class="gh-col-header">🔴 Issues <span style="color:#8b949e;font-size:.82rem;font-weight:400">\${issues.length} open</span></div>\${issues.length?issues.map(renderIssue).join(''):'<p class="gh-empty">No open issues ✓</p>'}</div><div><div class="gh-col-header">🟣 Pull Requests <span style="color:#8b949e;font-size:.82rem;font-weight:400">\${prs.length} open</span></div>\${prs.length?prs.map(renderPR).join(''):'<p class="gh-empty">No open PRs ✓</p>'}</div></div></div>\`;}
    function refreshGitHub(){const panel=document.querySelector('.gh-panel');if(panel)panel.style.opacity='0.5';fetch('/api/projects/'+encodedId+'/github?refresh=1').then(()=>location.reload()).catch(()=>{if(panel)panel.style.opacity='1';});}
    function promoteIdea(id,priority){if(!priority){const sel=document.getElementById('inc-pri-'+id);priority=sel?sel.value:'medium';}if(!priority)return;const rationale=prompt('Rationale: ground this in the project — what empirical evidence, docs, or observed behavior supports it?','');if(rationale===null)return;const btn=document.querySelector('#inc-'+id+' .btn-promote');if(btn){btn.disabled=true;btn.textContent='Promoting…';}fetch('/api/item/'+encodeURIComponent(id)+'/promote',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({priority,rationale,author: process.env.OPERATOR_HANDLE || 'operator'})}).then(r=>r.json()).then(d=>{if(d.ok)location.reload();else{if(btn){btn.disabled=false;btn.textContent='✓ Promote to work item';}alert('Cannot promote: '+d.error);}}).catch(()=>{if(btn){btn.disabled=false;btn.textContent='✓ Promote to work item';}});}
    function sendIncComment(id,author){const inp=document.getElementById('inc-inp-'+id);const text=(inp?.value||'').trim();if(!text)return;const btn=document.querySelector('#inc-'+id+' .btn-send-comment');if(btn){btn.disabled=true;btn.textContent='Sending…';}fetch('/api/item/'+encodeURIComponent(id)+'/comment',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({text,author: author || process.env.OPERATOR_HANDLE || 'operator'})}).then(r=>r.json()).then(d=>{if(d.ok)location.reload();else{if(btn){btn.disabled=false;btn.textContent='Comment';}alert('Error: '+d.error);}}).catch(()=>{if(btn){btn.disabled=false;btn.textContent='Comment';}});}
    function renderIncubatorItem(i){
      const journal=(i.journal||[]).filter(e=>e.type==='comment'||e.type==='ai'||e.type==='incubate-feedback');
      const journalHtml=journal.length?'<div class="inc-journal">'+journal.map(e=>\`<div class="inc-journal-entry"><span class="je-ts">\${timeAgo(e.ts)}</span><span class="je-author">\${esc(e.author||'?')}:</span>\${esc(e.text||'')}</div>\`).join('')+'</div>':'';
      return\`<div class="incubator-item" id="inc-\${i.id}">
        <div class="inc-title">💡 \${esc(i.title||'Untitled')}</div>
        \${i.description?'<div class="inc-desc">'+esc(i.description)+'</div>':''}
        \${journalHtml}
        <div class="inc-comment-form">
          <input class="inc-comment-input" id="inc-inp-\${i.id}" placeholder="Add a comment or refinement…" onkeydown="if(event.key==='Enter')sendIncComment('\${i.id}')">
          <button class="btn-send-comment" onclick="sendIncComment('\${i.id}')">Comment</button>
        </div>
        <div class="inc-actions">
          <select class="inc-pri-sel" id="inc-pri-\${i.id}"><option value="medium">medium</option><option value="normal">normal</option><option value="high">high</option><option value="urgent">urgent</option><option value="low">low</option></select>
          <button class="btn-promote" onclick="promoteIdea('\${i.id}')">✓ Promote to work item</button>
          <span style="font-size:.72rem;color:#8b949e">\${timeAgo(i.created||i.createdAt)} · \${i.source||'api'}</span>
        </div>
      </div>\`;
    }
    function renderIncubatorSection(incubating){if(!incubating.length)return'';return\`<div class="card incubator-section"><div style="display:flex;align-items:center;justify-content:space-between;margin-bottom:.75rem"><h2>💡 Idea Incubator (\${incubating.length})</h2><span style="font-size:.78rem;color:#8b949e">Comment to refine · Promote when ready</span></div>\${incubating.map(renderIncubatorItem).join('')}</div>\`;}
    Promise.all([
      fetch('/api/projects/'+encodedId).then(r=>r.json()),
      fetch('/api/queue').then(r=>r.json()),
      fetch('/api/projects/'+encodedId+'/github').then(r=>r.json()).catch(()=>null),
    ]).then(([p, qdata, ghData])=>{
      if(p.error){document.getElementById('root').innerHTML='<p class="error">'+p.error+'</p>';return;}
      const items=[...(qdata.items||[]),...(qdata.completed||[])].filter(i=>i.project===projectId||i.repo===projectId||(i.slack_channels||[]).some(c=>c===projectId));
      const incubating=items.filter(i=>i.status==='incubating');
      const active=items.filter(i=>!['completed','cancelled','incubating'].includes(i.status));
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
        \${renderIncubatorSection(incubating)}
        \${active.length?'<div class="queue-section card"><h2>Active Work ('+active.length+')</h2>'+active.map(renderItem).join('')+'</div>':''}
        \${done.length?'<div class="queue-section card" style="margin-top:.5rem"><h2>Recent Completed</h2>'+done.map(renderItem).join('')+'</div>':''}
        \${!active.length&&!done.length&&!incubating.length?'<div class="card"><p style="color:#8b949e;font-size:.875rem">No queue items for this project yet.</p></div>':''}
        \${renderGitHub(ghData)}
      \`
    }).catch(e=>{document.getElementById('root').innerHTML='<p class="error">Failed to load: '+e.message+'</p>';});
  </script></body></html>`;
}

// ── Slack helpers ───────────────────────────────────────────────────────────

// Dedup: track last 1000 processed event IDs to ignore retries
const processedEventIds = new Set();
function trackEventId(id) {
  if (processedEventIds.has(id)) return false;
  processedEventIds.add(id);
  if (processedEventIds.size > 1000) {
    processedEventIds.delete(processedEventIds.values().next().value);
  }
  return true;
}

// Map Slack team_id → bot token
function getSlackToken(teamId) {
  if (teamId === 'THJ9A47K3') return process.env.OFFTERA_BOT;
  if (teamId === 'TE0V8MBEJ') return process.env.OMGJKH_BOT;
  return process.env.SLACK_TOKEN;
}

// Post a message to Slack, optionally threaded
async function postSlackReply(token, channel, text, thread_ts) {
  const payload = { channel, text };
  if (thread_ts) payload.thread_ts = thread_ts;
  const r = await fetch('https://slack.com/api/chat.postMessage', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', 'Authorization': `Bearer ${token}` },
    body: JSON.stringify(payload),
  });
  return r.json();
}

// Read raw body bytes (needed before JSON/form parse, for HMAC verification)
function readRawBody(req) {
  return new Promise((resolve, reject) => {
    let body = '';
    req.on('data', chunk => { body += chunk; if (body.length > 1e6) reject(new Error('Body too large')); });
    req.on('end', () => resolve(body));
    req.on('error', reject);
  });
}

// Parse application/x-www-form-urlencoded (Slack slash commands)
function parseFormBody(raw) {
  const obj = {};
  for (const [k, v] of new URLSearchParams(raw).entries()) obj[k] = v;
  return obj;
}

// Verify Slack signing secret — returns true if signature valid and timestamp fresh
function verifySlackSignature(rawBody, headers) {
  const secret = process.env.SLACK_SIGNING_SECRET;
  if (!secret) return false;
  const ts = headers['x-slack-request-timestamp'];
  const sig = headers['x-slack-signature'];
  if (!ts || !sig) return false;
  if (Math.abs(Date.now() / 1000 - parseInt(ts, 10)) > 300) return false; // > 5 min old
  const expected = 'v0=' + createHmac('sha256', secret).update(`v0:${ts}:${rawBody}`).digest('hex');
  try { return timingSafeEqual(Buffer.from(expected), Buffer.from(sig)); } catch { return false; }
}

// ── Shared Slack event/command handlers ────────────────────────────────────

// Handle an inbound Slack event (app_mention or DM). Called by HTTP handler and Socket Mode.
async function handleSlackEvent(event, teamId) {
  const token = getSlackToken(teamId);
  if (!token) return;

  // ── app_mention ──────────────────────────────────────────────────────────
  if (event.type === 'app_mention') {
    const { channel, user, text, thread_ts, ts } = event;
    const cleanText = (text || '').replace(/<@[A-Z0-9]+>/g, '').trim();
    const replyThread = thread_ts || ts;

    await postSlackReply(token, channel, '...', replyThread);

    let ctxNote = '';
    try {
      const repos = await getPump().listRepos();
      const projects = await readProjects();
      let repo = repos.find(r => r.ownership?.slack_channel === channel);
      if (!repo) {
        const pe = projects.find(p => (p.slack_channels || []).some(c => c.channel_id === channel));
        if (pe) repo = repos.find(r => r.full_name === pe.id);
      }
      if (repo) ctxNote = ` Context: project ${repo.full_name}. ${repo.description || ''}`.trimEnd();
    } catch {}

    const reply = await askBrain(
      [
        { role: 'system', content: `You are Rocky, an AI assistant. Be concise and helpful.${ctxNote}` },
        { role: 'user', content: cleanText || '(no message text)' },
      ],
      { channel, user, teamId },
    ).catch(() => "Sorry, I timed out thinking about that 🐿️");

    await postSlackReply(token, channel, reply, replyThread);
  }

  // ── DM messages (not from bots) ─────────────────────────────────────────
  if (event.type === 'message' && !event.bot_id && event.channel?.startsWith('D')) {
    const { channel, user, text, thread_ts, ts } = event;
    if (!text) return;
    const replyThread = thread_ts || ts;

    const reply = await askBrain(
      [
        { role: 'system', content: 'You are Rocky, an AI assistant. Be concise and helpful.' },
        { role: 'user', content: text },
      ],
      { channel, user, teamId },
    ).catch(() => "Sorry, I timed out thinking about that 🐿️");

    await postSlackReply(token, channel, reply, replyThread);
  }
}

// Handle an inbound slash command payload. Returns { text, response_type } or null if handled inline.
// For commands that need async responses, caller must handle the response_url follow-up.
async function handleSlackCommand(payload) {
  const { text = '', response_url, channel_id, team_id, user_id } = payload;
  const args = (text || '').trim().split(/\s+/);
  const sub = args[0]?.toLowerCase() || '';

  if (!sub || sub === 'help') {
    return {
      response_type: 'ephemeral',
      text: '*Rocky Command Center — Available Commands*\n' +
        '• `/rcc status` — agent health summary\n' +
        '• `/rcc queue` — top 5 pending work items\n' +
        '• `/rcc ask <question>` — ask Rocky a question\n' +
        '• `/rcc help` — this message',
    };
  }

  if (sub === 'status') {
    const agents = await readAgents();
    const q = await readQueue();
    const cutoff = Date.now() - 10 * 60 * 1000;
    const agentEntries = Object.entries(agents);
    const onlineNames = new Set(
      agentEntries.filter(([name]) => heartbeats[name] && new Date(heartbeats[name].ts).getTime() > cutoff).map(([n]) => n)
    );
    const pending = (q.items || []).filter(i => !['completed', 'cancelled'].includes(i.status));
    const lines = [
      `*RCC Status* — ${onlineNames.size}/${agentEntries.length} agents online, ${pending.length} pending`,
      ...agentEntries.map(([name]) => `${onlineNames.has(name) ? '✅' : '⬛'} ${name}`),
    ];
    return { response_type: 'in_channel', text: lines.join('\n') };
  }

  if (sub === 'queue') {
    const q = await readQueue();
    const pending = (q.items || []).filter(i => !['completed', 'cancelled'].includes(i.status)).slice(0, 5);
    if (!pending.length) {
      return { response_type: 'in_channel', text: '*Queue is empty* ✓' };
    }
    const lines = pending.map((item, i) =>
      `${i + 1}. *${item.title || 'Untitled'}* — \`${item.status || 'pending'}\`${item.project ? ` [${item.project}]` : ''}`
    );
    return { response_type: 'in_channel', text: `*Top ${pending.length} Queue Items*\n${lines.join('\n')}` };
  }

  if (sub === 'ask') {
    const question = args.slice(1).join(' ').trim();
    if (!question) {
      return { response_type: 'ephemeral', text: 'Usage: `/rcc ask <your question>`' };
    }
    // Fire async — caller handles immediate ack
    setImmediate(async () => {
      const answer = await askBrain(
        [
          { role: 'system', content: 'You are Rocky, an AI assistant. Be concise and helpful.' },
          { role: 'user', content: question },
        ],
        { channel: channel_id, user: user_id, teamId: team_id },
      ).catch(() => "Sorry, I timed out thinking about that 🐿️");

      try {
        if (response_url) {
          await fetch(response_url, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ response_type: 'in_channel', text: answer, replace_original: false }),
          });
        } else {
          const token = getSlackToken(team_id);
          if (token && channel_id) await postSlackReply(token, channel_id, answer);
        }
      } catch (err) {
        console.error('[rcc-api] Slash command reply error:', err.message);
      }
    });
    return { response_type: 'in_channel', text: '...thinking 🐿️', _async: true };
  }

  return {
    response_type: 'ephemeral',
    text: `Unknown command \`/rcc ${sub}\`. Try \`/rcc help\`.`,
  };
}

// ── Socket Mode client ──────────────────────────────────────────────────────

let _socketRetries = 0;
const SOCKET_MAX_RETRIES = 10;

async function startSocketMode() {
  const appToken = process.env.SLACK_APP_TOKEN;
  if (!appToken) return;

  if (_socketRetries >= SOCKET_MAX_RETRIES) {
    console.warn('[rcc-api] Socket Mode: max retries reached, giving up');
    return;
  }

  let wssUrl;
  try {
    const r = await fetch('https://slack.com/api/apps.connections.open', {
      method: 'POST',
      headers: { 'Authorization': `Bearer ${appToken}`, 'Content-Type': 'application/x-www-form-urlencoded' },
    });
    const data = await r.json();
    if (!data.ok) throw new Error(data.error || 'apps.connections.open failed');
    wssUrl = data.url;
  } catch (err) {
    console.error('[rcc-api] Socket Mode: failed to get WSS URL:', err.message);
    _socketRetries++;
    setTimeout(startSocketMode, 5000);
    return;
  }

  let ws;
  try {
    ws = new WebSocket(wssUrl);
  } catch (err) {
    console.error('[rcc-api] Socket Mode: WebSocket constructor failed:', err.message);
    _socketRetries++;
    setTimeout(startSocketMode, 5000);
    return;
  }

  ws.addEventListener('open', () => {
    _socketRetries = 0;
    console.log('[rcc-api] Slack Socket Mode connected');
  });

  ws.addEventListener('message', async (msgEvent) => {
    let envelope;
    try {
      envelope = JSON.parse(msgEvent.data);
    } catch {
      return; // ignore non-JSON
    }

    const { envelope_id, type, payload } = envelope;

    // ACK immediately
    if (envelope_id) {
      try { ws.send(JSON.stringify({ envelope_id })); } catch {}
    }

    try {
      if (type === 'hello') {
        console.log('[rcc-api] Socket Mode: hello received');

      } else if (type === 'disconnect') {
        console.log('[rcc-api] Socket Mode: disconnect received, reconnecting in 2s');
        ws.close();
        setTimeout(startSocketMode, 2000);

      } else if (type === 'events_api') {
        const event = payload?.event;
        const teamId = payload?.team_id;
        if (event && teamId) {
          // Dedup
          const eventId = payload?.event_id;
          if (!eventId || trackEventId(eventId)) {
            handleSlackEvent(event, teamId).catch(err =>
              console.error('[rcc-api] Socket Mode event error:', err.message)
            );
          }
        }

      } else if (type === 'slash_commands') {
        const result = await handleSlackCommand(payload || {}).catch(err => {
          console.error('[rcc-api] Socket Mode command error:', err.message);
          return null;
        });
        // For slash commands over Socket Mode, Slack expects the ACK to carry the response
        // Re-send ACK with payload (second ACK with body)
        if (result && envelope_id && !result._async) {
          const { _async: _, ...response } = result;
          try { ws.send(JSON.stringify({ envelope_id, payload: response })); } catch {}
        }
      }
    } catch (err) {
      console.error('[rcc-api] Socket Mode message handler error:', err.message);
    }
  });

  ws.addEventListener('close', (evt) => {
    if (evt.code === 1000) return; // clean close, no reconnect
    console.warn(`[rcc-api] Socket Mode: connection closed (code ${evt.code}), reconnecting in 5s`);
    _socketRetries++;
    setTimeout(startSocketMode, 5000);
  });

  ws.addEventListener('error', (err) => {
    console.error('[rcc-api] Socket Mode WebSocket error:', err.message || err);
  });
}

// Ask brain and return the response text, with 25s timeout
async function askBrain(messages, metadata = {}) {
  const b = await getBrain();
  const brainReq = createRequest({ messages, maxTokens: 800, priority: 'normal', metadata });
  return Promise.race([
    new Promise(resolve => {
      const onDone = (r) => { if (r.id === brainReq.id) { b.off('completed', onDone); resolve(r.result); } };
      b.on('completed', onDone);
      b.enqueue(brainReq);
    }),
    new Promise(resolve => setTimeout(() => resolve("Sorry, I timed out thinking about that 🐿️"), 25000)),
  ]);
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

    // ── GET /api/agents/best?task=X&executor=Y — capability-based routing ──
    // Accepts:
    //   ?task=gpu|render|code|review|...  — semantic task type
    //   ?executor=claude_cli|gpu|inference_key  — direct executor type (new)
    // Registry manifests (executors[] format) take precedence over old agents.json
    // capability flags.  Online agents (heartbeat within 10min) are preferred.
    if (method === 'GET' && path === '/api/agents/best') {
      const task             = url.searchParams.get('task')     || '';
      const preferredExec    = url.searchParams.get('executor') || null;
      const agents           = await readAgents();
      const caps             = await readCapabilities();
      const manifests        = registry.list();
      const manifestMap      = new Map(manifests.map(m => [m.agent, m]));
      const GPU_TASKS        = new Set(['gpu', 'render', 'training', 'inference']);
      const CLAUDE_TASKS     = new Set(['claude', 'code', 'review', 'debug', 'triage']);
      const CTX_PRIORITY     = { large: 3, medium: 2, small: 1 };

      // Build candidates from agents store; supplement with any registry-only agents
      // (agents that published via /api/capabilities but never called /api/agents/register)
      const agentCandidates = Object.entries(agents).map(([name, agent]) => ({
        name,
        ...agent,
        capabilities: { ...(agent.capabilities || {}), ...(caps[name] || {}) },
        manifest:  manifestMap.get(name) || null,
        heartbeat: heartbeats[name] || null,
      }));
      const agentNames = new Set(agentCandidates.map(a => a.name));
      const registryOnlyCandidates = manifests
        .filter(m => !agentNames.has(m.agent))
        .map(m => ({
          name:         m.agent,
          capabilities: {
            gpu:           m.executors.includes('gpu'),
            claude_cli:    m.executors.includes('claude_cli'),
            inference_key: m.executors.includes('inference_key'),
            gpu_vram_gb:   m.gpuSpec?.vram_gb ?? 0,
            context_size:  null,
            preferred_tasks: m.skills || [],
          },
          manifest:  m,
          heartbeat: heartbeats[m.agent] || null,
        }));
      const candidates = [...agentCandidates, ...registryOnlyCandidates];

      // Prefer online agents (heartbeat within last 10 min); fall back to all
      const onlineCutoff = Date.now() - 10 * 60 * 1000;
      const online = candidates.filter(a => a.heartbeat && new Date(a.heartbeat.ts).getTime() > onlineCutoff);
      const pool   = online.length > 0 ? online : candidates;

      // Helper: check if a candidate supports an executor (registry first, old flags fallback)
      function supportsExecutor(a, exec) {
        if (a.manifest?.executors?.includes(exec)) return true;
        if (exec === 'gpu')           return !!a.capabilities?.gpu;
        if (exec === 'claude_cli')    return !!a.capabilities?.claude_cli;
        if (exec === 'inference_key') return a.capabilities?.inference_key !== false;
        return false;
      }

      // Helper: GPU VRAM from registry or old capabilities
      function gpuVram(a) {
        return a.manifest?.gpuSpec?.vram_gb ?? a.capabilities?.gpu_vram_gb ?? 0;
      }

      let best = null;

      if (preferredExec) {
        // Direct executor routing — consult registry, fall back to old flags
        const capable = pool.filter(a => supportsExecutor(a, preferredExec));
        if (capable.length) {
          best = preferredExec === 'gpu'
            ? capable.sort((a, b) => gpuVram(b) - gpuVram(a))[0]
            : capable[0];
        }
        // If no online capable agent, try all registered agents
        if (!best && online.length > 0) {
          const anyCapable = candidates.filter(a => supportsExecutor(a, preferredExec));
          if (anyCapable.length) {
            best = preferredExec === 'gpu'
              ? anyCapable.sort((a, b) => gpuVram(b) - gpuVram(a))[0]
              : anyCapable[0];
          }
        }
      } else if (GPU_TASKS.has(task)) {
        const gpu = pool.filter(a => supportsExecutor(a, 'gpu'));
        if (gpu.length) best = gpu.sort((a, b) => gpuVram(b) - gpuVram(a))[0];
      } else if (CLAUDE_TASKS.has(task)) {
        const cli = pool.filter(a => supportsExecutor(a, 'claude_cli'));
        if (cli.length) best = cli.sort((a, b) =>
          (CTX_PRIORITY[b.capabilities?.context_size] || 0) - (CTX_PRIORITY[a.capabilities?.context_size] || 0)
        )[0];
      }

      if (!best) {
        // Match skills in registry manifest OR preferred_tasks in old capabilities
        const bySkill = pool.filter(a =>
          (a.manifest?.skills || []).includes(task) ||
          (a.capabilities?.preferred_tasks || []).includes(task)
        );
        if (bySkill.length) best = bySkill[0];
      }

      if (!best && pool.length) best = pool[0];
      if (!best) return json(res, 404, { error: 'No agents available' });
      return json(res, 200, { agent: best, task, executor: preferredExec });
    }

    if (method === 'GET' && path === '/api/heartbeats') {
      return json(res, 200, heartbeats);
    }

    // ── GET /api/capabilities — list all registered agent manifests (public) ─
    if (method === 'GET' && path === '/api/capabilities') {
      return json(res, 200, registry.list());
    }

    // ── GET /api/capabilities/:agent — single agent manifest (public) ────────
    const capSingleMatch = path.match(/^\/api\/capabilities\/([^/]+)$/);
    if (method === 'GET' && capSingleMatch) {
      const agentName = decodeURIComponent(capSingleMatch[1]);
      const manifest  = registry.get(agentName);
      if (!manifest) return json(res, 404, { error: `Agent '${agentName}' not registered in capability registry` });
      return json(res, 200, manifest);
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

    // ── POST /api/capabilities — agent publishes its manifest (auth required) ─
    if (method === 'POST' && path === '/api/capabilities') {
      const body = await readBody(req);
      try {
        const manifest = registry.publish(body);
        console.log(`[rcc-api] Capability manifest published: ${manifest.agent} (${manifest.executors.join(', ')})`);
        return json(res, 200, { ok: true, manifest });
      } catch (err) {
        return json(res, 400, { error: err.message });
      }
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
        // Items created with priority "idea" start in the incubator, not the work queue
        status: (body.status === 'incubating' || body.priority === 'idea') ? 'incubating' : 'pending',
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
        repo: body.repo || body.project || null,
        project: body.project || body.repo || null,
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
      const allowed = ['title','description','priority','assignee','status','notes','choices','claimedBy','claimedAt','result','completedAt','project','repo'];
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
      const userEntry = { ts: now, author: body.author || process.env.OPERATOR_HANDLE || 'operator', type: 'ai', text: `✨ ${prompt}` };
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

    // ── POST /api/item/:id/promote — graduate incubating idea to work item ─
    // Promotion gate rules (enforced server-side):
    //   1. Must have at least 1 comment/discussion entry in the journal
    //   2. Must provide a `rationale` field grounding the idea in project reality
    //      (relevant to the project, based on empirical info, docs, goals, or data)
    //   3. Must have a `project` field linking it to a specific project
    // Use `force: true` to bypass rules (e.g. operator override)
    const promoteMatch = path.match(/^\/api\/item\/([^/]+)\/promote$/);
    if (method === 'POST' && promoteMatch) {
      const id = decodeURIComponent(promoteMatch[1]);
      const body = await readBody(req);
      const q = await readQueue();
      const item = q.items?.find(i => i.id === id);
      if (!item) return json(res, 404, { error: 'Item not found' });

      // Gate check (skip if force:true)
      if (!body.force) {
        const discussionEntries = (item.journal || []).filter(e =>
          ['comment', 'ai', 'incubate-feedback'].includes(e.type)
        );
        if (discussionEntries.length === 0) {
          return json(res, 422, {
            error: 'Promotion blocked: idea needs at least one comment or discussion entry before promotion. Refine the idea first.',
            gate: 'needs_discussion',
          });
        }
        if (!body.rationale || body.rationale.trim().length < 20) {
          return json(res, 422, {
            error: 'Promotion blocked: provide a `rationale` (≥20 chars) grounding this idea in the project — relevant to project goals, based on empirical info, docs, or observed behavior.',
            gate: 'needs_rationale',
          });
        }
        if (!item.project && !item.repo) {
          return json(res, 422, {
            error: 'Promotion blocked: idea must be linked to a specific project. Set `project` field first.',
            gate: 'needs_project',
          });
        }
      }

      const now = new Date().toISOString();
      const prevStatus = item.status;
      item.status = 'pending';
      item.priority = body.priority || item.priority || 'medium';
      if (!item.journal) item.journal = [];
      if (body.rationale) {
        item.journal.push({
          ts: now,
          author: body.author || 'api',
          type: 'promotion-rationale',
          text: `Rationale: ${body.rationale}`,
        });
      }
      item.journal.push({
        ts: now,
        author: body.author || 'api',
        type: 'promoted',
        text: `Promoted from incubation (was: ${prevStatus}) with priority: ${item.priority}${body.force ? ' [force]' : ''}`,
      });
      item.itemVersion = (item.itemVersion || 0) + 1;
      await writeQueue(q);
      return json(res, 200, { ok: true, item });
    }

    // ── POST /api/item/:id/incubate — send item back to incubation ────────
    const incubateMatch = path.match(/^\/api\/item\/([^/]+)\/incubate$/);
    if (method === 'POST' && incubateMatch) {
      const id = decodeURIComponent(incubateMatch[1]);
      const body = await readBody(req);
      const q = await readQueue();
      const item = q.items?.find(i => i.id === id);
      if (!item) return json(res, 404, { error: 'Item not found' });
      const now = new Date().toISOString();
      const prevStatus = item.status;
      item.status = 'incubating';
      item.claimedBy = null;
      item.claimedAt = null;
      if (!item.journal) item.journal = [];
      item.journal.push({
        ts: now,
        author: body.author || 'api',
        type: 'incubate-feedback',
        text: body.feedback ? `Sent back for incubation: ${body.feedback}` : `Sent back for incubation (was: ${prevStatus})`,
      });
      item.itemVersion = (item.itemVersion || 0) + 1;
      await writeQueue(q);
      return json(res, 200, { ok: true, item });
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
      // Update agent lastSeen
      const agents = await readAgents();
      if (agents[agent]) {
        agents[agent].lastSeen = heartbeats[agent].ts;
        await writeAgents(agents);
      }
      return json(res, 200, { ok: true });
    }

    // ── POST /api/complete/:id ────────────────────────────────────────────
    const completeMatch = path.match(/^\/api\/complete\/([^/]+)$/);
    if (method === 'POST' && completeMatch) {
      const id = decodeURIComponent(completeMatch[1]);
      const q = await readQueue();
      const item = q.items?.find(i => i.id === id);
      if (!item) return json(res, 404, { error: 'Item not found' });
      item.status = 'completed';
      item.completedAt = new Date().toISOString();
      item.itemVersion = (item.itemVersion || 0) + 1;
      await writeQueue(q);
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
        if (!repo.ownership.slack_channel || body.workspace === (process.env.PRIMARY_SLACK_WORKSPACE || '')) {
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

    // ── POST /api/slack/events — inbound Slack Events API ────────────────
    if (method === 'POST' && path === '/api/slack/events') {
      const rawBody = await readRawBody(req);
      if (!verifySlackSignature(rawBody, req.headers)) {
        return json(res, 401, { error: 'Invalid Slack signature' });
      }
      let body;
      try { body = JSON.parse(rawBody); } catch { return json(res, 400, { error: 'Invalid JSON' }); }

      // URL verification challenge (initial app setup)
      if (body.type === 'url_verification') {
        return json(res, 200, { challenge: body.challenge });
      }

      if (body.type !== 'event_callback') {
        return json(res, 200, { ok: true });
      }

      const event = body.event || {};
      const teamId = body.team_id;
      const eventId = body.event_id;

      // Dedup: acknowledge and skip if already processed
      if (eventId && !trackEventId(eventId)) {
        return json(res, 200, { ok: true, deduped: true });
      }

      // Acknowledge within 3s — processing happens async below
      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify({ ok: true }));

      setImmediate(() => {
        handleSlackEvent(event, teamId).catch(err =>
          console.error('[rcc-api] Slack event processing error:', err.message)
        );
      });

      return; // already responded
    }

    // ── POST /api/slack/commands — Slack slash commands ───────────────────
    if (method === 'POST' && path === '/api/slack/commands') {
      const rawBody = await readRawBody(req);
      if (!verifySlackSignature(rawBody, req.headers)) {
        return json(res, 401, { error: 'Invalid Slack signature' });
      }
      const body = parseFormBody(rawBody);
      const { text = '', response_url, channel_id, team_id, user_id } = body;

      const result = await handleSlackCommand({ text, response_url, channel_id, team_id, user_id });
      const { _async, ...response } = result;
      if (_async) {
        // /rcc ask: already fired async; send placeholder ack
        res.writeHead(200, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify(response));
        return;
      }
      return json(res, 200, response);
    }

    // ── POST /api/slack/send — send a message via Slack as a named agent ──
    if (method === 'POST' && path === '/api/slack/send') {
      const slackToken = process.env.SLACK_TOKEN;
      if (!slackToken) {
        return json(res, 503, { error: 'Slack not configured on this RCC server' });
      }
      const body = await readBody(req);
      const { text, channel, as_agent, thread_ts } = body;
      if (!text || !channel) {
        return json(res, 400, { error: 'text and channel are required' });
      }

      // Load agent display config from capabilities file if available
      let username = as_agent ? (as_agent.charAt(0).toUpperCase() + as_agent.slice(1)) : 'Agent';
      let icon_emoji = ':robot_face:';
      if (as_agent) {
        try {
          const capPath = new URL(`../agents/${as_agent}.capabilities.json`, import.meta.url).pathname;
          const capRaw = await readFile(capPath, 'utf8');
          const cap = JSON.parse(capRaw);
          if (cap.slack?.username) username = cap.slack.username;
          if (cap.slack?.icon_emoji) icon_emoji = cap.slack.icon_emoji;
        } catch { /* no capabilities file — use defaults */ }
      }

      const payload = { channel, text, username, icon_emoji };
      if (thread_ts) payload.thread_ts = thread_ts;

      const slackRes = await fetch('https://slack.com/api/chat.postMessage', {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          'Authorization': `Bearer ${slackToken}`,
        },
        body: JSON.stringify(payload),
      });
      const slackData = await slackRes.json();
      if (!slackData.ok) {
        return json(res, 502, { error: slackData.error || 'Slack API error' });
      }
      return json(res, 200, { ok: true, ts: slackData.ts });
    }

    return json(res, 404, { error: 'Not found' });

  } catch (err) {
    console.error('[rcc-api] Error:', err.message);
    if (!res.headersSent) {
      json(res, 500, { error: err.message });
    }
  }
}

// ── Start server ───────────────────────────────────────────────────────────
export function startServer(port = PORT) {
  // Load persisted capability registry and publish Rocky's own manifest
  (async () => {
    try {
      await registry.load();
      registry.publish({
        agent:     'rocky',
        host:      process.env.HOSTNAME || hostname(),
        executors: ['claude_cli', 'inference_key'],
        gpuSpec:   null,
        skills:    ['code', 'review', 'debug', 'triage', 'ci'],
        status:    'online',
      });
      console.log('[rcc-api] Rocky capability manifest published to registry');
    } catch (err) {
      console.error('[rcc-api] Registry init error:', err.message);
    }
  })();

  const server = createServer(handleRequest);
  server.listen(port, '0.0.0.0', () => {
    console.log(`[rcc-api] 🐿️ RCC API running on http://0.0.0.0:${port}`);
    console.log(`[rcc-api] Auth: ${AUTH_TOKENS.size > 0 ? `${AUTH_TOKENS.size} token(s) configured` : 'OPEN (no tokens set)'}`);
    if (process.env.SLACK_APP_TOKEN) {
      startSocketMode().catch(err => console.error('[rcc-api] Socket Mode startup error:', err.message));
    }
  });
  return server;
}

if (process.argv[1] === new URL(import.meta.url).pathname) {
  startServer();
  process.on('SIGTERM', () => process.exit(0));
  process.on('SIGINT',  () => process.exit(0));
}
