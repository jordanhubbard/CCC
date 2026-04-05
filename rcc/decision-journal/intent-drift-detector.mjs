/**
 * IntentDriftDetector — detects behavioral drift in agent decision patterns.
 *
 * Reads DecisionJournal entries over a sliding window and computes a drift
 * score by comparing the current distribution of principles/confidence/conflicts
 * against a baseline window. Alerts when drift exceeds a configurable threshold.
 *
 * Exposes: detectDrift(opts), buildBaseline(opts), driftReport(opts)
 *
 * Used by: CCC /api/drift endpoint, heartbeat checks, agent self-monitoring.
 *
 * Example drift signals:
 *   - Confidence trend declining (agent becoming less certain)
 *   - was_conflict rate rising (more principle clashes)
 *   - Principle distribution shifting (different values being invoked)
 *   - Outcome mix changing (more 'blocked'/'escalated', fewer 'proceed')
 */

import { DecisionJournal } from './index.mjs';

// Outcomes ordered by autonomy level (higher = more autonomous)
const OUTCOME_WEIGHT = { proceed: 1.0, ok: 0.9, skipped: 0.5, escalated: 0.2, blocked: 0.0 };

/**
 * Compute a principle distribution (freq map normalized to 0-1 each).
 */
function principleDistribution(entries) {
  const freq = {};
  for (const e of entries) {
    freq[e.principle_used] = (freq[e.principle_used] || 0) + 1;
  }
  const total = entries.length || 1;
  return Object.fromEntries(Object.entries(freq).map(([k, v]) => [k, v / total]));
}

/**
 * KL-divergence-lite: symmetric difference between two freq distributions.
 * Returns 0.0 (identical) to 1.0+ (completely different).
 */
function distributionDelta(baselineDist, currentDist) {
  const allKeys = new Set([...Object.keys(baselineDist), ...Object.keys(currentDist)]);
  let delta = 0;
  for (const k of allKeys) {
    const b = baselineDist[k] || 0;
    const c = currentDist[k] || 0;
    delta += Math.abs(b - c);
  }
  return delta / 2; // normalize to 0–1
}

/**
 * Compute the mean autonomy score of an entry set based on outcome weights.
 */
function autonomyScore(entries) {
  if (!entries.length) return 0.5;
  return entries.reduce((s, e) => s + (OUTCOME_WEIGHT[e.outcome] ?? 0.5), 0) / entries.length;
}

/**
 * Build a baseline snapshot from the oldest `windowSize` entries.
 * @param {object} opts
 * @param {DecisionJournal} opts.journal
 * @param {string|null}     [opts.agent]      - Filter by agent (null = all)
 * @param {number}          [opts.windowSize] - Number of entries for baseline (default 50)
 * @returns {object} baseline snapshot
 */
export function buildBaseline({ journal, agent = null, windowSize = 50 } = {}) {
  const entries = journal.getRecent({ limit: windowSize * 2, agent }).slice(0, windowSize);
  if (entries.length < 5) {
    return { insufficient_data: true, count: entries.length };
  }
  return {
    count: entries.length,
    avg_confidence: entries.reduce((s, e) => s + e.confidence, 0) / entries.length,
    conflict_rate: entries.filter(e => e.was_conflict).length / entries.length,
    autonomy_score: autonomyScore(entries),
    principle_dist: principleDistribution(entries),
    window_start: entries[0]?.ts,
    window_end: entries[entries.length - 1]?.ts,
  };
}

/**
 * Detect drift between baseline and a current sliding window.
 *
 * @param {object} opts
 * @param {DecisionJournal} opts.journal
 * @param {string|null}     [opts.agent]            - Filter by agent (null = all)
 * @param {number}          [opts.windowSize]       - Recent entries to compare (default 20)
 * @param {object}          [opts.baseline]         - Pre-computed baseline (or computed fresh)
 * @param {number}          [opts.baselineWindow]   - Entries for baseline if not provided (default 50)
 * @param {number}          [opts.driftThreshold]   - Alert if score > this (default 0.25)
 * @returns {object} drift report
 */
export function detectDrift({
  journal,
  agent = null,
  windowSize = 20,
  baseline = null,
  baselineWindow = 50,
  driftThreshold = 0.25,
} = {}) {
  // Get recent entries (current window)
  const recent = journal.getRecent({ limit: windowSize, agent });
  if (recent.length < 3) {
    return {
      ok: true,
      drift_score: 0,
      alert: false,
      reason: 'insufficient_data',
      recent_count: recent.length,
    };
  }

  // Build or use provided baseline
  const base = baseline ?? buildBaseline({ journal, agent, windowSize: baselineWindow });
  if (base.insufficient_data) {
    return {
      ok: true,
      drift_score: 0,
      alert: false,
      reason: 'insufficient_baseline_data',
      baseline_count: base.count,
    };
  }

  // Current window metrics
  const currentConfidence = recent.reduce((s, e) => s + e.confidence, 0) / recent.length;
  const currentConflictRate = recent.filter(e => e.was_conflict).length / recent.length;
  const currentAutonomy = autonomyScore(recent);
  const currentDist = principleDistribution(recent);

  // Component drift scores (each 0–1)
  const confidenceDrift = Math.abs(currentConfidence - base.avg_confidence);
  const conflictDrift   = Math.abs(currentConflictRate - base.conflict_rate);
  const autonomyDrift   = Math.abs(currentAutonomy - base.autonomy_score);
  const principleDrift  = distributionDelta(base.principle_dist, currentDist);

  // Weighted composite drift score
  const driftScore = (
    confidenceDrift * 0.30 +
    conflictDrift   * 0.30 +
    autonomyDrift   * 0.20 +
    principleDrift  * 0.20
  );

  const alert = driftScore > driftThreshold;

  // Identify the dominant drift signal
  const signals = [
    { name: 'confidence',  delta: confidenceDrift,  direction: currentConfidence < base.avg_confidence ? 'declining' : 'rising' },
    { name: 'conflict',    delta: conflictDrift,     direction: currentConflictRate > base.conflict_rate ? 'rising' : 'declining' },
    { name: 'autonomy',    delta: autonomyDrift,     direction: currentAutonomy < base.autonomy_score ? 'declining' : 'rising' },
    { name: 'principles',  delta: principleDrift,    direction: 'shifted' },
  ].sort((a, b) => b.delta - a.delta);

  return {
    ok: !alert,
    alert,
    drift_score: Math.round(driftScore * 1000) / 1000,
    threshold: driftThreshold,
    dominant_signal: signals[0],
    signals,
    current: {
      window: recent.length,
      avg_confidence: Math.round(currentConfidence * 1000) / 1000,
      conflict_rate: Math.round(currentConflictRate * 1000) / 1000,
      autonomy_score: Math.round(currentAutonomy * 1000) / 1000,
      principle_dist: currentDist,
      window_start: recent[0]?.ts,
      window_end: recent[recent.length - 1]?.ts,
    },
    baseline: {
      window: base.count,
      avg_confidence: Math.round(base.avg_confidence * 1000) / 1000,
      conflict_rate: Math.round(base.conflict_rate * 1000) / 1000,
      autonomy_score: Math.round(base.autonomy_score * 1000) / 1000,
      window_start: base.window_start,
      window_end: base.window_end,
    },
  };
}

/**
 * Produce a human-readable drift report string.
 */
export function driftReport(result) {
  const { alert, drift_score, threshold, dominant_signal, current, baseline } = result;
  const status = alert ? '⚠️  DRIFT ALERT' : '✓  Within baseline';
  const lines = [
    `${status} | score=${drift_score.toFixed(3)} (threshold=${threshold})`,
    `  Current window: ${current?.window} entries | conf=${current?.avg_confidence} | conflict=${current?.conflict_rate} | autonomy=${current?.autonomy_score}`,
    `  Baseline:       ${baseline?.window} entries | conf=${baseline?.avg_confidence} | conflict=${baseline?.conflict_rate} | autonomy=${baseline?.autonomy_score}`,
  ];
  if (alert && dominant_signal) {
    lines.push(`  Dominant signal: ${dominant_signal.name} ${dominant_signal.direction} (Δ=${dominant_signal.delta.toFixed(3)})`);
  }
  return lines.join('\n');
}

/**
 * Convenience: create a detector bound to a specific journal + agent.
 */
export class IntentDriftDetector {
  constructor({ journal, agent = null, windowSize = 20, baselineWindow = 50, driftThreshold = 0.25 } = {}) {
    if (!journal) throw new Error('IntentDriftDetector: journal is required');
    this.journal = journal;
    this.agent = agent;
    this.windowSize = windowSize;
    this.baselineWindow = baselineWindow;
    this.driftThreshold = driftThreshold;
    this._baseline = null;
  }

  /** Snapshot current distribution as baseline. Call at startup or after a known-good period. */
  captureBaseline() {
    this._baseline = buildBaseline({
      journal: this.journal,
      agent: this.agent,
      windowSize: this.baselineWindow,
    });
    return this._baseline;
  }

  /** Check for drift against captured (or fresh) baseline. */
  check() {
    return detectDrift({
      journal: this.journal,
      agent: this.agent,
      windowSize: this.windowSize,
      baseline: this._baseline,
      baselineWindow: this.baselineWindow,
      driftThreshold: this.driftThreshold,
    });
  }

  /** Human-readable report. */
  report() {
    return driftReport(this.check());
  }
}
