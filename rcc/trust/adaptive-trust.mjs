/**
 * AdaptiveTrust — per-agent × per-service × per-operation trust tracking.
 *
 * Three trust levels:
 *   none  — always block (post-revocation default)
 *   ask   — ask user before proceeding (default for new service/op)
 *   auto  — proceed without asking (explicit grant only, or high streak annotation)
 *
 * Three change paths:
 *   Earned  — streak tracking surfaces suggestions; never auto-promotes to 'auto'
 *   Granted — user explicitly calls grantTrust() — the ONLY path to 'auto'
 *   Revoked — incident or user call to revokeTrust() drops level
 *
 * Safety floor: 'auto' is NEVER granted automatically regardless of streak.
 *
 * State persisted at: ~/.rcc/trust/<agentName>.json
 */

import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';

const TRUST_DIR = path.join(os.homedir(), '.rcc', 'trust');

// ── Helpers ──────────────────────────────────────────────────────────

function profilePath(agentName) {
  return path.join(TRUST_DIR, `${agentName}.json`);
}

function loadProfile(agentName) {
  const p = profilePath(agentName);
  if (fs.existsSync(p)) {
    try { return JSON.parse(fs.readFileSync(p, 'utf-8')); } catch { /* corrupt — fall through */ }
  }
  return { agentName, updatedAt: new Date().toISOString(), services: {} };
}

function saveProfile(profile) {
  try {
    if (!fs.existsSync(TRUST_DIR)) fs.mkdirSync(TRUST_DIR, { recursive: true });
    profile.updatedAt = new Date().toISOString();
    fs.writeFileSync(profilePath(profile.agentName), JSON.stringify(profile, null, 2));
  } catch { /* non-fatal */ }
}

function ensureOp(profile, service, operation) {
  if (!profile.services[service]) profile.services[service] = {};
  if (!profile.services[service][operation]) {
    profile.services[service][operation] = {
      level: 'ask',
      source: 'default',
      streak: 0,
      totalOps: 0,
      lastOp: null,
    };
  }
  return profile.services[service][operation];
}

// ── Public API ───────────────────────────────────────────────────────

/**
 * Get trust level for agent + service + operation.
 * @returns {'none'|'ask'|'auto'}
 */
export function getTrustLevel(agentName, service, operation) {
  const profile = loadProfile(agentName);
  return profile.services?.[service]?.[operation]?.level ?? 'ask';
}

/**
 * Record a successful operation — increments streak and totalOps.
 * At streak=10 or 20, logs a suggestion (but never auto-promotes to 'auto').
 */
export function recordSuccess(agentName, service, operation) {
  const profile = loadProfile(agentName);
  const entry = ensureOp(profile, service, operation);

  entry.streak++;
  entry.totalOps++;
  entry.lastOp = new Date().toISOString();

  // Streak milestones: annotate source so callers can surface suggestions
  if (entry.level === 'ask' && entry.source !== 'revoked') {
    if (entry.streak >= 20) {
      entry.source = 'earned:suggest-auto';
    } else if (entry.streak >= 10 && entry.source !== 'earned:suggest-auto') {
      entry.source = 'earned:streak';
    }
  }

  saveProfile(profile);
}

/**
 * Record a failure/incident — resets streak, optionally revokes trust.
 * @param {boolean} [revoke=false] - If true, level drops to 'none'; else stays at current level.
 */
export function recordFailure(agentName, service, operation, reason, revoke = false) {
  const profile = loadProfile(agentName);
  const entry = ensureOp(profile, service, operation);

  entry.streak = 0;
  entry.totalOps++;
  entry.lastOp = new Date().toISOString();

  if (revoke) {
    entry.level = 'none';
    entry.source = 'revoked';
    entry.revokedAt = new Date().toISOString();
    entry.revokedReason = reason;
  }

  saveProfile(profile);
}

/**
 * Explicitly grant trust (user action). This is the ONLY way to reach 'auto'.
 * @param {string} grantedBy - userId or identifier of the granting user
 */
export function grantTrust(agentName, service, operation, grantedBy) {
  const profile = loadProfile(agentName);
  const entry = ensureOp(profile, service, operation);

  entry.level = 'auto';
  entry.source = 'granted';
  entry.grantedBy = grantedBy;
  // Clear any revocation metadata
  delete entry.revokedAt;
  delete entry.revokedReason;

  saveProfile(profile);
}

/**
 * Revoke trust (user action or incident handler).
 * Level drops to 'none', streak resets.
 */
export function revokeTrust(agentName, service, operation, reason) {
  const profile = loadProfile(agentName);
  const entry = ensureOp(profile, service, operation);

  entry.level = 'none';
  entry.source = 'revoked';
  entry.streak = 0;
  entry.revokedAt = new Date().toISOString();
  entry.revokedReason = reason;

  saveProfile(profile);
}

/**
 * Get the full trust profile for an agent.
 * @returns {object} The full JSON object as stored on disk
 */
export function getTrustProfile(agentName) {
  return loadProfile(agentName);
}

/**
 * Get a human-readable trust summary for an agent.
 * Surfaces streak suggestions inline.
 * @returns {string}
 */
export function summarizeTrust(agentName) {
  const profile = loadProfile(agentName);
  const lines = [`Trust profile: ${agentName} (updated ${profile.updatedAt ?? 'never'})`];

  const services = Object.entries(profile.services ?? {});
  if (!services.length) {
    lines.push('  No trust records yet — all operations default to ask.');
    return lines.join('\n');
  }

  for (const [svc, ops] of services) {
    lines.push(`  ${svc}:`);
    for (const [op, entry] of Object.entries(ops)) {
      let note = '';
      if (entry.source === 'earned:suggest-auto') {
        note = ` ★ ${entry.streak} successful ops — consider granting auto`;
      } else if (entry.source === 'earned:streak') {
        note = ` (streak: ${entry.streak} — ask with positive framing)`;
      } else if (entry.streak > 0) {
        note = ` (streak: ${entry.streak})`;
      }
      if (entry.revokedReason) note += ` [revoked: ${entry.revokedReason}]`;
      lines.push(`    ${op}: ${entry.level} [${entry.source}] ops=${entry.totalOps}${note}`);
    }
  }

  return lines.join('\n');
}
