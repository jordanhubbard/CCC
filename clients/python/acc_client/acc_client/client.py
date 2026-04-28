"""Synchronous ACC API client.

Mirrors the shape of the Rust ``acc_client::Client``. Resource APIs are
attached as attributes so the mental model matches the Rust crate:

    client.tasks    → :class:`~acc_client._tasks.TasksApi`
    client.projects → :class:`~acc_client._projects.ProjectsApi`
    client.queue    → :class:`~acc_client._queue.QueueApi`
    client.items    → :class:`~acc_client._items.ItemsApi`
    client.bus      → :class:`~acc_client._bus.BusApi`
    client.memory   → :class:`~acc_client._memory.MemoryApi`
    client.agents   → :class:`~acc_client._agents.AgentsApi`

Each sub-API module is a direct mirror of the corresponding Rust module in
the ``acc-client`` crate (e.g. ``acc_client::tasks``, ``acc_client::bus``).
The Python naming convention uses snake_case attributes where Rust uses
method chaining builders.
"""
from __future__ import annotations

from typing import Any

import httpx

from ._agents import AgentsApi
from ._auth import resolve_base_url, resolve_token
from ._bus import BusApi, _extract_sse_data  # re-export for test backwards-compat
from ._errors import from_response
from ._items import ItemsApi
from ._memory import MemoryApi
from ._projects import ProjectsApi
from ._queue import QueueApi
from ._tasks import TasksApi

__all__ = [
    "Client",
    # Re-exported so existing test imports of the form
    #   ``from acc_client.client import _extract_sse_data``
    # continue to work after the refactor moved the function to _bus.py.
    "_extract_sse_data",
]

# Default per-request timeout.  Individual calls that need more (streaming,
# long server-side ops) can override this by passing a per-call client.
DEFAULT_TIMEOUT = 30.0


class Client:
    """Synchronous ACC client.

    Obtain one per process (or per thread) and share it — the underlying
    ``httpx.Client`` pools connections.  Always call :meth:`close` when
    done, or use the client as a context manager.

    The client is cheap to construct but not cheap to leak — ``close()``
    or ``with`` ensures the underlying HTTP pool shuts down cleanly.

    Credential resolution
    ---------------------
    If ``token`` is not supplied the constructor calls
    :func:`~acc_client._auth.resolve_token` which checks (highest priority
    first):

    1. ``ACC_TOKEN`` environment variable
    2. ``CCC_AGENT_TOKEN`` environment variable (legacy)
    3. ``ACC_AGENT_TOKEN`` environment variable
    4. ``~/.acc/.env`` file (keys ``ACC_TOKEN``, ``ACC_AGENT_TOKEN``)

    Base URL resolution follows :func:`~acc_client._auth.resolve_base_url`:
    ``ACC_HUB_URL`` → ``ACC_URL`` → ``CCC_URL`` → ``http://localhost:8789``.
    """

    def __init__(
        self,
        *,
        base_url: str | None = None,
        token: str | None = None,
        timeout: float = DEFAULT_TIMEOUT,
    ):
        self._base = resolve_base_url(base_url)
        self._token = resolve_token(token)
        self._http = httpx.Client(
            base_url=self._base,
            headers={"Authorization": f"Bearer {self._token}"},
            timeout=timeout,
        )

        # Sub-APIs — one instance per resource, each holding a reference
        # back to this Client so they share the pooled HTTP connection.
        self.tasks: TasksApi = TasksApi(self)
        self.projects: ProjectsApi = ProjectsApi(self)
        self.queue: QueueApi = QueueApi(self)
        self.items: ItemsApi = ItemsApi(self)
        self.bus: BusApi = BusApi(self)
        self.memory: MemoryApi = MemoryApi(self)
        self.agents: AgentsApi = AgentsApi(self)

    @classmethod
    def from_env(cls, *, timeout: float = DEFAULT_TIMEOUT) -> "Client":
        """Construct a client using environment variables / dotenv for credentials.

        Equivalent to ``Client()`` with no explicit arguments; provided as a
        named constructor to make call-sites self-documenting.
        """
        return cls(timeout=timeout)

    @property
    def base_url(self) -> str:
        """Base URL this client points at (no trailing slash)."""
        return self._base

    def close(self) -> None:
        """Close the underlying HTTP connection pool."""
        self._http.close()

    def __enter__(self) -> "Client":
        return self

    def __exit__(self, *_exc: Any) -> None:
        self.close()

    # ── Low-level request helper shared by all sub-API classes ────────────

    def _request(
        self,
        method: str,
        path: str,
        *,
        params: dict[str, Any] | None = None,
        json: Any | None = None,
    ) -> Any:
        """Issue a request, decode JSON, and raise on non-2xx.

        This is an internal helper — prefer typed sub-API methods where
        available.  Use this as an escape hatch for bespoke endpoints
        (custom dispatch, soul data, etc.) not worth typing upstream,
        mirroring :meth:`acc_client::Client::request_json` in the Rust
        crate.
        """
        resp = self._http.request(method, path, params=params, json=json)
        if not (200 <= resp.status_code < 300):
            body: dict[str, Any] | None = None
            try:
                body = resp.json()
                if not isinstance(body, dict):
                    body = {"error": f"http_{resp.status_code}", "message": str(body)}
            except ValueError:
                body = {
                    "error": f"http_{resp.status_code}",
                    "message": resp.text,
                }
            raise from_response(resp.status_code, body)
        if not resp.content:
            return None
        try:
            return resp.json()
        except ValueError:
            return resp.text
