"""Conversation markdown: list snippet is plaintext; detail renders formatted.

Regression for the linux UI-review findings (item 2):

* (b) The conversation-list row showed literal ``**bold**`` instead of ``bold``
  — the snippet ran no markdown->plaintext strip. Fixed in shared Rust
  (``fauna_core::markdown::markdown_to_plaintext`` used by
  ``fauna_conversations`` ``summarize``), so every app previews identically.

* (a) Clicking a conversation was reported to show an "empty" detail. The store
  is *not* empty on select — an injected/sent message is appended and the list
  passes the real thread id — so the detail loads its message. The actual
  "raw source, not formatted" defect was that *every* message was stamped
  ``BodyFormat::PlainText`` (the markdown render arm never ran); markdown-capable
  rails (FaunaMls) now stamp ``Markdown`` so the bubble renders ``**bold**``
  formatted (markers stripped in the rendered text).

Driven through the e2e mock-backend inject seam, which exercises the same shared
``summarize`` snippet path and the same per-``body_format`` bubble renderer as
production.
"""

import pytest

# Verified clients: linux (the original UI-review regression) + web (the
# per-`body_format` bubble render landed here too). The native UniFFI apps
# (windows/macos/ios/android) already branch on `body_format`; each adds its
# marker when it runs this through its inject seam.
pytestmark = [pytest.mark.tier_3, pytest.mark.linux, pytest.mark.web, pytest.mark.tui]


@pytest.mark.feature("message-formatting")
def test_list_snippet_is_plaintext_not_markdown(logged_in_app):
    """A ``**bold**`` body previews as plaintext ``bold preview`` in the list
    row — in the shared snapshot snippet and in the rendered ``dm-subject``
    element — never as raw ``**bold**``."""
    conv = logged_in_app.conversations
    thread_id = conv.inject_and_resolve_thread(
        rail="Smtp", sender="alice-markdown@host.test", subject=None, body="**bold** preview"
    )
    conv._ensure_on_conversations_page()

    # Shared snapshot snippet (the fauna-conversations `summarize` output), read from
    # the injected thread BY IDENTITY — a "bold"/"alice" match would also select the
    # sibling test's `**bold** body` thread, whichever sorted first.
    threads = conv.list_threads()
    injected = next((t for t in threads if t.thread_id == thread_id), None)
    assert injected is not None, (
        f"injected thread {thread_id} absent; have {[(t.thread_id, t.snippet) for t in threads]!r}"
    )
    assert injected.snippet == "bold preview", injected.snippet

    # Rendered list-row element: no row shows raw markdown; the injected one
    # previews as plaintext.
    n = logged_in_app.driver.count("dm-subject")
    rendered = [logged_in_app.driver.get_text("dm-subject", index=i) for i in range(n)]
    assert "bold preview" in rendered, rendered
    assert not any("**" in r for r in rendered), f"raw markdown in a snippet: {rendered!r}"


# macos+ios: apple Leg-1 inject handlers rail-derive bodyFormat (
# FaunaMacApp.swift:929 / FaunaApp.swift:575) so a FaunaMls body parses as
# markdown — both apple DM details strip `**` end-to-end (re-verified by this
# gate: diag DM verdict STRIPPED + the test GREEN
# on both apps). windows: DmMessageBubble.RenderBody paints the shared
# render-model document via DocumentPainter.Apply(BodyText="dm-message-text",
# _document) (markdown parsed shared-side into inlines, `**` stripped in the
# painted text — the same path apple/linux/web use); inject_and_open_thread is
# driver-agnostic (conversation-item rows). (Only this DETAIL test generalizes to
# windows — `dm-subject` is a list-row element on linux/web but a bubble element
# on windows, so the list-snippet test above stays linux+web.)
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.feature("message-formatting")
def test_detail_renders_markdown_on_select(logged_in_app):
    """Selecting a FaunaMls conversation opens the detail pane with its message
    bubble rendered formatted (the markdown markers stripped from the rendered
    text) — proving the detail is not empty on select and not raw source."""
    conv = logged_in_app.conversations
    conv.inject_and_open_thread(
        rail="FaunaMls", sender="bob-markdown@self-nest.test", subject=None, body="**bold** body"
    )

    # Detail pane open (thread-header visible) and the message bubble present.
    assert logged_in_app.driver.is_visible("thread-header"), "detail empty on select"
    assert logged_in_app.driver.count("dm-message-text") >= 1, "no message bubble"

    text = logged_in_app.driver.get_text("dm-message-text", index=0)
    assert text.strip(), "message bubble text empty"
    assert "bold body" in text, text
    # FaunaMls is markdown-capable, so the rendered text drops the `**` markers.
    assert "**" not in text, f"bubble rendered raw markdown source: {text!r}"
