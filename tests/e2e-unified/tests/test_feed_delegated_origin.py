"""Cross-app Feed — the delegated-origin badge (the D10 audit surface).

``docs/goal/behavior/atproto-pds-full.md`` § Problem 1 → D10 → *Audit* (mechanism
ratified 2026-07-29); ``docs/goal/principles.md`` § a capability grant is audited
from the user's own app; ``tests/e2e-unified/ui.yaml`` ``delegated-origin-badge``
(inside ``post-card``, IDs user-approved 2026-07-31).

A post an **external app** authored as the account — signed by the account's
delegated authoring sub-key rather than its identity key, with the
identity-signed ``DeviceAuthorization`` riding in the wire's ``signer_auth`` —
renders a badge saying so. This is what makes D10's grant *audited* rather than
merely revocable: the signed bytes **are** the log, read client-side, so a user
scrolling their own feed can tell which posts they did not personally write.

The badge fires **iff** ``AuthoringOriginStatus::Delegated``. Every app reads
the one shared signal ``fauna_feed::PostSummary::authoring_origin`` (priorities
#1/#2), never re-deriving origin per client — the same discipline the sibling
``unverified-source-badge`` follows, one field over.

**The two negative arms carry the security content.** ``Direct`` (the account
signed it itself) is the overwhelmingly common case: a badge on every post would
say nothing. ``Unknown`` deliberately covers *both* the undecoded nest-index list
card *and* the verification-**failed** case — and the latter is the one that
matters, because an unverified wire's ``signer_auth`` cert is precisely the thing
nothing authenticated. Badging it would let a forged wire describe its own
origin, which is the exact inversion of an audit surface.

tier_2: the real client driver renders the Feed page, but its post list is seeded
by the ``feed_inject_posts`` test-state command (the
``FeedManager::set_feed_snapshot_for_test`` seam) rather than a live
``fauna.feed.local.posts`` query — the same shape, and for the same reason, as
``test_feed_unverified_source.py``. A real nest query cannot produce a delegated
post on demand: doing so end-to-end needs a provisioned D10 delegation plus an
external ATProto app writing through the full-PDS bridge, which is the tier_3
``test_atproto_external_write.py`` journey. That journey pins the *production*
half — that a real external-app post decodes as ``Delegated`` through the shared
read face — and this file pins the *render* half on top of it.

tui is the lead app for this surface, and the verify-back conditions require that the six-app trickle-down **not
precede** the tui marker. Apple joined 2026-07-31 (macos + ios, shared FaunaKit);
linux and web joined 2026-08-15. Windows is the one app
still owing the render.

android:``DelegatedOriginBadge.kt``
has the exact ``delegated-origin-badge`` testTag, wired in ``FeedScreen``'s
``post-card`` and its ``quoted-post`` embed. android's ``feed_inject_posts``
test-agent seam did not exist before this pass (see
``test_feed_unverified_source.py``'s android note); execution still awaits a
device — the marker was previously withheld for that reason alone, which
undersold the actual gap.
"""

import pytest

pytestmark = [pytest.mark.tier_2]

# Three posts differing only in `authoring_origin`, so the asserts isolate the
# iff-Delegated gate. All three are `Verified`, so no unverified-source badge
# competes for attention and the signal under test is the only variable.
_POSTS = [
    {
        "post_id": "a" * 64,
        "author": "1" * 64,
        "body": "written by an external app",
        "verification": "Verified",
        "authoring_origin": "Delegated",
    },
    {
        "post_id": "b" * 64,
        "author": "2" * 64,
        "body": "written by the account itself",
        "verification": "Verified",
        "authoring_origin": "Direct",
    },
    {
        "post_id": "c" * 64,
        "author": "3" * 64,
        "body": "a not-yet-decoded post",
        "verification": "Unchecked",
        "authoring_origin": "Unknown",
    },
]


@pytest.mark.tui  # lead app for the D10 audit surface, 2026-07-31
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.linux  # trickle-down leg 2, 2026-08-15
@pytest.mark.web  # trickle-down leg 3, 2026-08-15
@pytest.mark.windows  # trickle-down leg 4 and last, 2026-08-24
@pytest.mark.android  # marked 2026-09-13
@pytest.mark.feature("post-badges")
def test_delegated_origin_badge_shows_only_on_a_delegated_post(logged_in_app):
    """The ``delegated-origin-badge`` renders on the externally-authored post's
    card and on neither the self-authored nor the undecoded one."""
    app = logged_in_app
    n = app.feed.seed_posts(_POSTS)
    assert n == len(_POSTS), (
        f"expected {len(_POSTS)} injected post-cards, got {n}; "
        f"error={app.error_text()!r}"
    )

    # Scoped per-card (never global-count-then-slice): posts[0]=Delegated →
    # badge; posts[1]=Direct and posts[2]=Unknown → no badge.
    assert app.driver.is_visible("delegated-origin-badge", scope="post-card[0]"), (
        "a post an external app wrote as the account must be identifiable as one "
        f"— that is the whole of D10's audit promise; error={app.error_text()!r}"
    )
    # Negative reads below the fold are COUNTS, not visibility (e2e-conventions.md
    # convention 6): windows' is_visible is !IsOffscreen, so a badge painted on a
    # card past the first reads "not visible" and the assertion passes vacuously.
    # Every badge here is Visibility-gated -> Collapsed -> out of the UIA tree when
    # absent, and FeedPage's PostsList is deliberately non-virtualizing, so count is
    # exact either way -- and unlike is_visible_scrolled it issues no UIA scroll.
    assert app.driver.count("delegated-origin-badge", scope="post-card[1]") == 0, (
        "a post the account signed itself must NOT be badged"
    )
    assert app.driver.count("delegated-origin-badge", scope="post-card[2]") == 0, (
        "an undecoded card knows nothing about origin and must NOT be badged"
    )


# The unverified arm, stated as its own case because it is the one an attacker
# reaches for: a wire whose signature did NOT verify carries a `signer_auth` cert
# nothing authenticated. The manager reports `Unknown` for it (never a cert claim
# read off unverified bytes), so the card shows the *unverified-source* badge and
# NOT the delegated-origin one. A client that read origin off a failed envelope
# would let a forgery paint itself as "merely delegated".
_FORGED_POST = [
    {
        "post_id": "d" * 64,
        "author": "4" * 64,
        "body": "a post whose envelope did not verify",
        "verification": "Failed",
        "authoring_origin": "Unknown",
    },
]


@pytest.mark.tui
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.windows
@pytest.mark.android  # marked 2026-09-13
@pytest.mark.feature("post-badges")
def test_a_post_that_failed_verification_is_never_badged_as_delegated(logged_in_app):
    """A ``Failed`` post shows the unverified-source badge and **no** origin
    badge — origin is only ever reported for bytes that actually verified."""
    app = logged_in_app
    n = app.feed.seed_posts(_FORGED_POST)
    assert n == len(_FORGED_POST), (
        f"expected {len(_FORGED_POST)} injected post-card, got {n}; "
        f"error={app.error_text()!r}"
    )

    # The control: this card DOES render a badge, so a bare "no delegated badge"
    # assert below cannot pass vacuously on a card that painted nothing at all.
    assert app.driver.is_visible("unverified-source-badge", scope="post-card[0]"), (
        f"the Failed post should show the unverified-source badge; "
        f"error={app.error_text()!r}"
    )
    assert app.driver.is_absent("delegated-origin-badge", scope="post-card[0]"), (
        "an unverified envelope's origin claim is unauthenticated and must never "
        "be rendered as an audit answer"
    )


# The quoted-*embed* badge. Two posts whose FOCAL origin is `Direct` (so neither
# card shows a focal badge) but which quote a post differing only in
# `authoring_origin`: the embed badge keys off the *quoted* post's own origin,
# folded into `RenderBlock::QuotedPost::authoring_origin` by the manager. The
# `quoted` spec pre-folds the embed via the production `build_post_document` fold
# (test_support), so no live resolve runs.
_QUOTING_POSTS = [
    {
        "post_id": "e" * 64,
        "author": "5" * 64,
        "body": "quoting an external-app post",
        "verification": "Verified",
        "authoring_origin": "Direct",
        "quoted": {
            "post_id": "f" * 64,
            "author": "6" * 64,
            "body": "the externally-authored quoted body",
            "verification": "Verified",
            "authoring_origin": "Delegated",
        },
    },
    {
        "post_id": "0" * 64,
        "author": "7" * 64,
        "body": "quoting a self-authored post",
        "verification": "Verified",
        "authoring_origin": "Direct",
        "quoted": {
            "post_id": "1" * 64,
            "author": "8" * 64,
            "body": "the self-authored quoted body",
            "verification": "Verified",
            "authoring_origin": "Direct",
        },
    },
]


@pytest.mark.tui
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.windows
@pytest.mark.android  # marked 2026-09-13
@pytest.mark.feature("post-badges")
def test_quoted_embed_badge_shows_only_on_a_delegated_quote(logged_in_app):
    """The quoted-post embed paints the ``delegated-origin-badge`` **iff** the
    *quoted* post was externally authored — independent of the focal post's own
    origin (both focal posts here are ``Direct``)."""
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

    # Scoped to the embed (`post-card[i]/quoted-post`). Neither focal card carries
    # a badge (both Direct), so this isolates the quoted-embed signal.
    assert app.driver.is_visible(
        "delegated-origin-badge", scope="post-card[0]/quoted-post"
    ), (
        "the quoted embed of an externally-authored quote should show the badge; "
        f"error={app.error_text()!r}"
    )
    assert app.driver.count(
        "delegated-origin-badge", scope="post-card[1]/quoted-post"
    ) == 0, "the quoted embed of a self-authored quote must NOT show the badge"

    # ⚠ `scope=` is SUBTREE-INCLUSIVE: `post-card[0]` matches the embed's badge
    # too, since the embed lives inside that card. So there is no way to spell
    # "the focal card itself is unbadged" here, and asserting
    # `not is_visible(..., scope="post-card[0]")` would be asserting something
    # false about the harness rather than about the product. The property that
    # the focal card emits no badge of its own IS pinned, at the level where the
    # element paths are visible: the tui unit test
    # `a_delegated_quote_badges_the_embed_not_the_focal_card` asserts exactly one
    # badge exists and that its path runs through `quoted-post`.
    #
    # What this level CAN say, and does: post-card[1] is Direct throughout —
    # focal and quote — so its whole subtree carries no badge at all. That is
    # what makes the positive above a signal rather than an artifact of the
    # badge appearing on every card.
    assert app.driver.count("delegated-origin-badge", scope="post-card[1]") == 0, (
        "a card that is self-authored focal AND self-authored quote must carry "
        "no origin badge anywhere in its subtree"
    )
