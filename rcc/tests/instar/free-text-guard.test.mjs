/**
 * Tests for .claude/hooks/free-text-guard.sh (instar item wq-INSTAR-*-09)
 *
 * Interface: echo '<json>' | bash .claude/hooks/free-text-guard.sh
 *   exit 0 — allow (decision question or no questions)
 *   exit 2 — block (free-text input question detected)
 *
 * Coverage: free-text patterns (password, api key, token, etc.),
 * decision patterns (which, prefer, choose), edge cases.
 */

import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { chmodSync } from 'node:fs';

const __dirname = fileURLToPath(new URL('.', import.meta.url));
const HOOK = resolve(__dirname, '../../../.claude/hooks/free-text-guard.sh');

function runHook(questions) {
  chmodSync(HOOK, 0o755);
  const input = JSON.stringify({
    tool_name: 'AskUserQuestion',
    tool_input: { questions: questions.map(q => ({ question: q })) },
  });
  const result = spawnSync('bash', [HOOK], {
    input,
    env: process.env,
    encoding: 'utf-8',
    timeout: 5000,
  });
  return { exitCode: result.status, stderr: result.stderr || '', stdout: result.stdout || '' };
}

describe('free-text-guard.sh — BLOCK: free-text patterns', () => {

  test('blocks "Enter your API key:"', () => {
    const r = runHook(['Enter your API key:']);
    assert.equal(r.exitCode, 2, `Expected block, got ${r.exitCode}: ${r.stderr}`);
    assert.ok(r.stderr.includes('BLOCKED'));
  });

  test('blocks "What is your password?"', () => {
    const r = runHook(['What is your password?']);
    assert.equal(r.exitCode, 2);
  });

  test('blocks "Please provide your auth token"', () => {
    const r = runHook(['Please provide your auth token']);
    assert.equal(r.exitCode, 2);
  });

  test('blocks "Enter your API key" (case variations)', () => {
    const r = runHook(['Enter your API Key']);
    assert.equal(r.exitCode, 2);
  });

  test('blocks credential requests', () => {
    const r = runHook(['Please enter your credentials to continue']);
    assert.equal(r.exitCode, 2);
  });

  test('blocks access token requests', () => {
    const r = runHook(['What is your access token?']);
    assert.equal(r.exitCode, 2);
  });
});

describe('free-text-guard.sh — ALLOW: decision/choice patterns', () => {

  test('allows "Which approach would you prefer?"', () => {
    const r = runHook(['Which approach would you prefer?']);
    assert.equal(r.exitCode, 0, `Expected allow, got ${r.exitCode}: ${r.stderr}`);
  });

  test('allows "Would you like to proceed with option A or B?"', () => {
    const r = runHook(['Would you like to proceed with option A or B?']);
    assert.equal(r.exitCode, 0);
  });

  test('allows "Should we deploy to staging or production?"', () => {
    const r = runHook(['Should we deploy to staging or production?']);
    assert.equal(r.exitCode, 0);
  });

  test('allows "How should we handle the merge conflict?"', () => {
    const r = runHook(['How should we handle the merge conflict?']);
    assert.equal(r.exitCode, 0);
  });

  test('allows "Pick one: fast or thorough?"', () => {
    const r = runHook(['Pick one: fast or thorough?']);
    assert.equal(r.exitCode, 0);
  });
});

describe('free-text-guard.sh — edge cases', () => {

  test('allows empty questions array', () => {
    const r = runHook([]);
    assert.equal(r.exitCode, 0);
  });

  test('allows when no tool_input provided', () => {
    const input = JSON.stringify({ tool_name: 'AskUserQuestion', tool_input: {} });
    const result = spawnSync('bash', [HOOK], {
      input, env: process.env, encoding: 'utf-8', timeout: 5000,
    });
    assert.equal(result.status, 0);
  });

  test('allows malformed JSON (fails gracefully)', () => {
    const result = spawnSync('bash', [HOOK], {
      input: 'not valid json', env: process.env, encoding: 'utf-8', timeout: 5000,
    });
    assert.equal(result.status, 0, 'malformed JSON should allow (fail-open)');
  });

  test('blocks on multiple questions when any is free-text', () => {
    const r = runHook([
      'Which environment do you prefer?',  // decision — would allow
      'Enter your API key:',                // free-text — should block
    ]);
    assert.equal(r.exitCode, 2, 'any free-text question in list should trigger block');
  });

  test('allows multiple decision questions', () => {
    const r = runHook([
      'Would you prefer A or B?',
      'Should we use option 1 or 2?',
    ]);
    assert.equal(r.exitCode, 0);
  });
});
