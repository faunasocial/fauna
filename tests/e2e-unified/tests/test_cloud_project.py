"""tier_1 unit tests for `scripts/_cloud_project.py` — which cloud project a box
lives in, and where its token comes from.

The provider scopes a token to a whole project, so the organisation's boxes are
split into a uat project and a production project
(`deployment-threat-model.md` § Concern 2, path 5). What is pinned here is
that a uat box can never resolve the production token: the map, the resolution
order (the env first, then the project's own file, then the legacy file for
production only) and the refusal of a token file other users can read.

Throwaway strings under a throwaway home; no token is ever read or printed.
"""

from __future__ import annotations

import importlib.util
import os

import pytest

pytestmark = pytest.mark.tier_1

_REPO_ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))))
_SCRIPT = os.path.join(_REPO_ROOT, "scripts", "_cloud_project.py")


def _load_module():
    spec = importlib.util.spec_from_file_location("_cloud_project", _SCRIPT)
    mod = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(mod)
    return mod


cp = _load_module()


@pytest.fixture
def home(tmp_path, monkeypatch):
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.setenv("USERPROFILE", str(tmp_path))
    monkeypatch.delenv(cp.ENV_VAR, raising=False)
    return tmp_path


def _write(home, rel: str, text: str, mode: int = 0o600) -> None:
    assert rel.startswith("~/")
    path = home / rel[2:]
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")
    os.chmod(path, mode)


# ── The map ──────────────────────────────────────────────────────────────────

@pytest.mark.parametrize("host", [
    "dev.fauna.build",
    "test.fauna.build",
    "fauna.build",
    "e2e-1a2b.fauna.build",
    "https://Dev.FAUNA.build:443/api/v1/health",
    "fauna.build.",
])
def test_the_uat_zone_and_everything_under_it_is_the_uat_project(host):
    assert cp.project_for(host) == "uat"


@pytest.mark.parametrize("host", ["fauna.social", "www.fauna.social", "https://fauna.social/"])
def test_the_apex_and_its_vhosts_are_the_production_project(host):
    assert cp.project_for(host) == "production"


def test_the_legacy_dev_box_is_in_todays_project_which_becomes_production():
    assert cp.project_for("dev.example.com") == "production"


@pytest.mark.parametrize("host", ["nest.example.org", "notfauna.build.example", "xfauna.social"])
def test_a_host_outside_the_map_has_no_project(host):
    assert cp.project_for(host) is None


@pytest.mark.parametrize("host", [None, "", "  "])
def test_no_host_is_the_uac_project(host):
    assert cp.project_for(host) == "uat"


def test_token_files_are_one_per_project():
    assert cp.token_file("uat") == "~/.config/fauna/cloud/uat.token"
    assert cp.token_file("production") == "~/.config/fauna/cloud/production.token"
    with pytest.raises(ValueError):
        cp.token_file("staging")


# ── Resolution order ─────────────────────────────────────────────────────────

def test_the_env_wins_over_every_file(home, monkeypatch):
    _write(home, cp.token_file("uat"), "tok-uat\n")
    monkeypatch.setenv(cp.ENV_VAR, "  tok-env \n")
    assert cp.token_for("dev.fauna.build") == "tok-env"
    assert cp.token_for("nest.example.org") == "tok-env"


def test_a_blank_env_value_does_not_shadow_the_file(home, monkeypatch):
    _write(home, cp.token_file("uat"), "\n tok-uat \n")
    monkeypatch.setenv(cp.ENV_VAR, "   ")
    assert cp.token_for("dev.fauna.build") == "tok-uat"


def test_each_box_reads_its_own_projects_file(home):
    _write(home, cp.token_file("uat"), "tok-uat\n")
    _write(home, cp.token_file("production"), "tok-prod\n")
    assert cp.token_for("test.fauna.build") == "tok-uat"
    assert cp.token_for(None) == "tok-uat"
    assert cp.token_for("fauna.social") == "tok-prod"


def test_a_uac_box_never_falls_back_to_a_production_token(home):
    _write(home, cp.token_file("production"), "tok-prod\n")
    _write(home, cp.LEGACY_TOKEN_FILE, "tok-legacy\n", mode=0o664)
    assert cp.token_for("dev.fauna.build") == ""
    assert cp.token_for(None) == ""


def test_production_falls_back_to_the_legacy_file(home):
    _write(home, cp.LEGACY_TOKEN_FILE, "tok-legacy\n", mode=0o664)
    assert cp.token_for("dev.example.com") == "tok-legacy"
    _write(home, cp.token_file("production"), "tok-prod\n")
    assert cp.token_for("dev.example.com") == "tok-prod"


def test_a_host_outside_the_map_reads_no_file(home):
    _write(home, cp.token_file("uat"), "tok-uat\n")
    assert cp.token_for("nest.example.org") == ""


def test_an_injected_env_mapping_replaces_the_process_env(home, monkeypatch):
    monkeypatch.setenv(cp.ENV_VAR, "tok-process")
    assert cp.token_for("dev.fauna.build", env={}) == ""
    assert cp.token_for("dev.fauna.build", env={cp.ENV_VAR: "tok-given"}) == "tok-given"


@pytest.mark.skipif(os.name == "nt", reason="POSIX mode bits")
def test_a_token_file_others_can_read_is_refused(home):
    _write(home, cp.token_file("uat"), "tok-uat\n", mode=0o644)
    with pytest.raises(cp.TokenFileModeError, match="chmod 600"):
        cp.token_for("dev.fauna.build")


def test_where_to_put_names_the_projects_file():
    assert "~/.config/fauna/cloud/uat.token" in cp.where_to_put("dev.fauna.build")
    assert "~/.hetzner-token" in cp.where_to_put("fauna.social")
    assert "not one of the organisation's boxes" in cp.where_to_put("nest.example.org")
