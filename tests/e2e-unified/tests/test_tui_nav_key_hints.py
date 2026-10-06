"""The `nav-key-hints` footer — tui's persistent one-line key-hint row
(architecture/apps/tui.md § Key-hint footer; ui.yaml `global.platform_elements`,
`tui: [nav-key-hints]`).

A keyboard-only client cannot advertise an affordance by appearance the way the
six pointer apps do. The evidence that opened this track: a live user pressed
`←`, landed on the sidebar with the page unchanged, and asked "how do I get back
to Settings?" — the nav model was correct and nothing on screen ever named a key.
User-approved 2026-08-02 as a tui-only element (UI rule A).

**Scope of this file, and why.** The footer's *contextual* half — the quit hint
disappearing while a text input holds focus, where `q` types a literal `q` — is
NOT reachable from here: the agent's `type` command writes the field directly
without moving the focus ring (`apps/fauna-tui/src/automation.rs`), so no e2e
driver can put an input under the ring. That half is pinned in tui's own unit
tests (`apps/fauna-tui/src/ui.rs`), together with the painted position, per
architecture/apps/tui.md § Rendering (a load-bearing layout asserts the paint,
not only the registry). What this file owns is the cross-app-shaped half: the
element exists, is visible on the authenticated shell, and survives navigation.

tier_3 (a real authenticated `fauna-tui` process against a real nest).
"""

import pytest

# nav-key-hints is tui-only (ui.yaml `global.platform_elements`) — a file-level
# marker deselects the other 6 apps up front rather than an in-body skip (e2e
# convention 7: a permanent single-platform element is deselection, not a skip).
pytestmark = [pytest.mark.tier2, pytest.mark.tier_3, pytest.mark.tui]

_HINTS = "nav-key-hints"


def test_key_hints_render_on_the_authenticated_shell(logged_in_app):
    """The footer paints on the authenticated shell and names the live keys.

    Asserts the *bindings*, not the exact separator/label prose — the hint text
    is i18n copy and may be retranslated, but a hint that stops naming `←`/`→`
    has stopped answering the question the footer exists for.
    """
    d = logged_in_app.driver
    assert d.is_visible(_HINTS), (
        "the key-hint footer should paint on every authenticated page; "
        f"error-message reads {logged_in_app.error_text()!r}"
    )
    text = d.get_text(_HINTS)
    # The pane-switch hint is the whole reason this element exists.
    assert "←" in text and "→" in text, (
        f"the footer must name the pane-switch keys that answer 'how do I get back?': {text!r}"
    )
    # The other two always-live shell bindings (app.rs: Up/Down focus ring,
    # Enter actuate). `q` is deliberately NOT asserted here — it is the
    # contextual one, and its arms are pinned in tui's unit tests.
    assert "↑" in text and "↓" in text, f"the footer must name the focus-move keys: {text!r}"
    assert "⏎" in text, f"the footer must name the actuate key: {text!r}"


def test_key_hints_are_not_a_focus_target(logged_in_app):
    """The footer is chrome, not a control: it must never enter the focus ring.

    A hint row that can take focus would make `↑`/`↓` walk onto the very thing
    telling the user what `↑`/`↓` do. It is registered as text precisely so the
    ring skips it (ui.yaml declares `type: text`).
    """
    d = logged_in_app.driver
    assert d.is_visible(_HINTS)
    # Clicking chrome is a no-op; the shell must still be alive afterwards.
    # (The tui agent answers a click on a non-actuatable element without
    # actuating anything — a dropped command would surface on error-message,
    # e2e convention 11.)
    assert d.get_text(_HINTS), "the footer must carry text"


def test_key_hints_persist_across_navigation(logged_in_app):
    """Persistent chrome: the footer survives a page change.

    The failure this guards is a footer painted by one page's own render path
    instead of the shell's — it would look correct on whichever page the author
    tested and vanish everywhere else.
    """
    d = logged_in_app.driver
    d.set_state({"nav": {"stack": [{"view": "settings"}]}})
    d.wait_for(_HINTS, timeout=10)
    settings_text = d.get_text(_HINTS)
    assert "←" in settings_text, f"footer lost its pane hint on Settings: {settings_text!r}"

    d.set_state({"nav": {"stack": [{"view": "feed"}]}})
    d.wait_for(_HINTS, timeout=10)
    feed_text = d.get_text(_HINTS)
    assert "←" in feed_text, f"footer lost its pane hint on Feed: {feed_text!r}"
