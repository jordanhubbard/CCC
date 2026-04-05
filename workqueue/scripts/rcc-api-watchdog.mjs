#!/usr/bin/env node
/**
 * rcc-api-watchdog.mjs
 * Natasha's CCC dashboard API watchdog.
 * Checks http://146.190.134.110:8788/api/queue — fires Mattermost alert to #agent-shared
 * if unreachable for >30 consecutive minutes.
 *
 * State: ~/.openclaw/workspace/workqueue/state-rcc-watchdog.json
 * Run: every 10-15 min via cron, or called from workqueue tick.
 */

import { readFileSync, writeFileSync, existsSync } from 'fs';

const API_URL       = 'http://146.190.134.110:8788/api/queue';
const STATE_PATH    = '/home/jkh/.openclaw/workspace/workqueue/state-rcc-watchdog.json';
const ALERT_AFTER_MS = 30 * 60 * 1000; // 30 minutes

// Mattermost config — same pattern as other scripts
const MM_URL    = process.env.MATTERMOST_URL    || 'http://localhost:8065';
const MM_TOKEN  = process.env.MATTERMOST_TOKEN  || '';
const MM_CHANNEL = process.env.MATTERMOST_AGENT_SHARED_CHANNEL || 'agent-shared';

// ── State management ──────────────────────────────────────────────────────────

function loadState() {
  if (!existsSync(STATE_PATH)) {
    return { firstDownTs: null, lastUpTs: null, alertSentTs: null, consecutiveFailures: 0 };
  }
  try {
    return JSON.parse(readFileSync(STATE_PATH, 'utf8'));
  } catch {
    return { firstDownTs: null, lastUpTs: null, alertSentTs: null, consecutiveFailures: 0 };
  }
}

function saveState(state) {
  writeFileSync(STATE_PATH, JSON.stringify(state, null, 2));
}

// ── Check API ─────────────────────────────────────────────────────────────────

async function checkApi() {
  try {
    const res = await fetch(API_URL, { signal: AbortSignal.timeout(8000) });
    return res.ok;
  } catch {
    return false;
  }
}

// ── Alert via Mattermost ──────────────────────────────────────────────────────

async function sendMattermostAlert(msg) {
  if (!MM_TOKEN) {
    console.log('[watchdog] No MATTERMOST_TOKEN — skipping alert, would send:', msg);
    return;
  }
  try {
    // Resolve channel id
    const chRes = await fetch(`${MM_URL}/api/v4/channels/name/${MM_CHANNEL}`, {
      headers: { Authorization: `Bearer ${MM_TOKEN}` },
      signal: AbortSignal.timeout(8000),
    });
    if (!chRes.ok) { console.error('[watchdog] Could not resolve channel:', await chRes.text()); return; }
    const ch = await chRes.json();

    const postRes = await fetch(`${MM_URL}/api/v4/posts`, {
      method: 'POST',
      headers: { Authorization: `Bearer ${MM_TOKEN}`, 'Content-Type': 'application/json' },
      body: JSON.stringify({ channel_id: ch.id, message: msg }),
      signal: AbortSignal.timeout(8000),
    });
    if (!postRes.ok) { console.error('[watchdog] Post failed:', await postRes.text()); }
    else { console.log('[watchdog] Alert posted to #agent-shared'); }
  } catch (e) {
    console.error('[watchdog] Mattermost error:', e.message);
  }
}

// ── Main ──────────────────────────────────────────────────────────────────────

async function main() {
  const now   = Date.now();
  const nowIso = new Date(now).toISOString();
  const state = loadState();
  const up    = await checkApi();

  if (up) {
    const wasDown = state.firstDownTs !== null;
    const downDuration = wasDown
      ? Math.round((now - new Date(state.firstDownTs).getTime()) / 60000)
      : 0;

    if (wasDown) {
      console.log(`[watchdog] API is back UP after ~${downDuration}min down`);
      // Post recovery notice if we had alerted
      if (state.alertSentTs) {
        await sendMattermostAlert(
          `✅ **CCC API recovered** — dashboard at 146.190.134.110:8788 is back online after ~${downDuration} min outage. (Natasha watchdog)`
        );
      }
    } else {
      console.log('[watchdog] API OK');
    }

    state.firstDownTs       = null;
    state.lastUpTs          = nowIso;
    state.alertSentTs       = null;
    state.consecutiveFailures = 0;
    saveState(state);
    return;
  }

  // API is down
  state.consecutiveFailures = (state.consecutiveFailures || 0) + 1;
  if (!state.firstDownTs) {
    state.firstDownTs = nowIso;
    console.log('[watchdog] API DOWN — recording first failure at', nowIso);
  } else {
    const downMs = now - new Date(state.firstDownTs).getTime();
    const downMin = Math.round(downMs / 60000);
    console.log(`[watchdog] API still DOWN — ${downMin}min since first failure`);

    // Alert if >30min and not already alerted this outage
    if (downMs >= ALERT_AFTER_MS && !state.alertSentTs) {
      state.alertSentTs = nowIso;
      const msg = `🚨 **CCC API OUTAGE** — dashboard API at 146.190.134.110:8788 has been unreachable for **${downMin} minutes**. Sync to authoritative queue is blocked for all agents. Someone should check the dashboard service on the CCC host. (Natasha watchdog @ ${nowIso})`;
      await sendMattermostAlert(msg);
    } else if (downMs >= ALERT_AFTER_MS) {
      console.log(`[watchdog] Already alerted at ${state.alertSentTs}, still down`);
    }
  }

  saveState(state);
}

main().catch(e => { console.error('[watchdog] Fatal:', e); process.exit(1); });
