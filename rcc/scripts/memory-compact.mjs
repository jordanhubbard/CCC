#!/usr/bin/env node
/**
 * rcc/scripts/memory-compact.mjs — Nightly memory compaction
 *
 * 1. Fetches recent memory items (last 24h, up to 500) from rcc-server
 * 2. Groups by agent
 * 3. For agents with >50 items: summarizes via tokenhub chat completions
 * 4. POSTs summary back as a new fleet memory item
 *
 * Env vars:
 *   CCC_URL         (default: http://localhost:8789)
 *   TOKENHUB_URL    (default: http://127.0.0.1:8090)
 *   CCC_AGENT_TOKEN (required for auth)
 */

const CCC_URL      = process.env.CCC_URL      ?? 'http://localhost:8789';
const TOKENHUB_URL = process.env.TOKENHUB_URL ?? 'http://127.0.0.1:8090';
const TOKEN        = process.env.CCC_AGENT_TOKEN ?? '';

const COMPACT_THRESHOLD = 50;
const SUMMARY_MODELS = [
  'nvidia/nemotron-3-super-120b-instruct',
  'meta/llama-3.3-70b-instruct',
];

function authHeaders() {
  const h = { 'Content-Type': 'application/json' };
  if (TOKEN) h['Authorization'] = `Bearer ${TOKEN}`;
  return h;
}

async function fetchRecentMemory() {
  const since = new Date(Date.now() - 24 * 60 * 60 * 1000).toISOString();
  const url = `${CCC_URL}/api/memory/recent?limit=500&since=${encodeURIComponent(since)}`;
  console.log(`[memory-compact] Fetching recent memory: ${url}`);
  const res = await fetch(url, { headers: authHeaders() });
  if (!res.ok) throw new Error(`GET /api/memory/recent failed: ${res.status} ${await res.text()}`);
  return res.json();
}

async function summarize(agent, items) {
  const itemList = items
    .map((item, i) => `${i + 1}. [${item.agent ?? agent}] ${item.text ?? item.content ?? JSON.stringify(item)}`)
    .join('\n');

  const prompt = `Summarize these memory items into a compact but complete fleet knowledge summary. \
Preserve all key facts, decisions, and context. Remove duplicate/redundant entries. \
Output should be a dense paragraph per topic.\n\nMemory items for agent "${agent}":\n${itemList}`;

  for (const model of SUMMARY_MODELS) {
    try {
      console.log(`[memory-compact] Summarizing ${items.length} items for agent "${agent}" using model ${model}`);
      const res = await fetch(`${TOKENHUB_URL}/v1/chat/completions`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          model,
          messages: [{ role: 'user', content: prompt }],
          max_tokens: 1024,
          temperature: 0.3,
        }),
      });
      if (!res.ok) {
        const body = await res.text();
        console.warn(`[memory-compact] Model ${model} failed: ${res.status} ${body}`);
        continue;
      }
      const data = await res.json();
      const summary = data.choices?.[0]?.message?.content?.trim();
      if (!summary) throw new Error('Empty summary from model');
      console.log(`[memory-compact] Summary for "${agent}": ${summary.slice(0, 120)}...`);
      return summary;
    } catch (err) {
      console.warn(`[memory-compact] Error with model ${model}: ${err.message}`);
    }
  }
  throw new Error(`All summarization models failed for agent "${agent}"`);
}

async function ingestSummary(agent, summary) {
  const body = {
    text: summary,
    agent: 'rocky',
    source: 'memory-compact',
    scope: 'fleet',
    metadata: { compacted_agent: agent, compacted_at: new Date().toISOString() },
  };
  const res = await fetch(`${CCC_URL}/api/memory/ingest`, {
    method: 'POST',
    headers: authHeaders(),
    body: JSON.stringify(body),
  });
  if (!res.ok) throw new Error(`POST /api/memory/ingest failed: ${res.status} ${await res.text()}`);
  const result = await res.json();
  console.log(`[memory-compact] Ingested summary for agent "${agent}": id=${result.id ?? result.ids?.[0] ?? '?'}`);
  return result;
}

async function main() {
  console.log(`[memory-compact] Starting — ${new Date().toISOString()}`);

  const items = await fetchRecentMemory();
  const list = Array.isArray(items) ? items : (items.items ?? items.results ?? []);
  console.log(`[memory-compact] Fetched ${list.length} items`);

  if (list.length === 0) {
    console.log('[memory-compact] No items — nothing to compact.');
    return;
  }

  // Group by agent
  const byAgent = {};
  for (const item of list) {
    const agent = item.agent ?? 'unknown';
    (byAgent[agent] ??= []).push(item);
  }

  console.log(`[memory-compact] Agents: ${Object.keys(byAgent).map(a => `${a}(${byAgent[a].length})`).join(', ')}`);

  let compacted = 0;
  for (const [agent, agentItems] of Object.entries(byAgent)) {
    if (agentItems.length <= COMPACT_THRESHOLD) {
      console.log(`[memory-compact] Agent "${agent}": ${agentItems.length} items — below threshold, skipping`);
      continue;
    }
    try {
      const summary = await summarize(agent, agentItems);
      await ingestSummary(agent, summary);
      compacted++;
    } catch (err) {
      console.error(`[memory-compact] Failed to compact agent "${agent}": ${err.message}`);
    }
  }

  console.log(`[memory-compact] Done — compacted ${compacted} agent(s) — ${new Date().toISOString()}`);
}

main().catch(err => {
  console.error('[memory-compact] Fatal:', err);
  process.exit(1);
});
