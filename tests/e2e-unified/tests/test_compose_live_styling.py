"""tier_3: the compose field styles markdown AS YOU TYPE, and moving the caret into a
formatted run brings its concealed marks back so they can be edited
(``docs/goal/ui/conversations.md`` § Compose-field inline markdown styling: "`**bold**`
… appear **visually styled** in the editor (bold/italic font, monospace, larger heading
text, quote indent), while the markdown **markers stay in the buffer**", and the hidden
markers are "revealed only at the caret *edge*").

Witnessed on **linux**, the decoration lead (§ Implementation status: "linux (lead,
``gtk::TextTag``s in ``compose_decoration.rs``)"), and the one app whose applied styling
a driver can read: ``get_attr("dm-text-field", "text-runs")`` returns the live
``gtk::TextBuffer``'s runs and the tags each carries — what the app APPLIED, never a
recomputation of the shared decoration plan it was handed. The caret moves by
``press_key``, which on linux emits the text view's own ``move-cursor`` keybinding signal,
so the caret-move handler that re-decorates runs exactly as for a real arrow key.

Writing this found the quote half of outcome 6 unbuilt on linux: its compose quote tag was
italic and grey with no indent, while the rendered bubble indents quotes 12px. The quote
now carries a paragraph-level indent tag over the whole quoted line.

tui is a **declared absence**, cited on every run: "the tui's terminal composer shows the
markers literally (a terminal cell grid has no live-preview type styling — the toolbar
wrap buttons are shared, the decoration engine is GUI-only)". web joined through the
cross-app lift row with a computed-style read in linux's shape; macos and ios read the
live ``NSTextStorage`` of the field's real text view (``MarkdownFieldHandle.textRuns``)
and move the caret through that view's own caret actions. windows joined the same day:
its driver answers ``text-runs`` through the app's ``compose_text_runs`` command, which
reads the live RichEdit document's character and paragraph formats in-process
(``MarkdownRichEditBox.TextRuns``; the field's UIA HelpText already carries its
``visible`` channel), and its quote line gained the indent it lacked. android answers
``text-runs`` the windows way, through its own ``compose_text_runs`` command, which
serializes the AnnotatedString the field's VisualTransformation last applied
(``appliedRuns`` in ``MarkdownCompose.kt``); its quote line likewise gained a 12sp
paragraph indent, and its caret moves by the bridge's ``/element/key``, which delivers
a hardware key event to the focused field.
"""

import json

import pytest

from helpers.app_surface import declared_absence
from helpers.budgets import UI_SETTLE_S
from helpers.waiting import wait_until

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.linux,
    pytest.mark.tui,
    pytest.mark.web,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.windows,
    pytest.mark.android,
]

_TUI_ABSENCE_DOC = (
    "ui/conversations.md § Compose-field inline markdown styling — 'the tui's terminal "
    "composer shows the markers literally (a terminal cell grid has no live-preview type "
    "styling — the toolbar wrap buttons are shared, the decoration engine is GUI-only)'"
)


def _open_composer(app) -> None:
    app.conversations.navigate()
    app.driver.click("new-conversation-button")
    app.driver.wait_for("dm-text-field", timeout=10.0)


def _looks(driver, word: str) -> list[dict]:
    """Every tag applied to the run(s) of ``dm-text-field`` holding ``word``."""
    raw = driver.get_attr("dm-text-field", "text-runs")
    assert raw, f"the app publishes no text-runs read of its compose field: {raw!r}"
    runs = json.loads(raw)
    hits = [tag for run in runs if word in run["text"] for tag in run["tags"]]
    held = [run["text"] for run in runs]
    assert any(word in text for text in held), f"{word!r} is in no run of {held}"
    return hits


@pytest.mark.feature("formatting-as-you-type")
def test_compose_styles_markdown_as_you_type(logged_in_app):
    """Bold looks bold, code is monospace, a heading is larger and a quote is indented —
    and a plain word on the same line carries none of those looks, so a field that styled
    everything (or a read that reported every tag everywhere) fails."""
    app = logged_in_app
    if app.driver.is_tui():
        declared_absence(
            app.driver,
            capability="live compose-field styling (bold, monospace code, larger "
            "headings, indented quotes as you type)",
            doc=_TUI_ABSENCE_DOC,
        )
    driver = app.driver
    _open_composer(app)
    driver.clear_and_type(
        "dm-text-field", "**boldword** `codeword` plainword\n# headword\n> quoteword"
    )

    def styled() -> bool:
        return any((t.get("weight") or 0) >= 700 for t in _looks(driver, "boldword"))

    wait_until(
        styled,
        UI_SETTLE_S,
        diagnose=lambda: f"bold never styled: {driver.get_attr('dm-text-field', 'text-runs')}",
    )
    assert any("mono" in (t.get("family") or "") for t in _looks(driver, "codeword")), (
        f"code must be monospace: {_looks(driver, 'codeword')}"
    )
    assert any((t.get("scale") or 1.0) > 1.0 for t in _looks(driver, "headword")), (
        f"a heading must be larger: {_looks(driver, 'headword')}"
    )
    assert any((t.get("left_margin") or 0) > 0 for t in _looks(driver, "quoteword")), (
        f"a quote must be indented: {_looks(driver, 'quoteword')}"
    )
    plain = _looks(driver, "plainword")
    assert not any(
        (t.get("weight") or 0) >= 700
        or "mono" in (t.get("family") or "")
        or (t.get("scale") or 1.0) > 1.0
        or (t.get("left_margin") or 0) > 0
        for t in plain
    ), f"an unformatted word must carry no formatting look: {plain}"
    # The source through the shared read (web's CodeMirror field hides the markers from
    # its DOM text, so the editor's own document is the read there).
    assert app.conversations.compose_body_text().startswith("**boldword** `codeword`"), (
        "the styling is decoration only — the field still holds the markdown source"
    )
    app.conversations.cancel_new_conversation()


@pytest.mark.feature("formatting-as-you-type")
def test_moving_the_caret_into_formatted_text_brings_its_marks_back(logged_in_app):
    """With the caret outside ``**bold**`` its marks are hidden; one step into the run's
    edge and they are back, exactly as typed; leaving again hides them. Eight steps left
    from the end put the caret at the start of ``trailing`` — outside the run — and the
    ninth reaches the run's closing edge, so the reveal is pinned to the caret entering
    the run, not to any caret movement at all."""
    app = logged_in_app
    if app.driver.is_tui():
        declared_absence(
            app.driver,
            capability="caret-edge marker reveal in the compose field (markers are "
            "never hidden, so there is nothing to bring back)",
            doc=_TUI_ABSENCE_DOC,
        )
    driver = app.driver
    conv = app.conversations
    _open_composer(app)
    driver.clear_and_type("dm-text-field", "**bold** trailing")

    def hidden() -> bool:
        vis = conv.compose_visible_text()
        return "bold trailing" in vis and "**" not in vis

    wait_until(hidden, UI_SETTLE_S,
               diagnose=lambda: f"markers should start hidden: {conv.compose_visible_text()!r}")

    for _ in range(len(" trailing") - 1):
        driver.press_key("dm-text-field", "ArrowLeft")
    assert hidden(), (
        "with the caret at the start of 'trailing' — outside the run — the marks stay "
        f"hidden: {conv.compose_visible_text()!r}"
    )

    driver.press_key("dm-text-field", "ArrowLeft")
    wait_until(
        lambda: conv.compose_visible_text() == "**bold** trailing",
        UI_SETTLE_S,
        diagnose=lambda: "the caret at the run's edge must bring its marks back: "
        f"{conv.compose_visible_text()!r}",
    )

    driver.press_key("dm-text-field", "End")
    wait_until(hidden, UI_SETTLE_S,
               diagnose=lambda: f"leaving the run must hide its marks again: {conv.compose_visible_text()!r}")
    conv.cancel_new_conversation()
