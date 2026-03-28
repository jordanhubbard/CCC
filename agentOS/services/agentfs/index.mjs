/**
 * agentOS AgentFS — content-addressed WASM module store
 *
 * REST API:
 *   POST   /agentfs/modules          upload WASM blob → returns {hash, size, url}
 *   GET    /agentfs/modules           list all stored modules
 *   GET    /agentfs/modules/:hash     fetch WASM bytes (streams from MinIO)
 *   DELETE /agentfs/modules/:hash     remove a module
 *   GET    /agentfs/health            service health
 *
 * Backend: MinIO (S3-compatible), bucket agentfs-modules
 * Auth:    Bearer token (AGENTFS_TOKEN env var)
 * Runs on: sparky, served over Tailscale for fleet access
 *
 * Content addressing: SHA-256 of raw WASM bytes = module identity.
 * Modules are immutable once stored (hash collision = same content).
 *
 * WASM validation: checks magic bytes + version before accepting.
 * Wasmtime AOT pre-compilation: on upload, compiles to .cwasm native artifact
 * and stores it alongside the .wasm in MinIO. GET ?aot=1 returns the .cwasm.
 * Eliminates JIT latency from vibe-swap hot-load cycles (<100ms swap target).
 */

import { createServer } from 'http';
import { createHash } from 'crypto';
import { execFile } from 'child_process';
import { promisify } from 'util';
import { tmpdir } from 'os';
import { writeFile, unlink, readFile } from 'fs/promises';
import { join } from 'path';
import {
  S3Client,
  PutObjectCommand,
  GetObjectCommand,
  DeleteObjectCommand,
  ListObjectsV2Command,
  HeadObjectCommand,
  CreateBucketCommand,
  HeadBucketCommand,
} from '@aws-sdk/client-s3';

const execFileAsync = promisify(execFile);

// ── Config ────────────────────────────────────────────────────────────────────
const PORT          = parseInt(process.env.AGENTFS_PORT   || '8791', 10);
const AUTH_TOKEN    = process.env.AGENTFS_TOKEN           || 'agentfs-dev-token';
const MINIO_EP      = process.env.MINIO_ENDPOINT          || 'http://100.89.199.14:9000';
const MINIO_KEY     = process.env.MINIO_ACCESS_KEY        || 'rocky2197fb96dde4618aa17f';
const MINIO_SECRET  = process.env.MINIO_SECRET_KEY        || 'e47696ac5fcd998be6f342bbc47d13bf5f2fcaebae0ba3e1';
const BUCKET        = process.env.AGENTFS_BUCKET          || 'agentfs-modules';
const MAX_SIZE      = parseInt(process.env.AGENTFS_MAX_MB || '64', 10) * 1024 * 1024;
const WASMTIME      = process.env.WASMTIME_PATH           || `${process.env.HOME}/.local/bin/wasmtime`;
const AOT_ENABLED   = process.env.AGENTFS_AOT !== '0';   // default on; set AGENTFS_AOT=0 to disable

// WASM magic bytes: \0asm + version 1
const WASM_MAGIC = Buffer.from([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]);

// ── S3 client (MinIO) ─────────────────────────────────────────────────────────
const s3 = new S3Client({
  endpoint: MINIO_EP,
  region: 'us-east-1',
  credentials: { accessKeyId: MINIO_KEY, secretAccessKey: MINIO_SECRET },
  forcePathStyle: true,
});

// ── Ensure bucket exists ──────────────────────────────────────────────────────
async function ensureBucket() {
  try {
    await s3.send(new HeadBucketCommand({ Bucket: BUCKET }));
  } catch {
    await s3.send(new CreateBucketCommand({ Bucket: BUCKET }));
    console.log(`[agentfs] Created bucket: ${BUCKET}`);
  }
}

// ── WASM validation ───────────────────────────────────────────────────────────
function validateWasmMagic(buf) {
  if (buf.length < 8) return { ok: false, reason: 'too small (< 8 bytes)' };
  if (!buf.slice(0, 8).equals(WASM_MAGIC)) {
    return { ok: false, reason: `invalid magic bytes (got ${buf.slice(0,4).toString('hex')}, want 0061736d)` };
  }
  return { ok: true };
}

/**
 * AOT-compile a WASM module using wasmtime.
 * Returns { ok, aot, cwasm? } where cwasm is the compiled Buffer if successful.
 * Falls back gracefully if wasmtime is not installed.
 */
async function aotCompile(buf) {
  if (!AOT_ENABLED) return { ok: true, aot: false };

  const id = `${Date.now()}-${Math.random().toString(36).slice(2)}`;
  const tmpIn  = join(tmpdir(), `agentfs-${id}.wasm`);
  const tmpOut = join(tmpdir(), `agentfs-${id}.cwasm`);
  const t0 = Date.now();

  try {
    await writeFile(tmpIn, buf);
    await execFileAsync(WASMTIME, ['compile', tmpIn, '-o', tmpOut], { timeout: 30_000 });
    const cwasm = await readFile(tmpOut);
    const elapsed = Date.now() - t0;
    console.log(`[agentfs] AOT compiled ${buf.length}B → ${cwasm.length}B in ${elapsed}ms`);
    return { ok: true, aot: true, cwasm, aot_ms: elapsed };
  } catch (err) {
    if (err.code === 'ENOENT') {
      // wasmtime not on PATH — magic-byte validation is sufficient
      return { ok: true, aot: false };
    }
    // wasmtime rejected the module → structural validation failure
    return { ok: false, reason: err.stderr?.slice(0, 200) || err.message };
  } finally {
    unlink(tmpIn).catch(() => {});
    unlink(tmpOut).catch(() => {});
  }
}

// ── Helpers ───────────────────────────────────────────────────────────────────
function sha256(buf) {
  return createHash('sha256').update(buf).digest('hex');
}

function json(res, status, body) {
  const payload = JSON.stringify(body);
  res.writeHead(status, { 'Content-Type': 'application/json', 'Content-Length': Buffer.byteLength(payload) });
  res.end(payload);
}

function auth(req) {
  const header = req.headers['authorization'] || '';
  return header === `Bearer ${AUTH_TOKEN}`;
}

async function readBody(req, maxBytes = MAX_SIZE) {
  return new Promise((resolve, reject) => {
    const chunks = [];
    let total = 0;
    req.on('data', chunk => {
      total += chunk.length;
      if (total > maxBytes) {
        req.destroy();
        reject(Object.assign(new Error('payload too large'), { status: 413 }));
      } else {
        chunks.push(chunk);
      }
    });
    req.on('end', () => resolve(Buffer.concat(chunks)));
    req.on('error', reject);
  });
}

function moduleKey(hash) {
  return `modules/${hash}.wasm`;
}

function aotKey(hash) {
  return `modules/${hash}.cwasm`;
}

function metaKey(hash) {
  return `modules/${hash}.meta.json`;
}

// ── Route handlers ────────────────────────────────────────────────────────────

// POST /agentfs/modules — upload WASM blob
async function handleUpload(req, res) {
  const buf = await readBody(req);

  // Validate magic bytes
  const magic = validateWasmMagic(buf);
  if (!magic.ok) return json(res, 400, { error: `invalid WASM: ${magic.reason}` });

  // AOT compile (also validates structurally via wasmtime)
  const compiled = await aotCompile(buf);
  if (!compiled.ok) return json(res, 400, { error: `WASM validation failed: ${compiled.reason}` });

  const hash = sha256(buf);
  const key  = moduleKey(hash);
  const meta = metaKey(hash);
  const now  = new Date().toISOString();

  // Check if already stored (idempotent)
  try {
    await s3.send(new HeadObjectCommand({ Bucket: BUCKET, Key: key }));
    return json(res, 200, { hash, size: buf.length, already_exists: true, aot: compiled.aot });
  } catch { /* not found, proceed */ }

  // Store WASM + AOT artifact (in parallel)
  const stores = [
    s3.send(new PutObjectCommand({
      Bucket: BUCKET, Key: key, Body: buf,
      ContentType: 'application/wasm',
      ContentLength: buf.length,
      Metadata: { hash, size: String(buf.length), uploaded_at: now },
    })),
  ];

  if (compiled.aot && compiled.cwasm) {
    stores.push(s3.send(new PutObjectCommand({
      Bucket: BUCKET, Key: aotKey(hash), Body: compiled.cwasm,
      ContentType: 'application/octet-stream',
      ContentLength: compiled.cwasm.length,
      Metadata: { hash, wasm_size: String(buf.length), aot_ms: String(compiled.aot_ms || 0) },
    })));
  }

  await Promise.all(stores);

  // Store metadata sidecar
  const metaObj = {
    hash,
    size: buf.length,
    uploaded_at: now,
    aot: compiled.aot,
    aot_size: compiled.cwasm?.length ?? null,
    aot_ms: compiled.aot_ms ?? null,
  };
  await s3.send(new PutObjectCommand({
    Bucket: BUCKET, Key: meta,
    Body: JSON.stringify(metaObj),
    ContentType: 'application/json',
  }));

  console.log(`[agentfs] stored ${hash} (${buf.length}B wasm, aot=${compiled.aot}${compiled.cwasm ? ` ${compiled.cwasm.length}B cwasm` : ''})`);
  json(res, 201, metaObj);
}

// GET /agentfs/modules — list all modules
async function handleList(req, res) {
  const result = await s3.send(new ListObjectsV2Command({
    Bucket: BUCKET, Prefix: 'modules/', Delimiter: '/',
  }));
  const objects = (result.Contents || []).filter(o => o.Key.endsWith('.meta.json'));
  const modules = await Promise.all(objects.map(async o => {
    try {
      const r = await s3.send(new GetObjectCommand({ Bucket: BUCKET, Key: o.Key }));
      const body = await r.Body.transformToString();
      return JSON.parse(body);
    } catch { return null; }
  }));
  json(res, 200, { modules: modules.filter(Boolean), count: modules.filter(Boolean).length });
}

// GET /agentfs/modules/:hash[?aot=1] — fetch WASM bytes or AOT-compiled .cwasm
async function handleFetch(req, res, hash, searchParams) {
  if (!/^[0-9a-f]{64}$/.test(hash)) return json(res, 400, { error: 'invalid hash format' });

  const wantAot = searchParams.get('aot') === '1';
  const key = wantAot ? aotKey(hash) : moduleKey(hash);
  const contentType = wantAot ? 'application/octet-stream' : 'application/wasm';

  try {
    const r = await s3.send(new GetObjectCommand({ Bucket: BUCKET, Key: key }));
    res.writeHead(200, {
      'Content-Type': contentType,
      'X-AgentFS-Hash': hash,
      'X-AgentFS-AOT': wantAot ? '1' : '0',
      'Cache-Control': 'public, max-age=31536000, immutable',
    });
    r.Body.pipe(res);
  } catch (err) {
    if (err.name === 'NoSuchKey') {
      if (wantAot) return json(res, 404, { error: 'AOT artifact not found for this module — was it uploaded after AOT support was added?', hash });
      return json(res, 404, { error: 'module not found', hash });
    }
    throw err;
  }
}

// DELETE /agentfs/modules/:hash — remove module + meta
async function handleDelete(req, res, hash) {
  if (!/^[0-9a-f]{64}$/.test(hash)) return json(res, 400, { error: 'invalid hash format' });
  try {
    await s3.send(new DeleteObjectCommand({ Bucket: BUCKET, Key: moduleKey(hash) }));
    await s3.send(new DeleteObjectCommand({ Bucket: BUCKET, Key: metaKey(hash) })).catch(() => {});
    console.log(`[agentfs] deleted ${hash}`);
    json(res, 200, { ok: true, hash });
  } catch (err) {
    if (err.name === 'NoSuchKey') return json(res, 404, { error: 'module not found', hash });
    throw err;
  }
}

// ── Router ────────────────────────────────────────────────────────────────────
async function router(req, res) {
  const url = new URL(req.url, `http://localhost`);
  const path = url.pathname;

  // Health (no auth)
  if (path === '/agentfs/health' && req.method === 'GET') {
    // Check wasmtime availability
    let wasmtimeVersion = null;
    try {
      const { stdout } = await execFileAsync(WASMTIME, ['--version'], { timeout: 3000 });
      wasmtimeVersion = stdout.trim();
    } catch { /* not available */ }
    return json(res, 200, {
      ok: true, service: 'agentfs', bucket: BUCKET, minio: MINIO_EP,
      aot: { enabled: AOT_ENABLED, wasmtime: wasmtimeVersion },
    });
  }

  // Auth gate
  if (!auth(req)) return json(res, 401, { error: 'unauthorized' });

  try {
    // POST /agentfs/modules
    if (path === '/agentfs/modules' && req.method === 'POST') {
      return await handleUpload(req, res);
    }
    // GET /agentfs/modules (list)
    if (path === '/agentfs/modules' && req.method === 'GET') {
      return await handleList(req, res);
    }
    // GET /agentfs/modules/:hash[?aot=1]
    const fetchMatch = path.match(/^\/agentfs\/modules\/([0-9a-f]{64})$/);
    if (fetchMatch && req.method === 'GET') {
      return await handleFetch(req, res, fetchMatch[1], url.searchParams);
    }
    // DELETE /agentfs/modules/:hash
    const deleteMatch = path.match(/^\/agentfs\/modules\/([0-9a-f]{64})$/);
    if (deleteMatch && req.method === 'DELETE') {
      return await handleDelete(req, res, deleteMatch[1]);
    }

    json(res, 404, { error: 'not found' });
  } catch (err) {
    const status = err.status || 500;
    console.error(`[agentfs] error ${req.method} ${path}:`, err.message);
    json(res, status, { error: err.message });
  }
}

// ── Start ─────────────────────────────────────────────────────────────────────
await ensureBucket();
const server = createServer(router);
server.listen(PORT, () => {
  console.log(`[agentfs] listening on :${PORT}`);
  console.log(`[agentfs] bucket=${BUCKET} minio=${MINIO_EP}`);
  console.log(`[agentfs] max_size=${MAX_SIZE / 1024 / 1024}MB`);
});

export { server };
