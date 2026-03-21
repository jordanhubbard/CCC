#!/usr/bin/env node
/**
 * test-crash-reporter.mjs — Quick test for the crash reporter module
 *
 * 1. Imports the crash reporter
 * 2. Reads queue.json before
 * 3. Throws an unhandled exception after 1 second
 * 4. The crash reporter should catch it, write to queue.json, then exit
 *
 * After running, check queue.json for a new crash task with tag "test-crash-reporter"
 */

import { initCrashReporter } from './crash-reporter.mjs';
import { readFileSync } from 'fs';

const QUEUE_PATH = '/home/jkh/.openclaw/workspace/workqueue/queue.json';

// Count existing crash tasks
const before = JSON.parse(readFileSync(QUEUE_PATH, 'utf8'));
const crashCountBefore = before.items.filter(i => i.tags && i.tags.includes('crash')).length;
console.log(`[test] Crash tasks before: ${crashCountBefore}`);
console.log(`[test] Total tasks before: ${before.items.length}`);

// Initialize crash reporter for test service
initCrashReporter({
  service: 'test-crash-reporter',
  sourceDir: '/home/jkh/.openclaw/workspace/lib'
});

console.log('[test] Crash reporter initialized. Throwing unhandled exception in 1 second...');

// Throw after a delay so the event loop has time to register the handler
setTimeout(() => {
  throw new Error('TEST CRASH: This is a deliberate test crash — ignore me!');
}, 1000);
