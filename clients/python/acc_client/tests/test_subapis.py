"""Tests for AgentsApi, ProjectsApi, QueueApi, ItemsApi, MemoryApi.

Mirrors the Rust acc-client tests in tests/registry_integration.rs and
tests/queue_integration.rs.
"""
from __future__ import annotations

import httpx
import pytest
import respx

from acc_client import Client, Conflict
from acc_client._agents import AgentsApi
from acc_client._items import ItemsApi
from acc_client._memory import MemoryApi
from acc_client._projects import ProjectsApi
from acc_client._queue import QueueApi


@pytest.fixture
def client():
    c = Client(base_url="http://hub.test", token="t")
    yield c
    c.close()


# ── Type checks ───────────────────────────────────────────────────────────────


def test_sub_api_types(client):
    assert isinstance(client.agents, AgentsApi)
    assert isinstance(client.projects, ProjectsApi)
    assert isinstance(client.queue, QueueApi)
    assert isinstance(client.items, ItemsApi)
    assert isinstance(client.memory, MemoryApi)


# ── AgentsApi ─────────────────────────────────────────────────────────────────


@respx.mock
def test_agents_list_filters_by_online(client):
    respx.get("http://hub.test/api/agents", params={"online": "true"}).mock(
        return_value=httpx.Response(
            200,
            json={
                "agents": [
                    {"name": "natasha", "online": True, "gpu_temp_c": 48.0},
                    {"name": "boris", "online": True},
                ]
            },
        )
    )
    agents = client.agents.list(online=True)
    assert len(agents) == 2
    natasha = next(a for a in agents if a["name"] == "natasha")
    assert natasha.get("gpu_temp_c") == 48.0


@respx.mock
def test_agents_list_accepts_bare_array(client):
    respx.get("http://hub.test/api/agents").mock(
        return_value=httpx.Response(200, json=[{"name": "a"}])
    )
    assert client.agents.list() == [{"name": "a"}]


@respx.mock
def test_agents_names_online(client):
    respx.get("http://hub.test/api/agents/names", params={"online": "true"}).mock(
        return_value=httpx.Response(200, json={"names": ["natasha", "boris"]})
    )
    names = client.agents.names(online=True)
    assert names == ["natasha", "boris"]


@respx.mock
def test_agents_names_bare_array(client):
    respx.get("http://hub.test/api/agents/names").mock(
        return_value=httpx.Response(200, json=["natasha"])
    )
    assert client.agents.names() == ["natasha"]


@respx.mock
def test_agents_get_unwraps_agent_envelope(client):
    respx.get("http://hub.test/api/agents/natasha").mock(
        return_value=httpx.Response(200, json={"agent": {"name": "natasha", "online": True}})
    )
    a = client.agents.get("natasha")
    assert a["name"] == "natasha"


# ── ProjectsApi ───────────────────────────────────────────────────────────────


@respx.mock
def test_projects_list_wrapped_with_total(client):
    respx.get("http://hub.test/api/projects").mock(
        return_value=httpx.Response(
            200,
            json={
                "projects": [{"id": "p-1", "name": "demo", "status": "active"}],
                "total": 1,
                "offset": 0,
            },
        )
    )
    projects = client.projects.list()
    assert len(projects) == 1
    assert projects[0]["name"] == "demo"


@respx.mock
def test_projects_create_handles_ok_envelope(client):
    respx.post("http://hub.test/api/projects").mock(
        return_value=httpx.Response(
            200,
            json={"ok": True, "project": {"id": "p-2", "name": "new", "status": "active"}},
        )
    )
    p = client.projects.create(name="new")
    assert p["id"] == "p-2"


@respx.mock
def test_projects_delete_soft(client):
    respx.delete("http://hub.test/api/projects/p-1").mock(
        return_value=httpx.Response(200, json={"ok": True})
    )
    client.projects.delete("p-1")  # should not raise


@respx.mock
def test_projects_delete_hard_sends_query_param(client):
    route = respx.delete("http://hub.test/api/projects/p-1").mock(
        return_value=httpx.Response(200, json={"ok": True})
    )
    client.projects.delete("p-1", hard=True)
    assert "hard=true" in str(route.calls.last.request.url)


# ── QueueApi ─────────────────────────────────────────────────────────────────


@respx.mock
def test_queue_list_accepts_bare_array(client):
    respx.get("http://hub.test/api/queue").mock(
        return_value=httpx.Response(
            200,
            json=[
                {"id": "wq-1", "status": "pending", "title": "item one"},
                {"id": "wq-2", "status": "pending"},
            ],
        )
    )
    items = client.queue.list()
    assert len(items) == 2
    assert items[0]["id"] == "wq-1"


@respx.mock
def test_queue_list_accepts_wrapped_envelope(client):
    respx.get("http://hub.test/api/queue").mock(
        return_value=httpx.Response(
            200, json={"items": [{"id": "wq-1", "status": "in-progress"}]}
        )
    )
    items = client.queue.list()
    assert items[0]["status"] == "in-progress"


@respx.mock
def test_queue_get_unwraps_item_envelope(client):
    respx.get("http://hub.test/api/item/wq-1").mock(
        return_value=httpx.Response(200, json={"item": {"id": "wq-1", "status": "pending"}})
    )
    item = client.queue.get("wq-1")
    assert item["id"] == "wq-1"


# ── ItemsApi ─────────────────────────────────────────────────────────────────


@respx.mock
def test_items_claim_conflict_maps_to_typed_error(client):
    respx.post("http://hub.test/api/item/wq-9/claim").mock(
        return_value=httpx.Response(409, json={"error": "already_claimed"})
    )
    with pytest.raises(Conflict) as exc:
        client.items.claim("wq-9", "a")
    assert exc.value.code == "already_claimed"


@respx.mock
def test_items_complete_sends_result_and_resolution(client):
    route = respx.post("http://hub.test/api/item/wq-1/complete").mock(
        return_value=httpx.Response(200, json={"ok": True})
    )
    client.items.complete("wq-1", "a", result="done", resolution="fixed")
    wire = route.calls.last.request.read()
    assert b'"result":"done"' in wire
    assert b'"resolution":"fixed"' in wire


@respx.mock
def test_items_fail_sends_reason(client):
    route = respx.post("http://hub.test/api/item/wq-1/fail").mock(
        return_value=httpx.Response(200, json={"ok": True})
    )
    client.items.fail("wq-1", "a", "timeout")
    wire = route.calls.last.request.read()
    assert b'"reason":"timeout"' in wire


@respx.mock
def test_items_keepalive_with_note(client):
    route = respx.post("http://hub.test/api/item/wq-1/keepalive").mock(
        return_value=httpx.Response(200, json={"ok": True})
    )
    client.items.keepalive("wq-1", "a", note="still working")
    wire = route.calls.last.request.read()
    assert b'"note":"still working"' in wire


@respx.mock
def test_items_heartbeat_posts_to_named_agent(client):
    route = respx.post("http://hub.test/api/heartbeat/boris").mock(
        return_value=httpx.Response(200, json={"ok": True})
    )
    client.items.heartbeat("boris", status="ok", note="cycle 1")
    assert route.calls.call_count == 1
    wire = route.calls.last.request.read()
    assert b'"status":"ok"' in wire


# ── MemoryApi ─────────────────────────────────────────────────────────────────


@respx.mock
def test_memory_search_accepts_results_envelope(client):
    respx.post("http://hub.test/api/memory/search").mock(
        return_value=httpx.Response(
            200,
            json={"results": [{"text": "hit 1", "score": 0.9}, {"text": "hit 2", "score": 0.8}]},
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
def test_memory_search_accepts_bare_array(client):
    respx.post("http://hub.test/api/memory/search").mock(
        return_value=httpx.Response(200, json=[{"text": "bare"}])
    )
    assert client.memory.search("q") == [{"text": "bare"}]


@respx.mock
def test_memory_store_sends_metadata(client):
    route = respx.post("http://hub.test/api/memory/store").mock(
        return_value=httpx.Response(200, json={"ok": True})
    )
    client.memory.store("some text", metadata={"agent": "boris", "tags": ["done"]})
    wire = route.calls.last.request.read()
    assert b'"text":"some text"' in wire
    assert b'"agent":"boris"' in wire


@respx.mock
def test_memory_store_without_metadata(client):
    route = respx.post("http://hub.test/api/memory/store").mock(
        return_value=httpx.Response(200, json={"ok": True})
    )
    client.memory.store("bare text")
    wire = route.calls.last.request.read()
    assert b"metadata" not in wire


@respx.mock
def test_memory_store_with_collection(client):
    route = respx.post("http://hub.test/api/memory/store").mock(
        return_value=httpx.Response(200, json={"ok": True})
    )
    client.memory.store("text", collection="custom")
    wire = route.calls.last.request.read()
    assert b'"collection":"custom"' in wire
