#!/usr/bin/env bash
# set-model-routing.sh — write fleet-wide model/CLI routing into ACC secrets.
#
# Examples:
#   bash deploy/set-model-routing.sh --model azure/openai/gpt-5.4 \
#     --provider openai --base-url https://inference-api.nvidia.com/v1 \
#     --cli-order cursor_cli,codex_cli,claude_cli --restart
#
#   OPENAI_API_KEY=sk-... bash deploy/set-model-routing.sh --model gpt-5.5 \
#     --provider openai --base-url https://api.openai.com/v1 \
#     --api-key-env OPENAI_API_KEY --restart
#
#   bash deploy/set-model-routing.sh --auto --shortlist 4 --restart
set -euo pipefail

if [[ -f "$HOME/.acc/.env" ]]; then
  set -a
  # shellcheck source=/dev/null
  source "$HOME/.acc/.env"
  set +a
fi

HUB_URL="${ACC_URL:-${CCC_URL:-http://localhost:8789}}"
TOKEN="${ACC_AGENT_TOKEN:-${ACC_TOKEN:-${CCC_AGENT_TOKEN:-}}}"
MODEL=""
PROVIDER="openai"
BASE_URL=""
API_KEY_ENV=""
API_KEY_FILE=""
CLI_ORDER=""
RESTART=false
AUTO=false
SHORTLIST=4
PROBE_TARGET=""
PREFER_TAILSCALE="${PREFER_TAILSCALE:-true}"

usage() {
  sed -n '2,16p' "$0" | sed 's/^# \{0,1\}//'
  cat <<'EOF'

Options:
  --model <name>          Model ID to use for Hermes/API-backed agents.
  --provider <name>       Provider type: openai or anthropic. Default: openai.
  --base-url <url>        Provider base URL, e.g. https://api.openai.com/v1.
  --api-key-env <var>     Store API key from environment variable <var>.
  --api-key-file <path>   Store API key read from file.
  --cli-order <list>      Default CLI order, e.g. codex_cli,cursor_cli,claude_cli.
  --auto                  Fetch catalog, shortlist Claude/GPT models, and probe until one works.
  --shortlist <n>         Candidate count for --auto. Default: 4.
  --probe-target <agent>  Prefer this registered agent for catalog/probe.
  --restart              Run deploy/restart-fleet.sh after writing secrets.
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --model) MODEL="${2:-}"; shift 2 ;;
    --provider) PROVIDER="${2:-}"; shift 2 ;;
    --base-url) BASE_URL="${2:-}"; shift 2 ;;
    --api-key-env) API_KEY_ENV="${2:-}"; shift 2 ;;
    --api-key-file) API_KEY_FILE="${2:-}"; shift 2 ;;
    --cli-order) CLI_ORDER="${2:-}"; shift 2 ;;
    --auto) AUTO=true; shift ;;
    --shortlist) SHORTLIST="${2:-4}"; shift 2 ;;
    --probe-target) PROBE_TARGET="${2:-}"; shift 2 ;;
    --restart) RESTART=true; shift ;;
    -h|--help) usage; exit 0 ;;
    *)
      if [[ -z "$MODEL" ]]; then
        MODEL="$1"
        shift
      else
        echo "[set-model-routing] unknown argument: $1" >&2
        usage >&2
        exit 2
      fi
      ;;
  esac
done

if [[ -z "$TOKEN" ]]; then
  echo "[set-model-routing] ERROR: ACC_AGENT_TOKEN/CCC_AGENT_TOKEN is not set" >&2
  exit 1
fi

if [[ -z "$MODEL" ]]; then
  AUTO=true
fi

json_body() {
  python3 -c 'import json,sys; print(json.dumps({"value": sys.argv[1]}))' "$1"
}

set_secret() {
  local key="$1" value="$2"
  curl -sf -X POST "${HUB_URL%/}/api/secrets/${key}" \
    -H "Authorization: Bearer ${TOKEN}" \
    -H "Content-Type: application/json" \
    -d "$(json_body "$value")" >/dev/null
  echo "[set-model-routing] set ${key}"
}

auto_select_model() {
  local agents_json
  agents_json="$(curl -sf --connect-timeout 5 --max-time 20 \
    -H "Authorization: Bearer ${TOKEN}" \
    "${HUB_URL%/}/api/agents?online=true")"
  local targets=""
  targets="$(printf '%s\n' "$agents_json" | jq -r '
    (.agents // .)[] |
    select(.ssh_user != null and .ssh_user != "" and .ssh_host != null and .ssh_host != "") |
    [
      (.name // "?"),
      .ssh_user,
      .ssh_host,
      (.ssh_port // 22),
      (if (.tailscale_ip | type) == "string" and .tailscale_ip != "" then .tailscale_ip else "-" end)
    ] | @tsv')"
  if [[ -z "$targets" ]]; then
    echo "[set-model-routing] ERROR: no online agents with SSH coordinates for probing" >&2
    return 1
  fi

  local row name user host port tailscale_ip ssh_host result status
  while IFS=$'\t' read -r name user host port tailscale_ip; do
    [[ -n "$name" ]] || continue
    if [[ -n "$PROBE_TARGET" && "$PROBE_TARGET" != "$name" && "$PROBE_TARGET" != "$host" && "$PROBE_TARGET" != "$tailscale_ip" ]]; then
      continue
    fi
    ssh_host="$host"
    if [[ "$PREFER_TAILSCALE" == "true" && "$tailscale_ip" != "-" ]]; then
      ssh_host="$tailscale_ip"
    fi
    echo "[set-model-routing] probing model catalog on ${name} (${user}@${ssh_host}:${port})" >&2
    set +e
    result="$(ssh -o ConnectTimeout=10 \
      -o BatchMode=yes \
      -o StrictHostKeyChecking=accept-new \
      -p "$port" \
      "${user}@${ssh_host}" \
      "ACC_SHORTLIST=${SHORTLIST} bash -s" <<'REMOTE'
set -u
if [ -f "$HOME/.acc/.env" ]; then
  set -a
  . "$HOME/.acc/.env"
  set +a
fi

base="${OPENAI_BASE_URL:-${LLM_URL:-${NVIDIA_API_BASE:-https://inference-api.nvidia.com/v1}}}"
key="${OPENAI_API_KEY:-${LLM_KEY:-${NVIDIA_API_KEY:-}}}"
if [ -z "$base" ] || [ -z "$key" ]; then
  echo "[remote-probe] missing OpenAI-compatible base URL or key" >&2
  exit 42
fi

tmp="${TMPDIR:-/tmp}/acc-model-probe.$$"
mkdir -p "$tmp"
trap 'rm -rf "$tmp"' EXIT

catalog="$tmp/catalog.json"
if ! curl -sf --connect-timeout 5 --max-time 20 \
  -H "Authorization: Bearer $key" \
  "${base%/}/models" > "$catalog"; then
  echo "[remote-probe] failed to fetch ${base%/}/models" >&2
  exit 43
fi

candidates="$(python3 - "$catalog" "${ACC_SHORTLIST:-4}" <<'PY'
import json, re, sys

catalog_path = sys.argv[1]
limit = int(sys.argv[2])
with open(catalog_path) as f:
    data = json.load(f)

ids = []
for item in data.get("data", []):
    mid = item.get("id") if isinstance(item, dict) else None
    if isinstance(mid, str):
        ids.append(mid)

def usable(mid: str) -> bool:
    s = mid.lower()
    rejects = ["embedding", "embed", "rerank", "evals", "fake-", "vision", "audio", "image", "batch"]
    return not any(r in s for r in rejects)

def gpt_score(mid: str):
    s = mid.lower()
    if "gpt" not in s or not usable(mid) or "codex" in s:
        return None
    if "gpt-oss" in s:
        return 100
    m = re.search(r"gpt-([0-9]+)(?:\.([0-9]+))?", s)
    if not m:
        return 0
    major = int(m.group(1))
    minor = int(m.group(2) or 0)
    score = major * 1000 + minor * 100
    if "chat" in s:
        score += 20
    if s.startswith("azure/openai/"):
        score += 12
    elif s.startswith("us/azure/openai/"):
        score += 10
    elif s.startswith("openai/"):
        score += 8
    return score

def claude_score(mid: str):
    s = mid.lower()
    if "claude" not in s or not usable(mid):
        return None
    tier_score = 0
    if "opus" in s:
        tier_score = 30
    elif "sonnet" in s:
        tier_score = 20
    elif "haiku" in s:
        tier_score = 10
    m = re.search(r"claude-[a-z]+-([0-9]+)-([0-9]+)", s)
    if not m:
        return tier_score
    major = int(m.group(1))
    minor = int(m.group(2))
    return major * 1000 + minor * 100 + tier_score

gpts = sorted(((gpt_score(mid), mid) for mid in ids if gpt_score(mid) is not None), reverse=True)
claudes = sorted(((claude_score(mid), mid) for mid in ids if claude_score(mid) is not None), reverse=True)

selected = []
families = [gpts, claudes]
while len(selected) < limit and any(families):
    next_families = []
    for family in families:
        while family and family[0][1] in selected:
            family.pop(0)
        if family and len(selected) < limit:
            selected.append(family.pop(0)[1])
        if family:
            next_families.append(family)
    families = next_families

for mid in selected[:limit]:
    print(mid)
PY
)"

if [ -z "$candidates" ]; then
  echo "[remote-probe] model catalog contained no Claude/GPT candidates" >&2
  exit 44
fi

printf '%s\n' "$candidates" | sed 's/^/[remote-probe] candidate /' >&2

while IFS= read -r model; do
  [ -n "$model" ] || continue
  body="$(python3 - "$model" <<'PY'
import json, sys
print(json.dumps({
    "model": sys.argv[1],
    "max_tokens": 16,
    "messages": [{"role": "user", "content": "Reply OK only"}],
}))
PY
)"
  resp="$tmp/resp.json"
  code="$(curl -sS --connect-timeout 5 --max-time 30 -o "$resp" -w "%{http_code}" \
    -H "Authorization: Bearer $key" \
    -H "Content-Type: application/json" \
    -d "$body" \
    "${base%/}/chat/completions" 2>"$tmp/curl.err" || true)"
  if python3 - "$resp" "$code" <<'PY'
import json, sys
path, code = sys.argv[1], sys.argv[2]
if not code.startswith("2"):
    sys.exit(1)
try:
    data = json.load(open(path))
except Exception:
    sys.exit(1)
choices = data.get("choices") or []
if choices and (choices[0].get("message") or {}).get("content") is not None:
    sys.exit(0)
sys.exit(1)
PY
  then
    echo "SELECTED|openai|${base%/}|$model"
    exit 0
  fi
  msg="$(python3 - "$resp" <<'PY' 2>/dev/null || true
import json, sys
try:
    data = json.load(open(sys.argv[1]))
    print((data.get("error") or {}).get("message") or data.get("error") or "")
except Exception:
    pass
PY
)"
  echo "[remote-probe] $model failed http=$code ${msg:0:180}" >&2
done <<< "$candidates"

exit 45
REMOTE
)"
    status=$?
    set -e
    if [[ "$status" -eq 0 && "$result" == SELECTED\|* ]]; then
      printf '%s\n' "$result"
      return 0
    fi
    echo "[set-model-routing] probe failed on ${name} (status=${status})" >&2
  done <<< "$targets"

  return 1
}

if $AUTO; then
  selected="$(auto_select_model)"
  IFS='|' read -r selected_tag PROVIDER BASE_URL MODEL <<EOF
$selected
EOF
  if [[ "$selected_tag" != "SELECTED" || -z "$MODEL" || -z "$BASE_URL" ]]; then
    echo "[set-model-routing] ERROR: automatic model selection failed" >&2
    exit 1
  fi
  echo "[set-model-routing] selected ${MODEL} via ${PROVIDER} at ${BASE_URL}"
fi

if [[ -n "$MODEL" ]]; then
  set_secret "hermes/provider" "$PROVIDER"
  set_secret "hermes/model" "$MODEL"
  case "$PROVIDER" in
    openai|openai-compat)
      set_secret "openai/model" "$MODEL"
      [[ -n "$BASE_URL" ]] && set_secret "openai/base_url" "$BASE_URL"
      ;;
    anthropic)
      set_secret "anthropic/model" "$MODEL"
      [[ -n "$BASE_URL" ]] && set_secret "anthropic/base_url" "$BASE_URL"
      ;;
    *)
      echo "[set-model-routing] WARNING: provider '$PROVIDER' will be passed through as-is" >&2
      ;;
  esac
fi

if [[ -n "$API_KEY_ENV" ]]; then
  value="${!API_KEY_ENV:-}"
  if [[ -z "$value" ]]; then
    echo "[set-model-routing] ERROR: environment variable $API_KEY_ENV is empty" >&2
    exit 1
  fi
  case "$PROVIDER" in
    anthropic) set_secret "anthropic/api_key" "$value" ;;
    *)         set_secret "openai/api_key" "$value" ;;
  esac
fi

if [[ -n "$API_KEY_FILE" ]]; then
  if [[ ! -f "$API_KEY_FILE" ]]; then
    echo "[set-model-routing] ERROR: API key file not found: $API_KEY_FILE" >&2
    exit 1
  fi
  value="$(tr -d '\r\n' < "$API_KEY_FILE")"
  case "$PROVIDER" in
    anthropic) set_secret "anthropic/api_key" "$value" ;;
    *)         set_secret "openai/api_key" "$value" ;;
  esac
fi

if [[ -n "$CLI_ORDER" ]]; then
  set_secret "cli/executor_order" "$CLI_ORDER"
fi

if $RESTART; then
  bash "$(dirname "$0")/restart-fleet.sh"
fi
