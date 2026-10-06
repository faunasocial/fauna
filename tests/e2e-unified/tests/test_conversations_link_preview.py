"""Cross-app Conversations — the D4 link-preview card in a DM bubble.

``docs/goal/architecture/render-model.md`` § D4; ``tests/e2e-unified/ui.yaml``
``link-preview-card`` (inside ``dm-message-bubble``, children ``link-preview-image``
/ ``-title`` / ``-description`` / ``-domain``).

An inbound DM whose body is a standalone bare url carries a shared
``RenderBlock::LinkPreview`` block (the producer emits it ``Resolving``); once
``ConversationsManager::resolve_link_preview`` resolves it via
``fauna.linkpreview.resolve`` the manager projects ``PreviewState::Resolved`` onto
the block (``thread_detail`` folds it before the D3 reveal walk), and each app
paints the **same** ``link-preview-card`` the feed does (image / title / description
/ domain). A ``Resolving``/``Failed`` block paints **no** card — the inline body link
already shows. Every app reads the one shared signal (the document's
``LinkPreview`` state, priorities #1/#2), never re-fetching the preview per client.

The og:image is **blocked-by-default** (render-model.md § D4, user-ratified
2026-06-27): a content-addressed nest blob, but it obeys the message's D3
remote-content reveal exactly like a body remote image, so the card paints
title/description/domain immediately and the image only after the message's
``load-remote-content-button`` is tapped.

tier_2: the real client driver renders the conversations bubble, but the bubble's
preview is seeded **pre-resolved** via the ``seed_resolved_link_preview`` inject seam
(``ConversationsManager::seed_resolved_link_preview_for_test``) — the conversations
twin of the feed's ``feed_inject_posts`` ``link_preview`` spec. Rationale, identical
to ``test_feed_link_preview.py``: a real resolve needs a live nest fetch of an
OpenGraph page (SSRF-guarded, tier_3), so the ``Resolved`` card paint is seeded
deterministically here. The real nest is needed only for auth (the same tier_2 shape
as the feed link-preview test). ``web`` + ``linux`` + ``tui`` + ``macos`` + ``ios`` +
``windows`` adopt the card here; the android render leg (its card is built) adds its
marker when its e2e inject-seam command lands. Windows joined 2026-09-21: its card
had been built for some time, so the only thing owed was the agent arm
(``ConversationsCommands.SeedResolvedLinkPreview`` over the already-generated UniFFI
binding) — no shared-Rust work at all, the seam was exported the whole time. The apple leg landed that seam: ``DmMessageBubble`` already built the same
``LinkPreviewCard`` leaf the feed paints, so only the agent command was owed —
``ConversationsTestInject.seedResolvedLinkPreview`` (one FaunaKit implementation
for macOS + iOS) over the SAME ``seed_resolved_link_preview_for_test`` UniFFI seam
linux and tui drive.
"""

import pytest

pytestmark = [pytest.mark.tier_2]

# A standalone `[url](url)` markdown paragraph (visible text == href) is the exact
# "bare url" the shared producer turns into a LinkPreview block. FaunaMls inbound is
# `BodyFormat::Markdown`; a bare `https://x` (no `[]()`) does NOT autolink.
LINK_URL = "https://example.com/article"
LINK_BODY = f"[{LINK_URL}]({LINK_URL})"


@pytest.mark.web
@pytest.mark.linux
@pytest.mark.tui
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.feature("link-previews")
def test_conversations_link_preview_card_paints_and_reveals(logged_in_app):
    """An inbound DM whose body is a bare url paints the ``link-preview-card``
    (title / description / domain) once its ``LinkPreview`` block resolves. The
    og:image is **blocked-by-default** (render-model.md § D4): absent + the message's
    ``load-remote-content-button`` shown, until the message is revealed — exactly like
    a body remote image."""
    app = logged_in_app
    conv = app.conversations
    d = app.driver

    # Inject the bubble, then seed the pre-resolved preview for its bare url. The
    # injected block is `Resolving`; the seed makes `thread_detail` fold it
    # `Resolved`, so the client's fire-once resolve sees a non-`Resolving` block and
    # NO real `fauna.linkpreview.resolve` fires (tier_2). Seed-then-open or
    # inject-then-seed both work — `seed` notifies, the next render folds the URL.
    thread_id = conv.inject_and_resolve_thread(
        rail="FaunaMls",
        sender="carol-linkpreview@self-nest.test",
        subject=None,
        body=LINK_BODY,
    )
    conv.seed_resolved_link_preview(
        url=LINK_URL,
        title="Example Article Title",
        description="A short description of the example article.",
        image_hash="ab" * 32,
    )
    conv.open_thread_by_id(thread_id)

    assert d.count("dm-message-text") >= 1, (
        f"no message bubble on select; error={app.error_text()!r}"
    )

    # The resolved card and its text children render.
    assert d.is_visible("link-preview-card"), (
        f"the resolved preview should paint a card; error={app.error_text()!r}"
    )
    assert d.is_visible("link-preview-title")
    assert d.is_visible("link-preview-description")
    assert d.is_visible("link-preview-domain")

    # The card carries the resolved og:title and the url's host (the `url_host`
    # shape — no scheme/path/port).
    assert d.get_text("link-preview-title") == "Example Article Title"
    assert d.get_text("link-preview-domain") == "example.com"

    # Blocked-by-default (D4 reveal gate): the og:image is ABSENT and the message's
    # `load-remote-content-button` is shown (the og:image drives the same blocked-
    # remote predicate as a body remote image — `has_blocked_remote_images`).
    assert d.count("link-preview-image") == 0, (
        "the og:image must be blocked (absent) until the message is revealed"
    )
    assert d.count("load-remote-content-button") == 1, (
        "a blocked og:image must surface the message's reveal button"
    )

    # Reveal the message → the manager projects `revealed: true` onto the og:image
    # and the card wires the image child (its hash → /api/v1/blob/<hash>); the reveal
    # button clears. Assert DOM presence, not is_visible — in tier_2 there is no real
    # nest blob, so the <img> never loads and has an empty bounding box (a real blob
    # paints it in tier_3+/prod); presence proves the reveal un-gated the image.
    conv.reveal_remote_content(0)
    assert d.count("link-preview-image") == 1, (
        "revealing the message must wire the og:image child off image_hash"
    )
    assert d.count("load-remote-content-button") == 0, (
        "the reveal button must clear once the og:image is revealed"
    )
