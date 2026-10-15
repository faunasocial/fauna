r"""tier_1: the live Hetzner suite resolves its token PER BOX — the env, else the
box's cloud project's token file.

``tests/live/conftest.py``'s opt-in gate once read ``HETZNER_API_TOKEN`` and
nothing else, so a box that already HAD the token on disk skipped the whole
live suite, and a session halted on a ``NEEDS FROM USER:`` for a secret sitting
on its own disk — measured on Windows, 2026-09-01. Since the cloud projects were split by trust tier
(``deployment-threat-model.md`` § Concern 2, path 5) the file is per project:
``helpers.live_provision.hetzner_token`` delegates to
``scripts/_cloud_project.py``'s ``token_for`` for the box the run names, so a
run against a uat box can never pick up the production token. The env wins,
the project's file is the fallback, neither is an honest empty string, and the
gate promotes a file-sourced token into the env so every downstream reader
(the four live tests, the ``hetzner`` fixture) keeps its one name.

The map and the resolution order themselves are pinned in
``tests/test_cloud_project.py``; here, the live suite's wiring. The files'
contents are never printed and never logged — these tests use throwaway
strings under a throwaway home.
"""
from __future__ import annotations

import os

import pytest

from helpers.live_provision import cloud_project, hetzner_token, live_target_host

pytestmark = [pytest.mark.tier_1]

_TARGET_VARS = ("FAUNA_E2E_STAGING_BOX", "FAUNA_E2E_ZONE", "FAUNA_LIVE_NEST_URL")


@pytest.fixture
def home(tmp_path, monkeypatch):
    """A throwaway home so ``~`` expands under ``tmp_path`` on every platform
    (``os.path.expanduser`` reads USERPROFILE on Windows, HOME elsewhere)."""
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.setenv("USERPROFILE", str(tmp_path))
    monkeypatch.delenv("HETZNER_API_TOKEN", raising=False)
    for var in _TARGET_VARS:
        monkeypatch.delenv(var, raising=False)
    return tmp_path


def _write_token_file(home, project: str, text: str) -> None:
    rel = cloud_project.token_file(project)
    assert rel.startswith("~/")
    path = home / rel[2:]
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")
    os.chmod(path, 0o600)


def test_the_env_wins_over_the_file(home, monkeypatch):
    _write_token_file(home, "uat", "tok-from-file\n")
    monkeypatch.setenv("HETZNER_API_TOKEN", "  tok-from-env \n")
    assert hetzner_token() == "tok-from-env"


def test_the_file_is_the_fallback_when_the_env_is_unset(home):
    _write_token_file(home, "uat", "tok-from-file\r\n")
    assert hetzner_token() == "tok-from-file"


def test_a_blank_env_value_does_not_shadow_the_file(home, monkeypatch):
    _write_token_file(home, "uat", "tok-from-file\n")
    monkeypatch.setenv("HETZNER_API_TOKEN", "   ")
    assert hetzner_token() == "tok-from-file"


def test_neither_home_is_an_honest_empty_string(home):
    assert hetzner_token() == ""


def test_an_empty_file_is_not_a_token(home):
    _write_token_file(home, "uat", "\n")
    assert hetzner_token() == ""


def test_the_run_names_its_box_through_the_env_contract(home, monkeypatch):
    assert live_target_host() is None
    monkeypatch.setenv("FAUNA_LIVE_NEST_URL", "https://fauna.social")
    assert live_target_host() == "https://fauna.social"
    monkeypatch.setenv("FAUNA_E2E_ZONE", "fauna.build")
    assert live_target_host() == "fauna.build"
    monkeypatch.setenv("FAUNA_E2E_STAGING_BOX", "dev.fauna.build")
    assert live_target_host() == "dev.fauna.build"


def test_the_token_follows_the_named_box_project(home, monkeypatch):
    _write_token_file(home, "uat", "tok-uat\n")
    _write_token_file(home, "production", "tok-prod\n")
    monkeypatch.setenv("FAUNA_E2E_STAGING_BOX", "test.fauna.build")
    assert hetzner_token() == "tok-uat"
    monkeypatch.setenv("FAUNA_E2E_STAGING_BOX", "fauna.social")
    assert hetzner_token() == "tok-prod"
    # An explicit host overrides the env contract.
    assert hetzner_token("dev.fauna.build") == "tok-uat"


def test_a_uac_run_never_takes_the_production_token(home, monkeypatch):
    _write_token_file(home, "production", "tok-prod\n")
    monkeypatch.setenv("FAUNA_E2E_STAGING_BOX", "dev.fauna.build")
    assert hetzner_token() == ""
