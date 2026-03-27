/**
 * Tests for rcc/memory/episodic.mjs (instar item wq-INSTAR-*-05)
 *
 * Covers: saveDigest, getDigest, listDigests, recentDigests,
 * saveSynthesis, getSynthesis, listSyntheses.
 */

import { test, describe, before, after } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';

let tmpDir;
let episodic;

before(async () => {
  tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'rcc-episodic-test-'));
  process.env.HOME = tmpDir;
  // Import after HOME override
  episodic = await import(`../../memory/episodic.mjs?v=${Date.now()}`);
});

after(() => {
  fs.rmSync(tmpDir, { recursive: true, force: true });
});

function makeDigest(overrides = {}) {
  return {
    id: `digest-${Date.now()}-${Math.random().toString(36).slice(2)}`,
    agentName: 'natasha',
    startTime: new Date(Date.now() - 60000).toISOString(),
    endTime: new Date().toISOString(),
    summary: 'Did some work',
    actions: ['read file', 'wrote tests'],
    learnings: ['tests improve confidence'],
    significance: 5,
    themes: ['testing', 'rcc'],
    boundarySignal: 'task_complete',
    ...overrides,
  };
}

describe('EpisodicMemory', () => {

  test('saveDigest and getDigest round-trip', async () => {
    const digest = makeDigest({ summary: 'Round-trip test digest' });
    await episodic.saveDigest(digest);
    const retrieved = await episodic.getDigest(digest.id);
    assert.ok(retrieved, 'should retrieve the saved digest');
    assert.equal(retrieved.id, digest.id);
    assert.equal(retrieved.summary, 'Round-trip test digest');
    assert.deepEqual(retrieved.themes, ['testing', 'rcc']);
  });

  test('getDigest returns null for unknown id', async () => {
    const result = await episodic.getDigest('nonexistent-id-xyz');
    assert.equal(result, null);
  });

  test('listDigests returns digests for today', async () => {
    const today = new Date().toISOString().slice(0, 10);
    const digest = makeDigest({ summary: 'Listed today' });
    await episodic.saveDigest(digest);
    const digests = await episodic.listDigests(today);
    assert.ok(Array.isArray(digests));
    const found = digests.find(d => d.id === digest.id);
    assert.ok(found, 'saved digest should appear in listDigests for today');
  });

  test('listDigests returns empty array for date with no digests', async () => {
    const digests = await episodic.listDigests('1970-01-01');
    assert.ok(Array.isArray(digests));
    assert.equal(digests.length, 0);
  });

  test('recentDigests returns digests within the last N hours', async () => {
    const recent = makeDigest({
      summary: 'Recent work',
      endTime: new Date().toISOString(),
    });
    await episodic.saveDigest(recent);
    const results = await episodic.recentDigests(24);
    assert.ok(Array.isArray(results));
    const found = results.find(d => d.id === recent.id);
    assert.ok(found, 'recently saved digest should appear in recentDigests(24)');
  });

  test('digest significance field is preserved', async () => {
    const highSig = makeDigest({ significance: 9, summary: 'High significance event' });
    await episodic.saveDigest(highSig);
    const retrieved = await episodic.getDigest(highSig.id);
    assert.equal(retrieved.significance, 9);
  });

  test('digest boundarySignal field is preserved', async () => {
    const d = makeDigest({ boundarySignal: 'session_end' });
    await episodic.saveDigest(d);
    const retrieved = await episodic.getDigest(d.id);
    assert.equal(retrieved.boundarySignal, 'session_end');
  });

  test('multiple digests can be saved and listed', async () => {
    const tag = Date.now();
    const d1 = makeDigest({ id: `multi-${tag}-1`, summary: 'First' });
    const d2 = makeDigest({ id: `multi-${tag}-2`, summary: 'Second' });
    const d3 = makeDigest({ id: `multi-${tag}-3`, summary: 'Third' });
    await episodic.saveDigest(d1);
    await episodic.saveDigest(d2);
    await episodic.saveDigest(d3);
    const today = new Date().toISOString().slice(0, 10);
    const list = await episodic.listDigests(today);
    const ids = list.map(d => d.id);
    assert.ok(ids.includes(d1.id));
    assert.ok(ids.includes(d2.id));
    assert.ok(ids.includes(d3.id));
  });

  test('saveSynthesis and getSynthesis round-trip', async () => {
    const date = new Date().toISOString().slice(0, 10);
    const synthesis = {
      sessionDate: date,  // implementation uses sessionDate to derive filename
      agentName: 'natasha',
      summary: 'A productive session',
      keyOutcomes: ['wrote tests', 'fixed bug'],
      allLearnings: ['testing is good'],
      significance: 7,
      themes: ['testing', 'quality'],
    };
    await episodic.saveSynthesis(synthesis);
    const retrieved = await episodic.getSynthesis(date);
    assert.ok(retrieved, 'should retrieve synthesis');
    assert.equal(retrieved.summary, 'A productive session');
    assert.deepEqual(retrieved.keyOutcomes, ['wrote tests', 'fixed bug']);
    assert.equal(retrieved.significance, 7);
  });

  test('getSynthesis returns null for date with no synthesis', async () => {
    const result = await episodic.getSynthesis('1970-01-01');
    assert.equal(result, null);
  });

  test('listSyntheses returns array of syntheses', async () => {
    // At least one synthesis already saved in previous test
    const list = await episodic.listSyntheses(10);
    assert.ok(Array.isArray(list));
    assert.ok(list.length >= 1);
    // Each item should have expected fields
    const first = list[0];
    assert.ok(first.date || first.summary, 'synthesis should have date or summary');
  });

  test('saveSynthesis overwrites existing synthesis for same date', async () => {
    const date = '2099-12-31'; // future date to avoid colliding with other tests
    // Implementation uses synthesis.sessionDate to determine the filename
    const s1 = { sessionDate: date, summary: 'First synthesis', keyOutcomes: ['a'], allLearnings: [], significance: 3, themes: [] };
    const s2 = { sessionDate: date, summary: 'Updated synthesis', keyOutcomes: ['b', 'c'], allLearnings: [], significance: 8, themes: [] };
    await episodic.saveSynthesis(s1);
    await episodic.saveSynthesis(s2);
    const retrieved = await episodic.getSynthesis(date);
    assert.ok(retrieved, 'synthesis should exist for date ' + date);
    assert.equal(retrieved.summary, 'Updated synthesis');
    assert.equal(retrieved.significance, 8);
  });
});
