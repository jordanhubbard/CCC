#!/usr/bin/env bash
# Rotate Rocky hub ACC bearer tokens without printing token material.
#
# Scope:
#   - Replaces Rocky's ACC_AUTH_TOKENS with new ACC-* tokens.
#   - Updates per-agent ~/.acc/.env ACC_AGENT_TOKEN values.
#   - Updates Rocky's agent registry token fields to the same new values.
#   - Updates Rocky auth.db user token hashes for the known hub users.
#   - Verifies all old static hub tokens are rejected.

set -euo pipefail

TS="$(date -u +%Y%m%dT%H%M%SZ)"

ROCKY_SSH="${ROCKY_SSH:-jkh@100.89.199.14}"
BORIS_SSH="${BORIS_SSH:-jkh@100.81.243.3}"
NATASHA_SSH="${NATASHA_SSH:-jkh@100.87.229.125}"
BULLWINKLE_SSH="${BULLWINKLE_SSH:-jkh@100.87.68.11}"

SSH_OPTS=(-o BatchMode=yes -o ConnectTimeout=10)

need() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "missing required command: $1" >&2
    exit 2
  }
}

need awk
need curl
need openssl
need ssh

new_token() {
  printf 'ACC-%s-%s' "$1" "$(openssl rand -hex 32)"
}

hash12() {
  if command -v shasum >/dev/null 2>&1; then
    printf '%s' "$1" | shasum -a 256 | awk '{print substr($1,1,12)}'
  else
    printf '%s' "$1" | sha256sum | awk '{print substr($1,1,12)}'
  fi
}

set_key_file() {
  local file="$1" key="$2" val="$3" dir tmp
  dir="$(dirname "$file")"
  mkdir -p "$dir"
  if [ ! -f "$file" ]; then
    : > "$file"
    chmod 600 "$file"
  fi
  [ -f "$file.rotbak.$TS" ] || cp "$file" "$file.rotbak.$TS"
  tmp="$file.tmp.$$"
  awk -v key="$key" -v val="$val" '
    BEGIN { found = 0 }
    index($0, key "=") == 1 { print key "=" val; found = 1; next }
    { print }
    END { if (!found) print key "=" val }
  ' "$file" > "$tmp"
  chmod 600 "$tmp"
  mv "$tmp" "$file"
}

set_key_if_present() {
  local file="$1" key="$2" val="$3"
  [ -f "$file" ] || return 0
  if grep -q "^${key}=" "$file"; then
    set_key_file "$file" "$key" "$val"
  fi
}

get_old_auth_tokens() {
  ssh "${SSH_OPTS[@]}" "$ROCKY_SSH" \
    'awk -F= '"'"'/^ACC_AUTH_TOKENS=/{print substr($0,index($0,"=")+1); exit}'"'"' "$HOME/.acc/.env"'
}

update_remote_agent_env() {
  local target="$1" token="$2" label="$3"
  ssh "${SSH_OPTS[@]}" "$target" \
    TS="$TS" TOKEN="$token" bash -s <<'REMOTE'
set -euo pipefail

set_key_file() {
  file="$1"; key="$2"; val="$3"; dir="$(dirname "$file")"
  mkdir -p "$dir"
  if [ ! -f "$file" ]; then
    : > "$file"
    chmod 600 "$file"
  fi
  [ -f "$file.rotbak.$TS" ] || cp "$file" "$file.rotbak.$TS"
  tmp="$file.tmp.$$"
  awk -v key="$key" -v val="$val" '
    BEGIN { found = 0 }
    index($0, key "=") == 1 { print key "=" val; found = 1; next }
    { print }
    END { if (!found) print key "=" val }
  ' "$file" > "$tmp"
  chmod 600 "$tmp"
  mv "$tmp" "$file"
}

set_key_if_present() {
  file="$1"; key="$2"; val="$3"
  [ -f "$file" ] || return 0
  if grep -q "^${key}=" "$file"; then
    set_key_file "$file" "$key" "$val"
  fi
}

for envfile in "$HOME/.acc/.env" "$HOME/.ccc/.env"; do
  [ -f "$envfile" ] || continue
  set_key_file "$envfile" ACC_AGENT_TOKEN "$TOKEN"
  set_key_if_present "$envfile" CCC_AGENT_TOKEN "$TOKEN"
  set_key_if_present "$envfile" ACC_TOKEN "$TOKEN"
done
REMOTE
  printf 'updated %-10s env token fp=%s\n' "$label" "$(hash12 "$token")"
}

update_rocky_hub_state() {
  ssh "${SSH_OPTS[@]}" "$ROCKY_SSH" \
    TS="$TS" \
    FLEETCTL_TOKEN="$FLEETCTL_TOKEN" \
    ROCKY_TOKEN="$ROCKY_TOKEN" \
    BORIS_TOKEN="$BORIS_TOKEN" \
    NATASHA_TOKEN="$NATASHA_TOKEN" \
    BULLWINKLE_TOKEN="$BULLWINKLE_TOKEN" \
    AUTH_TOKENS="$AUTH_TOKENS" \
    bash -s <<'REMOTE'
set -euo pipefail

set_key_file() {
  file="$1"; key="$2"; val="$3"; dir="$(dirname "$file")"
  mkdir -p "$dir"
  if [ ! -f "$file" ]; then
    : > "$file"
    chmod 600 "$file"
  fi
  [ -f "$file.rotbak.$TS" ] || cp "$file" "$file.rotbak.$TS"
  tmp="$file.tmp.$$"
  awk -v key="$key" -v val="$val" '
    BEGIN { found = 0 }
    index($0, key "=") == 1 { print key "=" val; found = 1; next }
    { print }
    END { if (!found) print key "=" val }
  ' "$file" > "$tmp"
  chmod 600 "$tmp"
  mv "$tmp" "$file"
}

set_key_if_present() {
  file="$1"; key="$2"; val="$3"
  [ -f "$file" ] || return 0
  if grep -q "^${key}=" "$file"; then
    set_key_file "$file" "$key" "$val"
  fi
}

hash_full() {
  if command -v sha256sum >/dev/null 2>&1; then
    printf '%s' "$1" | sha256sum | awk '{print $1}'
  else
    printf '%s' "$1" | shasum -a 256 | awk '{print $1}'
  fi
}

ENV_FILE="$HOME/.acc/.env"
set_key_file "$ENV_FILE" ACC_AGENT_TOKEN "$ROCKY_TOKEN"
set_key_if_present "$ENV_FILE" CCC_AGENT_TOKEN "$ROCKY_TOKEN"
set_key_if_present "$ENV_FILE" ACC_TOKEN "$ROCKY_TOKEN"
set_key_file "$ENV_FILE" ACC_AUTH_TOKENS "$AUTH_TOKENS"
set_key_if_present "$ENV_FILE" CCC_AUTH_TOKENS "$AUTH_TOKENS"

CFG="$HOME/.acc/acc.json"
if [ -f "$CFG" ] && command -v jq >/dev/null 2>&1; then
  cp "$CFG" "$CFG.rotbak.$TS"
  tmp="$CFG.tmp.$$"
  jq --arg auth "$AUTH_TOKENS" '.auth_tokens = ($auth | split(","))' "$CFG" > "$tmp"
  chmod 600 "$tmp"
  mv "$tmp" "$CFG"
elif [ -f "$CFG" ]; then
  echo "warning: jq unavailable on Rocky; ACC_AUTH_TOKENS env was rotated, acc.json auth_tokens not rewritten" >&2
fi

. "$ENV_FILE" 2>/dev/null || true
FLEET_DB="${ACC_DATA_DIR:+$ACC_DATA_DIR/acc.db}"
[ -n "$FLEET_DB" ] || FLEET_DB="${ACC_DB_PATH:-$HOME/.acc/data/acc.db}"
AUTH_DB="${AUTH_DB_PATH:-$HOME/.acc/auth.db}"

if command -v sqlite3 >/dev/null 2>&1 && [ -f "$FLEET_DB" ]; then
  cp "$FLEET_DB" "$FLEET_DB.rotbak.$TS"
  sqlite3 "$FLEET_DB" "UPDATE agents SET data=json_set(data, '$.token', '$ROCKY_TOKEN') WHERE name='rocky';"
  sqlite3 "$FLEET_DB" "UPDATE agents SET data=json_set(data, '$.token', '$BORIS_TOKEN') WHERE name='boris';"
  sqlite3 "$FLEET_DB" "UPDATE agents SET data=json_set(data, '$.token', '$NATASHA_TOKEN') WHERE name='natasha';"
  sqlite3 "$FLEET_DB" "UPDATE agents SET data=json_set(data, '$.token', '$BULLWINKLE_TOKEN') WHERE name='bullwinkle';"
else
  echo "warning: sqlite3 or fleet DB unavailable; agent registry tokens not rewritten" >&2
fi

if command -v sqlite3 >/dev/null 2>&1 && [ -f "$AUTH_DB" ]; then
  cp "$AUTH_DB" "$AUTH_DB.rotbak.$TS"
  sqlite3 "$AUTH_DB" "UPDATE users SET token_hash='$(hash_full "$FLEETCTL_TOKEN")' WHERE username='jkh';"
  sqlite3 "$AUTH_DB" "UPDATE users SET token_hash='$(hash_full "$ROCKY_TOKEN")' WHERE username='rocky';"
  sqlite3 "$AUTH_DB" "UPDATE users SET token_hash='$(hash_full "$BORIS_TOKEN")' WHERE username='boris';"
  sqlite3 "$AUTH_DB" "UPDATE users SET token_hash='$(hash_full "$BULLWINKLE_TOKEN")' WHERE username='bullwinkle';"
  sqlite3 "$AUTH_DB" "UPDATE users SET token_hash='$(hash_full "$NATASHA_TOKEN")' WHERE username='sparky';"
else
  echo "warning: sqlite3 or auth DB unavailable; auth.db user hashes not rewritten" >&2
fi
REMOTE
  echo "updated rocky hub env, config, registry, and auth database"
}

restart_rocky_hub() {
  ssh "${SSH_OPTS[@]}" "$ROCKY_SSH" \
    ROCKY_TOKEN="$ROCKY_TOKEN" bash -s <<'REMOTE'
set -euo pipefail

sudo -n systemctl restart acc-server.service
for _ in 1 2 3 4 5 6 7 8 9 10; do
  systemctl is-active acc-server.service >/dev/null 2>&1 && break
  sleep 1
done
systemctl is-active acc-server.service >/dev/null

code=000
for _ in 1 2 3 4 5 6 7 8 9 10; do
  code="$(curl -sS -o /dev/null -w '%{http_code}' \
    -H "Authorization: Bearer $ROCKY_TOKEN" \
    http://127.0.0.1:8789/api/secrets || true)"
  [ "$code" = 200 ] && break
  sleep 1
done
[ "$code" = 200 ] || {
  echo "rocky hub did not accept new token; http=$code" >&2
  exit 1
}
REMOTE
  echo "restarted rocky hub and verified new rocky token"
}

verify_new_tokens() {
  local url="$1" name token code
  for name in fleetctl rocky boris natasha bullwinkle; do
    case "$name" in
      fleetctl) token="$FLEETCTL_TOKEN" ;;
      rocky) token="$ROCKY_TOKEN" ;;
      boris) token="$BORIS_TOKEN" ;;
      natasha) token="$NATASHA_TOKEN" ;;
      bullwinkle) token="$BULLWINKLE_TOKEN" ;;
    esac
    code="$(curl -sS -o /dev/null -w '%{http_code}' \
      -H "Authorization: Bearer $token" \
      "$url/api/secrets" || true)"
    printf 'verify new %-10s http=%s\n' "$name" "$code"
    [ "$code" = 200 ] || exit 1
  done
}

verify_old_tokens_rejected() {
  local url="$1" old code old_ok
  old_ok=0
  IFS=, read -r -a old_tokens <<< "$OLD_AUTH_TOKENS"
  for old in "${old_tokens[@]}"; do
    [ -n "$old" ] || continue
    code="$(curl -sS -o /dev/null -w '%{http_code}' \
      -H "Authorization: Bearer $old" \
      "$url/api/secrets" || true)"
    if [ "$code" != 401 ]; then
      old_ok=$((old_ok + 1))
    fi
  done
  printf 'old static tokens still accepted: %s\n' "$old_ok"
  [ "$old_ok" -eq 0 ] || exit 1
}

update_local_env() {
  local env_file="$HOME/.acc/.env"
  set_key_file "$env_file" ACC_AGENT_TOKEN "$FLEETCTL_TOKEN"
  set_key_file "$env_file" CCC_AGENT_TOKEN "$FLEETCTL_TOKEN"
  set_key_if_present "$env_file" ACC_TOKEN "$FLEETCTL_TOKEN"
}

FLEETCTL_TOKEN="$(new_token fleetctl)"
ROCKY_TOKEN="$(new_token rocky)"
BORIS_TOKEN="$(new_token boris)"
NATASHA_TOKEN="$(new_token natasha)"
BULLWINKLE_TOKEN="$(new_token bullwinkle)"
AUTH_TOKENS="$FLEETCTL_TOKEN,$ROCKY_TOKEN,$BORIS_TOKEN,$NATASHA_TOKEN,$BULLWINKLE_TOKEN"

echo "new ACC token fingerprints:"
printf '  fleetctl  %s\n' "$(hash12 "$FLEETCTL_TOKEN")"
printf '  rocky     %s\n' "$(hash12 "$ROCKY_TOKEN")"
printf '  boris     %s\n' "$(hash12 "$BORIS_TOKEN")"
printf '  natasha   %s\n' "$(hash12 "$NATASHA_TOKEN")"
printf '  bullwinkle %s\n' "$(hash12 "$BULLWINKLE_TOKEN")"

OLD_AUTH_TOKENS="$(get_old_auth_tokens)"

update_remote_agent_env "$BORIS_SSH" "$BORIS_TOKEN" boris
update_remote_agent_env "$NATASHA_SSH" "$NATASHA_TOKEN" natasha
update_remote_agent_env "$BULLWINKLE_SSH" "$BULLWINKLE_TOKEN" bullwinkle
update_rocky_hub_state
restart_rocky_hub

set -a
# shellcheck source=/dev/null
source "$HOME/.acc/.env" 2>/dev/null || true
set +a
ACC_URL="${ACC_URL:-${CCC_URL:-http://127.0.0.1:8789}}"

verify_new_tokens "$ACC_URL"
verify_old_tokens_rejected "$ACC_URL"
update_local_env

echo "rotation timestamp: $TS"
