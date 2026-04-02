/**
 * rcc/vector/index.mjs — Milvus Vector Search Integration
 *
 * Provides semantic search over:
 *   - Lessons learned (by symptom + fix text)
 *   - Work queue items (by description + title)
 *   - Agent memory / context snippets (arbitrary text)
 *
 * Embedding model: NVIDIA NIM text-embedding-3-large (1536-dim)
 * Vector DB: Milvus standalone on localhost:19530
 *
 * Collections:
 *   - rcc_lessons  — lessons learned, keyed by lesson id
 *   - rcc_queue    — work queue items, keyed by item id
 *   - rcc_memory   — arbitrary agent memory snippets
 *
 * Usage:
 *   import { vectorSearch, vectorUpsert, vectorDelete, ensureCollections } from '../vector/index.mjs';
 *   await ensureCollections();
 *   await vectorUpsert('rcc_lessons', lesson.id, lesson.symptom + ' ' + lesson.fix, { ...meta });
 *   const hits = await vectorSearch('rcc_lessons', 'how to handle rate limits', 5);
 */

import { MilvusClient, DataType } from '@zilliz/milvus2-sdk-node';
import { createHash } from 'crypto';

// ── Config ─────────────────────────────────────────────────────────────────
const MILVUS_ADDRESS  = process.env.MILVUS_ADDRESS || 'localhost:19530';
const EMBED_API_URL   = process.env.NVIDIA_EMBED_URL || 'http://localhost:8090/v1/embeddings';
const EMBED_API_KEY   = process.env.TOKENHUB_API_KEY || process.env.NVIDIA_API_KEY || process.env.OPENAI_API_KEY || '';
const EMBED_MODEL     = process.env.EMBED_MODEL || 'text-embedding-3-large';
const EMBED_DIM       = parseInt(process.env.EMBED_DIM || '3072', 10);
// Note: azure/openai/text-embedding-3-large via NVIDIA gateway = 3072-dim vectors
const AGENT_NAME      = process.env.AGENT_NAME || 'rocky';

// ── Local embedding backend (ollama) ───────────────────────────────────────
// Set EMBED_BACKEND=local to use ollama nomic-embed-text (768-dim) on sparky.
// This avoids network latency for high-volume local memory ingest.
// Cross-agent queries should still use the default Azure 3072-dim backend.
const EMBED_BACKEND       = process.env.EMBED_BACKEND || 'remote';  // 'local' | 'remote'
const OLLAMA_BASE_URL     = process.env.OLLAMA_BASE_URL || 'http://localhost:11434';
const OLLAMA_EMBED_MODEL  = process.env.OLLAMA_EMBED_MODEL || 'nomic-embed-text';
const LOCAL_EMBED_DIM     = parseInt(process.env.LOCAL_EMBED_DIM || '768', 10);

// The effective embedding dimension depends on backend
const effectiveDim = EMBED_BACKEND === 'local' ? LOCAL_EMBED_DIM : EMBED_DIM;

// Sparky-local collection: uses 768-dim nomic-embed-text vectors
const LOCAL_COLLECTION = 'rcc_memory_sparky';

// NV-Embed-v2 outputs 4096-dim. If using text-embedding-3-large, set dim=1536.
// Default to nv-embed-v2 for NVIDIA infra.
const COLLECTION_CONFIGS = {
  rcc_lessons: {
    description: 'Lessons learned — semantic symptom+fix search',
    fields: [
      { name: 'id',     data_type: DataType.VarChar, is_primary_key: true, max_length: 64 },
      { name: 'vector', data_type: DataType.FloatVector, dim: EMBED_DIM },
      { name: 'agent',  data_type: DataType.VarChar, max_length: 32 },
      { name: 'domain', data_type: DataType.VarChar, max_length: 64 },
      { name: 'tags',   data_type: DataType.VarChar, max_length: 256 },
      { name: 'symptom',data_type: DataType.VarChar, max_length: 1024 },
      { name: 'fix',    data_type: DataType.VarChar, max_length: 2048 },
      { name: 'score',  data_type: DataType.Int32 },
      { name: 'ts',     data_type: DataType.VarChar, max_length: 32 },
    ],
  },
  rcc_queue: {
    description: 'Work queue items — semantic task search',
    fields: [
      { name: 'id',          data_type: DataType.VarChar, is_primary_key: true, max_length: 64 },
      { name: 'vector',      data_type: DataType.FloatVector, dim: EMBED_DIM },
      { name: 'title',       data_type: DataType.VarChar, max_length: 256 },
      { name: 'description', data_type: DataType.VarChar, max_length: 2048 },
      { name: 'status',      data_type: DataType.VarChar, max_length: 32 },
      { name: 'priority',    data_type: DataType.VarChar, max_length: 16 },
      { name: 'tags',        data_type: DataType.VarChar, max_length: 256 },
      { name: 'ts',          data_type: DataType.VarChar, max_length: 32 },
    ],
  },
  rcc_memory: {
    description: 'Agent memory snippets — semantic recall (3072-dim, cross-agent)',
    fields: [
      { name: 'id',      data_type: DataType.VarChar, is_primary_key: true, max_length: 128 },
      { name: 'vector',  data_type: DataType.FloatVector, dim: EMBED_DIM },
      { name: 'agent',   data_type: DataType.VarChar, max_length: 32 },
      { name: 'content', data_type: DataType.VarChar, max_length: 4096 },
      { name: 'source',  data_type: DataType.VarChar, max_length: 256 },
      { name: 'ts',      data_type: DataType.VarChar, max_length: 32 },
    ],
  },
  // Channel-scoped memory with platform+workspace+channel filtering
  // NOT for cross-agent queries — use rcc_memory for those.
  // Sparky-local: uses 768-dim nomic-embed-text via ollama for zero-latency GPU ingest.
  // High-volume writes: daily memory files, ClawBus messages, queue items.
  // NOT for cross-agent queries — use rcc_memory for those.
  rcc_memory_sparky: {
    description: 'Sparky-local memory (768-dim, nomic-embed-text, GPU-accelerated)',
    fields: [
      { name: 'id',      data_type: DataType.VarChar, is_primary_key: true, max_length: 128 },
      { name: 'vector',  data_type: DataType.FloatVector, dim: LOCAL_EMBED_DIM },
      { name: 'agent',   data_type: DataType.VarChar, max_length: 32 },
      { name: 'content', data_type: DataType.VarChar, max_length: 4096 },
      { name: 'source',  data_type: DataType.VarChar, max_length: 256 },
      { name: 'ts',      data_type: DataType.VarChar, max_length: 32 },
    ],
  },
  // Channel-scoped memory: platform/workspace/channel/user/conv filtered recall
  rcc_channel_memory: {
    description: 'Channel-scoped agent memory — filtered by platform+workspace+channel for contextual recall',
    fields: [
      { name: 'id',           data_type: DataType.VarChar, is_primary_key: true, max_length: 128 },
      { name: 'vector',       data_type: DataType.FloatVector, dim: EMBED_DIM },
      { name: 'agent',        data_type: DataType.VarChar, max_length: 32 },
      { name: 'content',      data_type: DataType.VarChar, max_length: 4096 },
      { name: 'source',       data_type: DataType.VarChar, max_length: 256 },
      { name: 'platform',     data_type: DataType.VarChar, max_length: 32 },
      { name: 'workspace_id', data_type: DataType.VarChar, max_length: 128 },
      { name: 'channel_id',   data_type: DataType.VarChar, max_length: 128 },
      { name: 'user_id',      data_type: DataType.VarChar, max_length: 128 },
      { name: 'conv_id',      data_type: DataType.VarChar, max_length: 128 },
      { name: 'ts',           data_type: DataType.VarChar, max_length: 32 },
    ],
  },
};

// ── Client singleton ────────────────────────────────────────────────────────
let _client = null;

function getClient() {
  if (!_client) {
    _client = new MilvusClient({ address: MILVUS_ADDRESS });
  }
  return _client;
}

// ── Embedding ───────────────────────────────────────────────────────────────

/**
 * Get a single embedding via ollama (nomic-embed-text, 768-dim).
 * Used when EMBED_BACKEND=local or for rcc_memory_sparky collection.
 */
async function embedLocal(text) {
  const resp = await fetch(`${OLLAMA_BASE_URL}/api/embed`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ model: OLLAMA_EMBED_MODEL, input: text.slice(0, 8000) }),
  });
  if (!resp.ok) {
    const err = await resp.text().catch(() => '');
    throw new Error(`[vector] ollama embed error ${resp.status}: ${err.slice(0, 200)}`);
  }
  const data = await resp.json();
  return data.embeddings[0];
}

/**
 * Batch embed via ollama. ollama /api/embed accepts arrays natively — use real batching.
 * Adaptive batch size: starts at OLLAMA_BATCH_SIZE (default 64), backs off on error.
 */

// Adaptive batch state (module-level, survives across calls in a single process)
let _batchSize = parseInt(process.env.OLLAMA_BATCH_SIZE || '64', 10);
let _lastBatchMs = null;  // latency of last successful batch (ms)

async function embedBatchLocal(texts) {
  if (!texts.length) return [];
  const results = [];
  let i = 0;
  while (i < texts.length) {
    const chunk = texts.slice(i, i + _batchSize);
    const t0 = Date.now();
    try {
      const resp = await fetch(`${OLLAMA_BASE_URL}/api/embed`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ model: OLLAMA_EMBED_MODEL, input: chunk }),
      });
      if (!resp.ok) throw new Error(`ollama batch embed ${resp.status}`);
      const data = await resp.json();
      const embeddings = data.embeddings;
      if (!Array.isArray(embeddings) || embeddings.length !== chunk.length) {
        throw new Error(`ollama returned ${embeddings?.length} embeddings for ${chunk.length} inputs`);
      }
      _lastBatchMs = Date.now() - t0;
      // Adaptive scale-up: if latency < 500ms, try a larger batch next time (cap at 512)
      if (_lastBatchMs < 500 && _batchSize < 512) {
        _batchSize = Math.min(512, Math.round(_batchSize * 1.5));
      }
      results.push(...embeddings);
      i += chunk.length;
    } catch (err) {
      // Back off batch size on error, retry with smaller chunks
      if (_batchSize > 8) {
        _batchSize = Math.max(8, Math.round(_batchSize / 2));
        console.warn(`[vector] ollama batch error — backing off to batch_size=${_batchSize}: ${err.message}`);
        // Don't advance i — retry same chunk with smaller batch
      } else {
        // At minimum batch size — fall back to single embeds for this chunk
        console.warn(`[vector] ollama single-embed fallback for chunk at i=${i}: ${err.message}`);
        for (const text of chunk) {
          results.push(await embedLocal(text));
        }
        i += chunk.length;
      }
    }
  }
  return results;
}

/**
 * Get embeddings from NVIDIA NIM or compatible OpenAI endpoint.
 * Returns float[] of length EMBED_DIM.
 */
async function embedRemote(text) {
  if (!EMBED_API_KEY) {
    throw new Error('[vector] TOKENHUB_API_KEY not set — cannot generate embeddings');
  }

  const resp = await fetch(EMBED_API_URL, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      'Authorization': `Bearer ${EMBED_API_KEY}`,
    },
    body: JSON.stringify({
      model: EMBED_MODEL,
      input: [text.slice(0, 8000)], // NIM max input length
      encoding_format: 'float',
    }),
  });

  if (!resp.ok) {
    const err = await resp.text().catch(() => '');
    throw new Error(`[vector] Embedding API error ${resp.status}: ${err.slice(0, 200)}`);
  }

  const data = await resp.json();
  return data.data[0].embedding;
}

/**
 * Batch embed multiple texts via remote API. Returns array of float[] vectors.
 * Respects NIM batch limits (chunks of 16).
 */
async function embedBatchRemote(texts, batchSize = 16) {
  if (!EMBED_API_KEY) throw new Error('[vector] TOKENHUB_API_KEY not set');
  const results = [];
  for (let i = 0; i < texts.length; i += batchSize) {
    const chunk = texts.slice(i, i + batchSize);
    const resp = await fetch(EMBED_API_URL, {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        'Authorization': `Bearer ${EMBED_API_KEY}`,
      },
      body: JSON.stringify({
        model: EMBED_MODEL,
        input: chunk.map(t => t.slice(0, 8000)),
        encoding_format: 'float',
      }),
    });
    if (!resp.ok) throw new Error(`[vector] Batch embed error ${resp.status}`);
    const data = await resp.json();
    results.push(...data.data.map(d => d.embedding));
  }
  return results;
}

/**
 * Embed text using the configured backend (local=ollama, remote=NVIDIA NIM).
 * Returns float[] of appropriate dimension (768 local, 3072 remote).
 */
export async function embed(text, backend = EMBED_BACKEND) {
  return backend === 'local' ? embedLocal(text) : embedRemote(text);
}

/**
 * Batch embed multiple texts. Routes to local (ollama) or remote (NIM) backend.
 */
export async function embedBatch(texts, batchSize = 16) {
  return EMBED_BACKEND === 'local'
    ? embedBatchLocal(texts)
    : embedBatchRemote(texts, batchSize);
}

/**
 * Embed for a specific collection — auto-selects backend based on collection name.
 * rcc_memory_sparky always uses local (768-dim); all others use the configured backend.
 */
async function embedForCollection(text, collection) {
  return collection === LOCAL_COLLECTION ? embedLocal(text) : embed(text);
}

// ── Collection management ────────────────────────────────────────────────────

/**
 * Ensure all RCC collections exist with correct schema + HNSW index.
 * Safe to call on every startup.
 */
export async function ensureCollections() {
  const client = getClient();

  for (const [name, cfg] of Object.entries(COLLECTION_CONFIGS)) {
    const exists = await client.hasCollection({ collection_name: name });
    if (!exists.value) {
      await client.createCollection({
        collection_name: name,
        description: cfg.description,
        fields: cfg.fields,
        enable_dynamic_field: true,
      });

      // Create HNSW index on the vector field
      await client.createIndex({
        collection_name: name,
        field_name: 'vector',
        index_type: 'HNSW',
        metric_type: 'COSINE',
        params: { M: 16, efConstruction: 256 },
      });

      console.log(`[vector] Created collection: ${name}`);
    }

    // Always load collection into memory for fast search
    await client.loadCollection({ collection_name: name }).catch(() => {});
  }
}

// ── Upsert ──────────────────────────────────────────────────────────────────

/**
 * Upsert a document into a Milvus collection.
 *
 * @param {string} collection - collection name (rcc_lessons|rcc_queue|rcc_memory)
 * @param {string} id - unique document id (primary key)
 * @param {string} text - text to embed
 * @param {object} meta - additional scalar fields to store (must match collection schema)
 */
export async function vectorUpsert(collection, id, text, meta = {}) {
  const client = getClient();
  const vector = await embedForCollection(text, collection);

  // Milvus v2 SDK: upsert is insert with overwrite (or use delete+insert)
  try {
    await client.delete({ collection_name: collection, filter: `id == "${id}"` });
  } catch { /* ok if not found */ }

  await client.insert({
    collection_name: collection,
    data: [{ id, vector, ...meta }],
  });
}

/**
 * Batch upsert. Embeds all texts in one batch call, then inserts.
 * @param {string} collection
 * @param {Array<{id, text, meta}>} items
 */
export async function vectorUpsertBatch(collection, items) {
  if (!items.length) return;
  const client = getClient();
  const texts = items.map(i => i.text);
  // Route batch embedding to local (ollama) for sparky-local collection
  const vectors = collection === LOCAL_COLLECTION
    ? await embedBatchLocal(texts)
    : await embedBatch(texts);

  // Delete existing ids
  const ids = items.map(i => `"${i.id}"`).join(', ');
  try {
    await client.delete({ collection_name: collection, filter: `id in [${ids}]` });
  } catch { /* ok */ }

  await client.insert({
    collection_name: collection,
    data: items.map((item, idx) => ({
      id: item.id,
      vector: vectors[idx],
      ...item.meta,
    })),
  });
}

// ── Search ───────────────────────────────────────────────────────────────────

/**
 * Semantic search in a collection.
 *
 * @param {string} collection - collection name
 * @param {string} query - natural language query
 * @param {number} [limit=5] - top K results
 * @param {string} [filter] - optional Milvus boolean expression filter, e.g. 'agent == "rocky"'
 * @returns {Array<{id, score, ...fields}>}
 */
export async function vectorSearch(collection, query, limit = 5, filter = '') {
  const client = getClient();
  const queryVec = await embedForCollection(query, collection);

  const cfg = COLLECTION_CONFIGS[collection];
  if (!cfg) throw new Error(`[vector] Unknown collection: ${collection}`);

  // Output fields = all non-vector fields
  const outputFields = cfg.fields
    .filter(f => f.name !== 'vector')
    .map(f => f.name);

  const searchParams = {
    collection_name: collection,
    data: [queryVec],
    anns_field: 'vector',
    limit,
    output_fields: outputFields,
    params: { ef: 64 },
    metric_type: 'COSINE',
  };
  if (filter) searchParams.filter = filter;

  const res = await client.search(searchParams);

  return (res.results || []).map(r => ({
    id: r.id,
    score: r.score,
    ...Object.fromEntries(outputFields.map(f => [f, r[f]])),
  }));
}

// ── Delete ───────────────────────────────────────────────────────────────────

/**
 * Delete a document by id.
 */
export async function vectorDelete(collection, id) {
  const client = getClient();
  await client.delete({ collection_name: collection, filter: `id == "${id}"` });
}

// ── Lesson helpers ───────────────────────────────────────────────────────────

/**
 * Index a lesson into Milvus.
 * Text = symptom + fix (what you search for = what the lesson is about)
 */
export async function indexLesson(lesson) {
  const text = `${lesson.symptom || ''} ${lesson.fix || ''} ${(lesson.tags || []).join(' ')}`;
  await vectorUpsert('rcc_lessons', lesson.id, text, {
    agent:   (lesson.agent  || 'unknown').slice(0, 32),
    domain:  (lesson.domain || 'unknown').slice(0, 64),
    tags:    (lesson.tags   || []).join(',').slice(0, 256),
    symptom: (lesson.symptom || '').slice(0, 1024),
    fix:     (lesson.fix     || '').slice(0, 2048),
    score:   lesson.score   || 1,
    ts:      (lesson.ts     || new Date().toISOString()).slice(0, 32),
  });
}

/**
 * Semantic lesson search across all domains.
 * Returns sorted by cosine similarity (best match first).
 */
export async function searchLessons(query, limit = 5, filter = '') {
  return vectorSearch('rcc_lessons', query, limit, filter);
}

// ── Queue item helpers ────────────────────────────────────────────────────────

/**
 * Index a work queue item.
 */
export async function indexQueueItem(item) {
  const text = `${item.title || ''} ${item.description || ''} ${(item.tags || []).join(' ')}`;
  await vectorUpsert('rcc_queue', item.id, text, {
    title:       (item.title       || '').slice(0, 256),
    description: (item.description || '').slice(0, 2048),
    status:      (item.status      || 'pending').slice(0, 32),
    priority:    (item.priority    || 'medium').slice(0, 16),
    tags:        (item.tags        || []).join(',').slice(0, 256),
    ts:          (item.createdAt   || item.ts || new Date().toISOString()).slice(0, 32),
  });
}

/**
 * Semantic work queue search. Great for "find tasks similar to X" or dedup.
 */
export async function searchQueue(query, limit = 5, filter = '') {
  return vectorSearch('rcc_queue', query, limit, filter);
}

// ── Memory helpers ────────────────────────────────────────────────────────────

/**
 * Store an agent memory snippet for later recall.
 * Uses the cross-agent rcc_memory collection (3072-dim, remote embed).
 * id = hash of content (dedup by content).
 */
export async function rememberSnippet(content, source = '', agent = AGENT_NAME) {
  const id = createHash('sha256').update(content).digest('hex').slice(0, 32);
  await vectorUpsert('rcc_memory', id, content, {
    agent:   agent.slice(0, 32),
    content: content.slice(0, 4096),
    source:  source.slice(0, 256),
    ts:      new Date().toISOString().slice(0, 32),
  });
  return id;
}

/**
 * Store a memory snippet into the Sparky-local collection (768-dim, GPU, no API key needed).
 * Use for high-volume local ingest on sparky: daily notes, session context, etc.
 * id = hash of content (dedup by content).
 */
export async function rememberSnippetLocal(content, source = '', agent = AGENT_NAME) {
  const id = createHash('sha256').update(content).digest('hex').slice(0, 32);
  await vectorUpsert(LOCAL_COLLECTION, id, content, {
    agent:   agent.slice(0, 32),
    content: content.slice(0, 4096),
    source:  source.slice(0, 256),
    ts:      new Date().toISOString().slice(0, 32),
  });
  return id;
}

/**
 * Recall relevant memory snippets for a query.
 * Uses cross-agent rcc_memory (3072-dim).
 */
export async function recallMemory(query, limit = 5, agentFilter = '') {
  const filter = agentFilter ? `agent == "${agentFilter}"` : '';
  return vectorSearch('rcc_memory', query, limit, filter);
}

/**
 * Recall from Sparky-local memory (768-dim, GPU embed, no API key).
 * Suitable for local-only recall on sparky.
 */
export async function recallMemoryLocal(query, limit = 5, agentFilter = '') {
  const filter = agentFilter ? `agent == "${agentFilter}"` : '';
  return vectorSearch(LOCAL_COLLECTION, query, limit, filter);
}

// ── Health check ──────────────────────────────────────────────────────────────

export async function vectorHealth() {
  try {
    const client = getClient();
    const health = await client.checkHealth();

    // Check ollama local embed health
    let localOk = false;
    let localModel = null;
    try {
      const ollamaResp = await fetch(`${OLLAMA_BASE_URL}/api/tags`);
      if (ollamaResp.ok) {
        const tags = await ollamaResp.json();
        localModel = (tags.models || []).find(m => m.name.startsWith(OLLAMA_EMBED_MODEL));
        localOk = !!localModel;
      }
    } catch { /* ollama not running */ }

    return {
      ok: health.isHealthy,
      address: MILVUS_ADDRESS,
      remote: { model: EMBED_MODEL, dim: EMBED_DIM },
      local: {
        ok: localOk,
        backend: EMBED_BACKEND,
        model: OLLAMA_EMBED_MODEL,
        dim: LOCAL_EMBED_DIM,
        url: OLLAMA_BASE_URL,
      },
    };
  } catch (err) {
    return { ok: false, error: err.message };
  }
}

export async function collectionStats() { return {}; }

// Search across all known collections, merge and sort by score
export async function searchAll(query, { k = 10 } = {}) {
  const collections = ['rcc_lessons', 'rcc_queue', 'rcc_memory'];
  const results = await Promise.all(
    collections.map(async col => {
      try {
        const hits = await vectorSearch(col, query, k);
        return hits.map(r => ({ collection: col, ...r }));
      } catch {
        return [];
      }
    })
  );
  return results.flat().sort((a, b) => (b.score ?? 0) - (a.score ?? 0)).slice(0, k);
}

// ── Aliases for rcc/api/index.mjs imports ──────────────────────────────────
export { vectorUpsert as upsert, vectorSearch as search, searchAll as vectorSearchAll };


// ── Channel-scoped memory ──────────────────────────────────────────────────

/**
 * Ingest a memory snippet scoped to a platform/workspace/channel/user/conversation.
 * Used for contextual recall filtered by chat scope.
 *
 * @param {string} id       - Unique id for this memory entry
 * @param {string} content  - Text content to embed and store
 * @param {object} scope    - {platform, workspace_id, channel_id, user_id, conv_id, agent, source}
 */
export async function channelMemoryIngest(id, content, scope = {}) {
  const {
    platform = '', workspace_id = '', channel_id = '',
    user_id = '', conv_id = '', agent = AGENT_NAME, source = 'ingest',
  } = scope;
  await vectorUpsert('rcc_channel_memory', id, content, {
    agent, content: content.slice(0, 4096), source: source.slice(0, 256),
    platform: platform.slice(0, 32),
    workspace_id: workspace_id.slice(0, 128),
    channel_id: channel_id.slice(0, 128),
    user_id: user_id.slice(0, 128),
    conv_id: conv_id.slice(0, 128),
    ts: new Date().toISOString().slice(0, 32),
  });
}

/**
 * Two-phase channel-scoped memory recall:
 *   Phase 1: Read ~/.openclaw/memory/*.md as recency cache (file-based, fast)
 *   Phase 2: Milvus vector search on rcc_channel_memory filtered by scope
 *
 * @param {string} query    - Semantic search query
 * @param {object} scope    - {platform, workspace_id, channel_id, user_id}
 * @param {object} opts     - {k: number (default 8), recencyLines: number (default 200)}
 * @returns {object}        - {recency: string[], semantic: object[]}
 */
export async function channelMemoryRecall(query, scope = {}, opts = {}) {
  const { platform = '', workspace_id = '', channel_id = '' } = scope;
  const { k = 8, recencyLines = 200 } = opts;

  // Phase 1: recency cache from ~/.openclaw/memory/*.md
  let recency = [];
  try {
    const { readdir, readFile } = await import('fs/promises');
    const { join, homedir } = await import('path');
    const { default: os } = await import('os');
    const memDir = join(os.homedir(), '.openclaw', 'workspace', 'memory');
    const files = (await readdir(memDir).catch(() => []))
      .filter(f => f.endsWith('.md'))
      .sort()
      .reverse()
      .slice(0, 5); // last 5 daily files
    const lines = [];
    for (const f of files) {
      const text = await readFile(join(memDir, f), 'utf8').catch(() => '');
      lines.push(...text.split('\n'));
      if (lines.length >= recencyLines) break;
    }
    // Simple keyword filter for relevance
    const qWords = query.toLowerCase().split(/\s+/).filter(w => w.length > 3);
    recency = lines
      .filter(l => l.trim().length > 20)
      .filter(l => qWords.some(w => l.toLowerCase().includes(w)))
      .slice(0, 20);
  } catch (_) { /* memory dir not available */ }

  // Phase 2: Milvus vector search filtered by scope
  let semantic = [];
  try {
    const filters = [];
    if (platform)     filters.push(`platform == "${platform.replace(/"/g, '')}"`);
    if (workspace_id) filters.push(`workspace_id == "${workspace_id.replace(/"/g, '')}"`);
    if (channel_id)   filters.push(`channel_id == "${channel_id.replace(/"/g, '')}"`);

    const filter = filters.length ? filters.join(' && ') : undefined;
    semantic = await vectorSearch('rcc_channel_memory', query, k, filter);
  } catch (_) { /* Milvus may not have data yet */ }

  return { recency, semantic };
}

