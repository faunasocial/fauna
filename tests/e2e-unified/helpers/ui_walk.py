"""Convention 17 layer (c): the e2e-level walk engine.

`docs/goal/architecture/e2e-conventions.md` § convention 17 defines three layers.
Layer **(a)** is tier_1 in-process walks (`apps/fauna-tui/src/walk.rs`), where the
state-space breadth belongs — microseconds a step, exhaustive sweeps plus proptest
random walks. Layer **(b)** is the post-frame invariant checker riding every
existing e2e test, observe-only. This is layer **(c)**: a *thin wiring proof* over
the real binary, driving the generic walk commands
(`fauna_e2e_agent::{FOCUS_MOVE, SWITCH_PANE}`) and asserting the same invariant
catalogue layer (b) observes.

**What layer (c) is for, stated narrowly, because the temptation is to grow it.**
It is NOT where breadth lives — the design record weighed e2e-level fuzzing as the
primary layer and rejected it: seconds a step against a real binary versus
microseconds in-process, plus the load-sensitivity convention 14 exists to fight.
What only this layer can prove is that the walk vocabulary *reaches the real app
at all*: that `focus_move` moves a real focus ring through a real key handler and
`switch_pane` crosses a real region boundary, on the shipped binary, with the
real registry and the real render underneath. A tier_1 walk that passes against
an app whose agent silently drops both commands proves nothing about the app —
and that was not hypothetical when this was written, since linux's agent dropped
every unimplemented command in silence until 2026-08-16.

**Scheduled sweeps only, never inner-loop** (convention 17's own words). The gate
is `--walk-sweep` in `conftest.py`, which prunes this suite's directory outright —
the same hard gate `tests/real_session/` and `tests/artifact/` use, and for the
same reason: an in-test skip still imports the module and lets a tier filter or a
`just` sweep pull it in.

**Latency-independence** (convention 14) is structural here rather than
negotiated: every step is a command whose ack *is* the barrier (the agent sets
`last_command_id` only after applying it on its UI thread), and every assertion
reads state that is already true at that ack. There is no wait to tune and no
sleep to add — and there must never be one.

⚠ **Known limitation, stated rather than left for a reader to notice: page
navigation here is a `nav` state patch, not a UI gesture.** Convention 8 exempts
fixture setup, and everything under test — every step the invariants judge — is a
real key door; but the honest reading is that this walk explores each page's focus
ring, not the *transitions between* pages, and a transition bug of the kind
convention 17 was ratified over (the stale sub-page on sidebar re-entry) lives
precisely in a transition. Closing that needs a third vocabulary entry — "activate
the currently focused element" — so the walk can reach pages the way a keyboard
user does. That is a deliberate follow-on, not an oversight: it is a new
cross-app contract entry, and minting one is a fleet-wide act that belongs in its
own slice rather than smuggled into this one. Until it exists, read a green sweep
as *"every page's ring is well-formed"*, never as *"navigation is well-formed"*.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path

import yaml

from helpers import frame_invariants

# The canonical top-level pages, read from the SPEC rather than hand-listed.
#
# Convention 17's first adoption discipline is that a walk must not under-cover,
# and its layer-(a) reference makes the point in the same way: reach the frontier
# "through the *real* row gestures so no hand-listed enum can silently
# under-cover". A hand-written page list here would drift the day someone adds a
# page — and drift downward, silently, which is the one direction a coverage
# mechanism must never fail in.
_UI_YAML = Path(__file__).resolve().parent.parent / "ui.yaml"


def canonical_pages() -> list[str]:
    """`navigation.pages` from ui.yaml — the spec's own top-level page list."""
    with open(_UI_YAML, encoding="utf-8") as f:
        spec = yaml.safe_load(f)
    pages = (spec.get("navigation") or {}).get("pages") or []
    if not pages:
        raise AssertionError(
            f"{_UI_YAML} has no `navigation.pages` — the walk's frontier is read "
            "from the spec, so an empty list would silently walk nothing and "
            "report a clean sweep. Fix the loader or the spec; do not hand-list "
            "the pages here."
        )
    return list(pages)


# How far to tab within one page's focus ring.
#
# Sized to overshoot the widest page rather than to match it: overshooting costs
# a few no-op steps at the ring's edge (or wraps, which is itself worth walking),
# while undershooting means the tail of a long page is never visited and the
# sweep still reports green. Well inside `fauna_e2e_agent::FOCUS_MOVE_MAX_TIMES`.
FOCUS_RING_STEPS = 24


@dataclass
class WalkReport:
    """What a sweep observed — the non-vacuity evidence, not just a verdict.

    A walk that took no steps, or read no frames, must never be reportable as a
    clean sweep. That failure mode is not hypothetical: layer (b) shipped a probe
    that produced **zero** observations across nine green tests because it hung
    off a fixture seven modules shadow, and it read as working. So this carries
    the counts, and the caller asserts on them.
    """

    steps: int = 0
    frames_read: int = 0
    pages_visited: list[str] = field(default_factory=list)
    violations: list[str] = field(default_factory=list)
    #: Invariants no app-published field could answer, tallied for triage. An
    #: app publishing no such field is `n/a`, never a violation — cross-app
    #: safety, ruled 2026-08-15 for `focus-fanout` in particular.
    not_applicable: dict[str, int] = field(default_factory=dict)

    #: Steps whose frame could not be READ at all (dead bridge, app gone). Kept
    #: apart from `frames_read` rather than folded into it: layer (b) learned
    #: that a run where every frame went unobserved and a run where every frame
    #: was clean are indistinguishable unless unobserved gets its own word.
    unobserved: list[str] = field(default_factory=list)
    #: Walk commands this app refused, deduped — see `WalkDriver._step`.
    refused: set = field(default_factory=set)

    def note_frame(self, where: str, state) -> None:
        """Judge one frame, or record that there was none to judge."""
        if state is None:
            self.unobserved.append(where)
            return
        self.frames_read += 1
        violations, na = frame_invariants.evaluate(state)
        self.violations.extend(f"{where}: {v}" for v in violations)
        for name in na:
            self.not_applicable[name] = self.not_applicable.get(name, 0) + 1


class WalkDriver:
    """Drives one app through the generic walk vocabulary, checking as it goes.

    Every mutation here is a walk command through the app's real key door, which
    is convention 8 satisfied in its strictest reading: the walk *is* the user,
    and there is no API shortcut anywhere in the loop.
    """

    def __init__(self, app, report: WalkReport):
        self.app = app
        self.driver = app.driver
        self.report = report

    # --- the two doors -------------------------------------------------------

    def _step(self, where: str, action: str, send) -> None:
        """Send one walk command, then read and judge the frame it produced.

        The refusal check is the load-bearing half and is deliberately narrow:
        it asserts the app's error surface does not name *the command just
        sent*. That is precise (every refusal names its command — pinned
        cross-app by `test_agent_refuses_unknown_command.py`), and it cannot cry
        wolf on an unrelated product error the way a bare `has_error()` would.
        A checker that cries wolf gets deleted, and this one has to survive
        every page in the app.
        """
        send()
        self.report.steps += 1

        # Reported ONCE per action, not once per step. The agent-failure slot
        # persists until the next reset (it outranks every on-screen error by
        # design), so a single unimplemented command would otherwise flag every
        # remaining step of the sweep — hundreds of lines for one root cause,
        # which buries the first and only informative one.
        text = self.app.error_text()
        if text and action in text and action not in self.report.refused:
            self.report.refused.add(action)
            self.report.violations.append(
                f"{where}: the app REFUSED `{action}` — {text!r}. The walk "
                "vocabulary is a convention-11 cross-app contract entry: this app "
                "owes either an implementation or a declared, goal-doc-cited "
                "absence, and until it has one every walk over it is measuring "
                "the refusal rather than the app. (Reported once; later steps "
                "read the same persistent slot.)"
            )
        self.report.note_frame(where, self._state())

    def _state(self):
        """The frame, or `None` if it could not be read.

        An unreadable frame is the fixture's business, not an invariant
        violation — but it is emphatically not an observation either, so
        `note_frame` files it under `unobserved` and it never inflates the
        non-vacuity counts the caller asserts on.
        """
        try:
            return self.driver.get_state()
        except Exception:
            return None

    def focus_move(self, where: str, direction: str = "next") -> None:
        self._step(where, "focus_move", lambda: self.driver.focus_move(direction, 1))

    def switch_pane(self, where: str, pane: str) -> None:
        self._step(where, "switch_pane", lambda: self.driver.switch_pane(pane))

    # --- the sweep -----------------------------------------------------------

    def sweep_page(self, page: str) -> None:
        """Walk one page: enter its content region, tab the ring, come back out.

        Crossing into the content region FIRST is not a flourish — on tui a page
        reached through the state protocol keeps whatever zone the session was
        already in (`App::apply` is zone-agnostic by design), so a walk that only
        tabbed would explore the sidebar ring on every page and report having
        explored the app. That asymmetry is exactly why `switch_pane` is part of
        the vocabulary at all.
        """
        self.report.pages_visited.append(page)
        self.switch_pane(f"{page}/enter-page", "page")
        for i in range(FOCUS_RING_STEPS):
            self.focus_move(f"{page}/ring[{i}]", "next")
        # Back out through the same door, and walk one step backwards — the
        # `prev` arm is half the contract and would otherwise never be exercised
        # by this sweep at all.
        self.switch_pane(f"{page}/enter-sidebar", "sidebar")
        self.focus_move(f"{page}/sidebar-back", "prev")
