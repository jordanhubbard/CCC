#!/usr/bin/env node
/**
 * vllm-watchdog.mjs — Health-check each Sweden vLLM tunnel port.
 * If /health is unresponsive while the tunnel port is LISTEN, trigger
 * a restart via CCC exec API (ClawBus shell exec to the container).
 *
 * Usage: node vllm-watchdog.mjs [--dry-run]
 * Runs as a one-shot check; intended to be called from CCC heartbeat or cron.
 */

const DRY_RUN = process.argv.includes('--dry-run');

const CCC_URL       = process.env.CCC_URL       || 'http://localhost:8789';
const RCC_AUTH      = process.env.CCC_AUTH_TOKEN || '<YOUR_AGENT_TOKEN>';
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

// Full clean restart: kill zombie EngineCore/GPU worker processes, purge leaked
// PSM shared memory objects and semaphores, then restart via supervisord.
// A plain 'supervisorctl restart' is not enough — zombie EngineCore processes
// from a prior CUDA error will poison the new start.
const CLEAN_RESTART_CMD = [
  'supervisorctl stop vllm',
  // Kill EngineCore, Worker, and ray processes — including GPU-holding VLLM::Worker zombies
  // that survive supervisord stop and keep VRAM allocated (~46.5GB each on L40s)
  "pkill -9 -f 'EngineCore|VLLM::Worker|VllmWorker|vllm.worker|ray::' 2>/dev/null || true",
  // Wait for GPU VRAM to actually release (kernel needs a moment after process kill)
  'sleep 3',
  // Purge leaked POSIX shared memory objects (PSM, /dev/shm/psm_*)
  "ls /dev/shm/ 2>/dev/null | grep -E 'psm_|vllm' | xargs -I{} rm -f /dev/shm/{} 2>/dev/null || true",
  // Purge leaked POSIX semaphores (/dev/shm/sem.*)
  "ls /dev/shm/ 2>/dev/null | grep '^sem\\.' | xargs -I{} rm -f /dev/shm/{} 2>/dev/null || true",
  // Verify GPUs are actually free before starting (log warning if not)
  "nvidia-smi --query-compute-apps=pid,used_memory --format=csv,noheader 2>/dev/null | grep -v '^$' && echo 'WARNING: GPU still has active processes' || true",
  'sleep 2',
  'supervisorctl start vllm',
].join(' && ');

async function restartVllm(agent) {
  if (DRY_RUN) {
    console.log(`[watchdog] DRY-RUN: would clean-restart vllm on ${agent}`);
    return;
  }
  console.log(`[watchdog] Sending clean-restart to ${agent} via CCC exec...`);
  const resp = await fetch(`${CCC_URL}/api/exec`, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      'Authorization': `Bearer ${RCC_AUTH}`,
    },
    body: JSON.stringify({
      targets: [agent],
      mode: 'shell',
      code: CLEAN_RESTART_CMD,
    }),
  });
  if (!resp.ok) {
    console.error(`[watchdog] CCC exec failed for ${agent}: HTTP ${resp.status}`);
    return;
  }
  const data = await resp.json();
  console.log(`[watchdog] Clean-restart dispatched to ${agent}: exec-id=${data.id}`);
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
