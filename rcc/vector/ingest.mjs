/**
 * rcc/vector/ingest.mjs — Ingest helpers for Milvus RAG pipeline
 *
 * Provides fire-and-forget ingest functions for all write paths:
 *   - Memory files (.md)
 *   - Queue items
 *   - Lessons
 *   - SquirrelChat messages
 *
 * All failures are logged but never thrown — ingest is best-effort.
 */

import { readFile } from 'fs/promises';
import { createHash } from 'crypto';
import { upsert, ensureCollections } from './index.mjs';

let collectionsReady = false;

async function ensureReady() {
  if (!collectionsReady) {
    await ensureCollections();
    collectionsReady = true;
  }
}

/**
 * Chunk a markdown file by headings + paragraphs.
 * Returns [{id, text, metadata}]
 */
function chunkMarkdown(content, filePath) {
  const chunks = [];
  const sections = content.split(/\n(?=#{1,3} )/);
  for (const section of sections) {
    const trimmed = section.trim();
    if (!trimmed || trimmed.length < 20) continue;
    // Split long sections into ~500-char paragraphs
    const paragraphs = trimmed.split(/\n{2,}/);
    for (const para of paragraphs) {
      const text = para.trim();
      if (!text || text.length < 20) continue;
      const id = createHash('md5').update(`${filePath}:${text.slice(0, 100)}`).digest('hex');
      chunks.push({ id, text: text.slice(0, 1000), metadata: { source: filePath, type: 'memory' } });
    }
  }
  return chunks;
}

/**
 * Ingest a markdown memory file into rcc_memory collection.
 * Chunks by heading/paragraph, upserts each chunk.
 */
export async function ingestMemoryFile(filePath) {
  try {
    await ensureReady();
    const content = await readFile(filePath, 'utf8');
    const chunks = chunkMarkdown(content, filePath);
    let count = 0;
    for (const chunk of chunks) {
      await upsert('rcc_memory', chunk).catch(() => {});
      count++;
    }
    console.log(`[ingest] ${filePath} → ${count}/${chunks.length} chunks`);
  } catch (err) {
    console.warn(`[ingest] Failed to ingest ${filePath}:`, err.message);
  }
}

/**
 * Ingest a queue item into rcc_queue collection.
 */
export async function ingestQueueItem(item) {
  try {
    await ensureReady();
    const text = [item.title, item.description, item.notes].filter(Boolean).join('\n').slice(0, 1000);
    if (!text || text.length < 10) return;
    const id = createHash('md5').update(`queue:${item.id}`).digest('hex');
    await upsert('rcc_queue', {
      id,
      text,
      metadata: { source: `queue:${item.id}`, type: 'queue', status: item.status, assignee: item.assignee }
    });
  } catch (err) {
    console.warn(`[ingest] Failed to ingest queue item ${item.id}:`, err.message);
  }
}

/**
 * Ingest a lesson into rcc_lessons collection.
 */
export async function ingestLesson(lesson) {
  try {
    await ensureReady();
    const text = [lesson.symptom, lesson.fix, lesson.context].filter(Boolean).join('\n').slice(0, 1000);
    if (!text || text.length < 10) return;
    const id = createHash('md5').update(`lesson:${lesson.id || text.slice(0, 50)}`).digest('hex');
    await upsert('rcc_lessons', {
      id,
      text,
      metadata: { source: `lesson:${lesson.id || 'unknown'}`, type: 'lesson' }
    });
  } catch (err) {
    console.warn(`[ingest] Failed to ingest lesson:`, err.message);
  }
}

/**
 * Ingest a SquirrelChat message into rcc_memory collection.
 */
export async function ingestMessage(msg) {
  try {
    await ensureReady();
    const text = msg.text || '';
    if (!text || text.length < 5) return;
    const id = createHash('md5').update(`squirrelchat:${msg.id}:${msg.ts}`).digest('hex');
    await upsert('rcc_memory', {
      id,
      text: text.slice(0, 1000),
      metadata: {
        source: `squirrelchat:${msg.id}`,
        type: 'squirrelchat',
        from_agent: msg.from_agent || 'unknown',
        channel: msg.channel || 'chat'
      }
    });
  } catch (err) {
    console.warn(`[ingest] Failed to ingest message ${msg.id}:`, err.message);
  }
}
