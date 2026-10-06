"""tier_1 proofs for ``helpers/staging_box.py`` — the decisions the kept
staging-box live test (``tests/live/test_staging_box_provision.py``) makes
before it spends anything: what belongs to the box, when a run provisions,
resumes or refuses, and how the admin credentials persist between runs.
"""
from __future__ import annotations

import json
import stat
import sys

import pytest

from helpers import staging_box as sb

pytestmark = pytest.mark.tier_1

DOMAIN = "dev.example.test"


def test_server_name_matches_the_orchestrators_scheme():
    assert sb.server_name_for(DOMAIN) == "dev-example-test"


def test_zone_is_the_longest_containing_zone():
    zones = ["example.test", "test", "other.test", "dev.example.test."]
    assert sb.zone_for(DOMAIN, zones) == "dev.example.test"
    assert sb.zone_for(DOMAIN, ["example.test", "other.test"]) == "example.test"
    assert sb.zone_for(DOMAIN, ["other.test", "ample.test"]) is None


def test_relative_name():
    assert sb.relative_name(DOMAIN, "example.test") == "dev"
    assert sb.relative_name("example.test", "example.test.") == "@"


def test_rrsets_under_takes_the_name_and_everything_below_it_only():
    rrsets = [
        {"name": "dev", "type": "A"},
        {"name": "mail.dev", "type": "A"},
        {"name": "s1._domainkey.dev", "type": "TXT"},
        {"name": "@", "type": "A"},
        {"name": "notdev", "type": "A"},
        {"name": "dev2", "type": "A"},
        {"name": "mail", "type": "A"},
    ]
    assert [r["name"] for r in sb.rrsets_under(rrsets, "dev")] == [
        "dev", "mail.dev", "s1._domainkey.dev",
    ]


def test_a_box_at_the_zone_apex_is_refused():
    with pytest.raises(sb.StagingPlanError):
        sb.rrsets_under([{"name": "@", "type": "A"}], "@")


def test_nothing_at_the_name_provisions():
    assert sb.decide_start(state={"domain": DOMAIN}, servers=[], rrsets=[]) == sb.PROVISION


def test_the_recorded_server_resumes():
    state = {"domain": DOMAIN, "server_id": 7}
    servers = [{"id": 7, "name": "dev-example-test"}]
    assert sb.decide_start(state=state, servers=servers, rrsets=[{"name": "dev"}]) == sb.RESUME


@pytest.mark.parametrize(
    "state, servers, rrsets",
    [
        # a server nobody recorded: someone else's, or built but never claimed
        ({"domain": DOMAIN}, [{"id": 7, "name": "dev-example-test"}], []),
        # a server, but not the recorded one
        ({"domain": DOMAIN, "server_id": 3}, [{"id": 7, "name": "dev-example-test"}], []),
        # stray records with no server
        ({"domain": DOMAIN}, [], [{"name": "dev", "type": "A"}]),
        # a recorded server that is gone, its records left behind
        ({"domain": DOMAIN, "server_id": 7}, [], [{"name": "dev", "type": "A"}]),
    ],
)
def test_anything_else_is_refused(state, servers, rrsets):
    with pytest.raises(sb.StagingPlanError):
        sb.decide_start(state=state, servers=servers, rrsets=rrsets)


def test_state_is_minted_once_private_and_reused(tmp_path, monkeypatch):
    monkeypatch.setattr(sb, "STATE_DIR", str(tmp_path / "staging-box"))
    first = sb.load_or_create_state(DOMAIN)
    assert first["handle"] == f"admin@{DOMAIN}"
    assert len(bytes.fromhex(first["secret_hex"])) == 32
    path = sb.state_path(DOMAIN)
    # POSIX mode bits only exist off Windows (os.stat reports 0o666 for any
    # writable file there); privacy there is the per-user profile ACL.
    posix_modes = sys.platform != "win32"
    if posix_modes:
        assert stat.S_IMODE(path.stat().st_mode) == 0o600
        assert stat.S_IMODE(path.parent.stat().st_mode) == 0o700

    first["server_id"] = 7
    sb.save_state(first)
    again = sb.load_or_create_state(DOMAIN)
    assert again == first, "a rerun must reuse the identity and what the run recorded"
    if posix_modes:
        assert stat.S_IMODE(path.stat().st_mode) == 0o600


def test_a_state_file_for_another_box_is_never_overwritten(tmp_path, monkeypatch):
    monkeypatch.setattr(sb, "STATE_DIR", str(tmp_path / "staging-box"))
    path = sb.state_path(DOMAIN)
    path.parent.mkdir(parents=True)
    path.write_text(json.dumps({"domain": "other.example.test", "secret_hex": "00" * 32}))
    with pytest.raises(sb.StagingPlanError):
        sb.load_or_create_state(DOMAIN)
    assert json.loads(path.read_text())["domain"] == "other.example.test"


def test_a_failure_before_the_claim_removes_what_the_run_created():
    servers = [{"id": 7, "name": "dev-example-test"}]
    rrsets = [{"name": "dev", "type": "A"}, {"name": "mail.dev", "type": "A"}]
    plan = sb.cleanup_plan(state={"domain": DOMAIN}, servers=servers, rrsets=rrsets)
    assert plan == {"servers": servers, "rrsets": rrsets}


def test_a_failure_after_the_claim_removes_nothing():
    """A recorded box is claimed: a rerun resumes it, so a later step failing
    (mail, certificate, DNS, the serving gate) must never take the box down."""
    state = {"domain": DOMAIN, "server_id": 7}
    plan = sb.cleanup_plan(
        state=state,
        servers=[{"id": 7, "name": "dev-example-test"}],
        rrsets=[{"name": "dev", "type": "A"}],
    )
    assert plan == {"servers": [], "rrsets": []}
