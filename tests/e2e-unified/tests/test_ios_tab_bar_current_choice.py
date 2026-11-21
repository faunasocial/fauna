"""tier_3: the iOS tab bar publishes the tab in force as the current choice.

`docs/goal/architecture/e2e-systematic-ui-walks.md` § Implementation status
today, the possibility-invariants bullet, layer (c) `actuation-changes-or-reports`:
re-choosing a one-of-N control's current choice changes nothing by design, and
passes only when the control publishes itself as the current choice (`state` on,
active, selected or current — `helpers/ui_walk_actuation.py::is_current_choice`).
On iOS the page nav is the tab bar; Settings is content of the More tab, so the
walk re-presses `more-tab` while it is in force. Each `{page}-tab` publishes
`state` on for the tab in force and off for the rest, and moves with the
selection.
"""

from __future__ import annotations

import pytest

from helpers import budgets
from helpers.ui_walk_actuation import is_current_choice
from helpers.waiting import wait_until

pytestmark = [pytest.mark.tier_3, pytest.mark.ios]

_TABS = ("conversations-tab", "feed-tab", "contacts-tab", "more-tab")


def _state(d, tab: str) -> str:
    return (d.get_attr(tab, "state") or "").lower()


def _wait_in_force(d, tab: str) -> None:
    wait_until(
        lambda: _state(d, tab) == "on"
        and all(_state(d, other) == "off" for other in _TABS if other != tab),
        budgets.UI_SETTLE_S,
        diagnose=lambda: (
            f"{tab} must publish state=on and every other tab state=off; read "
            + ", ".join(f"{t}={_state(d, t)!r}" for t in _TABS)
            + f"; error-message={d.get_text('error-message') if d.is_visible('error-message') else ''!r}"
        ),
    )


def test_tab_bar_publishes_tab_in_force(logged_in_app):
    d = logged_in_app.driver
    for tab in ("contacts-tab", "more-tab", "conversations-tab"):
        d.click(tab)
        _wait_in_force(d, tab)
        assert is_current_choice({"attrs": {"state": _state(d, tab)}}), (
            f"{tab} in force must read as the current choice to the walk"
        )


def test_more_tab_in_force_under_settings(logged_in_app):
    """The sweep's own case: Settings shows (More's content) and `more-tab` is
    the tab in force, so its re-press is a re-chosen current choice."""
    d = logged_in_app.driver
    d.click("more-tab")
    _wait_in_force(d, "more-tab")
    d.click("settings-tab")
    wait_until(
        lambda: d.is_visible("account-settings-link"),
        budgets.UI_SETTLE_S,
        diagnose=lambda: "the More list's Settings row did not open Settings (account-settings-link absent)",
    )
    d.click("more-tab")
    _wait_in_force(d, "more-tab")
