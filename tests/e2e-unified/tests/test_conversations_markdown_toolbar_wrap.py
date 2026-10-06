"""Cross-app compose markdown toolbar — italic / code / link wrap.

``tests/e2e-unified/ui.yaml`` ``markdown-toolbar`` (``markdown-italic-button``,
``markdown-code-button``, ``markdown-link-button``); the bold button's
equivalent round-trip is already covered
(``test_messaging.py::test_markdown_toolbar_visible``,
``test_compose_richeditbox_value_and_toolbar`` windows-specific). These three
were unreferenced by any test or action file until now.

Each button wraps the (empty) selection via the shared
``fauna_core::markdown::wrap_selection`` rule with the ``"text"`` placeholder —
linux ``compose_toolbar::wrap_selection`` / web ``wrapMarkdownSelection`` (over
wasm) / tui ``wrap_compose_body`` (direct Rust) / apple ``MarkdownCompose.wrap``
(over UniFFI) — so an empty ``dm-text-field`` + one click produces the literal
marked-up placeholder on every app (the wrap rule itself is platform-independent
shared Rust).

The italic/code/link trio widened to **macOS + iOS on 2026-08-06**: apple's wrap
had Swift unit coverage (``MarkdownComposeTests``) but no cross-app e2e, so the
one thing those units cannot see — that the real toolbar button is wired to the
real compose field — was untested. It reads the SOURCE (``get_text``), so the
2026-08-06 marker concealment does not touch these assertions. Heading/list stay
web/linux/tui: they are ``optional_elements`` and apple renders neither.

tier_3: real nest binary + real client driver, no mocks (mirrors
test_messaging.py's other conversations-chrome tests).
"""

import pytest

pytestmark = [pytest.mark.tier_3]


@pytest.mark.web
@pytest.mark.linux
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.tui
@pytest.mark.feature("message-formatting")
def test_italic_button_wraps_selection(logged_in_app):
    """``markdown-italic-button`` wraps an empty selection into ``*text*``."""
    app = logged_in_app
    app.conversations.navigate()
    app.driver.click("new-conversation-button")
    app.driver.wait_for("dm-text-field")

    app.driver.clear_and_type("dm-text-field", "")
    app.driver.click("markdown-italic-button")
    got = app.driver.get_text("dm-text-field")
    assert got == "*text*", (
        f"markdown-italic-button did not wrap the empty selection: {got!r} "
        f"({app.driver.diagnose('dm-text-field')})"
    )


@pytest.mark.web
@pytest.mark.linux
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.tui
@pytest.mark.feature("message-formatting")
def test_code_button_wraps_selection(logged_in_app):
    """``markdown-code-button`` wraps an empty selection into `` `text` ``."""
    app = logged_in_app
    app.conversations.navigate()
    app.driver.click("new-conversation-button")
    app.driver.wait_for("dm-text-field")

    app.driver.clear_and_type("dm-text-field", "")
    app.driver.click("markdown-code-button")
    got = app.driver.get_text("dm-text-field")
    assert got == "`text`", (
        f"markdown-code-button did not wrap the empty selection: {got!r} "
        f"({app.driver.diagnose('dm-text-field')})"
    )


@pytest.mark.web
@pytest.mark.linux
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.tui
@pytest.mark.feature("message-formatting")
def test_link_button_wraps_selection(logged_in_app):
    """``markdown-link-button`` wraps an empty selection into ``[text](url)``."""
    app = logged_in_app
    app.conversations.navigate()
    app.driver.click("new-conversation-button")
    app.driver.wait_for("dm-text-field")

    app.driver.clear_and_type("dm-text-field", "")
    app.driver.click("markdown-link-button")
    got = app.driver.get_text("dm-text-field")
    assert got == "[text](url)", (
        f"markdown-link-button did not wrap the empty selection: {got!r} "
        f"({app.driver.diagnose('dm-text-field')})"
    )


# markdown-heading-button / markdown-list-button are `optional_elements` — web's
# MarkdownToolbar.svelte always renders both; linux wires them into the one toolbar
# it actually uses (compose_toolbar::build_compact_toolbar) so the id is reachable
# from the same in-pane compose every other button here uses; tui joined 2026-07-30
# (the lead app paints the richest toolbar — `markdown_toolbar()` in
# `apps/fauna-tui/src/conversations/mod.rs`). All three pass the same prefix-only
# marker pair to the shared `wrap_selection`, which is why one assertion serves
# every app.


@pytest.mark.web
@pytest.mark.linux
@pytest.mark.tui
@pytest.mark.feature("message-formatting")
def test_heading_button_wraps_selection(logged_in_app):
    """``markdown-heading-button`` turns an empty selection into ``## text``
    (web's own H2 prefix; linux mirrors it for cross-app uniformity)."""
    app = logged_in_app
    app.conversations.navigate()
    app.driver.click("new-conversation-button")
    app.driver.wait_for("dm-text-field")

    app.driver.clear_and_type("dm-text-field", "")
    app.driver.click("markdown-heading-button")
    got = app.driver.get_text("dm-text-field")
    assert got == "## text", (
        f"markdown-heading-button did not prefix the empty selection: {got!r} "
        f"({app.driver.diagnose('dm-text-field')})"
    )


@pytest.mark.web
@pytest.mark.linux
@pytest.mark.tui
@pytest.mark.feature("message-formatting")
def test_list_button_wraps_selection(logged_in_app):
    """``markdown-list-button`` turns an empty selection into ``- text``."""
    app = logged_in_app
    app.conversations.navigate()
    app.driver.click("new-conversation-button")
    app.driver.wait_for("dm-text-field")

    app.driver.clear_and_type("dm-text-field", "")
    app.driver.click("markdown-list-button")
    got = app.driver.get_text("dm-text-field")
    assert got == "- text", (
        f"markdown-list-button did not prefix the empty selection: {got!r} "
        f"({app.driver.diagnose('dm-text-field')})"
    )
