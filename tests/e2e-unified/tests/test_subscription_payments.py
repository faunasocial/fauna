"""tier_3 e2e for the Pillar-3 payment-provider client legs —
`monetization.md` § Pillars 2+3 — app UX (IDs reserved 2026-07-12).

Two user journeys through real binaries, driven the way a user would
(testing.md rule 8 — mutations through the client UI, never a raw call):

- **Author — provider management** (profile Tiers-tab §4,
  `subscription-provider-*`): the creator adds a payment provider (kind +
  webhook-verification secret + entitled tier) through the form, the row
  renders without echoing the secret, and a *signed webhook then actually
  verifies against the form-entered secret* (the round-trip proof that the
  form saved what the user typed); remove deregisters (the same webhook is
  rejected after).
- **Buyer — claim redemption** (`subscription-settings`,
  `subscription-claim-redeem-*`): an unbound payment mints a claim code
  (delivered in the webhook HTTP response — the provider-side channel); the
  buyer pastes it in their client and the queued entitlement renders exactly
  like a queued subscribe (a "pending" row in My Subscriptions); a bogus code
  surfaces the typed error via `error-message`.

The webhook POSTs are *external black-box* traffic (testing.md rule 8(c) —
the provider's delivery system speaks public HTTP, not a client UI); the
nest-side contract they exercise is pinned in
`tests/api/test_payments_webhook.py`. The buyer-journey fixture configures
the creator's provider over raw WS-RPC as precondition setup — the
client-UI-only equivalent is `test_provider_config_add_and_remove` in this
module (rule 8's citation requirement).

Linux is the lead app; the other 5 lift the same ui.yaml IDs after
(priority #1), at which point these tests run for them too.
"""

import hashlib
import hmac
import json
import urllib.error
import urllib.request
import uuid

import pytest
from nacl.signing import SigningKey

from actions.api_actor import ApiActor
from clients._ws_rpc_core import RpcCallError
from common.auth import create_actor_and_register
from helpers.app_surface import require_surface
from helpers.e2e_session import login_as
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [pytest.mark.tier_3]

FAKE_SECRET = "whsec_e2e_ui_fake_provider"
# Convention-14 named budget for an author-side grant (`test_subscriptions.py`).
AUTHOR_GRANT_BUDGET_S = 90


def _unique(prefix: str) -> str:
    return f"{prefix}-{uuid.uuid4().hex[:8]}"


def _as_signing_key(raw) -> SigningKey:
    """Nests return the admin signing key as either a `SigningKey` or hex."""
    return raw if isinstance(raw, SigningKey) else SigningKey(bytes.fromhex(raw))


def _sign_fake(secret: str, body: bytes) -> str:
    """The fake provider's scheme: hex(HMAC_SHA256(secret, raw_body)) —
    mirrors `libs/fauna-payments/src/fake.rs::sign`."""
    return hmac.new(secret.encode(), body, hashlib.sha256).hexdigest()


def _post_webhook(base_url: str, author_hex: str, provider: str, body: bytes, signature: str):
    """POST the provider webhook; returns (status, parsed-json-or-{})."""
    url = f"{base_url}/api/v1/payments/webhook/{author_hex}/{provider}"
    req = urllib.request.Request(url, data=body, method="POST")
    req.add_header("Content-Type", "application/json")
    req.add_header("X-Fauna-Signature", signature)
    try:
        with urllib.request.urlopen(req, timeout=15) as resp:
            return resp.status, json.loads(resp.read() or b"{}")
    except urllib.error.HTTPError as e:
        return e.code, {}


def _payment_body(
    *, reference: str | None = None, external_ref: str | None = None, product: str | None = None,
) -> bytes:
    """``product`` names what the payment was for, in the provider's own
    reference (the fake provider's `product` body field — on Stripe the
    Payment Link a Checkout Session came through); the per-product tier
    mapping (monetization.md § Pillar 3 → *Configuration*) covers a payment
    only when it names the mapped product."""
    body = {"id": external_ref or f"pay_{uuid.uuid4().hex[:10]}", "event": "payment"}
    if reference is not None:
        body["reference"] = reference
    if product is not None:
        body["product"] = product
    return json.dumps(body).encode()


@pytest.fixture
def payments_nest(request, nest_mode, tmp_path_factory):
    """A fresh nest for this suite (default storage handling — every tier
    created today is client-minted, so no mode commit; see memory/goal on the
    retired storage-mode axis)."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "payments-ui-nest")
    yield nest
    cleanup()


def _login_as(app, nest, user, *, handle: str) -> None:
    """Drive the client into a logged-in session for `user` against `nest`,
    landing on the feed view — the same set_state login `logged_in_app` uses
    (mirrors test_subscriptions.py). Thin wrapper over the shared
    `helpers.e2e_session.login_as` — see that docstring for the barrier
    rationale (2026-08-10)."""
    login_as(app, nest, user, handle=handle, device_id="test-device-payments")


def _has_new_ids(app) -> bool:
    """Whether this client has lifted the three ID sets added 2026-07-13
    (`subscription-provider-form-webhook-url` + copy button, the §2
    `subscription-request-paid-badge`, and the §5 `subscription-claim-*`
    section). linux (lead) + web + android + windows (2026-07-16) + tui
    (the tui Tiers author track) + apple (verified 2026-08-16 — macOS + iOS share
    the same FaunaKit `ProfileView`/`SubscriptionSettingsView`, all three ID
    sets already wired: `ProfileView.swift:560-654` for the webhook-url +
    claim section, `:699` for the paid badge, `SubscriptionSettingsView.swift:
    110-114` for the redeem input) so far — all 7 apps now covered (priority
    #1 — the same ui.yaml IDs everywhere).

    NOTE: android is CODE-wired + compile-verified + Robolectric-tested, but
    its e2e is host-emulator-gated fleet-wide,
    so this predicate has not yet been observed green for android — it runs
    once the emulator is stood up on the emulator host."""
    return (app.driver.is_linux() or app.driver.is_web() or app.driver.is_android()
            or app.driver.is_windows() or app.driver.is_tui()
            or app.driver.is_macos() or app.driver.is_ios())


def _gate_new_ids(app) -> None:
    if not _has_new_ids(app):
        pytest.skip(
            "the 2026-07-13 payment ID sets "
            "(subscription-provider-form-webhook-url, "
            "subscription-request-paid-badge, subscription-claim-*) have "
            "landed on linux + web + android + windows + apple; widen "
            "`_has_new_ids` as each remaining client lifts them."
        )


@pytest.mark.feature("subscriptions-and-tiers")
def test_provider_config_add_and_remove(app, payments_nest):
    """Author journey: add a provider through the §4 form; the row renders
    kind + status without the secret; a signed webhook verifies against the
    form-entered secret (the save round-trip proof); remove deregisters."""
    admin_sk = _as_signing_key(payments_nest["admin"]["signing_key"])
    author = create_actor_and_register(
        payments_nest["port"], base_url=payments_nest["url"], admin_signing_key=admin_sk,
    )
    buyer = create_actor_and_register(
        payments_nest["port"], base_url=payments_nest["url"], admin_signing_key=admin_sk,
    )

    _login_as(app, payments_nest, author, handle="e2e-provider-author")
    subs = app.subscriptions
    tier = _unique("gold")

    # ── The provider form needs a tier to map to (created through the UI). ──
    subs.navigate()
    subs.open_tiers_tab()
    subs.create_tier(tier, rank=1, price_hint="$5/mo")
    assert subs.wait_for_tier(tier), (
        f"created tier {tier!r} should appear in §1; error={subs.error_text()!r}"
    )

    # ── §4: add the provider through the form. ──────────────────────────────
    subs.add_provider("fake", FAKE_SECRET, tier)
    assert subs.wait_for_provider("fake"), (
        f"the configured provider should appear in §4; error={subs.error_text()!r}"
    )
    # Evidence-based status (fauna_core::format::provider_status_label): a
    # fresh row with no delivery evidence yet renders "Configured".
    assert subs.provider_status(0) == "Configured"

    # ── Round-trip proof: a webhook signed with the FORM-entered secret ──────
    # verifies, and the entitlement enqueues in §2 exactly like a manual
    # subscribe request (marked paid — `PendingRequest.payment_entitled`).
    body = _payment_body(reference=buyer["actor_id_hex"])
    status, reply = _post_webhook(
        payments_nest["url"], author["actor_id_hex"], "fake", body,
        _sign_fake(FAKE_SECRET, body),
    )
    assert status == 200 and reply.get("queued") is True, (
        f"a webhook signed with the form-entered secret must verify: {status} {reply!r}"
    )
    # The author's drain grants a payment-entitled row with no judgment, and its
    # poll races this read (30 s native, 2 s on web under the e2e agent): the row
    # standing in §2 is not latency-independent (convention 14). So the wait ends
    # on the row OR on the grant it turns into; only a row that stands has a badge
    # to read, and one that left while being read is not a missing badge.
    buyer_api = ApiActor(
        payments_nest["url"], buyer["token"], buyer["actor_id_hex"],
        bytes(buyer["signing_key"]),
    )

    def buyer_granted() -> bool:
        try:
            return buyer_api.subscription_key_blob(
                bytes.fromhex(author["actor_id_hex"]), tier,
            )["version"] >= 1
        except RpcCallError:
            return False

    def row_or_grant() -> bool:
        subs.refresh()
        return subs.pending_request_count() >= 1 or buyer_granted()

    wait_until(
        row_or_grant, AUTHOR_GRANT_BUDGET_S, interval=1,
        diagnose=lambda: f"no §2 row and no grant; error={subs.error_text()!r}",
    )
    # The paid badge on a standing row is witnessed, held, by
    # `test_a_payment_backed_request_shows_the_paid_badge` below: here the pump
    # is free to grant the row, so no badge reading would be deterministic.
    # The §4 status badge reflects the just-verified delivery evidence
    # (last_verified_at stamped at webhook ingress — provider_status_label).
    assert subs.wait_for_provider_status("Verified", 0), (
        f"a verified webhook delivery should flip the §4 status badge to "
        f"'Verified'; got {subs.provider_status(0)!r}; error={subs.error_text()!r}"
    )

    # ── A badly signed delivery flips the badge to "Error". ─────────────────
    # The nest refuses it non-2xx and stamps `last_rejected_at`; a rejection
    # wins unless a STRICTLY later verification follows it, and a same-second
    # tie resolves to error (`provider_status_label`), so this reading does
    # not depend on how close it lands to the verified delivery above. The
    # row reflects the stamp only once the page re-reads, so each poll
    # refreshes; the wait ends on the state, never a fixed delay
    # (convention 14).
    bad_body = _payment_body(reference=buyer["actor_id_hex"])
    status, _ = _post_webhook(
        payments_nest["url"], author["actor_id_hex"], "fake", bad_body, "0" * 64,
    )
    assert not 200 <= status < 300, (
        f"a webhook with a bad signature must be refused non-2xx, got {status}"
    )

    def provider_reads_error() -> bool:
        subs.refresh()
        return subs.provider_status(0) == "Error"

    wait_until(
        provider_reads_error, AUTHOR_GRANT_BUDGET_S, interval=1,
        diagnose=lambda: (
            f"a rejected webhook delivery should flip the §4 status badge to "
            f"'Error'; got {subs.provider_status(0)!r}; error={subs.error_text()!r}"
        ),
    )

    # ── Remove deregisters: the row goes, and the same webhook now 404s. ────
    subs.remove_first_provider()
    assert subs.wait_for_provider_gone("fake"), (
        f"removed provider should leave §4; error={subs.error_text()!r}"
    )
    body2 = _payment_body(reference=buyer["actor_id_hex"])
    status, _ = _post_webhook(
        payments_nest["url"], author["actor_id_hex"], "fake", body2,
        _sign_fake(FAKE_SECRET, body2),
    )
    assert status == 404, f"webhook to a removed provider must be non-2xx, got {status}"


@pytest.mark.feature("subscriptions-and-tiers")
def test_provider_product_mapping_covers_only_its_own_payments(app, payments_nest):
    """Author journey for the per-product tier mapping (monetization.md
    § Pillar 3 → *Configuration*, ruled 2026-11-05): the creator fills the
    §4 form's optional product field (`subscription-provider-form-product` —
    on Stripe the Payment Link id) and saves; `providers.list` carries it and
    the re-opened form pre-fills it (not a secret). Then two signed webhooks:
    a payment for ANOTHER product is acknowledged 2xx (so the provider never
    retries it) and mints nothing — `{"status":"ignored","reason":
    "product_not_mapped"}` — while a payment naming the mapped product mints
    a claim as before."""
    admin_sk = _as_signing_key(payments_nest["admin"]["signing_key"])
    author = create_actor_and_register(
        payments_nest["port"], base_url=payments_nest["url"], admin_signing_key=admin_sk,
    )

    _login_as(app, payments_nest, author, handle="e2e-product-author")
    subs = app.subscriptions
    tier = _unique("gold")
    product = f"plink_{uuid.uuid4().hex[:8]}"

    subs.navigate()
    subs.open_tiers_tab()
    subs.create_tier(tier, rank=1, price_hint="$5/mo")
    assert subs.wait_for_tier(tier), (
        f"created tier {tier!r} should appear in §1; error={subs.error_text()!r}"
    )

    # ── §4: add the provider WITH the product, through the form. ───────────
    subs.add_provider("fake", FAKE_SECRET, tier, product=product)
    assert subs.wait_for_provider("fake"), (
        f"the configured provider should appear in §4; error={subs.error_text()!r}"
    )

    # The nest holds the product (the wire's `product_ref`), readable by the
    # author — the proof the form sent what it showed.
    author_api = ApiActor(
        payments_nest["url"], author["token"], author["actor_id_hex"],
        bytes(author["signing_key"]),
    )
    listed = author_api._ws_call("fauna.payments.providers.list", {})
    rows = listed.get("providers") or []
    assert [r.get("product_ref") for r in rows if r.get("kind") == "fake"] == [product], (
        f"providers.list must carry the product the form saved: {listed!r}"
    )

    # Re-opening the form for the configured kind pre-fills the saved product
    # (it is not a secret — the secret stays blank).
    subs.open_provider_form("fake")
    wait_until(
        lambda: subs.provider_form_product() == product, 10.0, interval=0.5,
        diagnose=lambda: (
            f"the re-opened form should pre-fill the saved product {product!r}; "
            f"got {subs.provider_form_product()!r}; error={subs.error_text()!r}"
        ),
    )
    subs.close_provider_form()

    # ── A payment for another product (a tip link on the same account): ─────
    # acknowledged, not retried, nothing minted.
    other = _payment_body(product="plink_tips")
    status, reply = _post_webhook(
        payments_nest["url"], author["actor_id_hex"], "fake", other,
        _sign_fake(FAKE_SECRET, other),
    )
    assert status == 200, f"an unmapped product is acknowledged 2xx, got {status} {reply!r}"
    assert reply.get("status") == "ignored" and reply.get("reason") == "product_not_mapped", (
        f"an unmapped product mints nothing: {reply!r}"
    )

    # ── A payment naming the mapped product mints a claim as before. ────────
    mapped = _payment_body(product=product)
    status, reply = _post_webhook(
        payments_nest["url"], author["actor_id_hex"], "fake", mapped,
        _sign_fake(FAKE_SECRET, mapped),
    )
    assert status == 200 and reply.get("status") == "claim_minted", (
        f"the mapped product mints: {status} {reply!r}"
    )
    assert reply.get("claim_code"), f"claim_minted must carry the code: {reply!r}"


@pytest.mark.feature("subscriptions-and-tiers")
def test_a_payment_backed_request_shows_the_paid_badge(app, payments_nest):
    """Author journey: a verified payment's subscribe request stands in §2 with
    the paid badge (`subscription-request-paid-badge`, rendering
    `PendingRequest.payment_entitled` — monetization.md § Pillar 3).

    The author's own pump grants a payment-backed row with no judgment on its
    next tick, so the row is read with the drain HELD (the agent's
    `subscriptions_hold_author_drain`, checked after the pump's queue read):
    every row enqueued after the hold stands until the release, so the badge
    assertion is unconditional and no pump timing decides it (convention 14).
    Released, the same row is granted — the hold kept a real paid request, not
    an inert one."""
    require_surface(app.driver, "author-drain-hold")
    admin_sk = _as_signing_key(payments_nest["admin"]["signing_key"])
    author = create_actor_and_register(
        payments_nest["port"], base_url=payments_nest["url"], admin_signing_key=admin_sk,
    )
    buyer = create_actor_and_register(
        payments_nest["port"], base_url=payments_nest["url"], admin_signing_key=admin_sk,
    )

    _login_as(app, payments_nest, author, handle="e2e-paid-badge-author")
    subs = app.subscriptions
    tier = _unique("gold")

    subs.navigate()
    subs.open_tiers_tab()
    subs.create_tier(tier, rank=1, price_hint="$5/mo")
    assert subs.wait_for_tier(tier), (
        f"created tier {tier!r} should appear in §1; error={subs.error_text()!r}"
    )
    subs.add_provider("fake", FAKE_SECRET, tier)
    assert subs.wait_for_provider("fake"), (
        f"the configured provider should appear in §4; error={subs.error_text()!r}"
    )
    assert subs.pending_request_count() == 0, "no request may stand before the payment"

    subs.hold_author_drain()
    try:
        body = _payment_body(reference=buyer["actor_id_hex"])
        status, reply = _post_webhook(
            payments_nest["url"], author["actor_id_hex"], "fake", body,
            _sign_fake(FAKE_SECRET, body),
        )
        assert status == 200 and reply.get("queued") is True, (
            f"a webhook signed with the form-entered secret must verify: {status} {reply!r}"
        )

        def row_stands() -> bool:
            subs.refresh()
            return subs.pending_request_count() >= 1

        wait_until(
            row_stands, AUTHOR_GRANT_BUDGET_S, interval=1,
            diagnose=lambda: (
                f"the paid request never reached §2 with the drain held; "
                f"error={subs.error_text()!r}"
            ),
        )
        assert subs.pending_request_count() == 1, (
            f"exactly the one paid request should stand; "
            f"got {subs.pending_request_count()}"
        )
        assert subs.request_paid(0), (
            "a payment-backed subscribe request must show the paid badge "
            f"(PendingRequest.payment_entitled); error={subs.error_text()!r}"
        )
    finally:
        subs.release_author_drain()

    buyer_api = ApiActor(
        payments_nest["url"], buyer["token"], buyer["actor_id_hex"],
        bytes(buyer["signing_key"]),
    )

    def buyer_granted() -> bool:
        try:
            return buyer_api.subscription_key_blob(
                bytes.fromhex(author["actor_id_hex"]), tier,
            )["version"] >= 1
        except RpcCallError:
            return False

    wait_until(
        buyer_granted, AUTHOR_GRANT_BUDGET_S, interval=1,
        diagnose=lambda: (
            f"released, the author's pump should grant the paid request; "
            f"error={subs.error_text()!r}"
        ),
    )


@pytest.mark.feature("subscriptions-and-tiers")
def test_provider_webhook_url_preview(app, payments_nest):
    """Author journey: the §4 form shows the exact webhook URL to register at
    the provider's dashboard, live — derived client-side from the nest base
    URL + author actor_id + the selected kind, so it is visible as soon as the
    form opens, before the provider is even saved (no nest round-trip), and
    the creator never hand-assembles it."""
    _gate_new_ids(app)
    admin_sk = _as_signing_key(payments_nest["admin"]["signing_key"])
    author = create_actor_and_register(
        payments_nest["port"], base_url=payments_nest["url"], admin_signing_key=admin_sk,
    )

    _login_as(app, payments_nest, author, handle="e2e-webhook-url-author")
    subs = app.subscriptions
    subs.navigate()
    subs.open_tiers_tab()

    subs.open_provider_form("fake")
    webhook_url = subs.provider_webhook_url()
    expected_path = f"/api/v1/payments/webhook/{author['actor_id_hex']}/fake"
    assert webhook_url.endswith(expected_path), (
        f"webhook URL should end with {expected_path!r}, got {webhook_url!r}"
    )
    assert subs.driver.is_visible("subscription-provider-form-webhook-url-copy-button"), (
        "the webhook URL should be copyable"
    )
    subs.close_provider_form()


@pytest.mark.feature("subscriptions-and-tiers")
def test_provider_return_url_display(web_hosting_app, web_hosting_nest):
    """Author journey: with a public web address the §4 form shows the exact
    paywall return URL to paste into the provider's post-checkout redirect —
    built on the creator's WEB-SITE origin (the subdomain the web-settings page
    shows), not the nest URL the webhook display uses, and ending in the
    selected provider's own placeholder (`{CHECKOUT_SESSION_ID}` for stripe,
    `{REF}` otherwise). Compared as the full string per kind."""
    app = web_hosting_app
    from conftest import WEB_HOSTING_DOMAIN

    web = app.web_settings
    web.navigate()
    web.set_subdomain_enabled(True)
    handle = web_hosting_nest["user"]["handle"]
    author_hex = web_hosting_nest["user"]["actor_id_hex"]
    origin = f"https://{handle}.{WEB_HOSTING_DOMAIN}"

    subs = app.subscriptions
    subs.navigate()
    subs.open_tiers_tab()
    for kind, placeholder in (("fake", "{REF}"), ("stripe", "{CHECKOUT_SESSION_ID}")):
        subs.open_provider_form(kind)
        # The origin hydrates asynchronously on form open — wait for the URL,
        # never a fixed delay (convention 14).
        expected = f"{origin}/api/v1/paywall/return/{kind}?ref={placeholder}"
        shown = wait_until(
            lambda: subs.provider_return_url() if subs.provider_return_url() == expected else None,
            10,
            diagnose=lambda: (
                f"return URL for {kind!r} never became {expected!r}; "
                f"got {subs.provider_return_url()!r}; error={app.error_text()!r}"
            ),
        )
        assert shown == expected
        assert subs.driver.is_visible("subscription-provider-form-return-url-copy-button"), (
            "the return URL should be copyable"
        )
        # Beside it on the same form: the webhook address for the same kind.
        webhook_url = subs.provider_webhook_url()
        webhook_path = f"/api/v1/payments/webhook/{author_hex}/{kind}"
        assert webhook_url.endswith(webhook_path), (
            f"the {kind!r} form should show the webhook address beside the return "
            f"URL; got {webhook_url!r}"
        )
        subs.close_provider_form()


@pytest.mark.feature("subscriptions-and-tiers")
def test_claim_redeem_via_ui(app, payments_nest):
    """Buyer journey: paste a post-payment claim code on the
    `subscription-settings` page → the queued entitlement renders exactly
    like a queued subscribe (a "pending" My Subscriptions row); a bogus code
    surfaces the typed error via `error-message`."""
    admin_sk = _as_signing_key(payments_nest["admin"]["signing_key"])
    creator = create_actor_and_register(
        payments_nest["port"], base_url=payments_nest["url"], admin_signing_key=admin_sk,
    )
    buyer = create_actor_and_register(
        payments_nest["port"], base_url=payments_nest["url"], admin_signing_key=admin_sk,
    )

    # ── Precondition setup (headless creator): tier + provider config over ──
    # raw WS-RPC. The client-UI-only equivalent of this mutation is
    # `test_provider_config_add_and_remove` above (testing.md rule 8 citation).
    creator_actor = ApiActor(
        payments_nest["url"], creator["token"], creator["actor_id_hex"],
        bytes(creator["signing_key"]),
    )
    tier = _unique("silver")
    reply = creator_actor.subscription_create_tier(tier, rank=1, price_hint="$3/mo")
    assert reply.get("created") is True, f"tier create failed: {reply!r}"
    saved = creator_actor._ws_call(
        "fauna.payments.providers.set",
        {"kind": "fake", "webhook_secret": FAKE_SECRET, "tier": tier},
    )
    assert saved.get("saved") is True, f"providers.set failed: {saved!r}"

    # ── An unbound payment mints a claim code (webhook = provider traffic; ──
    # the code rides the HTTP response — its only delivery channel today).
    body = _payment_body()
    status, reply = _post_webhook(
        payments_nest["url"], creator["actor_id_hex"], "fake", body,
        _sign_fake(FAKE_SECRET, body),
    )
    assert status == 200 and reply.get("status") == "claim_minted", f"{status} {reply!r}"
    code = reply.get("claim_code")
    assert code, f"claim_minted must carry the code: {reply!r}"

    # ── Buyer: a bogus code surfaces the typed error via error-message. ─────
    _login_as(app, payments_nest, buyer, handle="e2e-claim-buyer")
    subs = app.subscriptions
    subs.navigate_settings()
    subs.redeem_claim("NOSUCH1234")
    err = subs.wait_for_error()
    assert err, "a bogus claim code must surface fauna.payments.claim_not_found"

    # ── Buyer: the real code binds their actor; the queued entitlement ──────
    # renders exactly like a queued subscribe — a "pending" row.
    subs.redeem_claim(code)
    assert subs.wait_for_mine_subscription(tier), (
        f"the redeemed entitlement should appear in My Subscriptions; "
        f"error={subs.error_text()!r}"
    )
    idx = subs.mine_tiers().index(tier)
    assert subs.mine_status(idx) == "pending", (
        "a redeemed claim on a client-minted tier queues for the creator's "
        f"drain pump — the row must render pending, got {subs.mine_status(idx)!r}"
    )

    # ── The redeem actually bound the buyer (nest-side check): the creator ──
    # sees the payment-marked request in their queue.
    rows = creator_actor._ws_call("fauna.subscriptions.requests.list", {}).get("requests", [])
    want = bytes.fromhex(buyer["actor_id_hex"])
    mine = [r for r in rows if r.get("subscriber_id") == want]
    assert len(mine) == 1 and mine[0]["payment_entitled"] is True, (
        f"the redeemed claim should enqueue a payment-marked request: {mine!r}"
    )


@pytest.mark.feature("subscriptions-and-tiers")
def test_manual_claim_mint_and_list(app, payments_nest):
    """Author journey (clients that have lifted §5 — see `_has_new_ids`):
    mint a manual claim code for a tier through the §5 form; the code lists
    as "unredeemed". Once redeemed, the same row flips to "redeemed" — the
    audit-surface proof (monetization.md § Pillar 3: claims.list is the
    audit surface for BOTH manually- and webhook-minted codes). The
    buyer-side redeem UI is already covered by test_claim_redeem_via_ui
    above; here the redeem is driven over raw WS-RPC as a precondition for
    the audit-list assertion (testing.md rule 8 citation:
    test_claim_redeem_via_ui)."""
    _gate_new_ids(app)
    admin_sk = _as_signing_key(payments_nest["admin"]["signing_key"])
    author = create_actor_and_register(
        payments_nest["port"], base_url=payments_nest["url"], admin_signing_key=admin_sk,
    )
    buyer = create_actor_and_register(
        payments_nest["port"], base_url=payments_nest["url"], admin_signing_key=admin_sk,
    )

    _login_as(app, payments_nest, author, handle="e2e-claim-mint-author")
    subs = app.subscriptions
    tier = _unique("bronze")
    subs.navigate()
    subs.open_tiers_tab()
    subs.create_tier(tier, rank=1, price_hint="$1/mo")
    assert subs.wait_for_tier(tier), (
        f"created tier {tier!r} should appear in §1; error={subs.error_text()!r}"
    )

    # ── §5: mint a manual claim code for the tier through the UI. ───────────
    subs.mint_claim(tier)
    assert subs.wait_for_claim_count(1), (
        f"a minted claim should appear in §5; error={subs.error_text()!r}"
    )
    code = subs.claim_codes()[0]
    # The row COUNT above cannot tell a rendered code from a blank placeholder:
    # a client painting `subscription-claim-code` with an empty string still
    # yields one row, and `wait_for_claim_count(1)` passes. Found by mutation on
    # tui 2026-08-01 — blanking the leaf reddened this test only ~8 lines later,
    # at the redeem below (`fauna.payments.malformed`), which diagnoses the nest
    # rather than the surface that actually broke.
    assert code, (
        "§5 rendered a claim row with an EMPTY subscription-claim-code; "
        f"claim_codes()={subs.claim_codes()!r}"
    )
    assert subs.claim_tiers()[0] == tier
    assert subs.claim_status(0) == S.subscriptions.claim_status_unredeemed, (
        f"a fresh claim should be unredeemed, got {subs.claim_status(0)!r}"
    )

    # ── Redeem over raw WS-RPC as a precondition (the buyer-side UI redeem ──
    # flow is already covered by test_claim_redeem_via_ui above).
    buyer_actor = ApiActor(
        payments_nest["url"], buyer["token"], buyer["actor_id_hex"], bytes(buyer["signing_key"]),
    )
    redeemed = buyer_actor._ws_call("fauna.payments.claims.redeem", {"code": code})
    assert redeemed.get("queued") is True, f"redeem failed: {redeemed!r}"

    # ── The §5 audit list re-reads and shows the code as redeemed — the ─────
    # claims.list audit surface covers BOTH manual and webhook-minted codes.
    subs.refresh()
    assert subs.claim_status(0) == S.subscriptions.claim_status_redeemed, (
        f"a redeemed claim should flip status, got {subs.claim_status(0)!r}"
    )


@pytest.mark.feature("subscriptions-and-tiers")
def test_manual_claim_void(app, payments_nest):
    """Author journey: void a minted code from its §5 row
    (`subscription-claim-void-button`, `fauna.payments.claims.void` —
    monetization.md § Pillar 3 → *Buyer↔actor binding (Q4)*). The first press
    only arms the confirm in place — the same button now names the consequence
    and the code still works; the second press voids. The row then reads
    "voided" and carries no Void, and a buyer's redeem of the code is refused
    (`fauna.payments.claim_voided`) — the proof the press reached the nest,
    not just the paint. The redeem is raw WS-RPC because it is the refusal
    under test, not a buyer journey (testing.md rule 8 citation:
    test_claim_redeem_via_ui covers the buyer's UI redeem)."""
    from clients.ws_rpc_admin_client import RpcCallError

    _gate_new_ids(app)
    require_surface(app.driver, "subscription-claim-void-button")
    admin_sk = _as_signing_key(payments_nest["admin"]["signing_key"])
    author = create_actor_and_register(
        payments_nest["port"], base_url=payments_nest["url"], admin_signing_key=admin_sk,
    )
    buyer = create_actor_and_register(
        payments_nest["port"], base_url=payments_nest["url"], admin_signing_key=admin_sk,
    )

    _login_as(app, payments_nest, author, handle="e2e-claim-void-author")
    subs = app.subscriptions
    tier = _unique("bronze")
    subs.navigate()
    subs.open_tiers_tab()
    subs.create_tier(tier, rank=1, price_hint="$1/mo")
    assert subs.wait_for_tier(tier), (
        f"created tier {tier!r} should appear in §1; error={subs.error_text()!r}"
    )
    subs.mint_claim(tier)
    assert subs.wait_for_claim_count(1), (
        f"a minted claim should appear in §5; error={subs.error_text()!r}"
    )
    code = subs.claim_codes()[0]
    assert code, f"§5 rendered an EMPTY claim code: {subs.claim_codes()!r}"
    assert subs.claim_void_text(0) == S.subscriptions.void_claim, (
        f"an unredeemed row should offer Void, got {subs.claim_void_text(0)!r}"
    )

    # ── First press arms in place: the consequence is named, nothing sent. ──
    subs.arm_claim_void(0)
    wait_until(
        lambda: subs.claim_void_text(0) == S.subscriptions.void_claim_confirm_unredeemed,
        10.0,
        diagnose=lambda: f"the armed Void should name the consequence, "
        f"got {subs.claim_void_text(0)!r}",
    )
    assert subs.claim_status(0) == S.subscriptions.claim_status_unredeemed, (
        "the arming press must not void the code"
    )

    # ── Second press voids; the re-read paints the row voided, no Void. ─────
    subs.confirm_claim_void(0)
    assert subs.wait_for_claim_status(S.subscriptions.claim_status_voided, 0), (
        f"the voided code should read voided, got {subs.claim_status(0)!r}; "
        f"error={subs.error_text()!r}"
    )
    assert not subs.has_claim_void_button(0), "a voided row has nothing left to void"
    assert subs.claim_codes()[0] == code

    # ── The nest refuses the voided code to a buyer. ────────────────────────
    buyer_actor = ApiActor(
        payments_nest["url"], buyer["token"], buyer["actor_id_hex"], bytes(buyer["signing_key"]),
    )
    with pytest.raises(RpcCallError) as refused:
        buyer_actor._ws_call("fauna.payments.claims.redeem", {"code": code})
    assert refused.value.code == "fauna.payments.claim_voided", (
        f"a voided code must be refused as voided, got {refused.value.code!r}"
    )


@pytest.mark.feature("subscriptions-and-tiers")
def test_the_payment_link_subscribes_first_and_binds_the_checkout(
    app, payments_nest, opened_urls,
):
    """Buyer journey (``monetization.md`` § Pillar 3 → *Buyer↔actor binding
    (Q4)* → *A checkout-bound buyer homed on another nest*, the Q13 rules (1)
    and (2)): the storefront's ``subscription-offer-payment-link`` sends the
    buyer's subscribe intent FIRST and then opens the checkout, whose URL
    carries the buyer's actor id as ``client_reference_id`` on a Stripe host.
    A signed provider webhook naming that same reference then marks the ONE
    pending request paid — the binding with no claim code pasted.

    The author is headless (an orphan tier is fine: what is asserted is the
    request row the payment completes, not a minted KeyBlob — the drain of a
    paid row is ``test_a_payment_backed_request_shows_the_paid_badge``'s).
    The opened URL is read from the session's opener record (``opened_urls``),
    never a browser."""
    require_surface(app.driver, "payment-link-subscribes-first")
    app.profile.require_other_profile_offers_browse_supported()
    admin_sk = _as_signing_key(payments_nest["admin"]["signing_key"])
    author = create_actor_and_register(
        payments_nest["port"], base_url=payments_nest["url"], admin_signing_key=admin_sk,
    )
    buyer = create_actor_and_register(
        payments_nest["port"], base_url=payments_nest["url"], admin_signing_key=admin_sk,
    )
    author_actor = ApiActor(
        payments_nest["url"], author["token"], author["actor_id_hex"],
        bytes(author["signing_key"]),
    )
    tier = _unique("gold")
    checkout = f"https://buy.stripe.com/test_{uuid.uuid4().hex[:10]}"
    author_actor.subscription_create_tier(
        tier, rank=1, price_hint="$5/mo", payment_url=checkout,
    )
    saved = author_actor._ws_call(
        "fauna.payments.providers.set",
        {"kind": "fake", "webhook_secret": FAKE_SECRET, "tier": tier},
    )
    assert saved.get("saved") is True, f"providers.set failed: {saved!r}"

    buyer_actor = ApiActor(
        payments_nest["url"], buyer["token"], buyer["actor_id_hex"],
        bytes(buyer["signing_key"]),
    )
    buyer_actor.accept_knock(author["actor_id_hex"])
    _login_as(app, payments_nest, buyer, handle="e2e-paylink-buyer")
    app.contacts.open_contact_profile(author["actor_id_hex"])
    app.driver.wait_for("profile-view", timeout=10.0)
    app.profile.open_tiers_tab()
    assert app.profile.wait_for_offer(tier), (
        f"the author's tier {tier!r} should render in the offers section; "
        f"error={app.profile.error_text()!r}"
    )

    opened_before = len(opened_urls())
    app.profile.open_offer_payment_link(0)

    want_url = f"{checkout}?client_reference_id={buyer['actor_id_hex']}"
    wait_until(
        lambda: want_url in opened_urls()[opened_before:], 30, interval=0.5,
        diagnose=lambda: (
            f"the payment link should open {want_url!r}; opened since the tap: "
            f"{opened_urls()[opened_before:]!r}; error={app.profile.error_text()!r}"
        ),
    )

    def buyer_rows() -> list:
        want = bytes.fromhex(buyer["actor_id_hex"])
        return [r for r in author_actor.subscription_requests_list()
                if bytes(r["subscriber_id"]) == want]

    # Rule (1): the checkout opens only after the subscribe returned, so the
    # pending request is already on the author's nest — a causal barrier, no
    # wait (convention 14).
    rows = buyer_rows()
    assert len(rows) == 1 and not rows[0].get("payment_entitled"), (
        f"the tap must leave one unpaid pending request for the buyer: {rows!r}"
    )

    # The provider reports the payment carrying the reference the opened URL
    # handed it — the same row turns paid; no second row, no claim code.
    body = _payment_body(reference=buyer["actor_id_hex"])
    status, reply = _post_webhook(
        payments_nest["url"], author["actor_id_hex"], "fake", body,
        _sign_fake(FAKE_SECRET, body),
    )
    assert status == 200 and reply.get("queued") is True, (
        f"the signed webhook must verify and bind: {status} {reply!r}"
    )
    assert not reply.get("claim_code"), f"a bound payment mints no claim code: {reply!r}"
    rows = buyer_rows()
    assert len(rows) == 1 and rows[0].get("payment_entitled") is True, (
        f"the payment must complete the app's own request row: {rows!r}"
    )
