"""Token resolution precedence tests."""
from __future__ import annotations

import pytest

from acc_client import NoToken
from acc_client._auth import _parse_dotenv, resolve_base_url, resolve_token


def test_explicit_beats_env(monkeypatch):
    monkeypatch.setenv("ACC_TOKEN", "env-token")
    assert resolve_token("explicit") == "explicit"


def test_acc_token_beats_ccc_agent_token(monkeypatch):
    monkeypatch.setenv("ACC_TOKEN", "a")
    monkeypatch.setenv("CCC_AGENT_TOKEN", "b")
    assert resolve_token() == "a"


def test_ccc_agent_token_as_fallback(monkeypatch):
    monkeypatch.delenv("ACC_TOKEN", raising=False)
    monkeypatch.setenv("CCC_AGENT_TOKEN", "legacy")
    assert resolve_token() == "legacy"


def test_no_token_raises(monkeypatch, tmp_path):
    for k in ("ACC_TOKEN", "CCC_AGENT_TOKEN", "ACC_AGENT_TOKEN"):
        monkeypatch.delenv(k, raising=False)
    monkeypatch.setenv("HOME", str(tmp_path))  # no .acc/.env here
    with pytest.raises(NoToken):
        resolve_token()


def test_dotenv_parser_handles_quotes_and_comments():
    text = '# comment\nACC_TOKEN="abc"\nACC_AGENT_TOKEN=plain\nOTHER=ignore\n'
    parsed = _parse_dotenv(text)
    assert parsed["ACC_TOKEN"] == "abc"
    assert parsed["ACC_AGENT_TOKEN"] == "plain"
    assert parsed["OTHER"] == "ignore"


def test_dotenv_used_when_no_env(monkeypatch, tmp_path):
    for k in ("ACC_TOKEN", "CCC_AGENT_TOKEN", "ACC_AGENT_TOKEN"):
        monkeypatch.delenv(k, raising=False)
    env = tmp_path / ".acc" / ".env"
    env.parent.mkdir()
    env.write_text("ACC_TOKEN=file-token\n")
    monkeypatch.setenv("HOME", str(tmp_path))
    assert resolve_token() == "file-token"


def test_base_url_strips_trailing_slash():
    assert resolve_base_url("http://hub/") == "http://hub"
    assert resolve_base_url("http://hub") == "http://hub"


def test_base_url_default(monkeypatch):
    monkeypatch.delenv("ACC_URL", raising=False)
    monkeypatch.delenv("CCC_URL", raising=False)
    monkeypatch.delenv("ACC_HUB_URL", raising=False)
    assert resolve_base_url() == "http://localhost:8789"


def test_acc_hub_url_beats_acc_url(monkeypatch):
    """ACC_HUB_URL has higher precedence than ACC_URL (mirrors Rust acc-client)."""
    monkeypatch.setenv("ACC_HUB_URL", "http://hub-url")
    monkeypatch.setenv("ACC_URL", "http://acc-url")
    assert resolve_base_url() == "http://hub-url"


def test_acc_url_beats_ccc_url(monkeypatch):
    monkeypatch.delenv("ACC_HUB_URL", raising=False)
    monkeypatch.setenv("ACC_URL", "http://acc-url")
    monkeypatch.setenv("CCC_URL", "http://ccc-url")
    assert resolve_base_url() == "http://acc-url"
