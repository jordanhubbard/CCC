#!/usr/bin/env node
/**
 * rcc/scripts/advertise-llms.mjs — Advertise local LLMs to CCC
 *
 * Queries the local ollama instance for running models, then registers
 * them as a first-class LLM endpoint on CCC so other agents can discover
 * and use them.
 *
 * Usage:
 *   node advertise-llms.mjs
 *   node advertise-llms.mjs --backend vllm --base-url http://localhost:8000/v1
 *   node advertise-llms.mjs --watch           # re-advertise every 5 minutes
 *
 * Environment:
 *   CCC_URL          — CCC base URL (default: http://localhost:8789)
 *   CCC_AGENT_TOKEN  — Agent bearer token
 *   AGENT_NAME       — This agent's name (default: hostname)
 *   OLLAMA_URL       — Ollama base URL (default: http://localhost:11434)
 */

import { readFile, existsSync } from 'fs';
import { promisify } from 'util';
import { hostname } from 'os';

const readFileProm = promisify(readFile);

// ── Config ──────────────────────────────────────────────────────────────────

function loadEnv() {
  const envPath = `${process.env.HOME}/.rcc/.env`;
  if (existsSync(envPath)) {
    const lines = require('fs').readFileSync(envPath, 'utf8').split('\n');
    for (const line of lines) {
      const [k, ...rest] = line.split('=');
      if (k && rest.length && !process.env[k.trim()]) {
        process.env[k.trim()] = rest.join('=').trim();
      }
    }
  }
}

try { loadEnv(); } catch {}

const CCC_URL       = process.env.CCC_URL        || 'http://localhost:8789';
const RCC_TOKEN     = process.env.CCC_AGENT_TOKEN || '';
const AGENT_NAME    = process.env.AGENT_NAME      || hostname();
const OLLAMA_URL    = process.env.OLLAMA_URL      || 'http://localhost:11434';

const args = process.argv.slice(2);
const BACKEND      = args.includes('--backend')   ? args[args.indexOf('--backend') + 1]   : 'ollama';
const CUSTOM_URL   = args.includes('--base-url')  ? args[args.indexOf('--base-url') + 1]  : null;
const WATCH_MODE   = args.includes('--watch');
const WATCH_MS     = parseInt(args[args.indexOf('--interval') + 1] || '300') * 1000;
const VERBOSE      = args.includes('--verbose') || args.includes('-v');

const BASE_URL     = CUSTOM_URL || (BACKEND === 'ollama' ? `${OLLAMA_URL}/v1` : 'http://localhost:8000/v1');

// ── Model type inference ────────────────────────────────────────────────────

const EMBED_PATTERNS    = /embed|nomic|e5-|bge-|gte-|minilm|instructor/i;
const VISION_PATTERNS   = /vision|llava|bakllava|moondream|minicpm-v/i;
const RERANK_PATTERNS   = /rerank|cross-encoder/i;

function inferModelType(name) {
  if (RERANK_PATTERNS.test(name)) return 'rerank';
  if (EMBED_PATTERNS.test(name))  return 'embedding';
  if (VISION_PATTERNS.test(name)) return 'vision';
  return 'chat';
}

const EMBED_DIMS = {
  'nomic-embed-text':  768,
  'all-minilm':        384,
  'mxbai-embed-large': 1024,
  'bge-large-en':      1024,
  'bge-base-en':       768,
  'e5-large':          1024,
  'snowflake-arctic-embed': 1024,
};

function inferDims(name) {
  for (const [k, v] of Object.entries(EMBED_DIMS)) {
    if (name.startsWith(k)) return v;
  }
  return null;
}

// ── Tags ────────────────────────────────────────────────────────────────────

function inferTags(name) {
  const tags = ['local', 'private'];
  if (/70b|72b|65b|mixtral/i.test(name)) tags.push('large');
  else if (/7b|8b|13b|mistral/i.test(name)) tags.push('medium');
  else if (/1b|2b|3b|tiny|small/i.test(name)) tags.push('small', 'fast');
  if (/q4|q5|gguf/i.test(name)) tags.push('quantized');
  if (/fp16|bf16/i.test(name)) tags.push('full-precision');
  return tags;
}

// ── Ollama helpers ──────────────────────────────────────────────────────────

async function getOllamaModels() {
  try {
    const resp = await fetch(`${OLLAMA_URL}/api/tags`);
    if (!resp.ok) throw new Error(`ollama /api/tags returned ${resp.status}`);
    const data = await resp.json();
    return (data.models || []).map(m => {
      const name = m.name.replace(/:latest$/, '');
      const type = inferModelType(name);
      return {
        name,
        aliases: [m.name],
        type,
        contextLen:   null,  // ollama doesn't expose this in /api/tags
        dims:         type === 'embedding' ? inferDims(name) : null,
        quantization: m.details?.quantization_level || null,
        tags:         inferTags(name),
        vram_gb:      m.size ? Math.round(m.size / (1024 ** 3) * 10) / 10 : null,
      };
    });
  } catch (err) {
    console.warn(`[advertise-llms] Could not query ollama: ${err.message}`);
    return [];
  }
}

// ── Advertise ───────────────────────────────────────────────────────────────

async function advertise() {
  let models = [];

  if (BACKEND === 'ollama') {
    models = await getOllamaModels();
  } else {
    // Generic: try OpenAI /models endpoint
    try {
      const resp = await fetch(`${BASE_URL}/models`);
      if (resp.ok) {
        const data = await resp.json();
        models = (data.data || []).map(m => ({
          name:    m.id,
          aliases: [],
          type:    inferModelType(m.id),
          contextLen: null,
          dims:    inferModelType(m.id) === 'embedding' ? inferDims(m.id) : null,
          quantization: null,
          tags:    inferTags(m.id),
          vram_gb: null,
        }));
      }
    } catch (err) {
      console.warn(`[advertise-llms] Could not query models from ${BASE_URL}: ${err.message}`);
    }
  }

  if (models.length === 0) {
    console.log('[advertise-llms] No models found — skipping advertisement');
    return false;
  }

  if (VERBOSE) {
    console.log(`[advertise-llms] Advertising ${models.length} model(s) from ${AGENT_NAME}:`);
    for (const m of models) {
      console.log(`  ${m.type.padEnd(10)} ${m.name}${m.dims ? ` (${m.dims}d)` : ''}`);
    }
  }

  const payload = {
    agent:   AGENT_NAME,
    host:    hostname(),
    baseUrl: BASE_URL,
    models,
    backend: BACKEND,
    status:  'online',
  };

  const resp = await fetch(`${CCC_URL}/api/llms`, {
    method:  'POST',
    headers: {
      'Content-Type':  'application/json',
      'Authorization': `Bearer ${RCC_TOKEN}`,
    },
    body: JSON.stringify(payload),
  });

  const result = await resp.json().catch(() => ({}));
  if (!resp.ok) {
    console.error(`[advertise-llms] Failed: ${result.error || resp.status}`);
    return false;
  }

  console.log(`[advertise-llms] ✅ ${AGENT_NAME} advertising ${models.length} model(s) to CCC`);
  return true;
}

// ── Main ─────────────────────────────────────────────────────────────────────

await advertise();

if (WATCH_MODE) {
  console.log(`[advertise-llms] Watch mode: re-advertising every ${WATCH_MS / 1000}s`);
  setInterval(advertise, WATCH_MS);
}
