/**
 * rcc/api/routes/models.mjs — vLLM fleet model deployment pipeline
 *
 * POST /api/models/deploy         — Initiate a model hot-swap on vLLM fleet
 * GET  /api/models/deploy/:jobId  — Poll deploy job status
 * GET  /api/models/fleet          — Current model per vLLM agent
 * GET  /api/models/catalog        — Known models (from LLM registry)
 */

import { readFile, writeFile, mkdir } from 'fs/promises';
import { existsSync } from 'fs';
import { join as pathJoin, dirname } from 'path';
import { randomUUID } from 'crypto';
import { fileURLToPath } from 'url';
import { spawn } from 'child_process';

const __dirname = dirname(fileURLToPath(import.meta.url));

// In-memory job store (survives restarts via persist to disk)
const DEPLOY_JOBS_PATH = pathJoin(__dirname, '../data/model-deploy-jobs.json');
const deployJobs = new Map(); // jobId -> JobRecord

async function loadJobs() {
  try {
    const data = JSON.parse(await readFile(DEPLOY_JOBS_PATH, 'utf8'));
    for (const job of data) deployJobs.set(job.id, job);
  } catch {}
}

async function persistJobs() {
  const arr = Array.from(deployJobs.values()).slice(-50); // keep last 50
  try {
    await mkdir(dirname(DEPLOY_JOBS_PATH), { recursive: true });
    await writeFile(DEPLOY_JOBS_PATH, JSON.stringify(arr, null, 2));
  } catch (e) {
    console.error('[models] Failed to persist jobs:', e.message);
  }
}

// Load on module init
loadJobs();

// Agent tunnel config (mirror of model-deploy.mjs)
const AGENT_TUNNELS = {
  boris:   { port: 18080 },
  peabody: { port: 18081 },
  sherman: { port: 18082 },
  snidely: { port: 18083 },
  dudley:  { port: 18084 },
};

/** Query vLLM /v1/models on a tunnel port (2s timeout) */
async function queryVllmModels(port) {
  try {
    const ctrl = new AbortController();
    const tid = setTimeout(() => ctrl.abort(), 2500);
    const resp = await fetch(`http://127.0.0.1:${port}/v1/models`, { signal: ctrl.signal });
    clearTimeout(tid);
    if (!resp.ok) return { ok: false, models: [] };
    const data = await resp.json();
    return { ok: true, models: (data.data || []).map(m => m.id) };
  } catch {
    return { ok: false, models: [] };
  }
}

/** Validate a HuggingFace model ID via HF API */
async function validateHFModel(modelId, hfToken) {
  const headers = hfToken ? { Authorization: `Bearer ${hfToken}` } : {};
  const ctrl = new AbortController();
  const tid = setTimeout(() => ctrl.abort(), 8000);
  const resp = await fetch(`https://huggingface.co/api/models/${encodeURIComponent(modelId)}`, { headers, signal: ctrl.signal });
  clearTimeout(tid);
  if (!resp.ok) throw new Error(`HF model not found: ${modelId} (HTTP ${resp.status})`);
  const data = await resp.json();
  return {
    id: data.id,
    tags: data.tags || [],
    siblings: (data.siblings || []).map(s => s.rfilename),
    size: data.safetensors?.total || null,
    pipelineTag: data.pipeline_tag || null,
  };
}

/** Spawn model-deploy.mjs as a background process, stream output to job log */
function spawnDeployScript(job) {
  const scriptPath = pathJoin(__dirname, '../../scripts/model-deploy.mjs');
  const agents = job.targetAgents.join(',');
  const env = {
    ...process.env,
    CCC_AGENT_TOKEN: process.env.CCC_AGENT_TOKEN || process.env.CCC_AUTH_TOKENS?.split(',')[0] || '',
    HF_TOKEN: process.env.HF_TOKEN || '',
    CCC_URL: process.env.CCC_URL || 'http://localhost:8789',
    DEPLOY_JOB_ID: job.id,
  };

  const child = spawn(
    'node', [scriptPath, '--model', job.modelId, '--agents', agents, '--deploy'],
    { env, stdio: ['ignore', 'pipe', 'pipe'], detached: false }
  );

  const appendLog = (line) => {
    const j = deployJobs.get(job.id);
    if (!j) return;
    j.log = (j.log || []);
    j.log.push(`[${new Date().toISOString()}] ${line.trim()}`);
    if (j.log.length > 500) j.log = j.log.slice(-500);
    persistJobs();
  };

  child.stdout.on('data', d => appendLog(d.toString()));
  child.stderr.on('data', d => appendLog(`STDERR: ${d.toString()}`));

  child.on('exit', (code) => {
    const j = deployJobs.get(job.id);
    if (!j) return;
    j.status = code === 0 ? 'completed' : 'failed';
    j.completedAt = new Date().toISOString();
    j.exitCode = code;
    appendLog(`Deploy process exited with code ${code}`);
    persistJobs();
  });

  child.on('error', (e) => {
    const j = deployJobs.get(job.id);
    if (!j) return;
    j.status = 'failed';
    j.error = e.message;
    j.completedAt = new Date().toISOString();
    persistJobs();
  });

  return child.pid;
}

export default function registerModels(app, state) {
  const { json, readBody, isAuthed } = state;

  // ── GET /api/models/fleet — current model running on each vLLM agent ───────
  app.on('GET', '/api/models/fleet', async (req, res) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });

    const results = await Promise.all(
      Object.entries(AGENT_TUNNELS).map(async ([agent, { port }]) => {
        const r = await queryVllmModels(port);
        return { agent, port, online: r.ok, models: r.models };
      })
    );

    return json(res, 200, {
      fleet: results,
      ts: new Date().toISOString(),
    });
  });

  // ── GET /api/models/catalog — known HF models (recent deploys + registry) ──
  app.on('GET', '/api/models/catalog', async (req, res) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });

    const completed = Array.from(deployJobs.values())
      .filter(j => j.status === 'completed')
      .map(j => ({ modelId: j.modelId, deployedAt: j.completedAt, targetAgents: j.targetAgents }));

    // Deduplicate by modelId
    const seen = new Set();
    const unique = completed.filter(m => seen.has(m.modelId) ? false : (seen.add(m.modelId), true));

    return json(res, 200, { models: unique });
  });

  // ── POST /api/models/deploy — initiate fleet-wide model hot-swap ────────────
  app.on('POST', '/api/models/deploy', async (req, res) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });

    let body;
    try { body = JSON.parse(await readBody(req)); }
    catch { return json(res, 400, { error: 'Invalid JSON' }); }

    const { hf_model_id, model_id, target_agents, dry_run = false } = body;
    const modelId = hf_model_id || model_id;

    if (!modelId) return json(res, 400, { error: 'hf_model_id required' });

    const targetAgents = Array.isArray(target_agents) && target_agents.length > 0
      ? target_agents.filter(a => AGENT_TUNNELS[a])
      : Object.keys(AGENT_TUNNELS);

    if (targetAgents.length === 0)
      return json(res, 400, { error: 'No valid target agents. Valid: ' + Object.keys(AGENT_TUNNELS).join(', ') });

    // Validate model ID with HF
    let modelInfo;
    try {
      modelInfo = await validateHFModel(modelId, process.env.HF_TOKEN);
    } catch (e) {
      return json(res, 422, { error: 'Model validation failed', message: e.message });
    }

    const jobId = `deploy-${Date.now()}-${randomUUID().slice(0, 8)}`;
    const job = {
      id: jobId,
      modelId,
      modelInfo,
      targetAgents,
      dryRun: !!dry_run,
      status: dry_run ? 'dry_run_complete' : 'queued',
      createdAt: new Date().toISOString(),
      completedAt: dry_run ? new Date().toISOString() : null,
      exitCode: null,
      log: [`Model ${modelId} validated OK. tags: ${modelInfo.tags.slice(0, 5).join(', ')}. pipeline: ${modelInfo.pipelineTag || 'unknown'}. Agents: ${targetAgents.join(', ')}.`],
      pid: null,
    };

    deployJobs.set(jobId, job);
    await persistJobs();

    if (!dry_run) {
      // Spawn deploy in background
      try {
        const pid = spawnDeployScript(job);
        job.status = 'running';
        job.pid = pid;
        await persistJobs();
      } catch (e) {
        job.status = 'failed';
        job.error = `Failed to spawn deploy script: ${e.message}`;
        await persistJobs();
        return json(res, 500, { error: 'Deploy spawn failed', message: e.message, jobId });
      }
    }

    return json(res, 202, {
      ok: true,
      jobId,
      modelId,
      modelInfo,
      targetAgents,
      dryRun: dry_run,
      status: job.status,
      statusUrl: `/api/models/deploy/${jobId}`,
    });
  });

  // ── GET /api/models/deploy/:jobId — poll deploy job status ─────────────────
  app.on('GET', /^\/api\/models\/deploy\/([^/]+)$/, async (req, res, m) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });

    const jobId = m[1];
    const job = deployJobs.get(jobId);
    if (!job) return json(res, 404, { error: 'Job not found', jobId });

    return json(res, 200, {
      ...job,
      logTail: (job.log || []).slice(-50), // Last 50 lines for polling clients
    });
  });

  // ── GET /api/models/deploy — list all deploy jobs ──────────────────────────
  app.on('GET', '/api/models/deploy', async (req, res) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });

    const jobs = Array.from(deployJobs.values())
      .sort((a, b) => new Date(b.createdAt) - new Date(a.createdAt))
      .slice(0, 20)
      .map(j => ({
        id: j.id, modelId: j.modelId, status: j.status,
        targetAgents: j.targetAgents, createdAt: j.createdAt,
        completedAt: j.completedAt, dryRun: j.dryRun,
      }));

    return json(res, 200, { jobs });
  });
}
