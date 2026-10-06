"""tier_3: the compose field hides inline markdown markers by default, and a
per-editor toggle (``markdown-marker-toggle-button``) reveals them — the
cross-app acceptance test for the hide-by-default compose editor
(``docs/goal/ui/conversations.md`` § Compose-field inline markdown styling).

The shared hide/dim policy (``fauna_core::markdown::compose_decoration_plan``) is
unit-proven (Rust) and the web applier is browser-harness-proven; this proves the
REAL conversations compose-page wiring: the toggle renders, the default conceals
the inline emphasis markers, and clicking it reveals them (dimmed) — uniformly on
web and linux (the two apps that run together on the shared Linux dev host), on
windows, on android, and — since 2026-08-06 — on macOS + iOS, which share one FaunaKit
compose field. Each native leg consumes the same shared plan via the
``compose_decoration_plan`` UniFFI face; ``--client`` deselects any unwired one.

Apple conceals at the **glyph** layer (``MarkdownConcealingGlyphDelegate`` answers
TextKit 1's ``shouldGenerateGlyphs`` with ``.null`` for the decorator's concealed runs),
so the text storage keeps every marker character and the exact-source assertion below is
a real check on apple too, not a tautology.

Enter-still-sends is structurally preserved — the web ``composeHideExtension`` binds
no Enter key, so Enter falls through to the compose send handler, and the broader
conversations send suite now runs under this hide-by-default. This test instead proves
the editor preserves the real markdown SOURCE across the toggle (so a send carries the
true bytes): revealing must restore the EXACT ``**bold** trailing`` — concealment is
visual only, never a mutation of the buffer source. (That exact-match assertion caught a
real linux hide-mode bug: the body-change/render reads used ``include_hidden_chars=false``,
so concealing a marker dropped it from the forwarded draft — fixed in the same change.)

tui is a **declared absence**, not buildout: this test asserts the markers are
*concealed by default*, which is a property of the decoration engine, and
``conversations.md`` § Compose-field inline markdown styling ratifies that "the
tui's terminal composer shows the markers literally (a terminal cell grid has no
live-preview type styling — the toolbar wrap buttons are shared, the decoration
engine is GUI-only)". With one marker mode there is nothing for a per-editor
toggle to flip. The marker is deliberately WIDENED to tui anyway so the absence
reports itself as a cited ``s`` every run rather than vanishing into the
deselect delta (the mode-1 blindness); the
wrap buttons themselves — including heading/list — are covered on tui by
``test_conversations_markdown_toolbar_wrap.py``.
"""

import time

import pytest

from helpers.app_surface import declared_absence

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.web,
    pytest.mark.linux,
    pytest.mark.windows,
    pytest.mark.android,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.tui,
]


def _wait_visible(conv, pred, timeout: float = 5.0) -> str:
    """Poll the compose field's visible text until ``pred`` holds (or time out).
    The decoration re-apply after a type / toggle is async (a CM frame on web, the
    GTK ``changed``/``cursor-position`` handlers on linux), so read condition-based
    rather than once (e2e action-layer convention). Returns the last value read."""
    deadline = time.time() + timeout
    last = ""
    while time.time() < deadline:
        last = conv.compose_visible_text()
        if pred(last):
            return last
        time.sleep(0.1)
    return last


@pytest.mark.feature("formatting-as-you-type")
def test_compose_hides_inline_markers_by_default_and_toggle_reveals(logged_in_app):
    app = logged_in_app
    if app.driver.is_tui():
        declared_absence(
            app.driver,
            capability="compose-field marker concealment (and therefore a "
            "per-editor markdown-marker-toggle-button, which flips between the "
            "hidden and dimmed decoration modes)",
            doc="ui/conversations.md § Compose-field inline markdown styling — "
            "'the tui's terminal composer shows the markers literally (a "
            "terminal cell grid has no live-preview type styling — the toolbar "
            "wrap buttons are shared, the decoration engine is GUI-only)'",
        )
    driver = app.driver
    conv = app.conversations

    # Open the new-thread composer — it renders the compose field + markdown toolbar
    # off the snapshot with no recipient (markdown is enabled by default until a rail's
    # caps resolve), the same chokepoint test_messaging::test_markdown_toolbar_visible
    # and the draft-persistence test drive.
    conv.navigate()
    driver.click("new-conversation-button")
    driver.wait_for("dm-text-field", timeout=10.0)

    # The per-editor marker-visibility toggle is wired into the real compose toolbar.
    assert driver.is_visible("markdown-marker-toggle-button"), (
        "the compose toolbar should render the marker-visibility toggle: "
        f"{driver.diagnose('markdown-marker-toggle-button')}"
    )

    # clear_and_type (not type_text): `nest_instance`/`test_user` are session-scoped, so in a
    # combined `--client web,linux` run both apps share one nest + actor, and the first
    # client's new-thread compose draft is RESTORED into the second client's composer on launch
    # (conversations.md § Persistence). A bare type_text would APPEND to that restored
    # `**bold** trailing`, doubling the buffer — clearing first makes the test independent of any
    # restored/leftover draft. The trailing word puts the caret AWAY from the `**bold**` run (the
    # caret-edge reveal would otherwise reveal a run the caret sits inside).
    driver.clear_and_type("dm-text-field", "**bold** trailing")

    # Default = markers HIDDEN: the visible text shows `bold trailing` with no `**` markers.
    vis = _wait_visible(conv, lambda v: "bold trailing" in v and "**" not in v)
    assert "bold trailing" in vis and "**" not in vis, (
        f"inline `**` markers should be concealed by default; visible text = {vis!r}"
    )

    # Click the toggle → markers REVEALED (shown dimmed): the full source is visible again.
    # Assert the EXACT text: this both proves the reveal AND that the hidden markers were
    # preserved in the source (a hide-mode bug that dropped the `**` from the buffer would
    # surface here as a missing/garbled marker, not just a missing substring).
    driver.click("markdown-marker-toggle-button")
    vis_shown = _wait_visible(conv, lambda v: v == "**bold** trailing")
    assert vis_shown == "**bold** trailing", (
        f"toggling should reveal the exact `**bold** trailing` source; visible text = {vis_shown!r}"
    )

    # Toggle back → concealed again (a real per-editor flip, not one-way).
    driver.click("markdown-marker-toggle-button")
    vis_again = _wait_visible(conv, lambda v: "bold trailing" in v and "**" not in v)
    assert "bold trailing" in vis_again and "**" not in vis_again, (
        f"toggling back should re-conceal the `**` markers; visible text = {vis_again!r}"
    )

    # Hygiene: discard the new-thread draft so this body doesn't persist into the shared
    # session nest and pollute another client's composer (the restore path above).
    conv.cancel_new_conversation()
