/**
 * Tests for rcc/trust/adaptive-trust.mjs (instar item wq-INSTAR-*-03)
 *
 * Covers: getTrustLevel, recordSuccess, recordFailure, grantTrust, revokeTrust,
 * getTrustProfile, summarizeTrust, safety floor, streak milestones.
 */

import { test, describe, before, after } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';

// ── Isolate test storage from production ──────────────────────────────────────
const originalHome = process.env.HOME;
let tmpDir;

before(() => {
  tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'rcc-trust-test-'));
  process.env.HOME = tmpDir;
});

after(() => {
  process.env.HOME = originalHome;
  fs.rmSync(tmpDir, { recursive: true, force: true });
});

// Dynamic import AFTER env is set (module reads HOME at import time via os.homedir())
async function trust() {
  return import(`../../trust/adaptive-trust.mjs?v=${Date.now()}`);
}

// ── Tests ─────────────────────────────────────────────────────────────────────

describe('AdaptiveTrust', () => {

  test('defaults to ask for unknown agent/service/op', async () => {
    const t = await trust();
    const level = t.getTrustLevel('agent-x', 'gmail', 'read');
    assert.equal(level, 'ask');
  });

  test('getTrustProfile returns empty services for new agent', async () => {
    const t = await trust();
    const profile = t.getTrustProfile('new-agent');
    assert.equal(profile.agentName, 'new-agent');
    assert.deepEqual(profile.services, {});
  });

  test('recordSuccess increments streak and totalOps', async () => {
    const t = await trust();
    const agent = 'agent-streak-' + Date.now();
    t.recordSuccess(agent, 'slack', 'write');
    t.recordSuccess(agent, 'slack', 'write');
    t.recordSuccess(agent, 'slack', 'write');
    const profile = t.getTrustProfile(agent);
    const entry = profile.services.slack.write;
    assert.equal(entry.streak, 3);
    assert.equal(entry.totalOps, 3);
    assert.equal(entry.level, 'ask'); // success alone never grants auto
  });

  test('safety floor: recordSuccess never auto-promotes to auto regardless of streak', async () => {
    const t = await trust();
    const agent = 'agent-safety-' + Date.now();
    // Simulate 50 consecutive successes
    for (let i = 0; i < 50; i++) t.recordSuccess(agent, 'email', 'delete');
    const level = t.getTrustLevel(agent, 'email', 'delete');
    assert.notEqual(level, 'auto', 'auto must never be granted automatically');
    // Level should still be ask (or at most earned:suggest-auto annotation)
    assert.equal(level, 'ask');
  });

  test('streak milestone 10 sets source to earned:streak', async () => {
    const t = await trust();
    const agent = 'agent-milestone10-' + Date.now();
    for (let i = 0; i < 10; i++) t.recordSuccess(agent, 'calendar', 'write');
    const profile = t.getTrustProfile(agent);
    assert.equal(profile.services.calendar.write.source, 'earned:streak');
  });

  test('streak milestone 20 sets source to earned:suggest-auto', async () => {
    const t = await trust();
    const agent = 'agent-milestone20-' + Date.now();
    for (let i = 0; i < 20; i++) t.recordSuccess(agent, 'calendar', 'write');
    const profile = t.getTrustProfile(agent);
    assert.equal(profile.services.calendar.write.source, 'earned:suggest-auto');
  });

  test('grantTrust is the ONLY path to auto', async () => {
    const t = await trust();
    const agent = 'agent-grant-' + Date.now();
    t.grantTrust(agent, 'github', 'push', 'jkh');
    assert.equal(t.getTrustLevel(agent, 'github', 'push'), 'auto');
    const profile = t.getTrustProfile(agent);
    assert.equal(profile.services.github.push.source, 'granted');
    assert.equal(profile.services.github.push.grantedBy, 'jkh');
  });

  test('recordFailure resets streak but does not change level without revoke', async () => {
    const t = await trust();
    const agent = 'agent-fail-' + Date.now();
    t.recordSuccess(agent, 'slack', 'write');
    t.recordSuccess(agent, 'slack', 'write');
    t.recordFailure(agent, 'slack', 'write', 'test failure', false);
    const profile = t.getTrustProfile(agent);
    const entry = profile.services.slack.write;
    assert.equal(entry.streak, 0);
    assert.equal(entry.level, 'ask'); // level unchanged without revoke=true
    assert.equal(entry.totalOps, 3);
  });

  test('recordFailure with revoke=true drops level to none', async () => {
    const t = await trust();
    const agent = 'agent-revoke-fail-' + Date.now();
    // First grant auto, then fail with revoke
    t.grantTrust(agent, 'email', 'delete', 'jkh');
    t.recordFailure(agent, 'email', 'delete', 'sent wrong email', true);
    assert.equal(t.getTrustLevel(agent, 'email', 'delete'), 'none');
    const profile = t.getTrustProfile(agent);
    assert.equal(profile.services.email.delete.source, 'revoked');
    assert.equal(profile.services.email.delete.streak, 0);
    assert.ok(profile.services.email.delete.revokedReason);
  });

  test('revokeTrust drops level to none and records reason', async () => {
    const t = await trust();
    const agent = 'agent-revoke-' + Date.now();
    t.grantTrust(agent, 'github', 'push', 'jkh');
    t.revokeTrust(agent, 'github', 'push', 'pushed to main accidentally');
    assert.equal(t.getTrustLevel(agent, 'github', 'push'), 'none');
    const profile = t.getTrustProfile(agent);
    assert.equal(profile.services.github.push.source, 'revoked');
    assert.equal(profile.services.github.push.revokedReason, 'pushed to main accidentally');
    assert.equal(profile.services.github.push.streak, 0);
  });

  test('grantTrust after revocation restores auto and clears revocation metadata', async () => {
    const t = await trust();
    const agent = 'agent-regrant-' + Date.now();
    t.grantTrust(agent, 'slack', 'post', 'jkh');
    t.revokeTrust(agent, 'slack', 'post', 'spam incident');
    // Re-grant after incident
    t.grantTrust(agent, 'slack', 'post', 'jkh');
    assert.equal(t.getTrustLevel(agent, 'slack', 'post'), 'auto');
    const profile = t.getTrustProfile(agent);
    assert.ok(!profile.services.slack.post.revokedReason, 'revocation metadata should be cleared');
    assert.ok(!profile.services.slack.post.revokedAt);
  });

  test('trust is independent per service and operation', async () => {
    const t = await trust();
    const agent = 'agent-isolation-' + Date.now();
    t.grantTrust(agent, 'github', 'read', 'jkh');
    // github.read is auto; github.write should still default to ask
    assert.equal(t.getTrustLevel(agent, 'github', 'read'), 'auto');
    assert.equal(t.getTrustLevel(agent, 'github', 'write'), 'ask');
    // gmail unrelated service should also default to ask
    assert.equal(t.getTrustLevel(agent, 'gmail', 'read'), 'ask');
  });

  test('summarizeTrust returns a non-empty string', async () => {
    const t = await trust();
    const agent = 'agent-summary-' + Date.now();
    t.recordSuccess(agent, 'mattermost', 'post');
    const summary = t.summarizeTrust(agent);
    assert.ok(typeof summary === 'string');
    assert.ok(summary.includes(agent));
    assert.ok(summary.includes('mattermost'));
  });

  test('summarizeTrust surface streak suggestions at milestone 20', async () => {
    const t = await trust();
    const agent = 'agent-summary-streak-' + Date.now();
    for (let i = 0; i < 20; i++) t.recordSuccess(agent, 'slack', 'post');
    const summary = t.summarizeTrust(agent);
    assert.ok(summary.includes('consider granting auto'), `Expected suggestion in: ${summary}`);
  });

  test('trust state persists across separate loadProfile calls', async () => {
    const t = await trust();
    const agent = 'agent-persist-' + Date.now();
    t.grantTrust(agent, 'github', 'push', 'jkh');
    // Re-read profile (simulates a fresh load)
    const profile = t.getTrustProfile(agent);
    assert.equal(profile.services.github.push.level, 'auto');
  });
});
