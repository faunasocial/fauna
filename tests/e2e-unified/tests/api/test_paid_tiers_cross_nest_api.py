"""tier_3, real nest binaries: what the nests do with a PAID tier across them.

``docs/goal/behavior/monetization.md`` § Pillar 3 and § Per-post
pay-to-unlock own the rules; ``docs/goal/behavior/federated-follow.md``
§ Decision 2 owns the follow twin they ride. An author sells tiers on their
home nest H; the buyer's account lives on another nest F, and every gesture
the buyer makes reaches H only through F. This module observes the four
``[nest]`` outcomes of ``docs/features/subscriptions-and-tiers.md`` that
cross nests, from outside, with no app in it:

* outcome 31 — *Which nest an in-app redeem binds on*: a code minted on H is
  redeemed through F's own ``claims.redeem``; the payment binds to the buyer on
  H and the post the tier sells reaches F whatever its age; a code of another
  author is refused as unknown.
* outcome 32 — *A checkout-bound buyer homed on another nest*, rule (3): a
  payment that lands before the buyer's relayed subscribe is a half-row the
  author neither lists nor approves; the subscribe completes it into one
  request.
* outcome 33 — § Per-post pay-to-unlock, the rank cascade: a buyer on F of a
  ranked tier is served the key of every per-post tier at or below its rank,
  and of none above it.
* outcome 34 — *Expiry & revocation*: a refund, the author's void of a
  redeemed code and a lapse past the grace each end the subscription on H and
  reach F as ``removed``; a renewal inside the grace keeps the follow.

The client halves run through the shared Rust every app calls: the buyer's
follow and redeem gestures (``fauna_ffi.harness_follow`` /
``harness_redeem``) and the author's tier editor and pump
(``harness_author_create_tier`` / ``harness_author_reconcile``, whose drain
approves a payment-backed request with no author judgment). The provider is
the nest's ``fake`` adapter, whose signed webhook stands in for a real
provider's delivery. These are the outside-observed twins of
``conformance_cross_nest_conversations_client.rs``'s
``a_buyer_redeems_a_code_minted_on_another_nest_through_their_own_nest``,
``a_checkout_bound_buyer_homed_on_another_nest_meets_their_payment_on_one_request``,
``a_remote_buyers_nest_follows_the_per_post_tiers_their_paid_tier_includes`` and
``a_refunded_buyer_homed_on_another_nest_is_removed_through_the_leaves_path``.

**Topology:** ``cross_nest_foreign`` is the author's home H,
``cross_nest_foreign_ephemeral`` the buyers' nest F — the pair
``test_federated_follow_cross_nest_api.py`` uses.

**Convention 14:** every wait is a deadline poll over projected state; a lapse
is a window that had already ended when it was paid, and the lapse sweep is
run by the nest's test-hooks poke, never waited for. An absence is asserted
only after a barrier: a post that must not reach one reader is checked once
the same post has reached another reader on the same nest.
"""

from __future__ import annotations

import hashlib
import hmac
import json
import secrets
import time
import uuid

import pytest
import requests

import fauna_ffi
from clients._ws_rpc_core import RpcCallError
from conftest import provision_foreign_handled_actor
from helpers.federated_follow import (
    CROSSING_BUDGET_S,
    call,
    hex_of,
    listed_to,
    new_reader,
    peer_of,
    post_as,
    pump_until,
    seed_of,
    status_of,
    wait_listed,
)
from helpers.room_seats import wait_for

pytestmark = pytest.mark.tier_3

GOLD = "gold"
BACKFILL_POSTS = 50
"""``follow_delivery::BACKFILL_POSTS``: the newest posts a first subscriber
homed on a nest is sent. A post older than that reaches the nest only as the
post its tier sells."""

LAPSE_GRACE_S = 3 * 24 * 3600
"""``entitlement_lapse::PAID_WINDOW_LAPSE_GRACE_SECS``."""


# ── helpers ────────────────────────────────────────────────────────────────


def _author_id(author: dict) -> bytes:
    return bytes(author["actor_id_bytes"])


def _tier(home: dict, author: dict, name: str, rank: int, unlocks_post: str | None = None):
    """The author creates a non-auto-approve tier through the shared tier
    editor; its period key rests in the account's custody, so the pump can
    approve its buyers and remove them."""
    created = fauna_ffi.harness_author_create_tier(
        home["url"], seed_of(author), name, rank, unlocks_post=unlocks_post
    )
    assert created.get("created") is True, f"the tier {name!r} is created: {created!r}"


def _mint(home: dict, author: dict, tier: str) -> str:
    minted = call(home, author, "fauna.payments.claims.mint", {"tier": tier})
    assert minted.get("tier") == tier and minted.get("code"), minted
    return minted["code"]


def _redeem(away: dict, buyer: dict, code: str, author: dict, home: dict) -> dict:
    """The buyer redeems ``code`` in their app, on their own nest, naming the
    author's home as the profile carries it."""
    return fauna_ffi.harness_redeem(
        away["url"], seed_of(buyer), code, _author_id(author), peer_of(home)
    )


def _provider(home: dict, author: dict, tier: str) -> str:
    """The author connects the ``fake`` provider, selling ``tier``; returns the
    webhook secret."""
    secret = f"whsec_{uuid.uuid4().hex}"
    saved = call(
        home,
        author,
        "fauna.payments.providers.set",
        {"kind": "fake", "webhook_secret": secret, "tier": tier},
    )
    assert saved.get("saved") is True, f"providers.set: {saved!r}"
    return secret


def _webhook(home: dict, author: dict, secret: str, **event) -> dict:
    """One signed delivery from the fake provider to the author's webhook on
    H — ``reference`` is the checkout's ``client_reference_id``, an actor id
    and never a home (``libs/fauna-payments/src/fake.rs``)."""
    body = {"id": event.pop("external_ref", None) or f"pay_{uuid.uuid4().hex[:10]}"}
    body["event"] = event.pop("event", "payment")
    body.update(event)
    raw = json.dumps(body).encode()
    resp = requests.post(
        f"{home['url']}/api/v1/payments/webhook/{hex_of(author)}/fake",
        data=raw,
        headers={
            "Content-Type": "application/json",
            "X-Fauna-Signature": hmac.new(secret.encode(), raw, hashlib.sha256).hexdigest(),
        },
        timeout=15,
        verify=False,  # the floor TLS of a loopback test nest
    )
    assert resp.status_code == 200, f"a signed delivery is accepted: {resp.status_code} {resp.text}"
    return resp.json()


def _requests_from(home: dict, author: dict, buyer: dict) -> list[dict]:
    """The author's subscribe requests filed by ``buyer`` — what the author's
    app lists and its drain reads."""
    reply = call(home, author, "fauna.subscriptions.requests.list", {})
    return [r for r in reply.get("requests", []) if bytes(r["subscriber_id"]) == _author_id(buyer)]


def _roster(home: dict, author: dict, tier: str) -> set[str]:
    reply = call(home, author, "fauna.subscriptions.subscribers.list", {"tier_name": tier})
    return {bytes(e["subscriber_id"]).hex() for e in reply.get("subscribers", [])}


def _subscribed(away: dict, buyer: dict, author: dict, tier: str):
    """Predicate: the buyer's own nest holds the follow of ``tier`` approved."""
    s = status_of(away, buyer, hex_of(author))
    return s.get("tier") == tier and not s.get("follow_pending")


def _key_blob_via(away: dict, buyer: dict, author: dict, tier: str):
    """The buyer's key-blob read for ``tier``, as their app makes it — on their
    own nest, which relays it to the author's home. ``None`` on a refusal."""
    try:
        return call(
            away,
            buyer,
            "fauna.subscriptions.key_blob.get",
            {"author_id": _author_id(author), "tier_name": tier},
        )
    except RpcCallError:
        return None


def _key_blob_at_home(home: dict, author: dict, tier: str) -> dict:
    return call(
        home,
        author,
        "fauna.subscriptions.key_blob.get",
        {"author_id": _author_id(author), "tier_name": tier},
    )


def _lapse_sweep(home: dict) -> dict:
    """Run the hourly entitlement-lapse sweep once, on H
    (``membership_lapse_test_hook.rs``): the causal barrier a lapse needs."""
    resp = requests.post(
        f"{home['url']}/api/v1/test/membership_lapse/run-now", json={}, timeout=60, verify=False
    )
    assert resp.status_code == 200, f"lapse sweep: {resp.status_code} {resp.text}"
    body = resp.json()
    assert body.get("ok") is True, body
    return body


# ── outcome 31: a code minted on another nest, redeemed from your own ──────


@pytest.mark.feature("subscriptions-and-tiers")
def test_a_code_minted_on_another_nest_is_redeemed_from_the_buyers_own_nest(
    cross_nest_foreign, cross_nest_foreign_ephemeral
):
    """An author on H sells one old post through a per-post tier and mints a
    code for it; a buyer on F redeems the code in their app. The payment binds
    to the buyer on H, the author's app approves it, and the post — older than
    every post a first follow is sent — arrives on F. A code of another
    author, redeemed naming this one, is unknown.

    Flow: ``claims.mint`` on H → ``harness_redeem`` → F's
    ``fauna.payments.claims.redeem`` carrying the buyer's signed
    ``ClaimRedeemIntent`` → F relays ``fauna.federation.claims.redeem`` → H
    redeems the claim to the buyer and queues the payment-entitled request with
    F as its home → F keeps a pending remote follow → the author's drain
    approves → ``follow.state approved`` reaches F, and H's backfill to F's
    first subscriber carries the designated post whatever its age
    (``follow_delivery::backfill_new_home``) → F lists it to the buyer.
    """
    home, away = cross_nest_foreign, cross_nest_foreign_ephemeral
    author = provision_foreign_handled_actor(home)
    author_hex = hex_of(author)

    sold = post_as(home, author, f"the post you bought {secrets.token_hex(4)}")
    for i in range(BACKFILL_POSTS):
        post_as(home, author, f"a newer post {i} {secrets.token_hex(2)}")
    _tier(home, author, "this-post", 1, unlocks_post=sold)
    code = _mint(home, author, "this-post")

    buyer = new_reader(away)
    redeemed = _redeem(away, buyer, code, author, home)
    assert redeemed.get("tier") == "this-post" and redeemed.get("queued") is True, redeemed
    assert bytes(redeemed.get("author")) == _author_id(author), redeemed

    claim = [
        c
        for c in call(home, author, "fauna.payments.claims.list", {}).get("claims", [])
        if c.get("code") == code
    ]
    assert claim and bytes(claim[0].get("redeemed_by") or b"") == _author_id(buyer), (
        f"the code is bound to the buyer on the author's nest: {claim!r}"
    )

    pump_until(
        home,
        author,
        "the paid request approved at the home and the approval reaching the buyer's nest",
        lambda: _subscribed(away, buyer, author, "this-post"),
        bool,
    )
    wait_listed(away, buyer, author_hex, sold, "the bought post arriving on the buyer's nest")

    # ── Another author's code, redeemed naming this author ────────────────
    other = provision_foreign_handled_actor(home)
    _tier(home, other, GOLD, 1)
    foreign_code = _mint(home, other, GOLD)
    with pytest.raises(RpcCallError) as refused:
        _redeem(away, new_reader(away), foreign_code, author, home)
    assert refused.value.code == "fauna.payments.claim_not_found", refused.value.code
    still = [
        c
        for c in call(home, other, "fauna.payments.claims.list", {}).get("claims", [])
        if c.get("code") == foreign_code
    ]
    assert still and still[0].get("redeemed_by") is None, (
        f"a refused redeem leaves the other author's code unredeemed: {still!r}"
    )


# ── outcome 32: a payment ahead of the request is held until it names its home


@pytest.mark.feature("subscriptions-and-tiers")
def test_a_payment_ahead_of_the_request_from_another_nest_is_held_until_the_request_names_its_home(
    cross_nest_foreign, cross_nest_foreign_ephemeral
):
    """A buyer on F pays at the author's checkout before their app's subscribe
    has reached H. The author sees no request and their app approves nothing;
    once the subscribe from F arrives the two are one paid request, which the
    author's app approves and F learns.

    Flow: the provider's webhook names the buyer as ``reference`` → H's
    ``db::upsert_payment_entitled_request`` opens a row with no home →
    ``requests.list`` withholds it and the drain finds nothing to approve →
    ``harness_follow`` → F relays the buyer's signed ``FollowIntent`` → H's
    ``subscribe_core_from`` stamps F onto that row → listed, payment-entitled
    → the drain approves → ``follow.state approved`` → F's ``status.get``
    holds the tier.
    """
    home, away = cross_nest_foreign, cross_nest_foreign_ephemeral
    author = provision_foreign_handled_actor(home)
    _tier(home, author, GOLD, 1)
    secret = _provider(home, author, GOLD)
    buyer = new_reader(away)

    _webhook(
        home, author, secret, reference=hex_of(buyer), valid_until=int(time.time()) + 30 * 86400
    )
    assert _requests_from(home, author, buyer) == [], (
        "a payment with no request from the buyer's nest yet is not a request the author sees"
    )
    passed = fauna_ffi.harness_author_reconcile(home["url"], seed_of(author))
    assert passed.get("approved") == 0, f"the author's app approves nothing yet: {passed!r}"
    assert hex_of(buyer) not in _roster(home, author, GOLD)

    asked = fauna_ffi.harness_follow(
        away["url"], seed_of(buyer), _author_id(author), GOLD, peer_of(home)
    )
    assert "err" not in asked, f"the subscribe is answered: {asked!r}"
    rows = wait_for(
        "the subscribe from the buyer's nest meeting the payment on one request",
        lambda: _requests_from(home, author, buyer),
        lambda rs: len(rs) == 1,
        CROSSING_BUDGET_S,
    )
    assert rows[0].get("payment_entitled") is True and rows[0].get("tier_name") == GOLD, rows

    pump_until(
        home,
        author,
        "the paid request approved and the approval reaching the buyer's nest",
        lambda: _subscribed(away, buyer, author, GOLD),
        bool,
    )
    assert hex_of(buyer) in _roster(home, author, GOLD)


# ── outcome 33: a ranked tier includes the per-post tiers below it ─────────


@pytest.mark.feature("subscriptions-and-tiers")
def test_a_buyer_on_another_nest_of_a_ranked_tier_is_served_the_per_post_tiers_it_includes(
    cross_nest_foreign, cross_nest_foreign_ephemeral
):
    """An author on H sells ``gold`` at rank 2 and two posts on their own: one
    through a per-post tier at rank 1, one at rank 3. A buyer on F redeems a
    ``gold`` code: their own nest serves them the key of the rank-1 post's tier,
    as H serves a subscriber it hosts, and not the key of the rank-3 one.

    Flow: ``harness_redeem`` for ``gold`` → H's fan-out queues a request for
    every per-post tier at or below rank 2, each homed at F → the author's
    drain approves both → each approval's ``follow.state`` push reaches F,
    which opens a remote follow for the per-post tier it held none of → the
    buyer's ``key_blob.get`` for that tier, relayed by F, is H's blob; for the
    rank-3 tier the home answers ``not_subscribed``.
    """
    home, away = cross_nest_foreign, cross_nest_foreign_ephemeral
    author = provision_foreign_handled_actor(home)
    included = post_as(home, author, f"sold at rank 1 {secrets.token_hex(4)}")
    above = post_as(home, author, f"sold at rank 3 {secrets.token_hex(4)}")
    _tier(home, author, GOLD, 2)
    _tier(home, author, "post-below", 1, unlocks_post=included)
    _tier(home, author, "post-above", 3, unlocks_post=above)

    buyer = new_reader(away)
    redeemed = _redeem(away, buyer, _mint(home, author, GOLD), author, home)
    assert redeemed.get("tier") == GOLD, redeemed

    relayed = pump_until(
        home,
        author,
        "the per-post tier's key served to the buyer on their own nest",
        lambda: _key_blob_via(away, buyer, author, "post-below"),
        lambda blob: blob is not None,
    )
    assert relayed == _key_blob_at_home(home, author, "post-below"), (
        "the buyer's nest serves them the home's own key blob for the included tier"
    )
    assert _key_blob_via(away, buyer, author, GOLD) is not None, (
        "the ranked tier itself is served to the buyer on their own nest"
    )
    wait_listed(away, buyer, hex_of(author), included, "the included post reaching the buyer's nest")

    with pytest.raises(RpcCallError) as refused:
        call(
            away,
            buyer,
            "fauna.subscriptions.key_blob.get",
            {"author_id": _author_id(author), "tier_name": "post-above"},
        )
    assert refused.value.code.endswith("not_subscribed"), (
        f"a per-post tier above the paid rank is not included: {refused.value.code}"
    )


# ── outcome 34: the paid window's end reaches the subscriber's nest ────────


@pytest.mark.feature("subscriptions-and-tiers")
def test_a_paid_windows_end_reaches_the_buyers_nest_and_a_renewal_inside_the_grace_keeps_it(
    cross_nest_foreign, cross_nest_foreign_ephemeral
):
    """Four buyers on F pay for ``gold`` on H. One is refunded, one's code is
    voided by the author, one's window had run out more than the grace ago,
    and one's window ran out a day ago and is renewed. After the lapse sweep
    and the author's app, the first three no longer follow on F and receive no
    later post; the renewed buyer still follows and receives it.

    Flow: each payment approved by the author's drain → ``follow.state
    approved`` on F. Then the fake provider's ``refund`` →
    ``payment_core::void_entitlement_window``; ``claims.void`` → the same; the
    lapse sweep → ``queue_lapsed_paid_window_removals`` for a window past the
    grace; each queues the leave's ``unsubscribe`` row with the roster row's
    home → the author's pump commits it by the removal rotation →
    ``follow_state removed`` → F's ``status.get`` holds no tier. The renewal
    re-stamps the standing roster row in place (``grant_paid_entitlement``):
    nothing queues, and the sweep leaves it.
    """
    home, away = cross_nest_foreign, cross_nest_foreign_ephemeral
    author = provision_foreign_handled_actor(home)
    author_hex = hex_of(author)
    _tier(home, author, GOLD, 1)
    secret = _provider(home, author, GOLD)
    now = int(time.time())
    refunded, voided, lapsed, renewed = (new_reader(away) for _ in range(4))

    def pay(buyer: dict, valid_until: int, external_ref: str | None = None) -> dict:
        asked = fauna_ffi.harness_follow(
            away["url"], seed_of(buyer), _author_id(author), GOLD, peer_of(home)
        )
        assert "err" not in asked, f"the subscribe is answered: {asked!r}"
        return _webhook(
            home,
            author,
            secret,
            reference=hex_of(buyer),
            valid_until=valid_until,
            external_ref=external_ref,
        )

    refund_ref = f"pay_{uuid.uuid4().hex[:10]}"
    pay(refunded, now + 30 * 86400, refund_ref)
    pay(lapsed, now - LAPSE_GRACE_S - 86400)
    pay(renewed, now - 86400)
    code = _mint(home, author, GOLD)
    _redeem(away, voided, code, author, home)

    buyers = (refunded, voided, lapsed, renewed)
    pump_until(
        home,
        author,
        "every paid request approved and each approval reaching the buyers' nest",
        lambda: [_subscribed(away, b, author, GOLD) for b in buyers],
        all,
    )
    first = post_as(home, author, f"to every buyer {secrets.token_hex(4)}")
    for b in buyers:
        wait_listed(away, b, author_hex, first, "the first post reaching each buyer")

    # ── The window ends three ways; one renewal inside the grace ──────────
    refund = _webhook(home, author, secret, reference=hex_of(refunded), event="refund",
                      external_ref=refund_ref)
    assert refund.get("status") == "refund_applied", refund
    call(home, author, "fauna.payments.claims.void", {"code": code})
    renewal = _webhook(home, author, secret, reference=hex_of(renewed),
                       valid_until=now + 30 * 86400)
    assert renewal.get("queued") is False, (
        f"a renewal inside the grace re-stamps the standing subscription, never re-queues: "
        f"{renewal!r}"
    )
    _lapse_sweep(home)

    ended = (refunded, voided, lapsed)
    pump_until(
        home,
        author,
        "each ended window's removal committed at the home and reaching the buyer's nest",
        lambda: [status_of(away, b, author_hex).get("tier") for b in ended],
        lambda tiers: tiers == [None, None, None],
    )
    roster = _roster(home, author, GOLD)
    assert hex_of(renewed) in roster and not {hex_of(b) for b in ended} & roster, roster
    assert _subscribed(away, renewed, author, GOLD), "the renewed buyer still follows"

    later = post_as(home, author, f"after the windows ended {secrets.token_hex(4)}")
    wait_listed(away, renewed, author_hex, later, "the next post reaching the renewed buyer")
    for b in ended:
        assert later not in listed_to(away, b, author_hex), (
            "a post made after a paid window ended still lists to that buyer"
        )
