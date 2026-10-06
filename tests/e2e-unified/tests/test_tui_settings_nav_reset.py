"""Returning to Settings via the sidebar lands on the rail Root, not a stale
sub-page (`apps/fauna-tui/src/app.rs::set_page`).

A live user left Settings for a different sub-page (Folders), navigated away
via the sidebar, and came back — landing right back on Folders instead of the
rail Root, "stuck there forever" (Folders has no `settings-nav-back` button,
and Esc is undiscoverable). Root cause: `App::apply` used to compute
`let edge = self.page != page;` to decide whether to reset `settings.sub` — but
`App::click_sidebar` and the sidebar-zone arms of `App::focus_next`/
`focus_prev` ("selection *is* navigation": moving the ring live-previews the
destination) mutated `self.page` DIRECTLY, before `apply()` ever ran. So by the
time `apply()`'s edge check ran, `self.page` already equalled the target and
`edge` always read `false` — the reset never fired via the sidebar. Fixed by routing every `self.page` write through one door,
`App::set_page`, which computes the edge FIRST.

**Why two tests.** The e2e agent's own `nav` state-patch path
(`automation.rs::run_nav_enter` -> `App::apply`) never pre-mutates `self.page`,
so it computed the edge correctly even BEFORE the fix — a nav-patch-only test
would be green before AND after the fix and prove nothing about this
regression. Clicking a `{page}-tab` element through the automation agent is
*also* agent-path: it resolves the element's registered `Gesture::Nav` and
calls `App::apply` directly (`automation.rs::run_gesture` ->
`app::gesture_work`), never `App::click_sidebar`. The only way to drive the
REAL, formerly-buggy code (`click_sidebar` / the sidebar-zone `focus_next`/
`focus_prev` arms) through the current e2e surface is the `focus_move` test
command this track added (`automation.rs::dispatch_command`): it calls
`App::focus_next`/`focus_prev` directly — the SAME functions a real Left/Right
+ Up/Down/Tab keystroke sequence calls (`app.rs::handle_key`) — so moving the
sidebar ring onto Settings this way is the real human path, not a shortcut
around it. (`/element/key` has no consumers and is a documented no-op —
`fauna-e2e-agent::handle` — which is why this track added `focus_move` rather
than assuming raw key dispatch already worked.)

tier_3 (a real authenticated `fauna-tui` process against a real nest) — the bug
lives in state-mutation ordering no mock can stand in for.
"""
import pytest

pytestmark = [pytest.mark.tier2, pytest.mark.tier_3, pytest.mark.tui]

_ROOT_ELEMENT = "account-settings-link"
_FOLDERS_ELEMENT = "folder-add-button"
_SETTINGS_ROOT_NAV = {"nav": {"stack": [{"view": "settings"}]}}
_FOLDERS_NAV = {
    "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "folders"}]}
}
_FEED_NAV = {"nav": {"stack": [{"view": "feed"}]}}

# Comfortably more than the sidebar's ~13-15 rows (Page::ALL plus the two
# gated admin/family rows), so a bounded forward walk always reaches Settings
# regardless of how many gated rows are present.
_MAX_RING_STEPS = 20


def _current_nav_view(app) -> str | None:
    nav = app.driver.get_state("nav")
    if not nav or not nav.get("stack"):
        return None
    return nav["stack"][-1].get("view")


def _leave_settings_for_folders(app):
    d = app.driver
    d.set_state(_SETTINGS_ROOT_NAV)
    d.wait_for(_ROOT_ELEMENT, timeout=10)
    d.set_state(_FOLDERS_NAV)
    d.wait_for(_FOLDERS_ELEMENT, timeout=10)
    d.set_state(_FEED_NAV)
    assert _current_nav_view(app) == "feed", (
        f"setup: expected to have left Settings for Feed, got nav view "
        f"{_current_nav_view(app)!r}: {app.error_text()!r}"
    )


def test_nav_patch_return_to_settings_lands_on_root(logged_in_app):
    """The agent's own `nav` patch path, as a baseline: `App::apply` never had
    this bug (it computes its edge before mutating `self.page`), so this stays
    green whether or not the fix is applied — it does NOT prove the
    regression fix on its own (see the ring-driven test below for that), but a
    regression here would mean the agent-path door itself broke.
    """
    _leave_settings_for_folders(logged_in_app)
    d = logged_in_app.driver
    d.set_state(_SETTINGS_ROOT_NAV)
    d.wait_for(_ROOT_ELEMENT, timeout=10)
    assert d.is_visible(_ROOT_ELEMENT), (
        f"returning to Settings via a nav patch should land on the rail Root: "
        f"{d.diagnose(_ROOT_ELEMENT)}"
    )
    assert d.is_absent(_FOLDERS_ELEMENT), (
        "returning to Settings via a nav patch should NOT still show Folders: "
        f"{d.diagnose(_FOLDERS_ELEMENT)}"
    )


def test_sidebar_ring_return_to_settings_lands_on_root(logged_in_app):
    """The REAL human path: walk the sidebar focus ring (Up/Down/Tab's
    `App::focus_next`, in the sidebar zone every fresh authenticated session
    starts in) from Feed back onto Settings, and confirm the rail Root paints
    — not the stale Folders sub-page.

    Red before the fix: the ring's own `set_page` call (formerly a direct
    `self.page = page` write with no edge check) never reset `settings.sub`,
    so this would have kept showing `folder-add-button` instead of
    `account-settings-link`.
    """
    _leave_settings_for_folders(logged_in_app)
    d = logged_in_app.driver

    reached = False
    for step in range(_MAX_RING_STEPS):
        if _current_nav_view(logged_in_app) == "settings":
            reached = True
            break
        d.call_command("focus_move", {"direction": "next", "times": 1})
    assert reached, (
        f"the sidebar ring never reached Settings within {_MAX_RING_STEPS} "
        f"focus_next() steps from Feed: last nav view "
        f"{_current_nav_view(logged_in_app)!r}"
    )

    d.wait_for(_ROOT_ELEMENT, timeout=10)
    assert d.is_visible(_ROOT_ELEMENT), (
        "returning to Settings via the sidebar ring should land on the rail "
        f"Root: {d.diagnose(_ROOT_ELEMENT)}"
    )
    assert d.is_absent(_FOLDERS_ELEMENT), (
        "returning to Settings via the sidebar ring should NOT still show "
        f"Folders (the regression): {d.diagnose(_FOLDERS_ELEMENT)}"
    )


def test_folders_nav_back_button_returns_to_root(logged_in_app):
    """The gap the fix did NOT cover, per this file's own docstring: a
    session that goes straight to Folders and never leaves `Page::Settings`
    at all has no sidebar edge to fire the reset on — clicking "Settings"
    again is a same-page no-op (`App::set_page`'s `edge` check). Before this
    fix the ONLY way out was `Esc`, undiscoverable (a live user's report,
    2026-08-03). `settings-nav-back` (`Action::NavBack`,
    `apps/fauna-tui/src/settings/folders.rs`) is the explicit affordance,
    the same pattern `account.rs`/`privacy.rs` already carry.
    """
    d = logged_in_app.driver
    d.set_state(_FOLDERS_NAV)
    d.wait_for(_FOLDERS_ELEMENT, timeout=10)
    assert d.is_visible("settings-nav-back"), (
        f"Folders should render a way back to the Settings hub: "
        f"{d.diagnose('settings-nav-back')}"
    )

    d.click("settings-nav-back")

    d.wait_for(_ROOT_ELEMENT, timeout=10)
    assert d.is_visible(_ROOT_ELEMENT), (
        f"settings-nav-back should return to the rail Root: {d.diagnose(_ROOT_ELEMENT)}"
    )
    assert d.is_absent(_FOLDERS_ELEMENT), (
        f"settings-nav-back should leave Folders: {d.diagnose(_FOLDERS_ELEMENT)}"
    )


# The remaining sub-pages that had the same gap, closed in the same session as
# the tracking item that scoped this sweep. NOT included:
# `engagement_cues.rs`/`trained_topics.rs` (facets of the
# `personalization` sub-page, which already carries `settings-nav-back` via
# `labeler_catalog.rs::personalization_elements`) and `recovery.rs` (a section
# of the `account` sub-page, which already carries it via `account.rs`) — the
# the recipe's per-FILE grep counted these as gaps, but they are not
# separate `SubPage` destinations, so painting a second `settings-nav-back`
# there would have been a duplicate id on an already-fixed page.
_OTHER_NAV_BACK_PAGES = ["atproto", "devices", "logs", "mail-settings", "nests"]


@pytest.mark.parametrize("sub_page_id", _OTHER_NAV_BACK_PAGES)
def test_settings_sub_page_nav_back_button_returns_to_root(logged_in_app, sub_page_id):
    """Same shape as `test_folders_nav_back_button_returns_to_root`, swept
    over the other 5 sub-pages that had the identical Esc-only gap. `mail`'s
    absence was previously excused by a doc comment analogizing it to the
    feed's `post_detail` dialog (a transient overlay, unlike a Settings rail
    destination) — re-derived as a stale analogy, not a deliberate ui.yaml
    scoping choice, so it is included here like the other four.
    """
    d = logged_in_app.driver
    d.set_state(
        {"nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": sub_page_id}]}}
    )
    d.wait_for("settings-nav-back", timeout=10)
    assert d.is_visible("settings-nav-back"), (
        f"{sub_page_id} should render a way back to the Settings hub: "
        f"{d.diagnose('settings-nav-back')}"
    )

    d.click("settings-nav-back")

    d.wait_for(_ROOT_ELEMENT, timeout=10)
    assert d.is_visible(_ROOT_ELEMENT), (
        f"settings-nav-back on {sub_page_id} should return to the rail Root: "
        f"{d.diagnose(_ROOT_ELEMENT)}"
    )
    assert d.is_absent("settings-nav-back"), (
        f"settings-nav-back on {sub_page_id} should leave the sub-page (Root "
        f"paints no settings-nav-back of its own): {d.diagnose('settings-nav-back')}"
    )
