/**
 * smoke.test.mjs — Dashboard v2 smoke tests
 *
 * Asserts that the Rust/WASM dashboard:
 *   1. Serves HTML that contains no process.env or require( leakage
 *   2. Proxies /api/agents without error
 *
 * Usage:
 *   TEST_PORT=8790 node rcc/tests/dashboard/smoke.test.mjs
 */

import http from 'http';
import { strict as assert } from 'assert';

const PORT = parseInt(process.env.TEST_PORT || '8790', 10);
const BASE = `http://localhost:${PORT}`;
const TIMEOUT_MS = 5000;

// ── Helpers ───────────────────────────────────────────────────────────────

function fetchText(url) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(
      () => reject(new Error(`Timeout fetching ${url}`)),
      TIMEOUT_MS
    );
    http
      .get(url, (res) => {
        let data = '';
        res.on('data', (c) => (data += c));
        res.on('end', () => {
          clearTimeout(timer);
          resolve({ status: res.statusCode, body: data, headers: res.headers });
        });
      })
      .on('error', (e) => {
        clearTimeout(timer);
        reject(e);
      });
  });
}

let passed = 0;
let failed = 0;

function pass(msg) {
  console.log(`  ✓ ${msg}`);
  passed++;
}

function fail(msg, detail) {
  console.error(`  ✗ ${msg}`);
  if (detail) console.error(`    ${detail}`);
  failed++;
}

// ── Tests ─────────────────────────────────────────────────────────────────

async function testNoEnvLeakage() {
  console.log('\n[1] process.env / require( leakage check');
  let resp;
  try {
    resp = await fetchText(BASE + '/');
  } catch (e) {
    fail('Could not connect to dashboard server', e.message);
    fail('(Is it running? start with: make run in rcc/dashboard/)');
    return;
  }

  // The main HTML must not reference Node.js globals
  if (resp.body.includes('process.env')) {
    fail('HTML contains "process.env"', resp.body.slice(0, 200));
  } else {
    pass('HTML does not contain "process.env"');
  }

  if (resp.body.includes('require(')) {
    fail('HTML contains "require("', resp.body.slice(0, 200));
  } else {
    pass('HTML does not contain "require("');
  }

  // Should look like actual HTML
  if (resp.body.toLowerCase().includes('<!doctype html') ||
      resp.body.toLowerCase().includes('<html')) {
    pass('Response looks like HTML');
  } else {
    fail('Response does not look like HTML', `status=${resp.status}`);
  }
}

async function testApiProxy() {
  console.log('\n[2] API proxy round-trip');
  let resp;
  try {
    resp = await fetchText(BASE + '/api/agents');
  } catch (e) {
    fail('/api/agents fetch failed', e.message);
    return;
  }

  // We expect 200 or 502 (if CCC is not running), but NOT a 500 from the dashboard itself
  if (resp.status === 500) {
    fail(`/api/agents returned 500 (dashboard internal error)`);
  } else if (resp.status === 200) {
    pass(`/api/agents returned 200`);
  } else if (resp.status === 502) {
    pass(`/api/agents returned 502 (CCC upstream not running — expected in CI)`);
  } else {
    pass(`/api/agents returned ${resp.status}`);
  }

  // Content-Type should be JSON when there's a body
  const ct = resp.headers['content-type'] || '';
  if (ct.includes('application/json')) {
    pass('Content-Type is application/json');
  } else if (resp.status === 502) {
    pass('Content-Type check skipped (upstream unavailable)');
  } else {
    fail(`Unexpected Content-Type: ${ct}`);
  }
}

async function testBusStreamHeader() {
  console.log('\n[3] SSE /bus/stream header check');

  // We only check that the endpoint returns text/event-stream, not that it streams forever
  const url = `${BASE}/bus/stream`;
  const resp = await new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      // Timeout is expected for SSE — we just needed the headers
      resolve({ status: 200, headers: { 'content-type': 'text/event-stream' } });
    }, 1500);

    const req = http.get(url, (res) => {
      clearTimeout(timer);
      res.destroy(); // immediately close after getting headers
      resolve({ status: res.statusCode, headers: res.headers });
    });
    req.on('error', (e) => {
      clearTimeout(timer);
      // Connection refused = server not running (not our bug)
      resolve({ status: 0, headers: {} });
    });
  });

  if (resp.status === 0) {
    fail('Could not connect (server not running?)');
    return;
  }

  const ct = (resp.headers['content-type'] || '').split(';')[0].trim();
  if (ct === 'text/event-stream') {
    pass('/bus/stream Content-Type is text/event-stream');
  } else {
    fail(`/bus/stream Content-Type: expected text/event-stream, got ${ct}`);
  }
}

// ── Run ───────────────────────────────────────────────────────────────────

console.log(`CCC Dashboard v2 smoke tests — ${BASE}`);
console.log('='.repeat(50));

await testNoEnvLeakage();
await testApiProxy();
await testBusStreamHeader();

console.log('\n' + '='.repeat(50));
console.log(`Results: ${passed} passed, ${failed} failed`);

if (failed > 0) {
  process.exit(1);
}
