"""Task operations on ``/api/tasks``.

Mirrors the Rust ``acc_client::tasks`` module.
"""
from __future__ import annotations

from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from .client import Client


class TasksApi:
    """Operations on ``/api/tasks``.

    Obtain via ``client.tasks`` — do not instantiate directly.
    """

    def __init__(self, client: "Client") -> None:
        self._c = client

    def list(
        self,
        *,
        status: str | None = None,
        task_type: str | None = None,
        project: str | None = None,
        agent: str | None = None,
        limit: int | None = None,
    ) -> list[dict[str, Any]]:
        """GET /api/tasks — list tasks with optional filters."""
        params: dict[str, Any] = {}
        if status is not None:
            params["status"] = status
        if task_type is not None:
            params["task_type"] = task_type
        if project is not None:
            params["project"] = project
        if agent is not None:
            params["agent"] = agent
        if limit is not None:
            params["limit"] = limit
        resp = self._c._request("GET", "/api/tasks", params=params) or {}
        return resp.get("tasks", [])

    def get(self, task_id: str) -> dict[str, Any]:
        """GET /api/tasks/{id}."""
        resp = self._c._request("GET", f"/api/tasks/{task_id}") or {}
        return resp.get("task", resp)

    def create(self, **fields: Any) -> dict[str, Any]:
        """POST /api/tasks — create a task.

        Keyword arguments map directly to wire fields.  Common keys:
        ``project_id``, ``title``, ``description``, ``priority``,
        ``task_type``, ``agent``, ``review_of``, ``phase``,
        ``blocked_by``.
        """
        resp = self._c._request("POST", "/api/tasks", json=fields) or {}
        return resp.get("task", resp)

    def claim(self, task_id: str, agent: str) -> dict[str, Any]:
        """PUT /api/tasks/{id}/claim.

        Raises :class:`~acc_client.Conflict` (HTTP 409) if already claimed.
        Raises :class:`~acc_client.Locked` (HTTP 423) if blocked by
        unfulfilled dependencies; the blocking task ID is in
        ``exc.extra["pending"]``.
        """
        resp = self._c._request(
            "PUT", f"/api/tasks/{task_id}/claim", json={"agent": agent}
        ) or {}
        return resp.get("task", resp)

    def unclaim(self, task_id: str, agent: str | None = None) -> None:
        """PUT /api/tasks/{id}/unclaim."""
        body: dict[str, Any] = {}
        if agent is not None:
            body["agent"] = agent
        self._c._request("PUT", f"/api/tasks/{task_id}/unclaim", json=body)

    def complete(
        self,
        task_id: str,
        agent: str | None = None,
        output: str | None = None,
    ) -> None:
        """PUT /api/tasks/{id}/complete."""
        body: dict[str, Any] = {}
        if agent is not None:
            body["agent"] = agent
        if output is not None:
            body["output"] = output
        self._c._request("PUT", f"/api/tasks/{task_id}/complete", json=body)

    def review_result(
        self,
        task_id: str,
        result: str,
        *,
        agent: str | None = None,
        notes: str | None = None,
        summary_hallucination: bool | None = None,
    ) -> None:
        """PUT /api/tasks/{id}/review-result.

        ``summary_hallucination=True`` signals that the worker's
        ``work_output_summary`` describes code that does not exist in the
        actual diff (fabricated summary).  The server records this flag so
        upstream analytics can penalise the submitting agent.

        Mirrors :meth:`acc_client::tasks::TasksApi::review_result` in the Rust
        crate including the :class:`~acc_client.model.ReviewResultRequest`
        ``summary_hallucination`` field.
        """
        body: dict[str, Any] = {"result": result}
        if agent is not None:
            body["agent"] = agent
        if notes is not None:
            body["notes"] = notes
        if summary_hallucination is not None:
            body["summary_hallucination"] = summary_hallucination
        self._c._request("PUT", f"/api/tasks/{task_id}/review-result", json=body)

    def vote(
        self,
        task_id: str,
        agent: str,
        vote: str,
        *,
        refinement: str | None = None,
    ) -> dict[str, Any]:
        """PUT /api/tasks/{id}/vote — cast an idea-review vote.

        ``vote`` must be ``"approve"`` or ``"reject"``.

        A non-empty ``refinement`` is **always** sent so the server never
        receives a missing field and returns HTTP 400.  The rules are:

        * ``"approve"`` — ``refinement`` is required and must be non-empty;
          a :class:`ValueError` is raised client-side if it is ``None`` or
          blank, rather than letting the server reject the request.
        * ``"reject"``  — ``refinement`` is optional; when omitted the
          client substitutes the synthetic string
          ``"(no refinement provided)"`` so the wire body always contains
          the field.
        """
        if not refinement:
            if vote == "approve":
                raise ValueError(
                    "vote() requires a non-empty 'refinement' for an 'approve' vote"
                )
            refinement = "(no refinement provided)"
        body: dict[str, Any] = {"agent": agent, "vote": vote, "refinement": refinement}
        resp = self._c._request("PUT", f"/api/tasks/{task_id}/vote", json=body)
        return resp if isinstance(resp, dict) else {}

    def cancel(self, task_id: str) -> None:
        """DELETE /api/tasks/{id} — cancel / abort."""
        self._c._request("DELETE", f"/api/tasks/{task_id}")
