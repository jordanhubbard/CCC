"""Project operations on ``/api/projects``.

Mirrors the Rust ``acc_client::projects`` module.
"""
from __future__ import annotations

from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from .client import Client


class ProjectsApi:
    """Project operations on ``/api/projects``.

    Obtain via ``client.projects`` — do not instantiate directly.
    """

    def __init__(self, client: "Client") -> None:
        self._c = client

    def list(
        self,
        *,
        status: str | None = None,
        q: str | None = None,
        limit: int | None = None,
    ) -> list[dict[str, Any]]:
        """GET /api/projects — list projects with optional filters."""
        params: dict[str, Any] = {}
        if status is not None:
            params["status"] = status
        if q is not None:
            params["q"] = q
        if limit is not None:
            params["limit"] = limit
        resp = self._c._request("GET", "/api/projects", params=params)
        if isinstance(resp, list):
            return resp
        if isinstance(resp, dict):
            return resp.get("projects", [])
        return []

    def get(self, project_id: str) -> dict[str, Any]:
        """GET /api/projects/{id}."""
        resp = self._c._request("GET", f"/api/projects/{project_id}") or {}
        return resp.get("project", resp)

    def create(self, **fields: Any) -> dict[str, Any]:
        """POST /api/projects — create a project.

        Common keyword arguments: ``name``, ``description``, ``repo``.
        """
        resp = self._c._request("POST", "/api/projects", json=fields) or {}
        return resp.get("project", resp)

    def delete(self, project_id: str, *, hard: bool = False) -> None:
        """DELETE /api/projects/{id}.

        ``hard=True`` requests a hard-delete (permanent); the default
        is a soft-archive.
        """
        params: dict[str, Any] = {}
        if hard:
            params["hard"] = "true"
        self._c._request("DELETE", f"/api/projects/{project_id}", params=params)
