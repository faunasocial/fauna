"""Navigate to a Settings sub-page by state patch.

One patch, no re-send: on a phone the app itself lands a jump that brings
Settings on screen and names a sub-page in one transaction
(`Fauna-iOS/App/ContentView.swift`'s `MoreView.settingsPath` holds the path
until a fresh Settings root has mounted and re-pushes a page the stack writes
back before it settled), so a user's cross-tab jump lands the first time and
this patch does too. Shared by every action class whose page is
a Settings sub-page (the Personalization home, the Community-labelers catalog,
Folders).
"""
from __future__ import annotations

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from drivers.base import PlatformDriver


def settings_stack(driver: PlatformDriver) -> list:
    """The app's own reported nav stack — for a caller's failure message."""
    return (driver.get_state().get("nav") or {}).get("stack") or []


def navigate_settings_subpage(driver: PlatformDriver, sub_id: str) -> None:
    """Patch the nav stack to Settings → `sub_id`. A page the app drops fails
    on the caller's own assertion, whose message carries `settings_stack`."""
    driver.set_state({"nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": sub_id}]}})
