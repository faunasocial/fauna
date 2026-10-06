"""The post-frame invariant catalogue — convention 17's layer (b).

`docs/goal/architecture/e2e-conventions.md` § point 17. `helpers/frame_invariants.py`
rides every e2e test and checks a small catalogue of GENERAL properties on the
frame each test leaves behind, because a hand-authored suite asserts only the
outcomes someone thought to write down.

These are tier_1: the catalogue is pure functions over a state dict, so its
rulings can be pinned without a nest, a driver or an app. That separation is the
point — the invariants are checked against real frames by riding the tier_3
suite, but the *judgements about what counts as a violation* are decided here,
where they are cheap to keep honest.

**The ruling this file exists to protect: `focused_line_count == 0` is LEGITIMATE.**
A fresh authenticated tui session starts in `Zone::Sidebar`, where the page's
focus ring does not hold the keyboard and the count reads 0 — which is why
`tests/test_tui_settings_focus.py` calls `switch_pane` before asserting anything.
The obvious phrasing of the invariant ("exactly one line paints focused") fires
on most of the suite, gets called noise, and gets the whole checker deleted. The
real invariant is `<= 1`, and `test_zero_focused_lines_is_not_a_violation` below
is what stops a future session from "tightening" it back into uselessness.
"""

import pytest

pytestmark = [pytest.mark.tier1, pytest.mark.tier_1]

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from helpers import frame_invariants  # noqa: E402


#: The corpus path a summary names. Compared as `str(_CORPUS)`: the summary
#: prints the native form, which is `\h\c.log` on Windows.
_CORPUS = Path("/h/c.log")


def _violations(state):
    return frame_invariants.evaluate(state)[0]


def _names(state):
    return [v.split(":")[0] for v in _violations(state)]


# ── focus-fanout ────────────────────────────────────────────────────────────


def test_the_real_collision_bug_is_caught():
    """Many lines painting focused at once — the defect a live human found.

    tui compared focus by element id STRING and the Settings rail's ~19 rows all
    carry an empty id, so every one of them painted highlighted together. This is the shape that reached the state surface.
    """
    assert _names({"focused_line_count": 19}) == ["focus-fanout"]


def test_zero_focused_lines_is_not_a_violation():
    """⚠ Load-bearing, and the reason the whole checker survives.

    Focus in the sidebar zone paints no page line at all. If this ever starts
    failing because someone tightened the bound to `== 1`, the checker will fire
    on most of the suite, be declared noise, and be deleted — which is exactly
    how convention 17 says a checker dies. Do not "fix" this test.
    """
    assert _violations({"focused_line_count": 0}) == []


def test_one_focused_line_is_not_a_violation():
    assert _violations({"focused_line_count": 1}) == []


def test_a_non_integer_count_is_a_violation():
    """The field losing its type is as blinding as the bug it exists to catch."""
    assert _names({"focused_line_count": "1"}) == ["focus-fanout"]


def test_a_bool_is_not_accepted_as_a_count():
    """`True` is an `int` in Python — an accident that would silently pass."""
    assert _names({"focused_line_count": True}) == ["focus-fanout"]


# ── messages-readable ───────────────────────────────────────────────────────


def test_an_error_being_present_is_never_a_violation():
    """Many tests assert an error. Only the surface's READABILITY is invariant."""
    assert _violations({"messages": {"error": "nest unreachable"}}) == []


def test_a_null_error_is_not_a_violation():
    assert _violations({"messages": {"error": None}}) == []


def test_the_message_surface_losing_its_shape_is_a_violation():
    assert _names({"messages": "nest unreachable"}) == ["messages-readable"]


def test_a_non_text_error_is_a_violation():
    """`error_text()`/`has_error()` read this — convention 2 and 6 both route
    through it, so a shape break takes the suite's self-diagnosis with it."""
    assert _names({"messages": {"error": {"code": 500}}}) == ["messages-readable"]


def test_omitting_messages_is_how_an_app_says_read_the_dom_instead():
    """The cross-app contract the first GUI-driver corpus forced (2026-08-16).

    An app whose frame has no authoritative state-level message surface must
    OMIT `messages`, not publish it as `null`. Both read identically to
    `_message_from_state` — `state.get` collapses them, and either falls back to
    the DOM `error-message` element — but the checker splits them on purpose:
    absent is a per-app n/a, published-null is shape loss
    (`test_a_published_null_is_a_violation_not_an_absence`, red-verified against
    a tui mutation that serialized as null on a no-error frame).

    web published `null` to mean "defer to the DOM" and so violated this
    invariant on 10 of 10 frames the moment layer (b) first observed a GUI
    driver. `web-bridge/agent.js` now omits the key; this pin is what stops a
    future reader "simplifying" it back into the collision, and what stops the
    opposite fix — weakening the checker to accept null, which would hand tui's
    red-verified catch back.
    """
    assert _violations({"conv_receive_cycles": [0, 0]}) == []
    assert _names({"messages": None}) == ["messages-readable"]


# ── nav-stack ───────────────────────────────────────────────────────────────


def test_a_healthy_nav_stack_is_not_a_violation():
    assert _violations({"nav": {"stack": [{"view": "feed"}]}}) == []


def test_an_empty_nav_stack_is_a_violation():
    assert _names({"nav": {"stack": []}}) == ["nav-stack"]


def test_a_stack_entry_naming_no_view_is_a_violation():
    assert _names({"nav": {"stack": [{"view": ""}]}}) == ["nav-stack"]
    assert _names({"nav": {"stack": [{}]}}) == ["nav-stack"]


# ── receive-loop-alive ──────────────────────────────────────────────────────


def test_a_panicked_receive_loop_is_caught():
    """The native loop died by panic — the dead-rail class this entry exists for."""
    frame = {"conv_receive_cycles": {"started": 3, "completed": 2, "exit": "panicked"}}
    assert _names(frame) == ["receive-loop-alive"]


def test_a_stalled_web_pump_is_caught():
    """Web's shape of the same death: a pass outran the pump's ceiling.

    The three-day web outage was exactly this — the promise of a panicking wasm
    task never settles, so the pump sat busy forever with nothing to say so.
    """
    frame = {"conv_receive_cycles": {"started": 1, "completed": 0, "exit": "stalled"}}
    assert _names(frame) == ["receive-loop-alive"]


def test_a_running_loop_is_not_a_violation():
    frame = {"conv_receive_cycles": {"started": 4, "completed": 4, "exit": None}}
    assert _violations(frame) == []


@pytest.mark.parametrize("designed", ["closed", "retired"])
def test_a_designed_exit_is_not_a_violation(designed):
    """⚠ Load-bearing: a loop legitimately leaves when its session closes or its
    engine is handed over. A checker that fired on any exit — the phrasing
    "the receive loop is alive" suggests — would fire on every account switch and
    every re-login, be called noise, and be deleted."""
    frame = {"conv_receive_cycles": {"started": 2, "completed": 2, "exit": designed}}
    assert _violations(frame) == []


def test_zero_counters_before_any_session_are_not_a_violation():
    """Pre-login frames publish zeros — a legitimate state, not a dead loop."""
    frame = {"conv_receive_cycles": {"started": 0, "completed": 0, "exit": None}}
    assert _violations(frame) == []


def test_counters_without_an_exit_field_are_na():
    """An app that publishes the counters but not yet the exit leg is `n/a` —
    neither a clean bill of health nor a violation."""
    violations, not_applicable = frame_invariants.evaluate(
        {"conv_receive_cycles": {"started": 1, "completed": 1}}
    )
    assert violations == []
    assert "receive-loop-alive" in not_applicable


def test_a_published_null_receive_cycles_is_na_by_the_keys_own_contract():
    """Unlike `messages`, a published `null` here is NOT a lost shape: the key's
    contract (`fauna_e2e_agent::CONV_RECEIVE_CYCLES_KEY`) reserves `null` for
    "this app has no leg", and web's bridge publishes exactly that when its
    hook is absent."""
    violations, not_applicable = frame_invariants.evaluate({"conv_receive_cycles": None})
    assert violations == []
    assert "receive-loop-alive" in not_applicable


def test_an_unknown_exit_word_is_a_violation():
    """A new exit word nobody taught the checker must not quietly read as alive."""
    frame = {"conv_receive_cycles": {"started": 1, "completed": 1, "exit": "gone"}}
    assert _names(frame) == ["receive-loop-alive"]


# ── cross-app safety: an unpublished field is n/a, never a violation ────────


def test_an_app_publishing_nothing_violates_nothing():
    """The checker rides all 7 apps from day one.

    An app that has not built the convention-11 twin yet is a queued slice, not
    a defect on the frame in front of us. If a missing key counted as a
    violation, every non-tui app would report on every test and the trace would
    be unreadable on the day it shipped.
    """
    violations, not_applicable = frame_invariants.evaluate({})
    assert violations == []
    assert set(not_applicable) == {name for name, _ in frame_invariants.INVARIANTS}


def test_a_toolkit_app_is_fully_covered_by_the_two_shape_invariants():
    """The shape every GUI app presents: `messages` + `nav`, no focus count.

    ⚠ That `focus-fanout` n/a is PERMANENT and reasoned, not a queued leg (ruled
    2026-08-15). tui is the only app that paints its own focus affordance,
    because it is the only one without a toolkit that owns focus — GTK, the DOM,
    WinUI, SwiftUI and Compose each guarantee at most one focused element per
    root, so the identity-collision class cannot arise there. linux already
    matches this shape and is covered by the checker with no per-app work.
    """
    violations, not_applicable = frame_invariants.evaluate(
        {
            "messages": {"error": None},
            "nav": {"stack": [{"view": "feed"}]},
            # linux publishes the shared receive-cycle JSON too, exit included.
            "conv_receive_cycles": {"started": 1, "completed": 1, "exit": None},
        }
    )
    assert violations == []
    assert not_applicable == ["focus-fanout", "region-block-never-silent"]


def test_an_unobservable_frame_is_not_a_violation():
    """No agent, app not up: the fixture's business, never a frame violation."""
    assert frame_invariants.evaluate(None) == (
        [],
        [
            "focus-fanout",
            "messages-readable",
            "nav-stack",
            "receive-loop-alive",
            "region-block-never-silent",
        ],
    )


# ── region-block-never-silent ───────────────────────────────────────────────
#
# region-blocking.md § What the build owes in tests: wherever the engine says
# `Block` for the region source, the placeholder is present on that surface.


def test_a_region_block_without_its_placeholder_is_caught():
    frame = {"region_block_render": {"blocked": 2, "placeholders": 1}}
    assert _names(frame) == ["region-block-never-silent"]


def test_every_region_block_with_its_placeholder_passes():
    assert _names({"region_block_render": {"blocked": 2, "placeholders": 2}}) == []
    assert _names({"region_block_render": {"blocked": 0, "placeholders": 0}}) == []


def test_extra_placeholders_are_not_a_violation():
    """A placeholder can outlive the frame's own verdict count (a surface reusing a
    verdict computed elsewhere); only a block with NO placeholder is silent."""
    assert _names({"region_block_render": {"blocked": 1, "placeholders": 3}}) == []


def test_a_malformed_region_block_render_is_a_violation():
    assert _names({"region_block_render": None}) == ["region-block-never-silent"]
    assert _names({"region_block_render": {"blocked": "2", "placeholders": 2}}) == [
        "region-block-never-silent"
    ]


# ── absent vs. published-as-null ────────────────────────────────────────────
#
# Found by RED-VERIFICATION, not by review, and it is the bug this checker was
# most likely to ship with. `state.get(k)` collapses "this app does not publish
# k" and "k is present and null" into the same `None` — and those are opposite
# verdicts: the first is a queued per-app leg, the second is the field having
# lost its shape. The live mutation that made tui's `messages` a bare string
# happened to serialize as null on a frame with no error, and the checker
# reported `n/a` — silently declining to catch the very defect it was aimed at.


def test_a_published_null_is_a_violation_not_an_absence():
    assert _names({"messages": None}) == ["messages-readable"]
    assert _names({"nav": None}) == ["nav-stack"]
    assert _names({"focused_line_count": None}) == ["focus-fanout"]


def test_a_null_nav_stack_is_a_violation_not_an_absence():
    """One level deeper, same collapse: `nav` present, `stack` published null."""
    assert _names({"nav": {"stack": None}}) == ["nav-stack"]


def test_a_genuinely_absent_field_is_still_na():
    """The other half of the distinction — regression guard on the fix itself."""
    violations, not_applicable = frame_invariants.evaluate({"session": {}})
    assert violations == []
    assert set(not_applicable) == {name for name, _ in frame_invariants.INVARIANTS}


# ── the checker can never fail the run it observes ──────────────────────────


def test_a_raising_checker_is_reported_not_propagated(monkeypatch):
    """A checker that can break a run it was only supposed to watch would be
    switched off fleet-wide after its first bad day."""

    def explodes(_state):
        raise RuntimeError("boom")

    monkeypatch.setattr(
        frame_invariants, "INVARIANTS", (("exploding", explodes),), raising=True
    )
    violations, _ = frame_invariants.evaluate({"anything": 1})
    assert violations == ["exploding: checker raised RuntimeError: boom"]


# ── the trace line ──────────────────────────────────────────────────────────


def test_a_clean_frame_traces_ok():
    assert frame_invariants.format_line("t.py::a", [], []) == "OK\tt.py::a\n"


def test_a_violation_line_carries_the_detail_and_the_test_verdict():
    line = frame_invariants.format_line(
        "t.py::a", ["focus-fanout: 19 lines paint focused at once (at most 1 may)"], [], "failed"
    )
    assert line.startswith("VIOLATION[focus-fanout: 19 lines paint focused")
    # A violation on the frame left by an already-failing test is usually a
    # consequence; triage that cannot see the difference drowns.
    assert "test=failed" in line
    assert line.endswith("\tt.py::a\n")


def test_a_passing_test_does_not_clutter_the_line_with_its_verdict():
    assert "test=" not in frame_invariants.format_line("t.py::a", [], [], "passed")


def test_an_unreadable_frame_never_traces_as_ok():
    """The one failure mode a checker like this must never have.

    A run where every frame went UNOBSERVED and a run where every frame was
    clean are the same file otherwise — and the first is a broken probe
    reporting success, which is worse than no probe at all. So an unreadable
    frame gets its own verdict word, and its reason travels with it.
    """
    line = frame_invariants.format_line(
        "t.py::a", [], ["focus-fanout"], "passed", unobserved="BridgeDead: gone"
    )
    assert line.startswith("UNOBSERVED[BridgeDead: gone]")
    assert not line.startswith("OK")
    # The n/a tally is meaningless when nothing was read — it would read as
    # "this app publishes no fields" rather than "we never looked".
    assert "n/a=" not in line


# ── where the corpus lives ──────────────────────────────────────────────────
#
# Layer (b) shipped 2026-08-15 off-unless-`FAUNA_E2E_FRAME_INVARIANTS`-names-a-file,
# with promotion gated on "a clean full `--app sweep`". Measured 2026-08-15: an
# `--app sweep` collects 3757 tests on ONE dev machine's app set, and the other two
# machines carry their own — so the gate is a day-scale act nobody performs first, and
# while the probe is off by default the fleet's continuous e2e running contributes
# nothing toward it. An observation nobody makes is not coverage. So observation
# is the DEFAULT now, and the env var became an override rather than a switch.


def test_no_env_means_observe_into_the_default_corpus():
    """The flip that makes the corpus exist at all.

    Every session's ordinary e2e run now contributes frames, which is the only
    way the promotion evidence ever accumulates.
    """
    path = frame_invariants.resolve_corpus_path(None, Path("/h"))
    assert path == Path("/h") / frame_invariants.DEFAULT_CORPUS_NAME


def test_an_explicit_path_still_wins():
    """The pre-flip contract: a named file is where the trace goes, verbatim."""
    assert frame_invariants.resolve_corpus_path("/tmp/mine.log", Path("/h")) == Path("/tmp/mine.log")


@pytest.mark.parametrize("token", ["0", "off", "no", "false", "", "  OFF  ", "None"])
def test_the_opt_out_survives_the_flip(token):
    """Default-on is only defensible while turning it off stays one env var.

    A probe that cannot be switched off is one bad day away from being deleted
    from the harness entirely — the same fate the `== 1` phrasing of
    `focus-fanout` would have earned.
    """
    assert frame_invariants.resolve_corpus_path(token, Path("/h")) is None


def test_a_path_that_merely_looks_like_an_off_token_is_a_path():
    """`./off` is a file someone chose, not a request to stop observing."""
    assert frame_invariants.resolve_corpus_path("./off", Path("/h")) == Path("./off")


# ── what the run says about itself ──────────────────────────────────────────
#
# The corpus is a file, and a file nobody opens is the same as no observation.
# The tally is what makes the run report its own verdict where the session that
# produced it can see it, without ever failing that run.


def test_a_run_that_observed_nothing_says_nothing():
    """A tier_1-only run must not grow a line about a probe that never ran."""
    assert frame_invariants.RunTally().summary(_CORPUS) == []


def test_a_clean_run_reports_its_frame_count_and_where_the_corpus_is():
    tally = frame_invariants.RunTally()
    for i in range(3):
        tally.record(f"t.py::a{i}", [], None)
    lines = tally.summary(_CORPUS)
    assert len(lines) == 1
    assert "3 frames observed" in lines[0]
    assert "0 violated" in lines[0]
    assert str(_CORPUS) in lines[0]


def test_a_violated_frame_is_named_in_the_summary():
    """The whole point of the flip: the session that caused it, sees it.

    pytest swallows fixture prints on passing tests — and the test that ends on
    a violating frame usually PASSES, because the violation is latent. The
    terminal summary is the one surface that is not swallowed.
    """
    tally = frame_invariants.RunTally()
    tally.record("t.py::clean", [], None)
    tally.record("t.py::bad", ["focus-fanout: 19 lines paint focused at once"], None)
    lines = tally.summary(_CORPUS)
    assert "2 frames observed" in lines[0]
    assert "1 violated" in lines[0]
    assert any("t.py::bad" in line and "focus-fanout" in line for line in lines[1:])
    assert not any("t.py::clean" in line for line in lines[1:])


def test_a_run_where_every_frame_went_unobserved_never_reads_as_clean():
    """The one failure mode a checker must never have, at run scope.

    `format_line` already refuses to trace an unread frame as OK. The same
    refusal has to hold for the summary, or a wholly broken probe reports a
    clean run — which is worse than no probe, because it is believed.
    """
    tally = frame_invariants.RunTally()
    for i in range(4):
        tally.record(f"t.py::a{i}", [], "BridgeDead: gone")
    lines = tally.summary(_CORPUS)
    assert lines, "a run that read no frame at all must still report"
    assert "0 frames observed" in lines[0]
    assert "4 unobserved" in lines[0]


def test_a_healthy_run_does_not_carry_an_unobserved_tally():
    tally = frame_invariants.RunTally()
    tally.record("t.py::a", [], None)
    assert "unobserved" not in tally.summary(_CORPUS)[0]


# ── whose frame is it ───────────────────────────────────────────────────────
#
# Layer (b) hung off the conftest `app` fixture's teardown when it shipped, and
# the first real run measured what that actually reached: `test_navigation.py`
# defines its OWN class-level `app` over `persistent_app` (:50), so the probe
# never fired once across 9 green tests and wrote no corpus at all. Seven
# modules shadow `app` that way and ten touch `persistent_app` — including the
# state-protocol and UI-honesty modules, which are the frame-richest in the
# suite. "Rides every existing e2e test" has to mean every test with a driver,
# so the probe resolves the driver from the test's OWN fixtures instead of
# assuming which fixture produced it.


class _FakeDriver:
    """A driver is what answers `get_state` and `reset` — the two the harness drives."""

    def get_state(self, *a, **k):
        return {}

    def reset(self):
        pass


class _FakeActionLayer:
    def __init__(self, driver):
        self.driver = driver


def test_a_test_with_no_driver_at_all_is_not_probed():
    """API-only and pure tier_1 tests have no frame; they must not be reached for one."""
    assert frame_invariants.driver_for({"nest_instance": object()}) is None


def test_the_conftest_app_fixture_is_found():
    driver = _FakeDriver()
    assert frame_invariants.driver_for({"app": _FakeActionLayer(driver)}) is driver


def test_a_module_that_shadows_app_over_persistent_app_is_still_reached():
    """The measured miss: `test_navigation.py`'s own `app` fixture.

    It resolves to the same ActionLayer shape, so resolving by VALUE rather than
    by which fixture built it is what closes the hole — and it closes the
    `persistent_app`-directly modules in the same move.
    """
    driver = _FakeDriver()
    assert frame_invariants.driver_for({"persistent_app": _FakeActionLayer(driver)}) is driver


def test_a_bare_driver_value_counts_as_a_driver():
    driver = _FakeDriver()
    assert frame_invariants.driver_for({"app": driver}) is driver


def test_the_preference_order_beats_both_dict_order_and_alphabetical_order():
    """When a test holds several seats, the most specific one is probed.

    ⚠ The names here are chosen to DISCRIMINATE, and that is the whole point of
    the test: `admin_app` sorts before `logged_in_app` alphabetically while the
    preference puts `logged_in_app` first, so a fallback that merely scans names
    in sorted order answers differently. An earlier draft of this pin used
    `logged_in_app`/`persistent_app`, whose alphabetical and preference orders
    coincide — it passed against a mutant with the preference loop deleted
    outright, i.e. it graded nothing. Mutation-grading is what caught that.
    """
    preferred, other = _FakeDriver(), _FakeDriver()
    funcargs = {"admin_app": _FakeActionLayer(other), "logged_in_app": _FakeActionLayer(preferred)}
    assert frame_invariants.driver_for(funcargs) is preferred
    assert frame_invariants.driver_for(dict(reversed(list(funcargs.items())))) is preferred


def test_a_driver_under_an_unknown_fixture_name_is_still_found():
    """The preference list is an ordering, never the membership test.

    A fixture named for a future app would otherwise go unobserved silently —
    the failure mode where the probe reports a clean run it never looked at.
    """
    driver = _FakeDriver()
    assert frame_invariants.driver_for({"some_future_seat": _FakeActionLayer(driver)}) is driver


def test_a_non_driver_object_is_never_mistaken_for_one():
    class HalfShaped:  # answers get_state, but is not a driver
        def get_state(self):
            return {}

    assert frame_invariants.driver_for({"snapshot": HalfShaped()}) is None


def test_the_named_violations_are_capped_so_the_summary_stays_a_summary():
    """A broken build violates on every frame; the summary must not become the corpus."""
    tally = frame_invariants.RunTally()
    for i in range(40):
        tally.record(f"t.py::a{i}", ["nav-stack: nav.stack is empty"], None)
    lines = tally.summary(_CORPUS)
    assert len(lines) <= frame_invariants.SUMMARY_DETAIL_CAP + 2
    # …and it must say so, with the way to read the rest.
    assert any("more" in line and str(_CORPUS) in line for line in lines[1:])
