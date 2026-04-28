"""pytest configuration for the acc-client test suite.

This file is intentionally minimal — all shared fixtures that span multiple
test modules live here; module-local fixtures stay in their own test files.

Session-scoped environment sanity
----------------------------------
The auth module reads ``ACC_TOKEN``, ``CCC_AGENT_TOKEN``, ``ACC_AGENT_TOKEN``,
``ACC_HUB_URL``, ``ACC_URL``, and ``CCC_URL`` from the environment.  When
running in CI these variables are not set, which is correct — the tests that
exercise token resolution either supply their own monkeypatches or assert that
:class:`~acc_client.NoToken` is raised.

To prevent developer-machine environment variables from silently making
"should raise NoToken" tests pass for the wrong reason, we strip the relevant
vars from ``os.environ`` for the entire test session.  Individual tests that
need a token can restore them via ``monkeypatch.setenv`` or the ``token_env``
fixture below.
"""
from __future__ import annotations

import os

import pytest


# Variables that must not bleed in from a developer's real environment.
_CRED_VARS = (
    "ACC_TOKEN",
    "CCC_AGENT_TOKEN",
    "ACC_AGENT_TOKEN",
    "ACC_HUB_URL",
    "ACC_URL",
    "CCC_URL",
)


@pytest.fixture(autouse=True, scope="session")
def _strip_env_credentials():
    """Remove real credentials from the test-session environment.

    This is a session-scoped autouse fixture so it runs once before any
    test.  Individual tests restore variables they need via
    ``monkeypatch.setenv``.
    """
    saved = {k: os.environ.pop(k) for k in _CRED_VARS if k in os.environ}
    yield
    os.environ.update(saved)


@pytest.fixture
def token_env(monkeypatch):
    """Set ``ACC_TOKEN=test-token`` for the duration of one test.

    Use this fixture in tests that construct a :class:`~acc_client.Client`
    via ``Client.from_env()`` and don't care about the specific token value.
    """
    monkeypatch.setenv("ACC_TOKEN", "test-token")
    return "test-token"
