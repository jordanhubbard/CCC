# Proposal: Replace Brave Search with Browser-Based Google Search

## Problem

Brave Search API is over its monthly budget ($5/mo limit hit). We have no web search
capability until it resets. This is the second budget-related outage today (after HAI Maker).

## Options

### Option A: Google Search via Browser Automation ⭐ RECOMMENDED
**What:** Use the browser tool to navigate to google.com, type queries, scrape results.
**Implementation:** Build an OpenClaw skill that wraps browser automation into a search interface.

- ✅ No API key needed — uses jkh's existing Google login
- ✅ No cost, no budget limits
- ✅ Full Google Search quality (better results than Brave)
- ✅ Bullwinkle and Natasha both have browser access
- ⚠️ Slower than API (~3-5 sec vs <1 sec)
- ⚠️ Google may CAPTCHA if we hit it too hard (unlikely at our volume)
- ⚠️ Depends on browser being available (not always true in isolated cron sessions)
- ⚠️ Rocky (headless Linux VPS) doesn't have browser access — needs different approach

**Skill design:**
```
# google-search skill
1. browser → navigate to https://www.google.com/search?q={query}
2. browser → snapshot the results page
3. Parse titles, URLs, snippets from the snapshot
4. Return structured results
```

### Option B: SearXNG (self-hosted meta-search)
**What:** Run SearXNG on Rocky's VPS — it aggregates results from Google, Bing, DuckDuckGo, etc.
- ✅ No API keys for any search engine
- ✅ All agents can query via HTTP API
- ✅ Privacy-friendly — no tracking
- ✅ Multiple search engine fallback built in
- ⚠️ Requires Docker on Rocky (~100MB RAM)
- ⚠️ Search engines may block the VPS IP over time
- ⚠️ Another service to maintain

### Option C: DuckDuckGo Instant Answer API
**What:** DDG has a free, no-auth API for instant answers.
- ✅ Free, no key needed
- ⚠️ Very limited — only "instant answers," not full search results
- ⚠️ No pagination, no deep results
- **Verdict:** Supplement, not replacement

### Option D: Serper.dev or SerpAPI (paid)
**What:** Google Search API wrappers, pay per query.
- ✅ Real Google results
- ⚠️ Another paid API to manage budgets for
- ⚠️ Serper: $50/mo for 2500 queries. SerpAPI: $50/mo for 5000 queries.
- **Verdict:** Trading one budget problem for another

### Option E: Raise Brave Search budget
**What:** Just pay more for Brave.
- ⚠️ $5/mo only gets 2000 queries — we clearly need more
- ⚠️ Next tier is $15/mo for 10K queries
- **Verdict:** Easy but doesn't solve the structural problem

## Recommendation

**Option A (browser-based Google Search) as primary + Option B (SearXNG on Rocky) for headless/cron contexts.**

- Browser search for interactive sessions (Bullwinkle, Natasha) — fast enough, free, best results
- SearXNG on Rocky for cron jobs and headless contexts where browser isn't available
- Both are $0/month with no budget limits
- Keep Brave Search configured as fallback when its budget resets each month

### Implementation Plan

1. **Phase 1 (now):** Build a `google-search` skill that uses browser automation
   - Assign to Bullwinkle (I have browser access right now)
   - Test with a few queries
   - Share with Natasha

2. **Phase 2 (next):** Deploy SearXNG on Rocky
   - `docker run -p 8080:8080 searxng/searxng`
   - All agents query via `http://100.89.199.14:8080/search?q={query}&format=json`
   - Tailscale-only, no public exposure

3. **Phase 3 (optional):** Build an OpenClaw skill that tries SearXNG first, falls back to browser, falls back to Brave
   - Unified search interface regardless of backend

## What jkh needs to do

- **Phase 1:** Nothing — we already have browser access
- **Phase 2:** Approve Docker on Rocky (Rocky can self-install)
- **Phase 3:** Nothing — we handle it
