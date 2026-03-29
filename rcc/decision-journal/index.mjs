/**
 * DecisionJournal — prerequisite for IntentDriftDetector
 *
 * Logs every significant agent decision to rcc/logs/decision-journal.jsonl.
 * Each entry: { ts, agent, principle_used, confidence, was_conflict, outcome, context }
 *
 * Usage:
 *   import { DecisionJournal } from './rcc/decision-journal/index.mjs';
 *   const journal = new DecisionJournal({ agent: 'rocky' });
 *   await journal.log({ principle_used: 'fail-safe', confidence: 0.9, was_conflict: false, outcome: 'skipped' });
 *
 * CLI usage:
 *   echo '{"agent":"rocky","principle_used":"test","confidence":0.8,"was_conflict":false,"outcome":"ok"}' | node index.mjs
 */

import { appendFileSync, mkdirSync, existsSync, readFileSync } from 'fs';
import { join, dirname } from 'path';
import { fileURLToPath } from 'url';

const __dirname = dirname(fileURLToPath(import.meta.url));

// Default log path: ~/.rcc/logs/decision-journal.jsonl
const DEFAULT_LOG = join(process.env.HOME || '/root', '.rcc', 'logs', 'decision-journal.jsonl');

export class DecisionJournal {
  /**
   * @param {object} opts
   * @param {string} opts.agent         - Agent name (e.g. 'rocky', 'natasha')
   * @param {string} [opts.logPath]     - Override log file path
   * @param {boolean} [opts.silent]     - Suppress console output
   */
  constructor({ agent, logPath, silent = false } = {}) {
    if (!agent) throw new Error('DecisionJournal: agent is required');
    this.agent = agent;
    this.logPath = logPath || DEFAULT_LOG;
    this.silent = silent;
    this._ensureDir();
  }

  _ensureDir() {
    const dir = dirname(this.logPath);
    if (!existsSync(dir)) mkdirSync(dir, { recursive: true });
  }

  /**
   * Log a decision entry.
   * @param {object} entry
   * @param {string} entry.principle_used  - Guiding principle (e.g. 'fail-safe', 'minimal-footprint')
   * @param {number} entry.confidence      - 0.0–1.0 confidence in this decision
   * @param {boolean} entry.was_conflict   - Did this conflict with another principle?
   * @param {string} entry.outcome         - Result: 'proceed', 'skipped', 'escalated', 'blocked', 'ok', etc.
   * @param {string} [entry.context]       - Optional freeform context string
   * @param {string} [entry.task_id]       - Optional workqueue item ID
   * @returns {object} The logged entry
   */
  log({ principle_used, confidence, was_conflict, outcome, context, task_id } = {}) {
    if (!principle_used) throw new Error('principle_used is required');
    if (typeof confidence !== 'number' || confidence < 0 || confidence > 1) {
      throw new Error('confidence must be a number 0.0–1.0');
    }
    if (typeof was_conflict !== 'boolean') throw new Error('was_conflict must be boolean');
    if (!outcome) throw new Error('outcome is required');

    const entry = {
      ts: new Date().toISOString(),
      agent: this.agent,
      principle_used,
      confidence,
      was_conflict,
      outcome,
      ...(context ? { context } : {}),
      ...(task_id ? { task_id } : {}),
    };

    appendFileSync(this.logPath, JSON.stringify(entry) + '\n');
    if (!this.silent) {
      console.log(`[DecisionJournal] ${entry.ts} ${this.agent} | ${principle_used} | conf=${confidence.toFixed(2)} | conflict=${was_conflict} | ${outcome}`);
    }
    return entry;
  }

  /**
   * Read recent entries for this agent.
   * @param {object} opts
   * @param {number} [opts.limit=100]  - Max entries to return
   * @param {string} [opts.agent]      - Filter by agent (default: this.agent, pass null for all)
   * @param {number} [opts.sinceMs]    - Only entries newer than this epoch ms
   * @returns {object[]}
   */
  getRecent({ limit = 100, agent, sinceMs } = {}) {
    if (!existsSync(this.logPath)) return [];
    const filterAgent = agent !== null ? (agent ?? this.agent) : null;
    const lines = readFileSync(this.logPath, 'utf8')
      .split('\n')
      .filter(Boolean)
      .map(l => { try { return JSON.parse(l); } catch { return null; } })
      .filter(Boolean);

    return lines
      .filter(e => !filterAgent || e.agent === filterAgent)
      .filter(e => !sinceMs || new Date(e.ts).getTime() >= sinceMs)
      .slice(-limit);
  }

  /**
   * Compute basic stats over recent entries.
   * Useful for IntentDriftDetector.
   * @param {object} opts  (same as getRecent)
   * @returns {object} stats
   */
  stats(opts = {}) {
    const entries = this.getRecent(opts);
    if (entries.length === 0) return { count: 0 };

    const conflictCount = entries.filter(e => e.was_conflict).length;
    const avgConfidence = entries.reduce((s, e) => s + e.confidence, 0) / entries.length;
    const principleFreq = {};
    for (const e of entries) {
      principleFreq[e.principle_used] = (principleFreq[e.principle_used] || 0) + 1;
    }
    const topPrinciples = Object.entries(principleFreq)
      .sort(([, a], [, b]) => b - a)
      .slice(0, 5)
      .map(([p, n]) => ({ principle: p, count: n }));

    return {
      count: entries.length,
      conflict_rate: conflictCount / entries.length,
      avg_confidence: Math.round(avgConfidence * 1000) / 1000,
      top_principles: topPrinciples,
    };
  }
}

// ── CLI shim ──────────────────────────────────────────────────────────────────
// Usage: echo '<json>' | node index.mjs
if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  let buf = '';
  process.stdin.on('data', c => (buf += c));
  process.stdin.on('end', () => {
    try {
      const entry = JSON.parse(buf.trim());
      if (!entry.agent) { console.error('agent field required'); process.exit(1); }
      const j = new DecisionJournal({ agent: entry.agent, silent: false });
      const { agent: _a, ...rest } = entry;
      j.log(rest);
    } catch (e) {
      console.error('DecisionJournal CLI error:', e.message);
      process.exit(1);
    }
  });
}
