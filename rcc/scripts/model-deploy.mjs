#!/usr/bin/env node
/**
 * model-deploy.mjs — Fleet-wide vLLM model hot-swap orchestrator
 *
 * Usage:
 *   node model-deploy.mjs --model google/gemma-4-31B-it [--agents boris,peabody,...]
 *   node model-deploy.mjs --list                          # list current models
 *   node model-deploy.mjs --validate google/gemma-4-31B-it  # dry-run validate only
 *
 * Environment:
 *   CCC_AGENT_TOKEN  — CCC API auth token
 *   HF_TOKEN         — HuggingFace API token
 *   CCC_URL          — CCC base URL (default: http://localhost:8789)
 */

import { execSync, spawnSync } from 'child_process';
import { existsSync, mkdirSync, writeFileSync, readFileSync, appendFileSync } from 'fs';
import { join } from 'path';
import { parseArgs } from 'util';

// ── Config ───────────────────────────────────────────────────────────────────

const CCC_URL = process.env.CCC_URL || 'http://localhost:8789';
const RCC_TOKEN = process.env.CCC_AGENT_TOKEN || '';
const HF_TOKEN = process.env.HF_TOKEN || '';
const LOG_DIR = process.env.HOME + '/.openclaw/workspace/logs';
// Use ClawFS (JuiceFS shared volume) as model cache if mounted — models stored there are
// accessible to all fleet agents with ClawFS mounted, eliminating per-node re-downloads.
// Falls back to /tmp if ClawFS is not available (e.g. during initial setup).
const CLAWFS_DIR = process.env.HOME + '/clawfs';
const CLAWFS_SENTINEL = CLAWFS_DIR + '/.config';  // JuiceFS sentinel file present when mounted
const CLAWFS_MOUNTED = existsSync(CLAWFS_SENTINEL);
const MODEL_CACHE_DIR = CLAWFS_MOUNTED
  ? (CLAWFS_DIR + '/models')
  : '/tmp/model-deploy-cache';

// Sweden container tunnel ports (on do-host1 localhost)
const AGENT_TUNNELS = {
  boris:   { port: 18080, ssh_port: 10022 },
  peabody: { port: 18081, ssh_port: 10023 },
  sherman: { port: 18082, ssh_port: 10024 },
  snidely: { port: 18083, ssh_port: 10025 },
  dudley:  { port: 18084, ssh_port: 10026 },
};

// ── Utilities ────────────────────────────────────────────────────────────────

function log(msg) {
  const ts = new Date().toISOString();
  const line = `[${ts}] ${msg}`;
  console.log(line);
  try {
    mkdirSync(LOG_DIR, { recursive: true });
    appendFileSync(join(LOG_DIR, 'model-deploy.log'), line + '\n');
  } catch {}
}

function err(msg) { log(`ERROR: ${msg}`); }

async function rccGet(path) {
  const resp = await fetch(`${CCC_URL}${path}`, {
    headers: { Authorization: `Bearer ${RCC_TOKEN}` }
  });
  return resp.json();
}

async function rccPost(path, body) {
  const resp = await fetch(`${CCC_URL}${path}`, {
    method: 'POST',
    headers: { Authorization: `Bearer ${RCC_TOKEN}`, 'Content-Type': 'application/json' },
    body: JSON.stringify(body)
  });
  return resp.json();
}

async function vllmModels(port) {
  try {
    const resp = await fetch(`http://127.0.0.1:${port}/v1/models`, {
      signal: AbortSignal.timeout(5000)
    });
    const data = await resp.json();
    return data.data?.map(m => m.id) || [];
  } catch {
    return null; // unreachable
  }
}

// ── Phase 1: Validate HuggingFace model exists ───────────────────────────────

async function validateHFModel(modelId) {
  log(`Validating HuggingFace model: ${modelId}`);
  const resp = await fetch(`https://huggingface.co/api/models/${modelId}`, {
    headers: HF_TOKEN ? { Authorization: `Bearer ${HF_TOKEN}` } : {}
  });
  if (!resp.ok) {
    throw new Error(`HF model ${modelId} not found (HTTP ${resp.status}). Check model ID.`);
  }
  const data = await resp.json();
  const size = data.safetensors?.total || null;
  const tags = data.tags || [];
  log(`  Model found: ${data.id}, siblings: ${data.siblings?.length || 0} files`);
  if (size) log(`  Model size: ~${(size / 1e9).toFixed(1)} GB`);
  log(`  Tags: ${tags.filter(t => ['llm','text-generation'].includes(t)).join(', ') || tags.slice(0,3).join(', ')}`);
  return { id: data.id, size, tags };
}

// ── Phase 2: Download model on orchestrator (Rocky/do-host1) ─────────────────

async function downloadModel(modelId) {
  log(`Downloading model ${modelId} via HuggingFace CLI...`);
  mkdirSync(MODEL_CACHE_DIR, { recursive: true });

  // Use huggingface-cli or python huggingface_hub if available
  const hfCliAvail = spawnSync('which', ['huggingface-cli']).status === 0;
  const pythonAvail = spawnSync('which', ['python3']).status === 0;

  if (!hfCliAvail && !pythonAvail) {
    throw new Error('huggingface-cli and python3 both unavailable. Cannot download model.');
  }

  const env = { ...process.env, HF_TOKEN, HF_HOME: MODEL_CACHE_DIR, TRANSFORMERS_CACHE: MODEL_CACHE_DIR };

  if (hfCliAvail) {
    log('  Using huggingface-cli download...');
    const result = spawnSync('huggingface-cli', ['download', modelId, '--quiet'], { env, stdio: 'inherit' });
    if (result.status !== 0) throw new Error('huggingface-cli download failed');
  } else {
    log('  Using python huggingface_hub.snapshot_download...');
    const pyScript = `
from huggingface_hub import snapshot_download
import os
path = snapshot_download(
  repo_id="${modelId}",
  cache_dir="${MODEL_CACHE_DIR}",
  token="${HF_TOKEN}",
  ignore_patterns=["*.msgpack","*.h5","flax_model*","tf_model*"],
)
print("DOWNLOADED_TO:", path)
`;
    const result = spawnSync('python3', ['-c', pyScript], { env, stdio: ['ignore', 'pipe', 'pipe'] });
    const stdout = result.stdout?.toString() || '';
    const stderr = result.stderr?.toString() || '';
    if (result.status !== 0) {
      log(`  Python download stderr: ${stderr.slice(0, 500)}`);
      throw new Error('python snapshot_download failed');
    }
    const match = stdout.match(/DOWNLOADED_TO: (.+)/);
    if (match) log(`  Downloaded to: ${match[1].trim()}`);
  }

  log(`  Download complete.`);
}

// ── Phase 3: Store model in MinIO for fleet access ───────────────────────────

async function storeInMinIO(modelId) {
  log(`Storing model in MinIO (agents/models/${modelId.replace('/', '__')})...`);
  const mcAvail = spawnSync('which', ['mc']).status === 0;

  if (!mcAvail) {
    log('  mc (MinIO client) not available — skipping MinIO upload. vLLM will need to download directly.');
    return null;
  }

  // Ensure alias
  spawnSync('mc', ['alias', 'set', 'local', 'http://localhost:9000', 'minioadmin', 'minioadmin'], { stdio: 'ignore' });

  const modelSlug = modelId.replace('/', '__');
  const bucket = `agents/models/${modelSlug}`;

  // Create bucket/prefix
  const cpResult = spawnSync('mc', ['mirror', `${MODEL_CACHE_DIR}/models--${modelId.replace('/', '--')}`, `local/${bucket}/`],
    { stdio: 'inherit' });

  if (cpResult.status !== 0) {
    log('  MinIO upload returned non-zero — continuing anyway (vLLM can download directly from HF)');
    return null;
  }

  log(`  Stored at: ${bucket}`);
  return bucket;
}

// ── Phase 4: Send restart commands to each vLLM agent ────────────────────────

async function reloadAgentModel(agentName, port, newModelId, hfToken) {
  log(`Reloading ${agentName} (port ${port}) → ${newModelId}`);

  // Check current model first
  const currentModels = await vllmModels(port);
  if (currentModels === null) {
    log(`  ${agentName}: UNREACHABLE — skipping`);
    return { agent: agentName, status: 'unreachable' };
  }
  log(`  ${agentName}: current model(s): ${currentModels.join(', ')}`);

  // Send reload command via CCC exec API
  const execPayload = {
    targets: [agentName],
    mode: 'shell',
    code: [
      `set -e`,
      // Find vllm process and kill it gracefully
      `VLLM_PID=$(pgrep -f 'vllm.entrypoints' | head -1 || true)`,
      `if [ -n "$VLLM_PID" ]; then`,
      `  echo "Stopping vLLM (PID $VLLM_PID)..."`,
      `  kill -TERM $VLLM_PID`,
      `  sleep 10`,
      `  kill -9 $VLLM_PID 2>/dev/null || true`,
      `fi`,
      // Start vLLM with new model
      `export HF_TOKEN="${hfToken}"`,
      `export HF_HOME=/tmp/hf_cache`,
      `export VLLM_MODEL="${newModelId}"`,
      // supervisorctl restart if available, else direct
      `if command -v supervisorctl &>/dev/null; then`,
      `  supervisorctl restart vllm || true`,
      `else`,
      `  nohup python3 -m vllm.entrypoints.openai.api_server \\`,
      `    --model "${newModelId}" \\`,
      `    --host 0.0.0.0 --port 8080 \\`,
      `    --served-model-name "${newModelId.split('/').pop()}" \\`,
      `    --trust-remote-code \\`,
      `    > /tmp/vllm-restart.log 2>&1 &`,
      `fi`,
      `echo "DONE: vLLM restart initiated for ${newModelId}"`,
    ].join('\n')
  };

  try {
    const execResp = await rccPost('/api/exec', execPayload);
    log(`  ${agentName}: exec dispatched, id=${execResp.id || 'unknown'}`);

    // Poll for result (up to 60s)
    if (execResp.id) {
      for (let i = 0; i < 12; i++) {
        await new Promise(r => setTimeout(r, 5000));
        const result = await rccGet(`/api/exec/${execResp.id}`);
        if (result.results?.[agentName]) {
          const r = result.results[agentName];
          log(`  ${agentName}: exec result: ${r.stdout?.slice(0, 200) || r.error || 'no output'}`);
          break;
        }
      }
    }

    return { agent: agentName, status: 'reload-sent' };
  } catch (e) {
    log(`  ${agentName}: exec failed: ${e.message}`);
    return { agent: agentName, status: 'failed', error: e.message };
  }
}

// ── Phase 5: Verify new model is up ──────────────────────────────────────────

async function verifyAgentModel(agentName, port, expectedModel, timeoutMs = 300000) {
  log(`Verifying ${agentName} is serving ${expectedModel}...`);
  const start = Date.now();
  const modelBase = expectedModel.split('/').pop();

  while (Date.now() - start < timeoutMs) {
    const models = await vllmModels(port);
    if (models !== null) {
      const match = models.some(m => m === expectedModel || m.includes(modelBase));
      if (match) {
        log(`  ✓ ${agentName}: serving ${models.join(', ')}`);
        return { agent: agentName, status: 'ok', models };
      }
      log(`  ${agentName}: waiting... current models: ${models.join(', ') || 'none'}`);
    }
    await new Promise(r => setTimeout(r, 30000));
  }

  log(`  ✗ ${agentName}: timeout after ${timeoutMs/1000}s`);
  return { agent: agentName, status: 'timeout' };
}

// ── Phase 6: Update tokenhub provider entries ─────────────────────────────────

async function updateTokenhub(agentName, newModelId) {
  const tokenhubUrl = process.env.TOKENHUB_URL || 'http://localhost:8090';
  const adminToken = process.env.TOKENHUB_ADMIN_TOKEN || '';
  const modelBase = newModelId.split('/').pop().toLowerCase().replace(/[^a-z0-9]/g, '-');
  const providerId = `${agentName}-${modelBase}`;

  log(`Updating tokenhub: registering ${providerId}...`);

  try {
    const resp = await fetch(`${tokenhubUrl}/api/admin/providers`, {
      method: 'POST',
      headers: {
        Authorization: `Bearer ${adminToken}`,
        'Content-Type': 'application/json'
      },
      body: JSON.stringify({
        id: providerId,
        name: `${agentName} — ${newModelId}`,
        base_url: `http://127.0.0.1:${AGENT_TUNNELS[agentName]?.port || 18080}/v1`,
        type: 'openai-compat',
        model: newModelId,
        models: [newModelId, modelBase],
        context_window: 131072,
        priority: 5,
        cost_per_1k: 0,
        enabled: true,
        tags: [agentName, 'vllm', 'self-hosted'],
      })
    });
    const data = await resp.json();
    if (resp.ok) {
      log(`  ✓ tokenhub: ${providerId} registered`);
    } else {
      log(`  tokenhub returned: ${JSON.stringify(data).slice(0, 200)}`);
    }
  } catch (e) {
    log(`  tokenhub update failed: ${e.message}`);
  }
}

// ── List current models ───────────────────────────────────────────────────────

async function listModels() {
  console.log('\nCurrent vLLM models on Sweden nodes:\n');
  for (const [name, { port }] of Object.entries(AGENT_TUNNELS)) {
    const models = await vllmModels(port);
    const status = models === null ? '✗ UNREACHABLE' : `✓ ${models.join(', ') || '(none)'}`;
    console.log(`  ${name.padEnd(10)} (port ${port}):  ${status}`);
  }
  console.log('');
}

// ── Main orchestration ────────────────────────────────────────────────────────

async function deploy(modelId, targetAgents, dryRun = false) {
  log(`\n${'═'.repeat(60)}`);
  log(`Model Deploy Pipeline starting`);
  log(`  Model:  ${modelId}`);
  log(`  Agents: ${targetAgents.join(', ')}`);
  log(`  Mode:   ${dryRun ? 'DRY-RUN (validate only)' : 'LIVE DEPLOY'}`);
  log(`  Cache:  ${MODEL_CACHE_DIR} (ClawFS: ${CLAWFS_MOUNTED ? 'YES — fleet-shared' : 'NO — local /tmp'})`);
  log(`${'═'.repeat(60)}\n`);

  // Phase 1: Validate model
  let modelInfo;
  try {
    modelInfo = await validateHFModel(modelId);
  } catch (e) {
    err(`Validation failed: ${e.message}`);
    return { success: false, phase: 'validate', error: e.message };
  }

  if (dryRun) {
    log('\nDRY RUN complete. Model is valid. Pass --deploy to proceed.');
    return { success: true, phase: 'validate', model: modelInfo };
  }

  // Phase 2: Download (skip if model is large and we'll have containers download directly)
  const modelSizeGB = modelInfo.size ? modelInfo.size / 1e9 : null;
  if (modelSizeGB && modelSizeGB > 50) {
    log(`Model is ${modelSizeGB.toFixed(0)}GB — containers will download directly from HF (skipping local download)`);
  } else {
    try {
      await downloadModel(modelId);
      await storeInMinIO(modelId);
    } catch (e) {
      log(`Download/MinIO phase skipped: ${e.message} — proceeding with direct HF download on containers`);
    }
  }

  // Phase 3: Reload each agent
  const reloadResults = [];
  for (const agentName of targetAgents) {
    const tunnel = AGENT_TUNNELS[agentName];
    if (!tunnel) {
      log(`Unknown agent: ${agentName} — skipping`);
      reloadResults.push({ agent: agentName, status: 'unknown' });
      continue;
    }
    const result = await reloadAgentModel(agentName, tunnel.port, modelId, HF_TOKEN);
    reloadResults.push(result);
    // Stagger restarts by 30s to avoid all nodes going down simultaneously
    if (targetAgents.indexOf(agentName) < targetAgents.length - 1) {
      log('Waiting 30s before next node restart...');
      await new Promise(r => setTimeout(r, 30000));
    }
  }

  // Phase 4: Verify (with generous timeout — model download can take a while)
  const verifyResults = [];
  log('\nVerifying model deployment (timeout: 10min per node)...');
  for (const agentName of targetAgents) {
    const tunnel = AGENT_TUNNELS[agentName];
    if (!tunnel) continue;
    const result = await verifyAgentModel(agentName, tunnel.port, modelId, 600000);
    verifyResults.push(result);
    if (result.status === 'ok') {
      await updateTokenhub(agentName, modelId);
    }
  }

  // Summary
  const succeeded = verifyResults.filter(r => r.status === 'ok').map(r => r.agent);
  const failed = verifyResults.filter(r => r.status !== 'ok').map(r => r.agent);

  log(`\n${'═'.repeat(60)}`);
  log(`Deploy complete.`);
  log(`  Success: ${succeeded.join(', ') || 'none'}`);
  log(`  Failed:  ${failed.join(', ') || 'none'}`);
  log(`${'═'.repeat(60)}\n`);

  // Post result to CCC queue item if DEPLOY_ITEM_ID is set
  if (process.env.DEPLOY_ITEM_ID) {
    const resultText = `Model ${modelId} deployed. Success: [${succeeded.join(',')}] Failed: [${failed.join(',')}]`;
    await rccPost(`/api/item/${process.env.DEPLOY_ITEM_ID}/complete`, {
      agent: 'rocky',
      result: resultText,
      resolution: failed.length === 0 ? 'success' : 'partial'
    });
  }

  return { success: failed.length === 0, succeeded, failed };
}

// ── CLI entrypoint ────────────────────────────────────────────────────────────

const { values, positionals } = parseArgs({
  options: {
    model:    { type: 'string' },
    agents:   { type: 'string' },
    list:     { type: 'boolean' },
    validate: { type: 'boolean' },
    deploy:   { type: 'boolean' },
    help:     { type: 'boolean', short: 'h' },
  },
  allowPositionals: true,
});

if (values.help) {
  console.log(`
model-deploy.mjs — Fleet vLLM model hot-swap

Usage:
  node model-deploy.mjs --list
  node model-deploy.mjs --model <hf-model-id> --validate
  node model-deploy.mjs --model <hf-model-id> [--agents boris,peabody,...] --deploy

Options:
  --model <id>      HuggingFace model ID (e.g. google/gemma-4-31B-it)
  --agents <list>   Comma-separated agent names (default: all 5 Sweden containers)
  --list            Show current models on all agents
  --validate        Validate model exists on HF without deploying
  --deploy          Execute the deploy (required for non-list/validate)
  -h, --help        This help

Environment:
  CCC_AGENT_TOKEN   CCC API token
  HF_TOKEN          HuggingFace API token
  CCC_URL           CCC URL (default: http://localhost:8789)
`);
  process.exit(0);
}

if (!RCC_TOKEN) {
  console.error('CCC_AGENT_TOKEN not set. source ~/.rcc/.env first.');
  process.exit(1);
}

if (values.list) {
  await listModels();
  process.exit(0);
}

const modelId = values.model || positionals[0];
if (!modelId) {
  console.error('--model required');
  process.exit(1);
}

const targetAgents = values.agents
  ? values.agents.split(',').map(a => a.trim())
  : Object.keys(AGENT_TUNNELS);

if (values.validate && !values.deploy) {
  await deploy(modelId, targetAgents, true);
} else if (values.deploy) {
  const result = await deploy(modelId, targetAgents, false);
  process.exit(result.success ? 0 : 1);
} else {
  console.error('Specify --validate, --deploy, or --list');
  process.exit(1);
}
