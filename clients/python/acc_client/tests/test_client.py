"""Integration tests against a mocked HTTP transport."""
from __future__ import annotations

import httpx
import pytest
import respx

from acc_client import (
    ApiError,
    Client,
    Conflict,
    Locked,
    NotFound,
    Unauthorized,
)
from acc_client._bus import _extract_sse_data  # canonical location post-refactor


@pytest.fixture
def client():
    c = Client(base_url="http://hub.test", token="t")
    yield c
    c.close()


# ── tasks ─────────────────────────────────────────────────────────────


@respx.mock
def test_tasks_list_filters_by_status(client):
    respx.get("http://hub.test/api/tasks", params={"status": "open", "limit": 5}).mock(
        return_value=httpx.Response(200, json={"tasks": [{"id": "t-1"}], "count": 1})
    )
    tasks = client.tasks.list(status="open", limit=5)
    assert tasks == [{"id": "t-1"}]


@respx.mock
def test_tasks_get_unwraps_task_envelope(client):
    respx.get("http://hub.test/api/tasks/t-1").mock(
        return_value=httpx.Response(200, json={"task": {"id": "t-1", "title": "x"}})
    )
    assert client.tasks.get("t-1")["title"] == "x"


@respx.mock
def test_tasks_claim_409_raises_conflict(client):
    respx.put("http://hub.test/api/tasks/t-9/claim").mock(
        return_value=httpx.Response(409, json={"error": "already_claimed"})
    )
    with pytest.raises(Conflict) as exc:
        client.tasks.claim("t-9", agent="a")
    assert exc.value.code == "already_claimed"
    assert exc.value.status == 409


@respx.mock
def test_tasks_claim_423_preserves_pending_field(client):
    respx.put("http://hub.test/api/tasks/t-9/claim").mock(
        return_value=httpx.Response(
            423, json={"error": "blocked", "pending": "t-1"}
        )
    )
    with pytest.raises(Locked) as exc:
        client.tasks.claim("t-9", agent="a")
    assert exc.value.extra["pending"] == "t-1"


@respx.mock
def test_tasks_complete_sends_body(client):
    route = respx.put("http://hub.test/api/tasks/t-1/complete").mock(
        return_value=httpx.Response(200, json={"ok": True})
    )
    client.tasks.complete("t-1", agent="a", output="done")
    assert route.calls.last.request.read() == b'{"agent":"a","output":"done"}'


@respx.mock
def test_non_json_error_body_still_maps(client):
    respx.put("http://hub.test/api/tasks/t-1/complete").mock(
        return_value=httpx.Response(500, text="internal error")
    )
    with pytest.raises(ApiError) as exc:
        client.tasks.complete("t-1", agent="a")
    assert exc.value.status == 500
    assert exc.value.code == "http_500"


# ── memory (the hermes plugin's path) ─────────────────────────────────


@respx.mock
def test_memory_search_accepts_results_envelope(client):
    respx.post("http://hub.test/api/memory/search").mock(
        return_value=httpx.Response(
            200,
            json={
                "results": [
                    {"text": "hit 1", "score": 0.9},
                    {"text": "hit 2", "score": 0.8},
                ]
            },
        )
    )
    hits = client.memory.search("buffer overflow", limit=10, collection="acc_memory")
    assert len(hits) == 2
    assert hits[0]["score"] == 0.9


@respx.mock
def test_memory_search_accepts_hits_envelope(client):
    respx.post("http://hub.test/api/memory/search").mock(
        return_value=httpx.Response(200, json={"hits": [{"text": "h"}]})
    )
    assert client.memory.search("q") == [{"text": "h"}]


@respx.mock
def test_memory_store_sends_metadata(client):
    route = respx.post("http://hub.test/api/memory/store").mock(
        return_value=httpx.Response(200, json={"ok": True})
    )
    client.memory.store("some text", metadata={"agent": "boris", "tags": ["done"]})
    body = route.calls.last.request.read()
    assert b'"text":"some text"' in body
    assert b'"agent":"boris"' in body


# ── items / heartbeat ─────────────────────────────────────────────────


@respx.mock
def test_item_claim_409_raises_conflict(client):
    respx.post("http://hub.test/api/item/wq-9/claim").mock(
        return_value=httpx.Response(409, json={"error": "already_claimed"})
    )
    with pytest.raises(Conflict):
        client.items.claim("wq-9", agent="a")


@respx.mock
def test_heartbeat_posts_to_named_agent(client):
    route = respx.post("http://hub.test/api/heartbeat/boris").mock(
        return_value=httpx.Response(200, json={"ok": True})
    )
    client.items.heartbeat("boris", status="ok", note="cycle 1")
    assert route.calls.call_count == 1


# ── bus ───────────────────────────────────────────────────────────────


@respx.mock
def test_bus_send_uses_type_field_on_wire(client):
    route = respx.post("http://hub.test/api/bus/send").mock(
        return_value=httpx.Response(200, json={"ok": True})
    )
    client.bus.send("hello", from_="tester", body="hi")
    body = route.calls.last.request.read()
    assert b'"type":"hello"' in body


@respx.mock
def test_bus_send_maps_from_underscore_to_wire_from(client):
    """``from_`` kwarg must appear as ``from`` on the wire (not ``from_``)."""
    route = respx.post("http://hub.test/api/bus/send").mock(
        return_value=httpx.Response(200, json={"ok": True})
    )
    client.bus.send("ping", from_="boris")
    body = route.calls.last.request.read()
    assert b'"from":"boris"' in body
    assert b'"from_"' not in body


@respx.mock
def test_bus_messages_filters_by_kind(client):
    respx.get("http://hub.test/api/bus/messages", params={"type": "tasks:claimed"}).mock(
        return_value=httpx.Response(
            200,
            json={"messages": [{"id": "m-1", "type": "tasks:claimed"}]},
        )
    )
    msgs = client.bus.messages(kind="tasks:claimed")
    assert msgs == [{"id": "m-1", "type": "tasks:claimed"}]


# ── misc ──────────────────────────────────────────────────────────────


@respx.mock
def test_404_raises_notfound(client):
    respx.get("http://hub.test/api/tasks/nope").mock(
        return_value=httpx.Response(404, json={"error": "not_found"})
    )
    with pytest.raises(NotFound):
        client.tasks.get("nope")


@respx.mock
def test_401_raises_unauthorized(client):
    respx.get("http://hub.test/api/tasks").mock(
        return_value=httpx.Response(401, json={"error": "unauthorized"})
    )
    with pytest.raises(Unauthorized):
        client.tasks.list()


def test_context_manager_closes_http(monkeypatch):
    monkeypatch.setenv("ACC_TOKEN", "t")
    with Client(base_url="http://hub.test") as c:
        assert c.base_url == "http://hub.test"


# ── SSE _extract_sse_data unit tests ──────────────────────────────────────────


def test_extract_sse_data_returns_payload():
    frame = 'data: {"type":"hello"}\n'
    assert _extract_sse_data(frame) == '{"type":"hello"}'


def test_extract_sse_data_strips_optional_leading_space():
    # "data: " (with space) and "data:" (without) both valid per SSE spec.
    assert _extract_sse_data("data: abc\n") == "abc"
    assert _extract_sse_data("data:abc\n") == "abc"


def test_extract_sse_data_joins_multiple_data_lines():
    frame = 'data: {"a":1,\ndata: "b":2}\n'
    assert _extract_sse_data(frame) == '{"a":1,\n"b":2}'


def test_extract_sse_data_ignores_comment_and_meta_lines():
    frame = ": keepalive\nevent: msg\ndata: hi\n"
    assert _extract_sse_data(frame) == "hi"


def test_extract_sse_data_returns_none_for_keepalive_only():
    frame = ": keepalive\n"
    assert _extract_sse_data(frame) is None


def test_extract_sse_data_returns_none_for_empty():
    assert _extract_sse_data("") is None


def test_extract_sse_data_ignores_retry_and_id_lines():
    frame = "id: 42\nretry: 3000\ndata: payload\n"
    assert _extract_sse_data(frame) == "payload"


# ── bus.stream() integration tests ────────────────────────────────────────────


@respx.mock
def test_bus_stream_yields_each_data_frame(client):
    """Three frames in one body; keep-alive comment frame is silently dropped."""
    body = (
        ": keepalive\n\n"
        'data: {"id":"m-1","type":"first","seq":1}\n\n'
        'data: {"id":"m-2","type":"second","seq":2}\n\n'
    )
    respx.get("http://hub.test/api/bus/stream").mock(
        return_value=httpx.Response(
            200,
            text=body,
            headers={"content-type": "text/event-stream"},
        )
    )
    msgs = list(client.bus.stream())
    assert len(msgs) == 2
    assert msgs[0]["type"] == "first"
    assert msgs[0]["id"] == "m-1"
    assert msgs[1]["type"] == "second"
    assert msgs[1]["seq"] == 2


@respx.mock
def test_bus_stream_joins_multiline_data_fields(client):
    """Multiple data: lines in one frame are joined with \\n before JSON decode."""
    body = 'data: {"type":"multi",\ndata: "id":"m-x"}\n\n'
    respx.get("http://hub.test/api/bus/stream").mock(
        return_value=httpx.Response(
            200,
            text=body,
            headers={"content-type": "text/event-stream"},
        )
    )
    msgs = list(client.bus.stream())
    assert len(msgs) == 1
    assert msgs[0]["type"] == "multi"
    assert msgs[0]["id"] == "m-x"


@respx.mock
def test_bus_stream_skips_malformed_json_frame(client):
    """A garbled frame is silently skipped; valid frames still yield."""
    body = (
        "data: NOT_JSON\n\n"
        'data: {"type":"good"}\n\n'
    )
    respx.get("http://hub.test/api/bus/stream").mock(
        return_value=httpx.Response(
            200,
            text=body,
            headers={"content-type": "text/event-stream"},
        )
    )
    msgs = list(client.bus.stream())
    assert len(msgs) == 1
    assert msgs[0]["type"] == "good"


@respx.mock
def test_bus_stream_raises_on_non_2xx(client):
    """A non-2xx status before the stream body raises ApiError immediately."""
    respx.get("http://hub.test/api/bus/stream").mock(
        return_value=httpx.Response(401, json={"error": "unauthorized"})
    )
    with pytest.raises(Unauthorized):
        list(client.bus.stream())


@respx.mock
def test_bus_stream_empty_body_yields_nothing(client):
    """Server that sends no frames (immediate close) yields an empty sequence."""
    respx.get("http://hub.test/api/bus/stream").mock(
        return_value=httpx.Response(
            200,
            text="",
            headers={"content-type": "text/event-stream"},
        )
    )
    assert list(client.bus.stream()) == []


# ── tasks.review_result — summary_hallucination flag ─────────────────────────


@respx.mock
def test_review_result_sends_summary_hallucination_flag(client):
    """summary_hallucination=True is forwarded to the server as a boolean field."""
    route = respx.put("http://hub.test/api/tasks/t-1/review-result").mock(
        return_value=httpx.Response(200, json={"ok": True})
    )
    client.tasks.review_result(
        "t-1",
        result="rejected",
        agent="reviewer",
        summary_hallucination=True,
    )
    body = route.calls.last.request.read()
    assert b'"summary_hallucination":true' in body
    assert b'"result":"rejected"' in body
    assert b'"agent":"reviewer"' in body


@respx.mock
def test_review_result_omits_summary_hallucination_when_none(client):
    """summary_hallucination must not appear in the body when not set."""
    route = respx.put("http://hub.test/api/tasks/t-2/review-result").mock(
        return_value=httpx.Response(200, json={"ok": True})
    )
    client.tasks.review_result("t-2", result="approved")
    body = route.calls.last.request.read()
    assert b"summary_hallucination" not in body


# ── tasks.vote ────────────────────────────────────────────────────────────────


@respx.mock
def test_vote_approve_sends_refinement(client):
    """An approve vote must carry a non-empty refinement string."""
    route = respx.put("http://hub.test/api/tasks/t-3/vote").mock(
        return_value=httpx.Response(200, json={"ok": True, "approved": True})
    )
    result = client.tasks.vote("t-3", agent="alice", vote="approve", refinement="scope A only")
    assert result.get("approved") is True
    body = route.calls.last.request.read()
    assert b'"vote":"approve"' in body
    assert b'"refinement":"scope A only"' in body
    assert b'"agent":"alice"' in body


@respx.mock
def test_vote_reject_without_refinement(client):
    """A reject vote omits the caller's refinement argument but the client still
    sends the field with a synthetic placeholder so the server never receives a
    missing ``refinement`` key and returns HTTP 400."""
    route = respx.put("http://hub.test/api/tasks/t-4/vote").mock(
        return_value=httpx.Response(200, json={"ok": True})
    )
    client.tasks.vote("t-4", agent="bob", vote="reject")
    body = route.calls.last.request.read()
    assert b'"vote":"reject"' in body
    # The client substitutes "(no refinement provided)" — field must be present.
    assert b'"refinement"' in body
    assert b"(no refinement provided)" in body


@respx.mock
def test_vote_empty_response_returns_empty_dict(client):
    """204 No Content (empty body) must not crash the caller."""
    respx.put("http://hub.test/api/tasks/t-5/vote").mock(
        return_value=httpx.Response(204, content=b"")
    )
    result = client.tasks.vote("t-5", agent="carol", vote="reject")
    assert result == {} or result is None  # either is acceptable
