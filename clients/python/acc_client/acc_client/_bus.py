"""Bus send, recent-messages query, and live SSE stream.

Mirrors the Rust ``acc_client::bus`` module, including the SSE streaming
path used by ``acc-cli``'s ``bus tail`` sub-command and the hermes
``acc_shared_memory`` plugin.

The SSE stream API
------------------
:meth:`BusApi.stream` opens ``GET /api/bus/stream`` with the
``Accept: text/event-stream`` header and yields one decoded dict per bus
message.  The underlying HTTP client is configured with ``read=None``
timeout so the streaming body is never forcibly interrupted while the
connect/first-byte deadlines remain in effect.

Frame parsing
~~~~~~~~~~~~~
The parser follows the
`W3C Server-Sent Events specification <https://www.w3.org/TR/eventsource/>`_:

* Frames are separated by blank lines (``\\n\\n`` or ``\\r\\n\\r\\n``).
* Each frame may contain multiple ``data:`` lines; they are joined with
  ``\\n`` before JSON-decoding — matching the Rust
  ``acc_client::bus::extract_sse_data`` behaviour.
* Lines beginning with ``:`` are comments (keep-alive pings) and are
  silently skipped.
* ``event:``, ``id:``, and ``retry:`` fields are accepted but currently
  not surfaced to callers.
* Malformed JSON in a single frame is silently skipped so a garbled server
  event does not kill the whole stream.
"""
from __future__ import annotations

import json
from typing import TYPE_CHECKING, Any, Generator

import httpx

from ._errors import from_response

if TYPE_CHECKING:
    from .client import Client

# httpx does not impose a timeout on streaming bodies by default when
# ``timeout`` is a plain float — only on connect + first-byte.  For SSE we
# set read=None (no deadline on the stream body itself) while keeping the
# connect + first-byte limit.
_DEFAULT_CONNECT_TIMEOUT = 30.0
_SSE_TIMEOUT = httpx.Timeout(_DEFAULT_CONNECT_TIMEOUT, read=None)


class BusApi:
    """Bus operations: send, recent messages, and live SSE stream.

    Obtain via ``client.bus`` — do not instantiate directly.
    """

    def __init__(self, client: "Client") -> None:
        self._c = client

    def send(self, kind: str, **fields: Any) -> None:
        """POST /api/bus/send.

        ``kind`` becomes the wire field ``type``.  Additional keyword
        arguments map directly to wire fields.  The special name ``from_``
        is translated to the wire field ``from`` so callers can write
        ``send("hello", from_="boris")`` without shadowing the Python
        built-in.

        Example::

            client.bus.send(
                "tasks:added",
                from_="boris",
                body="new item queued",
                data={"task_id": "t-1"},
            )
        """
        body: dict[str, Any] = {"type": kind}
        for k, v in fields.items():
            if v is None:
                continue
            wire_key = "from" if k == "from_" else k
            body[wire_key] = v
        self._c._request("POST", "/api/bus/send", json=body)

    def messages(
        self,
        *,
        kind: str | None = None,
        limit: int | None = None,
    ) -> list[dict[str, Any]]:
        """GET /api/bus/messages — list recent bus messages.

        ``kind`` filters by the wire ``type`` field (passed as the
        ``type`` query parameter).  Returns raw dicts; the ``type`` field
        in each dict is the wire name (not renamed to ``kind``).

        Accepts both a bare JSON array and the ``{"messages": [...]}``
        envelope.
        """
        params: dict[str, Any] = {}
        if kind is not None:
            params["type"] = kind
        if limit is not None:
            params["limit"] = limit
        resp = self._c._request("GET", "/api/bus/messages", params=params)
        if isinstance(resp, list):
            return resp
        if isinstance(resp, dict):
            return resp.get("messages", [])
        return []

    def stream(self) -> Generator[dict[str, Any], None, None]:
        """Open ``GET /api/bus/stream`` and yield bus-message dicts.

        Each yielded value is the JSON-decoded payload of one SSE ``data:``
        field.  Multiple ``data:`` lines within a single frame are joined
        with ``\\n`` before decoding — matching the wire spec and the Rust
        crate's ``extract_sse_data`` implementation.

        Malformed JSON in a single frame is silently skipped so a garbled
        server event does not terminate the whole stream.

        The generator terminates when the server closes the connection;
        implement reconnect logic at the call site if needed.

        Usage::

            for msg in client.bus.stream():
                kind = msg.get("type")
                if kind == "tasks:added":
                    print("new task:", msg.get("data", {}).get("task_id"))
        """
        url = self._c._base + "/api/bus/stream"
        headers = {
            "Authorization": f"Bearer {self._c._token}",
            "Accept": "text/event-stream",
        }
        with httpx.Client(timeout=_SSE_TIMEOUT) as http:
            with http.stream("GET", url, headers=headers) as resp:
                if not (200 <= resp.status_code < 300):
                    body_bytes = resp.read()
                    body: dict[str, Any] | None = None
                    try:
                        parsed = json.loads(body_bytes)
                        if isinstance(parsed, dict):
                            body = parsed
                    except Exception:
                        pass
                    raise from_response(resp.status_code, body)

                buf = ""
                for raw_line in resp.iter_lines():
                    # httpx.iter_lines() strips the trailing \n / \r\n but
                    # does not collapse SSE frames.  We accumulate lines and
                    # dispatch on blank-line frame boundaries ourselves.
                    if raw_line == "":
                        # Blank line → end of frame; process whatever we have.
                        if buf:
                            data = _extract_sse_data(buf)
                            buf = ""
                            if data is not None:
                                try:
                                    yield json.loads(data)
                                except json.JSONDecodeError:
                                    pass  # skip malformed frames silently
                    else:
                        buf += raw_line + "\n"

                # Flush a partial frame that was not terminated with a blank
                # line (server closed the connection mid-frame).
                if buf:
                    data = _extract_sse_data(buf)
                    if data is not None:
                        try:
                            yield json.loads(data)
                        except json.JSONDecodeError:
                            pass


# ── SSE frame helpers ─────────────────────────────────────────────────────────


def _extract_sse_data(frame: str) -> str | None:
    """Extract and concatenate all ``data:`` lines from one SSE frame.

    Per the SSE spec multiple ``data:`` lines in a single event are joined
    with ``\\n``.  Lines beginning with ``:`` are comments and are ignored.
    Returns ``None`` if the frame contains no ``data:`` lines (keep-alive,
    metadata-only, etc.).

    Mirrors the Rust ``acc_client::bus::extract_sse_data`` function so both
    implementations parse frames identically.
    """
    parts: list[str] = []
    for line in frame.splitlines():
        if not line or line.startswith(":"):
            continue
        if line.startswith("data:"):
            rest = line[5:]  # strip "data:"
            # One optional leading space per the SSE spec.
            if rest.startswith(" "):
                rest = rest[1:]
            parts.append(rest)
        # Other fields (event:, id:, retry:) — accepted but not surfaced.
    return "\n".join(parts) if parts else None
