# Description: Remove legacy RCC/consul infrastructure and clean up stale .env vars
#
# Context: CCC was previously named RCC, and early deployments included Consul for
#   service discovery. Neither is in use anymore. This migration:
#     1. Stops and disables the Consul service if it is running
#     2. Removes stale env vars (RCC_*, CONSUL_*, legacy path vars, MILVUS_ADDRESS)
#     3. Fixes consul DNS URLs in TOKENHUB_URL and QDRANT_URL to localhost
#     4. Deduplicates memory-git-commit cron entries
#     5. Removes incubator-digest.mjs cron entry (obsolete)
# Condition: all platforms

ENV_FILE="$HOME/.ccc/.env"

# ── 1. Stop consul if it's running ────────────────────────────────────────
if command -v systemctl &>/dev/null 2>&1; then
  if systemctl --user is-active consul.service &>/dev/null 2>&1; then
    m_info "Stopping consul.service..."
    systemctl --user stop consul.service 2>/dev/null || true
    systemctl --user disable consul.service 2>/dev/null || true
    m_success "consul.service stopped and disabled"
  elif sudo systemctl is-active consul.service &>/dev/null 2>&1; then
    m_info "Stopping system consul.service..."
    sudo systemctl stop consul.service 2>/dev/null || true
    sudo systemctl disable consul.service 2>/dev/null || true
    m_success "System consul.service stopped and disabled"
  else
    m_info "Consul service not running — skipping"
  fi
elif command -v launchctl &>/dev/null 2>&1; then
  CONSUL_PLIST="$HOME/Library/LaunchAgents/com.ccc.consul.plist"
  if [ -f "$CONSUL_PLIST" ]; then
    m_info "Unloading consul LaunchAgent..."
    launchctl unload "$CONSUL_PLIST" 2>/dev/null || true
    m_success "Consul LaunchAgent unloaded"
  else
    m_info "Consul LaunchAgent not found — skipping"
  fi
fi

# ── 2. Remove stale .env vars ────────────────────────────────────────────
if [ ! -f "$ENV_FILE" ]; then
  m_warn ".env not found at $ENV_FILE — skipping env cleanup"
else
  # macOS vs Linux sed -i
  if [ "$(uname)" = "Darwin" ]; then
    _sed_i() { sed -i '' "$@"; }
  else
    _sed_i() { sed -i "$@"; }
  fi

  STALE_KEYS="RCC_URL RCC_API_INTERNAL RCC_PORT RCC_AUTH_TOKENS RCC_ADMIN_TOKEN RCC_AGENT_TOKEN \
    CONSUL_SERVER_ADDR CONSUL_HTTP_ADDR \
    QUEUE_PATH AGENTS_PATH REPOS_PATH BRAIN_STATE_PATH LESSONS_DIR SERENDIPITY_DROP_DIR \
    MILVUS_ADDRESS"

  _removed=0
  for _key in $STALE_KEYS; do
    if grep -q "^${_key}=" "$ENV_FILE" 2>/dev/null; then
      _sed_i "/^${_key}=/d" "$ENV_FILE"
      _removed=$((_removed + 1))
    fi
  done

  if [ "$_removed" -gt 0 ]; then
    m_success "Removed $_removed stale env vars from .env"
  else
    m_info "No stale env vars found in .env"
  fi

  # ── 3. Fix consul DNS URLs in .env ──────────────────────────────────────
  _fixed=0
  for _key in TOKENHUB_URL QDRANT_URL; do
    _val=$(grep "^${_key}=" "$ENV_FILE" 2>/dev/null | cut -d= -f2- || true)
    if echo "$_val" | grep -q '\.service\.consul'; then
      # Replace the consul hostname but preserve the port
      _port=$(echo "$_val" | grep -o ':[0-9]*$' | tr -d ':')
      _new_val="http://localhost:${_port}"
      _sed_i "s|^${_key}=.*|${_key}=${_new_val}|" "$ENV_FILE"
      m_success "Fixed ${_key}: consul DNS → ${_new_val}"
      _fixed=$((_fixed + 1))
    fi
  done
  [ "$_fixed" -eq 0 ] && m_info "No consul DNS URLs to fix in .env"
fi

# ── 4. Deduplicate memory-git-commit cron entries ─────────────────────────
if command -v crontab &>/dev/null 2>&1; then
  _cron=$(crontab -l 2>/dev/null || true)
  if [ -n "$_cron" ]; then
    # Count memory-git-commit entries
    _count=$(echo "$_cron" | grep -c 'memory-git-commit' 2>/dev/null || true)
    if [ "$_count" -gt 1 ]; then
      m_info "Found $_count memory-git-commit cron entries — deduplicating..."
      # Keep only the first occurrence, remove subsequent duplicates
      _new_cron=$(echo "$_cron" | awk '
        /memory-git-commit/ { if (!seen_mgc++) { print; next } next }
        { print }
      ')
      echo "$_new_cron" | crontab -
      m_success "Deduplicated memory-git-commit cron (kept 1 of $_count)"
    else
      m_info "memory-git-commit cron entries OK ($_count found)"
    fi

    # ── 5. Remove incubator-digest.mjs cron entry ─────────────────────────
    if echo "$_cron" | grep -q 'incubator-digest'; then
      m_info "Removing obsolete incubator-digest.mjs cron entry..."
      crontab -l 2>/dev/null | grep -v 'incubator-digest' | crontab -
      m_success "incubator-digest cron removed"
    else
      m_info "incubator-digest cron not present — skipping"
    fi
  else
    m_info "Crontab is empty — no cron cleanup needed"
  fi
else
  m_info "crontab not available — skipping cron cleanup"
fi

m_success "Migration 0014 complete — legacy consul/RCC infrastructure removed"
