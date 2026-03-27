#!/usr/bin/env node
/**
 * Workqueue API server
 * Port 8787, Tailscale-only
 *
 * POST /complete/:id   — mark item complete, unblock dependents, republish dashboard
 * GET  /status         — health check
 * GET  /queue          — return current queue JSON (for dashboard CORS fetch)
 */

import http from 'http';
import fs from 'fs';
import path from 'path';
import { execSync } from 'child_process';
import { fileURLToPath } from 'url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const WORKSPACE = path.resolve(__dirname, '..');
const QUEUE_FILE = path.join(WORKSPACE, 'workqueue', 'queue.json');
const MC = process.env.MC_PATH || '/usr/local/bin/mc';
const MINIO_ALIAS = process.env.MINIO_ALIAS || 'local';
const AGENT_NAME  = process.env.AGENT_NAME  || 'agent';
const GEN_SCRIPT = path.join(WORKSPACE, 'workqueue', 'scripts', 'gen-dashboard.py');
const AZURE_SAS = 'https://loomdd566f62.blob.core.windows.net/assets/agent-dashboard.html?se=2029-03-19T02%3A25Z&sp=rwdlcu&spr=https&sv=2026-02-06&ss=b&srt=sco&sig=Dn4faVsJCz0ufWyHmiKCFCrgiLQkSIRtp7MLmqXKiUA%3D';

const PORT = 8787;
const TOKEN = process.env.WQ_API_TOKEN || 'wq-api-token'; // set WQ_API_TOKEN in .env

// ── Slack DM helper ───────────────────────────────────────────────────────────
const SLACK_TOKEN = process.env.SLACK_TOKEN || ''; // set via environment, not hardcoded
const JKH_SLACK_ID = 'UDYR7H4SC';

async function slackDm(text) {
  try {
    const { default: https } = await import('https');
    const body = JSON.stringify({ channel: JKH_SLACK_ID, text });
    await new Promise((resolve, reject) => {
      const req = https.request({
        hostname: 'slack.com',
        path: '/api/chat.postMessage',
        method: 'POST',
        headers: {
          'Authorization': `Bearer ${SLACK_TOKEN}`,
          'Content-Type': 'application/json',
          'Content-Length': Buffer.byteLength(body)
        }
      }, res => {
        let d = '';
        res.on('data', c => d += c);
        res.on('end', () => {
          const j = JSON.parse(d);
          if (!j.ok) console.error('[slack-dm]', j.error);
          resolve(j);
        });
      });
      req.on('error', reject);
      req.write(body);
      req.end();
    });
  } catch (e) {
    console.error('[slack-dm error]', e.message);
  }
}

// ── Queue helpers ─────────────────────────────────────────────────────────────
function loadQueue() {
  return JSON.parse(fs.readFileSync(QUEUE_FILE, 'utf8'));
}

function saveQueue(q) {
  fs.writeFileSync(QUEUE_FILE, JSON.stringify(q, null, 2));
  // Also push to MinIO
  try {
    execSync(`${MC} cp ${QUEUE_FILE} /agents//queue.json`, { timeout: 10000, stdio: 'pipe' });
  } catch (e) {
    console.error('[minio sync error]', e.message);
  }
}

function republishDashboard() {
  try {
    const rocky    = execSync(`${MC} cat ${MINIO_ALIAS}/agents/shared/agent-heartbeat-rocky.json 2>/dev/null || echo null`, { timeout: 8000 }).toString().trim();
    const natasha  = execSync(`${MC} cat ${MINIO_ALIAS}/agents/shared/agent-heartbeat-natasha.json 2>/dev/null || echo null`, { timeout: 8000 }).toString().trim();
    const bull     = execSync(`${MC} cat ${MINIO_ALIAS}/agents/shared/agent-heartbeat-bullwinkle.json 2>/dev/null || echo null`, { timeout: 8000 }).toString().trim();
    const queueJson = fs.readFileSync(QUEUE_FILE, 'utf8');
    const html = execSync(
      `python3 ${GEN_SCRIPT} '${rocky.replace(/'/g,"'\\''")}'  '${natasha.replace(/'/g,"'\\''")}'  '${bull.replace(/'/g,"'\\''")}'  '${queueJson.replace(/'/g,"'\\''")}'`,
      { timeout: 15000, maxBuffer: 4 * 1024 * 1024 }
    );
    fs.writeFileSync('/tmp/agent-dashboard.html', html);
    execSync(
      `curl -s -o /dev/null -X PUT -H "x-ms-blob-type: BlockBlob" -H "Content-Type: text/html" --data-binary @/tmp/agent-dashboard.html "${AZURE_SAS}"`,
      { timeout: 20000, stdio: 'pipe' }
    );
    console.log('[dashboard] republished');
  } catch (e) {
    console.error('[dashboard republish error]', e.message);
  }
}

// Find items that were blocked because of a given completed item id.
// Heuristic: item notes or description mention the completed item's id,
// OR item.assignee === 'jkh' and it was deferred pending a jkh action on another item.
function findUnblockable(queue, completedId, completedTitle) {
  const candidates = [];
  for (const item of queue.items) {
    if (item.status !== 'blocked' && item.status !== 'deferred') continue;
    const haystack = `${item.notes || ''} ${item.description || ''} ${item.title || ''}`.toLowerCase();
    if (haystack.includes(completedId.toLowerCase()) ||
        (completedTitle && haystack.includes(completedTitle.toLowerCase().slice(0, 20)))) {
      candidates.push(item);
    }
  }
  return candidates;
}

// ── Parse intent from jkh's free-text comment on a blocked item ──────────────
// Returns { action: 'unblock'|'subtask'|'delete'|'note', details }
function parseCommentIntent(text) {
  const lower = text.toLowerCase().trim();
  if (/\bdelete\b|\bremove\b|\bkill\b|\bdrop\b/.test(lower)) {
    return { action: 'delete', details: text };
  }
  if (/\bsubtask\b|\bbreak\b|\bsplit\b|\bdivide\b|\bstep\b|\bpart\b/.test(lower)) {
    return { action: 'subtask', details: text };
  }
  if (/\bunblock\b|\bready\b|\bdone\b|\bfixed\b|\bresolved\b|\bproceed\b/.test(lower)) {
    return { action: 'unblock', details: text };
  }
  // Default: treat as clarifying note, unblock the item
  return { action: 'unblock', details: text };
}

// ── HTTP server ───────────────────────────────────────────────────────────────
const server = http.createServer((req, res) => {
  // CORS — allow the Azure Blob dashboard origin
  res.setHeader('Access-Control-Allow-Origin', '*');
  res.setHeader('Access-Control-Allow-Methods', 'GET, POST, OPTIONS');
  res.setHeader('Access-Control-Allow-Headers', 'Content-Type, Authorization');

  if (req.method === 'OPTIONS') { res.writeHead(204); res.end(); return; }

  const url = new URL(req.url, `http://localhost:${PORT}`);

  // GET /status
  if (req.method === 'GET' && url.pathname === '/status') {
    res.writeHead(200, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify({ ok: true, service: 'wq-api', host: process.env.AGENT_HOST || 'localhost', port: PORT }));
    return;
  }

  // GET /queue
  if (req.method === 'GET' && url.pathname === '/queue') {
    try {
      const q = loadQueue();
      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify(q));
    } catch (e) {
      res.writeHead(500); res.end(JSON.stringify({ error: e.message }));
    }
    return;
  }

  // POST /upvote/:id  — jkh upvotes an idea → promotes to pending task
  const upvoteMatch = url.pathname.match(/^\/upvote\/(.+)$/);
  if (req.method === 'POST' && upvoteMatch) {
    const auth = req.headers['authorization'] || '';
    if (!auth.includes(TOKEN)) {
      res.writeHead(401); res.end(JSON.stringify({ error: 'unauthorized' }));
      return;
    }
    const itemId = decodeURIComponent(upvoteMatch[1]);
    let body = '';
    req.on('data', d => body += d);
    req.on('end', () => {
      try {
        const queue = loadQueue();
        const now = new Date().toISOString();
        const item = queue.items.find(i => i.id === itemId);
        if (!item) {
          res.writeHead(404); res.end(JSON.stringify({ error: 'item not found', id: itemId }));
          return;
        }

        // Add jkh to votes array
        if (!item.votes) item.votes = [];
        if (!item.votes.includes('jkh')) item.votes.push('jkh');

        let promoted = false;
        // jkh upvote immediately promotes to pending task
        item.status = 'pending';
        item.priority = item.priority === 'idea' ? 'normal' : item.priority;
        item.itemVersion = (item.itemVersion || 1) + 1;
        item.notes = (item.notes || '') + `\nPromoted to task by jkh at ${now} (upvote from dashboard).`;
        // Remove idea tag
        if (item.tags) item.tags = item.tags.filter(t => t !== 'idea');
        promoted = true;

        saveQueue(queue);
        setImmediate(async () => {
          republishDashboard();
          await slackDm(
            `⬆️ *jkh upvoted idea → now a task:* \`${itemId}\` — _${item.title}_\n` +
            `Status: pending | Priority: ${item.priority}. Next agent cron will pick it up.`
          );
        });

        const result = { ok: true, promoted, itemId, message: `✅ Promoted to task!` };
        res.writeHead(200, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify(result));
        console.log(`[upvote] ${itemId} promoted to task by jkh`);
      } catch (e) {
        console.error('[upvote error]', e);
        res.writeHead(500); res.end(JSON.stringify({ error: e.message }));
      }
    });
    return;
  }

  // POST /comment/:id  — jkh comments on a blocked item (unblock/subtask/delete)
  const commentMatch = url.pathname.match(/^\/comment\/(.+)$/);
  if (req.method === 'POST' && commentMatch) {
    const auth = req.headers['authorization'] || '';
    if (!auth.includes(TOKEN)) {
      res.writeHead(401); res.end(JSON.stringify({ error: 'unauthorized' }));
      return;
    }
    const itemId = decodeURIComponent(commentMatch[1]);
    let body = '';
    req.on('data', d => body += d);
    req.on('end', () => {
      try {
        const payload = JSON.parse(body || '{}');
        const comment = (payload.comment || '').trim();
        if (!comment) {
          res.writeHead(400); res.end(JSON.stringify({ error: 'comment required' }));
          return;
        }

        const queue = loadQueue();
        const now = new Date().toISOString();
        const item = queue.items.find(i => i.id === itemId);
        if (!item) {
          res.writeHead(404); res.end(JSON.stringify({ error: 'item not found', id: itemId }));
          return;
        }

        const intent = parseCommentIntent(comment);
        let actionTaken = '';

        if (intent.action === 'delete') {
          // Move to completed with deleted note
          item.status = 'completed';
          item.completedAt = now;
          item.result = `Deleted by jkh via dashboard comment: "${comment}"`;
          item.itemVersion = (item.itemVersion || 1) + 1;
          const idx = queue.items.indexOf(item);
          queue.items.splice(idx, 1);
          if (!queue.completed) queue.completed = [];
          queue.completed.unshift(item);
          actionTaken = 'deleted';
        } else if (intent.action === 'subtask') {
          // Add comment as subtask guidance note, unblock
          item.notes = (item.notes || '') + `\njkh comment [${now}]: ${comment}`;
          item.status = 'pending';
          item.itemVersion = (item.itemVersion || 1) + 1;
          actionTaken = 'unblocked with subtask guidance';
        } else {
          // unblock or note — add comment and unblock
          item.notes = (item.notes || '') + `\njkh comment [${now}]: ${comment}`;
          item.status = 'pending';
          item.itemVersion = (item.itemVersion || 1) + 1;
          actionTaken = 'unblocked';
        }

        saveQueue(queue);
        setImmediate(async () => {
          republishDashboard();
          await slackDm(
            `💬 *jkh commented on blocked item \`${itemId}\`:* _${item.title}_\n` +
            `Action: ${actionTaken}\nComment: "${comment}"\nNext agent cron will process.`
          );
        });

        res.writeHead(200, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify({ ok: true, actionTaken, itemId, message: `✅ ${actionTaken.charAt(0).toUpperCase() + actionTaken.slice(1)}` }));
        console.log(`[comment] ${itemId} → ${actionTaken}`);
      } catch (e) {
        console.error('[comment error]', e);
        res.writeHead(500); res.end(JSON.stringify({ error: e.message }));
      }
    });
    return;
  }

  // POST /complete/:id
  const m = url.pathname.match(/^\/complete\/(.+)$/);
  if (req.method === 'POST' && m) {
    // Auth check
    const auth = req.headers['authorization'] || '';
    if (!auth.includes(TOKEN)) {
      res.writeHead(401); res.end(JSON.stringify({ error: 'unauthorized' }));
      return;
    }

    const itemId = decodeURIComponent(m[1]);
    let body = '';
    req.on('data', d => body += d);
    req.on('end', () => {
      try {
        const queue = loadQueue();
        const now = new Date().toISOString();

        // Find and complete the item
        let completed = null;
        const idx = queue.items.findIndex(i => i.id === itemId);
        if (idx === -1) {
          res.writeHead(404); res.end(JSON.stringify({ error: 'item not found', id: itemId }));
          return;
        }

        completed = queue.items[idx];
        completed.status = 'completed';
        completed.completedAt = now;
        completed.result = `Completed by jkh via dashboard at ${now}`;
        completed.itemVersion = (completed.itemVersion || 1) + 1;

        // Move to completed array
        queue.items.splice(idx, 1);
        if (!queue.completed) queue.completed = [];
        queue.completed.unshift(completed);

        // Unblock any dependents
        const unblocked = findUnblockable(queue, itemId, completed.title);
        const unblockedIds = [];
        for (const item of unblocked) {
          item.status = 'pending';
          item.itemVersion = (item.itemVersion || 1) + 1;
          item.notes = (item.notes || '') + `\nUnblocked by completion of ${itemId} at ${now}`;
          unblockedIds.push(item.id);
        }

        saveQueue(queue);

        // Republish dashboard + notify Rocky async
        setImmediate(async () => {
          republishDashboard();
          // Notify Rocky (self) via Slack so Rocky is aware jkh completed a task
          const unblockedNote = unblockedIds.length
            ? ` Unblocked: ${unblockedIds.map(id => `\`${id}\``).join(', ')}.` : '';
          await slackDm(
            `✅ *jkh marked complete via dashboard:* \`${itemId}\` — _${completed.title}_${unblockedNote}\n` +
            `Next workqueue cycle will pick up any newly unblocked items.`
          );
        });

        const result = {
          ok: true,
          completed: itemId,
          unblocked: unblockedIds,
          message: unblockedIds.length
            ? `✅ Marked complete. Unblocked: ${unblockedIds.join(', ')}`
            : '✅ Marked complete.'
        };

        res.writeHead(200, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify(result));

        console.log(`[complete] ${itemId} → done. Unblocked: ${unblockedIds.join(', ') || 'none'}`);
      } catch (e) {
        console.error('[complete error]', e);
        res.writeHead(500); res.end(JSON.stringify({ error: e.message }));
      }
    });
    return;
  }

  res.writeHead(404); res.end(JSON.stringify({ error: 'not found' }));
});

server.listen(PORT, '0.0.0.0', () => {
  console.log(`[wq-api] listening on 0.0.0.0:${PORT}`);
});
