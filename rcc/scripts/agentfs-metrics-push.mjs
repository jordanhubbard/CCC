#!/usr/bin/env node
/**
 * agentfs-metrics-push.mjs
 *
 * Runs on sparky. Every 5 seconds:
 *   1. Fetches GET /api/agentos/metrics (Prometheus text format) from local RCC
 *   2. Parses into a JSON metrics object
 *   3. POSTs to ClawBus as type "agentos.metrics"
 *
 * Environment:
 *   RCC_AUTH_TOKEN    — Bearer token for local RCC + ClawBus (default hardcoded)
 *   SQUIRRELBUS_URL   — ClawBus base URL (default http://100.89.199.14:8788)
 *   RCC_LOCAL_URL     — Local RCC metrics endpoint (default http://127.0.0.1:8789)
 */

const TOKEN         = process.env.RCC_AUTH_TOKEN  || '<YOUR_RCC_TOKEN>';
const BUS_URL       = process.env.SQUIRRELBUS_URL || 'http://100.89.199.14:8788';
const RCC_LOCAL_URL = process.env.RCC_LOCAL_URL   || 'http://127.0.0.1:8789';
const INTERVAL_MS   = 5_000;

/**
 * Parse a Prometheus text exposition blob into a flat object.
 * Extracts numeric values for known agentos_* metric names.
 */
function parsePrometheus(text) {
  const out = {};
  for (const line of text.split('\n')) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith('#')) continue;
    // e.g. agentos_vibe_slots_active{host="sparky"} 2
    const m = trimmed.match(/^(\w+)\{[^}]*\}\s+([\d.]+)/);
    if (!m) {
      // try bare name e.g. agentos_vibe_slots_active 2
      const m2 = trimmed.match(/^(\w+)\s+([\d.]+)/);
      if (m2) out[m2[1]] = parseFloat(m2[2]);
    } else {
      out[m[1]] = parseFloat(m[2]);
    }
  }
  return {
    vibe_active:          out['agentos_vibe_slots_active']     ?? 0,
    vibe_idle:            out['agentos_vibe_slots_idle']       ?? 0,
    vibe_total:           out['agentos_vibe_slots_total']      ?? 0,
    gpu_queue_depth:      out['agentos_gpu_queue_depth']       ?? 0,
    watchdog_miss_total:  out['agentos_watchdog_miss_total']   ?? 0,
    agent_pool_total:     out['agentos_agent_pool_total']      ?? 0,
    agent_pool_available: out['agentos_agent_pool_available']  ?? 0,
    ts: new Date().toISOString(),
  };
}

async function tick() {
  let metrics;
  try {
    const resp = await fetch(`${RCC_LOCAL_URL}/api/agentos/metrics`, {
      headers: { Authorization: `Bearer ${TOKEN}` },
      signal: AbortSignal.timeout(4_000),
    });
    if (!resp.ok) throw new Error(`metrics fetch HTTP ${resp.status}`);
    const text = await resp.text();
    metrics = parsePrometheus(text);
  } catch (e) {
    process.stderr.write(`[agentfs-push] fetch error: ${e.message}\n`);
    return;
  }

  try {
    const resp = await fetch(`${BUS_URL}/bus/send`, {
      method: 'POST',
      headers: {
        Authorization: `Bearer ${TOKEN}`,
        'Content-Type': 'application/json',
      },
      body: JSON.stringify({ type: 'agentos.metrics', from: 'natasha', payload: metrics }),
      signal: AbortSignal.timeout(4_000),
    });
    if (!resp.ok) throw new Error(`bus send HTTP ${resp.status}`);
    process.stderr.write(
      `[agentfs-push] vibe=${metrics.vibe_active}/${metrics.vibe_total} gpu_q=${metrics.gpu_queue_depth} pool=${metrics.agent_pool_available}/${metrics.agent_pool_total} wdog=${metrics.watchdog_miss_total}\n`
    );
  } catch (e) {
    process.stderr.write(`[agentfs-push] bus send error: ${e.message}\n`);
  }
}

// Run immediately, then on interval
tick();
setInterval(tick, INTERVAL_MS);
