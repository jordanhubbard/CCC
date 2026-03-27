/**
 * rcc/capabilities/registry.mjs — Agent Capability Registry
 *
 * Each agent publishes a manifest describing what it can execute and what it's
 * good at.  The work pump uses this to route tasks to the best available agent.
 *
 * Manifest schema:
 *   {
 *     agent:     string            — unique agent name (e.g. "rocky")
 *     host:      string            — hostname or IP where the agent runs
 *     executors: string[]          — executor types this agent supports:
 *                                    "claude_cli" | "inference_key" | "gpu"
 *     gpuSpec:   object | null     — only present when "gpu" is in executors
 *       { model, vram_gb, count }
 *     skills:    string[]          — high-level task categories this agent handles
 *                                    e.g. ["code","review","debug","triage","ci"]
 *     status:    string            — "online" | "offline" | "busy"
 *     updatedAt: ISO string        — when the manifest was last published
 *   }
 *
 * Usage:
 *   import * as registry from './registry.mjs';
 *   registry.configure({ path: '/path/to/capabilities-registry.json' });
 *   await registry.load();
 *   registry.publish({ agent: 'rocky', executors: ['claude_cli'], ... });
 *   registry.findByExecutor('claude_cli');  // → [manifest, ...]
 */

import { readFile, writeFile, mkdir } from 'fs/promises';
import { existsSync } from 'fs';
import { dirname } from 'path';

// ── Constants ───────────────────────────────────────────────────────────────

export const VALID_EXECUTORS = ['claude_cli', 'inference_key', 'gpu'];
export const VALID_STATUSES  = ['online', 'offline', 'busy'];

// ── In-memory store ─────────────────────────────────────────────────────────

const _store = new Map();   // agent name → normalized manifest
let   _path  = null;        // persistence file path (set via configure())

// ── Configuration ───────────────────────────────────────────────────────────

/**
 * Set the file path used for persistence.
 * Call this before load() or publish() if you want disk persistence.
 */
export function configure(opts = {}) {
  if (opts.path) _path = opts.path;
}

// ── Schema validation ────────────────────────────────────────────────────────

/**
 * Validate a raw manifest object.
 * Throws with a descriptive message if invalid.
 */
export function validateManifest(raw) {
  if (!raw || typeof raw !== 'object') throw new Error('manifest must be an object');
  if (!raw.agent || typeof raw.agent !== 'string') throw new Error('agent (string) required');
  if (!Array.isArray(raw.executors)) throw new Error('executors (array) required');

  const invalid = raw.executors.filter(e => !VALID_EXECUTORS.includes(e));
  if (invalid.length) {
    throw new Error(`invalid executor(s): ${invalid.join(', ')}. Valid: ${VALID_EXECUTORS.join(', ')}`);
  }

  if (raw.gpuSpec !== null && raw.gpuSpec !== undefined && typeof raw.gpuSpec !== 'object') {
    throw new Error('gpuSpec must be an object or null');
  }

  if (raw.skills !== undefined && !Array.isArray(raw.skills)) {
    throw new Error('skills must be an array of strings');
  }

  if (raw.status !== undefined && !VALID_STATUSES.includes(raw.status)) {
    throw new Error(`invalid status: ${raw.status}. Valid: ${VALID_STATUSES.join(', ')}`);
  }

  return true;
}

// ── Normalization ────────────────────────────────────────────────────────────

function normalize(raw) {
  const hasGpu = (raw.executors || []).includes('gpu');
  return {
    agent:     raw.agent,
    host:      raw.host || 'unknown',
    executors: [...(raw.executors || [])],
    gpuSpec:   hasGpu ? {
      model:   raw.gpuSpec?.model   || null,
      vram_gb: raw.gpuSpec?.vram_gb || 0,
      count:   raw.gpuSpec?.count   || 1,
    } : null,
    skills:    Array.isArray(raw.skills) ? [...raw.skills] : [],
    status:    raw.status || 'online',
    updatedAt: new Date().toISOString(),
  };
}

// ── Persistence ──────────────────────────────────────────────────────────────

/**
 * Load persisted manifests from disk into the in-memory store.
 * Safe to call even if the file doesn't exist yet.
 */
export async function load(path) {
  const p = path || _path;
  if (!p || !existsSync(p)) return;
  try {
    const raw = JSON.parse(await readFile(p, 'utf8'));
    for (const [name, manifest] of Object.entries(raw)) {
      _store.set(name, manifest);
    }
    console.log(`[capabilities] Loaded ${_store.size} agent manifest(s) from disk`);
  } catch (err) {
    console.warn(`[capabilities] Failed to load registry: ${err.message}`);
  }
}

async function persist() {
  if (!_path) return;
  try {
    await mkdir(dirname(_path), { recursive: true });
    await writeFile(_path, JSON.stringify(Object.fromEntries(_store), null, 2));
  } catch (err) {
    console.warn('[capabilities] Failed to persist registry:', err.message);
  }
}

// ── Public API ───────────────────────────────────────────────────────────────

/**
 * Publish (upsert) an agent's capability manifest.
 * Always sets updatedAt to now.
 * @param {object} raw  Manifest — see schema at top of file
 * @returns {object}    Normalized, stored manifest
 */
export function publish(raw) {
  validateManifest(raw);
  const manifest = normalize(raw);
  _store.set(manifest.agent, manifest);
  persist().catch(() => {});
  return manifest;
}

/**
 * Get a single agent's manifest, or null if not registered.
 */
export function get(agent) {
  return _store.get(agent) ?? null;
}

/**
 * List all registered agent manifests.
 * @returns {object[]}
 */
export function list() {
  return [..._store.values()];
}

/**
 * Find all agents whose executors array includes the given executor type.
 * @param {string} executor  e.g. 'claude_cli', 'gpu', 'inference_key'
 * @returns {object[]}
 */
export function findByExecutor(executor) {
  return [..._store.values()].filter(m => m.executors.includes(executor));
}

/**
 * Find agents that have the given executor AND are considered recently active.
 *
 * "Recently active" = updatedAt is within maxAgeMs of now (default 15 min).
 * For real-time online/offline status, cross-reference with heartbeats.
 *
 * @param {string} executor
 * @param {number} [maxAgeMs=900000]  15 minutes
 * @returns {object[]}
 */
export function findRecentByExecutor(executor, maxAgeMs = 15 * 60 * 1000) {
  const cutoff = Date.now() - maxAgeMs;
  return [..._store.values()].filter(m =>
    m.executors.includes(executor) &&
    new Date(m.updatedAt).getTime() > cutoff
  );
}

/**
 * Remove an agent's manifest from the registry.
 * Returns true if it existed, false otherwise.
 */
export function remove(agent) {
  const existed = _store.has(agent);
  if (existed) {
    _store.delete(agent);
    persist().catch(() => {});
  }
  return existed;
}
