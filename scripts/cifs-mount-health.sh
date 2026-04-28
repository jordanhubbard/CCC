#!/usr/bin/env bash
# cifs-mount-health.sh — Diagnostic health-check for the AccFS CIFS/Samba mount.
#
# Run every 15 minutes via cron (see deploy/crontab-acc.txt):
#   */15 * * * * root bash WORKSPACE/scripts/cifs-mount-health.sh >> /var/log/accfs-health.log 2>&1
#
# Output is also tee-d to ${LOG_DIR}/accfs-health.log (default:
# ~/.acc/logs/accfs-health.log) so per-node log rotation is available
# independently of the system crontab redirect.
#
# Exit codes:
#   0  mount is healthy (or auto-remounted successfully)
#   1  mount is unhealthy and could not be recovered

set -euo pipefail

# ── Config ────────────────────────────────────────────────────────────────────
ACC_DIR="${ACC_DIR:-${HOME}/.acc}"
AGENTFS_MOUNT="${AGENTFS_MOUNT:-${ACC_DIR}/shared}"
AGENTFS_HOST="${AGENTFS_HOST:-100.89.199.14}"
AGENTFS_SHARE="${AGENTFS_SHARE:-accfs}"
AGENTFS_CREDS="${AGENTFS_CREDS:-/etc/samba/smbcredentials}"
AGENTFS_USER="${AGENTFS_USER:-jkh}"
STAT_TIMEOUT="${STAT_TIMEOUT:-10}"   # seconds before declaring a stale mount
LOG_DIR="${LOG_DIR:-${ACC_DIR}/logs}"
LOG_FILE="${LOG_DIR}/accfs-health.log"

# Disk-space thresholds (MiB of free space on the CIFS mount)
DISK_WARN_MIB="${DISK_WARN_MIB:-5120}"   # Warning < 5120 MiB, Critical < 2048 MiB
DISK_CRIT_MIB="${DISK_CRIT_MIB:-2048}"

# Source .env for any site-specific overrides (non-fatal if absent)
ENV_FILE="${ACC_DIR}/.env"
# shellcheck source=/dev/null
[ -f "$ENV_FILE" ] && source "$ENV_FILE" || true

# ── Per-node log file setup ───────────────────────────────────────────────────
# Ensure the log directory exists, then redirect all subsequent output through
# tee so every log line reaches both stdout (captured by the crontab redirect)
# and the per-node log file (suitable for independent logrotate rules).
mkdir -p "$LOG_DIR"
exec > >(tee -a "$LOG_FILE") 2>&1

# ── Logging ───────────────────────────────────────────────────────────────────
TS() { date -u +"%Y-%m-%dT%H:%M:%SZ"; }
log()  { echo "[$(TS)] [accfs-health] $*"; }
info() { log "INFO  $*"; }
warn() { log "WARN  $*"; }
err()  { log "ERROR $*"; }

# ── Helper: is the mount currently active? ────────────────────────────────────
_is_mounted() {
  if command -v findmnt &>/dev/null; then
    findmnt --noheadings --target "$AGENTFS_MOUNT" &>/dev/null
  else
    mount | grep -q " on ${AGENTFS_MOUNT} "
  fi
}

# ── Helper: can we actually read the mountpoint within the timeout? ───────────
_is_readable() {
  if command -v timeout &>/dev/null; then
    timeout "$STAT_TIMEOUT" ls "$AGENTFS_MOUNT" &>/dev/null
  else
    ls "$AGENTFS_MOUNT" &>/dev/null
  fi
}

# ── Helper: run a privileged command (directly when root, via sudo -n otherwise)
_priv() {
  if [ "$(id -u)" -eq 0 ]; then
    "$@"
  else
    sudo -n "$@"
  fi
}

# ── Helper: attempt remount (Linux only) ─────────────────────────────────────
_remount() {
  # Requires: cifs-utils installed, /etc/samba/smbcredentials present,
  # and the calling user to have passwordless sudo access to mount (or be root).
  if ! command -v mount.cifs &>/dev/null && ! command -v mount &>/dev/null; then
    err "mount.cifs not available — cannot remount"
    return 1
  fi

  if [ ! -f "$AGENTFS_CREDS" ]; then
    err "Credentials file not found: $AGENTFS_CREDS — cannot remount"
    return 1
  fi

  info "Attempting remount of //${AGENTFS_HOST}/${AGENTFS_SHARE} → ${AGENTFS_MOUNT}"
  mkdir -p "$AGENTFS_MOUNT"

  # Try via systemd unit first (preferred — picks up unit options)
  local _unit
  if command -v systemd-escape &>/dev/null; then
    _unit="$(systemd-escape --path "$AGENTFS_MOUNT").mount"
    if _priv systemctl start "$_unit" 2>/dev/null; then
      info "Remounted via systemd unit: $_unit"
      return 0
    fi
  fi

  # Fall back to a direct mount.cifs call
  local _uid _gid
  _uid=$(id -u)
  _gid=$(id -g)
  if _priv mount -t cifs \
      "//${AGENTFS_HOST}/${AGENTFS_SHARE}" \
      "$AGENTFS_MOUNT" \
      -o "credentials=${AGENTFS_CREDS},uid=${_uid},gid=${_gid},file_mode=0664,dir_mode=0775,_netdev,vers=3.0,nofail" \
      2>/dev/null; then
    info "Remounted via direct mount.cifs"
    return 0
  fi

  err "All remount attempts failed"
  return 1
}

# ── Helper: forcibly detach a stale mount ────────────────────────────────────
_force_umount() {
  info "Attempting lazy umount of $AGENTFS_MOUNT (stale)"
  _priv umount -l "$AGENTFS_MOUNT" 2>/dev/null || true
}

# ── Main health check ─────────────────────────────────────────────────────────
info "Starting CIFS mount health check for ${AGENTFS_MOUNT}"

PLATFORM="linux"
[[ "$(uname)" == "Darwin" ]] && PLATFORM="macos"

CHECKS_PASSED=0
CHECKS_FAILED=0

# (1) Mountpoint directory must exist
if [ ! -d "$AGENTFS_MOUNT" ]; then
  warn "Check 1 FAIL: Mountpoint directory does not exist: ${AGENTFS_MOUNT} — creating"
  mkdir -p "$AGENTFS_MOUNT" || { err "Could not create mountpoint"; exit 1; }
else
  info "Check 1 PASS: Mountpoint directory exists: ${AGENTFS_MOUNT}"
  (( CHECKS_PASSED++ )) || true
fi

# (2) Check whether something is mounted there at all
if ! _is_mounted; then
  warn "Check 2 FAIL: Nothing mounted at ${AGENTFS_MOUNT}"
  (( CHECKS_FAILED++ )) || true
  if [[ "$PLATFORM" == "linux" ]]; then
    if _remount; then
      info "Remount succeeded"
    else
      err "Remount failed — AccFS is unavailable"
      exit 1
    fi
  else
    # macOS: mount is managed by LaunchAgent; we can only report
    warn "macOS: mount recovery is handled by the LaunchAgent (com.acc.accfs-mount)"
    err "AccFS is not mounted — check ~/Library/LaunchAgents/com.acc.accfs-mount.plist"
    exit 1
  fi
else
  info "Check 2 PASS: Mount is active at ${AGENTFS_MOUNT}"
  (( CHECKS_PASSED++ )) || true
fi

# (3) Disk-space check — Warning < 5120 MiB, Critical < 2048 MiB
FREE_MIB=0
if command -v df &>/dev/null; then
  FREE_MIB=$(df -m "$AGENTFS_MOUNT" 2>/dev/null | awk 'NR==2 {print $4}') || FREE_MIB=0
fi
if [ "${FREE_MIB:-0}" -lt "$DISK_CRIT_MIB" ]; then
  err  "Check 3 FAIL: Free space CRITICAL — ${FREE_MIB} MiB free (threshold: ${DISK_CRIT_MIB} MiB)"
  (( CHECKS_FAILED++ )) || true
elif [ "${FREE_MIB:-0}" -lt "$DISK_WARN_MIB" ]; then
  warn "Check 3 WARN: Free space LOW — ${FREE_MIB} MiB free (threshold: ${DISK_WARN_MIB} MiB)"
  (( CHECKS_PASSED++ )) || true
else
  info "Check 3 PASS: Free space OK — ${FREE_MIB} MiB free"
  (( CHECKS_PASSED++ )) || true
fi

# (4) Verify the mount is actually readable (catches stale/hung mounts)
if ! _is_readable; then
  err "Check 4 FAIL: Mount exists at ${AGENTFS_MOUNT} but is unreadable or hung (stale CIFS mount?)"
  (( CHECKS_FAILED++ )) || true
  if [[ "$PLATFORM" == "linux" ]]; then
    _force_umount
    if _remount; then
      info "Stale mount cleared and remounted successfully"
    else
      err "Stale mount could not be recovered — AccFS is unavailable"
      exit 1
    fi
  else
    err "macOS stale mount — run: diskutil unmount force ${AGENTFS_MOUNT}"
    exit 1
  fi
else
  info "Check 4 PASS: Mount is readable at ${AGENTFS_MOUNT}"
  (( CHECKS_PASSED++ )) || true
fi

# (5) Write a sentinel file to verify write access (best-effort)
SENTINEL="${AGENTFS_MOUNT}/.cifs-health-$(hostname -s 2>/dev/null || echo local)"
if touch "$SENTINEL" 2>/dev/null; then
  rm -f "$SENTINEL" 2>/dev/null || true
  info "Check 5 PASS: Write access confirmed on ${AGENTFS_MOUNT}"
  (( CHECKS_PASSED++ )) || true
else
  warn "Check 5 WARN: Mount is readable but not writable at ${AGENTFS_MOUNT} (read-only share?)"
  (( CHECKS_FAILED++ )) || true
fi

# (6) Verify CIFS mount options include expected security/version parameters
MOUNT_OPTS=""
if command -v findmnt &>/dev/null; then
  MOUNT_OPTS=$(findmnt --noheadings --output OPTIONS --target "$AGENTFS_MOUNT" 2>/dev/null || true)
fi
if echo "$MOUNT_OPTS" | grep -q "vers="; then
  info "Check 6 PASS: CIFS protocol version option present in mount options"
  (( CHECKS_PASSED++ )) || true
else
  warn "Check 6 WARN: Could not verify CIFS protocol version in mount options (may be unavailable on this platform)"
  (( CHECKS_PASSED++ )) || true
fi

# (7) Confirm the share is reachable at the network level (non-fatal)
HOST_OK=false
if command -v ping &>/dev/null; then
  if ping -c 1 -W 3 "$AGENTFS_HOST" &>/dev/null 2>&1; then
    HOST_OK=true
  fi
fi
if $HOST_OK; then
  info "Check 7 PASS: CIFS host ${AGENTFS_HOST} is reachable via ICMP"
  (( CHECKS_PASSED++ )) || true
else
  warn "Check 7 WARN: CIFS host ${AGENTFS_HOST} did not respond to ping (ICMP may be blocked)"
  (( CHECKS_PASSED++ )) || true
fi

# ── Summary ───────────────────────────────────────────────────────────────────
info "Health check complete — ${CHECKS_PASSED} passed, ${CHECKS_FAILED} failed"

if [ "$CHECKS_FAILED" -gt 0 ]; then
  err "AccFS mount has issues — review warnings/errors above"
  exit 1
fi

info "AccFS mount is healthy: ${AGENTFS_MOUNT}"
exit 0
