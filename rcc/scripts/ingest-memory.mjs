#!/usr/bin/env node
/**
 * rcc/scripts/ingest-memory.mjs — One-shot memory backfill into Milvus
 *
 * Seeds Milvus rcc_memory collection with agent memory files.
 *
 * Paths (in priority order):
 *   1. MEMORY_DIR env var — explicit override
 *   2. ~/.openclaw/workspace/memory/ — OpenClaw-managed layout (Rocky, Bullwinkle)
 *   3. ~/.rcc/workspace/memory/ — CCC-only layout
 *
 * MEMORY.md is looked for at MEMORY_FILE env var, or sibling to memory/ dir.
 *
 * Run: node rcc/scripts/ingest-memory.mjs
 */

import { readdir } from 'fs/promises';
import { existsSync } from 'fs';
import { join, dirname } from 'path';
import { homedir } from 'os';
import { ensureCollections } from '../vector/index.mjs';
import { ingestMemoryFile } from '../vector/ingest.mjs';

const HOME = homedir();

// Resolve memory directory
function resolveMemoryDir() {
  if (process.env.MEMORY_DIR && existsSync(process.env.MEMORY_DIR)) return process.env.MEMORY_DIR;
  const openclaw = join(HOME, '.openclaw', 'workspace', 'memory');
  if (existsSync(openclaw)) return openclaw;
  const rcc = join(HOME, '.rcc', 'workspace', 'memory');
  if (existsSync(rcc)) return rcc;
  return null;
}

// Resolve MEMORY.md
function resolveMemoryMd(memoryDir) {
  if (process.env.MEMORY_FILE && existsSync(process.env.MEMORY_FILE)) return process.env.MEMORY_FILE;
  if (!memoryDir) return null;
  const sibling = join(dirname(memoryDir), 'MEMORY.md');
  if (existsSync(sibling)) return sibling;
  return null;
}

const memoryDir = resolveMemoryDir();
const memoryMd  = resolveMemoryMd(memoryDir);

if (!memoryDir && !memoryMd) {
  console.error('[ingest-memory] No memory directory found. Set MEMORY_DIR env var or create ~/.openclaw/workspace/memory/');
  process.exit(1);
}

console.log('[ingest-memory] Ensuring Milvus collections...');
await ensureCollections();
console.log('[ingest-memory] Collections ready');

const files = [];

if (memoryMd) {
  files.push(memoryMd);
  console.log(`[ingest-memory] MEMORY.md: ${memoryMd}`);
}

if (memoryDir) {
  const entries = await readdir(memoryDir);
  for (const entry of entries) {
    if (entry.endsWith('.md')) files.push(join(memoryDir, entry));
  }
  console.log(`[ingest-memory] memory/ dir: ${memoryDir} (${entries.filter(e => e.endsWith('.md')).length} files)`);
}

console.log(`[ingest-memory] Ingesting ${files.length} file(s)...`);
for (const filePath of files) {
  await ingestMemoryFile(filePath);
}
console.log('[ingest-memory] Done.');
