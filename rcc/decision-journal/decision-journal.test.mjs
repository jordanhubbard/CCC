/**
 * DecisionJournal tests
 * Run: node --test rcc/decision-journal/decision-journal.test.mjs
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, rmSync, existsSync, readFileSync } from 'fs';
import { join } from 'path';
import { tmpdir } from 'os';
import { DecisionJournal } from './index.mjs';

function tmpJournal() {
  const dir = mkdtempSync(join(tmpdir(), 'dj-test-'));
  const logPath = join(dir, 'decision-journal.jsonl');
  return { dir, logPath, j: new DecisionJournal({ agent: 'test-agent', logPath, silent: true }) };
}

test('constructor rejects missing agent', () => {
  assert.throws(() => new DecisionJournal({}), /agent is required/);
});

test('log writes a valid JSONL entry', () => {
  const { j, logPath, dir } = tmpJournal();
  j.log({ principle_used: 'fail-safe', confidence: 0.9, was_conflict: false, outcome: 'proceed' });
  const lines = readFileSync(logPath, 'utf8').trim().split('\n');
  assert.equal(lines.length, 1);
  const e = JSON.parse(lines[0]);
  assert.equal(e.agent, 'test-agent');
  assert.equal(e.principle_used, 'fail-safe');
  assert.equal(e.confidence, 0.9);
  assert.equal(e.was_conflict, false);
  assert.equal(e.outcome, 'proceed');
  assert.ok(e.ts);
  rmSync(dir, { recursive: true });
});

test('log returns the entry', () => {
  const { j, dir } = tmpJournal();
  const e = j.log({ principle_used: 'minimal-footprint', confidence: 0.7, was_conflict: true, outcome: 'skipped', context: 'ctx', task_id: 'wq-123' });
  assert.equal(e.principle_used, 'minimal-footprint');
  assert.equal(e.task_id, 'wq-123');
  assert.equal(e.context, 'ctx');
  rmSync(dir, { recursive: true });
});

test('log validates confidence range', () => {
  const { j, dir } = tmpJournal();
  assert.throws(() => j.log({ principle_used: 'p', confidence: 1.5, was_conflict: false, outcome: 'ok' }), /confidence/);
  assert.throws(() => j.log({ principle_used: 'p', confidence: -0.1, was_conflict: false, outcome: 'ok' }), /confidence/);
  rmSync(dir, { recursive: true });
});

test('log requires was_conflict boolean', () => {
  const { j, dir } = tmpJournal();
  assert.throws(() => j.log({ principle_used: 'p', confidence: 0.5, was_conflict: 'yes', outcome: 'ok' }), /was_conflict/);
  rmSync(dir, { recursive: true });
});

test('log requires outcome', () => {
  const { j, dir } = tmpJournal();
  assert.throws(() => j.log({ principle_used: 'p', confidence: 0.5, was_conflict: false }), /outcome/);
  rmSync(dir, { recursive: true });
});

test('getRecent returns entries for this agent', () => {
  const { j, dir } = tmpJournal();
  j.log({ principle_used: 'p1', confidence: 0.8, was_conflict: false, outcome: 'ok' });
  j.log({ principle_used: 'p2', confidence: 0.6, was_conflict: true, outcome: 'skipped' });
  const entries = j.getRecent();
  assert.equal(entries.length, 2);
  assert.equal(entries[0].principle_used, 'p1');
  rmSync(dir, { recursive: true });
});

test('getRecent filters by agent', () => {
  const { logPath, dir } = tmpJournal();
  const j1 = new DecisionJournal({ agent: 'rocky', logPath, silent: true });
  const j2 = new DecisionJournal({ agent: 'natasha', logPath, silent: true });
  j1.log({ principle_used: 'p1', confidence: 0.8, was_conflict: false, outcome: 'ok' });
  j2.log({ principle_used: 'p2', confidence: 0.6, was_conflict: false, outcome: 'ok' });
  const r = j1.getRecent({ agent: 'rocky' });
  assert.equal(r.length, 1);
  assert.equal(r[0].principle_used, 'p1');
  const all = j1.getRecent({ agent: null });
  assert.equal(all.length, 2);
  rmSync(dir, { recursive: true });
});

test('getRecent respects sinceMs', () => {
  const { j, dir } = tmpJournal();
  j.log({ principle_used: 'p1', confidence: 0.8, was_conflict: false, outcome: 'ok' });
  const future = Date.now() + 60000;
  const r = j.getRecent({ sinceMs: future });
  assert.equal(r.length, 0);
  rmSync(dir, { recursive: true });
});

test('getRecent returns empty when file missing', () => {
  const { j, dir } = tmpJournal();
  // No log written yet
  const r = j.getRecent();
  assert.equal(r.length, 0);
  rmSync(dir, { recursive: true });
});

test('stats computes conflict_rate and avg_confidence', () => {
  const { j, dir } = tmpJournal();
  j.log({ principle_used: 'fail-safe', confidence: 0.9, was_conflict: false, outcome: 'ok' });
  j.log({ principle_used: 'fail-safe', confidence: 0.7, was_conflict: true, outcome: 'skipped' });
  j.log({ principle_used: 'minimal', confidence: 0.8, was_conflict: false, outcome: 'ok' });
  const s = j.stats();
  assert.equal(s.count, 3);
  assert.ok(Math.abs(s.conflict_rate - 1/3) < 0.001);
  assert.ok(Math.abs(s.avg_confidence - 0.8) < 0.001);
  assert.equal(s.top_principles[0].principle, 'fail-safe');
  assert.equal(s.top_principles[0].count, 2);
  rmSync(dir, { recursive: true });
});

test('stats returns count=0 for empty journal', () => {
  const { j, dir } = tmpJournal();
  assert.deepEqual(j.stats(), { count: 0 });
  rmSync(dir, { recursive: true });
});

test('multiple agents, limit respected', () => {
  const { j, dir } = tmpJournal();
  for (let i = 0; i < 5; i++) {
    j.log({ principle_used: `p${i}`, confidence: 0.5, was_conflict: false, outcome: 'ok' });
  }
  const r = j.getRecent({ limit: 3 });
  assert.equal(r.length, 3);
  rmSync(dir, { recursive: true });
});
