"""Compose-field inline-markdown styling contract
(``docs/goal/ui/conversations.md`` § Compose-field inline markdown styling).

The windows DM compose field (``dm-text-field``) is a ``RichEditBox`` used as an
*in-place decoration surface* over the literal markdown source (markers stay in the
buffer; ``Document.GetText`` returns the literal string; source ranges are styled via
``ITextRange.CharacterFormat``). The single hardest constraint of that rewrite is that
the field must keep satisfying the e2e contracts the FlaUI bridge relies on:

  * **value contract (ValuePattern)** — ``type_text`` (SetValue) + ``get_text`` (Value).
    A plain WinUI ``TextBox`` satisfies it natively; a bare ``RichEditBox`` does NOT (its
    automation peer exposes TextPattern, not IValueProvider). The rewrite therefore
    carries a custom ``IValueProvider`` automation peer (``MarkdownRichEditBox``).
  * **toolbar-wrap contract** — the shared markdown toolbar wraps/​prefixes the field's
    selection through the ``IMarkdownEditTarget`` seam (constraint 4).

This is the regression gate that proves both hold across the swap. One composer-open,
order-independent. The assertions are universal contracts (text typed reads back; the
toolbar inserts literal markers), so the test is not platform-skipped; it is motivated by
— and primarily run for — ``--client windows``.

tier_3: real nest binary + real client driver, no mocks.
"""
import pytest

pytestmark = [pytest.mark.tier1, pytest.mark.tier_3]


@pytest.mark.feature("message-formatting")
def test_compose_richeditbox_value_and_toolbar(logged_in_app):
    """The RichEditBox compose field keeps both the ValuePattern set/get contract and
    the shared-toolbar-wrap contract, and holds *literal markdown source* (decoration
    surface, not WYSIWYG — conversations.md § Compose-field inline markdown styling /
    § Why inline-styling, not WYSIWYG)."""
    app = logged_in_app
    app.conversations.navigate()
    app.driver.click("new-conversation-button")
    app.driver.wait_for("dm-text-field")

    # Constraint 4 — the shared markdown toolbar wraps over the RichEditBox selection
    # via the IMarkdownEditTarget seam. With an empty selection, bold inserts the
    # literal "**text**" placeholder (MarkdownAuthoring.WrapSelection).
    #
    # Read via `compose_body_text()`, not a raw `driver.get_text("dm-text-field")`:
    # web's compose field is a CodeMirror 6 editor that HIDES inline markers outside
    # the caret's edge (`MarkdownEditor.svelte`'s default marker-visibility mode) and
    # virtualizes its viewport, so a rendered-DOM read is neither the literal source
    # nor complete — `compose_body_text()` is the per-app helper that already reads
    # the true buffer (web: the `window.__fauna_editor_docs` registry; windows: the
    # RichEditBox's own ValuePattern).
    app.driver.clear_and_type("dm-text-field", "")
    app.driver.click("markdown-bold-button")
    bolded = app.conversations.compose_body_text()
    assert bolded == "**text**", (
        f"markdown-bold-button did not wrap over the RichEditBox: {bolded!r} "
        f"({app.driver.diagnose('dm-text-field')})"
    )

    # Constraint 3 + literal-source — typed markdown round-trips verbatim (markers
    # present): ValuePattern set/get via the custom IValueProvider peer, decoration
    # surface (no rich-text tree, no serialize-on-send boundary). A literal-source send
    # therefore transmits exactly what was typed.
    src = "a *b* `c` **d** # e"
    app.driver.clear_and_type("dm-text-field", src)
    got = app.conversations.compose_body_text()
    assert got == src, (
        f"dm-text-field did not round-trip literal markdown: {got!r} "
        f"({app.driver.diagnose('dm-text-field')})"
    )


@pytest.mark.windows
def test_compose_edit_and_bold_wrap_on_markdown_thread_does_not_crash(logged_in_app):
    """Editing a markdown-capable thread's compose field and clicking the bold
    toolbar button must not crash the windows app, and the value/wrap contracts
    must hold.

    Context. The in-place ``RichEditBox`` live-preview decoration (``DmComposeBar``
    applying ``ITextRange.CharacterFormat``) was DISABLED 2026-06-20 — run
    synchronously per keystroke it wedged the native RichEdit layer into a busy-loop
    HANG (``test ``) and tripped a WinRT ``E_BOUNDS`` (``0x8000000b``) fail-fast CRASH
    on the bold wrap — then RE-ENABLED 2026-06-21, sound, by deferring + debouncing the
    pass off the synchronous notification so it no longer mutates formatting
    re-entrantly inside the keystroke insert.

    This test guards the contracts the headless harness CAN check — value
    (SetValue/get) + the shared toolbar wrap — over a markdown-capable thread, and (with
    decoration now re-enabled) that the deferred applier doesn't crash on the bold wrap
    via the SetValue path. It does NOT exercise the per-keystroke HANG: that needs real
    key input the headless e2e harness cannot inject (SendInput is foreground-blocked;
    programmatic ``Selection.TypeText``/``SetText`` no-op without an input context;
    ``SetValue`` rebuilds the document in one shot), so the no-hang behavior is validated
    manually (conversations.md windows clause).

    conversations.md § Compose-field inline markdown styling (windows clause).
    Windows-only: the surface is the WinUI ``RichEditBox``.
    """
    app = logged_in_app
    conv = app.conversations

    # FaunaMls is markdown-capable; opening this injected thread is the markdown path
    # (toolbar enabled). The decoration applier is currently hard-disabled, so this
    # exercises the value + wrap contracts that must hold regardless.
    conv.inject_and_open_thread(rail="FaunaMls", sender="bob-composestyle@self-nest.test", body="hi")
    app.driver.wait_for("dm-text-field")

    app.driver.clear_and_type("dm-text-field", "world")
    app.driver.click("markdown-bold-button")

    # The app must still be alive (a crash would take the field down with it) and the
    # bold wrap must have spliced markers into the literal source. With an empty
    # selection the shared rule inserts the "text" placeholder, so "world" + bold
    # yields e.g. "world**text**" (markers present; literal-source contract).
    assert app.driver.is_visible("dm-text-field"), (
        "dm-text-field is gone after editing the compose field — the app crashed. "
        f"{app.driver.diagnose('dm-text-field')}"
    )
    got = app.driver.get_text("dm-text-field")
    assert "world" in got and "**" in got, (
        f"compose field lost its content / the bold wrap after the edit: {got!r}"
    )
