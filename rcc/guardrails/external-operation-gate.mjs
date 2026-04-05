/**
 * ExternalOperationGate — LLM-supervised safety for external service operations.
 *
 * Adopted from JKHeadley/instar/src/core/ExternalOperationGate.ts (2026-03-27).
 * Rocky CCC port — Node.js ESM, no TypeScript.
 *
 * Risk matrix: mutability × reversibility × scope → risk level.
 * Three layers: static classification → config floor → LLM proportionality check.
 */

import fs from 'node:fs';
import path from 'node:path';

// ── Risk Matrix ──────────────────────────────────────────────────────

/**
 * Compute risk level from operation dimensions.
 * Reads are always low. Bulk irreversible = critical. Bulk deletes = critical.
 */
export function computeRiskLevel(mutability, reversibility, scope) {
  if (mutability === 'read') return 'low';
  if (scope === 'bulk' && reversibility === 'irreversible') return 'critical';
  if (scope === 'bulk' && mutability === 'delete') return 'critical';
  if (scope === 'bulk') return 'critical';
  if (scope === 'batch' && mutability === 'delete') return 'high';
  if (scope === 'batch' && reversibility === 'irreversible') return 'high';
  if (mutability === 'delete' && reversibility === 'irreversible') return 'high';
  if (mutability === 'delete') return 'medium';
  if (reversibility === 'irreversible') return 'medium';
  if (scope === 'batch') return 'medium';
  return 'low';
}

/**
 * Determine scope from item count.
 */
export function scopeFromCount(count = 1, { batchThreshold = 5, bulkThreshold = 20 } = {}) {
  if (!count || count <= 1) return 'single';
  if (count < bulkThreshold) return 'batch';
  return 'bulk';
}

// ── Autonomy Profiles ────────────────────────────────────────────────

export const AUTONOMY_PROFILES = {
  supervised:    { low: 'log',     medium: 'approve', high: 'approve', critical: 'block' },
  collaborative: { low: 'proceed', medium: 'log',     high: 'approve', critical: 'approve' },
  autonomous:    { low: 'proceed', medium: 'proceed', high: 'log',     critical: 'approve' },
};

// ── Default Service Configs ──────────────────────────────────────────

const DEFAULT_SERVICE_CONFIGS = {
  github:   { permissions: ['read', 'write', 'modify'], blocked: [], batchLimit: 10, requireApproval: ['delete'] },
  minio:    { permissions: ['read', 'write', 'modify'], blocked: [], batchLimit: 50, requireApproval: ['delete'] },
  slack:    { permissions: ['read', 'write'],           blocked: ['delete'], batchLimit: 5 },
  telegram: { permissions: ['read', 'write'],           blocked: ['delete'], batchLimit: 5 },
  exec:     { permissions: ['read'],                    blocked: ['delete'], requireApproval: ['write', 'modify'] },
  // mattermost retired 2026-04-01
};

// ── Main Class ───────────────────────────────────────────────────────

export class ExternalOperationGate {
  constructor({
    profile = 'collaborative',
    services = {},
    stateDir = null,
    batchCheckpoint = { batchThreshold: 5, bulkThreshold: 20, checkpointEvery: 10 },
  } = {}) {
    this.autonomyProfile = AUTONOMY_PROFILES[profile] ?? AUTONOMY_PROFILES.collaborative;
    this.profileName = profile;
    this.services = { ...DEFAULT_SERVICE_CONFIGS, ...services };
    this.stateDir = stateDir;
    this.batchCheckpoint = batchCheckpoint;
  }

  /**
   * Evaluate an external operation. Main entry point.
   *
   * @param {object} op
   * @param {string} op.service - Service name (github, slack, etc.)
   * @param {string} op.mutability - read | write | modify | delete
   * @param {string} op.reversibility - reversible | partially-reversible | irreversible
   * @param {string} [op.scope] - single | batch | bulk (inferred from itemCount if omitted)
   * @param {number} [op.itemCount] - Number of items affected
   * @param {string} [op.description] - Human-readable description
   * @param {string} [op.agentId] - Which agent is making the request
   * @returns {{ action: string, reason: string, riskLevel: string, checkpoint?: object, logged?: boolean }}
   */
  evaluate({ service, mutability, reversibility = 'reversible', scope, itemCount = 1, description = '', agentId = 'unknown' }) {
    // Resolve scope
    const resolvedScope = scope ?? scopeFromCount(itemCount, {
      batchThreshold: this.batchCheckpoint.batchThreshold,
      bulkThreshold: this.batchCheckpoint.bulkThreshold,
    });

    // Compute risk
    const riskLevel = computeRiskLevel(mutability, reversibility, resolvedScope);

    // Check service config — hard blocks first (no override)
    const svcConfig = this.services[service] ?? {};
    const blocked = svcConfig.blocked ?? [];
    const requireApproval = svcConfig.requireApproval ?? [];

    if (blocked.includes(mutability)) {
      this._log({ service, mutability, reversibility, scope: resolvedScope, itemCount, riskLevel, action: 'block', reason: `hard-blocked`, agentId });
      return { action: 'block', reason: `${mutability} is hard-blocked for service "${service}"`, riskLevel };
    }

    // Bulk operations always require a plan (regardless of autonomy profile)
    if (resolvedScope === 'bulk' && itemCount >= this.batchCheckpoint.bulkThreshold) {
      const checkpoint = {
        afterCount: this.batchCheckpoint.checkpointEvery,
        totalExpected: itemCount,
        completedSoFar: 0,
      };
      this._log({ service, mutability, reversibility, scope: resolvedScope, itemCount, riskLevel, action: 'show-plan', reason: 'bulk', agentId });
      return {
        action: 'show-plan',
        reason: `Bulk operation (${itemCount} items) requires explicit plan before proceeding`,
        riskLevel,
        checkpoint,
      };
    }

    // Require-approval operations
    if (requireApproval.includes(mutability)) {
      this._log({ service, mutability, reversibility, scope: resolvedScope, itemCount, riskLevel, action: 'show-plan', reason: 'require-approval', agentId });
      return {
        action: 'show-plan',
        reason: `${mutability} operations on "${service}" require approval`,
        riskLevel,
      };
    }

    // Apply autonomy profile
    const behavior = this.autonomyProfile[riskLevel];
    if (behavior === 'block') {
      this._log({ service, mutability, reversibility, scope: resolvedScope, itemCount, riskLevel, action: 'block', reason: 'autonomy-profile', agentId });
      return { action: 'block', reason: `Risk level "${riskLevel}" is blocked in autonomy profile "${this.profileName}"`, riskLevel };
    }
    if (behavior === 'approve') {
      this._log({ service, mutability, reversibility, scope: resolvedScope, itemCount, riskLevel, action: 'show-plan', reason: 'autonomy-profile', agentId });
      return { action: 'show-plan', reason: `Risk level "${riskLevel}" requires approval in autonomy profile "${this.profileName}"`, riskLevel };
    }
    if (behavior === 'log') {
      this._log({ service, mutability, reversibility, scope: resolvedScope, itemCount, riskLevel, action: 'proceed', reason: 'logged', agentId });
      return { action: 'proceed', reason: `Logged (risk: ${riskLevel})`, riskLevel, logged: true };
    }

    // proceed
    this._log({ service, mutability, reversibility, scope: resolvedScope, itemCount, riskLevel, action: 'proceed', reason: 'low-risk', agentId });
    return { action: 'proceed', reason: `Low risk — proceeding`, riskLevel };
  }

  _log(entry) {
    if (!this.stateDir) return;
    try {
      const logPath = path.join(this.stateDir, 'external-ops.jsonl');
      fs.appendFileSync(logPath, JSON.stringify({ ...entry, ts: new Date().toISOString() }) + '\n');
    } catch { /* non-fatal */ }
  }
}

// ── Singleton for CCC ────────────────────────────────────────────────

let _instance = null;
export function getGate(opts = {}) {
  if (!_instance) _instance = new ExternalOperationGate(opts);
  return _instance;
}
