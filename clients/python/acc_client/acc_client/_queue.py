"""Queue list / get operations on ``/api/queue`` and ``/api/item/{id}``.

Mirrors the Rust ``acc_client::queue`` module.
"""
from __future__ import annotations

from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from .client import Client


class QueueApi:
    """Read-only queue operations.

    Obtain via ``client.queue`` — do not instantiate directly.

    For per-item mutations (claim / complete / fail / comment / keepalive)
    use ``client.items`` (:class:`~acc_client._items.ItemsApi`).
    """

    def __init__(self, client: "Client") -> None:
        self._c = client

    def list(self) -> list[dict[str, Any]]:
        """GET /api/queue — list all queue items.

        Accepts both a bare JSON array and the ``{"items": [...]}`` envelope
        that some server versions return.
        """
        resp = self._c._request("GET", "/api/queue")
        if isinstance(resp, list):
            return resp
        if isinstance(resp, dict):
            return resp.get("items", [])
        return []

    def get(self, item_id: str) -> dict[str, Any]:
        """GET /api/item/{id} — fetch a single queue item."""
        resp = self._c._request("GET", f"/api/item/{item_id}") or {}
        return resp.get("item", resp)
