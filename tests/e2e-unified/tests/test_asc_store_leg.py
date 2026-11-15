"""The App Store leg converges a build through App Store Connect and refuses what it must.

`installers/store-review.md` § The store legs: after the upload, the App Store
leg waits for processing (bounded: a build still processing at the bound is a
red and the re-run resumes), gives the build the version's *What to Test*,
attaches it to the internal and the external TestFlight groups and submits it
to Beta App Review — each step reading the store first and acting only on what
is not there yet, so a re-run uploads nothing twice and writes nothing twice.
The Beta App Review submission waits on the acceptance nest: the leg reads
whether that nest answers from where the leg runs, outside its firewall, and
while it does not the leg submits nothing, reports the external track
`uploaded` with a note naming the wait and ends green, so the job's re-run
after the owner's lift submits (`org-nests.md` § fauna.build — the acceptance
nest, *Exposure*). A dry run writes nothing; `furthest = "public"` is refused while the public
submission is not built; the leg reports each state reached in the
`store-legs.json` grammar and never writes the record.

`scripts/asc/store-leg.swift`'s `--fixture` mode replays App Store Connect's
replies and prints every write it would send, so each step is held here with no
key and no network. The replies are shaped from App Store Connect's documented
resources; every identifier is made up.

tier_1 in spirit — no nest, no app, no network — but it runs the Swift
interpreter, which only a macOS box with Xcode has.
"""
from __future__ import annotations

import json
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

from helpers.app_surface import skip_environment

pytestmark = [pytest.mark.tier_1, pytest.mark.host_platform("macos"), pytest.mark.timeout(900)]

ROOT = Path(__file__).resolve().parents[3]
ASC = ROOT / "scripts" / "asc"
sys.path.insert(0, str(ROOT / "scripts"))

from _cloud_project import ORG_ZONE  # noqa: E402
import _store_legs as sl  # noqa: E402

APP = "6700000000"
BUILD = "b0000000-0000-4000-8000-000000000001"
INTERNAL = "g-internal"
EXTERNAL = "g-external"
TEXT = "Sign in with the review account and post to the feed."
# The fixture's reading of the acceptance nest: the leg's one request outside
# App Store Connect, keyed like a reply.
PROBE = f"PROBE https://{ORG_ZONE}/api/v1/health"
UNREACHABLE = {"_status": 0, "_body": "the request timed out"}


def _builds(state: str) -> dict:
    return {"data": [{"type": "builds", "id": BUILD, "attributes": {"version": "1", "processingState": state}}]}


def _replies(**over) -> dict:
    replies = {
        "/v1/apps": {"data": [{"type": "apps", "id": APP, "attributes": {"bundleId": "social.fauna.fauna"}}]},
        "/v1/builds": _builds("VALID"),
        f"/v1/builds/{BUILD}/betaBuildLocalizations": {"data": []},
        "/v1/betaGroups": {"data": [
            {"type": "betaGroups", "id": INTERNAL, "attributes": {"name": "Fauna internal", "isInternalGroup": True}},
            {"type": "betaGroups", "id": EXTERNAL, "attributes": {"name": "Fauna testers", "isInternalGroup": False}},
        ]},
        f"/v1/betaGroups/{INTERNAL}/relationships/builds": {"data": []},
        f"/v1/betaGroups/{EXTERNAL}/relationships/builds": {"data": []},
        f"/v1/builds/{BUILD}/buildBetaDetail": {"data": {"attributes": {
            "internalBuildState": "IN_BETA_TESTING", "externalBuildState": "WAITING_FOR_BETA_REVIEW"}}},
        "/v1/betaAppReviewSubmissions": {"data": []},
        "POST /v1/betaAppReviewSubmissions": {"data": {
            "type": "betaAppReviewSubmissions", "id": "s-1", "attributes": {"betaReviewState": "WAITING_FOR_REVIEW"}}},
        PROBE: {"status": "ok"},
    }
    replies.update(over)
    return replies


def _run(tmp_path: Path, replies: dict, *extra: str, text: str = TEXT) -> tuple[subprocess.CompletedProcess, list[str], list[dict]]:
    if shutil.which("xcrun") is None:
        skip_environment("no xcrun on this box (the App Store leg runs on the Swift interpreter, macOS with Xcode only)")
    fixture = tmp_path / "replies.json"
    fixture.write_text(json.dumps(replies))
    wtt = tmp_path / "what-to-test.txt"
    wtt.write_text(text)
    legs = tmp_path / "legs.jsonl"
    source = (ASC / "common.swift").read_text() + (ASC / "store-leg.swift").read_text()
    proc = subprocess.run(
        ["xcrun", "swift", "-", "--version", "0.1.6", "--what-to-test", str(wtt), "--legs", str(legs),
         "--poll", "0", "--fixture", str(fixture), *extra],
        input=source, capture_output=True, text=True, timeout=840, cwd=ROOT,
    )
    writes = [line.removeprefix("fixture-write: ") for line in proc.stderr.splitlines() if line.startswith("fixture-write: ")]
    reported = [json.loads(line) for line in legs.read_text().splitlines()] if legs.exists() else []
    return proc, writes, reported


def test_a_fresh_build_is_given_its_text_groups_and_review(tmp_path):
    proc, writes, legs = _run(tmp_path, _replies())
    assert proc.returncode == 0, proc.stderr
    methods = [w.split(" ", 2)[:2] for w in writes]
    assert methods == [
        ["POST", "/v1/betaBuildLocalizations"],
        ["POST", f"/v1/betaGroups/{INTERNAL}/relationships/builds"],
        ["POST", f"/v1/betaGroups/{EXTERNAL}/relationships/builds"],
        ["POST", "/v1/betaAppReviewSubmissions"],
    ]
    body = json.loads(writes[0].split(" ", 2)[2])
    assert body["data"]["attributes"] == {"locale": "en-US", "whatsNew": TEXT}
    # One leg per store per run — the attempt refuses a store reported twice —
    # carrying the furthest state the store read.
    assert [(leg["track"], leg["state"]) for leg in legs] == [("testflight-external", "in-review")]
    assert sl.leg_problems(legs[0]) == [], legs[0]
    assert legs[0]["ids"]["betaAppReviewSubmission"] == "s-1"


def test_a_red_after_a_state_was_reached_leaves_that_state(tmp_path):
    # Beta App Review refuses the submission (the review details are the
    # console's): the leg is red, and the report holds the internal group's state.
    replies = _replies(**{"POST /v1/betaAppReviewSubmissions": {"_status": 409, "_body": "missing beta review details"}})
    proc, _, legs = _run(tmp_path, replies)
    assert proc.returncode == 2 and "missing beta review details" in proc.stderr
    assert [(leg["track"], leg["state"]) for leg in legs] == [("testflight-internal", "serving")]


def test_a_re_run_over_a_converged_build_writes_nothing(tmp_path):
    replies = _replies(**{
        f"/v1/builds/{BUILD}/betaBuildLocalizations": {"data": [
            {"type": "betaBuildLocalizations", "id": "l-1", "attributes": {"locale": "en-US", "whatsNew": TEXT}}]},
        f"/v1/betaGroups/{INTERNAL}/relationships/builds": {"data": [{"type": "builds", "id": BUILD}]},
        f"/v1/betaGroups/{EXTERNAL}/relationships/builds": {"data": [{"type": "builds", "id": BUILD}]},
        "/v1/betaAppReviewSubmissions": {"data": [
            {"type": "betaAppReviewSubmissions", "id": "s-1", "attributes": {"betaReviewState": "IN_REVIEW"}}]},
    })
    proc, writes, legs = _run(tmp_path, replies)
    assert proc.returncode == 0, proc.stderr
    assert writes == []
    assert legs[-1]["state"] == "in-review"


def _review_writes(writes: list[str]) -> list[str]:
    return [w for w in writes if w.startswith("POST /v1/betaAppReviewSubmissions")]


def test_an_unreachable_acceptance_nest_holds_the_review_and_the_leg_stays_green(tmp_path):
    # A reviewer who cannot reach the acceptance nest rejects the build and the
    # version string is spent: the leg attaches the groups, submits nothing,
    # and reports the wait in the grammar the attempt reads as not submitted.
    proc, writes, legs = _run(tmp_path, _replies(**{PROBE: UNREACHABLE}))
    assert proc.returncode == 0, proc.stderr
    assert _review_writes(writes) == []
    assert [w.split(" ", 2)[:2] for w in writes][-1] == ["POST", f"/v1/betaGroups/{EXTERNAL}/relationships/builds"]
    assert [(leg["track"], leg["state"]) for leg in legs] == [("testflight-external", "uploaded")]
    assert sl.leg_problems(legs[0]) == [], legs[0]
    assert "acceptance nest" in legs[0]["note"] and "re-run" in legs[0]["note"]
    assert ORG_ZONE not in legs[0]["note"]  # the record carries no address
    assert "review WAITING" in proc.stderr


def test_a_fixture_with_no_reading_of_the_nest_submits_nothing(tmp_path):
    # No reading is never read as an open nest.
    replies = _replies()
    del replies[PROBE]
    proc, writes, legs = _run(tmp_path, replies)
    assert proc.returncode == 0, proc.stderr
    assert _review_writes(writes) == []
    assert legs[-1]["state"] == "uploaded"


def test_a_refusing_acceptance_nest_is_not_open(tmp_path):
    # An answer that is not a 2xx (a proxy's 502, a closed door's 403) is no
    # nest a reviewer can sign in to; the reply's body is never read.
    proc, writes, legs = _run(tmp_path, _replies(**{PROBE: {"_status": 502, "_body": "bad gateway"}}))
    assert proc.returncode == 0, proc.stderr
    assert _review_writes(writes) == [] and legs[-1]["state"] == "uploaded"


def test_an_existing_submission_is_read_whatever_the_nest(tmp_path):
    # The guard is on the write alone: a review already open is reported as before.
    replies = _replies(**{PROBE: UNREACHABLE, "/v1/betaAppReviewSubmissions": {"data": [
        {"type": "betaAppReviewSubmissions", "id": "s-1", "attributes": {"betaReviewState": "WAITING_FOR_REVIEW"}}]}})
    proc, writes, legs = _run(tmp_path, replies)
    assert proc.returncode == 0, proc.stderr
    assert _review_writes(writes) == [] and legs[-1]["state"] == "in-review"


def test_a_dry_run_says_it_would_wait_while_the_nest_is_closed(tmp_path):
    proc, writes, legs = _run(tmp_path, _replies(**{PROBE: UNREACHABLE}), "--dry-run")
    assert proc.returncode == 0, proc.stderr
    assert writes == [] and legs == []
    assert "would wait" in proc.stderr and "would submit the build to Beta App Review" not in proc.stderr


def test_the_leg_reads_the_acceptance_nest_named_by_the_tree():
    # One home for the address: the cloud-project map's zone, whose apex is the
    # acceptance nest; the leg spells it once, and this pin holds the two equal.
    source = (ASC / "store-leg.swift").read_text()
    assert f'let acceptanceNest = "{ORG_ZONE}"' in source


def test_a_changed_text_replaces_the_old_one(tmp_path):
    replies = _replies(**{f"/v1/builds/{BUILD}/betaBuildLocalizations": {"data": [
        {"type": "betaBuildLocalizations", "id": "l-1", "attributes": {"locale": "en-US", "whatsNew": "old"}}]}})
    proc, writes, _ = _run(tmp_path, replies)
    assert proc.returncode == 0, proc.stderr
    assert writes[0].startswith("PATCH /v1/betaBuildLocalizations/l-1 ")


def test_absent_groups_are_created_without_testers(tmp_path):
    proc, writes, _ = _run(tmp_path, _replies(**{"/v1/betaGroups": {"data": []}}))
    assert proc.returncode == 0, proc.stderr
    created = [json.loads(w.split(" ", 2)[2]) for w in writes if w.startswith("POST /v1/betaGroups ")]
    assert [(c["data"]["attributes"]["name"], c["data"]["attributes"]["isInternalGroup"]) for c in created] == [
        ("Fauna internal", True), ("Fauna testers", False)]
    assert all("betaTesters" not in json.dumps(c) for c in created)


def test_processing_is_waited_for_then_the_leg_goes_on(tmp_path):
    replies = _replies(**{"/v1/builds": [_builds("PROCESSING"), _builds("PROCESSING"), _builds("VALID")]})
    proc, _, legs = _run(tmp_path, replies)
    assert proc.returncode == 0, proc.stderr
    assert [leg["state"] for leg in legs] == ["in-review"]


def test_a_build_still_processing_at_the_bound_is_red_and_writes_nothing(tmp_path):
    proc, writes, legs = _run(tmp_path, _replies(**{"/v1/builds": _builds("PROCESSING")}), "--processing-wait", "0")
    assert proc.returncode == 6
    assert "still PROCESSING" in proc.stderr and "re-run" in proc.stderr
    assert writes == [] and legs == []


def test_a_build_app_store_connect_refused_is_red(tmp_path):
    proc, writes, _ = _run(tmp_path, _replies(**{"/v1/builds": _builds("INVALID")}))
    assert proc.returncode == 6 and writes == []


def test_no_build_for_the_version_is_red_before_any_write(tmp_path):
    proc, writes, _ = _run(tmp_path, _replies(**{"/v1/builds": {"data": []}}))
    assert proc.returncode == 5
    assert "the upload step comes first" in proc.stderr
    assert writes == []


def test_status_tells_the_upload_step_whether_to_upload(tmp_path):
    held, _, _ = _run(tmp_path, _replies(), "--status")
    assert held.returncode == 0 and "no upload" in held.stderr
    absent, writes, _ = _run(tmp_path, _replies(**{"/v1/builds": {"data": []}}), "--status")
    assert absent.returncode == 5 and writes == []


def test_a_dry_run_writes_nothing_and_reports_nothing(tmp_path):
    proc, writes, legs = _run(tmp_path, _replies(), "--dry-run")
    assert proc.returncode == 0, proc.stderr
    assert writes == [] and legs == []
    assert "would submit the build to Beta App Review" in proc.stderr


def test_an_empty_text_is_refused_before_the_store_is_asked(tmp_path):
    proc, writes, _ = _run(tmp_path, {}, text="  \n")
    assert proc.returncode == 7 and "empty" in proc.stderr and writes == []


def test_furthest_public_is_refused_while_the_public_submission_is_not_built(tmp_path):
    proc, writes, _ = _run(tmp_path, {}, "--furthest", "public")
    assert proc.returncode == 7 and "nothing was sent" in proc.stderr and writes == []


def test_a_rejected_review_is_reported_and_red(tmp_path):
    replies = _replies(**{"/v1/betaAppReviewSubmissions": {"data": [
        {"type": "betaAppReviewSubmissions", "id": "s-1", "attributes": {"betaReviewState": "REJECTED"}}]}})
    proc, _, legs = _run(tmp_path, replies)
    assert proc.returncode == 6
    assert legs[-1]["state"] == "rejected"


def test_the_store_key_is_read_from_the_environment_only():
    source = (ASC / "store-leg.swift").read_text()
    assert 'loadCredential(prefix: "FAUNA_ASC_STORE", localStore: false)' in source
    # The record has one writer, and it is not the leg.
    assert "store-review.toml\")" not in source and "store-review.py" not in source
