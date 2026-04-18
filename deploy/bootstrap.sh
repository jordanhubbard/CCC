#!/usr/bin/env bash
# bootstrap.sh — One-command agent bootstrap from CCC
# Installs hermes-agent, seeds workspace, configures agent identity.
#
# Usage:
#   curl -sSL https://raw.githubusercontent.com/${ACC_OWNER}/${ACC_REPO}/main/deploy/bootstrap.sh | \
#     bash -s -- --ccc=http://<your-ccc-hub-url>:8789 --token=<bootstrap-token> --agent=boris
#
# If you have a pre-known agent token, pass --agent-token=<token> to skip the bootstrap API call.
# All secrets (NVIDIA key, channel tokens, etc.) are fetched automatically from CCC.
set -euo pipefail

# Repository coordinates — override via env to fork/rename the ACC hub
ACC_OWNER="${ACC_OWNER:-jordanhubbard}"
ACC_REPO="${ACC_REPO:-ACC}"
HERMES_REPO="${HERMES_REPO:-https://github.com/${ACC_OWNER}/hermes-agent.git}"
ACC_REPO_URL="${ACC_REPO_URL:-https://github.com/${ACC_OWNER}/${ACC_REPO}.git}"

CCC=""
TOKEN=""
AGENT=""
AGENT_TOKEN_OVERRIDE=""
# These may be overridden by CLI args, but CCC secrets take precedence if not provided
NVIDIA_KEY=""
TELEGRAM_TOKEN=""

for arg in "$@"; do
  case "$arg" in
    --ccc=*)               CCC="${arg#--ccc=}"               ;;
    --token=*)             TOKEN="${arg#--token=}"             ;;
    --agent=*)             AGENT="${arg#--agent=}"             ;;
    --agent-token=*)       AGENT_TOKEN_OVERRIDE="${arg#--agent-token=}" ;;
    --nvidia-key=*)        NVIDIA_KEY="${arg#--nvidia-key=}"   ;;
    --telegram-token=*)    TELEGRAM_TOKEN="${arg#--telegram-token=}" ;;
  esac
done

if [[ -z "$CCC" || -z "$AGENT" ]]; then
  echo "Usage: bootstrap.sh --ccc=<url> --token=<bootstrap-token> --agent=<name> [--agent-token=<token>]" >&2
  echo "  --token is required unless --agent-token is provided directly." >&2
  exit 1
fi

if [[ -z "$TOKEN" && -z "$AGENT_TOKEN_OVERRIDE" ]]; then
  echo "ERROR: Either --token=<bootstrap-token> or --agent-token=<known-token> is required." >&2
  exit 1
fi

# ── Colors ────────────────────────────────────────────────────────────────
GREEN='\033[0;32m'; BLUE='\033[0;34m'; YELLOW='\033[1;33m'; RED='\033[0;31m'; NC='\033[0m'
info()    { echo -e "${BLUE}→${NC} $1"; }
success() { echo -e "${GREEN}✓${NC} $1"; }
warn()    { echo -e "${YELLOW}⚠${NC} $1"; }
error()   { echo -e "${RED}✗${NC} $1"; exit 1; }

echo ""
echo "🐻 CCC Agent Bootstrap: ${AGENT}"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

# ── 0. Clean slate (safe) ────────────────────────────────────────────────
info "Cleaning up previous install..."

# Back up .ccc/.env before wiping — we restore it if bootstrap fails
ENV_BACKUP=""
if [[ -f "$HOME/.acc/.env" ]]; then
  ENV_BACKUP="$(mktemp /tmp/ccc-env-backup.XXXXXX)"
  cp "$HOME/.acc/.env" "$ENV_BACKUP"
  info "Backed up existing .env to $ENV_BACKUP"
fi

# Unmount any network filesystems under .acc before wiping (rm -rf hangs on live mounts)
for _mnt in "$HOME/.acc/shared" "$HOME/.ccc/shared"; do
  mountpoint -q "$_mnt" 2>/dev/null && { sudo umount -l "$_mnt" 2>/dev/null || true; }
done
rm -rf "$HOME/.acc" 2>/dev/null || true
success "Clean slate ready"

# Trap: restore .env backup if we exit unexpectedly before step 7
_restore_env_on_failure() {
  local code=$?
  if [[ $code -ne 0 && -n "$ENV_BACKUP" && -f "$ENV_BACKUP" ]]; then
    mkdir -p "$HOME/.acc"
    cp "$ENV_BACKUP" "$HOME/.acc/.env"
    chmod 600 "$HOME/.acc/.env"
    echo "⚠ Bootstrap failed (exit $code) — restored previous .env from backup" >&2
    rm -f "$ENV_BACKUP"
  fi
}
trap _restore_env_on_failure EXIT

# ── 1. Dependency check ───────────────────────────────────────────────────
info "Checking dependencies..."
for dep in git curl; do
  command -v "$dep" &>/dev/null || error "$dep is required but not installed."
done
success "Core dependencies present"

# ── JSON helper — ccc-agent preferred, python3 fallback ──────────────────
_ACC_DIR="${HOME}/.acc"
# Prefer ~/.acc; fall back to ~/.ccc for pre-migration nodes
if [[ ! -d "$_ACC_DIR" && -d "${HOME}/.ccc" ]]; then _ACC_DIR="${HOME}/.ccc"; fi
CCC_AGENT="${CCC_AGENT:-$_ACC_DIR/bin/ccc-agent}"
[ ! -x "$CCC_AGENT" ] && CCC_AGENT="$(command -v ccc-agent 2>/dev/null || echo "")"

_json_get() {
  # Usage: echo "$JSON" | _json_get .path [.fallback]
  if [ -x "$CCC_AGENT" ]; then
    "$CCC_AGENT" json get "$@" 2>/dev/null || true
  else
    python3 - "$@" << 'PYEOF' 2>/dev/null
import json, sys
data = json.loads(sys.stdin.read())
for path in sys.argv[1:]:
    v = data
    for key in path.lstrip('.').split('.'):
        v = v.get(key, {}) if isinstance(v, dict) else None
        if not v and v != 0: break
    if v and not isinstance(v, (dict, list)):
        sys.stdout.write(str(v)); sys.exit(0)
PYEOF
  fi
}

# ── 2. Install Hermes Agent ───────────────────────────────────────────────
if command -v hermes &>/dev/null; then
  success "Hermes agent already installed ($(hermes --version 2>/dev/null || echo 'version unknown'))"
  # Ensure slack extras are present even on existing installs
  if ! python3 -c "import slack_bolt" 2>/dev/null; then
    info "slack-bolt not found — injecting slack extras..."
    if command -v pipx &>/dev/null; then
      pipx inject hermes-agent slack-bolt slack-sdk 2>/dev/null && success "Slack extras injected (pipx)" || true
    fi
    if ! python3 -c "import slack_bolt" 2>/dev/null; then
      pip3 install slack-bolt slack-sdk 2>/dev/null && success "Slack extras installed (pip3)" || true
    fi
  fi
else
  info "Installing Hermes agent..."
  if command -v pipx &>/dev/null; then
    pipx install 'hermes-agent[slack]' 2>/dev/null && success "Hermes installed (pipx)" || true
  fi
  if ! command -v hermes &>/dev/null; then
    pip3 install --user 'hermes-agent[slack]' 2>/dev/null && success "Hermes installed (pip3)" || true
  fi
  export PATH="$HOME/.local/bin:$PATH"
  # Fallback: clone from GitHub source if pip failed (package may not be on PyPI)
  if ! command -v hermes &>/dev/null; then
    HERMES_SRC="$HOME/.acc/hermes-src"
    if [[ ! -d "$HERMES_SRC/.git" ]]; then
      info "Cloning hermes-agent from source..."
      git clone --depth=1 ${HERMES_REPO} "$HERMES_SRC" 2>/dev/null || \
        warn "hermes-agent source clone failed — install manually"
    fi
    if [[ -d "$HERMES_SRC" ]]; then
      python3 -m venv "$HOME/.acc/hermes-venv" 2>/dev/null || true
      "$HOME/.acc/hermes-venv/bin/pip" install -e "$HERMES_SRC[slack]" 2>/dev/null || \
        "$HOME/.acc/hermes-venv/bin/pip" install -e "$HERMES_SRC" 2>/dev/null || true
      ln -sf "$HOME/.acc/hermes-venv/bin/hermes" "$HOME/.local/bin/hermes" 2>/dev/null || true
    fi
  fi
  if command -v hermes &>/dev/null; then
    success "Hermes agent installed"
  else
    warn "Hermes agent install failed — install manually from: ${HERMES_REPO%%.git}"
  fi
fi

# ── 3. Clone / update CCC workspace ──────────────────────────────────────
ACC_WORKSPACE="$HOME/.acc/workspace"
info "Setting up ACC workspace at $ACC_WORKSPACE..."
if [[ -d "$ACC_WORKSPACE/.git" ]]; then
  git -C "$ACC_WORKSPACE" pull --ff-only || warn "git pull failed"
else
  git clone "${CCC_REPO:-${ACC_REPO_URL}}" "$ACC_WORKSPACE"
fi
success "CCC workspace ready"

# ── 4. Call bootstrap API ─────────────────────────────────────────────────
BOOTSTRAP_JSON=""
REPO_URL=""
AGENT_TOKEN=""
CCC_URL="${CCC}"  # default to the --ccc URL; may be overridden by API response
DEPLOY_KEY=""

if [[ -n "$AGENT_TOKEN_OVERRIDE" ]]; then
  AGENT_TOKEN="$AGENT_TOKEN_OVERRIDE"
  info "Using pre-provided agent token (skipping bootstrap API call)"
  success "Agent token set from --agent-token"
else
  info "Consuming bootstrap token..."
  BOOTSTRAP_RESP=$(curl -sf "${CCC}/api/bootstrap?token=${TOKEN}" 2>&1) || true
  if echo "$BOOTSTRAP_RESP" | grep -q '"ok":true'; then
    BOOTSTRAP_JSON="$BOOTSTRAP_RESP"
  fi

  if [[ -z "$BOOTSTRAP_JSON" ]]; then
    if [[ -n "$ENV_BACKUP" ]]; then
      PREV_TOKEN=$(grep '^ACC_AGENT_TOKEN=' "$ENV_BACKUP" 2>/dev/null | cut -d= -f2 | tr -d '"' || true)
      if [[ -n "$PREV_TOKEN" ]]; then
        warn "Bootstrap API failed — re-using previous agent token from .env backup"
        warn "To use a fresh token, re-run with a valid --token or --agent-token"
        AGENT_TOKEN="$PREV_TOKEN"
      fi
    fi
    if [[ -z "$AGENT_TOKEN" ]]; then
      error "Bootstrap API call failed or returned invalid response.\nResponse: ${BOOTSTRAP_RESP:-<empty>}\nCheck that CCC is reachable at ${CCC} and the token is valid/unexpired.\nAlternatively, pass --agent-token=<known-token> to skip the API call."
    fi
  else
    REPO_URL=$(echo    "$BOOTSTRAP_JSON" | _json_get .repoUrl)
    AGENT_TOKEN=$(echo "$BOOTSTRAP_JSON" | _json_get .agentToken)
    CCC_URL=$(echo     "$BOOTSTRAP_JSON" | _json_get .cccUrl)
    DEPLOY_KEY=$(echo  "$BOOTSTRAP_JSON" | _json_get .deployKey)

    if [[ -z "$AGENT_TOKEN" ]]; then
      error "Bootstrap response missing agentToken. Response: ${BOOTSTRAP_JSON}"
    fi
    success "Bootstrap token consumed — agent token issued"
  fi
fi

# ── 4b. Extract secrets from bootstrap response ───────────────────────────
info "Extracting secrets from bootstrap response..."
[[ -z "$NVIDIA_KEY"     ]] && NVIDIA_KEY=$(echo    "$BOOTSTRAP_JSON" | _json_get .secrets.NVIDIA_API_KEY     .secrets.nvidia_api_key)
[[ -z "$TOKENHUB_URL"   ]] && TOKENHUB_URL=$(echo  "$BOOTSTRAP_JSON" | _json_get .secrets.TOKENHUB_URL       .secrets.tokenhub_url)
[[ -z "$TOKENHUB_KEY"   ]] && TOKENHUB_KEY=$(echo  "$BOOTSTRAP_JSON" | _json_get .secrets.TOKENHUB_API_KEY .secrets.TOKENHUB_AGENT_KEY .secrets.tokenhub_agent_key)
[[ -z "$TELEGRAM_TOKEN" ]] && TELEGRAM_TOKEN=$(echo "$BOOTSTRAP_JSON" | _json_get .secrets.TELEGRAM_TOKEN .secrets.TELEGRAM_BOT_TOKEN .secrets.telegram_token)

# Fetch per-agent Slack tokens from CCC (stored as <agent>_slack bundle)
SLACK_BOT_TOKEN=""
SLACK_APP_TOKEN=""
if [[ -n "$AGENT" ]]; then
  SLACK_BUNDLE=$(curl -sf "${CCC_URL}/api/secrets/${AGENT}_slack" \
    -H "Authorization: Bearer ${AGENT_TOKEN}" 2>/dev/null || echo "")
  if [[ -n "$SLACK_BUNDLE" ]]; then
    SLACK_BOT_TOKEN=$(echo "$SLACK_BUNDLE" | _json_get .secrets.SLACK_BOT_TOKEN)
    SLACK_APP_TOKEN=$(echo "$SLACK_BUNDLE" | _json_get .secrets.SLACK_APP_TOKEN)
  fi
fi
if [[ -n "$SLACK_BOT_TOKEN" ]]; then
  success "Slack tokens obtained from CCC secrets (${AGENT}_slack)"
else
  warn "No Slack tokens found in CCC for agent '${AGENT}' — Slack channel will be disabled"
fi

if [[ -n "$NVIDIA_KEY" ]]; then
  success "NVIDIA API key obtained from CCC secrets"
else
  warn "No NVIDIA_API_KEY in CCC secrets — configure ANTHROPIC_API_KEY in .env if needed"
fi

# ── 5. Deploy key + SSH config ────────────────────────────────────────────
if [[ -n "$DEPLOY_KEY" ]]; then
  info "Installing deploy key..."
  mkdir -p "$HOME/.ssh"
  printf '%s\n' "$DEPLOY_KEY" > "$HOME/.ssh/ccc-deploy-key"
  chmod 600 "$HOME/.ssh/ccc-deploy-key"
  SSH_CONF="$HOME/.ssh/config"
  if ! grep -q "ccc-deploy-key" "$SSH_CONF" 2>/dev/null; then
    cat >> "$SSH_CONF" <<'SSHEOF'

Host github.com
  IdentityFile ~/.ssh/ccc-deploy-key
  StrictHostKeyChecking no
SSHEOF
    chmod 600 "$SSH_CONF"
  fi
  if [[ -n "$REPO_URL" ]]; then
    git -C "$ACC_WORKSPACE" remote set-url origin "$REPO_URL"
    git -C "$ACC_WORKSPACE" fetch origin || warn "git fetch failed — deploy key may not have read access yet"
  fi
  success "Deploy key installed"
fi

# ── 6. Write ~/.acc/.env ──────────────────────────────────────────────────
info "Writing ~/.acc/.env..."
mkdir -p "$HOME/.acc"
ENV_FILE="$HOME/.acc/.env"
touch "$ENV_FILE"
for key in AGENT_NAME ACC_AGENT_TOKEN CCC_AGENT_TOKEN ACC_URL CCC_URL AGENT_HOST NVIDIA_API_KEY NVIDIA_API_BASE \
           SLACK_BOT_TOKEN SLACK_APP_TOKEN TELEGRAM_TOKEN TELEGRAM_BOT_TOKEN \
           TOKENHUB_API_KEY TOKENHUB_AGENT_KEY; do
  sed -i "/^${key}=/d" "$ENV_FILE" 2>/dev/null || true
done
cat >> "$ENV_FILE" <<ENVEOF
AGENT_NAME=${AGENT}
ACC_AGENT_TOKEN=${AGENT_TOKEN}
ACC_URL=${CCC_URL}
AGENT_HOST=$(hostname)
NVIDIA_API_BASE=https://inference-api.nvidia.com/v1
NVIDIA_API_KEY=${NVIDIA_KEY}
# Claude Code — routes through local header-stripping proxy (strips anthropic-beta
# before forwarding to NVIDIA; the raw NVIDIA endpoint rejects those headers)
ANTHROPIC_BASE_URL=http://localhost:9099
ANTHROPIC_API_KEY=${NVIDIA_KEY}
CLAUDE_CODE_DEFAULT_MODEL=azure/anthropic/claude-sonnet-4-6
# TokenHub — preferred inference router (aggregates local vLLM + NVIDIA NIM)
TOKENHUB_URL=${TOKENHUB_URL:-http://localhost:8090}
TOKENHUB_API_KEY=${TOKENHUB_KEY}
ENVEOF

# Write channel tokens if obtained
[[ -n "$SLACK_BOT_TOKEN"  ]] && echo "SLACK_BOT_TOKEN=${SLACK_BOT_TOKEN}"   >> "$ENV_FILE"
[[ -n "$SLACK_APP_TOKEN"  ]] && echo "SLACK_APP_TOKEN=${SLACK_APP_TOKEN}"   >> "$ENV_FILE"
[[ -n "$TELEGRAM_TOKEN"   ]] && echo "TELEGRAM_TOKEN=${TELEGRAM_TOKEN}"     >> "$ENV_FILE"
chmod 600 "$ENV_FILE"

# Smoke test: verify critical vars are non-empty
_env_val() { grep "^${1}=" "$ENV_FILE" 2>/dev/null | cut -d= -f2- | tr -d '"' || true; }
_SMOKE_OK=true
for _VAR in AGENT_NAME ACC_AGENT_TOKEN ACC_URL; do
  _VAL=$(_env_val "$_VAR")
  if [[ -z "$_VAL" ]]; then
    warn "SMOKE TEST FAIL: ${_VAR} is empty in .env — bootstrap may be incomplete"
    _SMOKE_OK=false
  fi
done
if [[ "$_SMOKE_OK" == true ]]; then
  success "~/.acc/.env written and smoke-tested (all critical vars non-empty)"
else
  warn "~/.acc/.env has empty critical vars — check the file before using this agent"
fi

# ── 6b. Write full secrets bundle to .env ────────────────────────────────
info "Writing secrets bundle to .env..."
if [ -x "$CCC_AGENT" ]; then
  echo "$BOOTSTRAP_JSON" | "$CCC_AGENT" json env-merge .secrets "$ENV_FILE" \
    && success "Secrets bundle written to .env" \
    || warn "Could not write secrets to .env (non-fatal)"
else
  python3 - "$ENV_FILE" << 'PYEOF' 2>/dev/null || warn "Could not write secrets to .env (non-fatal)"
import json, sys, os, re
env_file = sys.argv[1]
data = json.loads(sys.stdin.read())
secrets = data.get('secrets', {})
SKIP = {'CCC_AGENT_TOKEN','CCC_URL','AGENT_NAME','AGENT_HOST'}
existing = open(env_file).read() if os.path.exists(env_file) else ''
lines = existing.splitlines()
count = 0
for k, v in secrets.items():
    if not isinstance(v, str): continue
    if k in SKIP: continue
    if not re.match(r'^[A-Za-z_][A-Za-z0-9_]*$', k): continue
    lines = [l for l in lines if not l.startswith(k + '=')]
    lines.append(f'{k}={v}')
    count += 1
with open(env_file, 'w') as f:
    f.write('\n'.join(lines) + '\n')
os.chmod(env_file, 0o600)
print(f'Wrote {count} secrets', file=sys.stderr)
PYEOF
  success "Secrets bundle written to .env"
fi

# ── 7. Mount AccFS (Samba/SMB shared filesystem) ──────────────────────────
info "Setting up AccFS mount at $HOME/.acc/shared..."
bash "$ACC_WORKSPACE/deploy/run-migrations.sh" --only=0019 2>&1 \
  | grep -E "✓|⚠|ERROR|complete|mounted" | while read -r l; do info "$l"; done || true

# ── 8. vLLM (GPU nodes only) ──────────────────────────────────────────────
GPU_COUNT=0
if command -v nvidia-smi &>/dev/null; then
  GPU_COUNT=$(nvidia-smi --query-gpu=name --format=csv,noheader 2>/dev/null | wc -l || echo 0)
fi

if [[ ${GPU_COUNT:-0} -gt 0 ]]; then
  info "GPU detected — setting up vLLM model serving..."
  VLLM_MODEL="${VLLM_MODEL:-google/gemma-4-31B-it}"
  VLLM_SERVED_NAME="${VLLM_SERVED_NAME:-gemma}"
  VLLM_PORT="${VLLM_PORT:-8000}"

  if [[ -d "$HOME/models/$(basename $VLLM_MODEL)" ]]; then
    VLLM_MODEL_PATH="$HOME/models/$(basename $VLLM_MODEL)"
    success "Model found locally: $VLLM_MODEL_PATH"
  else
    VLLM_MODEL_PATH="$VLLM_MODEL"
    info "Model not cached — vLLM will download from HuggingFace on first start"
  fi

  if ! command -v vllm &>/dev/null && ! python3 -c "import vllm" 2>/dev/null; then
    info "Installing vLLM..."
    pip3 install vllm 2>/dev/null && success "vLLM installed" || warn "vLLM install failed — install manually: pip3 install vllm"
  fi

  VLLM_EXTRA_ARGS="${VLLM_EXTRA_ARGS:-}"
  if [[ ${GPU_COUNT} -gt 1 && -z "$VLLM_EXTRA_ARGS" ]]; then
    VLLM_EXTRA_ARGS="--tensor-parallel-size ${GPU_COUNT}"
    info "Multi-GPU detected: adding $VLLM_EXTRA_ARGS"
  fi

  for key in VLLM_ENABLED VLLM_MODEL VLLM_SERVED_NAME VLLM_PORT VLLM_MODEL_PATH VLLM_EXTRA_ARGS; do
    sed -i "/^${key}=/d" "$ENV_FILE" 2>/dev/null || true
  done
  cat >> "$ENV_FILE" <<VLLMENV
VLLM_ENABLED=true
VLLM_MODEL=${VLLM_MODEL}
VLLM_SERVED_NAME=${VLLM_SERVED_NAME}
VLLM_PORT=${VLLM_PORT}
VLLM_MODEL_PATH=${VLLM_MODEL_PATH}
VLLM_EXTRA_ARGS=${VLLM_EXTRA_ARGS}
VLLMENV

  if python3 -c "import vllm" 2>/dev/null; then
    _vllm_running() { curl -sf "http://127.0.0.1:${VLLM_PORT}/v1/models" > /dev/null 2>&1; }

    if _vllm_running; then
      success "vLLM already running on port ${VLLM_PORT}"
    elif command -v systemctl &>/dev/null && [[ "$(uname)" == "Linux" ]]; then
      VLLM_SVC="/etc/systemd/system/vllm-${AGENT}.service"
      cat > /tmp/vllm.service <<VLLMSVC
[Unit]
Description=vLLM model server for ${AGENT}
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=$(whoami)
Environment="HOME=${HOME}"
ExecStart=/usr/bin/python3 -m vllm.entrypoints.openai.api_server --model ${VLLM_MODEL_PATH} --served-model-name ${VLLM_SERVED_NAME} --port ${VLLM_PORT} ${VLLM_EXTRA_ARGS}
Restart=on-failure
RestartSec=15

[Install]
WantedBy=multi-user.target
VLLMSVC
      sudo mv /tmp/vllm.service "$VLLM_SVC"
      sudo systemctl daemon-reload
      sudo systemctl enable "vllm-${AGENT}" 2>/dev/null || true
      sudo systemctl start "vllm-${AGENT}" 2>/dev/null || true
      success "vLLM systemd service installed and started"
    else
      tmux kill-session -t vllm 2>/dev/null || true
      tmux new-session -d -s vllm "python3 -m vllm.entrypoints.openai.api_server --model ${VLLM_MODEL_PATH} --served-model-name ${VLLM_SERVED_NAME} --port ${VLLM_PORT} ${VLLM_EXTRA_ARGS}"
      success "vLLM started (tmux session 'vllm')"
    fi
  fi
else
  info "No GPU detected — skipping vLLM setup"
  for key in VLLM_ENABLED VLLM_MODEL VLLM_SERVED_NAME VLLM_PORT VLLM_MODEL_PATH VLLM_EXTRA_ARGS; do
    sed -i "/^${key}=/d" "$ENV_FILE" 2>/dev/null || true
  done
  echo "VLLM_ENABLED=false" >> "$ENV_FILE"
fi

# ── 9. Install Hermes skills and configure gateway ───────────────────────
if command -v hermes &>/dev/null; then
  info "Configuring Hermes agent..."
  mkdir -p "$HOME/.hermes/skills"

  # CCC fleet skill
  if [[ -d "$ACC_WORKSPACE/skills/acc-node" ]]; then
    cp -r "$ACC_WORKSPACE/skills/acc-node/" "$HOME/.hermes/skills/acc-node/"
    success "CCC-node skill installed into Hermes"
  fi

  # agent-skills engineering workflows — vendored in skills/agent-skills/
  if [[ -d "$ACC_WORKSPACE/skills/agent-skills" ]]; then
    _count=0
    for _skill_dir in "$ACC_WORKSPACE/skills/agent-skills"/*/; do
      [[ -d "$_skill_dir" ]] || continue
      _skill_name="$(basename "$_skill_dir")"
      cp -r "$_skill_dir" "$HOME/.hermes/skills/${_skill_name}/"
      _count=$((_count + 1))
    done
    success "agent-skills: ${_count} engineering workflow skills installed into Hermes"
  else
    warn "skills/agent-skills not found in workspace — agent-skills missing"
  fi

  # superpowers orchestration skills — vendored in skills/superpowers/
  if [[ -d "$ACC_WORKSPACE/skills/superpowers" ]]; then
    _count=0
    for _skill_file in "$ACC_WORKSPACE/skills/superpowers"/*.md; do
      [[ -f "$_skill_file" ]] || continue
      _skill_name="$(basename "$_skill_file" .md)"
      [[ "$_skill_name" == "using-superpowers" ]] && continue
      cp "$_skill_file" "$HOME/.hermes/skills/${_skill_name}.md"
      _count=$((_count + 1))
    done
    for _skill_dir in "$ACC_WORKSPACE/skills/superpowers"/*/; do
      [[ -d "$_skill_dir" ]] || continue
      _skill_name="$(basename "$_skill_dir")"
      cp -r "$_skill_dir" "$HOME/.hermes/skills/${_skill_name}/"
      _count=$((_count + 1))
    done
    success "superpowers: ${_count} orchestration skills installed into Hermes"
  else
    warn "skills/superpowers not found in workspace — superpowers missing"
  fi

  # Write ~/.hermes/config.yaml with CCC env vars and channel tokens.
  # Hermes reads this on startup — tokens are NOT baked into the supervisor conf.
  HERMES_CONFIG="$HOME/.hermes/config.yaml"
  info "Writing ~/.hermes/config.yaml..."
  cat > "$HERMES_CONFIG" <<HCEOF
env:
  ACC_URL: "${CCC_URL}"
  ACC_AGENT_TOKEN: "${AGENT_TOKEN}"
  AGENT_NAME: "${AGENT}"
HCEOF
  [[ -n "${SLACK_BOT_TOKEN:-}" ]] && echo "  SLACK_BOT_TOKEN: \"${SLACK_BOT_TOKEN}\"" >> "$HERMES_CONFIG"
  [[ -n "${SLACK_APP_TOKEN:-}" ]] && echo "  SLACK_APP_TOKEN: \"${SLACK_APP_TOKEN}\"" >> "$HERMES_CONFIG"

  # Write model + providers sections so hermes sends the API key on every inference call.
  # Without api_key in the provider block hermes falls back to "no-key-required".
  if [[ -n "${NVIDIA_KEY:-}" ]]; then
    cat >> "$HERMES_CONFIG" <<HPEOF

model:
  default: azure/anthropic/claude-sonnet-4-6
  provider: nvidia
  base_url: https://inference-api.nvidia.com/v1/
  api_key: "${NVIDIA_KEY}"

providers:
  nvidia:
    api: https://inference-api.nvidia.com/v1/
    name: nvidia
    api_key: "${NVIDIA_KEY}"
    transport: chat_completions

fallback_providers: []

toolsets:
- hermes-cli

agent:
  max_turns: 90
  gateway_timeout: 0
  restart_drain_timeout: 60
HPEOF
    success "hermes providers.nvidia written with API key"
  fi

  chmod 600 "$HERMES_CONFIG"
  success "~/.hermes/config.yaml written"

  # Register hermes-gateway with supervisord.
  # Try the conf.d drop-in directory first (preferred, idempotent per-file),
  # then fall back to appending to the monolithic conf.
  HERMES_BIN="$(command -v hermes || echo "$HOME/.local/bin/hermes")"
  HERMES_LOG="$HOME/.acc/logs/hermes-gateway.log"
  mkdir -p "$HOME/.acc/logs"

  _hermes_supervisor_block() {
    cat <<HCONF
[program:hermes-gateway]
command=${HERMES_BIN} gateway
user=$(whoami)
environment=HOME="${HOME}"
directory=${HOME}
stdout_logfile=${HERMES_LOG}
stdout_logfile_maxbytes=5MB
stdout_logfile_backups=2
redirect_stderr=true
autostart=true
autorestart=true
startsecs=10
startretries=5
priority=10
HCONF
  }

  _hermes_registered=false
  for _confd in "/etc/supervisor/conf.d" "/etc/supervisord.d"; do
    if [[ -d "$_confd" ]]; then
      _hconf="$_confd/hermes-gateway.conf"
      if [[ ! -f "$_hconf" ]]; then
        _hermes_supervisor_block | sudo tee "$_hconf" > /dev/null
        sudo supervisorctl reread 2>/dev/null && sudo supervisorctl update 2>/dev/null || true
        success "hermes-gateway supervisor conf: $_hconf"
      else
        success "hermes-gateway supervisor conf already exists: $_hconf"
      fi
      _hermes_registered=true
      break
    fi
  done

  if [[ "$_hermes_registered" == false ]]; then
    for _mainconf in "/etc/supervisord.conf" "/etc/supervisor/supervisord.conf"; do
      if [[ -f "$_mainconf" ]]; then
        if ! grep -q "\[program:hermes-gateway\]" "$_mainconf" 2>/dev/null; then
          { echo ""; _hermes_supervisor_block; } | sudo tee -a "$_mainconf" > /dev/null
          sudo supervisorctl reread 2>/dev/null && sudo supervisorctl update 2>/dev/null || true
          success "hermes-gateway appended to $_mainconf"
        else
          success "hermes-gateway already in $_mainconf"
        fi
        _hermes_registered=true
        break
      fi
    done
  fi

  if [[ "$_hermes_registered" == false ]]; then
    warn "No supervisord config found — start hermes gateway manually: hermes gateway"
    warn "  Or add it to your process manager. Log: ${HERMES_LOG}"
  fi
fi

# ── 9b. Install acc-bus-listener (systemd/launchd/supervisord) ───────────────
# Subscribes to AgentBus SSE stream and triggers agent-pull on acc.update.
# Uses install-bus-listener.sh which picks the right process manager.
info "Installing acc-bus-listener..."
INSTALL_BUS="${CCC_WORKSPACE}/deploy/install-bus-listener.sh"
if [[ -x "$INSTALL_BUS" ]]; then
  bash "$INSTALL_BUS" 2>&1 | grep -E "✓|⚠|ERROR|Done|active" | while read -r l; do info "$l"; done || true
  success "acc-bus-listener installed"
else
  warn "install-bus-listener.sh not found — AgentBus-triggered sync disabled"
fi

# ── 9c. Install acc-queue-worker (systemd/launchd/supervisord) ───────────────
# Polls /api/queue and autonomously executes pending items via claude/hermes.
info "Installing acc-queue-worker..."
INSTALL_QW="${CCC_WORKSPACE}/deploy/install-queue-worker.sh"
if [[ -x "$INSTALL_QW" ]]; then
  bash "$INSTALL_QW" 2>&1 | grep -E "✓|⚠|ERROR|Done|active" | while read -r l; do info "$l"; done || true
  success "acc-queue-worker installed"
else
  warn "install-queue-worker.sh not found — queue processing disabled"
fi


# ── 10. Hardware fingerprint + heartbeat ──────────────────────────────────
info "Collecting hardware fingerprint..."

GPU_MODEL=""
GPU_VRAM_GB=0
if command -v nvidia-smi &>/dev/null; then
  GPU_MODEL=$(nvidia-smi --query-gpu=name --format=csv,noheader 2>/dev/null | head -1 | tr -d '\n' || echo "")
  GPU_VRAM_MB=$(nvidia-smi --query-gpu=memory.total --format=csv,noheader,nounits 2>/dev/null | \
    awk '{s+=$1} END {print int(s)}' || echo 0)
  GPU_VRAM_GB=$(( GPU_VRAM_MB / 1024 ))
fi

CPU_CORES=$(nproc 2>/dev/null || grep -c ^processor /proc/cpuinfo 2>/dev/null || echo 0)
CPU_MODEL=$(grep 'model name' /proc/cpuinfo 2>/dev/null | head -1 | cut -d: -f2 | sed 's/^ *//' || echo "unknown")
CPU_ARCH=$(uname -m 2>/dev/null || echo "unknown")

RAM_GB=0
if [[ -r /proc/meminfo ]]; then
  RAM_KB=$(grep MemTotal /proc/meminfo | awk '{print $2}')
  RAM_GB=$(( RAM_KB / 1024 / 1024 ))
fi

DISK_FREE_GB=$(df -BG "$HOME" 2>/dev/null | tail -1 | awk '{print $4}' | tr -d 'G' || echo 0)

HW_JSON=$(cat <<HWEOF
{
  "gpu_count": ${GPU_COUNT},
  "gpu_model": "${GPU_MODEL}",
  "gpu_vram_gb": ${GPU_VRAM_GB},
  "cpu_cores": ${CPU_CORES},
  "cpu_model": "${CPU_MODEL}",
  "cpu_arch": "${CPU_ARCH}",
  "ram_gb": ${RAM_GB},
  "disk_free_gb": ${DISK_FREE_GB}
}
HWEOF
)

info "Hardware: ${GPU_COUNT}x ${GPU_MODEL:-none} (${GPU_VRAM_GB}GB VRAM), ${CPU_CORES}x CPU, ${RAM_GB}GB RAM"

info "Posting heartbeat + hardware fingerprint to CCC..."
curl -s -X POST "${CCC_URL}/api/heartbeat/${AGENT}" \
  -H "Authorization: Bearer ${AGENT_TOKEN}" \
  -H "Content-Type: application/json" \
  -d "{
    \"agent\":\"${AGENT}\",
    \"host\":\"$(hostname)\",
    \"status\":\"online\",
    \"version\":\"bootstrap\",
    \"hardware\":${HW_JSON}
  }" > /dev/null || warn "Heartbeat post failed (non-fatal)"

curl -s -X PATCH "${CCC_URL}/api/agents/${AGENT}" \
  -H "Authorization: Bearer ${AGENT_TOKEN}" \
  -H "Content-Type: application/json" \
  -d "{
    \"capabilities\": {
      \"gpu\": $([ ${GPU_COUNT} -gt 0 ] && echo true || echo false),
      \"gpu_model\": \"${GPU_MODEL}\",
      \"gpu_count\": ${GPU_COUNT},
      \"gpu_vram_gb\": ${GPU_VRAM_GB},
      \"cpu_cores\": ${CPU_CORES},
      \"cpu_model\": \"${CPU_MODEL}\",
      \"cpu_arch\": \"${CPU_ARCH}\",
      \"ram_gb\": ${RAM_GB}
    }
  }" > /dev/null || warn "Capabilities PATCH failed (non-fatal — dashboard may show stale hw info)"

success "Heartbeat + hardware fingerprint posted"

# ── Write onboarding signature ────────────────────────────────────────────
_AGENT_JSON="$HOME/.acc/agent.json"
if [ ! -f "$_AGENT_JSON" ]; then
  _CCC_VERSION=$(cd "${CCC_WORKSPACE:-$HOME/.acc/workspace}" && git rev-parse --short HEAD 2>/dev/null || echo "unknown")
  if [ -x "$CCC_AGENT" ]; then
    "$CCC_AGENT" agent init "$_AGENT_JSON" \
      --name="${AGENT:-unknown}" \
      --host="$(hostname)" \
      --version="$_CCC_VERSION" \
      --by="bootstrap.sh" \
      && success "Onboarding signature written to $_AGENT_JSON" \
      || warn "Failed to write agent.json (non-fatal)"
  elif command -v python3 >/dev/null 2>&1; then
    python3 - "$_AGENT_JSON" "${AGENT:-unknown}" "$(hostname)" "$_CCC_VERSION" << 'PYEOF'
import json, sys, os
from datetime import datetime, timezone
path, name, host, ver = sys.argv[1:5]
now = datetime.now(timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ')
os.makedirs(os.path.dirname(path), exist_ok=True)
with open(path, 'w') as f:
    json.dump({'schema_version':1,'agent_name':name,'host':host,
               'onboarded_at':now,'onboarded_by':'bootstrap.sh',
               'ccc_version':ver,'last_upgraded_at':now,'last_upgraded_version':ver}, f, indent=2)
    f.write('\n')
os.chmod(path, 0o600)
PYEOF
    success "Onboarding signature written to $_AGENT_JSON"
  else
    warn "Neither ccc-agent nor python3 found — skipping agent.json write"
  fi
else
  info "agent.json already exists at $_AGENT_JSON"
fi

# ── 11. Done ──────────────────────────────────────────────────────────────
trap - EXIT
[[ -n "${ENV_BACKUP:-}" && -f "${ENV_BACKUP:-/dev/null}" ]] && rm -f "$ENV_BACKUP"

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo -e "${GREEN}✅ Bootstrap complete!${NC} ${AGENT^} is alive."
echo ""
echo "  CCC workspace:  ${CCC_WORKSPACE}"
echo "  ACC env:        ${HOME}/.acc/.env"
echo ""
echo "  Next: hermes gateway   (starts the agent runtime)"
echo ""
