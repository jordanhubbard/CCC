#!/usr/bin/env bash
# 0022_vendor_hermes_skills.sh
# Installs vendored agent-skills and superpowers from the workspace into ~/.hermes/skills/.
# Replaces the old approach of cloning external repos (agent-skills, superpowers) at bootstrap time.
set -euo pipefail

ACC_DIR="${HOME}/.acc"
WORKSPACE="${ACC_DIR}/workspace"
SKILLS_DST="${HOME}/.hermes/skills"

GREEN='\033[0;32m'; YELLOW='\033[1;33m'; NC='\033[0m'
ok()   { echo -e "${GREEN}✓${NC} $1"; }
warn() { echo -e "${YELLOW}⚠${NC} $1"; }

if [[ ! -d "$WORKSPACE/.git" ]]; then
  warn "Workspace not found at $WORKSPACE — run setup-node.sh first"
  exit 1
fi

mkdir -p "$SKILLS_DST"

# ── agent-skills ─────────────────────────────────────────────────────────────
if [[ -d "$WORKSPACE/skills/agent-skills" ]]; then
  _count=0
  for _skill_dir in "$WORKSPACE/skills/agent-skills"/*/; do
    [[ -d "$_skill_dir" ]] || continue
    _name="$(basename "$_skill_dir")"
    cp -r "$_skill_dir" "$SKILLS_DST/${_name}/"
    _count=$((_count + 1))
  done
  ok "agent-skills: ${_count} skills → $SKILLS_DST"
else
  warn "skills/agent-skills not found in workspace — pull latest and retry"
fi

# ── superpowers ───────────────────────────────────────────────────────────────
if [[ -d "$WORKSPACE/skills/superpowers" ]]; then
  _count=0
  for _skill_file in "$WORKSPACE/skills/superpowers"/*.md; do
    [[ -f "$_skill_file" ]] || continue
    _name="$(basename "$_skill_file" .md)"
    [[ "$_name" == "using-superpowers" ]] && continue
    cp "$_skill_file" "$SKILLS_DST/${_name}.md"
    _count=$((_count + 1))
  done
  for _skill_dir in "$WORKSPACE/skills/superpowers"/*/; do
    [[ -d "$_skill_dir" ]] || continue
    _name="$(basename "$_skill_dir")"
    cp -r "$_skill_dir" "$SKILLS_DST/${_name}/"
    _count=$((_count + 1))
  done
  ok "superpowers: ${_count} skills → $SKILLS_DST"
else
  warn "skills/superpowers not found in workspace — pull latest and retry"
fi

ok "Migration 0022 complete — Hermes skills up to date from vendored source"
