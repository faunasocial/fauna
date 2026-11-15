"""The App Store Connect builds reader parses App Store Connect's replies into the
reading the store-review record takes.

`just ios-store-builds` (maintainer tooling: a Swift script run on the
interpreter) prints each build's processing state and TestFlight
state (``docs/goal/architecture/installers/ios.md`` § Implementation status
today; ``installers/store-review.md`` § The states). Its ``--fixture`` mode
replays recorded replies in place of App Store Connect, so the parsing is held
here with no key and no network: the build number and version string joined
from the included pre-release version, the processing state, the TestFlight
detail, a 403 on a sub-resource reported as not readable (never a failed read),
the groups holding each build — and the groups' own 403 likewise.

The fixture (``fixtures/asc/store-builds-replies.json``) is shaped from a real
reply of 2026-11-11 with every identifier but the app's public id replaced; it
names no person.

tier_1 in spirit — no nest, no app, no network — but it runs the Swift
interpreter, which only a macOS box with Xcode has. A tree without the recipe
(the reader is maintainer tooling) skips.
"""

from __future__ import annotations

import copy
import json
import subprocess
from pathlib import Path

import pytest

from helpers.app_surface import skip_environment

pytestmark = [pytest.mark.tier_1, pytest.mark.host_platform("macos"), pytest.mark.timeout(600)]

ROOT = Path(__file__).resolve().parents[3]
FIXTURE = ROOT / "tests/e2e-unified/fixtures/asc/store-builds-replies.json"
BUILD_1 = "0b5e1c2a-0000-4000-8000-000000000001"
BUILD_2 = "94c18c3e-a93b-4799-baef-531df478bdf9"
NOT_READABLE = "not readable by this key's role (HTTP 403)"


RECIPE = "ios-store-builds"


def _read(fixture: Path, *extra: str) -> subprocess.CompletedProcess:
    shown = subprocess.run(["just", "--show", RECIPE], cwd=ROOT, capture_output=True, text=True, timeout=60)
    if shown.returncode != 0:
        skip_environment(f"this tree has no `just {RECIPE}` recipe (the App Store Connect reader is maintainer tooling)")
    return subprocess.run(
        ["just", RECIPE, "--fixture", str(fixture), *extra],
        capture_output=True,
        text=True,
        timeout=540,
        cwd=ROOT,
    )


def _ok(fixture: Path) -> dict:
    proc = _read(fixture)
    assert proc.returncode == 0, f"reader exited {proc.returncode}:\n{proc.stderr}"
    return json.loads(proc.stdout)


def test_builds_parse_with_processing_and_testflight_state():
    out = _ok(FIXTURE)
    assert out["app"] == {"id": "6804301687", "bundleId": "social.fauna.fauna"}
    assert out["credentialSource"] == "fixture"
    builds = {b["id"]: b for b in out["builds"]}
    assert [b["id"] for b in out["builds"]] == [BUILD_2, BUILD_1], "newest upload first, as App Store Connect sorted"
    one = builds[BUILD_1]
    assert (one["version"], one["preReleaseVersion"], one["platform"]) == ("1", "0.1.6", "IOS")
    assert one["processingState"] == "VALID"
    assert one["expired"] is False
    assert one["internalBuildState"] == "READY_FOR_BETA_TESTING"
    assert one["externalBuildState"] == "READY_FOR_BETA_SUBMISSION"
    assert one["betaGroups"] == ["Internal"]
    two = builds[BUILD_2]
    assert two["processingState"] == "PROCESSING"
    assert two["betaDetail"] == NOT_READABLE, "a 403 on the beta detail is a note, not a failed read"
    assert "internalBuildState" not in two
    assert two["betaGroups"] == []
    groups = {g["name"]: g for g in out["betaGroups"]}
    assert groups["Internal"]["internal"] is True and groups["Internal"]["builds"] == [BUILD_1]
    assert groups["External"]["internal"] is False and groups["External"]["builds"] == []


def test_unreadable_groups_are_a_note(tmp_path):
    replies = json.loads(FIXTURE.read_text(encoding="utf-8"))
    replies["/v1/betaGroups"] = {"_status": 403, "_body": "forbidden"}
    fixture = tmp_path / "replies.json"
    fixture.write_text(json.dumps(replies), encoding="utf-8")
    out = _ok(fixture)
    assert out["betaGroupsUnreadable"] == NOT_READABLE
    assert "betaGroups" not in out
    assert all("betaGroups" not in b for b in out["builds"])


def test_a_refused_builds_read_fails_the_read(tmp_path):
    replies = copy.deepcopy(json.loads(FIXTURE.read_text(encoding="utf-8")))
    replies["/v1/builds"] = {"_status": 401, "_body": "NOT_AUTHORIZED"}
    fixture = tmp_path / "replies.json"
    fixture.write_text(json.dumps(replies), encoding="utf-8")
    proc = _read(fixture)
    assert proc.returncode == 2, proc.stderr
    assert "HTTP 401" in proc.stderr and "NOT_AUTHORIZED" in proc.stderr


def test_an_unknown_bundle_id_exits_4():
    proc = _read(FIXTURE, "--bundle-id", "social.fauna.nothing")
    assert proc.returncode == 4, proc.stderr
