/**
 * rcc/api/routes/agentos.mjs — AgentOS simulation/debug route handlers
 * Extracted from api/index.mjs (structural refactor only — no logic changes)
 */

export default function registerRoutes(app, state) {
  const { json, readBody, isAuthed } = state;

  // ── GET /api/agentos/slots — VibeEngine slot health + swap metrics ──────────
  app.on('GET', '/api/agentos/slots', async (req, res) => {
    const AGENTOS_CACHE_TTL = 5 * 60 * 1000;
    const now = Date.now();
    if (!state._agentosSlotCache) state._agentosSlotCache = { data: null, ts: 0 };
    const cache = state._agentosSlotCache;
    if (cache.data && (now - cache.ts) < AGENTOS_CACHE_TTL) {
      return json(res, 200, cache.data);
    }
    const AGENTFS_URL  = process.env.AGENTFS_URL  || 'http://100.87.229.125:8791';
    let agentfsHealth = null;
    let agentfsModuleCount = 0;
    try {
      const ctrl = new AbortController();
      const tid = setTimeout(() => ctrl.abort(), 3000);
      const hResp = await fetch(`${AGENTFS_URL}/health`, { signal: ctrl.signal });
      clearTimeout(tid);
      if (hResp.ok) agentfsHealth = await hResp.json();
      const ctrl2 = new AbortController();
      const tid2 = setTimeout(() => ctrl2.abort(), 3000);
      const mResp = await fetch(`${AGENTFS_URL}/modules`, { signal: ctrl2.signal });
      clearTimeout(tid2);
      if (mResp.ok) {
        const mData = await mResp.json();
        agentfsModuleCount = Array.isArray(mData) ? mData.length
          : (mData.count ?? mData.total ?? 0);
      }
    } catch (_) { /* AgentFS offline */ }

    const MAX_SWAP_SLOTS = 4;
    const AGENT_POOL_SIZE = 8;
    const agentfsOnline = agentfsHealth !== null;
    const slots = Array.from({ length: MAX_SWAP_SLOTS }, (_, i) => ({
      slot_id: i,
      state: i === 0 ? 'active' : 'idle',
      wasm_module_hash: i === 0 ? 'echo_service_demo_305b' : null,
      service_name: i === 0 ? 'toolsvc' : null,
      version: i === 0 ? 2 : 1,
      last_swap_time: i === 0 ? new Date(Date.now() - 90 * 60 * 1000).toISOString() : null,
    }));

    const result = {
      ts: new Date().toISOString(),
      agentfs: {
        online: agentfsOnline,
        url: AGENTFS_URL,
        module_count: agentfsModuleCount,
        ...(agentfsHealth || {}),
      },
      vibe_engine: {
        status: 'running',
        arch: process.env.AGENTOS_ARCH || 'riscv64',
        swap_slots: {
          total: MAX_SWAP_SLOTS,
          active: slots.filter(s => s.state === 'active').length,
          idle: slots.filter(s => s.state === 'idle').length,
        },
        slots,
      },
      agent_pool: {
        total_workers: AGENT_POOL_SIZE,
        available: AGENT_POOL_SIZE,
      },
    };
    cache.data = result;
    cache.ts = now;
    return json(res, 200, result);
  });

  // ── GET /api/agentos/debug/sessions ─────────────────────────────────────────
  app.on('GET', '/api/agentos/debug/sessions', async (req, res) => {
    if (!state._debugSessions) state._debugSessions = [];
    const sessions = state._debugSessions.filter(s => s.status === 'attached');
    return json(res, 200, {
      ok: true,
      ts: new Date().toISOString(),
      sessions: sessions.map(s => ({
        session_id: s.id,
        slot_id: s.slot_id,
        attached_at: s.attached_at,
        breakpoints: s.breakpoints || [],
        status: s.status,
        nano_source: s.nano_source || null,
        wasm_map: s.wasm_map || null,
      })),
      total: sessions.length,
    });
  });

  // ── POST /api/agentos/debug/attach ───────────────────────────────────────────
  app.on('POST', '/api/agentos/debug/attach', async (req, res) => {
    const body = await readBody(req);
    const slot_id = body.slot_id ?? body.slotId;
    if (slot_id === undefined || slot_id === null) {
      return json(res, 400, { error: 'slot_id required' });
    }
    if (!state._debugSessions) state._debugSessions = [];
    const existing = state._debugSessions.find(s => s.slot_id === slot_id && s.status === 'attached');
    if (existing) {
      return json(res, 409, { error: `Slot ${slot_id} already has debug session ${existing.id}` });
    }
    const session = {
      id: `dbg-${Date.now()}-${slot_id}`,
      slot_id,
      attached_at: new Date().toISOString(),
      status: 'attached',
      breakpoints: [],
      nano_source: body.nano_source || null,
      wasm_map: body.wasm_map || null,
      agent: body.agent || null,
    };
    state._debugSessions.push(session);
    return json(res, 200, { ok: true, session });
  });

  // ── POST /api/agentos/debug/detach ───────────────────────────────────────────
  app.on('POST', '/api/agentos/debug/detach', async (req, res) => {
    const body = await readBody(req);
    const session_id = body.session_id || body.sessionId;
    if (!session_id) return json(res, 400, { error: 'session_id required' });
    if (!state._debugSessions) state._debugSessions = [];
    const session = state._debugSessions.find(s => s.id === session_id);
    if (!session) return json(res, 404, { error: 'Session not found' });
    session.status = 'detached';
    session.detached_at = new Date().toISOString();
    return json(res, 200, { ok: true, session });
  });

  // ── POST /api/agentos/debug/breakpoint ──────────────────────────────────────
  app.on('POST', '/api/agentos/debug/breakpoint', async (req, res) => {
    const body = await readBody(req);
    const { session_id, wasm_offset, action } = body;
    if (!session_id || wasm_offset === undefined) {
      return json(res, 400, { error: 'session_id and wasm_offset required' });
    }
    if (!state._debugSessions) state._debugSessions = [];
    const session = state._debugSessions.find(s => s.id === session_id && s.status === 'attached');
    if (!session) return json(res, 404, { error: 'Active session not found' });
    if (action === 'clear') {
      session.breakpoints = (session.breakpoints || []).filter(bp => bp.wasm_offset !== wasm_offset);
    } else {
      const bp = { wasm_offset, set_at: new Date().toISOString(), line: body.line, col: body.col, func: body.func };
      session.breakpoints = [...(session.breakpoints || []), bp];
    }
    return json(res, 200, { ok: true, breakpoints: session.breakpoints });
  });

  // ── POST /api/agentos/debug/step ────────────────────────────────────────────
  app.on('POST', '/api/agentos/debug/step', async (req, res) => {
    const body = await readBody(req);
    if (!body.session_id) return json(res, 400, { error: 'session_id required' });
    if (!state._debugSessions) state._debugSessions = [];
    const session = state._debugSessions.find(s => s.id === body.session_id && s.status === 'attached');
    if (!session) return json(res, 404, { error: 'Active session not found' });
    return json(res, 200, {
      ok: true,
      session_id: session.id,
      slot_id: session.slot_id,
      step: 'completed',
      note: 'In production, this PPC to debug_bridge PD via OP_DBG_STEP (0x73)',
    });
  });

  // ── GET /api/agentos/console/:slot ──────────────────────────────────────────
  app.on('GET', /^\/api\/agentos\/console\/(\d+)$/, async (req, res, m) => {
    const slot = parseInt(m[1], 10);
    if (slot < 0 || slot > 15) return json(res, 400, { error: 'slot must be 0-15' });
    if (!state._consoleMuxRings) state._consoleMuxRings = {};
    const ring = state._consoleMuxRings[slot] || [];
    return json(res, 200, {
      slot,
      lines: ring.length > 0 ? ring : [`[console_mux] slot ${slot} ready — TODO: wire QEMU pipe`],
    });
  });

  // ── POST /api/agentos/console/attach/:slot ──────────────────────────────────
  app.on('POST', /^\/api\/agentos\/console\/attach\/(\d+)$/, async (req, res, m) => {
    const slot = parseInt(m[1], 10);
    if (slot < 0 || slot > 15) return json(res, 400, { error: 'slot must be 0-15' });
    return json(res, 200, { ok: true, slot, note: 'attach queued — TODO: wire to console_mux PD' });
  });

  // ── POST /api/agentos/console/push ──────────────────────────────────────────
  app.on('POST', '/api/agentos/console/push', async (req, res) => {
    const body = await readBody(req);
    const { slot, line } = body;
    if (typeof slot !== 'number' || typeof line !== 'string')
      return json(res, 400, { error: 'slot (number) and line (string) required' });
    if (!state._consoleMuxRings) state._consoleMuxRings = {};
    if (!state._consoleMuxRings[slot]) state._consoleMuxRings[slot] = [];
    state._consoleMuxRings[slot].push(line);
    if (state._consoleMuxRings[slot].length > 200)
      state._consoleMuxRings[slot] = state._consoleMuxRings[slot].slice(-200);
    return json(res, 200, { ok: true });
  });

  // ── GET /api/agentos/shell — SSE output stream ───────────────────────────────
  app.on('GET', '/api/agentos/shell', async (req, res) => {
    if (!state._devShellOutput) state._devShellOutput = [];
    if (!state._devShellSseClients) state._devShellSseClients = new Set();

    res.writeHead(200, {
      'Content-Type': 'text/event-stream',
      'Cache-Control': 'no-cache',
      'Connection': 'keep-alive',
      'X-Accel-Buffering': 'no',
      'Access-Control-Allow-Origin': '*',
    });
    res.flushHeaders?.();

    for (const line of state._devShellOutput.slice(-100)) {
      res.write(`data: ${JSON.stringify({ type: 'output', text: line })}\n\n`);
    }
    res.write(`data: ${JSON.stringify({ type: 'connected', ts: new Date().toISOString() })}\n\n`);

    state._devShellSseClients.add(res);
    const ka = setInterval(() => res.write(': ping\n\n'), 20000);
    req.on('close', () => {
      state._devShellSseClients.delete(res);
      clearInterval(ka);
    });
    return;
  });

  // ── POST /api/agentos/shell/cmd ──────────────────────────────────────────────
  app.on('POST', '/api/agentos/shell/cmd', async (req, res) => {
    const body = await readBody(req);
    const cmd = (typeof body.cmd === 'string') ? body.cmd.trim() : '';
    if (!cmd) return json(res, 400, { error: 'cmd required' });
    if (!state._devShellSseClients) state._devShellSseClients = new Set();
    if (!state._devShellOutput)    state._devShellOutput = [];

    const echoLine = `> ${cmd}`;
    state._devShellOutput.push(echoLine);
    if (state._devShellOutput.length > 500) state._devShellOutput.shift();

    for (const client of state._devShellSseClients) {
      client.write(`data: ${JSON.stringify({ type: 'output', text: echoLine })}\n\n`);
    }

    const simulated = {
      'help':      'agentOS dev_shell — type pd list, mem dump, trace dump, etc.\r\n> ',
      'version':   'agentOS v0.1.0-alpha\r\n> ',
      'pd list':   'PD list: controller(50) swap_slot_0..3(75) worker_0..7(80) init_agent(90) ...\r\n> ',
      'mr list':   'MRs: perf_ring vibe_code vibe_state gpu_tensor_buf dev_shell_ring\r\n> ',
      'perf show': '[0] pd=0 ch=0 lat=0ns  [1] pd=0 ch=0 lat=0ns (ring empty)\r\n> ',
      'trace dump':'trace dump: notified trace_recorder\r\n> ',
    };
    const resp = simulated[cmd] || `unknown command: ${cmd}\r\nType 'help' for command list.\r\n> `;
    state._devShellOutput.push(resp);
    if (state._devShellOutput.length > 500) state._devShellOutput.shift();
    for (const client of state._devShellSseClients) {
      client.write(`data: ${JSON.stringify({ type: 'output', text: resp })}\n\n`);
    }

    return json(res, 200, { ok: true, cmd, queued: true });
  });

  // ── POST /api/agentos/shell/push ────────────────────────────────────────────
  app.on('POST', '/api/agentos/shell/push', async (req, res) => {
    const body = await readBody(req);
    const text = (typeof body.text === 'string') ? body.text : '';
    if (!text) return json(res, 400, { error: 'text required' });
    if (!state._devShellSseClients) state._devShellSseClients = new Set();
    if (!state._devShellOutput)    state._devShellOutput = [];

    state._devShellOutput.push(text);
    if (state._devShellOutput.length > 500) state._devShellOutput.shift();
    for (const client of state._devShellSseClients) {
      client.write(`data: ${JSON.stringify({ type: 'output', text })}\n\n`);
    }
    return json(res, 200, { ok: true });
  });

  // ── GET /api/agentos/cap-events ─────────────────────────────────────────────
  app.on('GET', '/api/agentos/cap-events', async (req, res, _m, url) => {
    if (!state._capAuditEvents) state._capAuditEvents = [];
    if (!state._capAuditSeq) state._capAuditSeq = 0;

    const CAP_KINDS = {
      0x01: 'FS', 0x02: 'NET', 0x04: 'GPU', 0x08: 'IPC',
      0x10: 'TIMER', 0x20: 'STDIO', 0x40: 'SPAWN', 0x80: 'SWAP',
    };
    function capKindName(mask) {
      return Object.entries(CAP_KINDS).filter(([k]) => (mask & Number(k))).map(([,v]) => v).join('|') || `0x${mask.toString(16)}`;
    }

    const limitS    = url.searchParams.get('limit');
    const slotS     = url.searchParams.get('slot');
    const typeF     = url.searchParams.get('event_type');
    const sinceSeqS = url.searchParams.get('since_seq');
    const limit     = limitS    ? Math.min(parseInt(limitS, 10), 1000) : 100;
    const filterSlot= slotS     ? parseInt(slotS, 10) : null;
    const sinceSeq  = sinceSeqS ? parseInt(sinceSeqS, 10) : 0;

    let events = state._capAuditEvents;
    if (sinceSeq > 0)   events = events.filter(e => e.seq > sinceSeq);
    if (filterSlot !== null) events = events.filter(e => e.slot_id === filterSlot);
    if (typeF)          events = events.filter(e => e.event_type === typeF.toUpperCase());

    const slice = events.slice(-limit);
    return json(res, 200, {
      ok: true,
      events: slice.map(e => ({
        ...e,
        cap_kind_name: capKindName(e.rights ?? e.caps_mask ?? 0),
      })),
      total:         state._capAuditEvents.length,
      watermark_seq: state._capAuditSeq,
      ts:            new Date().toISOString(),
    });
  });

  // POST /api/agentos/cap-events/push
  app.on('POST', '/api/agentos/cap-events/push', async (req, res) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    if (!state._capAuditEvents) state._capAuditEvents = [];
    if (!state._capAuditSeq) state._capAuditSeq = 0;
    const body = await readBody(req);
    const incoming = Array.isArray(body.events) ? body.events : (body.event ? [body.event] : []);
    let added = 0;
    for (const ev of incoming) {
      const seq = ev.seq ?? ++state._capAuditSeq;
      const entry = {
        seq,
        ts:         ev.ts || new Date().toISOString(),
        tick:       ev.tick ?? 0,
        event_type: (ev.event_type || 'GRANT').toUpperCase(),
        agent_id:   ev.agent_id ?? ev.agentId ?? 0,
        slot_id:    ev.slot_id ?? ev.slotId ?? 0,
        caps_mask:  ev.caps_mask ?? ev.capsMask ?? ev.rights ?? 0,
        rights:     ev.caps_mask ?? ev.capsMask ?? ev.rights ?? 0,
      };
      state._capAuditEvents.push(entry);
      if (seq > state._capAuditSeq) state._capAuditSeq = seq;
      added++;
    }
    if (state._capAuditEvents.length > 10000)
      state._capAuditEvents = state._capAuditEvents.slice(-10000);
    if (state._capAuditSseClients && added > 0) {
      const payload = JSON.stringify({ type: 'cap_events', events: incoming });
      for (const client of state._capAuditSseClients) {
        try { client.write(`data: ${payload}\n\n`); }
        catch { state._capAuditSseClients.delete(client); }
      }
    }
    return json(res, 200, { ok: true, added });
  });

  // GET /api/agentos/cap-events/stream — SSE stream of live cap audit events
  app.on('GET', '/api/agentos/cap-events/stream', async (req, res) => {
    if (!state._capAuditSseClients) state._capAuditSseClients = new Set();
    if (!state._capAuditEvents) state._capAuditEvents = [];
    if (!state._capAuditSeq) state._capAuditSeq = 0;
    res.writeHead(200, {
      'Content-Type': 'text/event-stream',
      'Cache-Control': 'no-cache',
      'Connection': 'keep-alive',
      'Access-Control-Allow-Origin': '*',
    });
    res.flushHeaders?.();
    const recent = state._capAuditEvents.slice(-50);
    if (recent.length) {
      res.write(`data: ${JSON.stringify({ type: 'cap_events_backfill', events: recent })}\n\n`);
    }
    res.write(`data: ${JSON.stringify({ type: 'connected', seq: state._capAuditSeq })}\n\n`);
    state._capAuditSseClients.add(res);
    const ka = setInterval(() => res.write(': ping\n\n'), 20000);
    req.on('close', () => {
      state._capAuditSseClients.delete(res);
      clearInterval(ka);
    });
    return;
  });

  // GET /api/agentos/cap-events/export — download full audit log as JSON
  app.on('GET', '/api/agentos/cap-events/export', async (req, res) => {
    if (!state._capAuditEvents) state._capAuditEvents = [];
    const CAP_KINDS = {
      0x01: 'FS', 0x02: 'NET', 0x04: 'GPU', 0x08: 'IPC',
      0x10: 'TIMER', 0x20: 'STDIO', 0x40: 'SPAWN', 0x80: 'SWAP',
    };
    function capKindName(mask) {
      return Object.entries(CAP_KINDS).filter(([k]) => (mask & Number(k))).map(([,v]) => v).join('|') || `0x${mask.toString(16)}`;
    }
    const events = state._capAuditEvents.map(e => ({
      ...e,
      cap_kind_name: capKindName(e.rights ?? e.caps_mask ?? 0),
    }));
    res.writeHead(200, {
      'Content-Type': 'application/json',
      'Content-Disposition': `attachment; filename="cap_audit_${Date.now()}.json"`,
    });
    res.end(JSON.stringify({ exported_at: new Date().toISOString(), events }, null, 2));
    return;
  });

  // ── GET /api/agentos/events — synthetic agentOS lifecycle events ────────────
  app.on('GET', /^\/api\/agentos\/events/, async (req, res) => {
    const now = Date.now();
    const windowMs = 30 * 60 * 1000;
    const EVENT_TYPES = ['spawn','cap_grant','cap_revoke','quota_exceeded','fault','watchdog_reset','memory_alert','exit'];
    const EVENT_DETAILS = {
      spawn:          s => `slot ${s} agent spawned`,
      exit:           s => `slot ${s} exited cleanly (rc=0)`,
      cap_grant:      s => `granted cap=IPC_SEND to pid=${4000+s*100+((now>>4)&0x3f)}`,
      cap_revoke:     s => `revoked cap=IPC_SEND from pid=${4000+s*100+((now>>6)&0x3f)}`,
      quota_exceeded: s => `slot ${s} CPU quota exceeded (${80+((now>>8)&0x13)}%)`,
      fault:          s => `slot ${s} SIGSEGV at 0x${(0xdeadbe00+s*0x100+((now>>3)&0xff)).toString(16)}`,
      watchdog_reset: s => `slot ${s} heartbeat timeout — watchdog triggered reset`,
      memory_alert:   s => `slot ${s} memory spike: ${256+((now>>5)&0xff)}MB`,
    };
    const seed = Math.floor(now / 60000);
    function sr(n, s2) { return ((n * 1337 + s2 * 7919) % 997) / 997; }
    const events = [];
    for (let i = 0; i < 100; i++) {
      const slotId = Math.floor(sr(i, seed) * 8);
      const type = EVENT_TYPES[Math.floor(sr(i + 1000, seed) * EVENT_TYPES.length)];
      const ts = Math.floor(now - windowMs + sr(i + 2000, seed) * windowMs);
      events.push({ ts, slot_id: slotId, type, details: EVENT_DETAILS[type](slotId) });
    }
    events.sort((a, b) => a.ts - b.ts);
    return json(res, 200, { events, slots: [0,1,2,3,4,5,6,7], generated_at: now });
  });

  // ── GET /api/agentos/timeline — agent lifecycle event list (5s cache) ────────
  app.on('GET', /^\/api\/agentos\/timeline/, async (req, res) => {
    const now = Date.now();
    const CACHE_TTL = 5_000;
    if (state._tlCache && (now - state._tlCache.ts) < CACHE_TTL) {
      return json(res, 200, state._tlCache.data);
    }
    const windowMs = 30 * 60 * 1000;
    const EVENT_TYPES = ['spawn','cap_grant','cap_revoke','quota_exceeded','fault','watchdog_reset','memory_alert','hotreload'];
    const EVENT_DETAILS = {
      spawn:          s => `slot ${s} agent spawned`,
      hotreload:      s => `slot ${s} hot-reload triggered`,
      cap_grant:      s => `granted cap=IPC_SEND to pid=${4000+s*100+((now>>4)&0x3f)}`,
      cap_revoke:     s => `revoked cap=IPC_SEND from pid=${4000+s*100+((now>>6)&0x3f)}`,
      quota_exceeded: s => `slot ${s} CPU quota exceeded (${80+((now>>8)&0x13)}%)`,
      fault:          s => `slot ${s} SIGSEGV at 0x${(0xdeadbe00+s*0x100+((now>>3)&0xff)).toString(16)}`,
      watchdog_reset: s => `slot ${s} heartbeat timeout — watchdog triggered reset`,
      memory_alert:   s => `slot ${s} memory spike: ${256+((now>>5)&0xff)}MB`,
    };
    const seed = Math.floor(now / 60000);
    function tsr(n, s2) { return ((n * 1337 + s2 * 7919) % 997) / 997; }
    const tlEvents = [];
    for (let i = 0; i < 100; i++) {
      const slot_id = Math.floor(tsr(i, seed) * 8);
      const event_type = EVENT_TYPES[Math.floor(tsr(i + 1000, seed) * EVENT_TYPES.length)];
      const ts = Math.floor(now - windowMs + tsr(i + 2000, seed) * windowMs);
      tlEvents.push({ ts, slot_id, event_type, detail: EVENT_DETAILS[event_type](slot_id) });
    }
    tlEvents.sort((a, b) => b.ts - a.ts);
    const tlResult = { events: tlEvents.slice(0, 100), generated_at: now };
    state._tlCache = { ts: now, data: tlResult };
    return json(res, 200, tlResult);
  });

  // ── GET /api/agentos/wasm-profiles — WASM slot profiler snapshot ─────────
  // Returns per-slot CPU%, mem, tick counters, and call-stack frame breakdown.
  // Data feeds the Profiler tab SVG flame graph in the wasm-dashboard.
  // Polls agentOS console WS server at sparky:8790/api/agentos/profiler/snapshot
  // when available; falls back to synthetic jitter for disconnected dev mode.
  app.on('GET', '/api/agentos/wasm-profiles', async (req, res) => {
    const now = Date.now();
    const PROFILE_TTL = 2000; // 2s cache — matches dashboard poll interval
    if (state._profileCache && (now - state._profileCache.ts) < PROFILE_TTL) {
      return json(res, 200, state._profileCache.data);
    }

    const SPARKY_CONSOLE = process.env.SPARKY_CONSOLE_URL || 'http://100.87.229.125:8790';

    let data = null;
    try {
      const ctrl = new AbortController();
      const timer = setTimeout(() => ctrl.abort(), 1500);
      const r = await fetch(`${SPARKY_CONSOLE}/api/agentos/profiler/snapshot`, { signal: ctrl.signal });
      clearTimeout(timer);
      if (r.ok) data = await r.json();
    } catch { /* sparky offline — use synthetic */ }

    if (!data) {
      // Synthetic profiler snapshot — stable jitter seeded on 2s buckets
      const j = (base, range = 200, seed2 = 0) =>
        Math.max(0, base + Math.floor(((((now / 2000) + seed2 + base) * 2053) % range) - range / 2));

      data = {
        ts: now,
        slots: [
          {
            id: 0, name: 'inference_worker',
            cpu_pct: Math.min(99, Math.max(1, j(42, 10, 0))),
            mem_kb:  j(8192, 512, 1),
            ticks:   j(12450, 200, 2),
            frames: [
              { fn: 'matmul_f32',   ticks: j(5200, 400, 3),  depth: 0 },
              { fn: 'softmax',      ticks: j(2100, 200, 4),  depth: 1 },
              { fn: 'embed_lookup', ticks: j(1800, 150, 5),  depth: 1 },
              { fn: 'layer_norm',   ticks: j(900,  100, 6),  depth: 2 },
              { fn: 'rms_norm',     ticks: j(620,  80,  7),  depth: 2 },
              { fn: 'rope_enc',     ticks: j(480,  60,  8),  depth: 3 },
            ],
          },
          {
            id: 1, name: 'event_handler',
            cpu_pct: Math.min(99, Math.max(1, j(8, 4, 10))),
            mem_kb:  j(512, 64, 11),
            ticks:   j(2340, 150, 12),
            frames: [
              { fn: 'dispatch_event', ticks: j(1200, 100, 13), depth: 0 },
              { fn: 'cap_check',      ticks: j(600,  80,  14), depth: 1 },
              { fn: 'ring_enqueue',   ticks: j(340,  60,  15), depth: 2 },
            ],
          },
          {
            id: 2, name: 'vibe_validator',
            cpu_pct: Math.min(99, Math.max(1, j(15, 6, 20))),
            mem_kb:  j(2048, 256, 21),
            ticks:   j(4110, 300, 22),
            frames: [
              { fn: 'wasm_validate', ticks: j(2800, 200, 23), depth: 0 },
              { fn: 'section_parse', ticks: j(1100, 100, 24), depth: 1 },
              { fn: 'type_check',    ticks: j(540,  80,  25), depth: 2 },
            ],
          },
        ],
      };
    }

    state._profileCache = { ts: now, data };
    return json(res, 200, data);
  });
}
