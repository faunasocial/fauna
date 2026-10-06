"""The `tui-settings` page — the tui app's own settings sub-page
(architecture/apps/tui.md § External media handoff; ui.yaml `tui-settings`,
`platforms: [tui]`).

The one page exclusive to the tui app: the external-media handoff preference
(ask / always / never) that governs whether an audio/video attachment is handed
to the OS default player. The setting is client-local (a tui rendering
preference), so these tests drive the **select** through the UI and assert it
renders, defaults to `ask`, changes, and holds across in-session navigation.

The actual OS handoff (the `xdg-open`/`open`/`start` spawn) and the *Ask* inline
prompt land with M6 (the media page, where a real "open this clip" affordance
exists to gate); the gate + spawn seam is proven by the Rust unit tests in
`apps/fauna-tui/src/settings.rs`. Reached through `logged_in_app` because the
Settings shell only exists in the authenticated UI.

tier_3 (a real authenticated client process; the setting itself is client-local,
but the auth path that gates the shell is the full stack).
"""
import pytest

# tui-settings is a tui-only page (ui.yaml `tui-settings`, `platforms: [tui]`) —
# a file-level marker deselects the other 6 apps up front rather than an
# in-body skip (e2e convention 7: a runtime skip must declare its class, but a
# permanent single-platform page is better expressed as deselection).
pytestmark = [pytest.mark.tier2, pytest.mark.tier_3, pytest.mark.tui]

# The two-element Settings sub-page nav (settings.md § Navigation model):
# `{"view":"settings"},{"view":"settings","id":"tui-settings"}`.
_NAV = {"nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "tui-settings"}]}}
_SELECT = "tui-settings-external-media"


def _open(app):
    """Enter the tui-settings sub-page."""
    app.driver.set_state(_NAV)
    app.driver.wait_for(_SELECT, timeout=10)


def test_external_media_select_renders_and_defaults_to_ask(logged_in_app):
    """The external-media select renders and defaults to `ask` (the doc default:
    prompt inline before launching anything)."""
    _open(logged_in_app)
    assert logged_in_app.driver.is_visible(_SELECT), (
        "the external-media select should render on tui-settings: "
        f"{logged_in_app.driver.diagnose(_SELECT)}"
    )
    assert logged_in_app.driver.get_text(_SELECT) == "ask", (
        "the setting defaults to `ask` (tui.md § External media handoff): "
        f"got {logged_in_app.driver.get_text(_SELECT)!r}"
    )


def test_external_media_select_changes_and_holds_across_navigation(logged_in_app):
    """Selecting a mode is reflected by `get_text`, and the choice holds after
    navigating away and back (in-session client-local persistence)."""
    d = logged_in_app.driver
    _open(logged_in_app)

    for mode in ("always", "never", "ask"):
        d.select(_SELECT, mode)
        assert d.get_text(_SELECT) == mode, (
            f"selecting {mode!r} should be reflected by get_text, "
            f"got {d.get_text(_SELECT)!r}"
        )

    # Set a non-default value, leave the page, come back — it must still hold.
    d.select(_SELECT, "always")
    assert d.get_text(_SELECT) == "always"
    d.set_state({"nav": {"stack": [{"view": "feed"}]}})
    d.wait_for("feed-tab", timeout=10)
    d.set_state(_NAV)
    d.wait_for(_SELECT, timeout=10)
    assert d.get_text(_SELECT) == "always", (
        "the external-media choice should hold across in-session navigation "
        f"(client-local pref), got {d.get_text(_SELECT)!r}"
    )


def test_settings_nav_back_leaves_the_sub_page(logged_in_app):
    """`settings-nav-back` exits the sub-page back to the Settings rail — the
    select is no longer present after."""
    _open(logged_in_app)
    d = logged_in_app.driver
    assert d.is_visible("settings-nav-back"), (
        "the tui-settings sub-page exposes the settings-nav-back affordance"
    )
    d.click("settings-nav-back")
    # Back on the rail Root, the external-media select is gone.
    assert d.is_absent(_SELECT), (
        "settings-nav-back should leave the tui-settings sub-page "
        f"(the select should no longer render): {d.diagnose(_SELECT)}"
    )
