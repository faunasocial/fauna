"""Cross-app Feed — the D4 link-preview card.

``docs/goal/architecture/render-model.md`` § D4; ``tests/e2e-unified/ui.yaml``
``link-preview-card`` (inside ``post-card``, children ``link-preview-image`` /
``-title`` / ``-description`` / ``-domain``).

A post whose body is a standalone bare url carries a shared
``RenderBlock::LinkPreview`` block (the producer emits it in ``Resolving``); once
``FeedManager::resolve_link_preview`` resolves it via ``fauna.linkpreview.resolve``
the manager projects ``PreviewState::Resolved`` onto the block, and each app
paints the ``link-preview-card`` (image / title / description / domain). A
``Resolving``/``Failed`` block paints **no** card — the inline body link already
shows. Every app reads the one shared signal (the document's ``LinkPreview``
state, priorities #1/#2), never re-fetching the preview per client.

tier_2: the real client driver renders the Feed page, but the post list is seeded
by the ``feed_inject_posts`` test-state command with a pre-resolved ``link_preview``
spec (the ``test_support`` projection that mirrors the production ``snapshot()``
resolved-preview fold). Rationale — a real resolve needs a live nest fetch of an
OpenGraph page (SSRF-guarded, tier_3), so the ``Resolved`` card paint is seeded
deterministically here, exactly as the ``quoted`` embed badge is. The real nest is
needed only for auth (the same tier_2 shape as ``test_feed_unverified_source``).

The og:image is **blocked-by-default** (render-model.md § D4, user-ratified
2026-06-27): a content-addressed nest blob, but it obeys the post's D3
remote-content reveal exactly like a body remote image, so the card paints
title/description/domain immediately and the image only after the post's
``load-remote-content-button`` is tapped. ``web`` + ``linux`` + ``windows`` adopt the
card here; the android/apple render legs are entrusted separately (they
add their marker when they adopt the card).
"""

import pytest

pytestmark = [pytest.mark.tier_2]

# Post 0 carries a bare url + a pre-resolved preview → the card paints with all
# four children. Post 1 is a plain post (no bare url) → no card. The bodies are
# explicit `[url](url)` markdown links (visible text == href), the exact "bare url"
# the shared producer turns into a `LinkPreview` block.
_POSTS = [
    {
        "post_id": "a" * 64,
        "author": "1" * 64,
        "body": "[https://example.com/article](https://example.com/article)",
        "link_previews": [
            {
                "url": "https://example.com/article",
                "title": "Example Article Title",
                "description": "A short description of the example article.",
                "image_hash": "ab" * 32,
            }
        ],
    },
    {
        "post_id": "b" * 64,
        "author": "2" * 64,
        "body": "a plain post with no link",
    },
]


# macos verified green. The earlier "feed `List` cards don't
# realize in-process" diagnosis was WRONG: `feed_inject_posts` was injecting into a
# throwaway `FfiFeedManager` (`FfiNestClient.feed_manager` builds a fresh one per
# call) that the rendered view never observed, and the detail pane was gated on a
# selected feed. Fixed app-side (shared app-level `FeedVM` + render-when-posts-
# present); the production `List` then registers `post-card[i]` fine. ios shares the
# same app-side fix (macos+ios).
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios  # verified green (`--client ios`): shared fix
@pytest.mark.tui  # M3 slice G close-out: title/description/domain + reveal-gated image, off the same shared ResolvedLinkPreview projection
@pytest.mark.feature("link-previews")
def test_link_preview_card_paints_for_a_resolved_preview(logged_in_app):
    """The ``link-preview-card`` (title / description / domain) renders on the post
    whose ``LinkPreview`` block resolved, and NOT on a plain post. The og:image is
    **blocked-by-default** (render-model.md § D4, user-ratified 2026-06-27): it obeys
    the post's D3 remote-content reveal, so the image is absent until the post's
    ``load-remote-content-button`` is tapped — exactly like a body remote image."""
    app = logged_in_app
    n = app.feed.seed_posts(_POSTS)
    assert n == len(_POSTS), (
        f"expected {len(_POSTS)} injected post-cards, got {n}; "
        f"error={app.error_text()!r}"
    )

    # Post 0: the resolved card and its text children render (scoped to the card).
    assert app.driver.is_visible("link-preview-card", scope="post-card[0]"), (
        f"the resolved preview should paint a card; error={app.error_text()!r}"
    )
    assert app.driver.is_visible("link-preview-title", scope="post-card[0]")
    assert app.driver.is_visible("link-preview-description", scope="post-card[0]")
    assert app.driver.is_visible("link-preview-domain", scope="post-card[0]")

    # The card carries the resolved og:title/description and the url's host
    # (the `url_host` shape — no scheme/path/port).
    assert (
        app.driver.get_text("link-preview-title", scope="post-card[0]")
        == "Example Article Title"
    )
    assert (
        app.driver.get_text("link-preview-domain", scope="post-card[0]")
        == "example.com"
    )

    # Blocked-by-default (D4 reveal gate): the og:image is ABSENT and the post's
    # `load-remote-content-button` is shown (the og:image drives the same blocked-
    # remote predicate as a body remote image — `has_blocked_remote_images`).
    assert app.driver.count("link-preview-image", scope="post-card[0]") == 0, (
        "the og:image must be blocked (absent) until the post is revealed"
    )
    assert app.driver.count("load-remote-content-button", scope="post-card[0]") == 1, (
        "a blocked og:image must surface the post's reveal button"
    )

    # Reveal the post → the manager projects `revealed: true` onto the og:image and
    # the card wires the image child (its hash → /api/v1/blob/<hash>); the reveal
    # button clears. Assert DOM presence, not is_visible — in tier_2 there is no real
    # nest blob, so the <img> never loads and has an empty bounding box (a real blob
    # paints it in tier_3+/prod); presence proves the reveal un-gated the image.
    app.feed.reveal_remote_content(0)
    assert app.driver.count("link-preview-image", scope="post-card[0]") == 1, (
        "revealing the post must wire the og:image child off image_hash"
    )
    assert app.driver.count("load-remote-content-button", scope="post-card[0]") == 0, (
        "the reveal button must clear once the og:image is revealed"
    )

    # Post 1 (a plain post) paints no card.
    # Negative reads below the fold are COUNTS, not visibility (e2e-conventions.md
    # convention 6): windows' is_visible is !IsOffscreen, so a badge painted on a
    # card past the first reads "not visible" and the assertion passes vacuously.
    # Every badge here is Visibility-gated -> Collapsed -> out of the UIA tree when
    # absent, and FeedPage's PostsList is deliberately non-virtualizing, so count is
    # exact either way -- and unlike is_visible_scrolled it issues no UIA scroll.
    assert app.driver.count("link-preview-card", scope="post-card[1]") == 0, (
        "a plain post (no bare url) must not paint a link-preview card"
    )


# A body with TWO standalone bare-url paragraphs — the shape the producer emits a
# `LinkPreview` block for twice, and the one no test could even seed before the
# `link_previews` list replaced the single-preview seam.
_TWO_PREVIEW_POST = [
    {
        "post_id": "c" * 64,
        "author": "3" * 64,
        "body": (
            "[https://first.example.com/a](https://first.example.com/a)\n\n"
            "[https://second.example.org/b](https://second.example.org/b)"
        ),
        "link_previews": [
            {
                "url": "https://first.example.com/a",
                "title": "First Article",
                "description": "The first description.",
            },
            {
                "url": "https://second.example.org/b",
                "title": "Second Article",
                "description": "The second description.",
            },
        ],
    },
]


@pytest.mark.web
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.tui
@pytest.mark.feature("link-previews")
def test_two_bare_urls_paint_a_card_each(logged_in_app):
    """A two-bare-url body paints TWO ``link-preview-card``s under the one bare id,
    in body order, each child resolving within its OWN card.

    This pins the id scope ui.yaml ruled 2026-08-13 (§ ``link_preview_card``;
    render-model.md § D4 raised it): the card is ``indexed: true``, addressed
    positionally as ``post-card[i]/link-preview-card[n]``. Every app already painted N
    cards off the shared ``resolved_link_previews()`` projection, but nothing asserted
    it — every fixture seeded exactly one preview, so a client that dropped the second
    card, or crossed one card's title with another's, stayed green.

    The cross-card assertion is the point: reading ``link-preview-title`` under
    ``link-preview-card[1]`` must give the SECOND card's title. Red-verified against
    tui's pre-ruling shape (children painted as flat siblings of the cards): the
    card-scoped read then resolves the EMPTY string rather than the wrong card's title
    — scope matching is prefix-based, so an element whose path is shorter than the
    scope never matches at all. Either way the containment the ruling declares is what
    this asserts.
    """
    app = logged_in_app
    n = app.feed.seed_posts(_TWO_PREVIEW_POST)
    assert n == len(_TWO_PREVIEW_POST), (
        f"expected {len(_TWO_PREVIEW_POST)} injected post-cards, got {n}; "
        f"error={app.error_text()!r}"
    )

    # Two cards, not one — the multiplicity the ruling declares.
    assert app.driver.count("link-preview-card", scope="post-card[0]") == 2, (
        "a body with two standalone bare urls must paint a card each; "
        f"error={app.error_text()!r}"
    )

    # Each card's children resolve within that card, in body order.
    assert (
        app.driver.get_text(
            "link-preview-title", scope="post-card[0]/link-preview-card[0]"
        )
        == "First Article"
    )
    assert (
        app.driver.get_text(
            "link-preview-title", scope="post-card[0]/link-preview-card[1]"
        )
        == "Second Article"
    ), "the second card's title must come from the second preview, not the first card"
    assert (
        app.driver.get_text(
            "link-preview-description", scope="post-card[0]/link-preview-card[1]"
        )
        == "The second description."
    )

    # The domain child too — each card shows its OWN host (`url_host`: no scheme/path).
    assert (
        app.driver.get_text(
            "link-preview-domain", scope="post-card[0]/link-preview-card[0]"
        )
        == "first.example.com"
    )
    assert (
        app.driver.get_text(
            "link-preview-domain", scope="post-card[0]/link-preview-card[1]"
        )
        == "second.example.org"
    )

    # Scoping to the ancestor alone still resolves the FIRST card's children —
    # prefix-based scope matching is what keeps every pre-ruling single-preview test
    # (including the one above) passing unchanged.
    assert (
        app.driver.get_text("link-preview-title", scope="post-card[0]")
        == "First Article"
    )
