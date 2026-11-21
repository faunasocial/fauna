"""tier_3 e2e: the admin and Settings sub-page rails are reachable by CLICKS —
``admin-tab`` → ``admin-nav-row[<page id>]`` and ``settings-tab`` →
``settings-nav-row[<page id>]`` (ui.yaml ``navigation.sub_page_nav_rows``,
user-approved under rule A 2026-10-04) — each landing on its page's canary
element.

Why it exists: a test bound by the gesture-only rule (convention 8; the live
tier_4 tests' "no page is reached by injected navigation") cannot reach a
sub-page through the nav patch, and the driver clicks by element id only. These
are the three pages the live admin helpers need (``helpers/live_admin.py``);
the test drives each click path end to end from the primary view, the way a
human does, through the real ``am-i-admin`` gate that reveals ``admin-tab``.

tier_3: a real nest and a real app, nothing stubbed. tui is the lead app; the
other apps gain the ids in trickle-down rows and join this file's markers then
(android, web, linux, windows, macos and ios have).
"""

import pytest

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.android,
    pytest.mark.web,
    pytest.mark.linux,
    pytest.mark.windows,
    pytest.mark.macos,
    pytest.mark.ios,
]


@pytest.mark.parametrize(
    ("page_id", "canary"),
    [
        ("admin-dns", "admin-dns-add-domain-button"),
        ("admin-bridges-pending", "admin-bridges-pending-section"),
    ],
)
def test_admin_subpage_reached_by_rail_click(admin_main_app, page_id, canary):
    """From the primary view, ``admin-tab`` then ``admin-nav-row[<page_id>]``
    opens that admin sub-page."""
    admin_main_app.admin.open_page_by_click(page_id, canary)
    assert admin_main_app.driver.is_visible(canary), (
        f"clicking admin-nav-row[{page_id}] did not land on {page_id}: "
        f"{admin_main_app.driver.diagnose(canary)}"
    )


def test_admin_rail_row_reached_from_another_admin_page(admin_main_app):
    """Inside the shell the rail is already on screen: one sub-page to another
    is a single row click (``admin-dns`` → ``admin-bridges-pending``)."""
    admin_main_app.admin.open_dns_by_click()
    admin_main_app.admin.open_bridges_pending_by_click()
    assert admin_main_app.driver.is_visible("admin-bridges-pending-section"), (
        admin_main_app.driver.diagnose("admin-bridges-pending-section")
    )


def test_mail_settings_reached_by_rail_click(admin_main_app):
    """From the primary view, ``settings-tab`` then
    ``settings-nav-row[mail-settings]`` opens Mail & Calendar."""
    admin_main_app.mail_settings.open_by_click()
    assert admin_main_app.driver.is_visible("mail-settings-enabled-toggle"), (
        admin_main_app.driver.diagnose("mail-settings-enabled-toggle")
    )


def test_backups_row_reached_by_settings_click(logged_in_app):
    """On mobile, Settings carries a ``settings-nav-row[backups]`` row into the
    Backups page — not a Settings sub-page (settings.md's rail has no Backups
    entry) but the More hub's / drawer's own ``backups`` view, the page
    ``{"view":"backups"}`` lands on. Pressing it opens that page: its
    unconditionally painted ``last-backed-up`` line shows.

    Pins the press from a clean Settings frame, which once changed nothing on
    ios (the systematic walk's actuation sweep, convention 17)."""
    from helpers.app_surface import declared_absence
    from helpers.settings_tab_click import click_settings_tab

    if not logged_in_app.driver.is_mobile():
        declared_absence(
            logged_in_app.driver,
            capability="a Settings row into Backups (Backups is a primary-nav "
                       "peer on desktop and tui)",
            doc="ui/settings.md § Navigation model (the rail list has no Backups)",
        )
    row = "settings-nav-row[backups]"
    click_settings_tab(logged_in_app.driver, timeout=60.0)
    logged_in_app.driver.wait_for(row, timeout=60.0)
    logged_in_app.driver.click(row)
    logged_in_app.driver.wait_for("last-backed-up", timeout=30.0)
    assert logged_in_app.driver.is_visible("last-backed-up"), (
        f"clicking {row} did not open the Backups page: "
        f"{logged_in_app.driver.diagnose('last-backed-up')}"
    )
