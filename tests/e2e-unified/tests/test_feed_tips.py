"""Cross-app Feed — the post tip surface (`post-tip-total` / `post-tip-count`
/ `post-tip-list-button` / `post-tip-list` / `post-tip-item`).

``docs/goal/behavior/monetization.md`` § Tips (ratified 2026-07-22) + § Zap
receipts (trust ratified 2026-07-29) + § Implementation status today (the Tips
bullet); ``tests/e2e-unified/ui.yaml`` ``post-card`` + the ``post-tip-list``
component (IDs user-approved 2026-08-11).

A tip is a per-post payment that **unlocks nothing**: it names *(payee, post)*
and its whole ratified consequence is attribution and display. This file pins
that consequence actually reaching a screen — the totals existed on the nest for
weeks with no client able to render them, because the predecessor read was keyed
by *Nostr event id*, which a client holding a post never has.

Every app reads one shared projection, ``fauna_feed::PostSummary::tips``,
filled by ``FeedManager::resolve_post_tips`` (priorities #1/#2). No app calls
``TipsClient`` itself and none re-derives the unit, the singular, or the
"is there an amount" question — so each remaining app's leg is a render.

**The three cases below are not variations on one assertion.** They are the
three states the wire can actually produce, and the middle one is why
``post-tip-total`` and ``post-tip-count`` are separate elements at all.

tier_2: the real client driver renders the Feed page, but the post list is seeded
through the ``feed_inject_posts`` seam (``TestPostSpec.tips``) rather than a live
``fauna.tips.list`` query — the same shape, and for the same reason, as
``unlock_offer`` in ``test_sell_post.py``. Driving this end-to-end from a real
nest needs an *ingested, believed* payment receipt: a payee-designated signer
pubkey, a post materialized to an outbound mechanism row, and a crafted receipt
that survives the ingest gate. The nest half of that is already pinned tier_3 by
``bins/fauna-nest/tests/conformance_tips.rs``; this file pins the render half on
top of it.

``tui`` was first (the lead app for this surface, § Areas — every new UI
feature lands on tui first); ``linux``, ``web`` and ``android`` joined
2026-08-13, ``macos``/``ios`` joined 2026-08-24, and ``windows`` — the last
app — joined 2026-08-25. All 7 apps now carry this leg.
"""

import pytest

pytestmark = [pytest.mark.tier_2]

# Three posts, one per wire-producible state. Ordered so `post-card[i]` is
# stable: the client never re-sorts an injected list.
_POSTS = [
    # 0 — the ordinary tipped post: an amount and a count.
    {
        "post_id": "a" * 64,
        "author": "1" * 64,
        "body": "a post somebody thought was worth paying for",
        "tips": {
            "total_msats": 21_000,
            "tip_count": 2,
            "senders": [
                {
                    "sender": "b" * 64,
                    "amount_msats": 20_000,
                    "mechanism": "nostr_zap",
                    "received_at": 1_700_000_000,
                },
                # An outside tipper: no local actor, only the id their mechanism
                # published. Still counts, still displays — unattributed.
                {
                    "sender_ref": "npub1outsider",
                    "amount_msats": 1_000,
                    "mechanism": "nostr_zap",
                    "received_at": 1_699_999_000,
                },
            ],
            "has_more": False,
        },
    },
    # 1 — tipped, but nothing summable: every receipt carried an unparseable
    # invoice. `tip_count` deliberately exceeds the summable set here.
    {
        "post_id": "c" * 64,
        "author": "2" * 64,
        "body": "tipped by someone whose receipt carried no readable amount",
        "tips": {
            "total_msats": 0,
            "tip_count": 3,
            "senders": [
                {
                    "sender_ref": "npub1anonymous",
                    "mechanism": "nostr_zap",
                    "received_at": 1_699_998_000,
                }
            ],
            "has_more": False,
        },
    },
    # 2 — untipped. The tips fetch folds any error (`unknown_kind`) and a nest
    # with no tip mechanism compiled in to empty: one empty surface.
    {
        "post_id": "d" * 64,
        "author": "3" * 64,
        "body": "nobody tipped this one",
        "tips": {"total_msats": 0, "tip_count": 0, "senders": [], "has_more": False},
    },
]


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.android  # lead app for the tip surface, 2026-08-11
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.feature("paid-posts-and-tips")
def test_a_tipped_post_shows_its_total_and_count(logged_in_app):
    """A post that received a believed tip renders the amount and the count; an
    untipped one renders neither."""
    app = logged_in_app
    n = app.feed.seed_posts(_POSTS)
    assert n == len(_POSTS), (
        f"expected {len(_POSTS)} injected post-cards, got {n}; "
        f"error={app.error_text()!r}"
    )

    assert app.feed.has_tip_total(0), (
        "a tip's ratified consequence is attribution and display — a post with "
        f"21 sats of tips must show them; error={app.error_text()!r}"
    )
    total = app.feed.tip_total_text(0)
    assert "21" in total, (
        f"the total renders in SATS (21_000 msats = 21 sats), got {total!r} — a "
        "client showing the raw msat wire value is reading the wrong unit"
    )
    assert "2" in app.feed.tip_count_text(0), (
        f"2 tips on the post; got {app.feed.tip_count_text(0)!r}"
    )

    # The untipped card: no surface at all. This is also the render a failed
    # tips fetch and a payments-excised build produce, deliberately.
    # Negative reads below the fold are COUNTS, not visibility (e2e-conventions.md
    # convention 6): windows' is_visible is !IsOffscreen, so a badge painted on a
    # card past the first reads "not visible" and the assertion passes vacuously.
    # Every badge here is Visibility-gated -> Collapsed -> out of the UIA tree when
    # absent, and FeedPage's PostsList is deliberately non-virtualizing, so count is
    # exact either way -- and unlike is_visible_scrolled it issues no UIA scroll.
    assert app.driver.count("post-tip-total", scope="post-card[2]") == 0, (
        "an untipped post must show no amount"
    )
    assert app.driver.count("post-tip-count", scope="post-card[2]") == 0, (
        "an untipped post must show no count"
    )
    assert app.driver.count("post-tip-list-button", scope="post-card[2]") == 0, (
        "an untipped post must offer no attribution window"
    )


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.feature("paid-posts-and-tips")
def test_tips_with_no_readable_amount_show_the_count_and_no_total(logged_in_app):
    """**The state the two separate ids exist for.** ``tip_count`` counts every
    tip; ``total_msats`` sums only those whose receipt reported an amount. A post
    where none did has real tips and no amount — so the count renders and the
    total must not, because "0 sats" would tell the reader nobody paid
    (§ Tips — a missing amount is a real state, never coerced to 0)."""
    app = logged_in_app
    n = app.feed.seed_posts(_POSTS)
    assert n == len(_POSTS), f"expected {len(_POSTS)} cards, got {n}"

    assert not app.feed.has_tip_total(1), (
        "no receipt on this post reported an amount, so there is nothing "
        "truthful to show — rendering '0 sats' would say nobody paid"
    )
    count = app.feed.tip_count_text(1)
    assert "3" in count, (
        f"the tips are real and must still be reported; got {count!r}. A client "
        "gating the count on the total would make these three tips invisible"
    )
    assert app.driver.is_visible("post-tip-list-button", scope="post-card[1]"), (
        "a post with tips always has something to attribute, amount or not"
    )


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.feature("paid-posts-and-tips")
def test_the_attribution_window_names_every_tipper_it_was_given(logged_in_app):
    """``post-tip-list-button`` opens ``post-tip-list``; each ``post-tip-item``
    names a tipper and an amount, or says the amount was not reported.

    **Unfiltered by design**: authenticity is settled at ingest and never at read
    (§ Zap receipts — *at ingest, never at read*), so a client renders every row
    the nest sends. A client-side trust check here would re-open exactly the
    per-reader re-checking that discipline exists to prevent."""
    app = logged_in_app
    n = app.feed.seed_posts(_POSTS)
    assert n == len(_POSTS), f"expected {len(_POSTS)} cards, got {n}"

    assert app.driver.is_absent("post-tip-list"), (
        "the attribution window is closed until it is asked for"
    )
    app.feed.open_tip_list(0)
    assert app.driver.is_visible("post-tip-list"), (
        f"post-tip-list-button must open the window; error={app.error_text()!r}"
    )
    assert app.feed.tip_item_count() == 2, (
        f"both tips the nest sent must be listed, got "
        f"{app.feed.tip_item_count()}; error={app.error_text()!r}"
    )

    rows = [app.feed.tip_item_text(i) for i in range(app.feed.tip_item_count())]
    assert any("20" in r for r in rows), f"the 20-sat tip should be named: {rows}"
    # The outside tipper resolved to no local actor. Their mechanism-native id is
    # what a client shows rather than showing nothing — an outside tip counts.
    assert any("npub1outsider" in r for r in rows), (
        f"an unresolvable tipper must still be attributed by the id their "
        f"mechanism published, not dropped: {rows}"
    )
