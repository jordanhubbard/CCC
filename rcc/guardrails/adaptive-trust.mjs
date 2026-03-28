/**
 * AdaptiveTrust — Organic trust evolution per service/operation type.
 *
 * Adopted from JKHeadley/instar/src/core/AdaptiveTrust.ts (2026-03-27).
 * Rocky RCC port — Node.js ESM, no TypeScript.
 *
 * Trust is tracked per service x operation type:
 *   "I trust you with reading Slack but not deleting messages"
 *   "After 20 successful GitHub ops, I'll stop asking"
 *   "After that incident, always ask before modifying emails"
 *
 * Three trust change paths:
 *   1. Earned — consistent successful ops build trust automatically (max: 'log')
 *   2. Granted — user explicitly elevates trust (can reach 'autonomous')
 *   3. Revoked — incident or explicit user revocation drops trust
 *
 * Safety floor: trust can NEVER auto-escalate to 'autonomous'.
 * Only explicit user statements can grant that level.
 */

import fs from 'node:fs';
import path from 'node:path';

// ── Trust Levels (ordered: most restrictive → least) ──────────────────
const TRUST_ORDER = ['blocked', 'approve-always', 'approve-first', 'log', 'autonomous'];

/** Maximum level that auto-elevation can reach (never 'autonomous') */
const MAX_AUTO_LEVEL = 'log';

/** Default trust by operation type */
const DEFAULT_TRUST = {
  read:   'autonomous',
  write:  'log',
  modify: 'approve-always',
  delete: 'approve-always',
};

// ── Helpers ─────────────────────────────────────────────────────────

function compareTrust(a, b) {
  return TRUST_ORDER.indexOf(a) - TRUST_ORDER.indexOf(b);
}

function nextTrustLevel(current) {
  const idx = TRUST_ORDER.indexOf(current);
  if (idx < 0 || idx >= TRUST_ORDER.length - 1) return null;
  return TRUST_ORDER[idx + 1];
}

function trustToAutonomy(trustLevel) {
  switch (trustLevel) {
    case 'blocked':       return 'block';
    case 'approve-always':return 'approve';
    case 'approve-first': return 'approve';
    case 'log':           return 'log';
    case 'autonomous':    return 'proceed';
    default:              return 'approve';
  }
}

// ── AdaptiveTrust Class ──────────────────────────────────────────────

export class AdaptiveTrust {
  /**
   * @param {object} opts
   * @param {string} opts.stateDir - Directory for trust-profile.json persistence
   * @param {'supervised'|'collaborative'} [opts.floor='collaborative'] - Trust floor
   * @param {boolean} [opts.autoElevateEnabled=true] - Enable auto-elevation suggestions
   * @param {number} [opts.elevationThreshold=5] - Consecutive successes before suggesting elevation
   * @param {string} [opts.incidentDropLevel='approve-always'] - Level to drop to on incident
   */
  constructor({ stateDir, floor = 'collaborative', autoElevateEnabled = true, elevationThreshold = 5, incidentDropLevel = 'approve-always' }) {
    this.stateDir = stateDir;
    this.profilePath = path.join(stateDir, 'trust-profile.json');
    this.floor = floor;
    this.autoElevateEnabled = autoElevateEnabled;
    this.elevationThreshold = elevationThreshold;
    this.incidentDropLevel = incidentDropLevel;
    this.changeLog = [];
    this.profile = this._loadOrCreate();
  }

  /**
   * Get the trust entry for a specific service + operation.
   * @returns {{ level: string, source: string, changedAt: string }}
   */
  getTrustLevel(service, operation) {
    const svc = this.profile.services[service];
    if (!svc) {
      return { level: DEFAULT_TRUST[operation] ?? 'approve-always', source: 'default', changedAt: new Date().toISOString() };
    }
    return svc.operations[operation] ?? { level: DEFAULT_TRUST[operation] ?? 'approve-always', source: 'default', changedAt: new Date().toISOString() };
  }

  /**
   * Map trust level to ExternalOperationGate autonomy behavior.
   * @returns {'proceed'|'log'|'approve'|'block'}
   */
  trustToAutonomy(service, operation) {
    const entry = this.getTrustLevel(service, operation);
    return trustToAutonomy(entry.level);
  }

  /**
   * Record a successful operation — may trigger elevation suggestion.
   * @returns {object|null} Elevation suggestion if threshold reached, else null
   */
  recordSuccess(service, operation) {
    const svc = this._ensureService(service);
    svc.history.successCount++;
    svc.history.streakSinceIncident++;
    this._updateMaturity();
    this._save();

    if (this.autoElevateEnabled) {
      return this._checkElevation(service, operation);
    }
    return null;
  }

  /**
   * Record an incident — trust drops to incidentDropLevel.
   * @returns {object|null} TrustChangeEvent if level changed, else null
   */
  recordIncident(service, operation, reason = 'incident') {
    const svc = this._ensureService(service);
    const currentEntry = svc.operations[operation] ?? { level: DEFAULT_TRUST[operation] ?? 'approve-always' };
    const dropLevel = this.incidentDropLevel;

    svc.history.incidentCount++;
    svc.history.lastIncident = new Date().toISOString();
    svc.history.streakSinceIncident = 0;

    // Only drop if currently less restrictive than drop level
    if (compareTrust(currentEntry.level, dropLevel) > 0) {
      const event = this._setLevel(service, operation, dropLevel, 'revoked', reason);
      this._save();
      return event;
    }

    this._save();
    return null;
  }

  /**
   * User explicitly grants trust for a specific operation.
   * This is the ONLY way to reach 'autonomous'.
   * @returns {object} TrustChangeEvent
   */
  grantTrust(service, operation, level, userStatement = '') {
    this._ensureService(service);
    const event = this._setLevel(service, operation, level, 'user-explicit', userStatement);
    this._save();
    return event;
  }

  /**
   * User grants trust for ALL operations on a service.
   * @returns {object[]} TrustChangeEvent[]
   */
  grantServiceTrust(service, level, userStatement = '') {
    return ['read', 'write', 'modify', 'delete'].map(op => this.grantTrust(service, op, level, userStatement));
  }

  /**
   * Get all pending elevation suggestions.
   * @returns {object[]}
   */
  getPendingElevations() {
    if (!this.autoElevateEnabled) return [];
    const suggestions = [];
    for (const [service, trust] of Object.entries(this.profile.services)) {
      for (const op of ['read', 'write', 'modify', 'delete']) {
        const entry = trust.operations[op];
        if (!entry) continue;
        if (entry.source === 'user-explicit') continue;
        if (compareTrust(entry.level, MAX_AUTO_LEVEL) >= 0) continue;
        if (trust.history.streakSinceIncident >= this.elevationThreshold) {
          const nextLevel = nextTrustLevel(entry.level);
          if (nextLevel && compareTrust(nextLevel, MAX_AUTO_LEVEL) <= 0) {
            suggestions.push({
              service, operation: op,
              currentLevel: entry.level, suggestedLevel: nextLevel,
              reason: `${trust.history.streakSinceIncident} consecutive successful operations without incident.`,
              streak: trust.history.streakSinceIncident,
            });
          }
        }
      }
    }
    return suggestions;
  }

  /** Get the full trust profile (deep copy). */
  getProfile() { return JSON.parse(JSON.stringify(this.profile)); }

  /** Get recent change events. */
  getChangeLog() { return [...this.changeLog]; }

  /** Human-readable summary. */
  getSummary() {
    const lines = [
      `Trust floor: ${this.profile.global.floor}`,
      `Maturity: ${(this.profile.global.maturity * 100).toFixed(0)}%`,
    ];
    for (const [svc, trust] of Object.entries(this.profile.services)) {
      const ops = Object.entries(trust.operations).map(([op, e]) => `${op}=${e.level}`).join(', ');
      lines.push(`${svc}: ${ops} (streak: ${trust.history.streakSinceIncident})`);
    }
    if (!Object.keys(this.profile.services).length) lines.push('No services configured yet.');
    return lines.join('\n');
  }

  // ── Private ────────────────────────────────────────────────────────

  _ensureService(service) {
    if (!this.profile.services[service]) {
      const now = new Date().toISOString();
      this.profile.services[service] = {
        service,
        operations: {
          read:   { level: DEFAULT_TRUST.read,   source: 'default', changedAt: now },
          write:  { level: DEFAULT_TRUST.write,  source: 'default', changedAt: now },
          modify: { level: DEFAULT_TRUST.modify, source: 'default', changedAt: now },
          delete: { level: DEFAULT_TRUST.delete, source: 'default', changedAt: now },
        },
        history: { successCount: 0, incidentCount: 0, streakSinceIncident: 0 },
      };
    }
    return this.profile.services[service];
  }

  _setLevel(service, operation, level, source, reason) {
    const svc = this._ensureService(service);
    const current = svc.operations[operation] ?? { level: DEFAULT_TRUST[operation] ?? 'approve-always' };
    const now = new Date().toISOString();
    const event = { service, operation, from: current.level, to: level, source, timestamp: now, reason };
    svc.operations[operation] = { level, source, changedAt: now, ...(source === 'user-explicit' ? { userStatement: reason } : {}) };
    this.profile.global.lastEvent = `${service}.${operation}: ${current.level} → ${level}`;
    this.profile.global.lastEventAt = now;
    this.changeLog.push(event);
    return event;
  }

  _checkElevation(service, operation) {
    const svc = this.profile.services[service];
    if (!svc) return null;
    const entry = svc.operations[operation];
    if (!entry) return null;
    if (entry.source === 'user-explicit') return null;
    if (compareTrust(entry.level, MAX_AUTO_LEVEL) >= 0) return null;
    if (svc.history.streakSinceIncident >= this.elevationThreshold) {
      const nextLevel = nextTrustLevel(entry.level);
      if (nextLevel && compareTrust(nextLevel, MAX_AUTO_LEVEL) <= 0) {
        return { service, operation, currentLevel: entry.level, suggestedLevel: nextLevel,
          reason: `${svc.history.streakSinceIncident} consecutive successful operations without incident.`,
          streak: svc.history.streakSinceIncident };
      }
    }
    return null;
  }

  _updateMaturity() {
    const totalOps = Object.values(this.profile.services)
      .reduce((sum, svc) => sum + svc.history.successCount, 0);
    this.profile.global.maturity = Math.min(1, totalOps / 100);
  }

  _loadOrCreate() {
    if (fs.existsSync(this.profilePath)) {
      try { return JSON.parse(fs.readFileSync(this.profilePath, 'utf-8')); } catch { /* corrupt */ }
    }
    return {
      services: {},
      global: { maturity: 0, lastEvent: 'Profile created', lastEventAt: new Date().toISOString(), floor: this.floor },
    };
  }

  _save() {
    try {
      const dir = path.dirname(this.profilePath);
      if (!fs.existsSync(dir)) fs.mkdirSync(dir, { recursive: true });
      fs.writeFileSync(this.profilePath, JSON.stringify(this.profile, null, 2));
    } catch { /* non-fatal */ }
  }
}

// ── Singleton for RCC ────────────────────────────────────────────────

let _instance = null;
export function getTrust(opts = {}) {
  if (!_instance) _instance = new AdaptiveTrust(opts);
  return _instance;
}

export { trustToAutonomy, compareTrust, TRUST_ORDER, DEFAULT_TRUST, MAX_AUTO_LEVEL };
