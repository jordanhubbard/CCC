/**
 * rcc/llm/client.mjs — Peer LLM Client
 *
 * Looks up the best available LLM endpoint via CCC, then proxies
 * the call as an OpenAI-compatible request.
 *
 * Usage:
 *   import { PeerLLMClient } from './client.mjs';
 *
 *   const client = new PeerLLMClient({
 *     rccUrl:   'http://do-host1:8789',
 *     rccToken: process.env.CCC_AGENT_TOKEN,
 *   });
 *
 *   // Chat
 *   const reply = await client.chat({
 *     model: 'llama3.2',         // name as advertised — registry resolves agent+url
 *     messages: [{ role: 'user', content: 'hello' }],
 *   });
 *
 *   // Embeddings
 *   const { embedding } = await client.embed({
 *     model: 'nomic-embed-text',
 *     input: 'hello world',
 *   });
 *
 *   // Auto-select best chat model on the fleet
 *   const reply = await client.chat({
 *     messages: [{ role: 'user', content: 'hello' }],
 *     // no model specified → picks best from registry
 *   });
 */

// ── PeerLLMClient ────────────────────────────────────────────────────────────

export class PeerLLMClient {
  /**
   * @param {object} opts
   * @param {string} opts.rccUrl      — CCC base URL, e.g. http://do-host1:8789
   * @param {string} opts.rccToken    — Agent bearer token for CCC
   * @param {number} [opts.timeoutMs] — Per-request timeout (default 60s)
   */
  constructor({ rccUrl, rccToken, timeoutMs = 60_000 } = {}) {
    this.rccUrl   = (rccUrl   || process.env.CCC_URL    || 'http://localhost:8789').replace(/\/$/, '');
    this.rccToken = rccToken  || process.env.CCC_AGENT_TOKEN || '';
    this.timeoutMs = timeoutMs;
    // TokenHub fallback — used when no peer endpoint is available in the fleet registry
    this._tokenhubUrl = (process.env.TOKENHUB_URL || '').replace(/\/$/, '');
    this._tokenhubKey = process.env.TOKENHUB_AGENT_KEY || '';
    this._cache = null;
    this._cacheTs = 0;
    this._cacheTtl = 60_000; // refresh LLM list every 60s
  }

  // ── Private helpers ─────────────────────────────────────────────────────

  async _rccFetch(path, opts = {}) {
    const url = `${this.rccUrl}${path}`;
    const headers = {
      'Content-Type': 'application/json',
      'Authorization': `Bearer ${this.rccToken}`,
      ...opts.headers,
    };
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), this.timeoutMs);
    try {
      const resp = await fetch(url, { ...opts, headers, signal: controller.signal });
      const text = await resp.text();
      try { return JSON.parse(text); } catch { return text; }
    } finally {
      clearTimeout(timer);
    }
  }

  async _getLLMs() {
    const now = Date.now();
    if (this._cache && (now - this._cacheTs) < this._cacheTtl) return this._cache;
    const result = await this._rccFetch('/api/llms?fresh=1');
    this._cache = Array.isArray(result) ? result : (result.endpoints || []);
    this._cacheTs = now;
    return this._cache;
  }

  async _resolve(modelName, type = 'chat') {
    // Ask CCC fleet registry for best peer endpoint
    try {
      const qs = modelName
        ? `model=${encodeURIComponent(modelName)}&type=${type}`
        : `type=${type}`;
      const result = await this._rccFetch(`/api/llms/best?${qs}`);
      if (!result.error) return result; // { agent, baseUrl, model, ... }
    } catch { /* fall through to tokenhub */ }

    // Fallback: route through TokenHub (aggregates local vLLM + NVIDIA NIM)
    if (this._tokenhubUrl) {
      return {
        agent:   'tokenhub',
        baseUrl: `${this._tokenhubUrl}/v1`,
        model:   { name: modelName || (type === 'embedding' ? 'text-embedding-3-large' : 'nemotron') },
        _tokenhubKey: this._tokenhubKey,
      };
    }

    throw new Error(`No LLM available for type=${type} model=${modelName || 'any'} — fleet empty and no tokenhub configured`);
  }

  async _peerFetch(baseUrl, path, body, extraHeaders = {}) {
    const url = `${baseUrl.replace(/\/$/, '')}${path}`;
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), this.timeoutMs);
    try {
      const resp = await fetch(url, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', ...extraHeaders },
        body: JSON.stringify(body),
        signal: controller.signal,
      });
      const text = await resp.text();
      let data;
      try { data = JSON.parse(text); } catch { throw new Error(`Non-JSON response: ${text.slice(0, 200)}`); }
      if (!resp.ok) throw new Error(data.error?.message || data.error || `HTTP ${resp.status}`);
      return data;
    } finally {
      clearTimeout(timer);
    }
  }

  // ── Public API ──────────────────────────────────────────────────────────

  /**
   * Chat completion via peer LLM.
   *
   * @param {object} opts
   * @param {string}  [opts.model]       — model name (auto-selects if omitted)
   * @param {Array}   opts.messages      — OpenAI message array
   * @param {number}  [opts.maxTokens]   — max tokens
   * @param {number}  [opts.temperature]
   * @param {string}  [opts.agent]       — prefer a specific agent
   * @returns {Promise<{text: string, usage: object, endpoint: object}>}
   */
  async chat({ model, messages, maxTokens = 1024, temperature = 0.7, agent, ...rest } = {}) {
    const endpoint = await this._resolve(model, 'chat');
    const resolvedModel = model || endpoint.model?.name;
    const authHeaders = endpoint._tokenhubKey ? { Authorization: `Bearer ${endpoint._tokenhubKey}` } : {};

    const response = await this._peerFetch(endpoint.baseUrl, '/chat/completions', {
      model:       resolvedModel,
      messages,
      max_tokens:  maxTokens,
      temperature,
      ...rest,
    }, authHeaders);

    return {
      text:     response.choices?.[0]?.message?.content ?? '',
      usage:    response.usage ?? null,
      raw:      response,
      endpoint: { agent: endpoint.agent, baseUrl: endpoint.baseUrl, model: resolvedModel },
    };
  }

  /**
   * Text embedding via peer LLM.
   *
   * @param {object} opts
   * @param {string}  [opts.model]    — model name (auto-selects embedding model if omitted)
   * @param {string|string[]} opts.input — text(s) to embed
   * @param {string}  [opts.agent]    — prefer a specific agent
   * @returns {Promise<{embedding: number[], embeddings: number[][], usage: object, endpoint: object}>}
   */
  async embed({ model, input, agent } = {}) {
    const endpoint = await this._resolve(model, 'embedding');
    const resolvedModel = model || endpoint.model?.name;
    const authHeaders = endpoint._tokenhubKey ? { Authorization: `Bearer ${endpoint._tokenhubKey}` } : {};

    const response = await this._peerFetch(endpoint.baseUrl, '/embeddings', {
      model: resolvedModel,
      input,
    }, authHeaders);

    const data = response.data || [];
    const embeddings = data.map(d => d.embedding);

    return {
      embedding:  embeddings[0] ?? [],
      embeddings,
      usage:      response.usage ?? null,
      raw:        response,
      endpoint:   { agent: endpoint.agent, baseUrl: endpoint.baseUrl, model: resolvedModel },
    };
  }

  /**
   * List all available LLM endpoints in the fleet.
   */
  async listEndpoints() {
    return this._rccFetch('/api/llms');
  }

  /**
   * List all unique model names available across the fleet.
   * @param {string} [type] — filter by type: 'chat', 'embedding', etc.
   */
  async listModels(type) {
    const endpoints = await this._getLLMs();
    const seen = new Set();
    const models = [];
    for (const ep of endpoints) {
      for (const m of ep.models || []) {
        if (type && m.type !== type) continue;
        if (!seen.has(m.name)) {
          seen.add(m.name);
          models.push({ ...m, agent: ep.agent, baseUrl: ep.baseUrl, backend: ep.backend });
        }
      }
    }
    return models;
  }
}

// ── Convenience singleton ───────────────────────────────────────────────────
// Import as: import peerLLM from './client.mjs';
// Call peerLLM.chat(...) directly without newing up a client.

const peerLLM = new PeerLLMClient();
export default peerLLM;
