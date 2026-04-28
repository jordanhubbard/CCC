"""Token resolution with the same precedence as the Rust acc-client.

Highest to lowest:
    1. Explicit argument
    2. ACC_TOKEN env
    3. CCC_AGENT_TOKEN env (legacy)
    4. ACC_AGENT_TOKEN env
    5. ~/.acc/.env keys (ACC_TOKEN, then ACC_AGENT_TOKEN)
"""
from __future__ import annotations

import os
from pathlib import Path

from ._errors import NoToken


_ENV_KEYS = ("ACC_TOKEN", "CCC_AGENT_TOKEN", "ACC_AGENT_TOKEN")
_DOTENV_KEYS = ("ACC_TOKEN", "ACC_AGENT_TOKEN")


def resolve_token(explicit: str | None = None) -> str:
    """Return a bearer token, or raise NoToken.

    Empty strings are treated as absent.
    """
    if explicit:
        return explicit

    for key in _ENV_KEYS:
        val = os.environ.get(key)
        if val:
            return val

    env_path = Path.home() / ".acc" / ".env"
    if env_path.exists():
        parsed = _parse_dotenv(env_path.read_text())
        for key in _DOTENV_KEYS:
            if key in parsed and parsed[key]:
                return parsed[key]

    raise NoToken(
        "No API token found. Pass explicitly, set ACC_TOKEN/CCC_AGENT_TOKEN, "
        "or add ACC_TOKEN to ~/.acc/.env"
    )


def resolve_base_url(explicit: str | None = None) -> str:
    """Return the hub base URL, stripping any trailing slash.

    Precedence (highest first):
        1. Explicit argument
        2. ``ACC_HUB_URL`` env  — matches the Rust ``acc-client`` default
        3. ``ACC_URL`` env
        4. ``CCC_URL`` env  (legacy)
        5. Hard-coded default ``http://localhost:8789``
    """
    url = (
        explicit
        or os.environ.get("ACC_HUB_URL")
        or os.environ.get("ACC_URL")
        or os.environ.get("CCC_URL")
        or "http://localhost:8789"
    )
    return url.rstrip("/")


def _parse_dotenv(text: str) -> dict[str, str]:
    """Minimal .env parser: KEY=value lines, # comments, optional quotes."""
    out: dict[str, str] = {}
    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        if "=" not in line:
            continue
        key, _, val = line.partition("=")
        key = key.strip()
        val = val.strip()
        if (val.startswith('"') and val.endswith('"')) or (
            val.startswith("'") and val.endswith("'")
        ):
            val = val[1:-1]
        out[key] = val
    return out
