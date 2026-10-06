"""Windows resource-leak regression: ConversationsPage must not accumulate a
manager-registered SnapshotObserver on every re-navigation.

``ConversationsPage.Page_Loaded`` constructed a fresh ``ConversationsNotifyObserver``
and ``ConversationsViewModel`` on every page load, and the VM's constructor
unconditionally called ``manager.AddObserver(observer)``. ``Page_Unloaded`` only
unsubscribed the VM's own ``PropertyChanged`` C# event -- there is no
``manager.RemoveObserver`` in the shared Rust API (``libs/fauna-conversations/src/
manager.rs``), so the OLD observer (and the VM/``_spamWrite`` closure it retained)
stayed registered on the manager forever. ``ConversationsPage`` carries no
``NavigationCacheMode`` (WinUI's default ``Disabled`` applies), so a fresh page
instance -- and therefore a fresh ``Page_Loaded`` -- is built on every tab/nav-item
activation (``MainPage.xaml.cs``'s generic branch navigates unguarded even to the page
already on screen); repeatedly switching to and from the Conversations tab therefore
accumulated one more dead observer per visit, each firing a dead UI-thread dispatch on
every manager emit forever.

The manager exposes the live count at ``diagnostics.conversations_observer_count``
(``libs/fauna-conversations/src/manager.rs::observer_count``) so the regression is
assertable directly instead of only as a downstream memory/dispatch-storm
symptom.

Windows-only: this page's re-navigation lifecycle and the observer-registration
mechanism are both windows-specific. linux clears its window's observers wholesale at
sign-out (``manager.rs::clear_observers`` doc comment) rather than per-navigation.
"""

import pytest

pytestmark = [pytest.mark.tier_3, pytest.mark.windows]

# Enough re-navigations to be unambiguous (pre-fix this yields one extra observer per
# visit) while staying fast -- a tab switch, not a full UI journey.
_NAV_CYCLES = 4


def _observer_count(app, expected: int | None, timeout: float = 20.0) -> int:
    """Live conversations-observer count, polled until it reaches ``expected`` (or
    ``timeout``); ``expected=None`` polls only until the app reports a count.
    Condition-based rather than a bare read: state serialization is async on native
    apps, so a navigation can return before the count settles. On failure this
    returns the last observed value, so the assertion message reports what the app
    actually converged on.

    The count is the manager-wide observer total (``observer_count`` in
    ``libs/fauna-conversations/src/manager.rs``), not the page's own observers.
    """
    value = app.driver.get_state(
        "diagnostics.conversations_observer_count",
        wait_for=lambda v: v is not None and (expected is None or v == expected),
        timeout=timeout,
    )
    assert value is not None, (
        "app did not report diagnostics.conversations_observer_count"
    )
    return int(value)


def test_repeated_conversations_navigation_does_not_leak_observers(logged_in_app):
    """The first navigation to Conversations adds exactly one page observer over the
    baseline, and N further navigations away from and back leave that count flat."""
    app = logged_in_app

    # Baseline: the manager-wide count before the page exists. It is NOT zero -- the
    # app-lifetime observers (rooms refresh, DM toast, unread badge) are registered
    # once per manager at login and never grow with navigation.
    baseline = _observer_count(app, None)

    app.conversations.navigate()
    expected = baseline + 1
    observed = _observer_count(app, expected)
    assert observed == expected, (
        f"expected exactly one page observer over the baseline of {baseline} after "
        f"the first navigation, got {observed}"
    )

    for cycle in range(_NAV_CYCLES):
        app.feed.navigate()
        app.conversations.navigate()
        observed = _observer_count(app, expected)
        assert observed == expected, (
            f"cycle {cycle}: expected the observer count to stay at {expected} after "
            f"re-navigating to Conversations, got {observed} -- each stale observer "
            "retains its VM and fires one dead UI-thread dispatch per manager emit "
            "forever"
        )
