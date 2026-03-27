/**
 * Tests for scripts/serendipity-capture.sh (instar item wq-INSTAR-*-07)
 *
 * Tests validation logic (field requirements, category/readiness enums,
 * length limits, secret scanning, rate limiting) without hitting live RCC.
 *
 * For integration test (actual RCC post), see rcc/tests/integration.test.mjs.
 */

import { test, describe, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { mkdtempSync, rmSync, writeFileSync, chmodSync } from 'node:fs';
import { tmpdir, homedir } from 'node:os';
import { join } from 'node:path';

const __dirname = fileURLToPath(new URL('.', import.meta.url));
const SCRIPT = resolve(__dirname, '../../../scripts/serendipity-capture.sh');

// Required env — use a fake token to avoid hitting live RCC, but we need a valid URL
// Tests that would POST to RCC are skipped here; we test validation only.
const BASE_ENV = {
  ...process.env,
  RCC_AGENT_TOKEN: 'test-fake-token-for-validation-only',
  RCC_URL: 'http://127.0.0.1:19999', // non-listening port → connection refused → fast fail
  AGENT_NAME: 'test-natasha',
  CLAUDE_SESSION_ID: `test-session-${Date.now()}`,
};

function run(args, env = {}) {
  chmodSync(SCRIPT, 0o755);
  const result = spawnSync('bash', [SCRIPT, ...args], {
    env: { ...BASE_ENV, ...env },
    encoding: 'utf-8',
    timeout: 5000,
  });
  return { exitCode: result.status, stdout: result.stdout || '', stderr: result.stderr || '' };
}

function validArgs() {
  return [
    '--title', 'Test finding title',
    '--description', 'A description of the finding that is informative',
    '--category', 'improvement',
    '--rationale', 'This matters because it improves the system',
    '--readiness', 'idea-only',
  ];
}

describe('serendipity-capture.sh — required field validation', () => {

  test('exits non-zero when --title is missing', () => {
    const args = validArgs().filter((_, i, a) => a[i-1] !== '--title' && _ !== '--title' && (a[i-1] === '--title' ? false : true));
    // Simpler: just omit title
    const r = run(['--description', 'desc', '--category', 'improvement', '--rationale', 'reason', '--readiness', 'idea-only']);
    assert.notEqual(r.exitCode, 0, `Expected non-zero exit: ${r.stderr}`);
    assert.ok(r.stderr.includes('title'), `Expected title error: ${r.stderr}`);
  });

  test('exits non-zero when --description is missing', () => {
    const r = run(['--title', 'A title', '--category', 'bug', '--rationale', 'reason', '--readiness', 'idea-only']);
    assert.notEqual(r.exitCode, 0);
    assert.ok(r.stderr.includes('description'));
  });

  test('exits non-zero when --category is missing', () => {
    const r = run(['--title', 'A title', '--description', 'desc', '--rationale', 'reason', '--readiness', 'idea-only']);
    assert.notEqual(r.exitCode, 0);
    assert.ok(r.stderr.includes('category'));
  });

  test('exits non-zero when --rationale is missing', () => {
    const r = run(['--title', 'A title', '--description', 'desc', '--category', 'bug', '--readiness', 'idea-only']);
    assert.notEqual(r.exitCode, 0);
    assert.ok(r.stderr.includes('rationale'));
  });

  test('exits non-zero when --readiness is missing', () => {
    const r = run(['--title', 'A title', '--description', 'desc', '--category', 'bug', '--rationale', 'reason']);
    assert.notEqual(r.exitCode, 0);
    assert.ok(r.stderr.includes('readiness'));
  });
});

describe('serendipity-capture.sh — category enum validation', () => {

  test('accepts all valid categories', () => {
    const valid = ['bug', 'improvement', 'feature', 'pattern', 'refactor', 'security'];
    for (const cat of valid) {
      const r = run([...validArgs().map((v, i, a) => a[i-1] === '--category' ? cat : v)]);
      // Will fail at RCC post (connection refused) but should NOT fail at category validation
      // Category validation exits 1 with "must be one of" — connection error exits 1 too
      // but won't have the category error message
      if (r.exitCode !== 0) {
        assert.ok(!r.stderr.includes('--category must be one of'),
          `Category "${cat}" should be valid but got: ${r.stderr.slice(0, 100)}`);
      }
    }
  });

  test('rejects invalid category', () => {
    const args = validArgs();
    const idx = args.indexOf('improvement');
    args[idx] = 'invalid-category';
    const r = run(args);
    assert.notEqual(r.exitCode, 0);
    assert.ok(r.stderr.includes('--category must be one of'), `stderr: ${r.stderr}`);
  });
});

describe('serendipity-capture.sh — readiness enum validation', () => {

  test('accepts all valid readiness values', () => {
    const valid = ['idea-only', 'partially-implemented', 'implementation-complete', 'tested'];
    for (const readiness of valid) {
      const args = validArgs();
      const idx = args.indexOf('idea-only');
      if (idx >= 0) args[idx] = readiness;
      const r = run(args);
      if (r.exitCode !== 0) {
        assert.ok(!r.stderr.includes('--readiness must be one of'),
          `Readiness "${readiness}" should be valid but got: ${r.stderr.slice(0, 100)}`);
      }
    }
  });

  test('rejects invalid readiness value', () => {
    const args = validArgs();
    const idx = args.indexOf('idea-only');
    args[idx] = 'totally-done';
    const r = run(args);
    assert.notEqual(r.exitCode, 0);
    assert.ok(r.stderr.includes('--readiness must be one of'), `stderr: ${r.stderr}`);
  });
});

describe('serendipity-capture.sh — secret scanning', () => {

  test('blocks AWS access key in title', () => {
    const args = validArgs();
    args[args.indexOf('Test finding title')] = 'AKIAIOSFODNN7EXAMPLE found in config';
    const r = run(args);
    assert.notEqual(r.exitCode, 0);
    assert.ok(r.stderr.includes('secret') || r.stderr.includes('credential'),
      `Expected secret scan error: ${r.stderr}`);
  });

  test('blocks GitHub PAT in description', () => {
    const args = validArgs();
    // ghp_ prefix followed by 36 chars
    args[args.indexOf('A description of the finding that is informative')] =
      'Found ghp_' + 'a'.repeat(36) + ' in config file';
    const r = run(args);
    assert.notEqual(r.exitCode, 0);
    assert.ok(r.stderr.includes('secret') || r.stderr.includes('credential'));
  });
});

describe('serendipity-capture.sh — rate limiting', () => {

  test('rate limit is enforced after MAX_PER_SESSION findings', () => {
    const sessionId = `test-rate-limit-${Date.now()}`;
    const rateFile = join(tmpdir(), `serendipity-rate-${sessionId}`);

    // Simulate hitting the limit by writing count=5 to the rate file
    writeFileSync(rateFile, '5');

    const r = run(validArgs(), { CLAUDE_SESSION_ID: sessionId });
    assert.notEqual(r.exitCode, 0);
    assert.ok(r.stderr.includes('Rate limit'), `Expected rate limit error: ${r.stderr}`);

    // Cleanup
    try { rmSync(rateFile); } catch {}
  });

  test('rate limit is not triggered before MAX_PER_SESSION', () => {
    const sessionId = `test-rate-ok-${Date.now()}`;
    const rateFile = join(tmpdir(), `serendipity-rate-${sessionId}`);

    // 4 = one below limit of 5
    writeFileSync(rateFile, '4');

    const r = run(validArgs(), { CLAUDE_SESSION_ID: sessionId });
    // Should NOT fail with rate limit error (may fail at RCC connection, which is fine)
    assert.ok(!r.stderr.includes('Rate limit'),
      `Should not hit rate limit at count=4: ${r.stderr}`);

    try { rmSync(rateFile); } catch {}
  });
});

describe('serendipity-capture.sh — unknown argument', () => {

  test('exits non-zero for unknown argument', () => {
    const r = run([...validArgs(), '--unknown-flag', 'value']);
    assert.notEqual(r.exitCode, 0);
    assert.ok(r.stderr.includes('Unknown argument'), `stderr: ${r.stderr}`);
  });
});
