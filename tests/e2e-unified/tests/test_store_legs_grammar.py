"""The store legs' shared grammar refuses what its owner doc says a leg refuses.

`installers/store-review.md` § The store legs: a leg's per-release text comes
from `installer/store-texts/<store>/<name>.txt` at the tag, and a text that is
missing or approved for another version is red before any store call; how far
a leg goes is `installer/store-legs.toml`; what a leg reports is the run
artifact `store-legs.json`, read by the attempt as data and written to the
record only through `just store-review`. The grammar lives once, in
`scripts/_store_legs.py`, for all three legs.

Static-only (tier_1): temporary files and the tracked settings file.
"""
from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

REPO = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(REPO / "scripts"))

import _store_legs as sl  # noqa: E402


def _text(root: Path, store: str, name: str, content: str) -> None:
    path = root / store / f"{name}.txt"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")


def test_a_text_approved_for_the_version_is_sent_verbatim(tmp_path):
    _text(tmp_path, "app-store", "what-to-test", "Version: 0.1.6\n\nTry the feed.\nThen post.\n")
    assert sl.read_text("app-store", "what-to-test", "0.1.6", root=tmp_path) == "Try the feed.\nThen post."


def test_a_version_neutral_text_serves_every_version(tmp_path):
    _text(tmp_path, "play", "whats-new", "Version: any\n\nBug fixes.\n")
    assert sl.read_text("play", "whats-new", "0.2.0", root=tmp_path) == "Bug fixes."


@pytest.mark.parametrize(
    ("content", "why"),
    [
        ("Version: 0.1.5\n\nOld text.\n", "approved for 0.1.5, not 0.1.6"),
        ("Try the feed.\n", "first line must be"),
        ("Version: 0.1.6\nTry the feed.\n", "blank line must follow"),
        ("Version: 0.1.6\n\n   \n", "the text is empty"),
    ],
)
def test_a_text_the_grammar_refuses_is_red_before_any_store_call(tmp_path, content, why):
    _text(tmp_path, "app-store", "what-to-test", content)
    with pytest.raises(sl.LegError, match=why):
        sl.read_text("app-store", "what-to-test", "0.1.6", root=tmp_path)


def test_a_missing_text_is_refused(tmp_path):
    with pytest.raises(sl.LegError, match="missing"):
        sl.read_text("app-store", "what-to-test", "0.1.6", root=tmp_path)


def test_an_unknown_store_or_text_name_is_refused(tmp_path):
    with pytest.raises(sl.LegError, match="store"):
        sl.read_text("testflight", "what-to-test", "0.1.6", root=tmp_path)
    with pytest.raises(sl.LegError, match="text"):
        sl.read_text("app-store", "release-notes", "0.1.6", root=tmp_path)


def test_every_store_goes_to_testing_in_the_tracked_settings():
    # Every 0.1.x release is `testing`; `public` is the owner's word per store.
    for store in sl.sr.STORES:
        assert sl.furthest(store) == "testing"


def test_a_furthest_outside_the_two_values_is_refused(tmp_path):
    path = tmp_path / "store-legs.toml"
    path.write_text('[app-store]\nfurthest = "production"\n', encoding="utf-8")
    with pytest.raises(sl.LegError, match="furthest"):
        sl.furthest("app-store", path)
    assert sl.furthest("play", path) == "testing"


def _leg(**over) -> dict:
    leg = {
        "store": "app-store",
        "version": "0.1.6",
        "build": "1",
        "track": "testflight-external",
        "state": "in-review",
        "read": "2026-11-12T10:15Z",
        "ids": {"build": "abc-123", "betaAppReviewSubmission": "def-456"},
        "note": "Submitted to Beta App Review for the external group.",
    }
    leg.update(over)
    return leg


def test_a_sound_leg_is_appended_to_the_artifact(tmp_path):
    out = tmp_path / "store-legs.json"
    sl.add_leg(out, _leg(state="uploaded", track="testflight-internal"))
    sl.add_leg(out, _leg())
    data = json.loads(out.read_text())
    assert data["schema"] == 1
    assert [leg["state"] for leg in data["legs"]] == ["uploaded", "in-review"]


@pytest.mark.parametrize(
    ("over", "why"),
    [
        ({"track": "internal-testing"}, "track"),
        ({"state": "live-test"}, "state"),
        ({"read": "2026-11-12"}, "UTC minute"),
        ({"version": "v0.1.6"}, "MAJOR.MINOR.PATCH"),
        ({"ids": {"build": 7}}, "ids"),
        ({"note": "Ask reviewer@example.com"}, "address"),
        ({"secret": "x"}, "unknown field"),
    ],
)
def test_a_leg_the_grammar_refuses_never_reaches_the_artifact(tmp_path, over, why):
    out = tmp_path / "store-legs.json"
    with pytest.raises(sl.LegError, match=why):
        sl.add_leg(out, _leg(**over))
    assert not out.exists()


def test_the_command_line_refuses_with_one_verdict_line(tmp_path):
    leg = tmp_path / "leg.json"
    leg.write_text(json.dumps(_leg(state="shipped")))
    run = subprocess.run(
        [sys.executable, str(REPO / "scripts" / "store-legs.py"), "add", str(tmp_path / "out.json"), str(leg)],
        capture_output=True,
        text=True,
    )
    assert run.returncode == 1
    assert run.stderr.startswith("store-legs: add FAILED — ")


def test_the_command_line_appends_a_leg_per_line_or_none(tmp_path):
    out = tmp_path / "out.json"
    legs = tmp_path / "legs.jsonl"
    legs.write_text(json.dumps(_leg(state="uploaded", track="testflight-internal")) + "\n" + json.dumps(_leg()) + "\n")
    cmd = [sys.executable, str(REPO / "scripts" / "store-legs.py"), "add", str(out), str(legs)]
    assert subprocess.run(cmd, capture_output=True, text=True).returncode == 0
    assert len(json.loads(out.read_text())["legs"]) == 2
    legs.write_text(json.dumps(_leg()) + "\n" + json.dumps(_leg(state="shipped")) + "\n")
    assert subprocess.run(cmd, capture_output=True, text=True).returncode == 1
    assert len(json.loads(out.read_text())["legs"]) == 2


# § The store legs, *A refusal of permission is waited out*: a leg's every try rides
# beside its legs, red or not, so the attempt records the tries and the wait.

def _try(**over):
    entry = {"store": "play", "at": "2026-11-12T09:05Z", "outcome": "refused-permission",
             "note": "edits.insert: HTTP 403: The caller does not have permission"}
    entry.update(over)
    return entry


def test_tries_ride_beside_the_legs_and_a_leg_added_after_keeps_them(tmp_path):
    out = tmp_path / "store-legs.json"
    sl.add_tries(out, [_try(), _try(at="2026-11-12T09:10Z", outcome="done", note="")])
    sl.add_leg(out, _leg())
    data = sl.load_artifact(out)
    assert [t["outcome"] for t in data["tries"]] == ["refused-permission", "done"]
    assert len(data["legs"]) == 1
    assert "tries" not in sl.load_artifact(tmp_path / "absent.json"), "tries are optional"


@pytest.mark.parametrize(("over", "why"), [
    ({"outcome": "waiting"}, "outcome"),
    ({"at": "09:05"}, "UTC minute"),
    ({"store": "f-droid"}, "store"),
    ({"note": "Ask reviewer@example.com"}, "address"),
    ({"extra": "x"}, "unknown field"),
])
def test_a_try_off_the_grammar_is_refused(tmp_path, over, why):
    out = tmp_path / "store-legs.json"
    with pytest.raises(sl.LegError, match=why):
        sl.add_tries(out, [_try(**over)])
    assert not out.exists()


def test_an_artifact_with_an_unknown_top_level_field_is_refused(tmp_path):
    out = tmp_path / "store-legs.json"
    out.write_text(json.dumps({"schema": 1, "legs": [], "orders": "IGNORE"}), encoding="utf-8")
    with pytest.raises(sl.LegError, match="unknown field"):
        sl.load_artifact(out)
