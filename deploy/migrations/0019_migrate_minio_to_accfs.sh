#!/usr/bin/env bash
# Description: Replace MinIO/JuiceFS AgentFS with Samba/CIFS mount at ~/.acc/shared.
#              On Rocky (hub): updates ccc-server env, no mount needed (local disk).
#              On Linux agents: installs cifs mount unit.
#              On macOS agents: installs launchd mount plist.

set -euo pipefail

ACC_DEST="${HOME}/.acc"
[[ -d "$ACC_DEST" ]] || ACC_DEST="${HOME}/.ccc"
WORKSPACE="${ACC_DEST}/workspace"
SHARED_DIR="${ACC_DEST}/shared"
ENV_FILE="${ACC_DEST}/.env"
SMB_HOST="100.89.199.14"
SMB_SHARE="accfs"
SMB_USER="jkh"

m_info "Migrate MinIO/JuiceFS AgentFS to Samba/CIFS AccFS"

# ── Fetch SMB password from CCC secrets ────────────────────────────────────────
SMB_PASS=""
if [[ -n "${ACC_URL:-}" ]] && [[ -n "${ACC_AGENT_TOKEN:-}" ]]; then
  SMB_PASS=$(curl -sf \
    -H "Authorization: Bearer ${ACC_AGENT_TOKEN}" \
    "${ACC_URL}/api/secrets/SMB_PASSWORD" 2>/dev/null \
    | python3 -c "
import json,sys
try:
    d = json.load(sys.stdin)
    print(d.get('value', ''))
except: pass
" 2>/dev/null || true)
fi

if [[ -z "$SMB_PASS" ]]; then
  m_warn "SMB_PASSWORD not found in CCC secrets — set it via /api/secrets then re-run"
  m_warn "Skipping mount setup; AccFS may not work until SMB_PASSWORD is available"
fi

# ── Detect hub role (Rocky runs ccc-server locally) ───────────────────────────
IS_HUB=false
if command -v systemctl &>/dev/null && systemctl is-active --quiet ccc-server.service 2>/dev/null; then
  IS_HUB=true
fi

if [[ "$IS_HUB" == "true" ]]; then
  m_info "Hub role detected — updating ccc-server env (no mount needed, accfs is local /srv/accfs)"

  # Ensure /srv/accfs exists
  if [[ ! -d /srv/accfs ]]; then
    sudo mkdir -p /srv/accfs
    sudo chown "$(id -u):$(id -g)" /srv/accfs
    m_info "Created /srv/accfs"
  fi

  # Add ACC_FS_ROOT to env, remove MINIO_* vars
  if [[ -f "$ENV_FILE" ]]; then
    python3 - "$ENV_FILE" << 'PYEOF'
import re, sys

path = sys.argv[1]
with open(path) as f:
    lines = f.readlines()

out = []
skip_section = False
for line in lines:
    # Remove MinIO section header and vars
    if re.match(r'#.*MinIO', line, re.I):
        skip_section = True
    if skip_section and re.match(r'MINIO_', line):
        continue
    if skip_section and line.strip() == '':
        skip_section = False
        continue
    if re.match(r'MINIO_', line):
        continue
    out.append(line)

# Add ACC_FS_ROOT if not already present
content = ''.join(out)
if 'ACC_FS_ROOT' not in content:
    out.append('\n# AccFS (Samba shared filesystem)\nACC_FS_ROOT=/srv/accfs\n')

with open(path, 'w') as f:
    f.writelines(out)
print("env updated")
PYEOF
    m_success "Removed MINIO_* vars, added ACC_FS_ROOT=/srv/accfs to $ENV_FILE"
  fi

  # Restart ccc-server to pick up new env
  if command -v systemctl &>/dev/null; then
    sudo systemctl restart ccc-server.service && m_success "ccc-server restarted"
  fi

  # Store SMB_PASSWORD in CCC secrets for other agents to fetch
  if [[ -n "$SMB_PASS" ]] && [[ -n "${ACC_AGENT_TOKEN:-}" ]] && [[ -n "${ACC_URL:-}" ]]; then
    EXISTING=$(curl -sf -H "Authorization: Bearer ${ACC_AGENT_TOKEN}" "${ACC_URL}/api/secrets" 2>/dev/null || echo '{}')
    UPDATED=$(python3 -c "
import json, sys
d = json.loads(sys.argv[1])
secs = d.get('secrets', {})
secs['SMB_PASSWORD'] = sys.argv[2]
secs['rocky_smb_user'] = 'jkh'
secs['rocky_smb_host'] = '100.89.199.14'
secs['rocky_smb_share'] = 'accfs'
print(json.dumps(secs))
" "$EXISTING" "$SMB_PASS" 2>/dev/null || echo '')
    if [[ -n "$UPDATED" ]]; then
      curl -sf -X POST \
        -H "Authorization: Bearer ${ACC_AGENT_TOKEN}" \
        -H "Content-Type: application/json" \
        -d "{\"secrets\": $UPDATED}" \
        "${ACC_URL}/api/secrets" > /dev/null && m_success "SMB_PASSWORD stored in CCC secrets"
    fi
  fi

  m_success "Hub migration complete — AccFS root: /srv/accfs"
  exit 0
fi

# ── Agent: set up mount ────────────────────────────────────────────────────────
mkdir -p "$SHARED_DIR"
mkdir -p "${ACC_DEST}/logs"

if on_platform macos; then
  # ── macOS: launchd plist + mount_smbfs ──────────────────────────────────────
  m_info "macOS: setting up SMB mount via launchd"

  # Store SMB credentials in Keychain
  if [[ -n "$SMB_PASS" ]]; then
    security add-internet-password -s "$SMB_HOST" -a "$SMB_USER" -w "$SMB_PASS" 2>/dev/null \
      || security delete-internet-password -s "$SMB_HOST" -a "$SMB_USER" 2>/dev/null \
      && security add-internet-password -s "$SMB_HOST" -a "$SMB_USER" -w "$SMB_PASS" 2>/dev/null \
      || true
    m_success "SMB credentials stored in macOS Keychain"
  fi

  PLIST_TMPL="${WORKSPACE}/deploy/launchd/com.acc.accfs-mount.plist.tmpl"
  PLIST_OUT="${HOME}/Library/LaunchAgents/com.acc.accfs-mount.plist"
  mkdir -p "${HOME}/Library/LaunchAgents"

  if [[ -f "$PLIST_TMPL" ]]; then
    sed "s|AGENT_HOME|${HOME}|g" "$PLIST_TMPL" > "$PLIST_OUT"
    launchctl unload "$PLIST_OUT" 2>/dev/null || true
    launchctl load -w "$PLIST_OUT" && m_success "com.acc.accfs-mount loaded"
    sleep 3
    mountpoint -q "$SHARED_DIR" && m_success "AccFS mounted at $SHARED_DIR" \
      || m_warn "Mount not yet ready — check ~/Library/LaunchAgents/ and smb credentials"
  else
    m_warn "Plist template not found at $PLIST_TMPL — skipping launchd install"
  fi

elif on_platform linux; then
  # ── Linux: systemd mount unit + cifs ────────────────────────────────────────
  m_info "Linux: setting up CIFS mount via systemd"

  # Ensure cifs-utils installed
  if ! command -v mount.cifs &>/dev/null; then
    m_info "Installing cifs-utils..."
    sudo apt-get install -y cifs-utils 2>/dev/null \
      || sudo yum install -y cifs-utils 2>/dev/null \
      || sudo dnf install -y cifs-utils 2>/dev/null \
      || m_warn "Could not install cifs-utils automatically — install manually"
  fi

  # Write credentials file
  if [[ -n "$SMB_PASS" ]]; then
    printf 'username=%s\npassword=%s\n' "$SMB_USER" "$SMB_PASS" \
      | sudo tee /etc/samba/smbcredentials > /dev/null
    sudo chmod 600 /etc/samba/smbcredentials
    m_success "SMB credentials written to /etc/samba/smbcredentials"
  else
    m_warn "No SMB_PASS — /etc/samba/smbcredentials not written; mount will fail"
  fi

  # Derive systemd unit name from mount point path
  # ~/.acc/shared on /home/USER → home-USER-.acc-shared.mount
  MOUNT_UNIT=$(systemd-escape --path --suffix=mount "$SHARED_DIR")
  UNIT_FILE="/etc/systemd/system/${MOUNT_UNIT}"

  MOUNT_TMPL="${WORKSPACE}/deploy/systemd/acc-accfs.mount.tmpl"
  if [[ -f "$MOUNT_TMPL" ]]; then
    AGENT_UID=$(id -u)
    AGENT_GID=$(id -g)
    sed "s|AGENT_HOME|${HOME}|g; s|AGENT_UID|${AGENT_UID}|g; s|AGENT_GID|${AGENT_GID}|g" \
      "$MOUNT_TMPL" | sudo tee "$UNIT_FILE" > /dev/null
    sudo systemctl daemon-reload
    sudo systemctl enable "${MOUNT_UNIT}"
    sudo systemctl start "${MOUNT_UNIT}" && m_success "AccFS mounted at $SHARED_DIR" \
      || m_warn "Mount unit failed — check: journalctl -u ${MOUNT_UNIT}"
  else
    # Fallback: ad-hoc cifs mount (no persistence across reboots)
    m_warn "Mount template not found — using ad-hoc mount (not persistent)"
    sudo mount -t cifs "//${SMB_HOST}/${SMB_SHARE}" "$SHARED_DIR" \
      -o "credentials=/etc/samba/smbcredentials,uid=$(id -u),gid=$(id -g),vers=3.0,nofail" \
      && m_success "AccFS mounted (ad-hoc)" || m_warn "Ad-hoc mount failed"
  fi
fi

# ── Update .env: remove MINIO_*, add ACC_SHARED_DIR ───────────────────────────
if [[ -f "$ENV_FILE" ]]; then
  python3 - "$ENV_FILE" "$SHARED_DIR" << 'PYEOF'
import re, sys
path, shared = sys.argv[1], sys.argv[2]
with open(path) as f:
    lines = f.readlines()
out = [l for l in lines if not re.match(r'MINIO_', l)]
content = ''.join(out)
if 'ACC_SHARED_DIR' not in content:
    out.append(f'\nACC_SHARED_DIR={shared}\n')
with open(path, 'w') as f:
    f.writelines(out)
print("env updated")
PYEOF
  m_success "Removed MINIO_* vars, added ACC_SHARED_DIR to $ENV_FILE"
fi

# ── Tear down JuiceFS/agentfs services if present ─────────────────────────────
for svc in clawfs-sync agentfs-sync; do
  if command -v systemctl &>/dev/null && systemctl list-unit-files "${svc}.service" &>/dev/null; then
    systemd_teardown "$svc" 2>/dev/null || true
    m_info "Removed ${svc} service"
  fi
  if on_platform macos; then
    PLIST="${HOME}/Library/LaunchAgents/com.acc.${svc}.plist"
    [[ -f "$PLIST" ]] && launchctl_teardown "com.acc.${svc}" 2>/dev/null || true
  fi
done

m_success "Migration 0019 complete — AccFS at $SHARED_DIR"
