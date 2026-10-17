"""tier_3 e2e for who is offered what around a restricted or sold post —
`monetization.md` § Per-post pay-to-unlock ("a sold post's tier never appears
among the offers, the author's own tier list, or the compose audience picker")
and `restricted-posts.md` § Encryption at rest (a non-subscriber sees the teaser
and a badge naming the tier, never the post).

`test_sell_post.py` pins the sale itself, the buyer's purchase legs and the §1
exclusion; `test_gated_post_compose.py` pins an entitled reader's unlock. This
module pins the other side of each: the reader who is NOT entitled, the
subscriber a sale deliberately leaves out, the buyer the sale makes a follower,
and the three places a sold post's tier must never be offered.

Every absence below is read behind a positive barrier from the same render
(convention 14): an ordinary tier IS offered before the sold one is asserted
absent from the same list; a foreign sold post's price IS resolved before the
author's own card is asserted to carry none. The sold-post price, link and buy
controls are inside the `payments` plane (`dynamic-features.md` § Platform-family
surface excision → *The price-and-route class*); the e2e harness builds every
app's default flavor, which carries the plane, so no flavor gate applies.
"""

import time
import uuid

import pytest
from nacl.signing import SigningKey

from actions.api_actor import ApiActor
from clients._ws_rpc_core import RpcCallError
from common.auth import create_actor_and_register
from drivers.http_bridge import SelectOptionNotOffered
from helpers.e2e_session import login_as

pytestmark = [pytest.mark.tier_3]

PRICE = "$3"
UNLOCK_TIER_PREFIX = "post-unlock-"
FOLLOWERS_TIER = "followers"
# Convention-14 named budget for an author-side grant (`test_subscriptions.py`).
AUTHOR_GRANT_BUDGET_S = 90


def _unique(prefix: str) -> str:
    return f"{prefix}-{uuid.uuid4().hex[:8]}"


def _as_signing_key(raw) -> SigningKey:
    return raw if isinstance(raw, SigningKey) else SigningKey(bytes.fromhex(raw))


@pytest.fixture
def offers_nest(request, nest_mode, tmp_path_factory):
    """A dedicated fresh nest — tiers and sales cannot be un-minted."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "paid-offers-nest")
    yield nest
    cleanup()


@pytest.fixture
def offers_spa_url(static_dir, offers_nest):
    from conftest import _serve_spa_proxy
    url, server = _serve_spa_proxy(static_dir, offers_nest["url"])
    yield url
    server.shutdown()


def _register(nest) -> dict:
    return create_actor_and_register(
        nest["port"], base_url=nest["url"],
        admin_signing_key=_as_signing_key(nest["admin"]["signing_key"]),
    )


def _api(nest, user) -> ApiActor:
    return ApiActor(nest["url"], user["token"], user["actor_id_hex"], bytes(user["signing_key"]))


def _login_as(app, request, nest, user, *, handle: str) -> None:
    login_as(
        app, nest, user, handle=handle, device_id="test-device-paid-offers",
        request=request, spa_url_fixture="offers_spa_url",
        wait_for_id="compose-text-field",
    )


def _create_tier(app, name: str, rank: int) -> None:
    subs = app.subscriptions
    subs.navigate()
    subs.open_tiers_tab()
    subs.create_tier(name, rank=rank, price_hint="$5/mo")
    assert subs.wait_for_tier(name), (
        f"created tier {name!r} should appear in §1 My tiers; error={subs.error_text()!r}"
    )


def _can_read_tier(api: ApiActor, author_id: bytes, tier: str) -> bool:
    try:
        return api.subscription_key_blob(author_id, tier)["version"] >= 1
    except RpcCallError:
        return False


def _approve_until(app, done, timeout: float = AUTHOR_GRANT_BUDGET_S) -> bool:
    """Drive the author's §2 until ``done()``, approving whatever is pending
    when observed (the connect-time drain may win rows first)."""
    subs = app.subscriptions
    subs.navigate()
    subs.open_tiers_tab()
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if done():
            return True
        subs.refresh()
        if subs.pending_request_count() >= 1:
            subs.approve_first_request()
        time.sleep(1)  # sleep-ok: poll interval of a deadline loop
    return bool(done())


def _sell(app, preview: str, *, subscribers_get_it_free: bool = True) -> str:
    """Sell a post through the composer; return its auto-minted tier name."""
    feed = app.feed
    feed.navigate()
    feed.sell_post(
        f"the sold body {_unique('marker')}", preview, PRICE,
        subscribers_get_it_free=subscribers_get_it_free,
    )
    row = feed.wait_for_post_state_by_text(preview)
    assert row is not None, f"the sold post should land; error={app.error_text()!r}"
    card = feed.post_index_by_id(row["post_id"])
    badge = app.driver.get_text("gated-post-badge", scope=f"post-card[{card}]")
    assert badge.startswith(UNLOCK_TIER_PREFIX), (
        f"a sold post's badge names its {UNLOCK_TIER_PREFIX}* tier, got {badge!r}; "
        f"error={app.error_text()!r}"
    )
    return badge


def _card_for(app, preview: str) -> int:
    row = app.feed.wait_for_post_state_by_text(preview)
    assert row is not None, (
        f"the feed should list the post teased {preview!r}; error={app.error_text()!r}"
    )
    card = app.feed.post_index_by_id(row["post_id"])
    assert card >= 0, f"the post {row['post_id']} has no rendered card"
    return card


@pytest.mark.feature("paid-posts-and-tips")
def test_a_non_subscriber_sees_only_the_teaser_and_the_tier_badge(app, request, offers_nest):
    """Outcome: someone not subscribed to a post's tier sees only its teaser and
    a badge naming the tier, never the post itself."""
    app.subscriptions.require_sell_post_authoring_supported()
    author = _register(offers_nest)
    stranger = _register(offers_nest)
    _login_as(app, request, offers_nest, author, handle=_unique("e2e-gater"))
    tier = _unique("gold")
    _create_tier(app, tier, 1)
    preview = _unique("public-teaser")
    marker = _unique("members-only")
    app.feed.navigate()
    app.feed.create_gated_post(f"the members' text: {marker}", preview, tier)

    _login_as(app, request, offers_nest, stranger, handle=_unique("e2e-stranger"))
    card = _card_for(app, preview)
    row = app.feed.post_state_by_text(preview)
    assert marker not in row.get("body", ""), f"the post body reached a non-subscriber: {row!r}"
    badge = app.driver.get_text("gated-post-badge", scope=f"post-card[{card}]")
    assert badge == tier, (
        f"the card's badge should name the tier {tier!r}, got {badge!r}; "
        f"error={app.error_text()!r}"
    )
    assert not _can_read_tier(_api(offers_nest, stranger), author["actor_id_bytes"], tier), (
        "a non-subscriber must hold no key to the tier the post is sealed under"
    )


@pytest.mark.feature("paid-posts-and-tips")
def test_a_post_sold_to_everyone_leaves_existing_subscribers_out(app, request, offers_nest):
    """Outcome: you can sell a post so that even your existing subscribers have
    to buy it (the "subscribers get it free" toggle turned OFF).

    The creation-time fan-out runs inside the nest's `tiers.create` handler, so
    once the sale's tier exists the enqueue is settled: with the toggle off it
    named no existing subscriber. The subscriber's own app then shows the post
    as something to BUY."""
    app.subscriptions.require_sell_post_seller_fanout_leg_supported()
    seller = _register(offers_nest)
    subscriber = _register(offers_nest)
    seller_id = seller["actor_id_bytes"]
    seller_handle = _unique("e2e-seller")
    _login_as(app, request, offers_nest, seller, handle=seller_handle)
    gold = _unique("gold")
    _create_tier(app, gold, 2)
    sub_api = _api(offers_nest, subscriber)
    assert sub_api.subscribe(seller_id, gold).get("outcome") == "queued"
    assert _approve_until(app, lambda: _can_read_tier(sub_api, seller_id, gold)), (
        f"precondition: the gold subscriber is granted; error={app.subscriptions.error_text()!r}"
    )

    preview = _unique("everyone-pays")
    unlock = _sell(app, preview, subscribers_get_it_free=False)

    seller_api = _api(offers_nest, seller)
    fanned = [
        r for r in seller_api.subscription_requests_list()
        if r.get("tier_name") == unlock
    ]
    assert fanned == [], (
        f"a sale with subscribers-free OFF must enqueue no existing subscriber "
        f"into {unlock!r}, got {fanned!r}"
    )
    assert not _can_read_tier(sub_api, seller_id, unlock), (
        "an existing gold subscriber must not read a post sold to everyone"
    )

    _login_as(app, request, offers_nest, subscriber, handle=_unique("e2e-gold-sub"))
    card = _card_for(app, preview)
    assert app.feed.wait_for_unlock_offer_price(card=card), (
        f"the existing subscriber should be offered the post to buy; "
        f"error={app.error_text()!r}"
    )
    assert app.feed.unlock_offer_price_text(card=card) == PRICE


@pytest.mark.feature("paid-posts-and-tips")
def test_buying_a_subscribers_free_post_makes_you_a_follower(app, request, offers_nest):
    """Outcome: buying a post that the author's subscribers read free also makes
    you a follower of the author (`monetization.md` § Per-post pay-to-unlock —
    a designated grant fans out to undesignated tiers at or below its rank, and
    the free followers tier is rank 0).

    The followers tier exists once someone follows; a third actor's follow
    provisions it, so the buyer is never a follower before the purchase."""
    app.subscriptions.require_sell_post_seller_fanout_leg_supported()
    seller = _register(offers_nest)
    follower = _register(offers_nest)
    buyer = _register(offers_nest)
    seller_id = seller["actor_id_bytes"]
    seller_handle = _unique("e2e-seller")
    _login_as(app, request, offers_nest, seller, handle=seller_handle)

    follow_api = _api(offers_nest, follower)
    follow_api.subscribe(seller_id, FOLLOWERS_TIER)
    assert _approve_until(app, lambda: _can_read_tier(follow_api, seller_id, FOLLOWERS_TIER)), (
        "precondition: a third actor's follow provisions the followers tier"
    )
    preview = _unique("subscribers-free")
    unlock = _sell(app, preview)

    buyer_api = _api(offers_nest, buyer)
    assert buyer_api.subscription_status(seller_id).get("tier") is None, (
        "precondition: the buyer holds nothing from the seller before buying"
    )
    assert not _can_read_tier(buyer_api, seller_id, FOLLOWERS_TIER)

    # ── The buyer buys through the teaser (the mutation under test). ─────────
    _login_as(app, request, offers_nest, buyer, handle=_unique("e2e-buyer"))
    card = _card_for(app, preview)
    assert app.feed.wait_for_unlock_offer_price(card=card), (
        f"the buyer should be offered the post; error={app.error_text()!r}"
    )
    app.feed.buy_unlock_offer(card=card)
    assert app.feed.wait_for_unlock_offer_cleared(card=card), (
        f"the purchase should land; error={app.error_text()!r}"
    )

    # ── The seller's client grants it; the buyer becomes a follower too. ─────
    _login_as(app, request, offers_nest, seller, handle=seller_handle)
    assert _approve_until(
        app,
        lambda: _can_read_tier(buyer_api, seller_id, unlock)
        and _can_read_tier(buyer_api, seller_id, FOLLOWERS_TIER),
    ), (
        f"the buyer of a subscribers-free post should read it AND follow the "
        f"author; unlock={_can_read_tier(buyer_api, seller_id, unlock)}, "
        f"followers={_can_read_tier(buyer_api, seller_id, FOLLOWERS_TIER)}; "
        f"error={app.subscriptions.error_text()!r}"
    )
    followers = _api(offers_nest, seller).subscription_subscribers_list(FOLLOWERS_TIER)
    assert any(bytes(s["subscriber_id"]) == buyer["actor_id_bytes"] for s in followers), (
        f"the buyer should be on the seller's followers roster, got {followers!r}"
    )


@pytest.mark.feature("paid-posts-and-tips")
def test_a_sold_post_is_never_offered_as_a_tier_or_an_audience(app, request, offers_nest):
    """Outcomes: a post you sell is never offered on your profile as a tier to
    subscribe to, nor as an audience when you write a new post.

    Both are absences read behind a barrier from the same list: the ordinary
    tier IS offered (on the profile, in the composer) before the sold post's
    tier is asserted absent. The composer half needs no option-list command —
    an agent REFUSES a select value its frame did not paint
    (`SelectOptionNotOffered`, convention 11), so selecting the sold tier is
    the read."""
    app.subscriptions.require_sell_post_authoring_supported()
    app.subscriptions.require_sell_post_seller_fanout_leg_supported()
    app.profile.require_other_profile_offers_browse_supported()
    seller = _register(offers_nest)
    visitor = _register(offers_nest)
    _login_as(app, request, offers_nest, seller, handle=_unique("e2e-seller"))
    gold = _unique("gold")
    _create_tier(app, gold, 1)
    unlock = _sell(app, _unique("sold-teaser"))

    # ── The composer: gold is an audience, the sold post's tier is not. ──────
    feed = app.feed
    feed.navigate()
    assert feed.wait_for_audience_option(gold, timeout_s=15.0), (
        f"barrier: the composer should offer the ordinary tier {gold!r}; "
        f"error={app.error_text()!r}"
    )
    with pytest.raises(SelectOptionNotOffered):
        app.driver.select("compose-gate-tier-select", unlock)

    # ── The profile: a visitor is offered gold, never the sold post's tier. ──
    _api(offers_nest, visitor).accept_knock(seller["actor_id_hex"])
    _login_as(app, request, offers_nest, visitor, handle=_unique("e2e-visitor"))
    app.contacts.open_contact_profile(seller["actor_id_hex"])
    app.driver.wait_for("profile-view", timeout=10.0)
    app.profile.open_tiers_tab()
    assert app.profile.wait_for_offer(gold), (
        f"barrier: the seller's ordinary tier {gold!r} should be offered; "
        f"count={app.profile.offer_count()}, error={app.profile.error_text()!r}"
    )
    offered = [app.profile.offer_name(i) for i in range(app.profile.offer_count())]
    assert unlock not in offered and not any(o.startswith(UNLOCK_TIER_PREFIX) for o in offered), (
        f"a sold post's tier must never be offered on the profile, got {offered!r}"
    )


@pytest.mark.feature("paid-posts-and-tips")
def test_you_are_never_offered_your_own_sold_post(app, request, offers_nest):
    """Outcome: you are never offered a purchase of a post you sold yourself.

    Barrier: the same feed also holds ANOTHER seller's sold post, whose price
    resolves in the same render pass that would resolve the author's own — so
    once the foreign card shows its price, the own card's resolve has run, and
    its having painted no price is settled."""
    app.subscriptions.require_teaser_buy_supported()
    other = _register(offers_nest)
    me = _register(offers_nest)
    _login_as(app, request, offers_nest, other, handle=_unique("e2e-other-seller"))
    foreign_preview = _unique("foreign-sale")
    _sell(app, foreign_preview)

    _login_as(app, request, offers_nest, me, handle=_unique("e2e-self-seller"))
    own_preview = _unique("own-sale")
    _sell(app, own_preview)

    foreign = _card_for(app, foreign_preview)
    assert app.feed.wait_for_unlock_offer_price(card=foreign), (
        f"barrier: another seller's sold post should be offered with its price; "
        f"error={app.error_text()!r}"
    )
    own = _card_for(app, own_preview)
    assert app.driver.count("gated-post-buy-button", scope=f"post-card[{own}]") == 0, (
        "the author must never be offered a purchase of their own sold post"
    )
    assert app.driver.count("gated-post-price", scope=f"post-card[{own}]") == 0, (
        "the author's own sold post must carry no offer price"
    )
