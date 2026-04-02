/**
 * ClawBus receive sidecar for Natasha (sparky)
 * Listens for POST /bus/receive from other agents, injects into OpenClaw main session
 * via the local gateway chatCompletions endpoint.
 *
 * Port: 18799 (loopback — Tailscale serve exposes as /bus/receive)
 */

import http from 'http';

const GATEWAY_URL = 'http://127.0.0.1:18789/v1/chat/completions';
const GATEWAY_TOKEN = '<SQUIRRELBUS_GATEWAY_TOKEN>';
const BUS_TOKEN = '<YOUR_RCC_TOKEN>';   // rotated 2026-03-23
const PORT = 18799;

function log(...args) {
  console.log(new Date().toISOString(), ...args);
}

async function forwardToGateway(msg) {
  const from = msg.from || 'unknown';
  const subject = msg.subject || '(no subject)';
  const body = msg.body || '';
  const type = msg.type || 'text';

  // Format as a system notification into the main session
  const content = `[ClawBus message from ${from}]\nType: ${type}\nSubject: ${subject}\n\n${body}`;

  const payload = {
    model: 'openclaw:main',
    messages: [{ role: 'user', content }],
    stream: false,
  };

  const resp = await fetch(GATEWAY_URL, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      'Authorization': `Bearer ${GATEWAY_TOKEN}`,
    },
    body: JSON.stringify(payload),
  });

  if (!resp.ok) {
    const text = await resp.text();
    throw new Error(`Gateway returned ${resp.status}: ${text}`);
  }

  return resp.json();
}

const server = http.createServer(async (req, res) => {
  // Health check — no auth required
  // Health check — no auth required (also handle /bus/health for direct access)
  if (req.method === 'GET' && (req.url === '/health' || req.url === '/bus/health')) {
    res.writeHead(200, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify({ ok: true, agent: 'natasha', host: 'sparky' }));
    return;
  }

  // Auth check for all other routes
  const auth = req.headers['authorization'] || '';
  if (auth !== `Bearer ${BUS_TOKEN}`) {
    res.writeHead(401, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify({ error: 'unauthorized' }));
    return;
  }

  // Handle both /receive (Tailscale strips /bus prefix) and /bus/receive (direct)
  if (req.method === 'POST' && (req.url === '/receive' || req.url === '/bus/receive')) {
    let body = '';
    req.on('data', chunk => { body += chunk; });
    req.on('end', async () => {
      try {
        const msg = JSON.parse(body);
        log(`Received from=${msg.from} type=${msg.type} subject=${msg.subject}`);

        await forwardToGateway(msg);

        res.writeHead(200, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify({ ok: true }));
      } catch (err) {
        log('Error:', err.message);
        res.writeHead(500, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify({ error: err.message }));
      }
    });
    return;
  }

  res.writeHead(404, { 'Content-Type': 'application/json' });
  res.end(JSON.stringify({ error: 'not found' }));
});

server.listen(PORT, '127.0.0.1', () => {
  log(`ClawBus receive sidecar listening on 127.0.0.1:${PORT}`);
});
