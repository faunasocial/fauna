"""tier_1 unit tests for `scripts/_cloud_project.py` — which cloud project a box
lives in, and where its token comes from.

The provider scopes a token to a whole project, so the organisation's boxes and
zones are cut into four projects by who holds each token — zone, uat,
acceptance, production (`deployment-threat-model.md` § Concern 2, path 5) —
and a host resolves one project per purpose: its *cloud* project (the server
and firewall) and its *dns* project (the zone its records live in). What is
pinned here is that no token reaches past its project: a uat box never resolves
the acceptance or production token, a DNS lookup never resolves a token that
reaches a server, the resolution order (the env first, then the project's own
file, then the legacy file for production only) and the refusal of a token
file other users can read.

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
    "e2e-1a2b.fauna.build",
    "https://Dev.FAUNA.build:443/api/v1/health",
    "dev.fauna.build.",
])
def test_the_dev_test_and_throwaway_boxes_are_the_uat_project(host):
    assert cp.project_for(host) == "uat"
    assert cp.project_for(host, purpose="cloud") == "uat"


@pytest.mark.parametrize("host", ["fauna.build", "https://FAUNA.build/", "fauna.build."])
def test_the_acceptance_box_at_the_apex_is_the_acceptance_project(host):
    assert cp.project_for(host, purpose="cloud") == "acceptance"


@pytest.mark.parametrize("host", [
    "fauna.build", "dev.fauna.build", "test.fauna.build", "e2e-1a2b.fauna.build",
    "mta-sts.fauna.build",
])
def test_every_name_in_the_org_zone_resolves_dns_to_the_zone_project(host):
    assert cp.project_for(host, purpose="dns") == "zone"


@pytest.mark.parametrize("host", ["fauna.social", "www.fauna.social", "https://fauna.social/"])
def test_the_apex_and_its_vhosts_are_the_production_project_for_both_purposes(host):
    assert cp.project_for(host) == "production"
    assert cp.project_for(host, purpose="dns") == "production"


def test_the_legacy_dev_box_is_in_todays_project_which_becomes_production():
    assert cp.project_for("dev.example.com") == "production"


@pytest.mark.parametrize("host", ["nest.example.org", "notfauna.build.example", "xfauna.social"])
@pytest.mark.parametrize("purpose", ["cloud", "dns"])
def test_a_host_outside_the_map_has_no_project(host, purpose):
    assert cp.project_for(host, purpose=purpose) is None


@pytest.mark.parametrize("host", [None, "", "  "])
def test_no_host_is_where_the_throwaway_boxes_live(host):
    assert cp.project_for(host) == "uat"
    assert cp.project_for(host, purpose="dns") == "zone"


def test_an_unknown_purpose_is_refused():
    with pytest.raises(ValueError):
        cp.project_for("dev.fauna.build", purpose="firewall")


def test_token_files_are_one_per_project():
    for project in ("zone", "uat", "acceptance", "production"):
        assert cp.token_file(project) == f"~/.config/fauna/cloud/{project}.token"
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


def test_each_purpose_reads_its_own_projects_file(home):
    for project in ("zone", "uat", "acceptance", "production"):
        _write(home, cp.token_file(project), f"tok-{project}\n")
    assert cp.token_for("dev.fauna.build", purpose="cloud") == "tok-uat"
    assert cp.token_for("dev.fauna.build", purpose="dns") == "tok-zone"
    assert cp.token_for("fauna.build", purpose="cloud") == "tok-acceptance"
    assert cp.token_for("fauna.build", purpose="dns") == "tok-zone"
    assert cp.token_for(None, purpose="dns") == "tok-zone"
    assert cp.token_for("fauna.social", purpose="dns") == "tok-production"


def test_no_purpose_falls_back_to_another_projects_token(home):
    _write(home, cp.token_file("uat"), "tok-uat\n")
    _write(home, cp.token_file("production"), "tok-prod\n")
    assert cp.token_for("fauna.build", purpose="cloud") == ""
    assert cp.token_for("dev.fauna.build", purpose="dns") == ""
    _write(home, cp.token_file("zone"), "tok-zone\n")
    assert cp.token_for("fauna.build", purpose="cloud") == ""


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
    assert "~/.config/fauna/cloud/zone.token" in cp.where_to_put("dev.fauna.build", purpose="dns")
    assert "~/.config/fauna/cloud/acceptance.token" in cp.where_to_put("fauna.build")
    assert "~/.hetzner-token" in cp.where_to_put("fauna.social")
    assert "not one of the organisation's boxes" in cp.where_to_put("nest.example.org")
