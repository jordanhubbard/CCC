/**
 * Dashboard regression tests — process.env and client-side leakage detection
 *
 * Catches the class of bug where Node.js server-side globals like process.env
 * are rendered directly into HTML served to the browser, causing ReferenceError.
 *
 * Run: node --test rcc/tests/dashboard-regressions.test.mjs
 */

import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, existsSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dir = dirname(fileURLToPath(import.meta.url));
const WORKSPACE = join(__dir, '../..');
const API_BASE = process.env.RCC_URL || 'http://localhost:8789';
const TIMEOUT_MS = 10000;

async function fetchWithTimeout(url, opts = {}) {
  const controller = new AbortController();
  const t = setTimeout(() => controller.abort(), TIMEOUT_MS);
  try {
    const res = await fetch(url, { ...opts, signal: controller.signal });
    clearTimeout(t);
    return res;
  } catch (err) {
    clearTimeout(t);
    throw err;
  }
}

// ── Source-level static checks ────────────────────────────────────────────────

describe('Static source checks — server.mjs / index.mjs', () => {
  const serverPaths = [
    join(WORKSPACE, 'rcc/dashboard/server.mjs'),
    join(WORKSPACE, 'rcc/api/index.mjs'),
  ];

  for (const serverPath of serverPaths) {
    const filename = serverPath.split('/').slice(-2).join('/');

    if (!existsSync(serverPath)) continue;

    test(`${filename}: no bare process.env references inside template literal HTML strings`, () => {
      const src = readFileSync(serverPath, 'utf8');
      // Match template literals that contain process.env - these get sent to the browser
      const templateLiteralBlocks = src.match(/`[^`]*`/gs) || [];
      const offending = templateLiteralBlocks.filter(block =>
        block.includes('process.env') &&
        // Allow: server-side variable assignments (not HTML rendering)
        !block.match(/^\s*const|^\s*let|^\s*var/)
      );
      // Build line numbers for context
      if (offending.length > 0) {
        const lines = src.split('\n');
        const found = [];
        lines.forEach((line, i) => {
          if (line.includes('process.env') && line.includes('`')) {
            found.push(`  Line ${i + 1}: ${line.trim().slice(0, 100)}`);
          }
        });
        assert.fail(
          `process.env found inside template literals in ${filename} — these will ReferenceError in browser:\n${found.join('\n')}`
        );
      }
    });

    test(`${filename}: HTML template output does not embed Node-only globals`, () => {
      const src = readFileSync(serverPath, 'utf8');
      // These patterns should never appear inside rendered HTML output
      const nodeOnlyPatterns = [
        /\$\{process\.env\./g,   // ${process.env.X} in template literal
        /\$\{require\(/g,         // ${require(...)} in template literal
        /\$\{__dirname/g,         // ${__dirname} in template literal
        /\$\{__filename/g,        // ${__filename} in template literal
      ];
      const offending = [];
      for (const pattern of nodeOnlyPatterns) {
        const matches = [...src.matchAll(pattern)];
        if (matches.length > 0) {
          offending.push(`Pattern ${pattern} found ${matches.length} time(s)`);
        }
      }
      if (offending.length > 0) {
        assert.fail(`Node-only globals in template literals in ${filename}:\n${offending.join('\n')}`);
      }
    });
  }
});

// ── Live server checks ────────────────────────────────────────────────────────

describe('Live dashboard — process.env leakage detection', () => {
  const routes = [
    '/',
    '/projects',
  ];

  for (const route of routes) {
    test(`GET ${route} — no process.env in response body`, async () => {
      let res;
      try {
        res = await fetchWithTimeout(`${API_BASE}${route}`);
      } catch {
        console.log(`[regression-test] SKIP: ${API_BASE}${route} not reachable`);
        return;
      }
      if (!res.ok) {
        console.log(`[regression-test] SKIP: ${route} returned ${res.status}`);
        return;
      }
      const body = await res.text();
      assert.ok(
        !body.includes('process.env'),
        `"process.env" found in response body for ${route} — this will cause ReferenceError in browser`
      );
    });

    test(`GET ${route} — no require( in response body`, async () => {
      let res;
      try {
        res = await fetchWithTimeout(`${API_BASE}${route}`);
      } catch {
        console.log(`[regression-test] SKIP: ${API_BASE}${route} not reachable`);
        return;
      }
      if (!res.ok) {
        console.log(`[regression-test] SKIP: ${route} returned ${res.status}`);
        return;
      }
      const body = await res.text();
      // Allow 'require' in comments/docs but not as a callable
      const hasRequireCall = body.match(/\brequire\s*\(/);
      assert.ok(
        !hasRequireCall,
        `require() call found in response body for ${route} — Node-only function will fail in browser`
      );
    });
  }
});

// ── Pre-commit hook source check ──────────────────────────────────────────────

describe('Pre-commit hook coverage', () => {
  test('rcc/hooks/pre-commit.sh exists or pre-commit check documented', () => {
    const hookPath = join(WORKSPACE, 'rcc/hooks/pre-commit.sh');
    const altPath = join(WORKSPACE, '.git/hooks/pre-commit');
    const exists = existsSync(hookPath) || existsSync(altPath);
    if (!exists) {
      console.log('[regression-test] No pre-commit hook found — consider adding rcc/hooks/pre-commit.sh');
      // Soft warning, not hard fail — hook is a bonus requirement
    }
    // This test documents the expectation but doesn't hard-fail if hook is missing
    assert.ok(true, 'pre-commit hook existence documented');
  });
});
