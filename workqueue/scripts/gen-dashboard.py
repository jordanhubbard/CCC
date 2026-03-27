#!/usr/bin/env python3
"""Generate agent status dashboard HTML with embedded heartbeat + workqueue data."""
import sys, json

def load(s):
    if not s or s.strip() == 'null': return None
    try: return json.loads(s)
    except: return None

def normalize(d):
    if d is None: return None
    s = d.get("status", "ok")
    if s in ("ok", "online"): d["status"] = "online"
    return d

rocky      = normalize(load(sys.argv[1] if len(sys.argv) > 1 else None))
natasha    = normalize(load(sys.argv[2] if len(sys.argv) > 2 else None))
bullwinkle = normalize(load(sys.argv[3] if len(sys.argv) > 3 else None))
queue_json = load(sys.argv[4] if len(sys.argv) > 4 else None)

data = {"natasha": natasha, "rocky": rocky, "bullwinkle": bullwinkle}
data_json = json.dumps(data)

# Build merged queue items list (active items + completed items, most recent first)
all_items = []
if queue_json:
    active = queue_json.get("items", [])
    completed = queue_json.get("completed", [])
    all_items = active + completed
    # sort: pending/in_progress first, then by created desc
    STATUS_ORDER = {"in_progress": 0, "pending": 1, "blocked": 2, "deferred": 3, "completed": 4}
    all_items.sort(key=lambda x: (STATUS_ORDER.get(x.get("status",""), 9), x.get("created","")))

queue_items_json = json.dumps(all_items)

print(f"""<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta http-equiv="refresh" content="120">
<title>Agent Status Dashboard</title>
<style>
  * {{ box-sizing: border-box; margin: 0; padding: 0; }}
  body {{ font-family: 'Segoe UI', system-ui, sans-serif; background: #0d1117; color: #c9d1d9; min-height: 100vh; padding: 2rem; }}
  h1 {{ font-size: 1.4rem; font-weight: 600; color: #f0f6fc; margin-bottom: 0.25rem; }}
  h2 {{ font-size: 1.05rem; font-weight: 600; color: #f0f6fc; margin: 2rem 0 0.75rem; }}
  .subtitle {{ font-size: 0.8rem; color: #6e7681; margin-bottom: 2rem; }}
  .grid {{ display: grid; grid-template-columns: repeat(auto-fit, minmax(260px, 1fr)); gap: 1rem; }}
  .card {{ background: #161b22; border: 1px solid #30363d; border-radius: 8px; padding: 1.25rem; }}
  .card.online {{ border-left: 3px solid #3fb950; }}
  .card.offline {{ border-left: 3px solid #f85149; }}
  .card.stale {{ border-left: 3px solid #d29922; }}
  .agent-name {{ font-size: 1.1rem; font-weight: 600; color: #f0f6fc; display: flex; align-items: center; gap: 0.5rem; }}
  .dot {{ width: 8px; height: 8px; border-radius: 50%; flex-shrink: 0; }}
  .dot.online {{ background: #3fb950; box-shadow: 0 0 6px #3fb950aa; }}
  .dot.offline {{ background: #f85149; }}
  .dot.stale {{ background: #d29922; }}
  .host {{ font-size: 0.75rem; color: #6e7681; margin: 0.25rem 0 0.75rem; }}
  .field {{ font-size: 0.8rem; margin: 0.25rem 0; }}
  .label {{ color: #6e7681; }}
  .value {{ color: #c9d1d9; }}
  .badge {{ font-size: 0.7rem; font-weight: 600; padding: 0.15rem 0.5rem; border-radius: 12px; margin-left: auto; }}
  .badge-online {{ background: #1a3a1a; color: #3fb950; }}
  .badge-offline {{ background: #3a1a1a; color: #f85149; }}
  .badge-stale {{ background: #2e2200; color: #d29922; }}

  /* Workqueue table */
  .wq-wrap {{ overflow-x: auto; margin-top: 0.5rem; }}
  table {{ width: 100%; border-collapse: collapse; font-size: 0.8rem; }}
  thead tr {{ background: #161b22; }}
  thead th {{ text-align: left; padding: 0.5rem 0.75rem; color: #8b949e; font-weight: 600; border-bottom: 1px solid #30363d; white-space: nowrap; }}
  tbody tr {{ border-bottom: 1px solid #21262d; transition: background 0.1s; }}
  tbody tr:hover {{ background: #1c2129; }}
  td {{ padding: 0.45rem 0.75rem; vertical-align: top; }}
  .td-desc {{ max-width: 340px; color: #c9d1d9; }}
  .td-desc .title {{ color: #f0f6fc; font-weight: 500; }}
  .td-desc .desc-text {{ color: #8b949e; font-size: 0.75rem; margin-top: 0.15rem; display: -webkit-box; -webkit-line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden; }}
  .td-owner {{ white-space: nowrap; color: #c9d1d9; }}
  .td-ts {{ white-space: nowrap; color: #8b949e; font-size: 0.75rem; }}

  /* Status pills */
  .pill {{ display: inline-block; padding: 0.15rem 0.55rem; border-radius: 10px; font-size: 0.68rem; font-weight: 600; text-transform: uppercase; white-space: nowrap; }}
  .pill-pending    {{ background: #1a2a3a; color: #58a6ff; }}
  .pill-in_progress{{ background: #1a2a1a; color: #3fb950; animation: pulse 2s infinite; }}
  .pill-completed  {{ background: #1a2a1a; color: #56d364; opacity: 0.7; }}
  .pill-blocked    {{ background: #3a1a1a; color: #f85149; }}
  .pill-deferred   {{ background: #2e2200; color: #d29922; }}
  .pill-idea       {{ background: #2a1a3a; color: #bc8cff; }}
  @keyframes pulse {{ 0%,100% {{ opacity:1 }} 50% {{ opacity:0.65 }} }}

  /* Priority badge */
  .pri {{ display: inline-block; font-size: 0.63rem; padding: 0.1rem 0.4rem; border-radius: 8px; margin-left: 0.4rem; font-weight: 600; text-transform: uppercase; }}
  .pri-high   {{ background: #3a1a00; color: #f0883e; }}
  .pri-normal {{ background: #1a1a2e; color: #8b949e; }}
  .pri-idea   {{ background: #2a1a3a; color: #bc8cff; }}

  /* Owner badges */
  .owner-rocky      {{ color: #58a6ff; }}
  .owner-bullwinkle {{ color: #3fb950; }}
  .owner-natasha    {{ color: #e06eb6; }}
  .owner-all        {{ color: #d29922; }}
  .owner-jkh        {{ color: #f0883e; }}

  /* SquirrelBus */
  h2.bus-heading {{ margin-top: 2.5rem; }}
  .bus-filters {{ display: flex; gap: 6px; margin-bottom: 10px; flex-wrap: wrap; margin-top: 0.75rem; }}
  .bus-filter-btn {{ background: #21262d; color: #c9d1d9; border: 1px solid #30363d; padding: 5px 12px; border-radius: 16px; cursor: pointer; font-size: 12px; }}
  .bus-filter-btn.active {{ background: #1f6feb !important; color: #fff !important; border-color: #1f6feb !important; }}
  .bus-msg {{ background: #161b22; border: 1px solid #30363d; border-radius: 8px; padding: 10px 14px; margin-bottom: 6px; }}
  .bus-msg.compact {{ background: transparent; border: none; padding: 4px 14px; margin-bottom: 2px; color: #484f58; font-size: 12px; }}
  .bus-header {{ display: flex; justify-content: space-between; align-items: center; margin-bottom: 4px; font-size: 13px; }}
  .type-badge {{ display: inline-block; padding: 1px 6px; border-radius: 3px; font-size: 10px; color: #fff; margin-left: 4px; }}
  .send-form {{ background: #161b22; border: 1px solid #30363d; border-radius: 8px; padding: 12px; margin-bottom: 12px; margin-top: 0.75rem; }}
  .send-form summary {{ cursor: pointer; font-weight: 600; color: #58a6ff; font-size: 13px; }}
  .send-form input, .send-form textarea, .send-form select {{ background: #0d1117; color: #c9d1d9; border: 1px solid #30363d; padding: 5px 8px; border-radius: 4px; font-size: 12px; }}
  .send-form textarea {{ width: 100%; resize: vertical; min-height: 50px; }}
  .send-btn {{ background: #238636; color: #fff; border: none; padding: 5px 14px; border-radius: 4px; cursor: pointer; font-weight: 600; font-size: 12px; }}
  .send-btn:hover {{ background: #2ea043; }}
  #bus-status {{ font-size: 0.72rem; color: #6e7681; margin-top: 0.25rem; }}

  .wq-filters {{ display: flex; gap: 0.5rem; flex-wrap: wrap; margin-bottom: 0.75rem; margin-top: 0.75rem; }}
  .filter-btn {{ background: #161b22; border: 1px solid #30363d; border-radius: 6px; padding: 0.25rem 0.75rem; font-size: 0.75rem; color: #8b949e; cursor: pointer; transition: all 0.15s; }}
  .filter-btn:hover, .filter-btn.active {{ background: #1f2937; border-color: #58a6ff; color: #f0f6fc; }}

  /* jkh action rows */
  tr.jkh-row {{ background: #1a1500; }}
  tr.jkh-row:hover {{ background: #242000; }}
  tr.jkh-row td:first-child {{ border-left: 3px solid #f0883e; }}
  .jkh-tag {{ display: inline-block; font-size: 0.65rem; font-weight: 700; color: #f0883e; background: #2e1800; border: 1px solid #f0883e44; border-radius: 6px; padding: 0.1rem 0.4rem; margin-left: 0.4rem; vertical-align: middle; }}

  /* Complete button */
  .complete-btn {{
    display: inline-flex; align-items: center; gap: 0.3rem;
    background: #1a2e1a; border: 1px solid #3fb95066; color: #3fb950;
    border-radius: 6px; padding: 0.3rem 0.7rem; font-size: 0.72rem; font-weight: 600;
    cursor: pointer; transition: all 0.15s; white-space: nowrap;
  }}
  .complete-btn:hover {{ background: #1f3d1f; border-color: #3fb950; }}
  .complete-btn:disabled {{ opacity: 0.4; cursor: default; }}
  .complete-btn.done {{ background: #0f1f0f; color: #56d364; border-color: #56d36444; }}

  /* Upvote button (idea → task) */
  .upvote-btn {{
    display: inline-flex; align-items: center; gap: 0.3rem;
    background: #1a1a2e; border: 1px solid #bc8cff66; color: #bc8cff;
    border-radius: 6px; padding: 0.3rem 0.7rem; font-size: 0.72rem; font-weight: 600;
    cursor: pointer; transition: all 0.15s; white-space: nowrap;
  }}
  .upvote-btn:hover {{ background: #25183d; border-color: #bc8cff; }}
  .upvote-btn:disabled {{ opacity: 0.4; cursor: default; }}
  .upvote-btn.done {{ background: #1a1a2e; color: #58a6ff; border-color: #58a6ff44; }}

  /* Comment panel (blocked items) */
  .comment-panel {{
    margin-top: 0.4rem; display: flex; gap: 0.4rem; align-items: flex-start; flex-wrap: wrap;
  }}
  .comment-input {{
    flex: 1; min-width: 160px; max-width: 320px;
    background: #0d1117; border: 1px solid #30363d; border-radius: 6px;
    padding: 0.3rem 0.6rem; font-size: 0.72rem; color: #c9d1d9;
    font-family: inherit; resize: none; height: 2.5rem;
  }}
  .comment-input:focus {{ outline: none; border-color: #58a6ff; }}
  .comment-submit-btn {{
    background: #1a2030; border: 1px solid #58a6ff66; color: #58a6ff;
    border-radius: 6px; padding: 0.3rem 0.7rem; font-size: 0.72rem; font-weight: 600;
    cursor: pointer; transition: all 0.15s; white-space: nowrap; align-self: flex-end;
  }}
  .comment-submit-btn:hover {{ background: #1f2d45; border-color: #58a6ff; }}
  .comment-submit-btn:disabled {{ opacity: 0.4; cursor: default; }}
  .comment-hint {{ font-size: 0.65rem; color: #6e7681; width: 100%; margin-top: 0.2rem; }}

  .toast {{
    position: fixed; bottom: 1.5rem; right: 1.5rem;
    background: #161b22; border: 1px solid #30363d; border-radius: 8px;
    padding: 0.75rem 1.25rem; font-size: 0.85rem; color: #f0f6fc;
    box-shadow: 0 4px 16px #00000088; z-index: 999;
    transform: translateY(8px); opacity: 0; transition: all 0.25s;
    pointer-events: none;
  }}
  .toast.show {{ transform: translateY(0); opacity: 1; }}
  .toast.success {{ border-left: 3px solid #3fb950; }}
  .toast.error   {{ border-left: 3px solid #f85149; }}

  .footer {{ font-size: 0.72rem; color: #6e7681; border-top: 1px solid #21262d; padding-top: 1rem; margin-top: 2rem; }}
</style>
</head>
<body>
<h1>🤖 Agent Status Dashboard</h1>
<p class="subtitle" id="gen-time">Loading...</p>
<div class="grid" id="agent-grid"></div>

<h2>📋 Work Queue</h2>
<div class="wq-filters" id="wq-filters"></div>
<div class="wq-wrap">
  <table id="wq-table">
    <thead>
      <tr>
        <th>Status</th>
        <th>Description</th>
        <th>Ownership</th>
        <th>Last Update (UTC)</th>
        <th></th>
      </tr>
    </thead>
    <tbody id="wq-body"></tbody>
  </table>
</div>
<div class="toast" id="toast"></div>

<h2 class="bus-heading">📡 SquirrelBus</h2>
<details class="send-form">
  <summary>✉️ Send a message</summary>
  <div style="display:grid;grid-template-columns:1fr 1fr 1fr;gap:6px;margin-top:8px">
    <div><label style="font-size:11px;color:#8b949e">From</label><select id="msg-from" style="width:100%"><option value="rocky">Rocky</option><option value="jkh">jkh</option></select></div>
    <div><label style="font-size:11px;color:#8b949e">To</label><select id="msg-to" style="width:100%"><option value="all">All</option><option value="rocky">Rocky</option><option value="bullwinkle">Bullwinkle</option><option value="natasha">Natasha</option><option value="jkh">jkh</option></select></div>
    <div><label style="font-size:11px;color:#8b949e">Type</label><select id="msg-type" style="width:100%"><option value="text">text</option><option value="memo">memo</option></select></div>
  </div>
  <div style="margin-top:6px"><label style="font-size:11px;color:#8b949e">Subject</label><input id="msg-subject" style="width:100%" placeholder="Optional subject..."></div>
  <div style="margin-top:6px"><label style="font-size:11px;color:#8b949e">Body</label><textarea id="msg-body" placeholder="Type your message..."></textarea></div>
  <div style="margin-top:6px;text-align:right"><button class="send-btn" onclick="sendBusMessage()">Send</button></div>
</details>
<div class="bus-filters" id="bus-filters"></div>
<div id="bus-status">Loading messages...</div>
<div id="bus-messages"></div>

<div class="footer">Data baked at generation time by Rocky &middot; Heartbeats every 30 min &middot; Auto-refreshes every 2 min</div>

<script>
const AGENTS = [
  {{ id: 'natasha',    label: 'Natasha',    emoji: '🕵️\\u200d♀️', host: process.env.NATASHA_HOST || 'natasha-host' }},
  {{ id: 'rocky',      label: 'Rocky',      emoji: '🐿️',           host: '$MINIO_ALIAS (DigitalOcean)' }},
  {{ id: 'bullwinkle', label: 'Bullwinkle', emoji: '🫎',            host: 'puck (Mac)' }}
];
const DATA = {data_json};
const QUEUE = {queue_items_json};
const STALE_MS = 35 * 60 * 1000;

const OWNER_EMOJI = {{ rocky: '🐿️', bullwinkle: '🫎', natasha: '🕵️', all: '👥', jkh: '👤' }};

function timeAgo(ts) {{
  if (!ts) return 'never';
  const diff = Date.now() - new Date(ts).getTime();
  if (diff < 60000) return Math.floor(diff/1000) + 's ago';
  if (diff < 3600000) return Math.floor(diff/60000) + 'm ago';
  return Math.floor(diff/3600000) + 'h ' + Math.floor((diff%3600000)/60000) + 'm ago';
}}

function fmtUtc(ts) {{
  if (!ts) return '—';
  const d = new Date(ts);
  return d.toISOString().replace('T',' ').slice(0,16) + ' UTC';
}}

function cardClass(d) {{
  if (!d) return 'offline';
  const age = Date.now() - new Date(d.ts).getTime();
  if (age > STALE_MS) return 'stale';
  return d.status === 'online' ? 'online' : 'offline';
}}

// ── Agent cards ──────────────────────────────────────────────────────────────
const grid = document.getElementById('agent-grid');
AGENTS.forEach(a => {{
  const d = DATA[a.id];
  const cls = cardClass(d);
  const fields = [];
  if (d) {{
    fields.push(['Last seen', timeAgo(d.ts)]);
    if (d.load1m) fields.push(['Load', d.load1m]);
    if (d.uptime) fields.push(['Uptime', d.uptime]);
    if (d.queue_depth !== undefined) fields.push(['Queue depth', d.queue_depth]);
    if (d.queueDepth !== undefined) fields.push(['Queue depth', d.queueDepth]);
    if (d.services) fields.push(['Services', Object.entries(d.services).map(([k,v])=>k+':'+v).join(', ')]);
  }}
  grid.innerHTML += `<div class="card ${{cls}}">
    <div class="agent-name"><span class="dot ${{cls}}"></span>${{a.emoji}} ${{a.label}}<span class="badge badge-${{cls}}">${{cls.toUpperCase()}}</span></div>
    <div class="host">${{a.host}}</div>
    ${{fields.map(([l,v])=>`<div class="field"><span class="label">${{l}}: </span><span class="value">${{v}}</span></div>`).join('')}}
  </div>`;
}});

// ── Workqueue table ──────────────────────────────────────────────────────────
const WQ_API = 'http://<your-minio-host>:8787';
const WQ_TOKEN = 'wq-rocky-8787';
const FILTERS = ['all', 'jkh', 'pending', 'in_progress', 'blocked', 'deferred', 'completed', 'idea'];
let activeFilter = 'all';
const completedIds = new Set(); // track locally-completed items this session

function showToast(msg, type = 'success') {{
  const t = document.getElementById('toast');
  t.textContent = msg;
  t.className = 'toast ' + type + ' show';
  setTimeout(() => t.classList.remove('show'), 3500);
}}

async function completeItem(id, btn) {{
  btn.disabled = true;
  btn.textContent = '⏳ Updating…';
  try {{
    const r = await fetch(`${{WQ_API}}/complete/${{encodeURIComponent(id)}}`, {{
      method: 'POST',
      headers: {{ Authorization: 'Bearer ' + WQ_TOKEN, 'Content-Type': 'application/json' }},
      body: JSON.stringify({{ completedBy: 'jkh', ts: new Date().toISOString() }})
    }});
    const j = await r.json();
    if (j.ok) {{
      completedIds.add(id);
      btn.textContent = '✅ Done';
      btn.classList.add('done');
      showToast(j.message || '✅ Marked complete — dashboard updating…', 'success');
      renderTable();
    }} else {{
      throw new Error(j.error || 'Unknown error');
    }}
  }} catch (e) {{
    btn.disabled = false;
    btn.textContent = '✓ Done';
    showToast('⚠️ API unreachable — are you on Tailscale? ' + e.message, 'error');
  }}
}}

async function upvoteIdea(id, btn) {{
  btn.disabled = true;
  btn.textContent = '⏳…';
  try {{
    const r = await fetch(`${{WQ_API}}/upvote/${{encodeURIComponent(id)}}`, {{
      method: 'POST',
      headers: {{ Authorization: 'Bearer ' + WQ_TOKEN, 'Content-Type': 'application/json' }},
      body: JSON.stringify({{ votedBy: 'jkh', ts: new Date().toISOString() }})
    }});
    const j = await r.json();
    if (j.ok) {{
      btn.textContent = '✅ Queued!';
      btn.classList.add('done');
      showToast(j.message || '✅ Idea promoted to task!', 'success');
      renderTable();
    }} else {{
      throw new Error(j.error || 'Unknown error');
    }}
  }} catch (e) {{
    btn.disabled = false;
    btn.textContent = '⬆️ Make it happen';
    showToast('⚠️ API unreachable — are you on Tailscale? ' + e.message, 'error');
  }}
}}

async function submitComment(id, inputEl, btn) {{
  const comment = inputEl.value.trim();
  if (!comment) {{ inputEl.focus(); return; }}
  btn.disabled = true;
  inputEl.disabled = true;
  btn.textContent = '⏳…';
  try {{
    const r = await fetch(`${{WQ_API}}/comment/${{encodeURIComponent(id)}}`, {{
      method: 'POST',
      headers: {{ Authorization: 'Bearer ' + WQ_TOKEN, 'Content-Type': 'application/json' }},
      body: JSON.stringify({{ comment, commentBy: 'jkh', ts: new Date().toISOString() }})
    }});
    const j = await r.json();
    if (j.ok) {{
      btn.textContent = '✅ Sent';
      showToast(`✅ ${{j.message || j.actionTaken}} — Rocky will pick it up next cycle`, 'success');
      renderTable();
    }} else {{
      throw new Error(j.error || 'Unknown error');
    }}
  }} catch (e) {{
    btn.disabled = false;
    inputEl.disabled = false;
    btn.textContent = 'Send';
    showToast('⚠️ API unreachable — are you on Tailscale? ' + e.message, 'error');
  }}
}}

const filterBar = document.getElementById('wq-filters');
FILTERS.forEach(f => {{
  const btn = document.createElement('button');
  btn.className = 'filter-btn' + (f === 'all' ? ' active' : '');
  const count = f === 'all' ? QUEUE.length
    : f === 'jkh' ? QUEUE.filter(i => i.assignee === 'jkh' && i.status !== 'completed').length
    : QUEUE.filter(i => i.status === f || i.priority === f).length;
  btn.textContent = (f === 'jkh' ? '👤 jkh' : f) + (count ? ` (${{count}})` : '');
  btn.dataset.filter = f;
  btn.onclick = () => {{
    activeFilter = f;
    document.querySelectorAll('.filter-btn').forEach(b => b.classList.toggle('active', b.dataset.filter === f));
    renderTable();
  }};
  filterBar.appendChild(btn);
}});

function renderTable() {{
  const tbody = document.getElementById('wq-body');
  const items = QUEUE.filter(i => {{
    if (completedIds.has(i.id)) i = {{...i, status: 'completed'}}; // local optimistic
    if (activeFilter === 'all') return true;
    if (activeFilter === 'jkh') return i.assignee === 'jkh' && !completedIds.has(i.id) && i.status !== 'completed';
    if (activeFilter === 'idea') return i.priority === 'idea' || i.tags?.includes('idea');
    return i.status === activeFilter;
  }});

  if (!items.length) {{
    tbody.innerHTML = '<tr><td colspan="5" style="text-align:center;color:#6e7681;padding:1.5rem">No items</td></tr>';
    return;
  }}

  tbody.innerHTML = items.map(item => {{
    const isLocalDone = completedIds.has(item.id);
    const st = isLocalDone ? 'completed' : (item.status || 'pending');
    const isIdea = (item.priority === 'idea' || item.tags?.includes('idea')) && st !== 'completed';
    const isPill = isIdea ? 'idea' : st;
    const priCls = item.priority === 'high' ? 'pri-high' : item.priority === 'idea' ? 'pri-idea' : 'pri-normal';
    const isJkh = item.assignee === 'jkh' && st !== 'completed';
    const isBlocked = st === 'blocked' && !isLocalDone;

    const lastTs = item.completedAt || item.claimedAt || item.lastAttempt || item.created;
    const ownerKey = item.claimedBy || item.assignee || '—';
    const ownerEmoji = OWNER_EMOJI[ownerKey] || '';
    const ownerCls = 'owner-' + (ownerKey || 'unknown');

    const jkhTag = isJkh ? '<span class="jkh-tag">👤 YOUR TURN</span>' : '';

    // Action cell logic:
    // 1. jkh-assigned non-completed → Complete button
    // 2. idea (not completed) → Upvote/promote button
    // 3. blocked → inline comment panel
    // 4. otherwise → empty
    let actionCell;
    const safeId = item.id.replace(/'/g, "\\'");
    if (isJkh && !isLocalDone) {{
      actionCell = `<td><button class="complete-btn" onclick="completeItem('${{safeId}}', this)">✓ Done</button></td>`;
    }} else if (isIdea) {{
      actionCell = `<td><button class="upvote-btn" onclick="upvoteIdea('${{safeId}}', this)" title="Promote this idea to a real task">⬆️ Make it happen</button></td>`;
    }} else if (isBlocked) {{
      const uid = 'c_' + item.id.replace(/[^a-z0-9]/gi,'_');
      actionCell = `<td>
        <div class="comment-panel">
          <textarea class="comment-input" id="inp_${{uid}}" placeholder="Tell agents what to do with this blocked item…"></textarea>
          <button class="comment-submit-btn" onclick="submitComment('${{safeId}}', document.getElementById('inp_${{uid}}'), this)">Send</button>
          <div class="comment-hint">Say: "unblock — do X", "break into: step A, step B", or "delete this"</div>
        </div>
      </td>`;
    }} else {{
      actionCell = `<td></td>`;
    }}

    return `<tr class="${{isJkh ? 'jkh-row' : ''}}">
      <td><span class="pill pill-${{isPill}}">${{isPill}}</span>${{item.priority && item.priority !== 'idea' ? `<span class="pri ${{priCls}}">${{item.priority}}</span>` : ''}}</td>
      <td class="td-desc">
        <div class="title">${{item.title || item.id}}${{jkhTag}}</div>
        ${{item.description ? `<div class="desc-text">${{item.description}}</div>` : ''}}
        ${{item.notes && isBlocked ? `<div class="desc-text" style="color:#f85149;margin-top:0.25rem">🚫 ${{item.notes.split('\\n').pop()}}</div>` : ''}}
      </td>
      <td class="td-owner"><span class="${{ownerCls}}">${{ownerEmoji}} ${{ownerKey}}</span><br><span style="color:#6e7681;font-size:0.72rem">src: ${{item.source || '?'}}</span></td>
      <td class="td-ts"><span title="${{lastTs}}">${{fmtUtc(lastTs)}}</span><br><span style="color:#6e7681">${{timeAgo(lastTs)}}</span></td>
      ${{actionCell}}
    </tr>`;
  }}).join('');
}}

renderTable();

document.getElementById('gen-time').textContent = 'Generated ' + new Date().toLocaleString();

// ── SquirrelBus ───────────────────────────────────────────────────────────────
const BUS_API    = process.env.RCC_HOST_PUBLIC ? `http://${process.env.RCC_HOST_PUBLIC}` : 'http://localhost:8788';
const BUS_EMOJIS = {{ rocky: '🐿️', bullwinkle: '🫎', natasha: '🕵️', jkh: '👤' }};
const TYPE_COLORS = {{ text: '#58a6ff', memo: '#3fb950', blob: '#a371f7', heartbeat: '#8b949e', queue_sync: '#d29922', ping: '#3fb950', pong: '#3fb950', event: '#f85149', handoff: '#f0883e' }};

let busMessages = [];
let busFilter   = 'all';

function busEsc(s) {{
  if (!s) return '';
  const d = document.createElement('div');
  d.textContent = s;
  return d.innerHTML;
}}

async function loadBus() {{
  const statusEl = document.getElementById('bus-status');
  try {{
    const msgs = await fetch(BUS_API + '/bus/messages?limit=50').then(r => {{
      if (!r.ok) throw new Error('HTTP ' + r.status);
      return r.json();
    }});
    busMessages = Array.isArray(msgs) ? msgs : [];
    statusEl.textContent = busMessages.length + ' messages · last fetched ' + new Date().toLocaleTimeString();
    renderBusFilters();
    renderBus();
  }} catch (e) {{
    statusEl.textContent = '⚠️ Bus unreachable (' + e.message + ') — is $MINIO_ALIAS reachable from your network?';
    document.getElementById('bus-messages').innerHTML = '';
  }}
}}

function renderBusFilters() {{
  const agents = ['all', 'rocky', 'bullwinkle', 'natasha', 'jkh'];
  document.getElementById('bus-filters').innerHTML = agents.map(agent => {{
    const emoji  = agent === 'all' ? '📡' : (BUS_EMOJIS[agent] || '📨');
    const active = busFilter === agent ? ' active' : '';
    return '<button class="bus-filter-btn' + active + '" onclick="setBusFilter(\'' + agent + '\')">' +
           emoji + ' ' + agent.charAt(0).toUpperCase() + agent.slice(1) + '</button>';
  }}).join('');
}}

function setBusFilter(f) {{
  busFilter = f;
  renderBusFilters();
  renderBus();
}}

function renderBus() {{
  const filtered = busFilter === 'all'
    ? busMessages
    : busMessages.filter(m => m.from === busFilter || m.to === busFilter);
  const el = document.getElementById('bus-messages');
  if (!filtered.length) {{
    el.innerHTML = '<div style="color:#8b949e;padding:12px">No messages</div>';
    return;
  }}
  el.innerHTML = filtered.map(renderBusMsg).join('');
}}

function renderBusMsg(msg) {{
  const fromEmoji = BUS_EMOJIS[msg.from] || '📨';
  const toLabel   = msg.to === 'all' ? 'all' : msg.to;
  const ts = new Date(msg.ts).toLocaleString('en-US', {{ timeZone: 'America/Los_Angeles', month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' }});
  const typeColor = TYPE_COLORS[msg.type] || '#8b949e';

  if (msg.type === 'heartbeat' || msg.type === 'ping' || msg.type === 'pong') {{
    const icon = msg.type === 'heartbeat' ? '💓' : '🏓';
    return '<div class="bus-msg compact" data-from="' + busEsc(msg.from) + '">' +
           fromEmoji + ' ' + busEsc(msg.from) + ' ' + icon + ' ' + msg.type +
           ' · #' + msg.seq + ' · ' + busEsc(ts) + '</div>';
  }}

  let subject  = msg.subject ? '<div style="font-weight:600;color:#58a6ff;margin-bottom:3px;font-size:13px">' + busEsc(msg.subject) + '</div>' : '';
  let bodyHtml = '';

  switch (msg.type) {{
    case 'text':
    case 'memo':
      bodyHtml = '<div style="white-space:pre-wrap;font-size:13px">' + busEsc(msg.body) + '</div>';
      break;
    case 'blob':
      if (msg.mime && msg.mime.startsWith('image/')) {{
        const src = msg.enc === 'base64' ? 'data:' + msg.mime + ';base64,' + msg.body : busEsc(msg.body);
        bodyHtml = '<img src="' + src + '" style="max-width:360px;border-radius:6px;margin-top:4px">';
      }} else if (msg.mime && msg.mime.startsWith('audio/')) {{
        const src = msg.enc === 'base64' ? 'data:' + msg.mime + ';base64,' + msg.body : busEsc(msg.body);
        bodyHtml = '<audio controls src="' + src + '" style="margin-top:4px"></audio>';
      }} else {{
        bodyHtml = '<pre style="background:#0d1117;padding:6px;border-radius:4px;overflow-x:auto;font-size:11px">' + busEsc((msg.body || '').slice(0, 500)) + '</pre>';
      }}
      break;
    case 'queue_sync':
      bodyHtml = '<details style="margin-top:4px"><summary style="cursor:pointer;color:#58a6ff;font-size:12px">Queue sync data</summary>' +
                 '<pre style="background:#0d1117;padding:6px;border-radius:4px;overflow-x:auto;font-size:11px;margin-top:4px">' +
                 busEsc(typeof msg.body === 'string' ? msg.body : JSON.stringify(msg.body, null, 2)) + '</pre></details>';
      break;
    default:
      bodyHtml = '<pre style="background:#0d1117;padding:6px;border-radius:4px;overflow-x:auto;font-size:11px">' + busEsc(JSON.stringify(msg, null, 2)) + '</pre>';
  }}

  return '<div class="bus-msg" data-from="' + busEsc(msg.from) + '">' +
    '<div class="bus-header">' +
      '<div>' + fromEmoji + ' <strong style="color:#f0f6fc">' + busEsc(msg.from) + '</strong>' +
      ' <span style="color:#484f58">→</span> <strong>' + busEsc(toLabel) + '</strong>' +
      ' <span class="type-badge" style="background:' + typeColor + '">' + busEsc(msg.type) + '</span></div>' +
      '<div style="color:#484f58;font-size:11px">#' + msg.seq + ' · ' + busEsc(ts) + '</div>' +
    '</div>' +
    subject + bodyHtml +
    '</div>';
}}

async function sendBusMessage() {{
  const from    = document.getElementById('msg-from').value;
  const to      = document.getElementById('msg-to').value;
  const type    = document.getElementById('msg-type').value;
  const subject = document.getElementById('msg-subject').value.trim();
  const body    = document.getElementById('msg-body').value.trim();
  if (!body) {{ showToast('Body is required', 'error'); return; }}
  try {{
    const r = await fetch(BUS_API + '/bus/send', {{
      method: 'POST',
      headers: {{ 'Content-Type': 'application/json' }},
      body: JSON.stringify({{ from, to, type, subject: subject || undefined, body }})
    }});
    if (!r.ok) throw new Error('HTTP ' + r.status);
    document.getElementById('msg-body').value = '';
    document.getElementById('msg-subject').value = '';
    showToast('Message sent!', 'success');
    setTimeout(loadBus, 800);
  }} catch (e) {{
    showToast('⚠️ Send failed: ' + e.message, 'error');
  }}
}}

// Initial load + poll every 30s
loadBus();
setInterval(loadBus, 30000);
</script>
</body>
</html>""")
