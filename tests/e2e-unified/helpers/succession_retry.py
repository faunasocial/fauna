"""The post-succession sweep RETRY's render gate, asserted wherever a
succession runs.

Lives here rather than in either journey file because BOTH succession suites
must carry it and neither owns it: ``test_identity_succession_ceremony.py``
drives the ceremony for its own sake, ``test_identity_succession_aftermath.py``
drives it as setup for the aftermath — and between them they are the only
places in the suite where a real sweep report exists to gate on.

⚠ It is asserted from `_run_succession`-shaped call sites deliberately, not
from a journey of its own. Staging a sixth ceremony would cost minutes to
produce an arm the existing five already produce for free, and which arm a run
lands on is exactly what makes the gate worth asserting: the invariant holds on
all of them (``e2e-conventions.md`` point 17, layer (b)).
"""
from __future__ import annotations

from helpers.waiting import wait_until

# The retry affordance's render budget: the section is already on screen and the
# view rode across the switch, so this covers a scroll plus the section
# finishing its render — the `STOLEN_GATE_REVEAL_S` class
# (`helpers/succession_ceremony.py`), not a journey's.
_RETRY_AFFORDANCE_S = 20.0


def sweep_owes_work(sweep: dict) -> bool:
    """Whether the sweep left work a retry could finish — the spec, restated.

    Deliberately computed from the report's raw facts rather than read off the
    app: this is the *witness* for the render gate below, and reading the app's
    own answer would make the assertion vacuous. The three owing arms are the
    three the retry exists for (``settings.md`` § Recovery kit → *Finishing an
    unfinished group sweep*): the sweep never ran, it failed, or it swept some
    groups and not others. A sweep over an account with no groups at all owes
    nothing — "finish moving your groups" would offer to finish something that
    never started.
    """
    status = sweep.get("status")
    if status in ("no_engine", "failed"):
        return True
    if status == "ran":
        return sweep.get("groups_old_leaf_removed", 0) < sweep.get("groups", 0)
    return False


def assert_the_retry_affordance_matches_the_sweep(app, sweep: dict) -> None:
    """The button is on screen exactly when the sweep owes work — and when it
    is, pressing it answers in words.

    Two halves of one ruling (``settings.md`` § Recovery kit → *Finishing an
    unfinished group sweep*):

    * **The render gate is unfinished work, never "this device can retry".**
      The retry needs the retired identity's own MLS store, and gating the
      render on holding it would leave the sweep's own degraded copy naming a
      control that is not on screen. So the button's presence is a pure function
      of the report — which is what makes it assertable here at all, on whatever
      arm the run produced, rather than only on a staged one.
    * **It must answer in words on every press**, because a device that cannot
      retry is on screen: an answer *is* the gesture's whole product on three of
      its arms, and a press that reports nothing reads downstream as a dropped
      command (``e2e-conventions.md`` point 11). What the sentence SAYS is the
      shared projection's and is pinned where that projection lives; what this
      asserts is that one arrived.

      ⚠ **"In words" is three of the four arms, not all four.** The press that
      actually SWEEPS answers ``SweepRetryAnswer::Swept``, whose ``message()``
      is deliberately ``None`` — its outcome renders through the sweep's own
      lines instead, exactly as the ceremony's own sweep does. So the witness
      here is **one of two, never neither**: a sentence on ``error-message``, or
      a *refreshed* ``data.succession_sweep``. Asserting only the sentence would
      have made the one press that finishes the job read as a dropped command
      (found 2026-08-27 while building the first journey that ever presses this
      button — until then the press half was unreachable dead code).

    Riding the ceremony journeys rather than staging a sixth one is deliberate
    (``e2e-conventions.md`` point 17, layer (b)): every succession this suite
    runs already produces exactly one of the arms, and the invariant is true on
    all of them.
    """
    owes = sweep_owes_work(sweep)
    app.settings.navigate()
    app.settings.open_recovery_kit()

    def _present() -> bool:
        """Tree-membership, never `is_visible_scrolled`.

        ⚠ The absence half must NOT route through a scroll. On web a
        `scroll_to` for a genuinely absent element is a Playwright
        `scroll_into_view_if_needed` that waits for the locator to *appear*,
        runs out the bridge timeout and answers **500** rather than False
        (`drivers/web.py::is_nav_tab_revealed` documents exactly this, and
        overrides the base method for the same reason). Measured here the hard
        way 2026-08-27: the first cut of this helper used the scrolled read and
        died with `HTTPError: 500` on the one app whose sweep owed nothing.
        Membership is also the honest question — "did this app render the
        button at all", not "is it in the viewport".
        """
        return app.driver.count("recovery-kit-sweep-retry-button") >= 1

    if not owes:
        assert not _present(), (
            "the sweep owes no work, so there is nothing for "
            "recovery-kit-sweep-retry-button to finish — a standing button here "
            f"offers to re-run a pass that completed; sweep={sweep!r}"
        )
        print(
            "[sweep-retry] gate asserted ABSENT (the sweep owes nothing): "
            f"status={sweep.get('status')!r} groups={sweep.get('groups')!r}"
        )
        return

    from helpers.app_surface import skip_unbuilt

    try:
        wait_until(_present, _RETRY_AFFORDANCE_S, diagnose=lambda: "")
    except Exception:
        skip_unbuilt(
            app.driver,
            surface="recovery-kit-sweep-retry-button",
            detail=(
                "settings.md § Recovery kit — the group-sweep retry; tui, web, "
                "linux, windows, macos and ios render it, and android owes "
                "its own leg"
            ),
            tracked="docs/goal/ui/settings.md § Recovery kit",
        )

    # Present, so the scrolled read is safe now — and needed, since a driver
    # may refuse to click what is below the fold.
    app.driver.is_visible_scrolled("recovery-kit-sweep-retry-button")
    before = app.driver.get_state("data.succession_sweep")
    app.driver.click("recovery-kit-sweep-retry-button")

    def _answered() -> bool:
        """Either witness — see the docstring's second bullet."""
        if app.error_text().strip():
            return True
        return app.driver.get_state("data.succession_sweep") != before

    wait_until(
        _answered,
        _RETRY_AFFORDANCE_S,
        diagnose=lambda: (
            "the retry answered NOTHING — no sentence on error-message and a "
            "sweep report byte-identical to the one the button was pressed "
            "under, which is indistinguishable from a dropped command "
            f"(convention 11); sweep={sweep!r}"
        ),
    )
    print(
        "[sweep-retry] gate asserted PRESENT and the press answered: "
        f"status={sweep.get('status')!r} answer={app.error_text()!r} "
        f"report_now={app.driver.get_state('data.succession_sweep')!r}"
    )
