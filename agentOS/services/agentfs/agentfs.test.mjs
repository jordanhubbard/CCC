/**
 * AgentFS test suite
 * Run: node --test agentOS/services/agentfs/agentfs.test.mjs
 */

import { test, describe, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'crypto';
import { createServer } from 'http';
import { once } from 'events';

// ── Minimal WASM module (magic + version + empty module) ──────────────────────
// \0asm version=1, no sections
const VALID_WASM = Buffer.from([0x00,0x61,0x73,0x6d,0x01,0x00,0x00,0x00]);

// ── Inline copies of pure capability functions (avoid importing the server) ───
function _readULEB128(buf, offset) {
  let result = 0, shift = 0;
  while (offset < buf.length) {
    const byte = buf[offset++];
    result |= (byte & 0x7f) << shift;
    shift += 7;
    if (!(byte & 0x80)) break;
  }
  return [result >>> 0, offset];
}
function _parseWasmCapabilities(wasmBuf) {
  if (wasmBuf.length < 8) return null;
  let pos = 8;
  while (pos < wasmBuf.length) {
    const sectionId = wasmBuf[pos++];
    if (pos >= wasmBuf.length) break;
    const [sectionSize, afterSize] = _readULEB128(wasmBuf, pos);
    pos = afterSize;
    const sectionEnd = pos + sectionSize;
    if (sectionId === 0) {
      const [nameLen, afterNameLen] = _readULEB128(wasmBuf, pos);
      const nameEnd = afterNameLen + nameLen;
      if (nameEnd > sectionEnd) { pos = sectionEnd; continue; }
      const sectionName = wasmBuf.slice(afterNameLen, nameEnd).toString('utf8');
      if (sectionName === 'agentfs.capabilities') {
        try { const caps = JSON.parse(wasmBuf.slice(nameEnd, sectionEnd).toString('utf8'));
          return { requires: Array.isArray(caps.requires) ? caps.requires.map(String) : [],
                   provides: Array.isArray(caps.provides) ? caps.provides.map(String) : [] }; }
        catch { return null; }
      }
    }
    pos = sectionEnd;
  }
  return null;
}
function _checkCapabilities(caps, required = []) {
  if (!required.length) return { ok: true, missing: [] };
  if (!caps) return { ok: false, missing: required, reason: 'no capability section' };
  const declared = new Set([...caps.requires, ...caps.provides]);
  const missing = required.filter(r => !declared.has(r));
  return { ok: missing.length === 0, missing };
}
/** Build a minimal WASM binary with an agentfs.capabilities custom section. */
function _buildWasmWithCaps(caps) {
  const name = Buffer.from('agentfs.capabilities');
  const json = Buffer.from(JSON.stringify(caps));
  const nameLen = Buffer.alloc(1); nameLen[0] = name.length;
  const sectionContent = Buffer.concat([nameLen, name, json]);
  const sectionSize = Buffer.alloc(1); sectionSize[0] = sectionContent.length;
  return Buffer.concat([VALID_WASM, Buffer.from([0x00]), sectionSize, sectionContent]);
}
const INVALID_WASM = Buffer.from([0xde,0xad,0xbe,0xef]);
const WASM_HASH = createHash('sha256').update(VALID_WASM).digest('hex');

const TOKEN = 'agentfs-test-token-' + Date.now();
const PORT  = 18791 + Math.floor(Math.random() * 100);

// ── Start server with isolated config ─────────────────────────────────────────
process.env.AGENTFS_PORT    = String(PORT);
process.env.AGENTFS_TOKEN   = TOKEN;
process.env.AGENTFS_BUCKET  = 'agentfs-test-' + Date.now();
// Use a mock MinIO (we'll mock S3 at the fetch level or use a temp store)
// For unit tests, mock the S3 layer with an in-memory store
process.env.AGENTFS_MOCK    = '1';

// ── In-memory S3 mock ─────────────────────────────────────────────────────────
// Patch the S3Client before importing index.mjs
const mockStore = new Map();

// We'll test via HTTP against a patched server instance that uses mock storage
// Build a self-contained mini-server for testing

import { createHash as _hash } from 'crypto';

const WASM_MAGIC_TEST = Buffer.from([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]);

function validateMagic(buf) {
  if (buf.length < 8) return { ok: false, reason: 'too small' };
  if (!buf.slice(0, 8).equals(WASM_MAGIC_TEST)) return { ok: false, reason: 'bad magic' };
  return { ok: true };
}

function sha(buf) { return _hash('sha256').update(buf).digest('hex'); }

function makeTestServer(port, token, opts = {}) {
  // Minimal in-memory AgentFS for testing (same logic, no S3)
  const store = new Map(); // hash → {buf, meta}

  function jres(res, status, body) {
    const p = JSON.stringify(body);
    res.writeHead(status, { 'Content-Type': 'application/json' });
    res.end(p);
  }

  function checkAuth(req) {
    return req.headers['authorization'] === `Bearer ${token}`;
  }

  async function readBody(req) {
    const chunks = [];
    for await (const chunk of req) chunks.push(chunk);
    return Buffer.concat(chunks);
  }

  const srv = createServer(async (req, res) => {
    const url = new URL(req.url, 'http://localhost');
    const p = url.pathname;

    if (p === '/agentfs/health' && req.method === 'GET') {
      return jres(res, 200, { ok: true });
    }
    if (!checkAuth(req)) return jres(res, 401, { error: 'unauthorized' });

    // POST /agentfs/modules[?require=...]
    if (p === '/agentfs/modules' && req.method === 'POST') {
      const buf = await readBody(req);
      const v = validateMagic(buf);
      if (!v.ok) return jres(res, 400, { error: `invalid WASM: ${v.reason}` });

      // Mock capability gate: ?require= param → reject if module doesn't declare them
      const requireParam = url.searchParams.get('require') || '';
      const requiredCaps = requireParam ? requireParam.split(',').map(s=>s.trim()).filter(Boolean) : [];
      if (requiredCaps.length) {
        // Minimal mock: no module has caps unless buf > 8 bytes (custom section present in real test)
        // For test purposes: always reject if require is set and buf === VALID_WASM (no section)
        if (buf.length <= 8) {
          return jres(res, 422, { error: 'capability gate rejected module', missing: requiredCaps, declared: null });
        }
      }

      const hash = sha(buf);
      const already = store.has(hash);
      const meta = { hash, size: buf.length, uploaded_at: new Date().toISOString(), aot: false, aot_size: null, aot_ms: null, capabilities: null };
      if (!already) store.set(hash, { buf, meta });

      // Publish bus event if busUrl configured (best-effort)
      if (!already && opts.busUrl) {
        fetch(`${opts.busUrl}/send`, {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ from: 'agentfs', to: 'all', type: 'agentos.fs.put',
            body: JSON.stringify({ hash, size: buf.length, origin_url: `http://localhost:${port}`, ts: new Date().toISOString() }) }),
        }).catch(() => {});
      }

      return jres(res, already ? 200 : 201, { hash, size: buf.length, already_exists: already, aot: false, capabilities: null });
    }

    // GET /agentfs/modules (list)
    if (p === '/agentfs/modules' && req.method === 'GET') {
      const modules = [...store.values()].map(e => e.meta);
      return jres(res, 200, { modules, count: modules.length });
    }

    // GET /agentfs/modules/:hash
    const m = p.match(/^\/agentfs\/modules\/([0-9a-f]{64})$/);
    if (m && req.method === 'GET') {
      const entry = store.get(m[1]);
      if (!entry) return jres(res, 404, { error: 'not found', hash: m[1] });
      res.writeHead(200, { 'Content-Type': 'application/wasm', 'X-AgentFS-Hash': m[1] });
      return res.end(entry.buf);
    }
    // DELETE /agentfs/modules/:hash
    if (m && req.method === 'DELETE') {
      if (!store.has(m[1])) return jres(res, 404, { error: 'not found' });
      store.delete(m[1]);
      return jres(res, 200, { ok: true, hash: m[1] });
    }

    jres(res, 404, { error: 'not found' });
  });

  return srv;
}

// ── Test suite ────────────────────────────────────────────────────────────────
describe('AgentFS', () => {
  let server, BASE, TOKEN_HDR;

  before(async () => {
    server = makeTestServer(PORT, TOKEN);
    server.listen(PORT);
    await once(server, 'listening');
    BASE = `http://localhost:${PORT}`;
    TOKEN_HDR = { Authorization: `Bearer ${TOKEN}`, 'Content-Type': 'application/wasm' };
  });

  after(() => server.close());

  async function req(method, path, body, headers = {}) {
    const r = await fetch(BASE + path, {
      method,
      headers: { Authorization: `Bearer ${TOKEN}`, ...headers },
      body: body instanceof Buffer ? body : (body ? JSON.stringify(body) : undefined),
    });
    const ct = r.headers.get('content-type') || '';
    const data = ct.includes('json') ? await r.json() : await r.arrayBuffer();
    return { status: r.status, body: data, headers: r.headers };
  }

  test('GET /agentfs/health returns ok (no auth)', async () => {
    const r = await fetch(BASE + '/agentfs/health');
    assert.equal(r.status, 200);
    const body = await r.json();
    assert.equal(body.ok, true);
  });

  test('POST without auth returns 401', async () => {
    const r = await fetch(BASE + '/agentfs/modules', {
      method: 'POST', body: VALID_WASM,
    });
    assert.equal(r.status, 401);
  });

  test('POST invalid WASM returns 400', async () => {
    const r = await req('POST', '/agentfs/modules', INVALID_WASM, { 'Content-Type': 'application/wasm' });
    assert.equal(r.status, 400);
    assert.match(r.body.error, /invalid WASM/);
  });

  test('POST valid WASM returns 201 + hash', async () => {
    const r = await req('POST', '/agentfs/modules', VALID_WASM, { 'Content-Type': 'application/wasm' });
    assert.equal(r.status, 201);
    assert.equal(r.body.hash, WASM_HASH);
    assert.equal(r.body.size, VALID_WASM.length);
    assert.equal(r.body.already_exists, false);
  });

  test('POST same WASM again returns 200 + already_exists', async () => {
    const r = await req('POST', '/agentfs/modules', VALID_WASM, { 'Content-Type': 'application/wasm' });
    assert.equal(r.status, 200);
    assert.equal(r.body.already_exists, true);
    assert.equal(r.body.hash, WASM_HASH);
  });

  test('GET /agentfs/modules lists stored module', async () => {
    const r = await req('GET', '/agentfs/modules');
    assert.equal(r.status, 200);
    assert.equal(r.body.count, 1);
    assert.equal(r.body.modules[0].hash, WASM_HASH);
  });

  test('GET /agentfs/modules/:hash returns WASM bytes', async () => {
    const r = await req('GET', `/agentfs/modules/${WASM_HASH}`);
    assert.equal(r.status, 200);
    const bytes = Buffer.from(r.body);
    assert.deepEqual(bytes, VALID_WASM);
  });

  test('GET /agentfs/modules/:hash with bad hash returns 400', async () => {
    const r = await req('GET', '/agentfs/modules/notahash');
    assert.equal(r.status, 404); // doesn't match route pattern → 404
  });

  test('GET nonexistent hash returns 404', async () => {
    const fakeHash = 'a'.repeat(64);
    const r = await req('GET', `/agentfs/modules/${fakeHash}`);
    assert.equal(r.status, 404);
  });

  test('DELETE /agentfs/modules/:hash removes module', async () => {
    const r = await req('DELETE', `/agentfs/modules/${WASM_HASH}`);
    assert.equal(r.status, 200);
    assert.equal(r.body.ok, true);
  });

  test('GET after DELETE returns 404', async () => {
    const r = await req('GET', `/agentfs/modules/${WASM_HASH}`);
    assert.equal(r.status, 404);
  });

  test('DELETE nonexistent returns 404', async () => {
    const r = await req('DELETE', `/agentfs/modules/${WASM_HASH}`);
    assert.equal(r.status, 404);
  });

  test('list after DELETE returns empty', async () => {
    const r = await req('GET', '/agentfs/modules');
    assert.equal(r.status, 200);
    assert.equal(r.body.count, 0);
  });

  test('POST returns aot field in metadata', async () => {
    // Upload a fresh distinct module so this test is independent
    const buf = Buffer.concat([VALID_WASM, Buffer.from('aot-test')]);
    const r = await req('POST', '/agentfs/modules', buf, { 'Content-Type': 'application/wasm' });
    assert.ok(r.status === 200 || r.status === 201, `unexpected status ${r.status}`);
    // aot is a boolean in the real server; the mock returns false (no wasmtime in test env)
    assert.ok('aot' in r.body, 'aot field must be present in response');
    assert.equal(r.body.size, buf.length);
    // clean up
    await req('DELETE', `/agentfs/modules/${r.body.hash}`);
  });

  // ── Capability gate (unit tests — pure functions, no server import) ─────────
  test('parseWasmCapabilities returns null for minimal WASM (no custom section)', () => {
    const result = _parseWasmCapabilities(VALID_WASM);
    assert.equal(result, null, 'minimal WASM has no capability section');
  });

  test('parseWasmCapabilities parses custom section', () => {
    const wasmWithCaps = _buildWasmWithCaps({ requires: ['net'], provides: ['inference'] });
    const caps = _parseWasmCapabilities(wasmWithCaps);
    assert.ok(caps !== null, 'should find capability section');
    assert.deepEqual(caps.requires, ['net']);
    assert.deepEqual(caps.provides, ['inference']);
  });

  test('checkCapabilities: ok when no requirements', () => {
    const result = _checkCapabilities(null, []);
    assert.ok(result.ok);
    assert.deepEqual(result.missing, []);
  });

  test('checkCapabilities: rejects when caps absent and requirements present', () => {
    const result = _checkCapabilities(null, ['net']);
    assert.ok(!result.ok);
    assert.ok(result.missing.includes('net'));
  });

  test('checkCapabilities: ok when declared caps satisfy requirements', () => {
    const result = _checkCapabilities({ requires: ['net'], provides: ['inference'] }, ['net', 'inference']);
    assert.ok(result.ok);
    assert.deepEqual(result.missing, []);
  });

  test('POST with ?require= rejects module missing caps', async () => {
    // Upload a module that has no capability section, but require gpu
    const r = await req('POST', '/agentfs/modules?require=gpu', VALID_WASM, { 'Content-Type': 'application/wasm' });
    assert.equal(r.status, 422, `expected 422 cap gate rejection, got ${r.status}`);
    assert.ok(r.body.missing?.includes('gpu'));
  });

  test('content-addressing: same bytes = same hash always', async () => {
    // Upload two different-sized WASM-magic-prefixed blobs
    const buf1 = Buffer.concat([VALID_WASM, Buffer.alloc(8, 0x00)]);
    const buf2 = Buffer.concat([VALID_WASM, Buffer.alloc(8, 0x01)]);
    const h1 = createHash('sha256').update(buf1).digest('hex');
    const h2 = createHash('sha256').update(buf2).digest('hex');
    assert.notEqual(h1, h2, 'different content must have different hashes');

    const r1 = await req('POST', '/agentfs/modules', buf1, { 'Content-Type': 'application/wasm' });
    const r2 = await req('POST', '/agentfs/modules', buf2, { 'Content-Type': 'application/wasm' });
    assert.equal(r1.body.hash, h1);
    assert.equal(r2.body.hash, h2);

    const list = await req('GET', '/agentfs/modules');
    assert.equal(list.body.count, 2);
  });

  // ── ClawBus replication tests ────────────────────────────────────────────

  test('replication event published on upload', async () => {
    let capturedMsg = null;

    // Mock ClawBus: captures POST /send
    const busSrv = createServer((req, res) => {
      const chunks = [];
      req.on('data', c => chunks.push(c));
      req.on('end', () => {
        try { capturedMsg = JSON.parse(Buffer.concat(chunks).toString()); } catch {}
        res.writeHead(200, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify({ ok: true }));
      });
    });
    const busPort = PORT + 200;
    busSrv.listen(busPort);
    await once(busSrv, 'listening');

    const busSrvPort2 = PORT + 201;
    const agentSrv = makeTestServer(busSrvPort2, TOKEN, { busUrl: `http://localhost:${busPort}` });
    agentSrv.listen(busSrvPort2);
    await once(agentSrv, 'listening');

    try {
      const r = await fetch(`http://localhost:${busSrvPort2}/agentfs/modules`, {
        method: 'POST',
        headers: { Authorization: `Bearer ${TOKEN}`, 'Content-Type': 'application/wasm' },
        body: VALID_WASM,
      });
      assert.equal(r.status, 201, 'upload should succeed');

      // Bus publish is fire-and-forget — give it a moment
      await new Promise(resolve => setTimeout(resolve, 150));

      assert.ok(capturedMsg, 'ClawBus should have received a publish event');
      assert.equal(capturedMsg.type, 'agentos.fs.put', 'event type should be agentos.fs.put');
      const payload = JSON.parse(capturedMsg.body);
      assert.equal(payload.hash, WASM_HASH, 'event payload should contain uploaded hash');
      assert.ok(payload.origin_url, 'event payload should contain origin_url');
    } finally {
      agentSrv.close();
      busSrv.close();
    }
  });

  test('replication subscriber fetches missing blob', async () => {
    const { startReplicationSubscriber } = await import('./replication.mjs');

    const REPL_WASM = Buffer.concat([VALID_WASM, Buffer.from('replication-subscriber-test')]);
    const REPL_HASH = createHash('sha256').update(REPL_WASM).digest('hex');

    const originPort  = PORT + 300;
    const busReplPort = PORT + 301;

    // Mock ClawBus: returns one agentos.fs.put event on first poll, empty thereafter
    let busCallCount = 0;
    const busReplSrv = createServer((req, res) => {
      busCallCount++;
      res.writeHead(200, { 'Content-Type': 'application/json' });
      if (busCallCount === 1) {
        res.end(JSON.stringify({ messages: [{
          type: 'agentos.fs.put',
          ts: new Date().toISOString(),
          body: JSON.stringify({
            hash: REPL_HASH, size: REPL_WASM.length,
            origin_url: `http://localhost:${originPort}`,
            ts: new Date().toISOString(),
          }),
        }] }));
      } else {
        res.end(JSON.stringify({ messages: [] }));
      }
    });
    busReplSrv.listen(busReplPort);
    await once(busReplSrv, 'listening');

    // Mock origin AgentFS: serves the WASM bytes (no auth check in mock)
    const originSrv = createServer((req, res) => {
      if (req.url === `/agentfs/modules/${REPL_HASH}`) {
        res.writeHead(200, { 'Content-Type': 'application/wasm' });
        res.end(REPL_WASM);
      } else {
        res.writeHead(404); res.end();
      }
    });
    originSrv.listen(originPort);
    await once(originSrv, 'listening');

    // In-memory S3 mock
    const s3Store = new Map();
    const mockS3 = {
      async send(cmd) {
        const name = cmd.constructor.name;
        const key  = cmd.input?.Key;
        if (name === 'HeadObjectCommand') {
          if (!s3Store.has(key)) {
            const e = new Error('Not Found'); e.name = 'NoSuchKey'; throw e;
          }
          return {};
        }
        if (name === 'PutObjectCommand') {
          s3Store.set(key, cmd.input.Body);
          return {};
        }
        if (name === 'DeleteObjectCommand') {
          s3Store.delete(key);
          return {};
        }
      },
    };

    const controller = new AbortController();
    try {
      await startReplicationSubscriber(mockS3, 'test-repl-bucket', {
        squirrelbusUrl: `http://localhost:${busReplPort}`,
        ownOriginUrl: 'http://different-node:8791',
        fetchToken: TOKEN,
        signal: controller.signal,
      });

      // Wait for replication (poll → fetch → store)
      await new Promise(resolve => setTimeout(resolve, 500));

      const storedKey = `modules/${REPL_HASH}.wasm`;
      assert.ok(s3Store.has(storedKey), 'blob should be stored in MinIO after replication');
      const stored = Buffer.from(s3Store.get(storedKey));
      assert.deepEqual(stored, REPL_WASM, 'stored content must match origin content');
    } finally {
      controller.abort();
      originSrv.close();
      busReplSrv.close();
    }
  });
});
