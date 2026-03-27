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
const EMBED_API_URL   = process.env.NVIDIA_EMBED_URL || 'https://inference-api.nvidia.com/v1/embeddings';
const EMBED_API_KEY   = process.env.NVIDIA_API_KEY || process.env.OPENAI_API_KEY || '';
const EMBED_MODEL     = process.env.EMBED_MODEL || 'azure/openai/text-embedding-3-large';
const EMBED_DIM       = parseInt(process.env.EMBED_DIM || '3072', 10);
// Note: azure/openai/text-embedding-3-large via NVIDIA gateway = 3072-dim vectors
const AGENT_NAME      = process.env.AGENT_NAME || 'rocky';

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
    description: 'Agent memory snippets — semantic recall',
    fields: [
      { name: 'id',      data_type: DataType.VarChar, is_primary_key: true, max_length: 128 },
      { name: 'vector',  data_type: DataType.FloatVector, dim: EMBED_DIM },
      { name: 'agent',   data_type: DataType.VarChar, max_length: 32 },
      { name: 'content', data_type: DataType.VarChar, max_length: 4096 },
      { name: 'source',  data_type: DataType.VarChar, max_length: 256 },
      { name: 'ts',      data_type: DataType.VarChar, max_length: 32 },
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
 * Get embeddings from NVIDIA NIM or compatible OpenAI endpoint.
 * Returns float[] of length EMBED_DIM.
 */
export async function embed(text) {
  if (!EMBED_API_KEY) {
    throw new Error('[vector] NVIDIA_API_KEY not set — cannot generate embeddings');
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
 * Batch embed multiple texts. Returns array of float[] vectors.
 * Respects NIM batch limits (chunks of 16).
 */
export async function embedBatch(texts, batchSize = 16) {
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
  const vector = await embed(text);

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
  const vectors = await embedBatch(texts);

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
  const queryVec = await embed(query);

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
 * Recall relevant memory snippets for a query.
 */
export async function recallMemory(query, limit = 5, agentFilter = '') {
  const filter = agentFilter ? `agent == "${agentFilter}"` : '';
  return vectorSearch('rcc_memory', query, limit, filter);
}

// ── Health check ──────────────────────────────────────────────────────────────

export async function vectorHealth() {
  try {
    const client = getClient();
    const health = await client.checkHealth();
    return { ok: health.isHealthy, address: MILVUS_ADDRESS, model: EMBED_MODEL, dim: EMBED_DIM };
  } catch (err) {
    return { ok: false, error: err.message };
  }
}
