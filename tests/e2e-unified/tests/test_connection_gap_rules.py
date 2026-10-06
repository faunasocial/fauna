"""tier_3 e2e: the two rules of a connection gap — what it must say, and what it
must never say.

Goal doc: ``docs/goal/architecture/transport-connection.md`` § Connection-status
indicator (app UI). Two outcomes of the ``offline-aware-controls`` page:

* **A gap that keeps failing stops pretending to be a passing one.** After
  ``CONNECTION_UNREACHABLE_AFTER_CONSECUTIVE_FAILURES`` failed attempts in a row
  the indicator reads "Cannot connect", and it is **sticky** — further failed
  attempts leave it there — until a connection actually succeeds (§
  ``Unreachable``). The existing indicator journey
  (``test_nest_flip_resilience.py::test_connection_status_flips_while_nest_down``)
  deliberately stops at "leaves Connected" and never reaches this state.
* **A passing gap raises no error anywhere.** A transient gap is deliberately
  never an error banner, toast or ``rpc disconnected`` line — the in-gap request
  wait (``transport.md`` § Request lifecycle step 3) keeps requests from failing,
  and the indicator is the only place the gap shows.

**The two app-published observables that make both latency-independent
(convention 14).** Stickiness is a claim about every moment of a window, which no
read of the indicator can prove; ``connection_reports`` (reports vs. word
transitions, ``fauna_e2e_agent::CONNECTION_REPORTS_KEY``) turns it into "reports
moved while transitions did not". "No error anywhere" is likewise a claim over a
window; ``painted_errors`` (``fauna_e2e_agent::PAINTED_ERRORS_KEY``) counts every
error surface any painted frame showed, so "the count did not move" is the claim
itself. And the failure run is paced by ``reconnect_backoff``
(``fauna_e2e_agent::RECONNECT_BACKOFF``) — every one of the threshold's failures
is still a real refused dial, only no longer a minute of jittered backoff.

tier_3: a real ``fauna-nest`` binary, stopped and restarted in place on its own
port and data dir (``common.nest``) — never a process kill by name.
"""
from __future__ import annotations

import pytest

from common.nest import start_nest_in_place, stop_nest
from helpers.registry_audit import visible_page_tabs
from helpers.waiting import await_feed_reload_after, feed_reload_baseline, wait_until
from i18n.strings import S

pytestmark = pytest.mark.tier_3

CONNECTION_STATUS = "connection-status"
# The Settings shell's one uniform exit (`docs/goal/ui/settings.md` § Navigation
# model, step 3): a desktop shell swaps the page nav out while Settings shows,
# so the walk leaves Settings through it rather than through a tab that is not
# there.
SETTINGS_NAV_BACK = "settings-nav-back"
SETTINGS_TAB = "settings-tab"
FEED_TAB = "feed-tab"
CONNECTION_KEY = "connection"
CONNECTION_REPORTS_KEY = "connection_reports"
PAINTED_ERRORS_KEY = "painted_errors"
RECONNECT_BACKOFF = "reconnect_backoff"

# Deliberately generous (see `test_nest_flip_resilience.py`'s ceiling notes): a
# regression fails at any ceiling, and only load can blow a tight one.
CONNECT_WAIT_S = 90.0
DISCONNECT_WAIT_S = 60.0
# Reaching the threshold at the TEST pace (20 ms → 100 ms): eight refused dials
# cost well under a second, so this is pure load headroom.
UNREACHABLE_WAIT_S = 60.0
# The feed's reconnect re-query committing — `test_nest_flip_resilience.py`'s
# REHYDRATE_WAIT_S, for the same chain.
REHYDRATE_WAIT_S = 300.0


def _state(driver, key: str) -> dict:
    """An app-published state block, refusing loudly when the app has none
    (convention 11 — an unbuilt observable must never read as a pass)."""
    value = driver.get_state(key)
    assert isinstance(value, dict), (
        f"{type(driver).__name__} publishes no {key!r} — the observable this "
        "journey's assertion rests on. Implement it on the app (see "
        f"`fauna_e2e_agent::{key.upper()}_KEY`) rather than sampling the UI."
    )
    return value


def _word(driver) -> str | None:
    return (driver.get_state(CONNECTION_KEY) or {}).get("state")


def _wait_word(driver, predicate, budget_s: float, what: str) -> str:
    return wait_until(
        lambda: (word := _word(driver)) is not None and predicate(word) and word,
        budget_s,
        diagnose=lambda: (
            f"{what}: connection word still {_word(driver)!r}, indicator reads "
            f"{driver.get_text(CONNECTION_STATUS)!r}, reports "
            f"{driver.get_state(CONNECTION_REPORTS_KEY)!r}"
        ),
    )


def _nav_view(driver) -> str | None:
    stack = ((driver.get_state() or {}).get("nav") or {}).get("stack") or [{}]
    return stack[0].get("view")


def _leave_settings(driver, activate) -> None:
    """Leave the Settings shell the way a user does.

    A shell that offers ``settings-nav-back`` (the sidebar-swap shells) is left
    through it, its one uniform exit; where Settings offers none (tui's root
    page, a mobile settings nav) the page tabs are still reachable and the next
    tab press IS the way out. Decided from what the shell paints, never from
    which app this is — and only once the app reports it is ON Settings and a
    barrier has ordered us after that page's queued render, so the read cannot
    land on the frame before the shell swapped in.
    """
    wait_until(
        lambda: _nav_view(driver) == "settings",
        DISCONNECT_WAIT_S,
        diagnose=lambda: f"never reached Settings: nav view is {_nav_view(driver)!r}",
    )
    driver.barrier()
    if driver.is_visible(SETTINGS_NAV_BACK):
        activate(SETTINGS_NAV_BACK)
        driver.barrier()


def _walk_pages(driver, pages: list[str], activate) -> None:
    """Enter each page in turn, then return to the feed.

    ``pages`` is what the shell offers (``visible_page_tabs``), so the walk
    visits whatever this app's page nav holds — Settings among them on a
    desktop shell, the More hub on iOS. Only a walk that actually entered
    Settings has a Settings shell to leave, and it leaves it at once, before
    the next tab press needs the page nav back.
    """
    for tab in pages:
        activate(tab)
        driver.barrier()
        if tab == SETTINGS_TAB:
            _leave_settings(driver, activate)
    activate(FEED_TAB)
    driver.barrier()


@pytest.mark.feature("offline-aware-controls")
def test_a_failing_connection_reads_cannot_connect_until_one_succeeds(
    logged_in_app, nest_instance
):
    """A nest that stays down turns the indicator to "Cannot connect", further
    failed attempts leave it there, and only a real connection takes it back."""
    app = logged_in_app
    driver = app.driver
    _wait_word(driver, lambda w: w == "connected", CONNECT_WAIT_S, "precondition: connected")
    _state(driver, CONNECTION_REPORTS_KEY)

    driver.call_command(RECONNECT_BACKOFF, {"initial_ms": 20, "max_ms": 100})
    nest_down = False
    try:
        stop_nest(nest_instance, graceful=True)
        nest_down = True

        # ── 1. It keeps failing → the indicator stops calling it a passing gap.
        _wait_word(driver, lambda w: w == "unreachable", UNREACHABLE_WAIT_S, "the failure run")
        label = driver.get_text(CONNECTION_STATUS)
        assert label == S.common.cannot_connect, (
            "a connection that keeps failing must read "
            f"{S.common.cannot_connect!r}; the indicator reads {label!r}. "
            f"error surface: {app.error_text()!r}"
        )

        # ── 2. …and further failed attempts leave it there. Every retry publishes
        #       at least one report, so reports moving on while transitions stay
        #       put is exactly "more attempts, and the word never changed".
        tripped = _state(driver, CONNECTION_REPORTS_KEY)
        wait_until(
            lambda: _state(driver, CONNECTION_REPORTS_KEY)["reports"] >= tripped["reports"] + 4,
            UNREACHABLE_WAIT_S,
            diagnose=lambda: (
                "no further connection attempts were reported after the indicator "
                f"tripped: {tripped!r} → {driver.get_state(CONNECTION_REPORTS_KEY)!r}"
            ),
        )
        later = _state(driver, CONNECTION_REPORTS_KEY)
        assert later["transitions"] == tripped["transitions"] and later["word"] == "unreachable", (
            "'Cannot connect' must be sticky across further failed attempts — the "
            "indicator changed its word while the nest was still down: "
            f"{tripped!r} → {later!r}"
        )
        assert driver.get_text(CONNECTION_STATUS) == S.common.cannot_connect

        # ── 3. …until a connection actually succeeds.
        start_nest_in_place(nest_instance)
        nest_down = False
        _wait_word(driver, lambda w: w == "connected", CONNECT_WAIT_S, "the return")
        label = driver.get_text(CONNECTION_STATUS)
        assert label == S.common.connected, (
            f"a proven connection must clear 'Cannot connect'; the indicator reads {label!r}"
        )
    finally:
        if nest_down:
            start_nest_in_place(nest_instance)
        # The shared app's client must not keep a test pace for later tests.
        driver.call_command(RECONNECT_BACKOFF, {})


@pytest.mark.feature("offline-aware-controls")
def test_a_passing_gap_raises_no_error_anywhere(logged_in_app, nest_instance):
    """A short outage the user keeps working through shows on the indicator and
    nowhere else: no page raises an error during it or after it.

    The user does what a user does in a gap — moves between pages whose loads
    need the nest — so the in-gap request wait is exercised, not just idled
    past. The gap is kept short on purpose: it must stay a PASSING gap, well
    inside a request's own deadline (``transport.md`` § Request lifecycle step
    5), or an error would be the correct answer.
    """
    app = logged_in_app
    driver = app.driver
    _wait_word(driver, lambda w: w == "connected", CONNECT_WAIT_S, "precondition: connected")
    driver.navigate_to("feed")
    before = _state(driver, PAINTED_ERRORS_KEY)
    reloads_before = feed_reload_baseline(driver)
    # Every page the shell offers, the feed aside (the walk starts and ends
    # there) — read off the app's own registry, never a hand-picked list.
    pages = [tab for tab in visible_page_tabs(driver) if tab != FEED_TAB]
    assert len(pages) >= 2, f"the walk found too few pages to mean anything: {pages!r}"

    stop_nest(nest_instance, graceful=True)
    try:
        # The gap is real and the indicator shows it — the one place it may.
        _wait_word(driver, lambda w: w != "connected", DISCONNECT_WAIT_S, "the gap")
        label = driver.get_text(CONNECTION_STATUS)
        assert label != S.common.connected, (
            f"the indicator must show the gap; it reads {label!r}"
        )
        # The user keeps working: each page entry issues loads that wait in the
        # gap, and the return to the feed issues a feed re-query. Keyboard
        # activation, not a click: a user's keypress returns at once while the
        # page's load waits, whereas a driver click on tui awaits the page's
        # load — which is parked in the in-gap wait until the nest returns, so a
        # clicked walk would stall the agent and stretch the gap past a
        # request's own deadline, turning it into a gap that SHOULD error.
        _walk_pages(driver, pages, lambda target: driver.press_key(target, "Enter"))
    finally:
        start_nest_in_place(nest_instance)

    _wait_word(driver, lambda w: w == "connected", CONNECT_WAIT_S, "the return")
    # The causal anchor: a feed re-query that began after the baseline — issued
    # inside the gap — has committed its verdict, so the in-gap work has resolved
    # one way or the other rather than still waiting.
    await_feed_reload_after(driver, reloads_before, budget_s=REHYDRATE_WAIT_S, what="the passing gap")
    # Re-enter every page the gap touched: an in-gap load that failed after the
    # user left its page holds its error unpainted until the page is shown again.
    _walk_pages(driver, pages, driver.click)

    after = _state(driver, PAINTED_ERRORS_KEY)
    assert after["count"] == before["count"], (
        "a passing connection gap must raise no error anywhere — the indicator is "
        f"the only place it may show. Error surfaces painted: {before['count']} → "
        f"{after['count']}, newest frame shows {after['showing']!r}"
    )
