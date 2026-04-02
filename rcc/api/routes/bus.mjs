/**
 * rcc/api/routes/bus.mjs — ClawBus route handlers
 * Extracted from api/index.mjs (structural refactor only — no logic changes)
 */

import { existsSync } from 'fs';
import { appendFile } from 'fs/promises';
import { createInterface } from 'readline';
import { createReadStream as createRS } from 'fs';

export default function registerRoutes(app, state) {
  const {
    json, readBody, isAuthed,
    _busSeq, _busSSEClients, _busPresence, _busAcks, _busDeadLetters,
    _busAppend, _busReadMessages,
    BUS_LOG_PATH, ACK_LOG_PATH,
  } = state;

  // GET /bus/messages
  app.on('GET', '/bus/messages', async (req, res, _m, url) => {
    const { from, to, limit, since, type } = Object.fromEntries(url.searchParams);
    const msgs = await state._busReadMessages({ from, to, type, since, limit: limit ? parseInt(limit, 10) : 100 });
    return json(res, 200, msgs);
  });

  // POST /bus/send
  app.on('POST', '/bus/send', async (req, res) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const busBody = await readBody(req);
    const msg = await state._busAppend(busBody);
    return json(res, 200, { ok: true, message: msg });
  });

  // GET /bus/stream — SSE
  app.on('GET', '/bus/stream', async (req, res) => {
    res.writeHead(200, { 'Content-Type': 'text/event-stream', 'Cache-Control': 'no-cache', 'Connection': 'keep-alive', 'Access-Control-Allow-Origin': '*' });
    res.write('data: {"type":"connected"}\n\n');
    state._busSSEClients.add(res);
    req.on('close', () => state._busSSEClients.delete(res));
    return; // keep connection open
  });

  // POST /bus/heartbeat
  app.on('POST', '/bus/heartbeat', async (req, res) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const busHbBody = await readBody(req);
    const from = busHbBody.from;
    if (!from) return json(res, 400, { error: 'from required' });
    state._busPresence[from] = { agent: from, ts: new Date().toISOString(), status: 'online', ...busHbBody };
    await state._busAppend({ from, to: 'all', type: 'heartbeat', body: JSON.stringify({ status: 'online', ...busHbBody }), mime: 'application/json' });
    return json(res, 200, { ok: true, presence: state._busPresence });
  });

  // GET /bus/presence
  app.on('GET', '/bus/presence', async (req, res) => {
    return json(res, 200, state._busPresence);
  });

  // POST /bus/ack
  app.on('POST', '/bus/ack', async (req, res) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const busAckBody = await readBody(req);
    const { messageId, agent } = busAckBody;
    if (!messageId || !agent) return json(res, 400, { error: 'messageId and agent required' });
    const ack = { messageId, agent, ts: new Date().toISOString() };
    state._busAcks.set(messageId, ack);
    try { await appendFile(state.ACK_LOG_PATH, JSON.stringify(ack) + '\n', 'utf8'); } catch {}
    return json(res, 200, { ok: true, ack });
  });

  // GET /bus/dead
  app.on('GET', '/bus/dead', async (req, res) => {
    return json(res, 200, state._busDeadLetters);
  });

  // GET /bus/delivery-status
  app.on('GET', '/bus/delivery-status', async (req, res) => {
    const result = {};
    for (const [id] of state._busAcks) result[id] = 'acked';
    for (const d of state._busDeadLetters) result[d.id] = 'dead';
    return json(res, 200, result);
  });

  // GET /bus/message/:id/status
  app.on('GET', /^\/bus\/message\/([^/]+)\/status$/, async (req, res, m) => {
    const id = m[1];
    const ack  = state._busAcks.get(id) || null;
    const dead = state._busDeadLetters.find(d => d.id === id) || null;
    const ackState = dead ? 'dead' : ack ? 'acked' : 'fire-and-forget';
    return json(res, 200, { id, ackState, ack, deadReason: dead?._deadReason ?? null });
  });

  // GET /bus/replay — return missed messages since after_seq
  app.on('GET', '/bus/replay', async (req, res, _m, url) => {
    const agent     = url.searchParams.get('agent') || 'unknown';
    const afterSeqS = url.searchParams.get('after_seq');
    const channel   = url.searchParams.get('channel') || null;
    const limitS    = url.searchParams.get('limit');
    if (!state._busWatermarks) state._busWatermarks = {};
    const after_seq = afterSeqS ? parseInt(afterSeqS, 10) : (state._busWatermarks[agent]?.seq ?? 0);
    const limit     = limitS    ? Math.min(parseInt(limitS, 10), 1000) : 100;

    const msgs = [];
    try {
      if (existsSync(state.BUS_LOG_PATH)) {
        const rl = createInterface({ input: createRS(state.BUS_LOG_PATH), crlfDelay: Infinity });
        for await (const line of rl) {
          try {
            const m2 = JSON.parse(line);
            if (typeof m2.seq !== 'number' || m2.seq <= after_seq) continue;
            if (channel && channel !== 'all' && m2.to !== channel && m2.to !== 'all') continue;
            msgs.push(m2);
            if (msgs.length >= limit) break;
          } catch {}
        }
      }
    } catch (err) {
      console.warn('[bus/replay] read error:', err.message);
    }

    const watermark = msgs.length ? msgs[msgs.length - 1].seq : after_seq;
    return json(res, 200, {
      ok: true,
      agent,
      after_seq,
      messages: msgs,
      count: msgs.length,
      watermark,
      ts: new Date().toISOString(),
    });
  });

  // POST /bus/subscribe — register agent watermark, return pending count
  app.on('POST', '/bus/subscribe', async (req, res) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const body = await readBody(req);
    if (!state._busWatermarks) state._busWatermarks = {};
    const agent     = body.agent || 'unknown';
    const channel   = body.channel || 'all';
    const after_seq = typeof body.after_seq === 'number'
                      ? body.after_seq
                      : (state._busWatermarks[agent]?.seq ?? 0);

    let pending_count = 0;
    const current_watermark = after_seq;
    try {
      if (existsSync(state.BUS_LOG_PATH)) {
        const rl = createInterface({ input: createRS(state.BUS_LOG_PATH), crlfDelay: Infinity });
        for await (const line of rl) {
          try {
            const m2 = JSON.parse(line);
            if (typeof m2.seq === 'number' && m2.seq > after_seq) {
              if (channel === 'all' || m2.to === channel || m2.to === 'all') {
                pending_count++;
              }
            }
          } catch {}
        }
      }
    } catch {}

    state._busWatermarks[agent] = {
      seq:       after_seq,
      channel,
      ts:        new Date().toISOString(),
      agent,
    };

    console.log(`[bus/subscribe] ${agent} subscribed after_seq=${after_seq} pending=${pending_count}`);
    return json(res, 200, {
      ok: true,
      agent,
      channel,
      after_seq,
      watermark: current_watermark,
      pending_count,
      hint: pending_count > 0
            ? `GET /bus/replay?agent=${agent}&after_seq=${after_seq} to fetch ${pending_count} missed messages`
            : 'up-to-date',
    });
  });

  // POST /bus/watermark — advance agent watermark after processing
  app.on('POST', '/bus/watermark', async (req, res) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const body = await readBody(req);
    if (!state._busWatermarks) state._busWatermarks = {};
    const agent = body.agent || 'unknown';
    const seq   = typeof body.seq === 'number' ? body.seq : null;
    if (seq === null) return json(res, 400, { error: 'seq (number) required' });

    if (!state._busWatermarks[agent]) state._busWatermarks[agent] = {};
    if (seq > (state._busWatermarks[agent].seq ?? -1)) {
      state._busWatermarks[agent].seq = seq;
      state._busWatermarks[agent].ts  = new Date().toISOString();
      const wmPath = state.BUS_LOG_PATH.replace('bus.jsonl', 'bus.watermarks.jsonl');
      try {
        const wm = { agent, seq, ts: state._busWatermarks[agent].ts };
        await appendFile(wmPath, JSON.stringify(wm) + '\n', 'utf8');
      } catch {}
    }

    return json(res, 200, {
      ok: true,
      agent,
      seq: state._busWatermarks[agent].seq,
      ts:  state._busWatermarks[agent].ts,
    });
  });

  // GET /bus/watermarks — list all known agent watermarks
  app.on('GET', '/bus/watermarks', async (req, res) => {
    if (!state._busWatermarks) state._busWatermarks = {};
    return json(res, 200, {
      ok: true,
      watermarks: Object.entries(state._busWatermarks).map(([k, v]) => ({
        agent: k, ...v,
      })),
      bus_seq: state._busSeq,
      ts: new Date().toISOString(),
    });
  });

  // POST /api/bus/receive — handle incoming ClawBus messages
  app.on('POST', '/api/bus/receive', async (req, res) => {
    const body = await readBody(req);
    state.broadcastGeekEvent('bus_msg', body.from || 'unknown', body.to || 'all', 'ClawBus message');
    if (body.type === 'lesson') {
      await state.receiveLessonFromBus(body);
      return json(res, 200, { ok: true });
    }
    return json(res, 200, { ok: true, ignored: true });
  });
}
