"""Windows page-lifetime regression: a Feed page the user has left is detached for good.

The windows Feed page renders off the session's ONE feed manager
(``Core.FeedManagerHost``), which outlives every Feed page, and registers its observer
on it. Two defects kept a left Feed page attached (``apps/windows.md`` § Client
lifetime, the third rule — a page releases its resources in ``OnNavigatedFrom``):

1. ``OnNavigatedFrom`` never took the page's observer off the manager, nor unhooked its
   render, so after an ordinary Feed -> Settings navigation every later manager tick (an
   engagement toggle on Personalization, a push, the off-page reconnect re-pull)
   re-rendered the left page. Its render ends by closing its own error bar and nulling
   the app-wide error mirror, so it blanked the error the page ON SCREEN had just
   painted — the live symptom: ``test_trained_topics.py::
   test_publish_name_too_long_error_survives_unrelated_rerender[windows]`` red in a
   whole-set run on 2026-11-02, green in the run before.
2. A root-frame navigation (sign-out, account switch, the e2e ``reset``) calls
   ``OnNavigatedFrom`` on ``MainPage`` only; the page inside its ``ContentFrame`` gets
   ``Unloaded`` and nothing else, so its cleanup never ran at all — its cue-capture
   timer kept it alive and its observer kept it rendering for a dead session. The shells
   now end their inner page (``Views.NestedFrame``).

Asserted on the cause, as ``test_windows_login_cycle_poller_leak.py`` does for the
shell's poller: the app reports ``diagnostics.live_feed_pages``, the Feed pages still
attached to the session's manager — at most 1 while the feed is on screen, 0 anywhere
else — so a regression fails as itself, not as an error that flickers away in some
other test.

Windows-only: the counter, the WinUI nested frames and ``OnNavigatedFrom`` are the
WinUI shell's; the other apps tear their feed observers down on their own paths.
"""

import pytest

pytestmark = [pytest.mark.tier_3, pytest.mark.windows]

_LOGIN_CYCLES = 3


def _live_feed_pages(app, expected: int, timeout: float = 20.0) -> int:
    """The attached-Feed-page count, polled until it reaches ``expected``.

    Condition-based: the Feed page attaches at the end of its async load and detaches on
    the navigation's own UI turn, so neither edge is synchronous with the command that
    caused it. On a timeout the last observed value comes back, so the assertion reports
    what the app converged on.
    """
    value = app.driver.get_state(
        "diagnostics.live_feed_pages",
        wait_for=lambda v: v == expected,
        timeout=timeout,
    )
    assert value is not None, "app did not report diagnostics.live_feed_pages"
    return int(value)


def test_leaving_the_feed_detaches_the_feed_page(logged_in_app):
    """Feed -> Settings leaves no Feed page attached to the session's feed manager."""
    app = logged_in_app

    app.driver.navigate_to("feed")
    observed = _live_feed_pages(app, 1)
    assert observed == 1, f"expected the Feed page on screen to be attached, got {observed}"

    app.driver.navigate_to("settings")
    observed = _live_feed_pages(app, 0)
    assert observed == 0, (
        f"{observed} Feed page(s) still attached after navigating to Settings — the left "
        "page's observer is still on the session's feed manager, so every manager tick "
        "re-renders it and its render nulls the app-wide error mirror under Settings"
    )

    app.driver.navigate_to("feed")
    observed = _live_feed_pages(app, 1)
    assert observed == 1, (
        f"expected exactly the one Feed page on screen attached after returning, got {observed}"
    )


def test_a_reset_on_the_feed_detaches_the_feed_page(
    logged_in_app, request, nest_instance, test_user
):
    """A reset from the Feed page — a root-frame navigation — detaches it too, every
    login cycle, so no Feed page outlives its session."""
    from conftest import _login_app_as

    app = logged_in_app

    for cycle in range(_LOGIN_CYCLES):
        app.driver.navigate_to("feed")
        observed = _live_feed_pages(app, 1)
        assert observed == 1, (
            f"cycle {cycle}: expected exactly one attached Feed page on the feed, got "
            f"{observed} — more than one means a previous session's Feed page survived"
        )

        app.driver.reset()
        observed = _live_feed_pages(app, 0)
        assert observed == 0, (
            f"cycle {cycle}: {observed} Feed page(s) still attached after reset() — the "
            "root frame left MainPage, and the Feed page inside its ContentFrame never "
            "got its OnNavigatedFrom, so its cleanup never ran"
        )

        _login_app_as(app, request, nest_instance, test_user)
