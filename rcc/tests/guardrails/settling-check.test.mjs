/**
 * Tests for settling-check.mjs (CoherenceGate Phase 1)
 *
 * Run: node --test rcc/tests/guardrails/settling-check.test.mjs
 */

import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import { checkSettling, checkFalseCapability, coherenceCheck } from '../../guardrails/settling-check.mjs';

// ── Settling Detection ────────────────────────────────────────────────────────

describe('checkSettling()', () => {
  test('returns false for clean response', () => {
    const result = checkSettling("Here's the file content you requested.");
    assert.equal(result.settling, false);
    assert.deepEqual(result.patterns, []);
  });

  test('detects "I am unable to"', () => {
    const result = checkSettling("I am unable to access that file.");
    assert.equal(result.settling, true);
    assert.ok(result.patterns.length > 0);
  });

  test('detects "I cannot fetch"', () => {
    const result = checkSettling("I cannot fetch that URL.");
    assert.equal(result.settling, true);
  });

  test("detects \"I can't access\"", () => {
    const result = checkSettling("I can't access the database.");
    assert.equal(result.settling, true);
  });

  test('detects "I don\'t have access"', () => {
    const result = checkSettling("I don't have access to that resource.");
    assert.equal(result.settling, true);
  });

  test('detects "Unfortunately, I can\'t"', () => {
    const result = checkSettling("Unfortunately, I can't perform that action.");
    assert.equal(result.settling, true);
  });

  test('detects "beyond my capabilities"', () => {
    const result = checkSettling("This is beyond my capabilities.");
    assert.equal(result.settling, true);
  });

  test('detects "I lack the capability"', () => {
    const result = checkSettling("I lack the capability to do that.");
    assert.equal(result.settling, true);
  });

  test('detects "I\'m afraid I cannot"', () => {
    const result = checkSettling("I'm afraid I cannot complete this task.");
    assert.equal(result.settling, true);
  });

  test('allowlists legitimate credential limits', () => {
    const result = checkSettling("I don't have access to the API password.");
    assert.equal(result.allowlisted, true);
    assert.equal(result.settling, false, 'Credential limits should not be flagged as settling');
  });

  test('allowlists sudo/root access requirement', () => {
    const result = checkSettling("I cannot perform this because it requires sudo access.");
    assert.equal(result.allowlisted, true);
    assert.equal(result.settling, false);
  });

  test('handles null/undefined input', () => {
    assert.deepEqual(checkSettling(null), { settling: false, patterns: [], allowlisted: false });
    assert.deepEqual(checkSettling(undefined), { settling: false, patterns: [], allowlisted: false });
    assert.deepEqual(checkSettling(''), { settling: false, patterns: [], allowlisted: false });
  });
});

// ── False Capability Detection ────────────────────────────────────────────────

describe('checkFalseCapability()', () => {
  test('returns false for normal response', () => {
    const result = checkFalseCapability("I'll send you the message now.");
    assert.equal(result.suspected, false);
  });

  test('detects false claim about sending', () => {
    const result = checkFalseCapability("I can't send messages directly.");
    assert.equal(result.suspected, true);
  });

  test('detects false claim about searching', () => {
    const result = checkFalseCapability("I'm unable to search the web.");
    assert.equal(result.suspected, true);
  });

  test('handles null input', () => {
    const result = checkFalseCapability(null);
    assert.equal(result.suspected, false);
  });
});

// ── Combined Coherence Check ──────────────────────────────────────────────────

describe('coherenceCheck()', () => {
  test('passes clean response', () => {
    const result = coherenceCheck("I've completed the task successfully.");
    assert.equal(result.pass, true);
    assert.deepEqual(result.issues, []);
  });

  test('fails settling response', () => {
    const result = coherenceCheck("I am unable to complete this task.");
    assert.equal(result.pass, false);
    assert.ok(result.issues.some(i => i.type === 'settling'));
  });

  test('fails false capability response', () => {
    const result = coherenceCheck("I cannot send messages to Slack.");
    assert.equal(result.pass, false);
  });

  test('truncates response in output to 200 chars', () => {
    const longResponse = 'x'.repeat(500);
    const result = coherenceCheck(longResponse);
    assert.ok(result.response.length <= 200);
  });

  test('settling issues have severity warn', () => {
    const result = coherenceCheck("I am unable to access that resource.");
    const settling = result.issues.find(i => i.type === 'settling');
    assert.ok(settling, 'Expected settling issue');
    assert.equal(settling.severity, 'warn');
  });
});
