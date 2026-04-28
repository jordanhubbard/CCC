"""Unit tests for the _errors module.

Covers the exception hierarchy, status-to-class mapping, field extraction,
and the ``from_response`` factory.  These tests are pure-Python with no
HTTP calls, so no mocking is required.
"""
from __future__ import annotations

import pytest

from acc_client._errors import (
    ApiError,
    AtCapacity,
    Conflict,
    Locked,
    NotFound,
    Unauthorized,
    from_response,
)


# ── ApiError base class ───────────────────────────────────────────────────────


def test_api_error_stores_status_and_code():
    err = ApiError(503, {"error": "service_unavailable"})
    assert err.status == 503
    assert err.code == "service_unavailable"


def test_api_error_message_falls_back_to_code():
    err = ApiError(503, {"error": "oops"})
    assert "oops" in str(err)


def test_api_error_uses_message_field_when_present():
    err = ApiError(400, {"error": "bad_request", "message": "field x is required"})
    assert "field x is required" in str(err)


def test_api_error_synth_code_when_error_missing():
    """If the server omits 'error', we synthesise http_<status>."""
    err = ApiError(500, {"message": "boom"})
    assert err.code == "http_500"


def test_api_error_extra_carries_endpoint_fields():
    """Fields beyond 'error' and 'message' land in .extra."""
    err = ApiError(423, {"error": "blocked", "pending": "task-9", "count": 3})
    assert err.extra["pending"] == "task-9"
    assert err.extra["count"] == 3
    # 'error' and 'message' are NOT repeated in extra
    assert "error" not in err.extra
    assert "message" not in err.extra


def test_api_error_extra_empty_for_bare_code():
    err = ApiError(404, {"error": "not_found"})
    assert err.extra == {}


def test_api_error_none_body_defaults_gracefully():
    err = ApiError(500, None)
    assert err.code == "http_500"
    assert err.extra == {}


def test_api_error_empty_body_defaults_gracefully():
    err = ApiError(500, {})
    assert err.code == "http_500"


# ── Status-specific subclasses ────────────────────────────────────────────────


@pytest.mark.parametrize(
    "status, cls",
    [
        (401, Unauthorized),
        (404, NotFound),
        (409, Conflict),
        (423, Locked),
        (429, AtCapacity),
    ],
)
def test_from_response_returns_correct_subclass(status, cls):
    err = from_response(status, {"error": "test_code"})
    assert isinstance(err, cls)
    assert isinstance(err, ApiError)
    assert err.status == status
    assert err.code == "test_code"


def test_from_response_unknown_status_returns_api_error():
    err = from_response(503, {"error": "overloaded"})
    assert type(err) is ApiError
    assert err.status == 503


def test_from_response_none_body_still_works():
    err = from_response(500, None)
    assert isinstance(err, ApiError)
    assert err.status == 500
    assert err.code == "http_500"


# ── Inheritance / isinstance checks ──────────────────────────────────────────


def test_all_subclasses_are_api_error():
    for cls in (Unauthorized, NotFound, Conflict, Locked, AtCapacity):
        err = cls(409, {"error": "x"})
        assert isinstance(err, ApiError)
        assert isinstance(err, RuntimeError)


# ── Conflict — claim-race path ────────────────────────────────────────────────


def test_conflict_preserves_extra_fields():
    err = Conflict(409, {"error": "already_claimed", "claimedBy": "boris"})
    assert err.code == "already_claimed"
    assert err.extra.get("claimedBy") == "boris"


# ── Locked — dependency-blocked path ─────────────────────────────────────────


def test_locked_preserves_pending_field():
    """The server sends ``pending`` to identify the blocking task."""
    err = Locked(423, {"error": "blocked", "pending": "task-1"})
    assert err.extra["pending"] == "task-1"


# ── AtCapacity — concurrency limit path ──────────────────────────────────────


def test_at_capacity_preserves_active_and_max():
    err = AtCapacity(429, {"error": "at_capacity", "active": 3, "max": 3})
    assert err.extra["active"] == 3
    assert err.extra["max"] == 3


# ── str() output ─────────────────────────────────────────────────────────────


def test_str_includes_status_code():
    err = ApiError(418, {"error": "teapot"})
    assert "418" in str(err)


def test_str_includes_message_not_just_code():
    err = ApiError(422, {"error": "validation_error", "message": "name is required"})
    assert "name is required" in str(err)
