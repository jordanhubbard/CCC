/**
 * rcc/llm/registry.mjs — LLM Endpoint Registry
 *
 * Agents that serve LLMs (via ollama, llama.cpp, vLLM, or any OpenAI-compatible
 * API) advertise themselves here.  Other agents query this to find a peer LLM
 * instead of always hitting the NVIDIA gateway.
 *
 * Advertised model schema:
 *   {
 *     agent:       string          — agent name, e.g. "natasha", "sparky"
 *     host:        string          — hostname/IP
 *     baseUrl:     string          — OpenAI-compatible base URL, e.g. "http://sparky:11434/v1"
 *     models:      ModelEntry[]    — list of served models
 *     backend:     string          — "ollama" | "vllm" | "llama.cpp" | "openai" | "custom"
 *     status:      string          — "online" | "offline" | "loading"
 *     updatedAt:   ISO string
 *   }
 *
 * ModelEntry:
 *   {
 *     name:        string          — model id as served, e.g. "nomic-embed-text"
 *     aliases:     string[]        — common aliases, e.g. ["nomic-embed-text:latest"]
 *     type:        string          — "embedding" | "chat" | "completion" | "vision" | "rerank"
 *     contextLen:  number | null   — context window in tokens
 *     dims:        number | null   — embedding dimensions (type=embedding only)
 *     quantization: string | null  — e.g. "q4_0", "fp16", "bf16"
 *     tags:        string[]        — e.g. ["fast", "local", "private", "7b"]
 *     vram_gb:     number | null   — VRAM used by this model
 *   }
 *
 * Usage:
 *   import * as llmRegistry from './registry.mjs';
 *   llmRegistry.configure({ path: '/path/to/llm-registry.json' });
 *   await llmRegistry.load();
 *   llmRegistry.advertise({ agent: 'natasha', baseUrl: '...', models: [...], backend: 'ollama' });
 *   llmRegistry.findModel('nomic-embed-text');  // → [{ agent, baseUrl, model }, ...]
 *   llmRegistry.best({ type: 'chat', tag: 'fast' }); // → best endpoint or null
 */

import { readFile, writeFile, mkdir } from 'fs/promises';
import { existsSync } from 'fs';
import { dirname } from 'path';

// ── Constants ───────────────────────────────────────────────────────────────

export const VALID_BACKENDS = ['ollama', 'vllm', 'llama.cpp', 'openai', 'custom'];
export const VALID_STATUSES  = ['online', 'offline', 'loading'];
export const VALID_MODEL_TYPES = ['embedding', 'chat', 'completion', 'vision', 'rerank'];

// Stale threshold: if an agent hasn't re-advertised in 30 minutes, mark offline
const STALE_MS = 30 * 60 * 1000;

// ── In-memory store ─────────────────────────────────────────────────────────

const _store = new Map();   // agent name → LLMEndpoint
let   _path  = null;

// ── Configuration ────────────────────────────────────────────────────────────

export function configure(opts = {}) {
  if (opts.path) _path = opts.path;
}

// ── Validation ────────────────────────────────────────────────────────────────

export function validateAdvertisement(raw) {
  if (!raw || typeof raw !== 'object') throw new Error('advertisement must be an object');
  if (!raw.agent || typeof raw.agent !== 'string') throw new Error('agent (string) required');
  if (!raw.baseUrl || typeof raw.baseUrl !== 'string') throw new Error('baseUrl (string) required');
  if (!Array.isArray(raw.models) || raw.models.length === 0) throw new Error('models (non-empty array) required');

  for (const m of raw.models) {
    if (!m.name || typeof m.name !== 'string') throw new Error('each model must have a name');
    if (m.type && !VALID_MODEL_TYPES.includes(m.type)) {
      throw new Error(`invalid model type: ${m.type}. Valid: ${VALID_MODEL_TYPES.join(', ')}`);
    }
  }

  if (raw.backend && !VALID_BACKENDS.includes(raw.backend)) {
    throw new Error(`invalid backend: ${raw.backend}. Valid: ${VALID_BACKENDS.join(', ')}`);
  }
  if (raw.status && !VALID_STATUSES.includes(raw.status)) {
    throw new Error(`invalid status: ${raw.status}. Valid: ${VALID_STATUSES.join(', ')}`);
  }
  return true;
}

// ── Normalization ─────────────────────────────────────────────────────────────

function normalizeModel(m) {
  return {
    name:         m.name,
    aliases:      Array.isArray(m.aliases) ? [...m.aliases] : [],
    type:         m.type || 'chat',
    contextLen:   m.contextLen ?? null,
    dims:         m.dims ?? null,
    quantization: m.quantization ?? null,
    tags:         Array.isArray(m.tags) ? [...m.tags] : [],
    vram_gb:      m.vram_gb ?? null,
  };
}

function normalize(raw) {
  return {
    agent:     raw.agent,
    host:      raw.host || 'unknown',
    baseUrl:   raw.baseUrl,
    models:    (raw.models || []).map(normalizeModel),
    backend:   raw.backend || 'custom',
    status:    raw.status || 'online',
    updatedAt: new Date().toISOString(),
  };
}

// ── Persistence ───────────────────────────────────────────────────────────────

export async function load(path) {
  const p = path || _path;
  if (!p || !existsSync(p)) return;
  try {
    const raw = JSON.parse(await readFile(p, 'utf8'));
    for (const [name, entry] of Object.entries(raw)) {
      _store.set(name, entry);
    }
    console.log(`[llm-registry] Loaded ${_store.size} LLM endpoint(s) from disk`);
  } catch (err) {
    console.warn(`[llm-registry] Failed to load: ${err.message}`);
  }
}

async function persist() {
  if (!_path) return;
  try {
    await mkdir(dirname(_path), { recursive: true });
    await writeFile(_path, JSON.stringify(Object.fromEntries(_store), null, 2));
  } catch (err) {
    console.warn('[llm-registry] Failed to persist:', err.message);
  }
}

// ── Public API ────────────────────────────────────────────────────────────────

/**
 * Advertise (upsert) an agent's LLM endpoint.
 * Always refreshes updatedAt.
 */
export function advertise(raw) {
  validateAdvertisement(raw);
  const entry = normalize(raw);
  _store.set(entry.agent, entry);
  persist().catch(() => {});
  return entry;
}

/**
 * Get a single agent's LLM endpoint, or null.
 */
export function get(agent) {
  return _store.get(agent) ?? null;
}

/**
 * List all registered LLM endpoints, optionally filtering by freshness.
 * @param {object} [opts]
 * @param {boolean} [opts.onlyFresh=false] — exclude stale (>30min) entries
 * @param {string} [opts.status] — filter by status
 */
export function list(opts = {}) {
  const all = [..._store.values()];
  let results = all;
  if (opts.onlyFresh) {
    const cutoff = Date.now() - STALE_MS;
    results = results.filter(e => new Date(e.updatedAt).getTime() > cutoff);
  }
  if (opts.status) {
    results = results.filter(e => e.status === opts.status);
  }
  return results.map(e => ({
    ...e,
    fresh: (Date.now() - new Date(e.updatedAt).getTime()) < STALE_MS,
  }));
}

/**
 * Find all endpoints that serve a given model name (or alias).
 * Returns a flat list of { agent, baseUrl, backend, model } objects.
 *
 * @param {string} modelName
 * @param {object} [opts]
 * @param {boolean} [opts.onlyFresh=true]
 */
export function findModel(modelName, opts = { onlyFresh: true }) {
  const endpoints = opts.onlyFresh
    ? list({ onlyFresh: true })
    : list();

  const results = [];
  for (const ep of endpoints) {
    for (const m of ep.models) {
      const names = [m.name, ...m.aliases];
      if (names.some(n => n === modelName || n.startsWith(modelName + ':'))) {
        results.push({
          agent:   ep.agent,
          host:    ep.host,
          baseUrl: ep.baseUrl,
          backend: ep.backend,
          status:  ep.status,
          fresh:   ep.fresh,
          model:   m,
          updatedAt: ep.updatedAt,
        });
      }
    }
  }
  return results;
}

/**
 * Find the best endpoint for a given criteria.
 *
 * Selection strategy:
 *   1. Must be fresh (updated < 30min ago)
 *   2. Filter by model type if provided
 *   3. Filter by tag if provided
 *   4. Prefer highest-VRAM endpoint (more capable)
 *   5. If no VRAM info, prefer most recently updated
 *
 * @param {object} opts
 * @param {string} [opts.type]      — model type: "chat" | "embedding" | ...
 * @param {string} [opts.tag]       — model tag: "fast" | "local" | "large" | ...
 * @param {string} [opts.model]     — specific model name
 * @param {string} [opts.agent]     — prefer a specific agent
 */
export function best(opts = {}) {
  let candidates = [];

  if (opts.model) {
    candidates = findModel(opts.model, { onlyFresh: true });
  } else {
    // Explode all models across all fresh endpoints
    for (const ep of list({ onlyFresh: true })) {
      for (const m of ep.models) {
        if (opts.type && m.type !== opts.type) continue;
        if (opts.tag && !m.tags.includes(opts.tag)) continue;
        candidates.push({ agent: ep.agent, host: ep.host, baseUrl: ep.baseUrl, backend: ep.backend, model: m, updatedAt: ep.updatedAt });
      }
    }
  }

  if (candidates.length === 0) return null;

  // Prefer specific agent if requested
  if (opts.agent) {
    const preferred = candidates.filter(c => c.agent === opts.agent);
    if (preferred.length > 0) return preferred[0];
  }

  // Sort by VRAM (descending), then by recency
  candidates.sort((a, b) => {
    const va = a.model.vram_gb ?? 0;
    const vb = b.model.vram_gb ?? 0;
    if (vb !== va) return vb - va;
    return new Date(b.updatedAt).getTime() - new Date(a.updatedAt).getTime();
  });

  return candidates[0];
}

/**
 * Remove an agent's LLM advertisement.
 */
export function remove(agent) {
  const existed = _store.has(agent);
  if (existed) {
    _store.delete(agent);
    persist().catch(() => {});
  }
  return existed;
}

/**
 * Mark an agent's endpoint as offline (without removing it).
 */
export function markOffline(agent) {
  const entry = _store.get(agent);
  if (entry) {
    entry.status = 'offline';
    entry.updatedAt = new Date().toISOString();
    persist().catch(() => {});
  }
}

/**
 * Serialize the full registry for API responses.
 * Adds 'fresh' boolean and 'modelCount' convenience fields.
 */
export function serialize() {
  return list().map(e => ({
    ...e,
    modelCount: e.models.length,
    modelTypes: [...new Set(e.models.map(m => m.type))],
  }));
}
