#!/usr/bin/env node
/**
 * memory-digest.mjs — Cross-agent shared memory summarizer
 * wq-N-005 | Natasha, 2026-03-21
 *
 * Reads MEMORY.md + recent daily memory files, generates a compact weekly
 * digest, and publishes it to MinIO at:
 *   agents/shared/memory-digest-natasha-YYYY-Www.md
 *
 * Usage: node memory-digest.mjs [--dry-run]
 */

import fs from "fs";
import path from "path";
import { execSync } from "child_process";

const WORKSPACE = process.env.WORKSPACE || "/home/jkh/.openclaw/workspace";
const MINIO_ENDPOINT = "http://100.89.199.14:9000";
const MINIO_BUCKET = "agents";
const MINIO_ACCESS = "rockymoose4810f4cc7d28916f";
const MINIO_SECRET = "<MINIO_SECRET_KEY>";
const DRY_RUN = process.argv.includes("--dry-run");

// ISO week string: YYYY-Www
function isoWeek(date = new Date()) {
  const d = new Date(Date.UTC(date.getFullYear(), date.getMonth(), date.getDate()));
  const dayNum = d.getUTCDay() || 7;
  d.setUTCDate(d.getUTCDate() + 4 - dayNum);
  const yearStart = new Date(Date.UTC(d.getUTCFullYear(), 0, 1));
  const weekNum = Math.ceil((((d - yearStart) / 86400000) + 1) / 7);
  return `${d.getUTCFullYear()}-W${String(weekNum).padStart(2, "0")}`;
}

// Get dates for the past 7 days (YYYY-MM-DD)
function recentDates(n = 7) {
  const dates = [];
  for (let i = 0; i < n; i++) {
    const d = new Date();
    d.setDate(d.getDate() - i);
    dates.push(d.toISOString().slice(0, 10));
  }
  return dates;
}

// Read a file safely
function readFile(fp) {
  try {
    return fs.readFileSync(fp, "utf8");
  } catch {
    return null;
  }
}

// Extract key sections from MEMORY.md for the digest
function summarizeMemory(content) {
  if (!content) return "(no MEMORY.md found)";
  const lines = content.split("\n");
  const sections = [];
  let current = null;
  let depth = 0;

  for (const line of lines) {
    if (line.startsWith("## ")) {
      if (current) sections.push(current);
      current = { header: line, lines: [] };
      depth = 2;
    } else if (line.startsWith("### ") && current) {
      current.lines.push(line);
    } else if (current) {
      current.lines.push(line);
    }
  }
  if (current) sections.push(current);

  // Keep sections that are substantive (more than 3 lines of content)
  return sections
    .filter(s => s.lines.filter(l => l.trim()).length >= 2)
    .map(s => {
      // Truncate each section to ~8 lines to keep digest compact
      const body = s.lines.filter(l => l.trim()).slice(0, 8).join("\n");
      return `${s.header}\n${body}`;
    })
    .join("\n\n");
}

// Summarize daily memory files
function summarizeDailyLogs(dates) {
  const entries = [];
  for (const date of dates) {
    const fp = path.join(WORKSPACE, "memory", `${date}.md`);
    const content = readFile(fp);
    if (content && content.trim()) {
      // Take first 15 non-empty lines as summary
      const lines = content.split("\n").filter(l => l.trim()).slice(0, 15).join("\n");
      entries.push(`### ${date}\n${lines}`);
    }
  }
  return entries.length ? entries.join("\n\n") : "(no daily logs in past 7 days)";
}

// Read completed workqueue items from this week (from queue.json)
function recentCompletions() {
  const fp = path.join(WORKSPACE, "workqueue", "queue.json");
  const raw = readFile(fp);
  if (!raw) return "(queue.json not found)";
  const queue = JSON.parse(raw);
  const cutoff = new Date(Date.now() - 7 * 86400 * 1000);
  const all = [...(queue.items || []), ...(queue.completed || [])];
  const recent = all.filter(item =>
    item.status === "completed" &&
    item.source === "natasha" &&
    item.completedAt &&
    new Date(item.completedAt) >= cutoff
  );
  if (!recent.length) return "(no Natasha-completed items this week)";
  return recent.map(item =>
    `- **${item.id}** ${item.title} → ${(item.result || "").slice(0, 120)}`
  ).join("\n");
}

// Main
const week = isoWeek();
const now = new Date().toISOString();
const dates = recentDates(7);

const memoryContent = readFile(path.join(WORKSPACE, "MEMORY.md"));
const memorySummary = summarizeMemory(memoryContent);
const dailySummary = summarizeDailyLogs(dates);
const completions = recentCompletions();

const digest = `# Memory Digest — Natasha | ${week}
_Generated: ${now}_

This digest is published weekly to MinIO for peer agents (Rocky, Bullwinkle) to read.
It summarizes Natasha's current mental model, recent learnings, and completed work.

---

## Key Facts & Lessons Learned

${memorySummary}

---

## Daily Log Highlights (Past 7 Days)

${dailySummary}

---

## Work Completed This Week (Natasha as source)

${completions}

---

## Skills & Capabilities (Natasha)

- **GPU:** DGX Spark, GB10, 128GB unified memory. RTX rendering, Blender 4.0.2, Whisper transcription, local embeddings (nomic-embed-text-v1.5)
- **Skills:** blender-render, whisper-transcription, embedding-index, workqueue-processor
- **Routing:** Send GPU/render/transcription/embedding tasks to Natasha. Send infra/always-on to Rocky. Mac/browser/calendar to Bullwinkle.

## Memory Caveats

- MEMORY.md is **not shared in group contexts** (security). Only main session reads it.
- Daily logs are raw; MEMORY.md is curated. This digest bridges both.
- If Sparky crashed (OOM), check for a gap in daily logs — Natasha may have lost session memory.

---
_Next digest: ${isoWeek(new Date(Date.now() + 7 * 86400 * 1000))}_
`;

const filename = `memory-digest-natasha-${week}.md`;
const remotePath = `${MINIO_BUCKET}/shared/${filename}`;
const localPath = path.join(WORKSPACE, "workqueue", filename);

// Write locally first
fs.writeFileSync(localPath, digest, "utf8");
console.log(`[memory-digest] Written locally: ${localPath}`);

if (DRY_RUN) {
  console.log("[memory-digest] DRY RUN — skipping MinIO upload.");
  console.log(digest.slice(0, 800) + "\n...(truncated)");
  process.exit(0);
}

// Upload to MinIO via curl SigV4
try {
  const cmd = [
    "curl", "-sf", "--max-time", "15",
    "--aws-sigv4", "aws:amz:us-east-1:s3",
    "--user", `${MINIO_ACCESS}:${MINIO_SECRET}`,
    "-X", "PUT",
    "-H", "Content-Type: text/markdown",
    `--data-binary`, `@${localPath}`,
    `${MINIO_ENDPOINT}/${remotePath}`
  ].join(" ");
  execSync(cmd, { stdio: "pipe" });
  console.log(`[memory-digest] Uploaded to MinIO: ${remotePath}`);
} catch (e) {
  console.error("[memory-digest] MinIO upload failed:", e.message);
  process.exit(1);
}

console.log(`[memory-digest] Done. Week: ${week}`);
