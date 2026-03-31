#!/usr/bin/env node
/**
 * vllm-watchdog.mjs — Health-check each Sweden vLLM tunnel port.
 * If /health is unresponsive while the tunnel port is LISTEN, trigger
 * a restart via RCC exec API (SquirrelBus shell exec to the container).
 *
 * Usage: node vllm-watchdog.mjs [--dry-run]
 * Runs as a one-shot check; intended to be called from RCC heartbeat or cron.
 */

const DRY_RUN = process.argv.includes('--dry-run');

const RCC_URL       = process.env.RCC_URL       || 'http://localhost:8789';
const RCC_AUTH      = process.env.RCC_AUTH_TOKEN || 'rcc-agent-rocky-20maaghccmbmnby63so';
const HEALTH_TIMEOUT = 5000; // ms

const FLEET = [
  { name: 'boris',   port: 18080 },
  { name: 'peabody', port: 18081 },
  { name: 'sherman', port: 18082 },
  { name: 'snidely', port: 18083 },
  { name: 'dudley',  port: 18084 },
];

async function healthCheck(port) {
  const ctrl = new AbortController();
  const timer = setTimeout(() => ctrl.abort(), HEALTH_TIMEOUT);
  try {
    const r = await fetch(`http://127.0.0.1:${port}/health`, { signal: ctrl.signal });
    clearTimeout(timer);
    return r.ok;
  } catch {
    clearTimeout(timer);
    return false;
  }
}

async function tunnelListening(port) {
  // Check via /proc/net/tcp6 and /proc/net/tcp for local listeners
  try {
    const { readFile } = await import('fs/promises');
    const hexPort = port.toString(16).toUpperCase().padStart(4, '0');
    for (const f of ['/proc/net/tcp6', '/proc/net/tcp']) {
      const data = await readFile(f, 'utf8').catch(() => '');
      if (data.includes(`:${hexPort} `) || data.includes(`${hexPort}:`)) return true;
    }
    return false;
  } catch {
    return false;
  }
}

async function restartVllm(agent) {
  if (DRY_RUN) {
    console.log(`[watchdog] DRY-RUN: would restart vllm on ${agent}`);
    return;
  }
  console.log(`[watchdog] Sending restart command to ${agent} via RCC exec...`);
  const resp = await fetch(`${RCC_URL}/api/exec`, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      'Authorization': `Bearer ${RCC_AUTH}`,
    },
    body: JSON.stringify({
      targets: [agent],
      mode: 'shell',
      code: 'supervisorctl restart vllm',
    }),
  });
  if (!resp.ok) {
    console.error(`[watchdog] RCC exec failed for ${agent}: HTTP ${resp.status}`);
    return;
  }
  const data = await resp.json();
  console.log(`[watchdog] Restart dispatched to ${agent}: exec-id=${data.id}`);
}

async function main() {
  console.log(`[watchdog] Checking ${FLEET.length} vLLM nodes... (dry-run=${DRY_RUN})`);
  const results = [];

  for (const node of FLEET) {
    const listening = await tunnelListening(node.port);
    const healthy   = listening ? await healthCheck(node.port) : false;

    const status = !listening ? 'NO_TUNNEL' : healthy ? 'OK' : 'UNHEALTHY';
    results.push({ ...node, status });
    console.log(`[watchdog] ${node.name}:${node.port} → ${status}`);

    if (listening && !healthy) {
      console.warn(`[watchdog] ${node.name} tunnel up but vLLM unresponsive — restarting`);
      await restartVllm(node.name);
    }
  }

  const unhealthy = results.filter(r => r.status === 'UNHEALTHY');
  const noTunnel  = results.filter(r => r.status === 'NO_TUNNEL');
  const ok        = results.filter(r => r.status === 'OK');

  console.log(`[watchdog] Done. OK=${ok.length} UNHEALTHY=${unhealthy.length} NO_TUNNEL=${noTunnel.length}`);

  if (unhealthy.length > 0) {
    process.exit(2); // signal to caller that restarts were triggered
  }
}

main().catch(e => { console.error('[watchdog]', e.message); process.exit(1); });
