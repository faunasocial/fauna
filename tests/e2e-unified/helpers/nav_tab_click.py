"""Reach a primary destination by clicking its ``{page}-tab``, on every form factor.

A ``{page}-tab`` is a primary tab or sidebar row on most apps, but a phone keeps
the destinations its bar has no room for in the More list (``navigation.layouts``
in ui.yaml; ui-actual-ios.yaml ``navigation.more_items``): the row exists only
once ``more-tab`` has opened that list. And where the admin shell replaces the
tab bar (iOS), neither is on screen from inside an admin page: the human leaves
admin first (``admin-nav-back``, which lands on the primary view). A test bound
by the gesture-only rule (convention 8) — a main use case's journey above all
(main-use-cases.md § The journeys) — does the same. The branches key on the
elements on screen, never on the driver type (convention 3).

``driver.navigate_to`` / an action's ``navigate()`` reach a page by a state
write instead; that records no gesture, so a journey that must prove it walked
the tab opens the page with this first.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from drivers.base import PlatformDriver


def click_nav_tab(driver: PlatformDriver, tab: str, timeout: float = 15.0,
                  *, canary: str | None = None) -> None:
    """Click ``tab``, leaving admin and opening the More list first where either
    hides it; then wait for ``canary`` when one is given."""
    if (
        not driver.is_visible(tab)
        and not driver.is_visible("more-tab")
        and driver.is_visible("admin-nav-back")
    ):
        driver.click("admin-nav-back")
        try:
            driver.wait_for("more-tab", timeout=timeout)
        except TimeoutError:
            driver.wait_for(tab, timeout=timeout)
    if not driver.is_visible(tab) and driver.is_visible("more-tab"):
        driver.click("more-tab")
        driver.wait_for(tab, timeout=timeout)
    driver.click(tab)
    if canary is not None:
        driver.wait_for(canary, timeout=timeout)
