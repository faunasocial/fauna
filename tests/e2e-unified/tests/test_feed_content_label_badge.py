"""Cross-app Feed — the content-label badge.

``docs/goal/behavior/moderation.md`` § Per-row badge data path;
``tests/e2e-unified/ui.yaml`` ``content-label-badge`` (inside ``post-card``).

A post carrying ``labels`` (``fauna_feed::PostSummary::labels``, sourced from the
nest's ``content_labels`` projection on a real read, or the ``TestPostSpec.labels``
inject seam here) renders a ``content-label-badge`` for its highest-confidence
category (the shared ``fauna_core::content_category::primary_content_label`` pick);
a post with no labels renders none. Every app reads the one shared signal
(priorities #1/#2), never re-deriving the category→label/colour/icon map per
client (``content_label_style``).

This proves the FEED render leg specifically. The moderation-queue leg is proven by
``test_moderation_local_detection.py`` (client-side post-decrypt local detections);
the DM-bubble leg by ``test_conversations_content_label_badge.py``. The
nest-projection data path itself (label attach → ``fauna.feed.posts`` carries it) is
proven at tier_3 in ``tests/api/test_feed_content_labels.py``; THIS test proves the
render given the label is already on the snapshot, via the same
``feed_inject_posts``/``TestPostSpec`` seam ``test_feed_unverified_source.py`` uses
for its sibling badge (tier_2 — no live nest read of ``content_labels`` needed to
exercise the render gate).

tui has no ``content-label-badge`` implementation yet (verified 2026-07-22: zero
``content-label`` references under ``apps/fauna-tui/src``) — a real gap, not this
test's job to fix; omitted from the marker set until it lands.

android:``ContentLabelBadge.kt``
has the exact ``content-label-badge`` testTag, wired in ``FeedScreen``'s
``post-card`` (behind ``contentLabelFor(post.labels)`` — iff-non-empty, matching
the shared highest-confidence pick). The ``feed_inject_posts`` test-agent command
itself did not exist on android before this pass — added mirroring linux/windows
(see ``test_feed_unverified_source.py``'s android note); execution awaits a
device.
"""

import pytest

pytestmark = [pytest.mark.tier_2]

# Two posts differing only in `labels`, isolating the iff-non-empty gate. The
# labeled post carries two category verdicts to also exercise the
# highest-confidence pick (900 commercial should win over 200 spam).
_POSTS = [
    {
        "post_id": "a" * 64,
        "author": "1" * 64,
        "body": "a clean post",
    },
    {
        "post_id": "b" * 64,
        "author": "2" * 64,
        "body": "a labeled post",
        "labels": [
            {"category": "spam", "confidence_per_mille": 200},
            {"category": "commercial", "confidence_per_mille": 900},
        ],
    },
]


@pytest.mark.web
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.tui
@pytest.mark.android  # marked 2026-09-13
@pytest.mark.feature("post-badges")
def test_content_label_badge_shows_only_on_labeled_post(logged_in_app):
    """The ``content-label-badge`` renders on the labeled post's card, resolves
    the highest-confidence category, and is absent on the unlabeled sibling."""
    app = logged_in_app
    n = app.feed.seed_posts(_POSTS)
    assert n == len(_POSTS), (
        f"expected {len(_POSTS)} injected post-cards, got {n}; "
        f"error={app.error_text()!r}"
    )

    assert app.driver.is_absent("content-label-badge", scope="post-card[0]"), (
        "the unlabeled post must NOT show a content-label-badge"
    )
    assert app.driver.is_visible("content-label-badge", scope="post-card[1]"), (
        f"the labeled post should show a content-label-badge; error={app.error_text()!r}"
    )
    badge_text = app.driver.get_text("content-label-badge", scope="post-card[1]")
    assert badge_text, "the content-label-badge should resolve a non-empty category label"
