/**
 * settling-check.mjs — Phase 1 CoherenceGate: settling-detection patterns.
 *
 * Adopted from JKHeadley/instar CoherenceGate settling-detection reviewer (2026-03-27).
 * Lightweight JS port — no LLM needed, pure pattern matching.
 *
 * Catches: false inability claims, premature giving-up, capability understatements.
 */

// Patterns that suggest an agent is settling (giving up without trying alternatives)
const SETTLING_PATTERNS = [
  /I('m| am) (unable|not able) to\b/i,
  /I (can't|cannot) (access|fetch|read|write|do|perform|complete|help with)\b/i,
  /I don't have (access|permission|the ability|the capability|direct access)\b/i,
  /Unfortunately,?\s*I (can't|cannot|am unable)\b/i,
  /I'm afraid I (can't|cannot|am unable)\b/i,
  /I (lack|do not have) (the )?capability\b/i,
  /I don't have the (ability|means|tools?)\b/i,
  /beyond (my|the) (capabilities|scope|ability)\b/i,
  /I('m| am) not (equipped|able|designed) to\b/i,
  /I don't have (real-?time|live|current|up-?to-?date)\b/i,
];

// Patterns that are OK — legitimate capability limits
const ALLOWLIST_PATTERNS = [
  /I don't have (access to|the) .{0,30} (password|key|secret|token|credential)/i,
  /requires? (sudo|root|admin) (access|privileges?)/i,
  /I (can't|cannot) physically/i,
];

/**
 * Check a response for settling patterns.
 *
 * @param {string} response - The agent's response text
 * @returns {{ settling: boolean, patterns: string[], allowlisted: boolean }}
 */
export function checkSettling(response) {
  if (!response || typeof response !== 'string') return { settling: false, patterns: [], allowlisted: false };

  const matched = SETTLING_PATTERNS.filter(p => p.test(response)).map(p => p.toString());
  if (matched.length === 0) return { settling: false, patterns: [], allowlisted: false };

  // Check if any allowlist pattern matches — these are genuine limits, not settling
  const allowlisted = ALLOWLIST_PATTERNS.some(p => p.test(response));

  return {
    settling: !allowlisted,
    patterns: matched,
    allowlisted,
  };
}

/**
 * Check for false capability claims (agent claims it can't do something it can).
 * Lower confidence — just flags, doesn't block.
 */
const FALSE_CAPABILITY_PATTERNS = [
  /I (can't|cannot) (send|post|message|notify)\b/i,
  /I (don't|do not) have (the ability|access) to (send|post|write)\b/i,
  /I('m| am) (unable|not able) to (search|look up|find|fetch|retrieve)\b/i,
];

export function checkFalseCapability(response) {
  if (!response) return { suspected: false, patterns: [] };
  const matched = FALSE_CAPABILITY_PATTERNS.filter(p => p.test(response)).map(p => p.toString());
  return { suspected: matched.length > 0, patterns: matched };
}

/**
 * Combined coherence check — run both detectors.
 */
export function coherenceCheck(response) {
  const settling = checkSettling(response);
  const capability = checkFalseCapability(response);
  const issues = [];

  if (settling.settling) issues.push({ type: 'settling', severity: 'warn', patterns: settling.patterns });
  if (capability.suspected) issues.push({ type: 'false-capability', severity: 'info', patterns: capability.patterns });

  return {
    pass: issues.length === 0,
    issues,
    response: response?.slice(0, 200), // For log context
  };
}

// CLI mode: pipe response text via stdin
if (process.argv[1]?.endsWith('settling-check.mjs')) {
  let data = '';
  process.stdin.on('data', d => (data += d));
  process.stdin.on('end', () => {
    const result = coherenceCheck(data);
    console.log(JSON.stringify(result, null, 2));
    process.exit(result.pass ? 0 : 1);
  });
}
