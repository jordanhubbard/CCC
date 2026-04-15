# Description: Install CCC hub services (ccc-server, ccc-dashboard) on hub nodes
#
# Context: Hub nodes run the main ccc-server (Rust/Axum) and the dashboard frontend.
# Hub detection: HUB_NODE=true in .env, or IS_HUB=true, or ccc-server is in /usr/local/bin.
# Only runs on Linux since hub nodes run Ubuntu/Debian.

if ! on_platform linux; then return 0; fi

# Detect hub role
IS_HUB="${IS_HUB:-false}"
if [ -f "/usr/local/bin/ccc-server" ] || systemctl is-active ccc-server.service >/dev/null 2>&1; then
  IS_HUB=true
fi
if [ "${HUB_NODE:-false}" = "true" ]; then IS_HUB=true; fi

if [ "$IS_HUB" != "true" ]; then
  m_skip "not a hub node — skipping ccc-server and ccc-dashboard services"
  return 0
fi

systemd_install deploy/systemd/ccc-server.service    ccc-server.service
systemd_install deploy/systemd/ccc-dashboard.service ccc-dashboard.service
