"""Cross-app Feed — the unverified-source badge.

``docs/goal/architecture/security.md`` § App display of unverified content;
``tests/e2e-unified/ui.yaml`` ``unverified-source-badge`` (inside ``post-card``).

A post this client could **not** cryptographically verify (its own envelope
verification returned ``Failed``) renders a muted "unverified source" badge while
still showing the content (the DKIM-fail analogue); a ``Verified`` or the default
``Unchecked`` post renders **no** badge. The badge fires **iff**
``VerificationStatus::Failed`` — every app reads the one shared signal
``fauna_feed::PostSummary::verification`` (priorities #1/#2), never re-deriving
validity per client.

tier_2: the real client driver renders the Feed page, but its post list is seeded
by the ``feed_inject_posts`` test-state command (the
``FeedManager::set_feed_snapshot_for_test`` seam) rather than a live
``fauna.feed.local.posts`` query. Rationale — a real signed post the nest serves
is only ever ``Unchecked``/``Verified``: the manager flips ``verification`` to
``Failed`` only where THIS client's own envelope verification fails, which a real
nest query can't produce on demand. So the badge's ``Failed`` arm is unreachable
without injection; the real nest is needed only for auth (the same tier_2 shape as
``sync_inject_locations``). Per-function ``web``/``linux``/``windows`` markers —
windows seeds via the same ``feed_inject_posts`` command
(``FfiFeedManager.inject_posts_for_test``); the apple (macos/ios) render leg is
entrusted to its own machine.

windows note (tracked internally): both legs are e2e-verified GREEN. Two windows-only
issues found during enablement are fixed: (1) the FlaUI bridge's scoped
``FindAllDescendants`` escaped the ``post-card[i]`` subtree (a negative scoped
``is_visible`` found another card's badge) — re-imposed client-side in
``flaui-bridge/ElementFinder.cs``; (2) the quoted-post embed Border was pruned from
the UIA tree for lacking an ``AutomationProperties.Name`` (NOT an FFI failure — the
``RenderBlock::QuotedPost`` document crosses fine, locked by the ``FeedDocumentFfiTests``
dotnet round-trip) — named in ``FeedPage.xaml``.

android:``UnverifiedSourceBadge.kt``
has the exact ``unverified-source-badge`` testTag, wired in both ``FeedScreen``'s
``post-card`` and its quoted-embed rendering (``PostDetailScreen`` too). The
``feed_inject_posts`` test-agent command itself did not exist on android until this
pass (``TestAgent.kt``'s command table had no entry) — added mirroring linux
``handle_feed_inject_posts`` / windows ``FeedCommands.InjectPosts``, forwarding the
raw ``posts`` JSON to the already-generated ``FfiFeedManager.injectPostsForTest``
UniFFI binding. Untested on this dev VM for lack of a device — the standing
android-e2e gap every app-gated row carries.
"""

import pytest

pytestmark = [pytest.mark.tier_2]

# Three posts differing only in `verification`, so the asserts isolate the
# iff-Failed gate. `post_id`/`author` are 64-hex (32-byte) ids; `body` makes each
# card render real content (the badge is independent of the body, but a realistic
# card exercises the shared document walker too).
_POSTS = [
    {
        "post_id": "a" * 64,
        "author": "1" * 64,
        "body": "from an unverified source",
        "verification": "Failed",
    },
    {
        "post_id": "b" * 64,
        "author": "2" * 64,
        "body": "a verified post",
        "verification": "Verified",
    },
    {
        "post_id": "c" * 64,
        "author": "3" * 64,
        "body": "a not-yet-decoded post",
        "verification": "Unchecked",
    },
]


@pytest.mark.web
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos  # verified green: the apple feed render fix below
@pytest.mark.ios  # verified green (`--client ios`): shared fix
@pytest.mark.tui  # M3 slice G close-out: badge iff PostSummary.verification == Failed
@pytest.mark.android  # marked 2026-09-13 — UnverifiedSourceBadge.kt exact ID match
# The apple feed-card non-render was NOT a lazy-`List` gap: `feed_inject_posts` was
# injecting into a *throwaway* manager (`FfiNestClient.feed_manager` builds a fresh
# one per call) that the view never observed, and the detail pane was gated on a
# selected feed. Fixed app-side (shared macos+ios): the handler now seeds the shared
# app-level `FeedVM` the view renders off, and the pane renders when posts are
# present (cross-app contract).
@pytest.mark.feature("post-badges")
def test_unverified_source_badge_shows_only_on_failed(logged_in_app):
    """The ``unverified-source-badge`` renders on the ``Failed`` post's card and on
    neither the ``Verified`` nor the ``Unchecked`` one (badge **iff** Failed)."""
    app = logged_in_app
    n = app.feed.seed_posts(_POSTS)
    assert n == len(_POSTS), (
        f"expected {len(_POSTS)} injected post-cards, got {n}; "
        f"error={app.error_text()!r}"
    )

    # Scoped per-card (never global-count-then-slice): posts[0]=Failed → badge;
    # posts[1]=Verified and posts[2]=Unchecked → no badge.
    assert app.driver.is_visible("unverified-source-badge", scope="post-card[0]"), (
        "Failed post should show the unverified-source badge; "
        f"error={app.error_text()!r}"
    )
    # Negative reads below the fold are COUNTS, not visibility (e2e-conventions.md
    # convention 6): windows' is_visible is !IsOffscreen, so a badge painted on a
    # card past the first reads "not visible" and the assertion passes vacuously.
    # Every badge here is Visibility-gated -> Collapsed -> out of the UIA tree when
    # absent, and FeedPage's PostsList is deliberately non-virtualizing, so count is
    # exact either way -- and unlike is_visible_scrolled it issues no UIA scroll.
    assert app.driver.count("unverified-source-badge", scope="post-card[1]") == 0, (
        "Verified post must NOT show the unverified-source badge"
    )
    assert app.driver.count("unverified-source-badge", scope="post-card[2]") == 0, (
        "Unchecked post must NOT show the unverified-source badge"
    )


# Slice 2b — the quoted-*embed* badge. Two posts whose FOCAL status is `Unchecked`
# (so neither card shows a focal badge) but which quote a post differing only in
# `verification`: the embed badge keys off the *quoted* post's status, folded into
# `RenderBlock::QuotedPost::verification` by the manager (security.md § Client
# display of unverified content). The `quoted` spec pre-folds the embed via the
# production `build_post_document` fold (test_support), so no live resolve runs.
_QUOTING_POSTS = [
    {
        "post_id": "d" * 64,
        "author": "4" * 64,
        "body": "quoting an unverified post",
        "verification": "Unchecked",
        "quoted": {
            "post_id": "e" * 64,
            "author": "5" * 64,
            "body": "the unverified quoted body",
            "verification": "Failed",
        },
    },
    {
        "post_id": "f" * 64,
        "author": "6" * 64,
        "body": "quoting a verified post",
        "verification": "Unchecked",
        "quoted": {
            "post_id": "0" * 64,
            "author": "7" * 64,
            "body": "the verified quoted body",
            "verification": "Verified",
        },
    },
]


@pytest.mark.web
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.tui  # M3 slice G close-out: the quoted-post embed badge keys off QuotedPostEmbed::verification
@pytest.mark.android  # marked 2026-09-13
# macos confirmed GREEN 2026-06-28: the in-process driver now
# does real parent/child subtree scope modelling (apple-e2e-automation.md
# § limitation (a) — RESOLVED). Scoped containers push their `(id, index)` down the
# SwiftUI environment via `.automationScope` (`post-card` per ForEach row,
# `quoted-post` in the shared `QuotedPostCard`); each registered leaf captures its
# ancestor scope path, and `AutomationRegistry.resolved*` filter by path prefix — so
# the NEGATIVE assert `not is_visible("unverified-source-badge",
# scope="post-card[1]/quoted-post")` now correctly tells post-card[0]'s quoted badge
# (a Failed quote) from post-card[1]'s (a Verified quote, no badge). ios confirmed
# GREEN 2026-06-28 (`--client ios`, cold 697s) — the shared FaunaKit
# `.automationScope` + `resolved*` path lives in `QuotedPostCard`/`AutomationRegistry`,
# and the iOS `FeedListView` ForEach pushes `post-card`, so the leaf path-prefix filter
# resolves on iOS exactly as on macos.
@pytest.mark.feature("post-badges")
def test_quoted_embed_badge_shows_only_on_failed_quote(logged_in_app):
    """The quoted-post embed paints the ``unverified-source-badge`` **iff** the
    *quoted* post failed THIS client's verification — independent of the focal
    post's status (both focal posts here are ``Unchecked``)."""
    app = logged_in_app
    n = app.feed.seed_posts(_QUOTING_POSTS)
    assert n == len(_QUOTING_POSTS), (
        f"expected {len(_QUOTING_POSTS)} injected post-cards, got {n}; "
        f"error={app.error_text()!r}"
    )

    # Both quoting cards render their folded `RenderBlock::QuotedPost` embed card.
    assert app.driver.is_visible("quoted-post", scope="post-card[0]"), (
        f"post 0 should render its quoted-post embed; error={app.error_text()!r}"
    )
    assert app.driver.is_visible("quoted-post", scope="post-card[1]"), (
        "post 1 should render its quoted-post embed"
    )

    # Scoped to the embed (`post-card[i]/quoted-post`): the Failed quote shows the
    # badge, the Verified quote does not (badge iff Failed). Neither focal card
    # carries a badge (both Unchecked), so this isolates the quoted-embed signal.
    assert app.driver.is_visible(
        "unverified-source-badge", scope="post-card[0]/quoted-post"
    ), (
        "the quoted embed of a Failed-verification quote should show the badge; "
        f"error={app.error_text()!r}"
    )
    assert app.driver.count(
        "unverified-source-badge", scope="post-card[1]/quoted-post"
    ) == 0, "the quoted embed of a Verified quote must NOT show the badge"
