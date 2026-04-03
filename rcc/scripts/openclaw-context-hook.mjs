#!/usr/bin/env node
/**
 * rcc/scripts/openclaw-context-hook.mjs — Fleet memory context injection hook
 *
 * Called by OpenClaw at session start to inject relevant fleet memory context.
 *
 * Usage:
 *   node openclaw-context-hook.mjs [query]
 *   echo "query text" | node openclaw-context-hook.mjs
 *
 * Prints the context string to stdout for OpenClaw to inject into the system prompt.
 *
 * OpenClaw config: add to openclaw.json:
 *   agents.system_context_hook: "node /home/jkh/Src/CCC/rcc/scripts/openclaw-context-hook.mjs"
 *
 * Env vars:
 *   RCC_URL         (default: http://localhost:8789)
 *   RCC_AGENT_TOKEN (for auth)
 */

const RCC_URL = process.env.RCC_URL ?? 'http://localhost:8789';
const TOKEN   = process.env.RCC_AGENT_TOKEN ?? '';

const DEFAULT_QUERY = 'recent fleet activity and active projects';

async function readStdin() {
  if (process.stdin.isTTY) return null;
  const chunks = [];
  for await (const chunk of process.stdin) chunks.push(chunk);
  return Buffer.concat(chunks).toString('utf8').trim() || null;
}

async function main() {
  // Query from argv, then stdin, then default
  let query = process.argv[2]?.trim();
  if (!query) query = await readStdin();
  if (!query) query = DEFAULT_QUERY;

  const headers = { 'Content-Type': 'application/json' };
  if (TOKEN) headers['Authorization'] = `Bearer ${TOKEN}`;

  const res = await fetch(`${RCC_URL}/api/memory/context`, {
    method: 'POST',
    headers,
    body: JSON.stringify({ query, k: 10, max_tokens: 1500 }),
  });

  if (!res.ok) {
    process.stderr.write(`[openclaw-context-hook] RCC context request failed: ${res.status}\n`);
    process.exit(1);
  }

  const data = await res.json();
  const context = typeof data === 'string' ? data : (data.context ?? data.text ?? JSON.stringify(data));
  process.stdout.write(context);
}

main().catch(err => {
  process.stderr.write(`[openclaw-context-hook] Error: ${err.message}\n`);
  process.exit(1);
});
