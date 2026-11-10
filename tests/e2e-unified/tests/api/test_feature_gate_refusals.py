"""A feature you may not use, or have used up, is refused by your nest with a
typed refusal that says why — and with payments off, every act that adds to
the money plane is refused while every act that undoes one never is.

The witnesses for `docs/features/status-quotas-and-limits.md` outcome 15, and
for outcome 16.

`docs/goal/architecture/dynamic-features.md` § Implementation status today, the
(c) item and the per-surface composition paragraph: the nest's enforcement
floor answers a refused operation with the typed Dim-4 refusal —
`fauna.features.denied` for an availability deny, `fauna.features.over_quota`
for a quota that has run out — whose details name the feature, the gate
surface, the tier that bound it and, for a quota, the dimension, window, limit
and observed total, and nothing else (`bins/fauna-nest/src/feature_gate.rs`'s
`refusal`). Ruling (i) of the per-surface composition: de-escalation is never
gated — removing a provider, voiding a claim and clearing a web paywall stay
open under a deny, because a tier that can only tighten must never trap a user
in a configuration they can no longer undo.

The binding tier here is the account's OWN (`fauna.features.self_limits.update`,
the self tier): it is per-account, so a test on the shared session nest binds
nobody else, and the refusal names `self`. The admin tier's attribution is the
same `refusal` builder; its in-process pin is
`bins/fauna-nest/tests/conformance_feature_gate_surfaces.rs`, which drives
every wired surface through its own handler. This file is the same promise
observed from outside, over the production WS-RPC doors of a real binary.

And the last clause of outcome 16 — *a refused zap still lands as a tip*:
`docs/goal/behavior/monetization.md` § The Nostr advertisement of a sold post
and § Implementation status today, the reachability note. A payee who sells a
post and advertises it (`fauna.nostr.advertise.set`) has a priced kind-1 note
on the nest's relay, so a designated signer's receipt meeting the asking price
names a real sold post. The purchase is a second spend on the payments plane
(`payments.unlock.purchase`), and a payments quota never reaches the zaps
plane's own ingest gate (quota bounds do not inherit along the subset edge),
so under a used-up payments quota the receipt is still believed and the sats
are attributed to the post as a tip while nothing is sold
(`bins/fauna-nest/src/nostr/zap_ingest.rs`'s `apply_zap_purchase`). The
control is the same receipt with payments allowed: a purchase, which mints the
claim code an unbound buyer binds later, and no tip. The in-process twin is
`conformance_zap_purchase.rs`'s `a_payments_quota_degrades_a_zap_purchase_to_a_tip`.

tier_3: a real `fauna-nest` binary over its production doors; no app.
"""

from __future__ import annotations

import os
import secrets
import time

import blake3
import pytest

import fauna_ffi
from clients._ws_rpc_core import RpcCallError
from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import create_actor_and_register
from helpers import nostr_relay
from helpers.blob_upload import upload_blob
from helpers.budgets import RPC_ROUNDTRIP_S
from helpers.set_names import addressed
from helpers.waiting import wait_until
from helpers.zap_receipts import LNBC_21_SATS, ZapSigner
from tests.api import ws_api

pytestmark = pytest.mark.tier_3

DENIED = "fauna.features.denied"
OVER_QUOTA = "fauna.features.over_quota"


def _member(nest) -> dict:
    return create_actor_and_register(
        nest["port"], base_url=nest["url"], admin_signing_key=nest["admin"]["signing_key"],
    )


def _ws(nest, actor) -> WsRpcAdminClient:
    return WsRpcAdminClient(
        nest["url"], actor_id=bytes(actor["actor_id_bytes"]),
        signing_key=bytes(actor["signing_key"]),
    )


def _limit_myself(nest, actor, policy: dict) -> None:
    """The account's own limit on `payments` — the self tier's whole-document
    replace, the same write the Status page's feature-limit row makes."""
    with _ws(nest, actor) as ws:
        ws.call("fauna.features.self_limits.update", {"feature": "payments", "policy": policy})


def _create_tier(ws, actor, name: str, **extra) -> dict:
    payload = {
        "name": name, "rank": 1, "auto_approve": False,
        "encrypted_upload": fauna_ffi.build_tier_birth_upload(
            bytes(actor["signing_key"]), name, secrets.token_bytes(32)),
        **extra,
    }
    return ws.call("fauna.subscriptions.tiers.create", payload)


def _refused(call, *, code: str, surface: str) -> dict:
    """Run ``call``; assert it is the typed feature-gate refusal at ``surface``
    bound by the account's own tier, and return its details map."""
    with pytest.raises(RpcCallError) as exc:
        call()
    error = exc.value
    assert error.code == code, (
        f"expected {code} at {surface}, got {error.code} "
        f"(message_key={error.message_key}, details={error.details!r})"
    )
    details = error.details
    assert isinstance(details, dict), f"the refusal must carry a details map: {details!r}"
    assert details.get("feature") == "payments", f"the refusal must name its feature: {details!r}"
    assert details.get("surface") == surface, (
        f"the refusal must name the surface that was composed: {details!r}"
    )
    assert details.get("tier") == "self", (
        f"the refusal must name the tier that set the limit: {details!r}"
    )
    return details


@pytest.mark.feature("status-quotas-and-limits")
def test_a_denied_operation_is_refused_naming_its_feature_and_tier(nest_instance):
    nest = nest_instance
    me = _member(nest)
    with _ws(nest, me) as ws:
        _create_tier(ws, me, "gold")
    _limit_myself(nest, me, {"availability": "deny"})

    with _ws(nest, me) as ws:
        details = _refused(
            lambda: ws.call("fauna.payments.claims.mint", {"tier": "gold"}),
            code=DENIED, surface="payments.claim.mint",
        )
        assert set(details) == {"feature", "surface", "tier"}, (
            "a deny names the feature, surface and tier and carries nothing else "
            f"(no window: nothing was counted): {details!r}"
        )
        assert ws.call("fauna.payments.claims.list", {}).get("claims", []) == [], (
            "the refusal is the floor, not a warning: no claim was minted"
        )


@pytest.mark.feature("status-quotas-and-limits")
def test_a_used_up_quota_is_refused_naming_its_window_limit_and_use(nest_instance):
    nest = nest_instance
    me = _member(nest)
    with _ws(nest, me) as ws:
        _create_tier(ws, me, "gold")
    _limit_myself(nest, me, {"availability": "limit", "operations": {"per_day": 1}})

    with _ws(nest, me) as ws:
        first = ws.call("fauna.payments.claims.mint", {"tier": "gold"})
        assert first.get("code"), f"the one operation the quota allows must succeed: {first!r}"

        details = _refused(
            lambda: ws.call("fauna.payments.claims.mint", {"tier": "gold"}),
            code=OVER_QUOTA, surface="payments.claim.mint",
        )
        assert details.get("dimension") == "operations", details
        assert details.get("window") == "day", (
            f"a windowed quota names its window: {details!r}"
        )
        assert details.get("limit") == 1, f"the refusal names the limit: {details!r}"
        assert details.get("observed") == 1, (
            f"the refusal names what was already used: {details!r}"
        )
        assert set(details) == {
            "feature", "surface", "tier", "dimension", "window", "limit", "observed",
        }, f"the refusal carries nothing beyond its reason: {details!r}"
        codes = [c.get("code") for c in ws.call("fauna.payments.claims.list", {})["claims"]]
        assert codes == [first["code"]], (
            f"the refused mint wrote nothing; only the allowed one stands: {codes!r}"
        )


@pytest.mark.feature("status-quotas-and-limits")
def test_with_payments_denied_the_adds_are_refused_and_the_undos_never_are(nest_instance):
    nest = nest_instance
    me = _member(nest)
    seller = _member(nest)

    paywalled, unpaywalled = "premium", "extras"
    # Everything an undo will act on is set up while payments are allowed.
    with _ws(nest, me) as ws:
        _create_tier(ws, me, "gold")
        saved = ws.call(
            "fauna.payments.providers.set",
            {"kind": "fake", "webhook_secret": "whsec_e2e_gate", "tier": "gold"},
        )
        assert saved.get("saved") is True, f"providers.set failed: {saved!r}"
        my_code = ws.call("fauna.payments.claims.mint", {"tier": "gold"})["code"]
        for name in (paywalled, unpaywalled):
            fauna_ffi.harness_create_set(nest["url"], bytes(me["signing_key"]), {"name": name})
            ws.call("fauna.folders.update", addressed(name, website_enabled=True))
        ws.call("fauna.folders.set_web_paywall", addressed(paywalled, tier="gold"))
    with _ws(nest, seller) as ws:
        _create_tier(ws, seller, "silver")
        their_code = ws.call("fauna.payments.claims.mint", {"tier": "silver"})["code"]

    _limit_myself(nest, me, {"availability": "deny"})

    price = {"value": 21_000, "unit": "msat"}
    with _ws(nest, me) as ws:
        # ── The adds: each refused, typed, at its own surface. ──
        _refused(
            lambda: ws.call(
                "fauna.payments.providers.set",
                {"kind": "fake", "webhook_secret": "whsec_e2e_gate_2", "tier": "gold"},
            ),
            code=DENIED, surface="payments.provider.configure",
        )
        _refused(
            lambda: _create_tier(ws, me, "priced", asking_price=price),
            code=DENIED, surface="payments.tier.price",
        )
        _refused(
            lambda: ws.call(
                "fauna.subscriptions.tiers.update", {"name": "gold", "asking_price": price},
            ),
            code=DENIED, surface="payments.tier.price",
        )
        _refused(
            lambda: _create_tier(ws, me, "post-unlock", unlocks_post=secrets.token_hex(32)),
            code=DENIED, surface="payments.paywall.designate",
        )
        _refused(
            lambda: ws.call(
                "fauna.folders.set_web_paywall", addressed(unpaywalled, tier="gold"),
            ),
            code=DENIED, surface="payments.paywall.designate",
        )
        _refused(
            lambda: ws.call("fauna.payments.claims.mint", {"tier": "gold"}),
            code=DENIED, surface="payments.claim.mint",
        )
        _refused(
            lambda: ws.call("fauna.payments.claims.redeem", {"code": their_code}),
            code=DENIED, surface="payments.claim.redeem",
        )
        tiers = {t["name"] for t in ws.call("fauna.subscriptions.tiers.list", {})["tiers"]}
        assert "priced" not in tiers and "post-unlock" not in tiers, (
            f"a refused create wrote no tier: {sorted(tiers)!r}"
        )

        # ── The control: a free tier is not a payments operation. ──
        free = _create_tier(ws, me, f"free-{int(time.time())}")
        assert free.get("created") is True, (
            f"a deny on payments must not take ordinary tiers down with it: {free!r}"
        )

        # ── The undos: each still works under the deny. ──
        ws.call("fauna.payments.providers.remove", {"kind": "fake"})
        assert ws.call("fauna.payments.providers.list", {}).get("providers", []) == [], (
            "removing a provider is de-escalation and must never be gated"
        )
        ws.call("fauna.payments.claims.void", {"code": my_code})
        claims = {c["code"]: c for c in ws.call("fauna.payments.claims.list", {})["claims"]}
        assert claims[my_code].get("voided_at") is not None, (
            f"voiding a claim is de-escalation and must never be gated: {claims[my_code]!r}"
        )
        cleared = ws.call("fauna.folders.set_web_paywall", addressed(paywalled, tier=None))
        assert cleared.get("ok") is True, (
            f"clearing a web paywall is de-escalation and must never be gated: {cleared!r}"
        )


# ── A refused zap still lands as a tip ──────────────────────────────────────

#: The tier's asking price, met exactly by `LNBC_21_SATS` (the rule is `>=`).
ASKING_MSATS = 21_000


def _advertised_sold_post(nest) -> tuple[dict, str, str, dict]:
    """A payee who sells one post for 21 sats and advertises it on Nostr, with
    a Lightning provider designated: ``(payee, tier, post_id, note)``, where
    ``note`` is the priced kind-1 the nest signed onto its own relay.

    The drive is the "sell this post" flow over the production doors: link a
    Nostr key the nest holds (the advertisement is signed with it), mint the
    degenerate single-post tier priced and designating the post, author the
    post gated to it (its sealed half uploaded first), then advertise it."""
    payee = _member(nest)
    secret = bytes(payee["signing_key"])
    linked = ws_api.bridge_link(nest["port"], payee, "nostr", "generate")
    assert linked["linked"], linked
    pubkey = fauna_ffi.nostr_npub_to_hex(linked["identity"]["value"])

    tier = f"post-{secrets.token_hex(4)}"
    teaser = f"a post for sale {secrets.token_hex(4)}"
    period_key = secrets.token_bytes(32)
    body, sealed = fauna_ffi.build_gated_post(
        secret, teaser, f"{teaser}\n\nthe part you pay for", tier, 1,
        secrets.token_bytes(32), period_key,
    )
    # `post_id = blake3(body)`: the client knows it before the post exists,
    # which is how the tier can designate a post written after it.
    post_id = blake3.blake3(body).hexdigest()
    with _ws(nest, payee) as ws:
        created = ws.call("fauna.subscriptions.tiers.create", {
            "name": tier, "rank": 1, "auto_approve": False,
            "encrypted_upload": fauna_ffi.build_tier_birth_upload(secret, tier, period_key),
            "unlocks_post": post_id,
            "asking_price": {"value": ASKING_MSATS, "unit": "msat"},
        })
        assert created.get("created") is True, f"the unlock tier: {created!r}"
    upload_blob(nest["port"], payee["token"], sealed, audience_class="PeriodRestrictedPost")
    assert ws_api.create_post(nest["port"], payee, body) == post_id

    signer = ZapSigner()
    with _ws(nest, payee) as ws:
        ws.call("fauna.nostr.zap_signers.add",
                {"signer_pubkey": signer.pubkey, "label": "my wallet provider"})
        advertised = ws.call("fauna.nostr.advertise.set", {"post_id": post_id})
        assert advertised.get("advertised_at"), f"advertise.set: {advertised!r}"

    relay = nostr_relay.nest_relay_url(nest)
    note = wait_until(
        lambda: next(
            (e for e in nostr_relay.query(relay, {"authors": [pubkey], "kinds": [1]})
             if ["price", str(ASKING_MSATS), "msat"] in e["tags"]),
            None,
        ),
        RPC_ROUNDTRIP_S,
        diagnose=lambda: f"the advertisement of {post_id} never reached the nest's relay "
        f"priced at {ASKING_MSATS} msat",
    )
    payee["zap_signer"] = signer
    return payee, tier, post_id, note


def _zap(nest, payee, note) -> None:
    """The designated provider reports a 21-sat zap on the advertised note from
    a wallet no account on this nest holds — an unbound buyer."""
    receipt = payee["zap_signer"].receipt(
        payee_pubkey=note["pubkey"], target_event_id=note["id"],
        tipper_secret=os.urandom(32), bolt11=LNBC_21_SATS,
    )
    accepted, message = nostr_relay.publish(nostr_relay.nest_relay_url(nest), receipt)
    assert accepted, (
        "a payments bound never reaches the zaps plane's own ingest gate, so the "
        f"designated provider's receipt is believed: {message!r}"
    )


def _tips(nest, payee, post_id) -> tuple[int, int]:
    with _ws(nest, payee) as ws:
        reply = ws.call("fauna.tips.list", {"post_id": post_id})
    return reply["total_msats"], reply["tip_count"]


def _sales(nest, payee, tier) -> list[dict]:
    with _ws(nest, payee) as ws:
        claims = ws.call("fauna.payments.claims.list", {}).get("claims", [])
    return [c for c in claims if c.get("tier") == tier]


@pytest.mark.feature("status-quotas-and-limits")
def test_with_payments_used_up_a_zap_meeting_the_price_still_lands_as_a_tip(nest_instance):
    nest = nest_instance
    payee, tier, post_id, note = _advertised_sold_post(nest)
    # Nothing may be bought any more: the payee's own payments operations bound is 0.
    _limit_myself(nest, payee, {"availability": "limit", "operations": {"per_day": 0}})

    _zap(nest, payee, note)

    wait_until(
        lambda: _tips(nest, payee, post_id) == (ASKING_MSATS, 1) or None,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"the refused purchase must land as a 21-sat tip on {post_id}; "
            f"tips.list reads (total_msats, tip_count) = {_tips(nest, payee, post_id)}"
        ),
    )
    # The decision precedes the record (resolve → apply → record), so with the
    # tip visible the purchase leg has already been refused for good.
    sales = _sales(nest, payee, tier)
    assert sales == [], f"the payments bound refused the purchase, so nothing was sold: {sales!r}"
    with _ws(nest, payee) as ws:
        requests = ws.call("fauna.subscriptions.requests.list", {}).get("requests", [])
    assert [r for r in requests if r.get("tier_name") == tier] == [], (
        f"no entitlement on the sold tier was queued: {requests!r}"
    )


@pytest.mark.feature("status-quotas-and-limits")
def test_with_payments_allowed_the_same_zap_is_a_purchase_not_a_tip(nest_instance):
    """The control: the same advertised post and the same receipt, with no
    payments bound — the zap buys the post (an unbound buyer gets the claim
    code the engine mints) and is excluded from the post's tips."""
    nest = nest_instance
    payee, tier, post_id, note = _advertised_sold_post(nest)

    _zap(nest, payee, note)

    sold = wait_until(
        lambda: _sales(nest, payee, tier) or None,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"a zap meeting the asking price must buy {tier}; no claim was minted, and "
            f"tips.list reads {_tips(nest, payee, post_id)}"
        ),
    )
    assert len(sold) == 1, f"one zap, one sale: {sold!r}"
    assert _tips(nest, payee, post_id) == (0, 0), (
        "a zap that bought the post is a sale, never also a tip"
    )
