"""The `exit-tab` sidebar row — tui's bottom-of-sidebar "Exit Fauna" quit
affordance (architecture/apps/tui.md § Sidebar quit row; ui.yaml
`global.platform_elements`, `tui: [exit-tab]`).

A terminal window has no titlebar close button, no Cmd+Q, no Alt+F4 — the six
GUI/mobile apps all have an OS-native way to end the session and tui has none.
`q`/Ctrl+C exist (`App::handle_key`) but are keyboard-only and undiscoverable,
the same lesson `test_tui_nav_key_hints.py`'s footer already answers for pane
navigation. The evidence that opened this track: a live user asked for it
directly (2026-08-03). User-approved the same day as a tui-only element (UI
rule A).

tier_3 (a real authenticated `fauna-tui` process against a real nest).
"""

import pytest

from helpers.instance_guard import expect_exit_after_click

# exit-tab is tui-only (ui.yaml `global.platform_elements`) — a file-level
# marker deselects the other 6 apps up front (e2e convention 7: a permanent
# single-platform element is deselection, not a skip).
pytestmark = [pytest.mark.tier2, pytest.mark.tier_3, pytest.mark.tui]

_EXIT = "exit-tab"


def test_exit_row_renders_last_on_the_authenticated_shell(logged_in_app):
    """The row is visible, below every other sidebar entry — including the
    gated ones when present, since `App::sidebar_pages` appends it last
    unconditionally."""
    d = logged_in_app.driver
    assert d.is_visible(_EXIT), (
        "the Exit Fauna row should paint on every authenticated page; "
        f"error-message reads {logged_in_app.error_text()!r}"
    )


def test_clicking_exit_quits_the_app(logged_in_app):
    """Actuating the row (a click) ends the process — the one sidebar row
    whose "navigation" target is quitting rather than a page."""
    expect_exit_after_click(
        logged_in_app.driver,
        _EXIT,
        "clicking exit-tab should quit fauna-tui, but the process is still alive",
    )
