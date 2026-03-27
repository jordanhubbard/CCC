#!/usr/bin/env node
/**
 * rcc/scripts/ingest-memory.mjs — One-shot backfill script
 *
 * Seeds Milvus with:
 *   1. All .md files in /home/jkh/.openclaw/workspace/memory/
 *   2. /home/jkh/.openclaw/workspace/MEMORY.md
 *
 * Run once: node rcc/scripts/ingest-memory.mjs
 */

import { readdir } from 'fs/promises';
import { existsSync } from 'fs';
import { join } from 'path';
import { ensureCollections } from '../vector/index.mjs';
import { ingestMemoryFile } from '../vector/ingest.mjs';

const MEMORY_DIR  = '/home/jkh/.openclaw/workspace/memory';
const MEMORY_ROOT = '/home/jkh/.openclaw/workspace/MEMORY.md';

console.log('[ingest-memory] Ensuring collections...');
await ensureCollections();
console.log('[ingest-memory] Collections ready');

const files = [];

if (existsSync(MEMORY_ROOT)) {
  files.push(MEMORY_ROOT);
}

if (existsSync(MEMORY_DIR)) {
  const entries = await readdir(MEMORY_DIR);
  for (const entry of entries) {
    if (entry.endsWith('.md')) {
      files.push(join(MEMORY_DIR, entry));
    }
  }
}

console.log(`[ingest-memory] Found ${files.length} files to ingest`);

for (const filePath of files) {
  await ingestMemoryFile(filePath);
}

console.log('[ingest-memory] Done.');
