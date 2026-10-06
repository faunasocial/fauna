r"""tier_1: the live Hetzner suite's token has TWO homes — the env, else the file.

``tests/live/conftest.py``'s opt-in gate read ``HETZNER_API_TOKEN`` and nothing
else, while the fleet's documented per-machine home for that token is the file
``~/.hetzner-token`` (the deploy-verify cloud-firewall check reads exactly
those two, in that order). So a box that already HAD the token skipped the whole
live suite, and a session halted on a ``NEEDS FROM USER:`` for a secret sitting
on its own disk — measured on Windows, 2026-09-01. One resolver, ``helpers.live_provision.hetzner_token``, closes that:
the env wins, the file is the fallback, neither is an honest empty string, and
the gate promotes a file-sourced token into the env so every downstream reader
(the four live tests, the ``hetzner`` fixture) keeps its one name.

The file's contents are never printed and never logged — these tests use
throwaway strings under a throwaway home.
"""
from __future__ import annotations

import pytest

from helpers.live_provision import HETZNER_TOKEN_FILE, hetzner_token

pytestmark = [pytest.mark.tier_1]


@pytest.fixture
def home(tmp_path, monkeypatch):
    """A throwaway home so ``~`` expands under ``tmp_path`` on every platform
    (``os.path.expanduser`` reads USERPROFILE on Windows, HOME elsewhere)."""
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.setenv("USERPROFILE", str(tmp_path))
    monkeypatch.delenv("HETZNER_API_TOKEN", raising=False)
    return tmp_path


def _write_token_file(home, text: str) -> None:
    assert HETZNER_TOKEN_FILE.startswith("~/")
    (home / HETZNER_TOKEN_FILE[2:]).write_text(text, encoding="utf-8")


def test_the_env_wins_over_the_file(home, monkeypatch):
    _write_token_file(home, "tok-from-file\n")
    monkeypatch.setenv("HETZNER_API_TOKEN", "  tok-from-env \n")
    assert hetzner_token() == "tok-from-env"


def test_the_file_is_the_fallback_when_the_env_is_unset(home):
    _write_token_file(home, "tok-from-file\r\n")
    assert hetzner_token() == "tok-from-file"


def test_a_blank_env_value_does_not_shadow_the_file(home, monkeypatch):
    _write_token_file(home, "tok-from-file\n")
    monkeypatch.setenv("HETZNER_API_TOKEN", "   ")
    assert hetzner_token() == "tok-from-file"


def test_neither_home_is_an_honest_empty_string(home):
    assert hetzner_token() == ""


def test_an_empty_file_is_not_a_token(home):
    _write_token_file(home, "\n")
    assert hetzner_token() == ""
