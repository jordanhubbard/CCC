"""Per-item mutation endpoints on ``/api/item/{id}/*`` plus heartbeat.

Mirrors the Rust ``acc_client::items`` module.

These are the queue-worker write paths: claim, complete, fail, comment,
and keepalive against individual queue items.  The heartbeat endpoint
(``/api/heartbeat/{agent}``) lives here too — it is agent-liveness coupled
to the same queue-worker loop.
"""
from __future__ import annotations

from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from .client import Client


class ItemsApi:
    """Per-item mutations and agent heartbeat.

    Obtain via ``client.items`` — do not instantiate directly.
    """

    def __init__(self, client: "Client") -> None:
        self._c = client

    def claim(self, item_id: str, agent: str, note: str | None = None) -> None:
        """POST /api/item/{id}/claim.

        Raises :class:`~acc_client.Conflict` (HTTP 409) if another agent has
        already claimed the item.
        """
        body: dict[str, Any] = {"agent": agent}
        if note is not None:
            body["note"] = note
        self._c._request("POST", f"/api/item/{item_id}/claim", json=body)

    def complete(
        self,
        item_id: str,
        agent: str,
        *,
        result: str | None = None,
        resolution: str | None = None,
    ) -> None:
        """POST /api/item/{id}/complete."""
        body: dict[str, Any] = {"agent": agent}
        if result is not None:
            body["result"] = result
        if resolution is not None:
            body["resolution"] = resolution
        self._c._request("POST", f"/api/item/{item_id}/complete", json=body)

    def fail(self, item_id: str, agent: str, reason: str) -> None:
        """POST /api/item/{id}/fail."""
        self._c._request(
            "POST",
            f"/api/item/{item_id}/fail",
            json={"agent": agent, "reason": reason},
        )

    def comment(self, item_id: str, agent: str, comment: str) -> None:
        """POST /api/item/{id}/comment."""
        self._c._request(
            "POST",
            f"/api/item/{item_id}/comment",
            json={"agent": agent, "comment": comment},
        )

    def keepalive(self, item_id: str, agent: str, note: str | None = None) -> None:
        """POST /api/item/{id}/keepalive — extend the claim lease."""
        body: dict[str, Any] = {"agent": agent}
        if note is not None:
            body["note"] = note
        self._c._request("POST", f"/api/item/{item_id}/keepalive", json=body)

    def heartbeat(
        self,
        agent: str,
        *,
        status: str | None = None,
        note: str | None = None,
        host: str | None = None,
        ssh_user: str | None = None,
        ssh_host: str | None = None,
        ssh_port: int | None = None,
        ts: str | None = None,
    ) -> None:
        """POST /api/heartbeat/{agent} — agent liveness beacon.

        Mirrors :class:`acc_model::HeartbeatRequest` from the Rust crate.
        Fields that are ``None`` are omitted from the request body.
        """
        body: dict[str, Any] = {}
        for k, v in (
            ("ts", ts),
            ("status", status),
            ("note", note),
            ("host", host),
            ("ssh_user", ssh_user),
            ("ssh_host", ssh_host),
            ("ssh_port", ssh_port),
        ):
            if v is not None:
                body[k] = v
        self._c._request("POST", f"/api/heartbeat/{agent}", json=body)
