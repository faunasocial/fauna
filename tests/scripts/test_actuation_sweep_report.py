"""Pin `scripts/actuation-sweep-report.py` — the instrument that grades a
permissive actuation sweep.

Convention 11's whole method rests on one uncomfortable fact: **a sweep that
found nothing and a sweep whose detector never armed produce identical logs.**
apple's first iOS sweep read *1483 tests observed, 0 violating calls — CLEAN*
while 567 of its tests had silently skipped and the app never launched. The
report exists to make that confusion impossible, and its verdict is what
decides whether an app flips its actuation gate to refusing-by-default.

That makes the report a load-bearing instrument standing between an 8-hour run
and an irreversible product decision — and it had **no tests at all** while
every sibling script in this directory has them. The gap was not theoretical:
signal 3 shipped stamping a chunk's tree *after* the chunk ran, so a genuine
mid-run rebase could read as clean, and that bug was caught by eye hours later
rather than by a test.

The properties pinned here are the ones whose failure is silent:

1. **An unarmed detector is never called clean** — including when it is armed
   on only *some* of the routes its probe drives. Convention 11 is explicit
   that gating click alone is not compliance, so a probe covering click and
   select must be witnessed on both before the log counts as an enumeration.
2. **An empty offender list only means something when the run was whole** —
   unfinished, split-tree and straddled runs each get their own refusal.
3. **Triage is per element, not per call** — a single element with twelve
   calls is one decision, not twelve.
4. **A test asserting the refusal on purpose is exempt by (test module,
   element), never by element alone** — the same element driven from any
   other test is still a real offender.
"""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

_SCRIPT = (
    Path(__file__).resolve().parents[2] / "scripts" / "actuation-sweep-report.py"
)


def _load():
    spec = importlib.util.spec_from_file_location("actuation_sweep_report", _SCRIPT)
    mod = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = mod
    spec.loader.exec_module(mod)
    return mod


rep = _load()

# The windows probe drives a disabled button AND a disabled picker, on purpose,
# to cover both the click and the select route.
CLICK_PROBE, SELECT_PROBE = rep._PROBE_ELEMENTS["windows"]


def _log(tmp_path, *lines, name="markers.log"):
    p = tmp_path / name
    p.write_text("\n".join(lines) + "\n", encoding="utf-8")
    return p


def _mark(route, element):
    return f"[InProcessAutomation] DISABLED-ACTUATION {route} id={element} index=0"


def _test(nodeid):
    return f"=== TEST {nodeid}"


def _armed(*extra):
    """A marker log whose probe is fully witnessed, plus whatever else."""
    return (
        _test("tests/test_windows_disabled_actuation.py::test_a_button[windows]"),
        _mark("click", CLICK_PROBE),
        _test("tests/test_windows_disabled_actuation.py::test_a_picker[windows]"),
        _mark("select", SELECT_PROBE),
        *extra,
    )


def _progress(tmp_path, rows, name="progress.jsonl"):
    import json

    p = tmp_path / name
    p.write_text(
        "".join(json.dumps(r) + "\n" for r in rows), encoding="utf-8"
    )
    return p


def _chunk(index, tree, *, tree_start=None, rc=0):
    row = {"index": index, "name": f"c{index}", "rc": rc, "tree": tree}
    if tree_start is not None:
        row["tree_start"] = tree_start
    return row


def _run(capsys, marker_log, *argv):
    sys.argv = ["actuation-sweep-report.py", str(marker_log), *argv]
    rc = rep.main()
    return rc, capsys.readouterr().out


# ---------------------------------------------------------------------------
# Property 1 — an unarmed detector is never called clean.
# ---------------------------------------------------------------------------


def test_a_log_with_no_probe_marker_is_refused_as_an_enumeration(tmp_path, capsys):
    """The apple-iOS failure: silent skips read exactly like a clean sweep."""
    log = _log(tmp_path, _test("tests/test_feed.py::test_x[windows]"))
    rc, out = _run(capsys, log, "--app", "windows")
    assert rc == 2
    assert "❌ ABSENT" in out
    assert "INVALID as an enumeration" in out


def test_an_unwitnessed_probe_route_does_not_count_as_an_armed_detector(
    tmp_path, capsys
):
    """Signal 2 must witness EVERY route the probe drives, not just the first.

    Convention 11 is explicit that gating click alone is not compliance:
    typing into, or selecting from, a disabled control is the same illegal
    act. The windows probe therefore drives a disabled button *and* a disabled
    picker. If only the click witness reaches the log, the select detector is
    unproven — and an empty offender list for every select call in the sweep
    is then indistinguishable from a select gate that never ran at all, which
    is the precise confusion signal 2 exists to end.
    """
    log = _log(
        tmp_path,
        _test("tests/test_windows_disabled_actuation.py::test_a_button[windows]"),
        _mark("click", CLICK_PROBE),
    )
    rc, out = _run(capsys, log, "--app", "windows")
    assert rc == 2, "a half-armed detector must not grade as a valid enumeration"
    assert SELECT_PROBE in out, "the report must name the unwitnessed probe element"


def test_a_fully_witnessed_probe_with_no_offenders_is_ready_to_flip(
    tmp_path, capsys
):
    log = _log(tmp_path, *_armed())
    rc, out = _run(capsys, log, "--app", "windows")
    assert rc == 0
    assert "✅ PRESENT" in out
    assert "ready to flip" in out


def test_an_app_with_no_registered_probe_is_unknown_not_clean(tmp_path, capsys):
    log = _log(tmp_path, _test("tests/test_feed.py::test_x[plan9]"))
    rc, out = _run(capsys, log, "--app", "plan9")
    assert rc == 2
    assert "UNKNOWN" in out


# ---------------------------------------------------------------------------
# Property 2 — an empty offender list only means something when the run was whole.
# ---------------------------------------------------------------------------


def test_an_unfinished_sweep_is_partial_even_with_an_empty_offender_list(
    tmp_path, capsys
):
    log = _log(tmp_path, *_armed())
    prog = _progress(tmp_path, [_chunk(0, "a" * 40), _chunk(1, "a" * 40)])
    rc, out = _run(
        capsys, log, "--app", "windows",
        "--progress", str(prog), "--expect-chunks", "5",
    )
    assert rc == 3
    assert "❌ INCOMPLETE" in out
    assert "--start-at 2" in out, "the report must name where to resume"
    assert "PARTIAL" in out


def test_a_split_tree_run_is_refused_although_every_chunk_finished(
    tmp_path, capsys
):
    """A module swept before a rebase was never measured against shipped code."""
    log = _log(tmp_path, *_armed())
    prog = _progress(tmp_path, [_chunk(0, "a" * 40), _chunk(1, "b" * 40)])
    rc, out = _run(
        capsys, log, "--app", "windows",
        "--progress", str(prog), "--expect-chunks", "2",
    )
    assert rc == 4
    assert "❌ SPLIT" in out
    assert "SPLIT — the detector was armed" in out


def test_a_chunk_whose_tree_moved_under_it_mid_run_is_caught(tmp_path, capsys):
    """The regression that shipped: stamping the tree only AFTER a chunk ran.

    Both stamps taken at the end agree, so a rebase landing *inside* a chunk
    read as a single-tree run. `tree_start` is the witness; without this test
    the fix is one careless edit from being undone.
    """
    log = _log(tmp_path, *_armed())
    prog = _progress(
        tmp_path, [_chunk(0, "b" * 40, tree_start="a" * 40)]
    )
    rc, out = _run(
        capsys, log, "--app", "windows",
        "--progress", str(prog), "--expect-chunks", "1",
    )
    assert rc == 4
    assert "❌ STRADDLED" in out


def test_an_unusual_chunk_exit_code_is_surfaced(tmp_path, capsys):
    """rc 0/1 are pass/fail; anything else means the chunk did not report."""
    log = _log(tmp_path, *_armed())
    prog = _progress(tmp_path, [_chunk(0, "a" * 40, rc=137)])
    _, out = _run(
        capsys, log, "--app", "windows",
        "--progress", str(prog), "--expect-chunks", "1",
    )
    assert "unusual exit code" in out
    assert "0:rc=137" in out


# ---------------------------------------------------------------------------
# Property 3 — triage is per element, not per call.
# ---------------------------------------------------------------------------


def test_the_probes_own_elements_are_never_offered_as_offenders(tmp_path, capsys):
    """The probe drives disabled controls BY DESIGN on every route it covers."""
    log = _log(tmp_path, *_armed())
    rc, out = _run(capsys, log, "--app", "windows")
    assert rc == 0
    assert "offender list EMPTY" in out


def test_one_element_driven_many_times_is_one_triage_decision(tmp_path, capsys):
    log = _log(
        tmp_path,
        *_armed(
            _test("tests/test_feed.py::test_a[windows]"),
            _mark("click", "publish-button"),
            _test("tests/test_feed.py::test_b[windows]"),
            _mark("type", "publish-button"),
            _mark("click", "publish-button"),
        ),
    )
    rc, out = _run(capsys, log, "--app", "windows")
    assert rc == 1
    assert "publish-button  — 3 call(s), routes: click, type" in out
    assert "1 element(s) to triage." in out


def test_every_test_that_drove_an_offender_is_named(tmp_path, capsys):
    """Triage starts from the tests, so the report must hand them over."""
    log = _log(
        tmp_path,
        *_armed(
            _test("tests/test_feed.py::test_a[windows]"),
            _mark("click", "publish-button"),
            _test("tests/test_feed.py::test_b[windows]"),
            _mark("click", "publish-button"),
        ),
    )
    _, out = _run(capsys, log, "--app", "windows")
    assert "tests/test_feed.py::test_a[windows]" in out
    assert "tests/test_feed.py::test_b[windows]" in out


def test_a_test_asserting_the_refusal_itself_is_not_an_offender(tmp_path, capsys):
    """Some journeys drive a disabled control ON PURPOSE to assert the refusal.

    `test_conversation_room_roles.py` checks that a plain member's Remove chip
    is refused, not merely greyed. Under `--permissive-actuation` the gate
    marks that call like any other, so every permissive sweep on every room app
    logs it. Offered as an offender, it hands every future grader the same
    phantom triage the windows sweep of 2026-09-11 did.
    """
    log = _log(
        tmp_path,
        *_armed(
            _test(
                "tests/test_conversation_room_roles.py::"
                "test_a_room_is_born_governed_and_its_roles_govern_removal[windows]"
            ),
            _mark("click", "thread-member-chip"),
        ),
    )
    rc, out = _run(capsys, log, "--app", "windows")
    assert rc == 0, out
    assert "offender list EMPTY" in out
    assert "DELIBERATE" in out, "the deliberate drive is reported, not hidden"
    assert "thread-member-chip" in out


def test_the_same_element_driven_from_any_other_test_is_still_an_offender(
    tmp_path, capsys
):
    """The exemption is (test module, element), never the element alone."""
    log = _log(
        tmp_path,
        *_armed(
            _test("tests/test_conversations.py::test_remove_a_member[windows]"),
            _mark("click", "thread-member-chip"),
        ),
    )
    rc, out = _run(capsys, log, "--app", "windows")
    assert rc == 1
    assert "thread-member-chip  — 1 call(s), routes: click" in out


def test_an_offender_found_by_an_unarmed_detector_is_still_not_an_enumeration(
    tmp_path, capsys
):
    """A real offender is real; the EMPTY list around it is what is worthless.

    So the offender stays listed — it is evidence either way — while the
    verdict still refuses the log as an enumeration.
    """
    log = _log(
        tmp_path,
        _test("tests/test_feed.py::test_a[windows]"),
        _mark("click", "publish-button"),
    )
    rc, out = _run(capsys, log, "--app", "windows")
    assert rc == 2
    assert "INVALID as an enumeration" in out
    assert "publish-button" in out, "a real offender is still worth listing"


def test_an_empty_offender_list_under_an_unarmed_detector_says_so(tmp_path, capsys):
    """The dangerous pair: nothing found, and no proof anything could be."""
    log = _log(tmp_path, _test("tests/test_feed.py::test_a[windows]"))
    rc, out = _run(capsys, log, "--app", "windows")
    assert rc == 2
    assert "detector is UNPROVEN" in out
