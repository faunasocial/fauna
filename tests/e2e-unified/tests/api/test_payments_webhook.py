"""Payment-provider webhook → tier entitlement — the Pillar 3 nest contract.

Drives the full production surface of `docs/goal/behavior/monetization.md`
§ Pillar 3 against a real `fauna-nest` binary:

- the creator configures a provider from a client connection
  (`fauna.payments.providers.set` over authenticated WS-RPC — config from
  client, never CLI);
- the provider's delivery system POSTs the public HTTP webhook
  (`/api/v1/payments/webhook/{author}/{provider}` — providers can't speak
  WS-RPC), signed with the fake provider's scheme
  (`X-Fauna-Signature: hex(HMAC_SHA256(secret, raw_body))`);
- a signature-valid event carrying `client_reference_id = actor_id`
  **enqueues the buyer's entitlement exactly like a manual approval**: every
  tier created since the storage-mode axis retired is client-minted, so the
  grant lands as a `payment_entitled` subscribe request the creator's client
  drain pump mints for on its next pass (Rail A) — asserted here via the
  creator's `requests.list`;
- bad-signature / unknown-provider deliveries get a REAL non-2xx (the
  web-serving catch-all answers unmatched paths with a `200` info page a
  provider would log as delivered — pinned here);
- a payment with no reference mints a claim code;
  `fauna.payments.claims.redeem` binds it to the redeeming actor; a second
  actor can't take it;
- a refund landing before approval strips the request's payment marker.

What deliberately is NOT here: completing the grant (the author-client
KeyBlob mint) and the post-grant `valid_until` expiry/refund behavior need
the shared-Rust drain pump, which this API harness can't run — they are
covered deterministically Rust-side: `bins/fauna-nest/tests/
conformance_payments.rs` (client-minted grant enqueue, expiry lapse, refund void,
window stamping at approve) and the `fauna-client-subscriptions`
orchestration test (`drain_approves_a_payment_entitled_request_on_a_non_auto_tier`).

Each test registers its own fresh buyer (one grant story per actor —
module-shared nest, so cross-test actor reuse would make order load-bearing).
"""

import hashlib
import hmac
import json
import urllib.error
import urllib.request
import uuid

import pytest

from actions.api_actor import ApiActor
from clients._ws_rpc_core import RpcCallError
from common.auth import create_actor_and_register

pytestmark = pytest.mark.tier_3

FAKE_SECRET = "whsec_e2e_fake_provider"


def _sign_fake(secret: str, body: bytes) -> str:
    """The fake provider's scheme: hex(HMAC_SHA256(secret, raw_body)) —
    mirrors `libs/fauna-payments/src/fake.rs::sign`."""
    return hmac.new(secret.encode(), body, hashlib.sha256).hexdigest()


def _post_webhook(
    base_url: str,
    author_hex: str,
    provider: str,
    body: bytes,
    signature: str | None,
):
    """POST the webhook; returns (status, parsed-json-or-{})."""
    url = f"{base_url}/api/v1/payments/webhook/{author_hex}/{provider}"
    req = urllib.request.Request(url, data=body, method="POST")
    req.add_header("Content-Type", "application/json")
    if signature is not None:
        req.add_header("X-Fauna-Signature", signature)
    try:
        with urllib.request.urlopen(req, timeout=15) as resp:
            return resp.status, json.loads(resp.read() or b"{}")
    except urllib.error.HTTPError as e:
        return e.code, {}


def _payment_body(
    *,
    reference: str | None = None,
    valid_until: int | None = None,
    event: str = "payment",
    external_ref: str | None = None,
) -> bytes:
    body = {"id": external_ref or f"pay_{uuid.uuid4().hex[:10]}", "event": event}
    if reference is not None:
        body["reference"] = reference
    if valid_until is not None:
        body["valid_until"] = valid_until
    return json.dumps(body).encode()


@pytest.fixture(scope="module")
def paid_nest(request, nest_mode, tmp_path_factory):
    """A nest with one creator who owns a paid (non-auto, client-minted) tier
    and a configured fake payment provider mapping to it."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "payments-nest")
    port = nest["port"]
    admin_sk = nest["admin"]["signing_key"]

    creator = create_actor_and_register(
        port, base_url=nest["url"], admin_signing_key=admin_sk
    )
    creator_actor = ApiActor(
        nest["url"],
        creator["token"],
        creator["actor_id_hex"],
        bytes(creator["signing_key"]),
    )
    tier = "gold"
    reply = creator_actor.subscription_create_tier(
        tier, rank=1, auto_approve=False, price_hint="$5/mo"
    )
    assert reply.get("created") is True, f"tier create failed: {reply!r}"

    saved = creator_actor._ws_call(
        "fauna.payments.providers.set",
        {"kind": "fake", "webhook_secret": FAKE_SECRET, "tier": tier},
    )
    assert saved.get("saved") is True, f"providers.set failed: {saved!r}"

    yield {
        "nest": nest,
        "url": nest["url"],
        "port": port,
        "admin_sk": admin_sk,
        "creator": creator,
        "creator_actor": creator_actor,
        "tier": tier,
    }
    cleanup()


def _fresh_buyer(paid_nest) -> tuple[dict, ApiActor]:
    buyer = create_actor_and_register(
        paid_nest["port"],
        base_url=paid_nest["url"],
        admin_signing_key=paid_nest["admin_sk"],
    )
    actor = ApiActor(
        paid_nest["url"],
        buyer["token"],
        buyer["actor_id_hex"],
        bytes(buyer["signing_key"]),
    )
    return buyer, actor


def _requests_for(paid_nest, buyer: dict) -> list[dict]:
    """The creator's pending subscribe requests filed by `buyer` — the queue
    the author's drain pump reads (`fauna.subscriptions.requests.list`)."""
    reply = paid_nest["creator_actor"]._ws_call("fauna.subscriptions.requests.list", {})
    want = bytes.fromhex(buyer["actor_id_hex"])
    return [r for r in reply.get("requests", []) if r.get("subscriber_id") == want]


@pytest.mark.feature("subscriptions-and-tiers")
def test_providers_list_shows_config_without_secret(paid_nest):
    """The author's provider row round-trips kind + tier; the webhook secret
    never rides the list reply (least exposure — the form re-enters it)."""
    reply = paid_nest["creator_actor"]._ws_call("fauna.payments.providers.list", {})
    providers = reply.get("providers", [])
    assert len(providers) == 1, f"expected the one configured provider: {reply!r}"
    assert providers[0]["kind"] == "fake"
    assert providers[0]["tier"] == paid_nest["tier"]
    assert "webhook_secret" not in providers[0], (
        f"the verify secret must not be echoed: {providers[0]!r}"
    )


@pytest.mark.feature("subscriptions-and-tiers")
def test_signed_webhook_with_reference_enqueues_the_grant(paid_nest):
    """The primary Q4 binding: signed-in checkout carries
    `client_reference_id = actor_id`; a signature-valid webhook enqueues the
    buyer's entitlement in the same queue subscribe uses, marked
    `payment_entitled` so the creator's drain pump mints without judgment."""
    buyer, buyer_actor = _fresh_buyer(paid_nest)
    body = _payment_body(reference=buyer["actor_id_hex"])

    status, reply = _post_webhook(
        paid_nest["url"],
        paid_nest["creator"]["actor_id_hex"],
        "fake",
        body,
        _sign_fake(FAKE_SECRET, body),
    )
    assert status == 200, f"valid webhook must be accepted: {status} {reply!r}"
    assert reply.get("status") == "granted", f"unexpected outcome: {reply!r}"
    assert reply.get("queued") is True, (
        f"a client-minted tier enqueues for the creator's client: {reply!r}"
    )

    rows = _requests_for(paid_nest, buyer)
    assert len(rows) == 1, f"exactly one queued request for the buyer: {rows!r}"
    assert rows[0]["payment_entitled"] is True, (
        f"the request must carry the verified-payment marker: {rows[0]!r}"
    )
    assert rows[0]["tier_name"] == paid_nest["tier"]
    assert rows[0]["kind"] == "subscribe"

    # Not active yet — the creator's client mints on its next drain pass.
    sub = buyer_actor.subscription_status(paid_nest["creator"]["actor_id_bytes"])
    assert sub.get("tier") is None, f"no roster row before the mint: {sub!r}"

    # Redelivery (providers retry) is idempotent: still exactly one request.
    status, reply = _post_webhook(
        paid_nest["url"],
        paid_nest["creator"]["actor_id_hex"],
        "fake",
        body,
        _sign_fake(FAKE_SECRET, body),
    )
    assert status == 200 and reply.get("queued") is True
    assert len(_requests_for(paid_nest, buyer)) == 1

    # The buyer's own subscribe intent lands on the SAME queue row.
    subscribe = buyer_actor.subscribe(
        paid_nest["creator"]["actor_id_bytes"], paid_nest["tier"]
    )
    assert subscribe.get("outcome") == "queued", f"{subscribe!r}"
    assert len(_requests_for(paid_nest, buyer)) == 1


@pytest.mark.feature("subscriptions-and-tiers")
def test_bad_signature_and_unknown_provider_are_non_2xx(paid_nest):
    """The catch-all trap (monetization.md § Pillar 3 webhook ingress): the
    serving fallback answers unmatched paths 200, which a provider records as
    delivered — so every rejection on the REGISTERED route must be a real
    error status. Pinned against the fallback's actual 200."""
    buyer, buyer_actor = _fresh_buyer(paid_nest)
    body = _payment_body(reference=buyer["actor_id_hex"])
    author_hex = paid_nest["creator"]["actor_id_hex"]
    url = paid_nest["url"]

    # Bad signature → 401 (never granted).
    status, _ = _post_webhook(url, author_hex, "fake", body, "0" * 64)
    assert status == 401, f"bad signature must be non-2xx, got {status}"
    # Missing signature → 401.
    status, _ = _post_webhook(url, author_hex, "fake", body, None)
    assert status == 401, f"missing signature must be non-2xx, got {status}"
    # Unknown adapter kind → 404.
    status, _ = _post_webhook(url, author_hex, "paypal", body, _sign_fake(FAKE_SECRET, body))
    assert status == 404, f"unknown provider kind must be non-2xx, got {status}"
    # Known kind, no config row for this author → 404.
    status, _ = _post_webhook(url, author_hex, "stripe", body, _sign_fake(FAKE_SECRET, body))
    assert status == 404, f"unconfigured provider must be non-2xx, got {status}"
    # Unknown author → 404.
    status, _ = _post_webhook(url, "ff" * 32, "fake", body, _sign_fake(FAKE_SECRET, body))
    assert status == 404, f"unknown author must be non-2xx, got {status}"
    # Signature-valid but garbage body → 400.
    garbage = b"not json"
    status, _ = _post_webhook(url, author_hex, "fake", garbage, _sign_fake(FAKE_SECRET, garbage))
    assert status == 400, f"malformed body must be non-2xx, got {status}"

    # None of the rejects granted or enqueued anything.
    assert _requests_for(paid_nest, buyer) == [], "rejected webhooks must not enqueue"
    sub = buyer_actor.subscription_status(paid_nest["creator"]["actor_id_bytes"])
    assert sub.get("tier") is None, f"rejected webhooks must not grant: {sub!r}"

    # The trap itself, for contrast: an UNREGISTERED path falls through to the
    # web-serving fallback and answers 200 — exactly why the handler above may
    # never rely on the framework for its error statuses.
    req = urllib.request.Request(
        f"{url}/api/v1/payments/webhook-not-a-route", data=body, method="POST"
    )
    with urllib.request.urlopen(req, timeout=15) as resp:
        assert resp.status == 200, "the catch-all fallback answers unmatched paths 200"


@pytest.mark.feature("subscriptions-and-tiers")
def test_unbound_payment_mints_claim_and_redeem_binds_the_actor(paid_nest):
    """Q4's universal fallback: no reference → claim code (idempotent per
    provider external_ref); redemption binds the redeeming actor and lands the
    same queued entitlement; the code is single-redeemer."""
    external_ref = f"pay_{uuid.uuid4().hex[:10]}"
    body = _payment_body(external_ref=external_ref)
    author_hex = paid_nest["creator"]["actor_id_hex"]

    status, reply = _post_webhook(
        paid_nest["url"], author_hex, "fake", body, _sign_fake(FAKE_SECRET, body)
    )
    assert status == 200 and reply.get("status") == "claim_minted", f"{status} {reply!r}"
    code = reply.get("claim_code")
    assert code, f"claim_minted must carry the code: {reply!r}"

    # Redelivery returns the SAME claim, never a second one.
    status, reply = _post_webhook(
        paid_nest["url"], author_hex, "fake", body, _sign_fake(FAKE_SECRET, body)
    )
    assert status == 200 and reply.get("status") == "claim_exists"
    assert reply.get("claim_code") == code

    # The buyer pastes the code in their client → binds their actor; the
    # entitlement enqueues for the creator's client to mint (both rails).
    buyer, buyer_actor = _fresh_buyer(paid_nest)
    redeemed = buyer_actor._ws_call("fauna.payments.claims.redeem", {"code": code})
    assert redeemed.get("tier") == paid_nest["tier"], f"{redeemed!r}"
    assert redeemed.get("queued") is True
    assert redeemed.get("author") == bytes(paid_nest["creator"]["actor_id_bytes"]), (
        f"redeem reply names the creator: {redeemed!r}"
    )
    rows = _requests_for(paid_nest, buyer)
    assert len(rows) == 1 and rows[0]["payment_entitled"] is True, (
        f"redeemed claim enqueues the payment-marked request: {rows!r}"
    )

    # A different actor cannot take an already-redeemed code.
    _, thief_actor = _fresh_buyer(paid_nest)
    with pytest.raises(RpcCallError) as exc:
        thief_actor._ws_call("fauna.payments.claims.redeem", {"code": code})
    assert exc.value.code == "fauna.payments.claim_already_redeemed"

    # An unknown code is a typed not-found.
    with pytest.raises(RpcCallError) as exc:
        thief_actor._ws_call("fauna.payments.claims.redeem", {"code": "NOSUCH1234"})
    assert exc.value.code == "fauna.payments.claim_not_found"


@pytest.mark.feature("subscriptions-and-tiers")
def test_claims_mint_and_list_cover_the_manual_no_api_path(paid_nest):
    """`fauna.payments.claims.mint`/`.list` (monetization.md § Pillar 3: claim
    codes "also cover no-API providers (bank transfer, cash) — the creator
    mints them manually"). Entirely bypasses the webhook/HMAC path — the
    creator hands the buyer a code out-of-band."""
    creator_actor = paid_nest["creator_actor"]
    tier = paid_nest["tier"]

    minted = creator_actor._ws_call("fauna.payments.claims.mint", {"tier": tier})
    assert minted.get("tier") == tier, f"{minted!r}"
    code = minted.get("code")
    assert code, f"mint must return a code: {minted!r}"

    listed = creator_actor._ws_call("fauna.payments.claims.list", {})
    manual = [c for c in listed.get("claims", []) if c.get("code") == code]
    assert len(manual) == 1, f"minted code must appear in the author's list: {listed!r}"
    assert manual[0]["provider"] == "manual"
    assert manual[0].get("redeemed_by") is None

    _buyer, buyer_actor = _fresh_buyer(paid_nest)
    redeemed = buyer_actor._ws_call("fauna.payments.claims.redeem", {"code": code})
    assert redeemed.get("tier") == tier, f"{redeemed!r}"

    listed = creator_actor._ws_call("fauna.payments.claims.list", {})
    manual = [c for c in listed.get("claims", []) if c.get("code") == code]
    assert manual[0].get("redeemed_by") is not None, (
        f"the list must reflect the redemption: {manual[0]!r}"
    )

    # An unrelated creator has no visibility into another author's codes
    # (caller-scoped, mirrors providers.list).
    _other, other_actor = _fresh_buyer(paid_nest)
    assert other_actor._ws_call("fauna.payments.claims.list", {}).get("claims", []) == []

    # Minting against a tier the creator doesn't own is rejected.
    with pytest.raises(RpcCallError) as exc:
        creator_actor._ws_call("fauna.payments.claims.mint", {"tier": "no-such-tier"})
    assert exc.value.code == "fauna.payments.tier_not_found"


@pytest.mark.feature("subscriptions-and-tiers")
def test_refund_before_approval_strips_the_payment_marker(paid_nest):
    """A refund landing while the request is still queued demotes it to an
    ordinary manual request (the marker clears; the row survives for the
    creator to judge). Refund/expiry on an ACTIVE subscription is covered
    Rust-side (conformance_payments.rs) — completing a grant needs the
    author-client mint this harness can't run."""
    buyer, _buyer_actor = _fresh_buyer(paid_nest)
    external_ref = f"pay_{uuid.uuid4().hex[:10]}"
    body = _payment_body(reference=buyer["actor_id_hex"], external_ref=external_ref)
    author_hex = paid_nest["creator"]["actor_id_hex"]

    status, reply = _post_webhook(
        paid_nest["url"], author_hex, "fake", body, _sign_fake(FAKE_SECRET, body)
    )
    assert status == 200 and reply.get("queued") is True
    rows = _requests_for(paid_nest, buyer)
    assert len(rows) == 1 and rows[0]["payment_entitled"] is True

    refund = _payment_body(
        reference=buyer["actor_id_hex"], event="refund", external_ref=external_ref
    )
    status, reply = _post_webhook(
        paid_nest["url"], author_hex, "fake", refund, _sign_fake(FAKE_SECRET, refund)
    )
    assert status == 200 and reply.get("status") == "refund_applied", f"{reply!r}"

    rows = _requests_for(paid_nest, buyer)
    assert len(rows) == 1 and rows[0]["payment_entitled"] is False, (
        f"the refund strips the payment marker; the row stays manual: {rows!r}"
    )


def _fresh_provider_creator(paid_nest) -> tuple[dict, ApiActor, str]:
    """A brand-new creator + tier + fake-provider config on the SAME nest,
    isolated from `paid_nest`'s shared row — so the provider-status stamps
    this test asserts on can't be polluted by another test's webhooks
    against the shared creator's row (order-independent, mirrors
    `_fresh_buyer`)."""
    creator = create_actor_and_register(
        paid_nest["port"], base_url=paid_nest["url"], admin_signing_key=paid_nest["admin_sk"]
    )
    creator_actor = ApiActor(
        paid_nest["url"],
        creator["token"],
        creator["actor_id_hex"],
        bytes(creator["signing_key"]),
    )
    tier = "gold"
    reply = creator_actor.subscription_create_tier(
        tier, rank=1, auto_approve=False, price_hint="$5/mo"
    )
    assert reply.get("created") is True, f"tier create failed: {reply!r}"
    secret = f"whsec_{uuid.uuid4().hex}"
    saved = creator_actor._ws_call(
        "fauna.payments.providers.set",
        {"kind": "fake", "webhook_secret": secret, "tier": tier},
    )
    assert saved.get("saved") is True, f"providers.set failed: {saved!r}"
    return creator, creator_actor, secret


def test_provider_status_stamps_at_webhook_ingress(paid_nest):
    """monetization.md § Pillar 3 → "Provider status — evidence-based, no
    ping": the nest stamps `last_verified_at`/`last_rejected_at` on the
    provider row at webhook ingress only, never via an active probe —
    proven end-to-end through `providers.list`."""
    creator, creator_actor, secret = _fresh_provider_creator(paid_nest)
    author_hex = creator["actor_id_hex"]

    def provider_row() -> dict:
        reply = creator_actor._ws_call("fauna.payments.providers.list", {})
        rows = [p for p in reply.get("providers", []) if p["kind"] == "fake"]
        assert len(rows) == 1, f"expected the one configured provider: {reply!r}"
        return rows[0]

    # Freshly configured, no webhook delivery seen yet: no evidence.
    row = provider_row()
    assert row.get("last_verified_at") is None
    assert row.get("last_rejected_at") is None

    # A signature-valid delivery stamps last_verified_at (the exact grant
    # outcome — claim-minted, since this body carries no buyer reference —
    # doesn't matter here; only the ingress-side stamping does).
    body = _payment_body()
    status, _ = _post_webhook(paid_nest["url"], author_hex, "fake", body, _sign_fake(secret, body))
    assert status == 200
    row = provider_row()
    verified_at = row.get("last_verified_at")
    assert verified_at is not None, f"a valid delivery must stamp last_verified_at: {row!r}"
    assert row.get("last_rejected_at") is None

    # A bad-signature delivery against the SAME row stamps last_rejected_at —
    # never the nest actively probing the secret (no ping kind exists).
    status, _ = _post_webhook(paid_nest["url"], author_hex, "fake", body, "0" * 64)
    assert status == 401
    row = provider_row()
    rejected_at = row.get("last_rejected_at")
    assert rejected_at is not None, f"a bad-signature delivery must stamp last_rejected_at: {row!r}"
    assert row.get("last_verified_at") == verified_at, (
        "a rejection must not clear or move the earlier verification stamp"
    )
