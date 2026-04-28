"""Memory endpoints: semantic search and store.

Mirrors the Rust ``acc_client::memory`` module.

This is the HTTP path consumed by the hermes ``acc_shared_memory`` plugin.
The server's memory responses are loosely typed (vector-search results with
backend-specific metadata); we return raw dicts and let callers decide how
to interpret the payload fields.
"""
from __future__ import annotations

from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from .client import Client


class MemoryApi:
    """Semantic search and store on ``/api/memory/*``.

    Obtain via ``client.memory`` — do not instantiate directly.

    This is the primary Python interface for the hermes
    ``acc_shared_memory`` plugin — it calls :meth:`search` and
    :meth:`store` to retrieve and persist agent memory via the ACC hub.
    """

    def __init__(self, client: "Client") -> None:
        self._c = client

    def search(
        self,
        query: str,
        *,
        limit: int | None = None,
        collection: str | None = None,
    ) -> list[dict[str, Any]]:
        """POST /api/memory/search — semantic search.

        Returns a list of hit dicts.  Each dict typically contains
        ``text``, ``score``, and optional ``metadata`` and ``id`` fields,
        but the exact shape depends on the backend.

        Accepts both ``{"results": [...]}`` and ``{"hits": [...]}``
        envelopes as well as a bare JSON array.

        Example::

            hits = client.memory.search(
                "buffer overflow in render pipeline",
                limit=10,
                collection="acc_memory",
            )
            for h in hits:
                print(h.get("score"), h.get("text"))
        """
        body: dict[str, Any] = {"query": query}
        if limit is not None:
            body["limit"] = limit
        if collection is not None:
            body["collection"] = collection
        resp = self._c._request("POST", "/api/memory/search", json=body)
        if isinstance(resp, dict):
            # Server may return {results: [...]} or {hits: [...]}
            return resp.get("results", resp.get("hits", []))
        if isinstance(resp, list):
            return resp
        return []

    def store(
        self,
        text: str,
        *,
        metadata: dict[str, Any] | None = None,
        collection: str | None = None,
    ) -> None:
        """POST /api/memory/store — persist a memory entry.

        Mirrors :class:`acc_model::MemoryStoreRequest` from the Rust crate.

        Example::

            client.memory.store(
                "Fixed NPE in agent.rs line 42",
                metadata={"agent": "boris", "task_id": "task-123"},
            )
        """
        body: dict[str, Any] = {"text": text}
        if metadata is not None:
            body["metadata"] = metadata
        if collection is not None:
            body["collection"] = collection
        self._c._request("POST", "/api/memory/store", json=body)
