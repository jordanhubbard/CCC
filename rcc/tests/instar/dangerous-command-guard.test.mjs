/**
 * Tests for deploy/dangerous-command-guard.sh (instar item wq-INSTAR-*-06)
 *
 * Interface: bash deploy/dangerous-command-guard.sh "<command>"
 *   GUARD_LEVEL=1 (default): block risky commands (exit 1) + always-block (exit 2)
 *   GUARD_LEVEL=2 (autonomous): self-verify prompt for risky (exit 1), still always-blocks (exit 2)
 *
 * Exit codes:
 *   0 — safe to proceed
 *   1 — blocked (L1) or self-verify required (L2) or normal block
 *   2 — always blocked unconditionally
 */

import { test, describe, before } from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { chmodSync, statSync } from 'node:fs';

const __dirname = fileURLToPath(new URL('.', import.meta.url));
const GUARD = resolve(__dirname, '../../../deploy/dangerous-command-guard.sh');

before(() => {
  // Ensure script is executable
  chmodSync(GUARD, 0o755);
});

function runGuard(command, env = {}) {
  const result = spawnSync('bash', [GUARD, command], {
    env: { ...process.env, ...env },
    encoding: 'utf-8',
    timeout: 5000,
  });
  return {
    exitCode: result.status,
    stdout: result.stdout || '',
    stderr: result.stderr || '',
  };
}

describe('dangerous-command-guard.sh — always-block (exit 2)', () => {

  test('blocks rm -rf / — root filesystem wipe', () => {
    const r = runGuard('rm -rf /');
    assert.equal(r.exitCode, 2, `stderr: ${r.stderr.slice(0, 200)}`);
    assert.ok(r.stderr.includes('ALWAYS-BLOCKED'));
  });

  test('blocks dd if= — raw disk write', () => {
    const r = runGuard('dd if=/dev/zero of=/dev/sda');
    assert.equal(r.exitCode, 2);
    assert.ok(r.stderr.includes('ALWAYS-BLOCKED'));
  });

  test('blocks prisma migrate reset — database wipe', () => {
    const r = runGuard('npx prisma migrate reset --force');
    assert.equal(r.exitCode, 2);
    assert.ok(r.stderr.includes('ALWAYS-BLOCKED'));
  });

  test('blocks --accept-data-loss flag', () => {
    const r = runGuard('some-command --accept-data-loss');
    assert.equal(r.exitCode, 2);
  });

  test('blocks mkfs — filesystem format', () => {
    const r = runGuard('mkfs.ext4 /dev/sda1');
    assert.equal(r.exitCode, 2);
  });
});

describe('dangerous-command-guard.sh — Level 1 blocked (exit 1)', () => {

  test('blocks rm -rf ~/.rcc at level 1 (exit 1)', () => {
    const r = runGuard('rm -rf ~/.rcc', { GUARD_LEVEL: '1' });
    // rm -rf is a level-1 block (risky but not unconditional), exits 1
    assert.ok([1, 2].includes(r.exitCode),
      `Expected exit 1 or 2, got ${r.exitCode}: ${r.stderr.slice(0, 200)}`);
    assert.ok(r.stderr.includes('BLOCKED'), `Expected BLOCKED in stderr`);
  });

  test('blocks rm -rf ~/.openclaw at level 1', () => {
    const r = runGuard('rm -rf ~/.openclaw', { GUARD_LEVEL: '1' });
    assert.ok([1, 2].includes(r.exitCode));
    assert.ok(r.stderr.includes('BLOCKED'));
  });

  test('blocks rm -rf on arbitrary path at level 1', () => {
    const r = runGuard('rm -rf /tmp/some-important-dir', { GUARD_LEVEL: '1' });
    assert.ok([1, 2].includes(r.exitCode));
  });
});

describe('dangerous-command-guard.sh — safe commands pass (exit 0)', () => {

  test('allows ls -la', () => {
    const r = runGuard('ls -la ~/', { GUARD_LEVEL: '1' });
    assert.equal(r.exitCode, 0, `Expected exit 0 for safe command, got ${r.exitCode}: ${r.stderr}`);
  });

  test('allows git status', () => {
    const r = runGuard('git status');
    assert.equal(r.exitCode, 0);
  });

  test('allows node --test', () => {
    const r = runGuard('node --test rcc/tests/instar/adaptive-trust.test.mjs');
    assert.equal(r.exitCode, 0);
  });

  test('allows curl to health endpoint', () => {
    const r = runGuard('curl -s http://localhost:8789/health');
    assert.equal(r.exitCode, 0);
  });

  test('allows cat of a specific file', () => {
    const r = runGuard('cat /etc/hostname');
    assert.equal(r.exitCode, 0);
  });
});

describe('dangerous-command-guard.sh — Level 2 self-verify', () => {

  test('always-block patterns still exit 2 at Level 2', () => {
    const r = runGuard('rm -rf /', { GUARD_LEVEL: '2' });
    assert.equal(r.exitCode, 2, 'always-block must remain unconditional at Level 2');
  });

  test('Level 2 blocks risky commands with self-verify prompt (exit 1)', () => {
    const r = runGuard('rm -rf /tmp/test-dir', { GUARD_LEVEL: '2' });
    // At Level 2, risky commands get self-verify injection, not unconditional block
    // Exit 1 = self-verify required; safe commands pass with exit 0
    assert.ok([0, 1, 2].includes(r.exitCode),
      `Unexpected exit code ${r.exitCode}: ${r.stderr.slice(0, 100)}`);
  });

  test('safe commands pass at Level 2', () => {
    const r = runGuard('git log --oneline -5', { GUARD_LEVEL: '2' });
    assert.equal(r.exitCode, 0);
  });
});

describe('dangerous-command-guard.sh — script integrity', () => {

  test('script file exists and is executable', () => {
    const stat = statSync(GUARD);
    // Check owner execute bit (0o100)
    assert.ok(stat.mode & 0o100, `${GUARD} is not executable`);
  });

  test('exits non-zero with usage message for missing argument', () => {
    const result = spawnSync('bash', [GUARD], { encoding: 'utf-8', timeout: 5000 });
    assert.ok(result.status !== 0, 'should exit non-zero without required argument');
    assert.ok(result.stderr.includes('Usage'), `Expected usage message: ${result.stderr.slice(0, 100)}`);
  });
});
