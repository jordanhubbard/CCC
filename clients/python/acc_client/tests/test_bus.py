"""Tests for the SSE frame parsing and BusApi in _bus.py.

Verifies that _extract_sse_data (the canonical implementation moved out of
client.py) and BusApi.stream() behave identically to the Rust
acc_client::bus equivalents.
"""
from __future__ import annotations

import httpx
import pytest
import respx

from acc_client import Client
from acc_client._bus import BusApi, _extract_sse_data


@pytest.fixture
def client():
    c = Client(base_url="http://hub.test", token="t")
    yield c
    c.close()


# ── _extract_sse_data unit tests ──────────────────────────────────────────────


def test_extract_data_returns_payload():
    assert _extract_sse_data('data: {"type":"hello"}\n') == '{"type":"hello"}'


def test_extract_data_strips_optional_leading_space():
    assert _extract_sse_data("data: abc\n") == "abc"
    assert _extract_sse_data("data:abc\n") == "abc"


def test_extract_data_joins_multiple_data_lines():
    frame = 'data: {"a":1,\ndata: "b":2}\n'
    assert _extract_sse_data(frame) == '{"a":1,\n"b":2}'


def test_extract_data_ignores_comment_lines():
    assert _extract_sse_data(": keepalive\ndata: hi\n") == "hi"


def test_extract_data_returns_none_for_keepalive_only():
    assert _extract_sse_data(": keepalive\n") is None


def test_extract_data_returns_none_for_empty():
    assert _extract_sse_data("") is None


def test_extract_data_ignores_retry_and_id_lines():
    frame = "id: 42\nretry: 3000\ndata: payload\n"
    assert _extract_sse_data(frame) == "payload"


def test_extract_data_ignores_event_line():
    frame = "event: custom\ndata: body\n"
    assert _extract_sse_data(frame) == "body"


# ── BusApi.send ───────────────────────────────────────────────────────────────


@respx.mock
def test_bus_send_uses_type_field_on_wire(client):
    route = respx.post("http://hub.test/api/bus/send").mock(
        return_value=httpx.Response(200, json={"ok": True})
    )
    client.bus.send("hello", from_="tester", body="hi")
    wire = route.calls.last.request.read()
    assert b'"type":"hello"' in wire
    assert b'"from":"boris"' not in wire  # just checking "from" key, not value
    assert b'"from":"tester"' in wire


@respx.mock
def test_bus_send_translates_from_underscore(client):
    """``from_`` must appear as ``from`` on the wire."""
    route = respx.post("http://hub.test/api/bus/send").mock(
        return_value=httpx.Response(200, json={"ok": True})
    )
    client.bus.send("ping", from_="boris")
    wire = route.calls.last.request.read()
    assert b'"from":"boris"' in wire
    assert b'"from_"' not in wire


@respx.mock
def test_bus_send_omits_none_fields(client):
    """Keyword arguments that are None must not appear on the wire."""
    route = respx.post("http://hub.test/api/bus/send").mock(
        return_value=httpx.Response(200, json={"ok": True})
    )
    client.bus.send("ping", body=None, subject=None)
    wire = route.calls.last.request.read()
    assert b"body" not in wire
    assert b"subject" not in wire


# ── BusApi.messages ───────────────────────────────────────────────────────────


@respx.mock
def test_bus_messages_filters_by_kind(client):
    respx.get("http://hub.test/api/bus/messages", params={"type": "tasks:claimed"}).mock(
        return_value=httpx.Response(
            200, json={"messages": [{"id": "m-1", "type": "tasks:claimed"}]}
        )
    )
    msgs = client.bus.messages(kind="tasks:claimed")
    assert msgs == [{"id": "m-1", "type": "tasks:claimed"}]


@respx.mock
def test_bus_messages_accepts_bare_array(client):
    respx.get("http://hub.test/api/bus/messages").mock(
        return_value=httpx.Response(200, json=[{"id": "m-2", "type": "ping"}])
    )
    msgs = client.bus.messages()
    assert len(msgs) == 1
    assert msgs[0]["id"] == "m-2"


# ── BusApi.stream ─────────────────────────────────────────────────────────────


@respx.mock
def test_bus_stream_yields_each_data_frame(client):
    body = (
        ": keepalive\n\n"
        'data: {"id":"m-1","type":"first","seq":1}\n\n'
        'data: {"id":"m-2","type":"second","seq":2}\n\n'
    )
    respx.get("http://hub.test/api/bus/stream").mock(
        return_value=httpx.Response(
            200, text=body, headers={"content-type": "text/event-stream"}
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
    body = 'data: {"type":"multi",\ndata: "id":"m-x"}\n\n'
    respx.get("http://hub.test/api/bus/stream").mock(
        return_value=httpx.Response(
            200, text=body, headers={"content-type": "text/event-stream"}
        )
    )
    msgs = list(client.bus.stream())
    assert len(msgs) == 1
    assert msgs[0]["type"] == "multi"
    assert msgs[0]["id"] == "m-x"


@respx.mock
def test_bus_stream_skips_malformed_json(client):
    body = "data: NOT_JSON\n\n" + 'data: {"type":"good"}\n\n'
    respx.get("http://hub.test/api/bus/stream").mock(
        return_value=httpx.Response(
            200, text=body, headers={"content-type": "text/event-stream"}
        )
    )
    msgs = list(client.bus.stream())
    assert len(msgs) == 1
    assert msgs[0]["type"] == "good"


@respx.mock
def test_bus_stream_raises_on_non_2xx(client):
    from acc_client import Unauthorized

    respx.get("http://hub.test/api/bus/stream").mock(
        return_value=httpx.Response(401, json={"error": "unauthorized"})
    )
    with pytest.raises(Unauthorized):
        list(client.bus.stream())


@respx.mock
def test_bus_stream_empty_body_yields_nothing(client):
    respx.get("http://hub.test/api/bus/stream").mock(
        return_value=httpx.Response(
            200, text="", headers={"content-type": "text/event-stream"}
        )
    )
    assert list(client.bus.stream()) == []


# ── BusApi is a sub-API instance on client ────────────────────────────────────


def test_bus_api_type(client):
    assert isinstance(client.bus, BusApi)
