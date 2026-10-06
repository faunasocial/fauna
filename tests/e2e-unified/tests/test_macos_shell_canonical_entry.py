"""macOS owed the canonical-entry rule on both shells — Settings + Admin
resumed the last sub-page instead of resetting to Status/Dashboard on
re-entry (`docs/goal/ui/README.md` § Navigation model → *Entering a shell
lands on its canonical entry*). iOS already conformed and was the shape
copied (`Fauna-iOS/App/ContentView.swift`'s `selectedSettingsPage = nil` on
its More-hub entry edge); macOS's `AppState.swift` `selectedSettingsPage`/
`selectedAdminPage` were plain properties that persisted across
`selectedSidebar` changes, reset only on identity switch (and even then only
`selectedSettingsPage`, never `selectedAdminPage`).

**Why this drives the real sidebar tabs, not a `{"view":...}` nav patch.**
The goal doc's qualification 4 warns that a state-protocol check reports
macOS conformant either way: `FaunaMacApp.swift::applyNavPatch` already
resolves a bare admin/settings nav id to Dashboard/Status explicitly (its
own `AdminPage(navId:)` / `SettingsPage(navId:)` calls), which is a
different, pre-existing code path from the real entry affordance this rule
is actually about — a `d.set_state({"nav": ...})` setup step exercises only
that path and would read macOS as conformant before AND after the fix.
`d.click("admin-tab")`/`d.click("settings-tab")` instead fires the SAME
`selection = item` write a real sidebar click makes (`SidebarView.swift`'s
`.automationActivate` documents itself as firing "the *real* handler...
not a shortcut" — `AutomationRegistry.swift`), which lands on
`MacAppState.selectedSidebar`'s setter exactly as the List row's own
selection binding does — so this is the real-affordance path, not the trap.

tier_3 (a real authenticated macOS app against a real nest, admin identity
for the Admin-shell half) — the bug is state left over from a prior visit,
which no mock replaces.
"""
import pytest

pytestmark = [pytest.mark.tier2, pytest.mark.tier_3, pytest.mark.macos]


@pytest.mark.feature("admin-dashboard")
def test_admin_shell_reentry_after_leaving_a_deep_sub_page_lands_on_dashboard(admin_app):
    """Leave Admin on Users (a deep sub-page) via a nav patch (setup only),
    exit the shell via the REAL `admin-nav-back` affordance (`AdminNavRail`
    replaces the sidebar's row list while inside Admin, so `conversations-tab`
    isn't reachable — `admin-nav-back` is the only way out), then come back
    via the REAL `admin-tab` sidebar row — must land on Dashboard, not still
    show Users."""
    d = admin_app.driver
    d.set_state(
        {"nav": {"stack": [{"view": "admin"}, {"view": "admin", "id": "users"}]}}
    )
    d.wait_for("admin-users-heading", timeout=10)

    d.click("admin-nav-back")
    d.wait_for("new-conversation-button", timeout=10)

    d.click("admin-tab")
    d.wait_for("admin-dashboard-heading", timeout=10)
    assert d.is_visible("admin-dashboard-heading"), (
        "returning to Admin via the real admin-tab sidebar row should land on "
        f"Dashboard: {d.diagnose('admin-dashboard-heading')}"
    )
    assert d.is_absent("admin-users-heading"), (
        "returning to Admin via the real admin-tab sidebar row should NOT still "
        f"show Users (the stale-sub-page regression): {d.diagnose('admin-users-heading')}"
    )


def test_settings_shell_reentry_after_leaving_a_deep_sub_page_lands_on_status(logged_in_app):
    """Leave Settings on Folders (a deep sub-page) via a nav patch (setup
    only), exit the shell via the REAL `settings-nav-back` affordance
    (`SettingsNavRail` replaces the sidebar's row list while inside Settings,
    so `conversations-tab` isn't reachable — `settings-nav-back` is the only
    way out), then come back via the REAL `settings-tab` sidebar row — must
    land on Status, not still show Folders."""
    d = logged_in_app.driver
    d.set_state(
        {"nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "folders"}]}}
    )
    d.wait_for("folder-add-button", timeout=10)

    d.click("settings-nav-back")
    d.wait_for("new-conversation-button", timeout=10)

    d.click("settings-tab")
    d.wait_for("status-actor-id-copy-btn", timeout=10)
    assert d.is_visible("status-actor-id-copy-btn"), (
        "returning to Settings via the real settings-tab sidebar row should "
        f"land on Status: {d.diagnose('status-actor-id-copy-btn')}"
    )
    assert d.is_absent("folder-add-button"), (
        "returning to Settings via the real settings-tab sidebar row should NOT "
        f"still show Folders (the stale-sub-page regression): {d.diagnose('folder-add-button')}"
    )
