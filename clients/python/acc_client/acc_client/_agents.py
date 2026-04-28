"""Agent registry reads on ``/api/agents``.

Mirrors the Rust ``acc_client::agents`` module.
"""
from __future__ import annotations

from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from .client import Client


class AgentsApi:
    """Agent registry read operations.

    Obtain via ``client.agents`` — do not instantiate directly.
    """

    def __init__(self, client: "Client") -> None:
        self._c = client

    def list(self, *, online: bool | None = None) -> list[dict[str, Any]]:
        """GET /api/agents — list agents.

        Pass ``online=True`` to restrict to agents whose last heartbeat
        arrived within the server's liveness window.
        """
        params: dict[str, Any] = {}
        if online is not None:
            params["online"] = "true" if online else "false"
        resp = self._c._request("GET", "/api/agents", params=params)
        if isinstance(resp, list):
            return resp
        if isinstance(resp, dict):
            return resp.get("agents", [])
        return []

    def names(self, *, online: bool = False) -> list[str]:
        """GET /api/agents/names — lightweight list of agent name strings.

        Useful for peer-discovery where the full agent envelope would be
        wasteful.  Mirrors :meth:`acc_client::agents::AgentsApi::names`.
        """
        params: dict[str, Any] = {}
        if online:
            params["online"] = "true"
        resp = self._c._request("GET", "/api/agents/names", params=params)
        if isinstance(resp, list):
            return [n for n in resp if isinstance(n, str)]
        if isinstance(resp, dict):
            names = resp.get("names", [])
            return [n for n in names if isinstance(n, str)]
        return []

    def get(self, name: str) -> dict[str, Any]:
        """GET /api/agents/{name} — fetch a single agent record."""
        resp = self._c._request("GET", f"/api/agents/{name}") or {}
        return resp.get("agent", resp)
