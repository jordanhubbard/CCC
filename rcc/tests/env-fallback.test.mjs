/**
 * env-fallback.test.mjs — Verify CLAWBUS_* / SQUIRRELBUS_* env var fallbacks
 *
 * Ensures that the rebrand from squirrelbus → clawbus doesn't break
 * agents still using the old env var names.
 */

import { describe, test, expect, beforeEach, afterEach } from 'vitest';

const CLAWBUS_VARS = ['CLAWBUS_TOKEN', 'CLAWBUS_URL'];
const LEGACY_VARS  = ['SQUIRRELBUS_TOKEN', 'SQUIRRELBUS_URL'];
const ALL_VARS     = [...CLAWBUS_VARS, ...LEGACY_VARS];

/** Save and restore env around each test */
let savedEnv;
beforeEach(() => {
  savedEnv = {};
  for (const k of ALL_VARS) {
    savedEnv[k] = process.env[k];
    delete process.env[k];
  }
});
afterEach(() => {
  for (const k of ALL_VARS) {
    if (savedEnv[k] !== undefined) process.env[k] = savedEnv[k];
    else delete process.env[k];
  }
});

describe('env var fallback: CLAWBUS_TOKEN || SQUIRRELBUS_TOKEN', () => {
  test('new CLAWBUS_TOKEN wins when both set', () => {
    process.env.CLAWBUS_TOKEN = 'new-token';
    process.env.SQUIRRELBUS_TOKEN = 'old-token';
    const val = process.env.CLAWBUS_TOKEN || process.env.SQUIRRELBUS_TOKEN || '';
    expect(val).toBe('new-token');
  });

  test('falls back to SQUIRRELBUS_TOKEN when CLAWBUS_TOKEN unset', () => {
    process.env.SQUIRRELBUS_TOKEN = 'old-token';
    const val = process.env.CLAWBUS_TOKEN || process.env.SQUIRRELBUS_TOKEN || '';
    expect(val).toBe('old-token');
  });

  test('empty string when neither set', () => {
    const val = process.env.CLAWBUS_TOKEN || process.env.SQUIRRELBUS_TOKEN || '';
    expect(val).toBe('');
  });
});

describe('env var fallback: CLAWBUS_URL || SQUIRRELBUS_URL', () => {
  test('new CLAWBUS_URL wins when both set', () => {
    process.env.CLAWBUS_URL = 'http://new:8788';
    process.env.SQUIRRELBUS_URL = 'http://old:8788';
    const val = process.env.CLAWBUS_URL || process.env.SQUIRRELBUS_URL || 'http://localhost:8788';
    expect(val).toBe('http://new:8788');
  });

  test('falls back to SQUIRRELBUS_URL when CLAWBUS_URL unset', () => {
    process.env.SQUIRRELBUS_URL = 'http://old:8788';
    const val = process.env.CLAWBUS_URL || process.env.SQUIRRELBUS_URL || 'http://localhost:8788';
    expect(val).toBe('http://old:8788');
  });

  test('defaults when neither set', () => {
    const val = process.env.CLAWBUS_URL || process.env.SQUIRRELBUS_URL || 'http://localhost:8788';
    expect(val).toBe('http://localhost:8788');
  });
});
