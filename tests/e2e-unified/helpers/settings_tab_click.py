"""Reach the ``settings-tab`` entry by clicks, on every form factor.

The ``settings-tab`` case of ``helpers/nav_tab_click.py::click_nav_tab``, which
owns the More-list and admin-shell branches (iOS keeps ``settings-tab`` in the
More list; the admin shell replaces the tab bar there).
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from helpers.nav_tab_click import click_nav_tab

if TYPE_CHECKING:
    from drivers.base import PlatformDriver


def click_settings_tab(driver: PlatformDriver, timeout: float = 15.0) -> None:
    """Click ``settings-tab``, leaving admin and opening the More list first
    where either hides it."""
    click_nav_tab(driver, "settings-tab", timeout)
