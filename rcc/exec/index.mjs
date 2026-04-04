/**
 * rcc/exec/index.mjs — HMAC-SHA256 signing/verification for ClawBus exec payloads
 *
 * Security model:
 * - All exec payloads MUST be signed with a shared secret (CLAWBUS_TOKEN)
 * - canonicalize() produces deterministic JSON (sorted keys, no whitespace)
 * - signPayload() computes HMAC-SHA256 over canonical JSON, returns hex sig
 * - verifyPayload() recomputes HMAC over envelope minus sig field, compares via timingSafeEqual
 * - NEVER trust an unsigned or tampered payload
 */

import { createHmac, timingSafeEqual } from 'crypto';

/**
 * Deterministic JSON stringify — sorts keys recursively, no whitespace.
 * Arrays preserve order; object keys are sorted.
 *
 * @param {any} obj
 * @returns {string}
 */
export function canonicalize(obj) {
  if (obj === null || typeof obj !== 'object') {
    return JSON.stringify(obj);
  }
  if (Array.isArray(obj)) {
    return '[' + obj.map(canonicalize).join(',') + ']';
  }
  const sortedKeys = Object.keys(obj).sort();
  const pairs = sortedKeys.map(k => JSON.stringify(k) + ':' + canonicalize(obj[k]));
  return '{' + pairs.join(',') + '}';
}

/**
 * Sign a payload object with HMAC-SHA256.
 * Returns hex signature string.
 *
 * @param {object} payload - the payload to sign (will be canonicalized)
 * @param {string} secret  - shared secret (CLAWBUS_TOKEN)
 * @returns {string} hex HMAC-SHA256 signature
 */
export function signPayload(payload, secret) {
  const canonical = canonicalize(payload);
  return createHmac('sha256', secret).update(canonical).digest('hex');
}

/**
 * Verify a signed envelope.
 * Recomputes HMAC over envelope minus the `sig` field, compares in constant time.
 *
 * @param {object} envelope - full envelope including `sig` field
 * @param {string} secret   - shared secret (CLAWBUS_TOKEN)
 * @returns {boolean} true if valid, false if tampered/missing sig
 */
export function verifyPayload(envelope, secret) {
  if (!envelope || typeof envelope.sig !== 'string') return false;

  // Reconstruct payload without sig field
  const { sig, ...payload } = envelope;
  const expected = signPayload(payload, secret);

  // Constant-time comparison to prevent timing attacks
  try {
    const expectedBuf = Buffer.from(expected, 'utf8');
    const actualBuf   = Buffer.from(sig,      'utf8');
    if (expectedBuf.length !== actualBuf.length) return false;
    return timingSafeEqual(expectedBuf, actualBuf);
  } catch {
    return false;
  }
}
