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
from common.auth import create_actor_and_register
from helpers.app_surface import skip_unbuilt
from helpers.e2e_session import login_as
from i18n.strings import S

pytestmark = [pytest.mark.tier_3]

FAKE_SECRET = "whsec_e2e_ui_fake_provider"


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


def _payment_body(*, reference: str | None = None, external_ref: str | None = None) -> bytes:
    body = {"id": external_ref or f"pay_{uuid.uuid4().hex[:10]}", "event": "payment"}
    if reference is not None:
        body["reference"] = reference
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


def _gate(app) -> None:
    """The profile Tiers-tab §4 **provider section** landed on linux (lead,
    2026-07-13) + web + android (same day) + apple (2026-07-13, macOS lead,
    iOS shares the same FaunaKit view) + windows (2026-07-16) + tui
    (the tui Tiers author track); widen as the remaining lifts land (priority #1 —
    same ui.yaml IDs everywhere). NOTE: android is CODE-wired +
    compile-verified + Robolectric-tested, but its e2e is host-emulator-gated
    fleet-wide (deselected off the emulator host), so this assertion has not yet been
    observed green for android — it runs once the emulator is stood up.

    ⚠ This gate covers ONLY the §4 provider surface on the profile page. The
    claim-**redeem** input lives on the separate consumer `subscription-settings`
    page — gate that with the action layer's
    `require_consumer_subscriptions_page`. The two were one predicate until
    2026-08-01; splitting them is what let tui run the three provider/mint tests
    it genuinely built while its consumer page was still owed (that page landed
    2026-08-02, so both predicates now pass on all 7 apps — the split stands on
    its own terms, not on tui's gap)."""
    if not (app.driver.is_linux() or app.driver.is_web() or app.driver.is_android()
            or app.driver.is_macos() or app.driver.is_ios() or app.driver.is_windows()
            or app.driver.is_tui()):
        skip_unbuilt(
            app.driver,
            surface="Pillar-3 provider UI (profile Tiers tab §4 "
            "subscription-provider-*)",
            detail="landed on all 7 apps",
            tracked="monetization.md",
        )


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

    Separate from `_gate` on purpose: `_gate` covers the §4 provider section,
    which apple + web + android + windows already ship. Gating both on one
    predicate would skip the whole test on those clients and throw away
    coverage they genuinely have.

    NOTE: android is CODE-wired + compile-verified + Robolectric-tested (same
    as `_gate`'s android note), but its e2e is host-emulator-gated fleet-wide,
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
    _gate(app)
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
    subs.refresh()
    assert subs.wait_for_pending_request(1), (
        f"the payment-entitled request should surface in §2; error={subs.error_text()!r}"
    )
    if _has_new_ids(app):
        assert subs.request_paid(0), (
            "a payment-entitled request should show the paid badge "
            "(PendingRequest.payment_entitled)"
        )
    # The §4 status badge reflects the just-verified delivery evidence
    # (last_verified_at stamped at webhook ingress — provider_status_label).
    assert subs.wait_for_provider_status("Verified", 0), (
        f"a verified webhook delivery should flip the §4 status badge to "
        f"'Verified'; got {subs.provider_status(0)!r}; error={subs.error_text()!r}"
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
def test_provider_webhook_url_preview(app, payments_nest):
    """Author journey: the §4 form shows the exact webhook URL to register at
    the provider's dashboard, live — derived client-side from the nest base
    URL + author actor_id + the selected kind, so it is visible as soon as the
    form opens, before the provider is even saved (no nest round-trip), and
    the creator never hand-assembles it."""
    _gate(app)
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
def test_claim_redeem_via_ui(app, payments_nest):
    """Buyer journey: paste a post-payment claim code on the
    `subscription-settings` page → the queued entitlement renders exactly
    like a queued subscribe (a "pending" My Subscriptions row); a bogus code
    surfaces the typed error via `error-message`."""
    _gate(app)
    # This is the only test in the file that drives the CONSUMER page rather
    # than the author's profile Tiers tab (convention 7: the platform check
    # lives in the action layer).
    app.subscriptions.require_consumer_subscriptions_page()
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
