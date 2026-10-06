"""tier_3: a link the nest cannot preview stays a plain link in the text, with
no card.

``docs/goal/architecture/render-model.md`` § D4 — Link previews: the body keeps
its inline link, and a ``LinkPreview`` block beside it resolves through the
nest to ``Resolved`` (a card) or ``Failed`` (nothing — the link alone). The
sibling ``test_feed_link_preview.py`` seeds an already-resolved preview; this
one posts a real link through the composer and lets the real nest refuse it.

The link points at a private address, which the nest's fetcher refuses before
it connects (``tests/api/test_link_preview.py::test_private_ip_url_fails`` —
the SSRF guard, so there is no loopback carve-out to trip over).

**Why the wait is on the preview's state, not on the card's absence.** A card
is absent while the preview is still resolving too, so asserting absence right
after posting passes whether the resolve failed, succeeded late, or never ran.
The app's state dump carries each post's previews with their state
(``data.feed.posts[].link_previews`` — the shared
``RenderDocument::link_previews``), so the test waits until the preview
reads ``failed``: the snapshot that says so is the one the card is painted
from, and only then is "no card" a statement about a failed preview.
"""

import uuid

import pytest

from helpers.waiting import wait_until

pytestmark = [pytest.mark.tier_3]

# A private address: the nest's link-preview fetcher refuses it outright.
UNPREVIEWABLE_URL = "http://10.0.0.5/"

# The nest refuses a private address without connecting, so the resolve settles
# fast; this is a ceiling for a broken surface, never a subject (convention 14).
PREVIEW_SETTLE_BUDGET_S = 60.0


def _unique(prefix: str) -> str:
    return f"{prefix}-{uuid.uuid4().hex[:8]}"


@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.tui
@pytest.mark.windows
@pytest.mark.feature("link-previews")
def test_a_link_the_nest_cannot_preview_stays_a_plain_link(logged_in_app):
    app = logged_in_app
    feed = app.feed
    d = app.driver
    marker = _unique("unpreviewable-link")
    # A standalone `[url](url)` paragraph is the bare link the shared producer
    # gives a LinkPreview block (a bare `http://…` does not autolink).
    # The card paints the link, never its `[url](url)` source, so the landing
    # read waits on the marker (`create_post`'s `expect_top`).
    feed.create_post(
        text=f"{marker}\n\n[{UNPREVIEWABLE_URL}]({UNPREVIEWABLE_URL})",
        expect_top=marker,
    )
    row = feed.wait_for_post_state_by_text(marker)
    assert row is not None and row.get("post_id"), (
        f"the post {marker!r} should reach the feed; error={app.error_text()!r}"
    )
    post_id = row["post_id"]
    assert "link_previews" in row, (
        "this app's feed state does not publish data.feed.posts[].link_previews "
        "— the settled-preview read this test waits on (convention 11: an app "
        f"without the leg refuses, never passes on an absence); row={row!r}"
    )

    def previews():
        return (feed.post_state_by_id(post_id) or {}).get("link_previews")

    wait_until(
        lambda: previews() == [{"url": UNPREVIEWABLE_URL, "state": "failed"}],
        PREVIEW_SETTLE_BUDGET_S,
        diagnose=lambda: f"link_previews={previews()!r} error={app.error_text()!r}",
    )

    at = feed.post_index_by_id(post_id)
    scope = f"post-card[{at}]"
    assert d.count("link-preview-card", scope=scope) == 0, (
        "a link the nest could not preview must paint no link-preview-card"
    )
    body = d.get_text("feed-post-text", scope=scope) or ""
    assert UNPREVIEWABLE_URL in body, (
        f"the link must stay in the post's text; feed-post-text={body!r}"
    )
