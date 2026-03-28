/**
 * Tests for ExternalOperationGate (instar adoption)
 *
 * Run: node --test rcc/tests/guardrails/external-operation-gate.test.mjs
 */

import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import { computeRiskLevel, scopeFromCount, AUTONOMY_PROFILES, ExternalOperationGate } from '../../guardrails/external-operation-gate.mjs';

// ── Risk Matrix Tests ────────────────────────────────────────────────────────

describe('computeRiskLevel()', () => {
  test('reads are always low', () => {
    assert.equal(computeRiskLevel('read', 'irreversible', 'bulk'), 'low');
    assert.equal(computeRiskLevel('read', 'reversible', 'single'), 'low');
    assert.equal(computeRiskLevel('read', 'partially-reversible', 'batch'), 'low');
  });

  test('bulk + irreversible = critical', () => {
    assert.equal(computeRiskLevel('write', 'irreversible', 'bulk'), 'critical');
    assert.equal(computeRiskLevel('modify', 'irreversible', 'bulk'), 'critical');
  });

  test('bulk + delete = critical regardless of reversibility', () => {
    assert.equal(computeRiskLevel('delete', 'reversible', 'bulk'), 'critical');
    assert.equal(computeRiskLevel('delete', 'irreversible', 'bulk'), 'critical');
  });

  test('any bulk = critical', () => {
    assert.equal(computeRiskLevel('write', 'reversible', 'bulk'), 'critical');
  });

  test('batch + delete = high', () => {
    assert.equal(computeRiskLevel('delete', 'reversible', 'batch'), 'high');
  });

  test('batch + irreversible = high', () => {
    assert.equal(computeRiskLevel('write', 'irreversible', 'batch'), 'high');
  });

  test('single delete + irreversible = high', () => {
    assert.equal(computeRiskLevel('delete', 'irreversible', 'single'), 'high');
  });

  test('single delete + reversible = medium', () => {
    assert.equal(computeRiskLevel('delete', 'reversible', 'single'), 'medium');
  });

  test('single irreversible write = medium', () => {
    assert.equal(computeRiskLevel('write', 'irreversible', 'single'), 'medium');
  });

  test('batch reversible write = medium', () => {
    assert.equal(computeRiskLevel('write', 'reversible', 'batch'), 'medium');
  });

  test('single reversible write = low', () => {
    assert.equal(computeRiskLevel('write', 'reversible', 'single'), 'low');
  });
});

// ── Scope From Count ─────────────────────────────────────────────────────────

describe('scopeFromCount()', () => {
  test('1 item = single', () => assert.equal(scopeFromCount(1), 'single'));
  test('0 items = single', () => assert.equal(scopeFromCount(0), 'single'));
  test('undefined = single', () => assert.equal(scopeFromCount(), 'single'));
  test('5 items = batch (default threshold)', () => assert.equal(scopeFromCount(5), 'batch'));
  test('19 items = batch', () => assert.equal(scopeFromCount(19), 'batch'));
  test('20 items = bulk', () => assert.equal(scopeFromCount(20), 'bulk'));
  test('100 items = bulk', () => assert.equal(scopeFromCount(100), 'bulk'));
  test('respects custom thresholds', () => {
    assert.equal(scopeFromCount(3, { batchThreshold: 3, bulkThreshold: 10 }), 'batch');
    assert.equal(scopeFromCount(10, { batchThreshold: 3, bulkThreshold: 10 }), 'bulk');
  });
});

// ── Autonomy Profiles ─────────────────────────────────────────────────────────

describe('AUTONOMY_PROFILES', () => {
  test('supervised profile: low=log, high/critical=approve or block', () => {
    assert.equal(AUTONOMY_PROFILES.supervised.low, 'log');
    assert.equal(AUTONOMY_PROFILES.supervised.critical, 'block');
  });

  test('collaborative profile: low=proceed, high=approve', () => {
    assert.equal(AUTONOMY_PROFILES.collaborative.low, 'proceed');
    assert.equal(AUTONOMY_PROFILES.collaborative.high, 'approve');
    assert.equal(AUTONOMY_PROFILES.collaborative.critical, 'approve');
  });

  test('autonomous profile: low=proceed, medium=proceed, high=log', () => {
    assert.equal(AUTONOMY_PROFILES.autonomous.low, 'proceed');
    assert.equal(AUTONOMY_PROFILES.autonomous.medium, 'proceed');
    assert.equal(AUTONOMY_PROFILES.autonomous.high, 'log');
    assert.equal(AUTONOMY_PROFILES.autonomous.critical, 'approve');
  });

  test('autonomous profile: trust CANNOT auto-escalate past critical', () => {
    // Critical always requires approval — even in autonomous mode
    assert.equal(AUTONOMY_PROFILES.autonomous.critical, 'approve');
    assert.notEqual(AUTONOMY_PROFILES.autonomous.critical, 'proceed');
  });
});

// ── Gate Decisions ────────────────────────────────────────────────────────────

describe('ExternalOperationGate.evaluate()', () => {
  const gate = new ExternalOperationGate({ profile: 'collaborative' });

  test('read operation always proceeds', () => {
    const result = gate.evaluate({ service: 'github', mutability: 'read', reversibility: 'reversible', itemCount: 1 });
    assert.equal(result.action, 'proceed');
    assert.equal(result.riskLevel, 'low');
  });

  test('single reversible write proceeds', () => {
    const result = gate.evaluate({ service: 'minio', mutability: 'write', reversibility: 'reversible', itemCount: 1 });
    assert.equal(result.action, 'proceed');
  });

  test('bulk operation requires a plan (>= bulkThreshold items)', () => {
    const result = gate.evaluate({ service: 'github', mutability: 'delete', reversibility: 'reversible', itemCount: 25 });
    assert.equal(result.action, 'show-plan');
    assert.ok(result.checkpoint);
    assert.equal(result.checkpoint.totalExpected, 25);
  });

  test('hard-blocked operation returns block', () => {
    // Slack has 'delete' blocked
    const result = gate.evaluate({ service: 'slack', mutability: 'delete', reversibility: 'reversible', itemCount: 1 });
    assert.equal(result.action, 'block');
    assert.match(result.reason, /hard-blocked/);
  });

  test('require-approval operation returns show-plan', () => {
    // GitHub requires approval for deletes
    const result = gate.evaluate({ service: 'github', mutability: 'delete', reversibility: 'reversible', itemCount: 1 });
    assert.equal(result.action, 'show-plan');
  });

  test('collaborative profile blocks critical-risk in autonomy check', () => {
    // No service override — custom service with no blocks, bulk irreversible
    const openGate = new ExternalOperationGate({ 
      profile: 'collaborative',
      services: { custom: { permissions: ['delete'], blocked: [], requireApproval: [] } }
    });
    const result = openGate.evaluate({ service: 'custom', mutability: 'delete', reversibility: 'irreversible', itemCount: 1 });
    // single + irreversible delete = high; collaborative high=approve → show-plan
    assert.equal(result.action, 'show-plan');
  });

  test('supervised profile logs low-risk ops', () => {
    const supervised = new ExternalOperationGate({ 
      profile: 'supervised',
      services: { safe: { permissions: ['write'], blocked: [], requireApproval: [] } }
    });
    const result = supervised.evaluate({ service: 'safe', mutability: 'write', reversibility: 'reversible', itemCount: 1 });
    assert.equal(result.action, 'proceed');
    assert.equal(result.logged, true);
  });

  test('supervised profile blocks critical-risk', () => {
    const supervised = new ExternalOperationGate({
      profile: 'supervised',
      services: { unsafe: { permissions: ['delete'], blocked: [], requireApproval: [] } }
    });
    const result = supervised.evaluate({ service: 'unsafe', mutability: 'write', reversibility: 'irreversible', scope: 'bulk', itemCount: 5 });
    assert.equal(result.action, 'block');
  });

  test('result includes riskLevel in all cases', () => {
    const result = gate.evaluate({ service: 'github', mutability: 'read', reversibility: 'reversible' });
    assert.ok(result.riskLevel, 'riskLevel should be present');
  });

  test('infers scope from itemCount when scope not provided', () => {
    const result = gate.evaluate({ service: 'slack', mutability: 'write', reversibility: 'reversible', itemCount: 50 });
    // 50 items = bulk
    assert.ok(['show-plan', 'block'].includes(result.action), `Expected show-plan or block for bulk, got ${result.action}`);
  });

  test('telegram delete is hard-blocked', () => {
    const result = gate.evaluate({ service: 'telegram', mutability: 'delete', reversibility: 'reversible', itemCount: 1 });
    assert.equal(result.action, 'block');
  });

  test('exec write requires approval', () => {
    const result = gate.evaluate({ service: 'exec', mutability: 'write', reversibility: 'reversible', itemCount: 1 });
    assert.equal(result.action, 'show-plan');
  });
});

// ── Checkpoint Config ─────────────────────────────────────────────────────────

describe('Bulk checkpoint', () => {
  const gate = new ExternalOperationGate({ profile: 'autonomous' });

  test('bulk op provides checkpoint with checkpointEvery', () => {
    const result = gate.evaluate({ service: 'minio', mutability: 'write', reversibility: 'reversible', itemCount: 100 });
    assert.ok(result.checkpoint, 'checkpoint should be present for bulk ops');
    assert.equal(result.checkpoint.totalExpected, 100);
    assert.equal(result.checkpoint.completedSoFar, 0);
    assert.equal(result.checkpoint.afterCount, 10); // default checkpointEvery
  });
});
