#!/usr/bin/env node
/**
 * skills-drift-check.mjs
 * wq-R-002: Agent skills drift detection
 *
 * Compares each peer's live skills manifest (agents/shared/skills-*.json on MinIO)
 * against their latest heartbeat. If deployed skills differ from registry, flags
 * in peer-status.json and optionally alerts jkh.
 *
 * Usage: node skills-drift-check.mjs
 * Runs as part of Rocky's cron cycle.
 */

import { execSync } from 'child_process';
import { readFileSync, writeFileSync } from 'fs';

const MC = process.env.MC_BIN || '/home/jkh/.local/bin/mc';
const MINIO_ALIAS = process.env.MINIO_ALIAS || 'local';
const SHARED_PREFIX = `${MINIO_ALIAS}/agents/shared`;
const AGENT_NAME = process.env.AGENT_NAME || 'rocky';
const OFFLINE_THRESHOLD_MIN = 240;  // 4 hours
const DRIFT_ALERT_THRESHOLD_MIN = 480;  // 8 hours before Slack alert

function mcCat(path) {
  try {
    return JSON.parse(execSync(`${MC} cat ${path}`, { encoding: 'utf8', stdio: ['pipe','pipe','pipe'] }));
  } catch {
    return null;
  }
}

function mcPut(localPath, remotePath) {
  execSync(`${MC} cp ${localPath} ${remotePath}`, { stdio: ['pipe','pipe','pipe'] });
}

function ageMinutes(tsStr) {
  if (!tsStr) return Infinity;
  return (Date.now() - new Date(tsStr).getTime()) / 60000;
}

const peers = ['rocky', 'bullwinkle', 'natasha'];
const now = new Date().toISOString();
const result = { ts: now, checkedBy: AGENT_NAME, agents: {}, summary: { clean: [], drifted: [], missing_heartbeat: [], missing_registry: [] } };

for (const peer of peers) {
  const heartbeat = mcCat(`${SHARED_PREFIX}/agent-heartbeat-${peer}.json`);
  const registry = mcCat(`${SHARED_PREFIX}/skills-${peer}.json`);

  const hbAge = heartbeat ? ageMinutes(heartbeat.ts || heartbeat.updatedAt) : Infinity;
  const mfAge = registry ? ageMinutes(registry.updated || registry.updatedAt) : Infinity;
  const hbStatus = heartbeat?.status || 'unknown';

  const issues = [];

  if (hbAge > OFFLINE_THRESHOLD_MIN) {
    issues.push(`Heartbeat is ${Math.round(hbAge)}m old (offline threshold: ${OFFLINE_THRESHOLD_MIN}m)`);
  }

  if (!registry) {
    issues.push(`No skills registry found at agents/shared/skills-${peer}.json`);
    result.summary.missing_registry.push(peer);
  }

  if (hbAge > OFFLINE_THRESHOLD_MIN) {
    result.summary.missing_heartbeat.push(peer);
  }

  // Skills count comparison: if heartbeat reports skills but registry count diverges >20%, flag
  const hbSkillCount = heartbeat?.skillCount ?? null;
  const regSkillCount = Array.isArray(registry?.skills) ? registry.skills.length :
    (registry?.strengths ? registry.strengths.length : null);

  let drifted = false;
  if (hbSkillCount !== null && regSkillCount !== null) {
    const delta = Math.abs(hbSkillCount - regSkillCount);
    const pct = regSkillCount > 0 ? delta / regSkillCount : 0;
    if (pct > 0.2) {
      issues.push(`Skill count drift: heartbeat reports ${hbSkillCount}, registry has ${regSkillCount}`);
      drifted = true;
    }
  }

  const status = issues.length > 0 ? (hbAge > OFFLINE_THRESHOLD_MIN ? 'offline' : 'drifted') : 'clean';
  if (status === 'drifted') result.summary.drifted.push(peer);
  else if (status === 'clean') result.summary.clean.push(peer);

  result.agents[peer] = {
    status,
    heartbeatAge: hbAge === Infinity ? 'unknown' : `${Math.round(hbAge)}m`,
    manifestAge: mfAge === Infinity ? 'unknown' : `${Math.round(mfAge)}m`,
    registrySkillCount: regSkillCount,
    heartbeatStatus: hbStatus,
    issues: issues.length > 0 ? issues : null,
    note: issues.length > 0 ? issues[0] : 'No drift detected'
  };
}

// Write to MinIO
const tmpFile = '/tmp/skills-drift.json';
writeFileSync(tmpFile, JSON.stringify(result, null, 2));
mcPut(tmpFile, `${SHARED_PREFIX}/skills-drift.json`);

// Update peer-status.json with drift info
const peerStatus = mcCat(`${SHARED_PREFIX}/peer-status.json`) || { peers: {} };
for (const peer of peers) {
  const agentResult = result.agents[peer];
  if (!peerStatus.peers[peer]) peerStatus.peers[peer] = {};
  peerStatus.peers[peer].skillsDrift = agentResult.status !== 'clean';
  peerStatus.peers[peer].skillsDriftNote = agentResult.status !== 'clean' ? agentResult.note : null;
  peerStatus.peers[peer].skillsDriftTs = now;
  if (agentResult.status === 'offline' && !peerStatus.peers[peer].offlineSince) {
    peerStatus.peers[peer].offlineSince = now;
    peerStatus.peers[peer].status = 'offline';
    peerStatus.peers[peer].alertSent = false;
  }
}
peerStatus.lastUpdated = now;
peerStatus.lastCheckedBy = AGENT_NAME;
const peerTmp = '/tmp/peer-status-drift.json';
writeFileSync(peerTmp, JSON.stringify(peerStatus, null, 2));
mcPut(peerTmp, `${SHARED_PREFIX}/peer-status.json`);

// Print summary
console.log(`[skills-drift-check] ${now}`);
console.log(`Clean: ${result.summary.clean.join(', ') || 'none'}`);
console.log(`Drifted: ${result.summary.drifted.join(', ') || 'none'}`);
console.log(`Missing heartbeat: ${result.summary.missing_heartbeat.join(', ') || 'none'}`);
console.log(`Missing registry: ${result.summary.missing_registry.join(', ') || 'none'}`);

process.exit(0);
