/**
 * clawbus-receiver — OpenClaw plugin
 *
 * Registers a Gateway HTTP route POST /squirrelbus/receive.
 * When called with a ClawBus message JSON body, validates the bearer token,
 * appends the message to the local bus log, and injects it as a system event
 * into the next agent session via the before_prompt_build hook.
 */

import { appendFileSync, existsSync, mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { randomUUID } from 'node:crypto';

const SQUIRRELBUS_TOKEN = process.env.CLAWBUS_TOKEN || process.env.SQUIRRELBUS_TOKEN || '';
const PLUGIN_ID = 'clawbus-receiver';

/** Queue of pending bus messages to inject at next prompt build */
const pendingInjections = [];

function formatSystemEvent(msg) {
  const body = typeof msg.body === 'string' ? msg.body : JSON.stringify(msg.body ?? '');
  const content = msg.subject
    ? `${msg.subject}: ${body}`
    : body;
  return `System: [ClawBus] From @${msg.from}: ${content.slice(0, 200)}`;
}

function getLocalBusLogPath(workspaceDir) {
  const dir = workspaceDir
    ? join(workspaceDir, 'clawbus')
    : join(process.env.HOME || '/tmp', '.openclaw', 'workspace', 'clawbus');
  if (!existsSync(dir)) mkdirSync(dir, { recursive: true });
  return join(dir, 'bus.jsonl');
}

/** @type {import('@openclaw/sdk').OpenClawPluginDefinition} */
const definition = {
  id: PLUGIN_ID,
  name: 'ClawBus Receiver',
  description: 'Receives ClawBus push messages from Rocky and injects them as system events.',
  version: '1.0.0',

  register(api) {
    const { logger } = api;
    const workspaceDir = api.config?.workspaceDir;

    // ── HTTP route: POST /clawbus/receive ──────────────────────────────
    api.registerHttpRoute({
      path: '/clawbus/receive',
      auth: 'plugin',   // plugin validates the token itself
      handler: async (req, res) => {
        // Validate bearer token
        const authHeader = req.headers['authorization'] || '';
        const token = authHeader.startsWith('Bearer ') ? authHeader.slice(7) : '';
        if (token !== SQUIRRELBUS_TOKEN) {
          res.writeHead(401, { 'Content-Type': 'application/json' });
          res.end(JSON.stringify({ error: 'Unauthorized' }));
          return true;
        }

        // Parse JSON body
        let msg;
        try {
          const chunks = [];
          for await (const chunk of req) chunks.push(chunk);
          msg = JSON.parse(Buffer.concat(chunks).toString('utf8'));
        } catch (e) {
          res.writeHead(400, { 'Content-Type': 'application/json' });
          res.end(JSON.stringify({ error: 'Invalid JSON body' }));
          return true;
        }

        if (!msg || !msg.from || !msg.body) {
          res.writeHead(400, { 'Content-Type': 'application/json' });
          res.end(JSON.stringify({ error: 'Message must have from and body fields' }));
          return true;
        }

        // Normalize message
        const normalized = {
          id:      msg.id || randomUUID(),
          from:    msg.from,
          to:      msg.to || 'all',
          ts:      msg.ts || new Date().toISOString(),
          seq:     msg.seq ?? null,
          type:    msg.type || 'text',
          mime:    msg.mime || 'text/plain',
          enc:     msg.enc || 'none',
          body:    msg.body,
          ref:     msg.ref || null,
          subject: msg.subject || null,
          ttl:     msg.ttl ?? 604800,
          _pushed: true,   // mark as received via push
        };

        // Append to local bus log
        try {
          const logPath = getLocalBusLogPath(workspaceDir);
          appendFileSync(logPath, JSON.stringify(normalized) + '\n', 'utf8');
        } catch (e) {
          logger.warn(`[${PLUGIN_ID}] Failed to append to local bus log: ${e.message}`);
        }

        // Queue for system event injection at next prompt build
        const systemText = formatSystemEvent(normalized);
        pendingInjections.push({ msg: normalized, systemText, receivedAt: Date.now() });

        logger.info(`[${PLUGIN_ID}] Received bus message from @${normalized.from}: ${systemText.slice(0, 100)}`);
        console.log(`\n📨 [ClawBus] PUSH from @${normalized.from} → ${normalized.to}: ${systemText}\n`);

        res.writeHead(200, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify({ ok: true, id: normalized.id }));
        return true;
      },
    });

    // ── Hook: inject pending bus messages at prompt build time ────────────
    // Clears pending messages older than 10 minutes (avoid stale injections).
    const MAX_PENDING_AGE_MS = 10 * 60 * 1000;

    api.on('before_prompt_build', (event) => {
      const now = Date.now();
      // Evict stale entries
      while (pendingInjections.length > 0 && now - pendingInjections[0].receivedAt > MAX_PENDING_AGE_MS) {
        pendingInjections.shift();
      }
      if (pendingInjections.length === 0) return;

      const toInject = pendingInjections.splice(0);
      const injected = toInject.map(p => p.systemText).join('\n');

      // Prepend to system messages if the hook event supports it
      if (event && typeof event === 'object') {
        if (Array.isArray(event.systemMessages)) {
          event.systemMessages.unshift(injected);
        } else if ('systemPrompt' in event && typeof event.systemPrompt === 'string') {
          event.systemPrompt = `${injected}\n\n${event.systemPrompt}`;
        }
      }

      logger.info(`[${PLUGIN_ID}] Injected ${toInject.length} pending ClawBus message(s) into prompt.`);
    });

    logger.info(`[${PLUGIN_ID}] Registered: POST /clawbus/receive (token auth)`);
  },
};

export default definition;
