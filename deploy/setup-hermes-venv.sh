#!/usr/bin/env bash
# Creates ~/.acc/hermes-venv and installs hermes (editable) into it.
# Safe to re-run — upgrades pip and reinstalls if the venv already exists.
# Handles: Ubuntu (python3-venv), macOS (homebrew Python 3.11+), PEP 668.
set -euo pipefail

VENV="${HOME}/.acc/hermes-venv"
HERMES_SRC="${HOME}/Src/ACC/hermes"
ACC_CLIENT_SRC="${HOME}/Src/ACC/clients/python/acc_client"

if [ ! -d "${HERMES_SRC}" ]; then
    echo "[hermes-venv] ERROR: hermes source not found at ${HERMES_SRC}" >&2
    exit 1
fi

# Find a Python >= 3.11
PYTHON=""
for candidate in \
    /opt/homebrew/bin/python3.13 \
    /opt/homebrew/bin/python3.12 \
    /opt/homebrew/bin/python3.11 \
    /usr/bin/python3.13 \
    /usr/bin/python3.12 \
    /usr/bin/python3.11 \
    python3.13 \
    python3.12 \
    python3.11 \
    python3; do
    if command -v "$candidate" &>/dev/null; then
        VER=$("$candidate" -c 'import sys; print(sys.version_info[:2])' 2>/dev/null || echo "(0, 0)")
        if "$candidate" -c 'import sys; sys.exit(0 if sys.version_info >= (3, 11) else 1)' 2>/dev/null; then
            PYTHON="$candidate"
            break
        fi
    fi
done

if [ -z "$PYTHON" ]; then
    echo "[hermes-venv] ERROR: Python 3.11+ not found. Install via homebrew or apt." >&2
    echo "  macOS:  brew install python@3.12" >&2
    echo "  Ubuntu: sudo apt install python3.12" >&2
    exit 1
fi

echo "[hermes-venv] Using $PYTHON ($(${PYTHON} --version))"

# On Ubuntu/Debian, python3-venv is a separate apt package. setup-node.sh installs it.
if ! "$PYTHON" -m venv --help &>/dev/null 2>&1; then
    PY_VER=$("$PYTHON" -c 'import sys; print(f"{sys.version_info.major}.{sys.version_info.minor}")')
    echo "[hermes-venv] ERROR: python${PY_VER}-venv not installed." >&2
    echo "  Run: sudo apt-get install -y python${PY_VER}-venv" >&2
    echo "  Or:  bash ~/Src/ACC/deploy/setup-node.sh  (installs all prerequisites)" >&2
    exit 1
fi

echo "[hermes-venv] Creating venv at ${VENV}"
"$PYTHON" -m venv "${VENV}"

echo "[hermes-venv] Upgrading pip"
"${VENV}/bin/pip" install --quiet --upgrade pip

echo "[hermes-venv] Installing hermes from ${HERMES_SRC}"
"${VENV}/bin/pip" install --quiet -e "${HERMES_SRC}[slack]"

if [ -d "${ACC_CLIENT_SRC}" ]; then
    echo "[hermes-venv] Installing acc_client from ${ACC_CLIENT_SRC}"
    "${VENV}/bin/pip" install --quiet -e "${ACC_CLIENT_SRC}"
fi

echo "[hermes-venv] Installing hermes wrapper at ~/.local/bin/hermes"
mkdir -p "${HOME}/.local/bin"
cat > "${HOME}/.local/bin/hermes" << 'WRAPPER'
#!/usr/bin/env bash
VENV="${HOME}/.acc/hermes-venv"
if [ ! -x "${VENV}/bin/hermes" ]; then
    echo "[hermes] venv missing — run: bash ~/Src/ACC/deploy/setup-hermes-venv.sh" >&2
    exit 1
fi
exec "${VENV}/bin/hermes" "$@"
WRAPPER
chmod +x "${HOME}/.local/bin/hermes"

echo "[hermes-venv] Done."
"${VENV}/bin/hermes" --version 2>/dev/null || echo "[hermes-venv] Installed — PATH may need ~/.local/bin"
