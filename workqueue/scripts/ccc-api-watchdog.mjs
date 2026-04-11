#!/usr/bin/env node
/**
 * ccc-api-watchdog.mjs
 * Natasha's CCC dashboard API watchdog.
 * Checks CCC_URL/api/queue — posts ClawBus alert to #ops
 * if unreachable for >30 consecutive minutes.
 *
 * Formerly used Mattermost; now uses ClawBus /bus/send.
 * State: ~/.openclaw/workspace/workqueue/state-ccc-watchdog.json
 * Run: every 10-15 min via cron.
 */

import { readFileSync, writeFileSync, existsSync } from 'fs';

const CCC_URL   = process.env.CCC_URL   || 'http://localhost:8789';
const CCC_TOKEN = process.env.CCC_AGENT_TOKEN || '';
const CALLING_AGENT = process.env.AGENT_NAME || 'natasha';
const STATE_PATH = process.env.WATCHDOG_STATE ||
  `${process.env.HOME}/.ccc/state-ccc-watchdog.json`;
const ALERT_AFTER_MS = 30 * 60 * 1000; // 30 minutes

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
    const res = await fetch(`${CCC_URL}/api/queue`, {
      headers: { Authorization: `Bearer ${CCC_TOKEN}` },
      signal: AbortSignal.timeout(8000),
    });
    return res.ok;
  } catch {
    return false;
  }
}

// ── Alert via ClawBus ─────────────────────────────────────────────────────────

async function sendAlert(msg) {
  if (!CCC_TOKEN) {
    console.log('[watchdog] No CCC_AGENT_TOKEN — skipping alert, would send:', msg);
    return;
  }
  try {
    const res = await fetch(`${CCC_URL}/bus/send`, {
      method: 'POST',
      headers: {
        Authorization: `Bearer ${CCC_TOKEN}`,
        'Content-Type': 'application/json',
      },
      body: JSON.stringify({
        from: CALLING_AGENT,
        to: 'all',
        type: 'text',
        subject: 'ops',
        body: msg,
        mime: 'text/plain',
      }),
      signal: AbortSignal.timeout(8000),
    });
    if (!res.ok) {
      console.error('[watchdog] ClawBus post failed:', await res.text());
    } else {
      console.log('[watchdog] Alert posted to #ops via ClawBus');
    }
  } catch (e) {
    console.error('[watchdog] ClawBus error:', e.message);
  }
}

// ── Main ──────────────────────────────────────────────────────────────────────

async function main() {
  const now    = Date.now();
  const nowIso = new Date(now).toISOString();
  const state  = loadState();
  const up     = await checkApi();

  if (up) {
    const wasDown = state.firstDownTs !== null;
    const downMin = wasDown
      ? Math.round((now - new Date(state.firstDownTs).getTime()) / 60000)
      : 0;

    if (wasDown) {
      console.log(`[watchdog] API back UP after ~${downMin}min down`);
      if (state.alertSentTs) {
        await sendAlert(
          `[watchdog] CCC API recovered — back online after ~${downMin} min outage. (${CALLING_AGENT})`
        );
      }
    } else {
      console.log('[watchdog] API OK');
    }

    state.firstDownTs        = null;
    state.lastUpTs           = nowIso;
    state.alertSentTs        = null;
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
    const downMs  = now - new Date(state.firstDownTs).getTime();
    const downMin = Math.round(downMs / 60000);
    console.log(`[watchdog] API still DOWN — ${downMin}min since first failure`);

    if (downMs >= ALERT_AFTER_MS && !state.alertSentTs) {
      state.alertSentTs = nowIso;
      await sendAlert(
        `[watchdog] CCC API OUTAGE — ${CCC_URL}/api/queue unreachable for ${downMin} minutes. ` +
        `Sync blocked for all agents. Please check the ccc-server service. (${CALLING_AGENT} @ ${nowIso})`
      );
    } else if (downMs >= ALERT_AFTER_MS) {
      console.log(`[watchdog] Already alerted at ${state.alertSentTs}, still down`);
    }
  }

  saveState(state);
}

main().catch(e => { console.error('[watchdog] Fatal:', e); process.exit(1); });
