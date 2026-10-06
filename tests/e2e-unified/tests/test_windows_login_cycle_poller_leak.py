"""Windows resource-leak regression: a login cycle must not leak a sync-agent poller.

``MainPage`` is built fresh on every login — each sign-out / account switch / e2e
``reset()`` navigates the root frame to ``OnboardingPage``, and the next login constructs
a brand-new page. Its ~10s sync-agent status ``DispatcherTimer`` was started in
``Page_Loaded`` and **never stopped**, on the (self-refuting) premise that MainPage is
"the app-lifetime shell (constructed once per login)". A running ``DispatcherTimer`` is
rooted by the dispatcher, so every detached MainPage stayed alive and kept polling.

That is not a mere memory leak. Each tick calls ``NamedPipeClientStream.ConnectAsync``,
which on Windows wraps a **blocking** connect in ``Task.Run`` — so every leaked poller
parks a thread-pool thread for up to its timeout, every 10s. Past a handful of logins the
pool starves: WS-RPC continuations stop being serviced (``MainPage.CheckAdminStatusAsync``
never returns), ``TestAgent.PushStateAsync`` drifts from ~1s to ~29s, and queued UI-thread
post-actions run far too late. The app then fails whichever probe hits it first — a UIA
``GetMainWindow`` COM timeout, a dead FlaUI-bridge HTTP endpoint, or
``App did not acknowledge command ... within 10.0s``.

That was the "second family journey" flake: ``test_family_transfer_decline_
journey[windows]`` passed alone but failed 3/3 immediately after
``test_family_transfer_accept_journey``, because only the second journey accumulated
enough logins (8 MainPages -> 7 leaked pollers). Three sessions chased it as a *nav* bug
across three different proximate symptoms, because none of the symptoms names the cause.

This test asserts the cause directly instead of the symptom, so a regression fails as
itself in ~20s rather than as a timing flake in a 2.5-minute journey pair. The app
exposes the live poller count at ``diagnostics.live_sync_agent_pollers``; exactly one live
MainPage owns the poll at a time, so it must stay 1 no matter how many logins have run.

Windows-only: the leak, the ``DispatcherTimer`` mechanism and the counter are all in the
WinUI shell. linux/apple/web own their agent-status polls on different paths.
"""

import pytest

pytestmark = [pytest.mark.tier_3, pytest.mark.windows]

# Enough cycles to be unambiguous (pre-fix this yields one poller per login) while
# staying fast — each cycle is a reset + set_state login, not a UI journey.
_LOGIN_CYCLES = 4


def _live_pollers(app, expected: int, timeout: float = 20.0) -> int:
    """Live poller count, polled until it reaches ``expected`` (or ``timeout``).

    Condition-based rather than a bare read: the poll is started at the END of
    ``MainPage.Page_Loaded``, after two awaited status RPCs, so a login returns before
    the count settles. On failure this returns the last observed value, so the assertion
    message reports what the app actually converged on.
    """
    value = app.driver.get_state(
        "diagnostics.live_sync_agent_pollers",
        wait_for=lambda v: v == expected,
        timeout=timeout,
    )
    assert value is not None, "app did not report diagnostics.live_sync_agent_pollers"
    return int(value)


def test_repeated_logins_do_not_leak_sync_agent_pollers(
    logged_in_app, request, nest_instance, test_user
):
    """N reset+login cycles must leave exactly one live sync-agent poller."""
    from conftest import _login_app_as

    app = logged_in_app

    # One live MainPage after the fixture's own login.
    observed = _live_pollers(app, 1)
    assert observed == 1, (
        f"expected exactly one sync-agent poller after the initial login, got {observed}"
    )

    for cycle in range(_LOGIN_CYCLES):
        app.driver.reset()
        # No MainPage is in the frame between reset and login, so nothing should poll.
        observed = _live_pollers(app, 0)
        assert observed == 0, (
            f"cycle {cycle}: {observed} poller(s) survived reset() — the detached "
            "MainPage's DispatcherTimer was never stopped, so the page stays alive "
            "and keeps polling forever"
        )

        _login_app_as(app, request, nest_instance, test_user)
        observed = _live_pollers(app, 1)
        assert observed == 1, (
            f"cycle {cycle}: expected exactly one live sync-agent poller after "
            f"re-login, got {observed} — each leaked poller parks a thread-pool "
            "thread every 10s and eventually starves the whole process"
        )
