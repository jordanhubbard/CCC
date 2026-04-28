"""Unit tests for the acc_shared_memory hermes plugin.

All tests use a lightweight fake ``acc_client`` so no live server or real
network is required.  The fixture builds the fake once per session and
injects it via ``sys.modules`` patching.
"""

from __future__ import annotations

import json
import sys
from typing import Any, Dict, List, Optional
from unittest.mock import MagicMock, patch

import pytest


# ---------------------------------------------------------------------------
# Fake acc_client — no real HTTP, no server required
# ---------------------------------------------------------------------------


class _FakeMemoryApi:
    """Minimal fake that records calls to search() / store()."""

    def __init__(self) -> None:
        self._search_calls: List[Dict[str, Any]] = []
        self._store_calls: List[Dict[str, Any]] = []
        # Pre-canned hits returned by search()
        self._hits: List[Dict[str, Any]] = []

    def search(
        self,
        query: str,
        *,
        limit: int | None = None,
        collection: str | None = None,
    ) -> List[Dict[str, Any]]:
        self._search_calls.append(
            {"query": query, "limit": limit, "collection": collection}
        )
        return self._hits

    def store(
        self,
        text: str,
        *,
        metadata: Dict[str, Any] | None = None,
        collection: str | None = None,
    ) -> None:
        self._store_calls.append(
            {"text": text, "metadata": metadata, "collection": collection}
        )


class _FakeClient:
    def __init__(self) -> None:
        self.memory = _FakeMemoryApi()
        self.base_url = "http://hub.test"
        self._closed = False

    def close(self) -> None:
        self._closed = True

    @classmethod
    def from_env(cls) -> "_FakeClient":
        return cls()


class _FakeNoToken(RuntimeError):
    pass


def _make_fake_acc_client_module(client: _FakeClient | None = None) -> MagicMock:
    """Return a MagicMock that looks like the acc_client package."""
    mod = MagicMock(name="acc_client")
    mod.NoToken = _FakeNoToken
    if client is None:
        mod.Client.from_env.side_effect = _FakeNoToken("no token")
    else:
        mod.Client.from_env.return_value = client
    return mod


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


@pytest.fixture()
def fake_client() -> _FakeClient:
    return _FakeClient()


@pytest.fixture()
def provider_with_client(fake_client: _FakeClient) -> Any:
    """Return an initialized AccSharedMemoryProvider with a fake client."""
    fake_mod = _make_fake_acc_client_module(fake_client)
    with patch.dict(sys.modules, {"acc_client": fake_mod}):
        # Import the plugin inside the patch so _try_import_client() sees the fake.
        from hermes.plugins.memory.acc_shared_memory import AccSharedMemoryProvider  # type: ignore[import]

        p = AccSharedMemoryProvider()
        p._client = fake_client  # inject directly; skips re-import in initialize()
        p._collection = "hermes"
        p._search_limit = 5
        p._session_id = "sess-1"
        p._agent_identity = "boris"
        p._platform = "cli"
        return p, fake_client


@pytest.fixture()
def provider_no_client() -> Any:
    """Return an AccSharedMemoryProvider whose _client is None (unavailable)."""
    fake_mod = _make_fake_acc_client_module(None)
    with patch.dict(sys.modules, {"acc_client": fake_mod}):
        from hermes.plugins.memory.acc_shared_memory import AccSharedMemoryProvider  # type: ignore[import]

        p = AccSharedMemoryProvider()
        p._client = None
        return p


# ---------------------------------------------------------------------------
# is_available
# ---------------------------------------------------------------------------


def test_is_available_true_when_client_connectable(fake_client: _FakeClient) -> None:
    fake_mod = _make_fake_acc_client_module(fake_client)
    with patch.dict(sys.modules, {"acc_client": fake_mod}):
        from hermes.plugins.memory.acc_shared_memory import AccSharedMemoryProvider  # type: ignore[import]

        p = AccSharedMemoryProvider()
        assert p.is_available() is True


def test_is_available_false_when_no_token() -> None:
    fake_mod = _make_fake_acc_client_module(None)  # from_env raises NoToken
    with patch.dict(sys.modules, {"acc_client": fake_mod}):
        from hermes.plugins.memory.acc_shared_memory import AccSharedMemoryProvider  # type: ignore[import]

        p = AccSharedMemoryProvider()
        assert p.is_available() is False


def test_is_available_false_when_import_error() -> None:
    """If acc_client is not installed at all, is_available() must return False."""
    with patch.dict(sys.modules, {"acc_client": None}):  # type: ignore[dict-item]
        # Force an ImportError path by removing the module entirely.
        saved = sys.modules.pop("acc_client", None)
        try:
            from hermes.plugins.memory.acc_shared_memory import AccSharedMemoryProvider  # type: ignore[import]

            p = AccSharedMemoryProvider()
            # _try_import_client raises ImportError → is_available() returns False.
            assert p.is_available() is False
        finally:
            if saved is not None:
                sys.modules["acc_client"] = saved


# ---------------------------------------------------------------------------
# prefetch
# ---------------------------------------------------------------------------


def test_prefetch_returns_formatted_hits(provider_with_client: Any) -> None:
    p, fc = provider_with_client
    fc.memory._hits = [
        {"text": "Fixed NPE in agent.rs", "score": 0.95},
        {"text": "Buffer overflow in render", "score": 0.87},
    ]
    result = p.prefetch("render pipeline error")
    assert "Fixed NPE in agent.rs" in result
    assert "Buffer overflow in render" in result
    assert "0.950" in result
    assert "0.870" in result


def test_prefetch_calls_search_with_correct_params(provider_with_client: Any) -> None:
    p, fc = provider_with_client
    fc.memory._hits = []
    p.prefetch("foo bar")
    assert len(fc.memory._search_calls) == 1
    call = fc.memory._search_calls[0]
    assert call["query"] == "foo bar"
    assert call["limit"] == 5  # provider._search_limit
    assert call["collection"] == "hermes"


def test_prefetch_returns_empty_string_on_no_hits(provider_with_client: Any) -> None:
    p, fc = provider_with_client
    fc.memory._hits = []
    assert p.prefetch("anything") == ""


def test_prefetch_returns_empty_string_when_no_client(provider_no_client: Any) -> None:
    assert provider_no_client.prefetch("anything") == ""


def test_prefetch_returns_empty_string_for_blank_query(provider_with_client: Any) -> None:
    p, fc = provider_with_client
    assert p.prefetch("") == ""
    assert len(fc.memory._search_calls) == 0


def test_prefetch_swallows_exceptions(provider_with_client: Any) -> None:
    p, fc = provider_with_client
    fc.memory.search = MagicMock(side_effect=RuntimeError("boom"))
    # Must not propagate — MemoryManager wraps prefetch but the provider
    # should handle its own errors gracefully.
    result = p.prefetch("something")
    assert result == ""


# ---------------------------------------------------------------------------
# handle_tool_call — acc_memory_search
# ---------------------------------------------------------------------------


def test_search_tool_returns_hits(provider_with_client: Any) -> None:
    p, fc = provider_with_client
    fc.memory._hits = [{"text": "hit 1", "score": 0.9}]
    raw = p.handle_tool_call("acc_memory_search", {"query": "test"})
    data = json.loads(raw)
    assert data["count"] == 1
    assert data["hits"][0]["text"] == "hit 1"


def test_search_tool_uses_custom_limit_and_collection(provider_with_client: Any) -> None:
    p, fc = provider_with_client
    fc.memory._hits = []
    p.handle_tool_call(
        "acc_memory_search",
        {"query": "q", "limit": 3, "collection": "custom"},
    )
    call = fc.memory._search_calls[-1]
    assert call["limit"] == 3
    assert call["collection"] == "custom"


def test_search_tool_missing_query_returns_error(provider_with_client: Any) -> None:
    p, _ = provider_with_client
    raw = p.handle_tool_call("acc_memory_search", {})
    data = json.loads(raw)
    assert "error" in data


def test_search_tool_no_client_returns_error(provider_no_client: Any) -> None:
    raw = provider_no_client.handle_tool_call("acc_memory_search", {"query": "x"})
    data = json.loads(raw)
    assert "error" in data


# ---------------------------------------------------------------------------
# handle_tool_call — acc_memory_store
# ---------------------------------------------------------------------------


def test_store_tool_calls_client_store(provider_with_client: Any) -> None:
    p, fc = provider_with_client
    raw = p.handle_tool_call(
        "acc_memory_store",
        {"text": "Remember this", "metadata": {"task_id": "t-1"}},
    )
    data = json.loads(raw)
    assert data["ok"] is True
    assert data["stored"] == len("Remember this")
    assert len(fc.memory._store_calls) == 1
    call = fc.memory._store_calls[0]
    assert call["text"] == "Remember this"
    assert call["metadata"]["task_id"] == "t-1"


def test_store_tool_stamps_provenance_metadata(provider_with_client: Any) -> None:
    p, fc = provider_with_client
    p.handle_tool_call("acc_memory_store", {"text": "some text"})
    meta = fc.memory._store_calls[-1]["metadata"]
    assert meta["agent"] == "boris"
    assert meta["session_id"] == "sess-1"
    assert meta["platform"] == "cli"


def test_store_tool_caller_metadata_is_not_overwritten(provider_with_client: Any) -> None:
    p, fc = provider_with_client
    p.handle_tool_call(
        "acc_memory_store",
        {"text": "t", "metadata": {"agent": "natasha"}},
    )
    meta = fc.memory._store_calls[-1]["metadata"]
    # Caller-supplied agent must NOT be overwritten by setdefault.
    assert meta["agent"] == "natasha"


def test_store_tool_missing_text_returns_error(provider_with_client: Any) -> None:
    p, _ = provider_with_client
    raw = p.handle_tool_call("acc_memory_store", {})
    data = json.loads(raw)
    assert "error" in data


def test_store_tool_no_client_returns_error(provider_no_client: Any) -> None:
    raw = provider_no_client.handle_tool_call("acc_memory_store", {"text": "x"})
    data = json.loads(raw)
    assert "error" in data


def test_unknown_tool_returns_error(provider_with_client: Any) -> None:
    p, _ = provider_with_client
    raw = p.handle_tool_call("not_a_real_tool", {})
    data = json.loads(raw)
    assert "error" in data


# ---------------------------------------------------------------------------
# on_memory_write
# ---------------------------------------------------------------------------


def test_on_memory_write_add_pushes_to_hub(provider_with_client: Any) -> None:
    p, fc = provider_with_client
    p.on_memory_write("add", "memory", "I learned X")
    assert len(fc.memory._store_calls) == 1
    call = fc.memory._store_calls[0]
    assert call["text"] == "I learned X"
    assert call["metadata"]["source"] == "builtin_memory_write"
    assert call["metadata"]["action"] == "add"


def test_on_memory_write_replace_pushes_to_hub(provider_with_client: Any) -> None:
    p, fc = provider_with_client
    p.on_memory_write("replace", "user", "Updated profile")
    assert len(fc.memory._store_calls) == 1


def test_on_memory_write_remove_does_not_push(provider_with_client: Any) -> None:
    p, fc = provider_with_client
    p.on_memory_write("remove", "memory", "old entry")
    assert len(fc.memory._store_calls) == 0


def test_on_memory_write_no_client_is_silent(provider_no_client: Any) -> None:
    # Must not raise even with no client.
    provider_no_client.on_memory_write("add", "memory", "something")


def test_on_memory_write_swallows_exceptions(provider_with_client: Any) -> None:
    p, fc = provider_with_client
    fc.memory.store = MagicMock(side_effect=RuntimeError("network down"))
    # Must not propagate — local write must never be blocked by hub failure.
    p.on_memory_write("add", "memory", "important")


# ---------------------------------------------------------------------------
# shutdown / on_session_end
# ---------------------------------------------------------------------------


def test_shutdown_closes_client(provider_with_client: Any) -> None:
    p, fc = provider_with_client
    p.shutdown()
    assert fc._closed is True
    assert p._client is None


def test_on_session_end_closes_client(provider_with_client: Any) -> None:
    p, fc = provider_with_client
    p.on_session_end([])
    assert fc._closed is True


# ---------------------------------------------------------------------------
# system_prompt_block
# ---------------------------------------------------------------------------


def test_system_prompt_block_non_empty_when_client_present(
    provider_with_client: Any,
) -> None:
    p, _ = provider_with_client
    block = p.system_prompt_block()
    assert "acc_shared_memory" in block
    assert len(block) > 0


def test_system_prompt_block_empty_when_no_client(provider_no_client: Any) -> None:
    assert provider_no_client.system_prompt_block() == ""


# ---------------------------------------------------------------------------
# get_tool_schemas
# ---------------------------------------------------------------------------


def test_get_tool_schemas_returns_two_tools(provider_with_client: Any) -> None:
    p, _ = provider_with_client
    schemas = p.get_tool_schemas()
    names = {s["name"] for s in schemas}
    assert "acc_memory_search" in names
    assert "acc_memory_store" in names


def test_tool_schemas_have_input_schema(provider_with_client: Any) -> None:
    p, _ = provider_with_client
    for schema in p.get_tool_schemas():
        assert "input_schema" in schema
        assert schema["input_schema"]["type"] == "object"


# ---------------------------------------------------------------------------
# register() entry point
# ---------------------------------------------------------------------------


def test_register_calls_register_memory_provider(fake_client: _FakeClient) -> None:
    fake_mod = _make_fake_acc_client_module(fake_client)
    with patch.dict(sys.modules, {"acc_client": fake_mod}):
        from hermes.plugins.memory.acc_shared_memory import (  # type: ignore[import]
            register,
            AccSharedMemoryProvider,
        )

        collector = MagicMock()
        register(collector)
        collector.register_memory_provider.assert_called_once()
        arg = collector.register_memory_provider.call_args[0][0]
        assert isinstance(arg, AccSharedMemoryProvider)


# ---------------------------------------------------------------------------
# get_config_schema
# ---------------------------------------------------------------------------


def test_config_schema_contains_acc_token(provider_with_client: Any) -> None:
    p, _ = provider_with_client
    schema = p.get_config_schema()
    keys = [f["key"] for f in schema]
    assert "ACC_TOKEN" in keys


def test_config_schema_acc_token_is_secret(provider_with_client: Any) -> None:
    p, _ = provider_with_client
    for field in p.get_config_schema():
        if field["key"] == "ACC_TOKEN":
            assert field["secret"] is True
            assert field["required"] is True
            break
