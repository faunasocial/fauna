"""Web paywall (monetization.md § Pillar 2) — the full nest-side serving
contract on a real `fauna-nest` binary.

The tier_3 build-success bar for the web-paywall track: a creator gates a post
to a paid tier AND web-publishes it; a browser visitor with no token gets the
pre-rendered public **teaser** page (preview + tier + price + payment link); a
visitor presenting a valid short-lived signed **capability URL** gets the full
rendered post, served from the **sealed** `web_rendered_sealed` class and
unsealed at serve time by the web-serve holder under the creator's live
capability grant; a tampered/garbage token gets the teaser (expired-token
rejection is unit-pinned in `web_content::token`); **revoking the grant
darkens the paywalled slice** (the mint/revoke handlers refresh the in-process
holder registry and re-render before replying) while ungated public web
content keeps serving. No `Set-Cookie` ever appears (web-content-hosting.md
invariant 2), and nothing renders per-request (rule 1 — the pages are
pre-rendered; the gate is a stateless signature check).

Production data flow asserted end-to-end:

  creator mints tier (`fauna.subscriptions.tiers.create` w/ price_hint +
  payment_url) → builds a signed gated post via the shared
  `fauna_client_core::post::build_gated_post` seal helper (preview public,
  full body sealed under `derive_post_key(period_key, seal_id)`) → uploads
  the sealed blob (`POST /api/v1/blob`) → `fauna.posts.create` →
  `fauna.web.publish.set` (render: teaser only — no grant yet) → discovers
  the web-serve holder (`fauna.bridges.fetch_bridge_pubkey`
  role=content-processor) → mints it a `content.read{post:tier}` grant
  (`fauna.capabilities.mint`; X-Wing wrap to the holder's published ek) →
  the nest re-renders: the sealed full page materializes →
  `fauna.web.paywall.mint_token` → `GET /post/{slug}.html?token=…` serves
  the unsealed full page → `fauna.capabilities.revoke` → the same token
  serves the teaser again; the ungated sibling post is untouched.

The fixture mint here is the client-side operation the future creator-client
capability/compose surface performs in production — a fixture precondition per
the E2E rules' setup carve-out (b); the mutation under test is the SERVING
behavior, which is exactly what a browser visitor exercises.
"""

import hashlib
import hmac
import json
import time
import urllib.error
import urllib.parse
import urllib.request

import pytest

import fauna_ffi
from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient
from common.auth import create_actor_and_register
from helpers.blob_upload import SEALED_MIME, upload_blob
from tests.api import ws_api

pytestmark = pytest.mark.tier_3

TIER = "gold"
PRICE = "5 EUR / month"
PAY_URL = "https://pay.example/gold"
PREVIEW = "Premium Post\n\nA public teaser paragraph."
FULL_MARKER = "the-full-premium-content-marker"
FULL_BODY = f"Premium Post\n\nHere is {FULL_MARKER} with **bold** depth."
UNGATED_MARKER = "plain-public-post-marker"


def _upload_sealed_blob(port: int, token: str, data: bytes) -> str:
    """`POST /api/v1/blob` for an AEAD-sealed period-restricted-post blob — the
    shape `seal_for_audience` produces and the strict blob verifier expects
    (sealed class ⇒ sidecar MIME is octet-stream; the real MIME rides inside the
    seal). Returns the hex hash.

    The multipart assembly lives in `helpers/blob_upload.py`, the one writer for
    this wire shape — this file used to carry its own copy of it.
    """
    return upload_blob(
        port,
        token,
        data,
        audience_class="PeriodRestrictedPost",
        mime=SEALED_MIME,
    )


def _get(url: str, path: str) -> tuple[int, str, dict]:
    """GET `path` on the nest, returning (status, body_text, headers)."""
    req = urllib.request.Request(url.rstrip("/") + path, method="GET")
    try:
        resp = urllib.request.urlopen(req)
        return resp.status, resp.read().decode("utf-8", "replace"), dict(resp.headers)
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode("utf-8", "replace"), dict(e.headers)


def _actor_client(url: str, actor: dict) -> WsRpcAdminClient:
    return WsRpcAdminClient(
        url, actor_id=actor["actor_id_bytes"], signing_key=bytes(actor["signing_key"])
    )


def _assert_no_cookie(headers: dict, where: str) -> None:
    assert not any(k.lower() == "set-cookie" for k in headers), (
        f"no auth/session cookie may ever be set on the web-serving surface "
        f"(invariant 2), got Set-Cookie at {where}: {headers}"
    )


@pytest.mark.feature("paid-posts-and-tips")
def test_web_paywall_teaser_token_and_revoke(nest_instance):
    url = nest_instance["url"]
    port = nest_instance["port"]
    admin = nest_instance["admin"]
    admin_sk = admin["signing_key"]

    creator = create_actor_and_register(port, admin_signing_key=admin_sk)
    creator_id: bytes = bytes(creator["actor_id_bytes"])
    creator_secret = bytes(creator["signing_key"])
    now = int(time.time())

    # ── 1. The creator defines the paid tier (price + payment link feed the
    # teaser's paywall box) and authors one gated + one ungated post. ──
    period_key = bytes(range(32))
    key_blob_ref = b"\x11" * 32  # non-zero: the KeyBlob pointer subscribers read
    with _actor_client(url, creator) as ws:
        ws.call(
            "fauna.subscriptions.tiers.create",
            {
                "name": TIER,
                "rank": 2,
                "description": None,
                "price_hint": PRICE,
                "payment_url": PAY_URL,
                "auto_approve": False,
                # The required birth KeyBlob (empty roster).
                "encrypted_upload": fauna_ffi.build_tier_birth_upload(creator_secret, TIER, period_key),
            },
        )

    post_bytes, encrypted_blob = fauna_ffi.build_gated_post(
        creator_secret, PREVIEW, FULL_BODY, TIER, 2, key_blob_ref, period_key
    )
    # The sealed full-body blob rests in the nest blob store; the post's
    # encrypted_ref is its BLAKE3 (the upload reply echoes the same hash).
    blob_hash = _upload_sealed_blob(port, creator["token"], encrypted_blob)
    assert len(blob_hash) == 64

    gated_post_id = ws_api.create_post(port, creator, post_bytes)
    ungated_bytes = fauna_ffi.build_post(
        creator_secret, f"Plain Post\n\nJust {UNGATED_MARKER} here."
    )
    ungated_post_id = ws_api.create_post(port, creator, ungated_bytes)

    with _actor_client(url, creator) as ws:
        ws.call(
            "fauna.web.publish.set",
            {"post_id": bytes.fromhex(gated_post_id), "slug": "premium"},
        )
        ws.call(
            "fauna.web.publish.set",
            {"post_id": bytes.fromhex(ungated_post_id), "slug": "plain"},
        )

    # Serve the creator's site at the apex (the shared test nest has no
    # domain, so the apex catch-all answers every host).
    admin_ws = WsRpcAdminClient(
        url,
        actor_id=bytes(admin_sk.verify_key),
        signing_key=bytes(admin_sk),
    )
    with admin_ws:
        admin_ws.call("fauna.web.set_apex_actor", {"actor_id": creator_id})

    try:
        # ── 2. No grant yet: the canonical URL is the TEASER (preview + tier +
        # price + payment link), never the full content; nothing is sealed. ──
        status, body, headers = _get(url, "/post/premium.html")
        assert status == 200, body
        _assert_no_cookie(headers, "teaser")
        assert "A public teaser paragraph" in body, body
        assert TIER in body and PRICE in body and PAY_URL in body, (
            f"teaser must carry the paywall box (tier/price/payment link): {body}"
        )
        assert FULL_MARKER not in body, f"full content leaked into the teaser: {body}"

        # The owner mint surface refuses while no sealed page exists.
        with _actor_client(url, creator) as ws:
            with pytest.raises(RpcCallError) as ei:
                ws.call("fauna.web.paywall.mint_token", {"slug": "premium"})
            assert "not_paywalled" in str(ei.value), str(ei.value)

        # ── 3. The creator grants the web-serve holder the tier's period key
        # (discovered via fetch_bridge_pubkey; X-Wing wrap to its published
        # ek). The mint handler refreshes the registry + re-renders before
        # replying, so the sealed full page exists as soon as mint returns. ──
        with _actor_client(url, creator) as ws:
            holder = ws.call(
                "fauna.bridges.fetch_bridge_pubkey",
                {"bridge_role": "content-processor", "bridge_id": "web-serve"},
            )
            holder_x25519 = bytes(holder["x25519_pubkey"])
            holder_ek = bytes(holder["mlkem_ek"]) if holder.get("mlkem_ek") else None
            assert len(holder_x25519) == 32
            grant_id = b"\x07" * 16
            grant_blob = fauna_ffi.build_post_grant(
                creator_id, grant_id, holder_x25519, holder_ek,
                now, now + 3600, TIER, period_key,
            )
            mint = ws.call("fauna.capabilities.mint", {"grant_blob": grant_blob})
            assert mint.get("ok") is True, mint

            token_reply = ws.call("fauna.web.paywall.mint_token", {"slug": "premium"})
            token = token_reply["token"]
            assert token_reply["path"] == "post/premium.html"
            assert token_reply["expires"] > now

        # ── 4. A valid capability URL serves the FULL rendered post from the
        # sealed class, unsealed at serve time — private, uncacheable, no
        # cookie. The plain URL still serves the teaser. ──
        status, body, headers = _get(url, f"/post/premium.html?token={token}")
        assert status == 200, body
        _assert_no_cookie(headers, "sealed serve")
        assert FULL_MARKER in body, f"token must serve the full post: {body}"
        assert "<strong>bold</strong>" in body, f"full body renders markdown: {body}"
        cc = {k.lower(): v for k, v in headers.items()}.get("cache-control", "")
        assert cc == "private, no-store", (
            f"entitled content must never be shared-cacheable, got {cc!r}"
        )

        status, body, _ = _get(url, "/post/premium.html")
        assert FULL_MARKER not in body, "the token-less URL must stay the teaser"

        # ── 5. A forged/tampered token gets the teaser, not the content and
        # not an error page. (Expiry is unit-pinned in web_content::token.) ──
        tampered = token[:-2] + ("AA" if not token.endswith("AA") else "BB")
        status, body, headers = _get(url, f"/post/premium.html?token={tampered}")
        assert status == 200
        _assert_no_cookie(headers, "tampered token")
        assert FULL_MARKER not in body, "a tampered token must not open the seal"
        assert "A public teaser paragraph" in body

        status, body, _ = _get(url, "/post/premium.html?token=garbage")
        assert FULL_MARKER not in body

        # ── 6. Revoke darkens the paywalled slice at use time — the SAME
        # still-unexpired token now gets the teaser — while ungated public
        # content keeps serving untouched. ──
        with _actor_client(url, creator) as ws:
            revoked = ws.call("fauna.capabilities.revoke", {"grant_id": grant_id})
            assert revoked.get("ok") is True, revoked

        status, body, headers = _get(url, f"/post/premium.html?token={token}")
        assert status == 200
        _assert_no_cookie(headers, "post-revoke")
        assert FULL_MARKER not in body, (
            "a revoked grant must darken the sealed slice even for a valid token"
        )
        assert "A public teaser paragraph" in body, body

        status, body, _ = _get(url, "/post/plain.html")
        assert status == 200
        assert UNGATED_MARKER in body, (
            f"ungated public web content must keep serving after revoke: {body}"
        )
    finally:
        # Leave the shared session nest apex-clean for sibling tests.
        with admin_ws:
            admin_ws.call("fauna.web.set_apex_actor", {"actor_id": None})


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    """Surface a 3xx as the response instead of following it — the door's
    `303` and its `Location` are the assertion."""

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def _request(url: str, path: str, data: bytes | None = None) -> tuple[int, str, dict]:
    """GET (or form-POST when `data` is given) without following redirects."""
    req = urllib.request.Request(
        url.rstrip("/") + path, data=data, method="POST" if data is not None else "GET"
    )
    if data is not None:
        req.add_header("Content-Type", "application/x-www-form-urlencoded")
    opener = urllib.request.build_opener(_NoRedirect)
    try:
        resp = opener.open(req)
        return resp.status, resp.read().decode("utf-8", "replace"), dict(resp.headers)
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode("utf-8", "replace"), dict(e.headers)


def _redeem(url: str, code: str, path: str) -> tuple[int, str, dict]:
    form = urllib.parse.urlencode({"code": code, "path": path}).encode()
    return _request(url, "/api/v1/paywall/redeem", form)


@pytest.mark.feature("paid-posts-and-tips")
def test_anonymous_visitor_pays_or_redeems_on_the_teaser(nest_instance):
    """Outcome 17 — an anonymous web visitor gets past the teaser with no
    Fauna account (`monetization.md` § Pillar 2 → *Visitor entitlement
    doors*): the teaser carries a claim-code form whose redeem door answers
    `303` to the ordinary short-lived `?token=` URL and never consumes the
    code (it works again, and stays bindable in an app); a wrong code is a
    `403` page; and a paying visitor whom the provider redirects to the return
    door is shown the code the webhook minted for their checkout.
    """
    url = nest_instance["url"]
    port = nest_instance["port"]
    admin_sk = nest_instance["admin"]["signing_key"]

    creator = create_actor_and_register(port, admin_signing_key=admin_sk)
    creator_id: bytes = bytes(creator["actor_id_bytes"])
    creator_secret = bytes(creator["signing_key"])
    now = int(time.time())
    tier, slug, marker = "door-gold", "door-premium", "the-door-opened-marker"
    period_key = bytes(range(1, 33))

    # ── Fixture: a tier, a gated web-published post, the holder grant (the
    # same client-side setup as the teaser/token test above). ──
    with _actor_client(url, creator) as ws:
        ws.call(
            "fauna.subscriptions.tiers.create",
            {
                "name": tier,
                "rank": 2,
                "description": None,
                "price_hint": PRICE,
                "payment_url": PAY_URL,
                "auto_approve": False,
                "encrypted_upload": fauna_ffi.build_tier_birth_upload(
                    creator_secret, tier, period_key
                ),
            },
        )
    post_bytes, encrypted_blob = fauna_ffi.build_gated_post(
        creator_secret, PREVIEW, f"Door Post\n\nHere is {marker}.", tier, 2,
        b"\x12" * 32, period_key,
    )
    _upload_sealed_blob(port, creator["token"], encrypted_blob)
    post_id = ws_api.create_post(port, creator, post_bytes)
    with _actor_client(url, creator) as ws:
        ws.call("fauna.web.publish.set", {"post_id": bytes.fromhex(post_id), "slug": slug})
        holder = ws.call(
            "fauna.bridges.fetch_bridge_pubkey",
            {"bridge_role": "content-processor", "bridge_id": "web-serve"},
        )
        grant_blob = fauna_ffi.build_post_grant(
            creator_id, b"\x08" * 16, bytes(holder["x25519_pubkey"]),
            bytes(holder["mlkem_ek"]) if holder.get("mlkem_ek") else None,
            now, now + 3600, tier, period_key,
        )
        assert ws.call("fauna.capabilities.mint", {"grant_blob": grant_blob}).get("ok") is True
        code = ws.call("fauna.payments.claims.mint", {"tier": tier})["code"]

    admin_ws = WsRpcAdminClient(
        url, actor_id=bytes(admin_sk.verify_key), signing_key=bytes(admin_sk)
    )
    with admin_ws:
        admin_ws.call("fauna.web.set_apex_actor", {"actor_id": creator_id})

    page = f"/post/{slug}.html"
    resource = f"post/{slug}.html"
    try:
        # ── The teaser carries the code form. ──
        status, body, _ = _request(url, page)
        assert status == 200, body
        assert marker not in body
        assert 'action="/api/v1/paywall/redeem"' in body, (
            f"the teaser must carry the claim-code form: {body}"
        )

        # ── A wrong code: the one nest-owned 403 page, no cookie. ──
        status, body, headers = _redeem(url, "WRONGCODE9", resource)
        assert status == 403, (status, body)
        _assert_no_cookie(headers, "refused redeem")
        assert marker not in body

        # ── The right code: 303 to the token URL, which opens the post. ──
        def redeem_and_open(c: str) -> None:
            status, body, headers = _redeem(url, c, resource)
            assert status == 303, (status, body)
            _assert_no_cookie(headers, "redeem")
            location = {k.lower(): v for k, v in headers.items()}["location"]
            assert location.startswith(f"/{resource}?token="), location
            status, body, headers = _request(url, location)
            assert status == 200, body
            _assert_no_cookie(headers, "token serve")
            assert marker in body, f"the redeemed token must open the post: {body}"

        redeem_and_open(code)
        # Durable: the same code works again (a browser presentation is not a
        # redemption), and the claim stays unredeemed for a later app binding.
        redeem_and_open(code)
        with _actor_client(url, creator) as ws:
            claims = ws.call("fauna.payments.claims.list", {})["claims"]
        row = next(c for c in claims if c["code"] == code)
        assert row.get("redeemed_at") is None and row.get("redeemed_by") is None, row

        # ── The paid half: an unbound checkout's webhook mints a claim keyed
        # by its return reference; the provider's redirect lands the buyer on
        # the return door, which shows the code; the code opens the post. ──
        secret = "whsec_door_fake"
        with _actor_client(url, creator) as ws:
            saved = ws.call(
                "fauna.payments.providers.set",
                {"kind": "fake", "webhook_secret": secret, "tier": tier},
            )
            assert saved.get("saved") is True, saved
        return_ref = "cs_test_door_1"
        webhook = json.dumps(
            {"id": "pay_door_1", "event": "payment", "return_ref": return_ref}
        ).encode()
        req = urllib.request.Request(
            f"{url}/api/v1/payments/webhook/{creator_id.hex()}/fake",
            data=webhook, method="POST",
        )
        req.add_header("Content-Type", "application/json")
        req.add_header(
            "X-Fauna-Signature", hmac.new(secret.encode(), webhook, hashlib.sha256).hexdigest()
        )
        with urllib.request.urlopen(req, timeout=15) as resp:
            minted = json.loads(resp.read())
        assert minted.get("status") == "claim_minted", minted
        paid_code = minted["claim_code"]

        status, body, _ = _request(url, f"/api/v1/paywall/return/fake?ref={return_ref}")
        assert status == 200, body
        assert paid_code in body, f"the return door must show the minted code: {body}"
        redeem_and_open(paid_code)

        # Not yet confirmed → 202 with a refresh; `manual` is never served.
        status, body, _ = _request(url, "/api/v1/paywall/return/fake?ref=unknown")
        assert status == 202, (status, body)
        assert 'http-equiv="refresh"' in body
        status, _, _ = _request(url, "/api/v1/paywall/return/manual?ref=x")
        assert status == 403
    finally:
        with admin_ws:
            admin_ws.call("fauna.web.set_apex_actor", {"actor_id": None})
