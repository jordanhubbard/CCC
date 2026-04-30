#!/usr/bin/env bash
# reconcile-hub-supervisor.sh — Keep hub acc-server worker config in sync.
#
# Hub nodes run acc-agent workers under acc-server's supervisor using
# ~/.acc/acc.json, so worker additions that are gated by local env must also be
# represented in that process list. This script adds per-workspace Slack gateway
# processes when matching SLACK_APP_TOKEN_<WORKSPACE> env vars are present.
set -euo pipefail

ACC_DIR="${ACC_DIR:-$HOME/.acc}"
ENV_FILE="${ACC_DIR}/.env"
CONFIG_FILE="${ACC_CONFIG:-${ACC_DIR}/acc.json}"

if [[ -f "$ENV_FILE" ]]; then
  set -a
  # shellcheck source=/dev/null
  source "$ENV_FILE"
  set +a
fi

if [[ ! -f "$CONFIG_FILE" ]]; then
  echo "[reconcile-hub-supervisor] no acc.json at $CONFIG_FILE — skipping"
  exit 0
fi

if ! command -v python3 >/dev/null 2>&1; then
  echo "[reconcile-hub-supervisor] python3 unavailable — cannot edit $CONFIG_FILE" >&2
  exit 0
fi

declare -A WORKSPACES=()
while IFS='=' read -r key value; do
  case "$key" in
    SLACK_APP_TOKEN_*)
      [[ "$value" == xapp-* ]] || continue
      suffix="${key#SLACK_APP_TOKEN_}"
      suffix="$(printf '%s' "$suffix" | tr '[:upper:]_' '[:lower:]-')"
      # Historical typo compatibility.
      [[ "$suffix" == "ofterra" ]] && suffix="offtera"
      [[ -n "$suffix" ]] && WORKSPACES["$suffix"]=1
      ;;
  esac
done < <(env)

if [[ "${#WORKSPACES[@]}" -eq 0 ]]; then
  echo "[reconcile-hub-supervisor] no non-default Slack workspace app tokens — nothing to add"
  exit 0
fi

python3 - "$CONFIG_FILE" "${!WORKSPACES[@]}" <<'PY'
import json
import os
import sys
from pathlib import Path

path = Path(sys.argv[1]).expanduser()
workspaces = sorted({w.lower() for w in sys.argv[2:] if w.lower() not in {"", "default", "omgjkh"}})
if not workspaces:
    print("[reconcile-hub-supervisor] no workspace gateways requested")
    sys.exit(0)

try:
    data = json.loads(path.read_text())
except Exception as exc:
    print(f"[reconcile-hub-supervisor] cannot parse {path}: {exc}", file=sys.stderr)
    sys.exit(0)

supervisor = data.setdefault("supervisor", {})
processes = supervisor.setdefault("processes", [])
if not isinstance(processes, list):
    print(f"[reconcile-hub-supervisor] supervisor.processes is not an array in {path}", file=sys.stderr)
    sys.exit(0)

existing_names = {p.get("name") for p in processes if isinstance(p, dict)}
existing_workspaces = set()
for process in processes:
    if not isinstance(process, dict):
        continue
    args = process.get("args") or []
    if not isinstance(args, list):
        continue
    for idx, arg in enumerate(args[:-1]):
        if arg == "--workspace":
            existing_workspaces.add(str(args[idx + 1]).lower())

added = []
for workspace in workspaces:
    name = f"gateway-{workspace}"
    if name in existing_names or workspace in existing_workspaces:
        continue
    processes.append(
        {
            "name": name,
            "command": "acc-agent",
            "args": ["hermes", "--gateway", "--workspace", workspace],
        }
    )
    existing_names.add(name)
    existing_workspaces.add(workspace)
    added.append(name)

if not added:
    print("[reconcile-hub-supervisor] workspace gateway process already present")
    sys.exit(0)

tmp = path.with_suffix(path.suffix + f".tmp.{os.getpid()}")
tmp.write_text(json.dumps(data, indent=2) + "\n")
tmp.replace(path)
print("[reconcile-hub-supervisor] added " + ", ".join(added))
PY
