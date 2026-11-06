"""Reach the ``settings-tab`` entry by clicks, on every form factor.

``settings-tab`` is a primary tab or sidebar row on most apps, but iOS keeps it
in the More list (ui-actual-ios.yaml ``navigation.more_items``): the row exists
only once ``more-tab`` has opened that list. And where the admin shell replaces
the tab bar (iOS), neither is on screen from inside an admin page: the human
leaves admin first (``admin-nav-back``, which lands on the primary view). A test
bound by the gesture-only rule (convention 8) does the same. The branches key on
the elements on screen, never on the driver type (convention 3).
"""

from __future__ import annotations

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from drivers.base import PlatformDriver


def click_settings_tab(driver: PlatformDriver, timeout: float = 15.0) -> None:
    """Click ``settings-tab``, leaving admin and opening the More list first
    where either hides it."""
    if (
        not driver.is_visible("settings-tab")
        and not driver.is_visible("more-tab")
        and driver.is_visible("admin-nav-back")
    ):
        driver.click("admin-nav-back")
        try:
            driver.wait_for("more-tab", timeout=timeout)
        except TimeoutError:
            driver.wait_for("settings-tab", timeout=timeout)
    if not driver.is_visible("settings-tab") and driver.is_visible("more-tab"):
        driver.click("more-tab")
        driver.wait_for("settings-tab", timeout=timeout)
    driver.click("settings-tab")
