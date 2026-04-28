"""acc_shared_memory — hermes memory provider backed by the ACC hub.

Routes hermes memory operations (search, store, recall, mirror) to the ACC
hub's ``/api/memory/search`` and ``/api/memory/store`` endpoints via the
``acc_client`` Python package (``clients/python/acc_client/``).

This is the Python counterpart of the Rust ``acc_client::memory`` module and
the primary consumer that drove the memory API shape decisions in the Epic.

Configuration (any of these will activate the plugin):

    ACC_TOKEN=<bearer-token>
    ACC_HUB_URL=http://<hub-host>:8789   # optional, defaults to localhost:8789

or add them to ``~/.acc/.env``.

Plugin YAML key for ``memory.provider``: ``acc_shared_memory``

Example ``config.yaml``::

    memory:
      provider: acc_shared_memory
      collection: hermes_memory   # optional; defaults to "hermes"
      search_limit: 10            # optional; how many hits to recall per turn
"""

from __future__ import annotations

import json
import logging
from typing import Any, Dict, List, Optional

logger = logging.getLogger(__name__)


# ---------------------------------------------------------------------------
# Lazy import helpers — acc_client is an optional dep; we gate is_available()
# on whether it can be imported rather than hard-failing at import time.
# ---------------------------------------------------------------------------


def _try_import_client() -> Optional[Any]:
    """Return an acc_client.Client instance, or None if unavailable."""
    try:
        from acc_client import Client, NoToken  # type: ignore[import]

        try:
            return Client.from_env()
        except NoToken:
            return None
        except Exception as exc:
            logger.debug("acc_client.Client.from_env() failed: %s", exc)
            return None
    except ImportError:
        return None


# ---------------------------------------------------------------------------
# The provider class
# ---------------------------------------------------------------------------


class AccSharedMemoryProvider:
    """Hermes MemoryProvider that stores and retrieves memories via the ACC hub.

    Registered under the name ``acc_shared_memory``.  Implement the
    :class:`~agent.memory_provider.MemoryProvider` interface so MemoryManager
    can drive it without importing that ABC at module load time (which would
    force hermes to have the ABC in its import path even when the provider is
    not activated).

    Wire format
    -----------
    Every call goes through the ``acc_client`` Python package which is a
    direct mirror of the Rust ``acc-client`` crate.  Both use the same
    ``acc-model`` wire types:

    * **search** → ``POST /api/memory/search``  body: ``MemorySearchRequest``
      → response: ``[MemoryHit]``
    * **store**  → ``POST /api/memory/store``   body: ``MemoryStoreRequest``
    """

    # ------------------------------------------------------------------
    # MemoryProvider identity
    # ------------------------------------------------------------------

    @property
    def name(self) -> str:
        return "acc_shared_memory"

    # ------------------------------------------------------------------
    # MemoryProvider lifecycle
    # ------------------------------------------------------------------

    def __init__(self) -> None:
        self._client: Optional[Any] = None
        self._collection: str = "hermes"
        self._search_limit: int = 10
        self._session_id: str = ""
        self._agent_identity: str = ""
        self._platform: str = "cli"

    def is_available(self) -> bool:
        """Return True if ``acc_client`` is importable and credentials are set."""
        client = _try_import_client()
        return client is not None

    def initialize(self, session_id: str, **kwargs: Any) -> None:
        """Connect to the ACC hub and persist session metadata for recall tagging."""
        self._session_id = session_id
        self._platform = kwargs.get("platform", "cli")
        self._agent_identity = kwargs.get("agent_identity", "")

        # Allow collection / limit overrides via init kwargs (set by
        # run_agent.py from memory.collection / memory.search_limit config).
        if "collection" in kwargs:
            self._collection = kwargs["collection"]
        if "search_limit" in kwargs:
            try:
                self._search_limit = int(kwargs["search_limit"])
            except (TypeError, ValueError):
                pass

        self._client = _try_import_client()
        if self._client is None:
            logger.warning(
                "acc_shared_memory: could not connect to ACC hub — "
                "set ACC_TOKEN (and optionally ACC_HUB_URL) in your environment "
                "or in ~/.acc/.env"
            )
        else:
            logger.info(
                "acc_shared_memory: connected to %s (collection=%s, limit=%d)",
                self._client.base_url,
                self._collection,
                self._search_limit,
            )

    def system_prompt_block(self) -> str:
        """Return a brief system-prompt note about the active memory backend."""
        if self._client is None:
            return ""
        return (
            "\n<memory-provider: acc_shared_memory>\n"
            "You have access to shared fleet memory stored on the ACC hub.  "
            "Use the `acc_memory_search` tool to recall relevant context before "
            "answering, and `acc_memory_store` to record important findings.\n"
            "</memory-provider>\n"
        )

    def prefetch(self, query: str) -> str:
        """Search the ACC hub for memories relevant to *query*.

        Called by MemoryManager before each turn.  Returns a string block
        suitable for inclusion in the context window, or an empty string if
        nothing useful was found.
        """
        if not self._client or not query:
            return ""
        try:
            hits = self._client.memory.search(
                query,
                limit=self._search_limit,
                collection=self._collection,
            )
            if not hits:
                return ""
            lines = ["[Recalled from ACC shared memory]"]
            for hit in hits:
                text = hit.get("text") or ""
                score = hit.get("score")
                if text:
                    score_str = f"  (score={score:.3f})" if score is not None else ""
                    lines.append(f"• {text}{score_str}")
            return "\n".join(lines)
        except Exception as exc:
            logger.debug("acc_shared_memory prefetch error: %s", exc)
            return ""

    def sync_turn(self, user_msg: str, assistant_msg: str) -> None:
        """No automatic turn-by-turn writes.

        Writes are explicit via tool calls so the agent (and user) are in
        control of what gets stored in shared fleet memory.  This avoids
        polluting the hub with low-value conversational noise.
        """

    def get_tool_schemas(self) -> List[Dict[str, Any]]:
        """Expose search and store as tools the model can call."""
        return [
            {
                "name": "acc_memory_search",
                "description": (
                    "Search the ACC fleet's shared memory for relevant context.  "
                    "Use this before answering questions about past work, agent "
                    "observations, or fleet state.  Returns a list of matching "
                    "memory entries with relevance scores."
                ),
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Natural-language search query.",
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum number of results to return (default: 10).",
                            "default": 10,
                        },
                        "collection": {
                            "type": "string",
                            "description": (
                                "Memory collection to search (default: the provider's "
                                "configured collection, usually \"hermes\")."
                            ),
                        },
                    },
                    "required": ["query"],
                },
            },
            {
                "name": "acc_memory_store",
                "description": (
                    "Store a memory entry in the ACC fleet's shared memory.  "
                    "Use this to persist important findings, observations, or "
                    "decisions so other agents and future sessions can recall them."
                ),
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "text": {
                            "type": "string",
                            "description": "The memory text to store.",
                        },
                        "metadata": {
                            "type": "object",
                            "description": (
                                "Optional key/value metadata attached to the entry "
                                "(e.g. {\"agent\": \"boris\", \"task_id\": \"t-42\"})."
                            ),
                        },
                        "collection": {
                            "type": "string",
                            "description": (
                                "Target collection (default: the provider's configured "
                                "collection, usually \"hermes\")."
                            ),
                        },
                    },
                    "required": ["text"],
                },
            },
        ]

    def handle_tool_call(self, tool_name: str, tool_input: Dict[str, Any]) -> str:
        """Dispatch a tool call to the ACC hub.

        Returns a JSON string as required by the MemoryProvider contract.
        """
        if tool_name == "acc_memory_search":
            return self._handle_search(tool_input)
        if tool_name == "acc_memory_store":
            return self._handle_store(tool_input)
        return json.dumps({"error": f"Unknown tool: {tool_name}"})

    # ------------------------------------------------------------------
    # Optional hooks
    # ------------------------------------------------------------------

    def on_memory_write(self, action: str, target: str, content: str) -> None:
        """Mirror built-in memory writes to the ACC hub.

        When the agent writes to its local MEMORY.md / USER.md via the
        built-in memory tool, we also push that content to the ACC hub so
        the fleet can benefit from locally-derived insights.

        The ``action`` is ``"add"``, ``"replace"``, or ``"remove"``.  We
        only push ``"add"`` and ``"replace"`` writes; removals are not
        propagated because the hub has its own retention policy.
        """
        if action not in ("add", "replace"):
            return
        if not self._client or not content:
            return
        try:
            metadata: Dict[str, Any] = {
                "source": "builtin_memory_write",
                "action": action,
                "target": target,
                "agent": self._agent_identity or "hermes",
                "session_id": self._session_id,
                "platform": self._platform,
            }
            self._client.memory.store(
                content,
                metadata=metadata,
                collection=self._collection,
            )
            logger.debug(
                "acc_shared_memory: mirrored built-in %s write to hub (%d chars)",
                action,
                len(content),
            )
        except Exception as exc:
            # Never let mirroring block the local write.
            logger.debug("acc_shared_memory on_memory_write error: %s", exc)

    def on_session_end(self, messages: List[Dict[str, Any]]) -> None:
        """Close the acc_client HTTP connection pool on session end."""
        if self._client is not None:
            try:
                self._client.close()
            except Exception:
                pass
            self._client = None

    def shutdown(self) -> None:
        """Clean shutdown — close the HTTP connection pool."""
        self.on_session_end([])

    # ------------------------------------------------------------------
    # Config introspection (for `hermes memory setup`)
    # ------------------------------------------------------------------

    def get_config_schema(self) -> List[Dict[str, Any]]:
        return [
            {
                "key": "ACC_TOKEN",
                "description": "ACC hub API bearer token",
                "secret": True,
                "required": True,
                "env_var": "ACC_TOKEN",
                "url": "http://localhost:8789",
            },
            {
                "key": "ACC_HUB_URL",
                "description": "ACC hub base URL (default: http://localhost:8789)",
                "secret": False,
                "required": False,
                "default": "http://localhost:8789",
                "env_var": "ACC_HUB_URL",
            },
        ]

    # ------------------------------------------------------------------
    # Internal helpers
    # ------------------------------------------------------------------

    def _handle_search(self, inp: Dict[str, Any]) -> str:
        if not self._client:
            return json.dumps({"error": "ACC client not initialized"})
        query = inp.get("query", "")
        if not query:
            return json.dumps({"error": "query is required"})
        limit = int(inp.get("limit", self._search_limit))
        collection = inp.get("collection") or self._collection
        try:
            hits = self._client.memory.search(
                query, limit=limit, collection=collection
            )
            return json.dumps({"hits": hits, "count": len(hits)})
        except Exception as exc:
            logger.warning("acc_memory_search error: %s", exc)
            return json.dumps({"error": str(exc)})

    def _handle_store(self, inp: Dict[str, Any]) -> str:
        if not self._client:
            return json.dumps({"error": "ACC client not initialized"})
        text = inp.get("text", "")
        if not text:
            return json.dumps({"error": "text is required"})
        metadata: Dict[str, Any] = inp.get("metadata") or {}
        # Stamp provenance fields if the caller didn't supply them.
        metadata.setdefault("agent", self._agent_identity or "hermes")
        metadata.setdefault("session_id", self._session_id)
        metadata.setdefault("platform", self._platform)
        collection = inp.get("collection") or self._collection
        try:
            self._client.memory.store(text, metadata=metadata, collection=collection)
            return json.dumps({"ok": True, "stored": len(text)})
        except Exception as exc:
            logger.warning("acc_memory_store error: %s", exc)
            return json.dumps({"error": str(exc)})


# ---------------------------------------------------------------------------
# Plugin registration entry point
# ---------------------------------------------------------------------------


def register(ctx: Any) -> None:
    """Register the AccSharedMemoryProvider with the plugin context.

    Called by :func:`plugins.memory.load_memory_provider` when the plugin
    loader uses the ``register(ctx)`` pattern.
    """
    ctx.register_memory_provider(AccSharedMemoryProvider())
