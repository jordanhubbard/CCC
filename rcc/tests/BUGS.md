# RCC API Bug Report

Generated while writing the test suite on 2026-03-27.
Tests: api.test.mjs, integration.test.mjs, ui.test.mjs

---

## Bug: ideation module missing from codebase

- **Route:** N/A (import error)
- **Expected:** `rcc/ideation/ideation.mjs` exports `generateIdea` function, used by POST /api/ideation/generate
- **Actual:** The file does not exist. `import { generateIdea } from '../ideation/ideation.mjs'` in api/index.mjs throws ERR_MODULE_NOT_FOUND, preventing the entire API from loading. This also breaks `capabilities.test.mjs`.
- **Severity:** high (prevents local testing entirely; all test suites fail to start)
- **Status:** open — worked around by creating a stub at `rcc/ideation/ideation.mjs` for test purposes

---

## Bug: vector/index.mjs export name mismatch with api/index.mjs imports

- **Route:** N/A (import error)
- **Expected:** `rcc/vector/index.mjs` exports: `upsert`, `search`, `searchAll`, `collectionStats`
- **Actual:** The module exports `vectorUpsert`, `vectorSearch` (prefixed names), and has no `searchAll` or `collectionStats`. The API imports the unprefixed names which don't exist, causing ERR_ERR_MODULE_NOT_FOUND at startup.
- **Severity:** high (prevents local testing; referenced in commit `38825e9 fix: ingest.mjs vectorUpsert export mismatch`)
- **Status:** open — worked around by adding compatibility alias exports to `rcc/vector/index.mjs`

---

## Bug: setInterval in api/index.mjs prevents process exit after tests

- **Route:** N/A (process lifecycle)
- **Expected:** After `server.close()`, the test process exits cleanly
- **Actual:** `setInterval(runDisappearanceCheck, 5 * 60 * 1000)` at line 109 of api/index.mjs keeps the Node.js event loop alive indefinitely after the server closes. Test runner hangs until SIGTERM.
- **Severity:** low (test infrastructure inconvenience)
- **Status:** open — worked around in api.test.mjs with `setTimeout(() => process.exit(0), 500).unref()` in the after() hook

---

## Bug: /api/secrets endpoint not implemented

- **Route:** GET /api/secrets/:key, POST /api/secrets/:key
- **Expected:** Authenticated read/write of named secrets; GET returns value or 404, POST (admin) sets value, GET without auth returns 401
- **Actual:** Route does not exist in api/index.mjs — all requests return 404 regardless of auth. The word "secrets" does not appear anywhere in the API source.
- **Severity:** medium (missing feature documented in test spec)
- **Status:** open

---

## Bug: GET /api/bootstrap fails when deploy key not configured

- **Route:** GET /api/bootstrap?token=<valid-bootstrap-token>
- **Expected:** Returns 200 with ok:true, agentToken, rccUrl, deployKey, repoUrl
- **Actual:** Returns 500 {"error":"Deploy key not configured"} if `data/github-key.json` does not exist. This makes local unit testing of the full bootstrap flow impossible without pre-seeding a fake deploy key.
- **Severity:** medium (blocks local integration test of bootstrap success path)
- **Status:** open
- **Notes:** The deploy key file must be populated via POST /api/keys/github (admin-only) before GET /api/bootstrap can succeed. A test fixture or env-var override would make this testable in isolation.

---

## Bug: Bootstrap flow requires admin token — live token may not have admin privileges

- **Route:** POST /api/bootstrap/token
- **Expected:** Our token `wq-5dcad756f6d3e345c00b5cb3dfcbdedb` (listed as the agent/admin token) should work for admin-only endpoints
- **Actual:** `RCC_ADMIN_TOKEN` on the live server defaults to `process.env.RCC_ADMIN_TOKEN || process.env.RCC_AUTH_TOKENS?.split(',')[0]`. If the live server's `RCC_AUTH_TOKENS` has a different first token than ours, our token will fail admin checks with 401.
- **Severity:** low (depends on live server config)
- **Status:** open — verified by integration.test.mjs step 1; logs "SKIP" if 401 is returned

---

## Bug: GET /api/agents/status requires auth but GET /api/heartbeats does not

- **Route:** GET /api/heartbeats vs GET /api/agents/status
- **Expected:** Consistent auth requirement for agent status/heartbeat data
- **Actual:** `GET /api/heartbeats` is public (no auth check before line 632), but `GET /api/agents/status` requires auth (line 611). Both return sensitive agent-presence information. This is an auth inconsistency.
- **Severity:** low (information leakage — heartbeat data is public)
- **Status:** open

---

## Bug: PATCH /api/agents/:name does not update capabilities in the agents.json correctly

- **Route:** PATCH /api/agents/:name
- **Expected:** `Object.assign(agents[name].capabilities || {}, body.capabilities)` merges new capabilities
- **Actual:** If `agents[name].capabilities` is `undefined` (e.g., agent registered via bootstrap without capabilities), the assign is called on `undefined` → capabilities are lost. The `|| {}` fallback creates a new object but does not assign it back to `agents[name].capabilities`.
- **Severity:** medium (silent data loss on agents without initial capabilities)
- **Status:** open
- **Code reference:** api/index.mjs line 1076: `if (body.capabilities) Object.assign(agents[name].capabilities || {}, body.capabilities);`

---

## Bug: GET /api/bootstrap STILL requires Authorization header (regression not fully fixed)

- **Route:** GET /api/bootstrap?token=<token>
- **Expected:** Should work WITHOUT an Authorization header (agents use it on first boot before they have any token). Commit 2302f1c claimed to fix this.
- **Actual:** The route is positioned AFTER the `if (!isAuthed(req))` guard at line 763 of api/index.mjs. Any request to GET /api/bootstrap without an Authorization header returns 401 before the route handler can process the ?token= param. The fix in commit 2302f1c did not actually move this endpoint before the auth middleware.
- **Severity:** high (the entire bootstrap flow is broken for fresh agents with no prior token)
- **Status:** open — confirmed by api.test.mjs: GET /api/bootstrap without auth → 401 (not 400 for missing token)

---

## Bug: POST /api/queue returns 201 but scout_key duplicate check returns 200 ok:false

- **Route:** POST /api/queue with duplicate scout_key
- **Expected:** Consistent status code for "item not created" path — 409 Conflict would be conventional
- **Actual:** Returns 200 OK with body `{ok: false, duplicate: true}` instead of a 4xx status. Callers must check body.ok, not rely on HTTP status code.
- **Severity:** low (non-standard HTTP usage)
- **Status:** open

---

## Bug: Dashboard UI port (8788) is separate process — may not be running

- **Route:** GET http://146.190.134.110:8788/
- **Expected:** 200 text/html
- **Actual:** The dashboard at :8788 is a separate service (not served by the RCC API at :8789). If that process is down, ui.test.mjs will skip gracefully. The API at :8789 serves /projects as HTML but does not serve a full dashboard UI at its root.
- **Severity:** low (deployment concern, not an API bug)
- **Status:** open — ui.test.mjs handles this gracefully with a skip + log
