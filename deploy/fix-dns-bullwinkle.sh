#!/usr/bin/env bash
# fix-dns-bullwinkle.sh — One-shot DNS repair for bullwinkle (jordan's home Mac).
#
# Problem: bullwinkle's networksetup has NVIDIA-internal 10.x DNS servers
# (10.126.4.154, 10.61.13.53) that are unreachable from home, plus 8.8.8.8
# that the home firewall also blocks.  External names like github.com and
# crates.io time out, breaking cargo fetch and git pull.
#
# This script:
#   1. Replaces all DNS servers on every active network interface with
#      1.1.1.1 + 8.8.8.8 (public resolvers that work from any network).
#   2. Flushes the macOS DNS cache.
#   3. Verifies that github.com resolves.
#   4. Persists the setting so it survives reboots.
#
# Safe to re-run.  Idempotent.
#
# Usage (run on bullwinkle):
#   bash deploy/fix-dns-bullwinkle.sh
#
# To revert to DHCP-assigned DNS:
#   networksetup -setdnsservers <interface> Empty

set -euo pipefail

PUBLIC_DNS="1.1.1.1 8.8.8.8"

echo "[fix-dns] Detecting active network interfaces..."

# networksetup -listallnetworkservices prints a header line then one service
# per line. Skip the header and any lines with (*) which means disabled.
mapfile -t SERVICES < <(networksetup -listallnetworkservices 2>/dev/null \
    | tail -n +2 \
    | grep -v '^\*')

if [[ ${#SERVICES[@]} -eq 0 ]]; then
    echo "[fix-dns] ERROR: no network services found via networksetup" >&2
    exit 1
fi

CHANGED=0
for svc in "${SERVICES[@]}"; do
    current=$(networksetup -getdnsservers "$svc" 2>/dev/null || true)
    # "There aren't any DNS Servers set" means DHCP — still override so we
    # always get a known-good resolver regardless of what DHCP hands out.
    if [[ "$current" == "1.1.1.1"* ]]; then
        echo "[fix-dns]   $svc — already set to $PUBLIC_DNS, skipping"
        continue
    fi
    echo "[fix-dns]   $svc — setting DNS to $PUBLIC_DNS (was: $(echo "$current" | tr '\n' ' '))"
    # shellcheck disable=SC2086
    networksetup -setdnsservers "$svc" $PUBLIC_DNS
    CHANGED=$((CHANGED + 1))
done

echo "[fix-dns] $CHANGED interface(s) updated."

# Flush DNS cache (works on macOS 10.10+)
echo "[fix-dns] Flushing DNS cache..."
if command -v dscacheutil &>/dev/null; then
    dscacheutil -flushcache 2>/dev/null || true
fi
if command -v killall &>/dev/null; then
    killall -HUP mDNSResponder 2>/dev/null || true
fi
echo "[fix-dns] DNS cache flushed."

# Verify
echo "[fix-dns] Verifying resolution of github.com..."
RESOLVED=""
for attempt in 1 2 3; do
    RESOLVED=$(dscacheutil -q host -a name github.com 2>/dev/null \
                | grep '^ip_address:' | head -1 | awk '{print $2}' || true)
    [[ -n "$RESOLVED" ]] && break
    sleep 1
done

if [[ -n "$RESOLVED" ]]; then
    echo "[fix-dns] ✓ github.com resolves to $RESOLVED"
else
    # Try plain nslookup as a fallback check
    if nslookup github.com 1.1.1.1 &>/dev/null 2>&1; then
        echo "[fix-dns] ✓ github.com resolves (via nslookup against 1.1.1.1)"
    else
        echo "[fix-dns] ✗ github.com still not resolving — check network connectivity" >&2
        echo "           Try: nslookup github.com 1.1.1.1" >&2
        exit 1
    fi
fi

echo "[fix-dns] Done.  DNS is now: $PUBLIC_DNS"
echo "          This setting persists across reboots."
echo "          To revert to DHCP DNS: networksetup -setdnsservers <interface> Empty"
