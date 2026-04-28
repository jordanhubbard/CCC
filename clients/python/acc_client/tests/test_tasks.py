"""Tests for TasksApi in _tasks.py.

Mirrors the Rust acc-client integration tests in tests/tasks_integration.rs.
"""
from __future__ import annotations

import httpx
import pytest
import respx

from acc_client import Client, Conflict, Locked
from acc_client._tasks import TasksApi


@pytest.fixture
def client():
    c = Client(base_url="http://hub.test", token="t")
    yield c
    c.close()


def sample_task(task_id: str, status: str) -> dict:
    return {
        "id": task_id,
        "project_id": "proj-a",
        "title": "t",
        "description": "",
        "status": status,
        "priority": 2,
        "created_at": "2026-04-23T00:00:00Z",
        "task_type": "work",
        "metadata": {},
        "blocked_by": [],
    }


def test_tasks_api_type(client):
    assert isinstance(client.tasks, TasksApi)


@respx.mock
def test_tasks_list_filters_by_status(client):
    respx.get("http://hub.test/api/tasks", params={"status": "open", "limit": 5}).mock(
        return_value=httpx.Response(200, json={"tasks": [sample_task("t-1", "open")]})
    )
    tasks = client.tasks.list(status="open", limit=5)
    assert len(tasks) == 1
    assert tasks[0]["id"] == "t-1"
    assert tasks[0]["status"] == "open"


@respx.mock
def test_tasks_list_filters_by_task_type(client):
    respx.get(
        "http://hub.test/api/tasks",
        params={"task_type": "review"},
    ).mock(
        return_value=httpx.Response(200, json={"tasks": []})
    )
    tasks = client.tasks.list(task_type="review")
    assert tasks == []


@respx.mock
def test_tasks_get_unwraps_task_envelope(client):
    respx.get("http://hub.test/api/tasks/t-1").mock(
        return_value=httpx.Response(200, json={"task": sample_task("t-1", "open")})
    )
    task = client.tasks.get("t-1")
    assert task["id"] == "t-1"


@respx.mock
def test_tasks_get_accepts_bare_envelope(client):
    respx.get("http://hub.test/api/tasks/t-2").mock(
        return_value=httpx.Response(200, json=sample_task("t-2", "claimed"))
    )
    task = client.tasks.get("t-2")
    assert task["status"] == "claimed"


@respx.mock
def test_tasks_claim_conflict_maps_to_typed_error(client):
    respx.put("http://hub.test/api/tasks/t-9/claim").mock(
        return_value=httpx.Response(409, json={"error": "already_claimed"})
    )
    with pytest.raises(Conflict) as exc:
        client.tasks.claim("t-9", agent="a")
    assert exc.value.code == "already_claimed"


@respx.mock
def test_tasks_claim_locked_preserves_pending(client):
    respx.put("http://hub.test/api/tasks/t-9/claim").mock(
        return_value=httpx.Response(423, json={"error": "blocked", "pending": "t-1"})
    )
    with pytest.raises(Locked) as exc:
        client.tasks.claim("t-9", agent="a")
    assert exc.value.extra["pending"] == "t-1"


@respx.mock
def test_tasks_complete_sends_agent_and_output(client):
    route = respx.put("http://hub.test/api/tasks/t-1/complete").mock(
        return_value=httpx.Response(200, json={"ok": True})
    )
    client.tasks.complete("t-1", agent="a", output="done")
    wire = route.calls.last.request.read()
    assert b'"agent":"a"' in wire
    assert b'"output":"done"' in wire


@respx.mock
def test_tasks_review_result_sends_summary_hallucination(client):
    route = respx.put("http://hub.test/api/tasks/t-1/review-result").mock(
        return_value=httpx.Response(200, json={"ok": True})
    )
    client.tasks.review_result(
        "t-1", result="rejected", agent="reviewer", summary_hallucination=True
    )
    wire = route.calls.last.request.read()
    assert b'"summary_hallucination":true' in wire
    assert b'"result":"rejected"' in wire


@respx.mock
def test_tasks_review_result_omits_summary_hallucination_when_none(client):
    route = respx.put("http://hub.test/api/tasks/t-2/review-result").mock(
        return_value=httpx.Response(200, json={"ok": True})
    )
    client.tasks.review_result("t-2", result="approved")
    wire = route.calls.last.request.read()
    assert b"summary_hallucination" not in wire


@respx.mock
def test_tasks_vote_approve_sends_refinement(client):
    route = respx.put("http://hub.test/api/tasks/t-3/vote").mock(
        return_value=httpx.Response(200, json={"ok": True, "approved": True})
    )
    result = client.tasks.vote("t-3", agent="alice", vote="approve", refinement="scope A")
    assert result.get("approved") is True
    wire = route.calls.last.request.read()
    assert b'"refinement":"scope A"' in wire


@respx.mock
def test_tasks_vote_reject_without_refinement_sends_synthetic(client):
    """Omitting refinement on a reject vote must NOT drop the field; the client
    substitutes a synthetic placeholder so the server never sees a missing field."""
    route = respx.put("http://hub.test/api/tasks/t-4/vote").mock(
        return_value=httpx.Response(200, json={"ok": True})
    )
    client.tasks.vote("t-4", agent="bob", vote="reject")
    wire = route.calls.last.request.read()
    assert b'"refinement"' in wire
    assert b"(no refinement provided)" in wire


def test_tasks_vote_approve_without_refinement_raises(client):
    """Passing refinement=None for an approve vote must raise ValueError client-side
    rather than sending an incomplete request that the server rejects with HTTP 400."""
    with pytest.raises(ValueError, match="approve"):
        client.tasks.vote("t-4", agent="bob", vote="approve")


@respx.mock
def test_tasks_cancel_sends_delete(client):
    respx.delete("http://hub.test/api/tasks/t-5").mock(
        return_value=httpx.Response(200, json={"ok": True})
    )
    client.tasks.cancel("t-5")  # should not raise


@respx.mock
def test_tasks_unclaim_sends_put(client):
    route = respx.put("http://hub.test/api/tasks/t-6/unclaim").mock(
        return_value=httpx.Response(200, json={"ok": True})
    )
    client.tasks.unclaim("t-6", agent="a")
    wire = route.calls.last.request.read()
    assert b'"agent":"a"' in wire
