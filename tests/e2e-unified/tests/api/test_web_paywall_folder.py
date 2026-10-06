"""Web paywall — FILE SET half (monetization.md § Pillar 2) — the full
nest-side sealed-static-serving contract on a real ``fauna-nest`` binary.

The tier_3 build-success bar for the folder half of the web-paywall track: a
creator hosts a public ``web``-mode folder (a plaintext file serves to any
visitor) AND a **paywalled** ``web`` set gated to a paid tier (its files rest
**content-key-sealed** on the nest). A browser visitor with no token gets the
pre-rendered **teaser** (tier + price + payment link) at **HTTP 402**; a visitor
presenting a valid short-lived signed **capability URL** gets the real file
bytes, unsealed at serve time by the web-serve holder under the creator's live
capability grant; **revoking the grant darkens the sealed slice** while the
public file keeps serving. No ``Set-Cookie`` ever appears on the web-serving
surface (web-content-hosting.md invariant 2), and a sealed file served with a
token is ``private, no-store`` (never shared-cacheable).

Production data flow asserted end-to-end (web-content-hosting.md § Sealed static
files; mls-group-key-material.md § M2):

  creator mints tier (``fauna.subscriptions.tiers.create``) → creates a public
  ``web`` set + a paywalled ``web`` set (``fauna.folders.create`` mode=web) →
  ``fauna.folders.set_web_paywall(tier)`` on the paywalled set → registers a
  write-capable device (``fauna.sync.register``) → uploads a plaintext file's
  chunks+manifest (``POST /api/v1/chunks`` + ``/api/v1/manifests``) and records
  it (``fauna.sync.changes.record`` — no ``content_key_version`` ⇒ public) →
  uploads a content-key-SEALED file the same way and records it WITH a
  ``content_key_version`` (⇒ the ``web_files`` row rests sealed) → serves the
  creator's site at the apex (``fauna.web.set_apex_actor``) → discovers the
  web-serve holder (``fauna.bridges.fetch_bridge_pubkey`` role=content-processor)
  → mints it a ``content.read{folder:set}`` grant wrapping the set's content key
  (``fauna.capabilities.mint``) → ``fauna.web.paywall.mint_token(path=…)`` →
  ``GET /report.html?token=…`` serves the decrypted bytes →
  ``fauna.capabilities.revoke`` → the same token serves the 402 teaser again; the
  public file is untouched throughout.

The sealed-file bytes and the folder grant are built by the ``fauna_ffi``
helpers ``seal_folder_file`` / ``build_folder_grant`` — the Python twins of the
shared ``fauna_core`` chunk+seal path and the production
``fauna_client_capabilities::mint_folder_grant`` (so the harness never
re-implements the chunk KDF or the grant wire). Both are **fixture setup**: they
arrange the world the ``FoldersAuthor::paywall_set`` client orchestration will
later arrange (E2E rule 8 setup carve-out (b)); the mutation under test is the
SERVING behavior, which is exactly what a browser visitor exercises. The
client-UI equivalence for the paywall control itself is the per-set "paywall to
tier" select in the client folders settings UI (``ui/folders.md``).
"""

import json
import time
import urllib.error
import urllib.request

import pytest

import fauna_ffi
from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient
from common.auth import create_actor_and_register, port_base_url

pytestmark = pytest.mark.tier_3

TIER = "gold"
PRICE = "5 EUR / month"
PAY_URL = "https://pay.example/gold"

PUBLIC_SET = "site"
PAYWALLED_SET = "premium"
PUBLIC_PATH = "hello.html"
SEALED_PATH = "report.html"

PUBLIC_MARKER = "public-folder-marker"
SEALED_MARKER = "members-only-folder-marker"
PUBLIC_BODY = f"<h1>Hello</h1>\n<p>{PUBLIC_MARKER}</p>\n".encode()
SEALED_BODY = f"<h1>Quarterly Report</h1>\n<p>{SEALED_MARKER}</p>\n".encode()

# The paywalled set's content-key generation-1 key (what a client's re-seal
# would derive; here a fixed fixture value). The seal and the grant use the same
# key at the same version, exactly as production keeps them in lockstep.
CONTENT_KEY = bytes(range(32))
VERSION = 1
# A write-capable device for the creator (any 32-byte id under a fresh actor).
DEVICE_ID = bytes([0xD1] * 32)

# A second content-key generation — what a content-key ROTATION (member evict /
# explicit rotate) advances custody to. New uploads seal under the new current;
# the standing grant must gain this generation's wrap (via `capabilities.renew`)
# for the newly-sealed bytes to serve.
GEN2_KEY = bytes(range(32, 64))
GEN2_VERSION = 2
GEN2_PATH = "report-q2.html"
GEN2_MARKER = "rotated-generation-marker"
GEN2_BODY = f"<h1>Q2 Report</h1>\n<p>{GEN2_MARKER}</p>\n".encode()


def _actor_client(url: str, actor: dict) -> WsRpcAdminClient:
    return WsRpcAdminClient(
        url, actor_id=actor["actor_id_bytes"], signing_key=bytes(actor["signing_key"])
    )


def _get(url: str, path: str) -> tuple[int, str, dict]:
    """GET `path` on the nest, returning (status, body_text, headers). A 402
    teaser arrives as an HTTPError; unwrap it to the same tuple shape."""
    req = urllib.request.Request(url.rstrip("/") + path, method="GET")
    try:
        resp = urllib.request.urlopen(req)
        return resp.status, resp.read().decode("utf-8", "replace"), dict(resp.headers)
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode("utf-8", "replace"), dict(e.headers)


def _post_bytes(
    port: int, route: str, token: str, data: bytes, content_hash: bytes | None = None
) -> str:
    """POST raw `data` to a chunk/manifest route (Bearer auth); return the hex
    content hash the nest echoes.

    `content_hash` rides as `X-Content-Hash` — the store key a chunk upload
    claims. Pass `seal_folder_file`'s `store_key` for every chunk: a sealed
    chunk's key is `blake3(body)` (the nest's proof (a)), a plaintext chunk's is
    the hash of the plaintext its FRAMED body unframes to (proof (b)) — without
    the header the nest would key a framed plaintext chunk by `blake3(body)`.

    Through `port_base_url` rather than a literal `http://127.0.0.1:{port}`: the
    docker image serves TLS on its listener and has no plain-HTTP posture, so a
    hard-coded scheme dials plain HTTP at a TLS listener and the connection is
    closed before any assertion. That port→scheme fact has one home
    (`common.auth`), and every nest dial reads it there.
    """
    req = urllib.request.Request(
        f"{port_base_url(port)}{route}",
        data=data,
        headers={
            "Authorization": f"Bearer {token}",
            "Content-Type": "application/octet-stream",
            **({"X-Content-Hash": content_hash.hex()} if content_hash is not None else {}),
        },
        method="POST",
    )
    resp = urllib.request.urlopen(req)
    return json.loads(resp.read())["hash"]


def _assert_no_cookie(headers: dict, where: str) -> None:
    assert not any(k.lower() == "set-cookie" for k in headers), (
        f"no auth/session cookie may ever be set on the web-serving surface "
        f"(invariant 2), got Set-Cookie at {where}: {headers}"
    )


def _mint_expired_token(port: int, owner_hex: str, path: str) -> str:
    """``POST /api/v1/test/web-paywall/expired-token`` (``test-hooks``-gated):
    mint a capability-URL token signed by the real web-serve holder key but
    already past its ``expires`` — the production `mint_token` RPC always
    stamps `now + WEB_PAYWALL_TOKEN_TTL_SECS` (600s), so there is no way to
    reach the expiry branch through it without waiting out the real TTL."""
    req = urllib.request.Request(
        f"{port_base_url(port)}/api/v1/test/web-paywall/expired-token",
        data=json.dumps({"owner": owner_hex, "path": path}).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    resp = urllib.request.urlopen(req)
    return json.loads(resp.read())["token"]


def _seed_web_file(
    url: str,
    port: int,
    creator: dict,
    folder: str,
    path: str,
    content: bytes,
    content_key: bytes | None,
    version: int = VERSION,
) -> None:
    """Ingest a file into a website-enabled set exactly as a client would: seal
    (or leave plaintext), upload every chunk + the manifest through the chunk
    routes (the nest frames them at rest), then record the change — a
    `content_key_version` on a sealed file makes the `web_files` row sealed
    at that content-key generation. A sealed file rests in a PRIVATE folder,
    which rests no plaintext path (the S9 flip), so its record carries a
    synthetic `path_sealed` the way every seal-less API seed does; the
    plaintext `path` still reaches the `web_files` projection, which is keyed
    by URL path by design."""
    manifest_bytes, chunks = fauna_ffi.seal_folder_file(content, content_key)
    for store_key, body in chunks:
        got = _post_bytes(port, "/api/v1/chunks", creator["token"], body, store_key)
        assert got == store_key.hex(), (
            f"the nest must store the chunk under its store key {store_key.hex()}, got {got}"
        )
    manifest_hash = _post_bytes(port, "/api/v1/manifests", creator["token"], manifest_bytes)

    record = {
        "folder": folder,
        "device_id": DEVICE_ID.hex(),
        "path": path,
        "manifest_hash": manifest_hash,
        "size_bytes": len(content),
        "change_type": "create",
    }
    if content_key is not None:
        record["content_key_version"] = version
        record["path_sealed"] = b"e2e-synthetic-seal"
    fauna_ffi.harness_record_change(url, bytes(creator["signing_key"]), record)


@pytest.mark.feature("paid-posts-and-tips")
def test_web_paywall_folder_teaser_token_and_revoke(nest_instance):
    url = nest_instance["url"]
    port = nest_instance["port"]
    admin = nest_instance["admin"]
    admin_sk = admin["signing_key"]

    creator = create_actor_and_register(port, admin_signing_key=admin_sk)
    creator_id: bytes = bytes(creator["actor_id_bytes"])
    now = int(time.time())

    # ── 1. The creator defines the paid tier, two web sets (one public, one
    # paywalled to the tier), and a write-capable device. ──
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
                "encrypted_upload": fauna_ffi.build_tier_birth_upload(bytes(creator["signing_key"]), TIER, b"\x5a" * 32),
            },
        )
        # Phase 4 (folders re-model): the `web_files` fan-out keys on the
        # per-folder WEBSITE TOGGLE, not `mode` — so both sets flip it on. The
        # public set is additionally declassified (`audience: "public"`), the
        # ratified shape non-paywalled serving rests on; the paywalled set stays
        # sealed-audience (paywall is `shared`-audience machinery, never
        # `public`). The public set's seed rests its plaintext path through the
        # audience; the paywalled set's seed carries a synthetic `path_sealed`
        # (`_seed_web_file`), since a private folder rests no plaintext path.
        for name in (PUBLIC_SET, PAYWALLED_SET):
            fauna_ffi.harness_create_set(
                url, bytes(creator["signing_key"]),
                {"name": name},
            )
            ws.call("fauna.folders.update", {"name": name, "website_enabled": True})
        ws.call(
            "fauna.folders.update", {"name": PUBLIC_SET, "audience": "public"}
        )
        ws.call(
            "fauna.folders.set_web_paywall", {"name": PAYWALLED_SET, "tier": TIER}
        )
        ws.call(
            "fauna.sync.register",
            {"device_id": DEVICE_ID.hex(), "label": "seed", "capabilities": "read,write"},
        )

    # ── 2. Ingest a plaintext file (public set) and a content-key-sealed file
    # (paywalled set), each through the production chunk+record path. ──
    _seed_web_file(url, port, creator, PUBLIC_SET, PUBLIC_PATH, PUBLIC_BODY, None)
    _seed_web_file(url, port, creator, PAYWALLED_SET, SEALED_PATH, SEALED_BODY, CONTENT_KEY)

    # Serve the creator's site at the apex (the shared test nest has no domain,
    # so the apex catch-all answers every host).
    admin_ws = WsRpcAdminClient(
        url, actor_id=bytes(admin_sk.verify_key), signing_key=bytes(admin_sk)
    )
    with admin_ws:
        admin_ws.call("fauna.web.set_apex_actor", {"actor_id": creator_id})

    try:
        # ── 3. The public plaintext file serves to anyone, verbatim. ──
        status, body, headers = _get(url, f"/{PUBLIC_PATH}")
        assert status == 200, body
        _assert_no_cookie(headers, "public file")
        assert PUBLIC_MARKER in body, f"public web file must serve: {body}"

        # ── 4. The sealed file with no token is the 402 teaser: tier + price +
        # payment link + the path, never the sealed content. ──
        status, body, headers = _get(url, f"/{SEALED_PATH}")
        assert status == 402, f"a sealed file with no token must be 402, got {status}: {body}"
        _assert_no_cookie(headers, "teaser")
        assert TIER in body and PRICE in body and PAY_URL in body, (
            f"teaser must carry the paywall box (tier/price/payment link): {body}"
        )
        assert SEALED_PATH in body, f"teaser names the requested path: {body}"
        assert SEALED_MARKER not in body, f"sealed content leaked into the teaser: {body}"

        # The mint surface refuses a token for a NON-paywalled (public) path.
        with _actor_client(url, creator) as ws:
            with pytest.raises(RpcCallError) as ei:
                ws.call("fauna.web.paywall.mint_token", {"path": PUBLIC_PATH})
            assert "not_paywalled" in str(ei.value), str(ei.value)

        # ── 5. The creator grants the web-serve holder the set's content key
        # (discovered via fetch_bridge_pubkey; X-Wing wrap when it published an
        # ek). The mint handler refreshes the in-process holder registry before
        # replying, so the key is live as soon as mint returns. ──
        with _actor_client(url, creator) as ws:
            holder = ws.call(
                "fauna.bridges.fetch_bridge_pubkey",
                {"bridge_role": "content-processor", "bridge_id": "web-serve"},
            )
            holder_x25519 = bytes(holder["x25519_pubkey"])
            holder_ek = bytes(holder["mlkem_ek"]) if holder.get("mlkem_ek") else None
            assert len(holder_x25519) == 32
            grant_id = b"\x07" * 16
            grant_blob = fauna_ffi.build_folder_grant(
                creator_id,
                grant_id,
                holder_x25519,
                holder_ek,
                now,
                now + 3600,
                PAYWALLED_SET,
                VERSION,
                CONTENT_KEY,
            )
            mint = ws.call("fauna.capabilities.mint", {"grant_blob": grant_blob})
            assert mint.get("ok") is True, mint

            token_reply = ws.call("fauna.web.paywall.mint_token", {"path": SEALED_PATH})
            token = token_reply["token"]
            assert token_reply["path"] == SEALED_PATH
            assert token_reply["expires"] > now

        # ── 6. A valid capability URL serves the real (decrypted) file bytes —
        # private, uncacheable, no cookie. The token-less URL stays the teaser. ──
        status, body, headers = _get(url, f"/{SEALED_PATH}?token={token}")
        assert status == 200, body
        _assert_no_cookie(headers, "sealed serve")
        assert SEALED_MARKER in body, f"a valid token must serve the sealed file: {body}"
        cc = {k.lower(): v for k, v in headers.items()}.get("cache-control", "")
        assert cc == "private, no-store", (
            f"entitled content must never be shared-cacheable, got {cc!r}"
        )

        status, body, _ = _get(url, f"/{SEALED_PATH}")
        assert status == 402 and SEALED_MARKER not in body, (
            "the token-less URL must stay the teaser"
        )

        # ── 7. A tampered token opens nothing — the teaser, not the content and
        # not an error page. (Expiry is unit-pinned in web_content::token.) ──
        tampered = token[:-2] + ("AA" if not token.endswith("AA") else "BB")
        status, body, headers = _get(url, f"/{SEALED_PATH}?token={tampered}")
        assert status == 402
        _assert_no_cookie(headers, "tampered token")
        assert SEALED_MARKER not in body, "a tampered token must not open the seal"

        # ── 8. Revoke darkens the sealed slice at use time — the SAME
        # still-unexpired token now gets the teaser — while the public file keeps
        # serving untouched. ──
        with _actor_client(url, creator) as ws:
            revoked = ws.call("fauna.capabilities.revoke", {"grant_id": grant_id})
            assert revoked.get("ok") is True, revoked

        status, body, headers = _get(url, f"/{SEALED_PATH}?token={token}")
        assert status == 402, body
        _assert_no_cookie(headers, "post-revoke")
        assert SEALED_MARKER not in body, (
            "a revoked grant must darken the sealed slice even for a valid token"
        )

        status, body, _ = _get(url, f"/{PUBLIC_PATH}")
        assert status == 200 and PUBLIC_MARKER in body, (
            f"public web content must keep serving after revoke: {body}"
        )
    finally:
        # Leave the shared session nest apex-clean for sibling tests.
        with admin_ws:
            admin_ws.call("fauna.web.set_apex_actor", {"actor_id": None})


@pytest.mark.feature("paid-posts-and-tips")
def test_web_paywall_folder_rotation_renews_the_grant(nest_instance):
    """The ROTATION leg (S6c): after a paywalled set's content key rotates, a
    file newly sealed under the advanced generation is DARK until the standing
    grant gains that generation's wrap via ``fauna.capabilities.renew`` — then it
    serves, and the older generation keeps serving (history preserved).

    Mirrors the production ``FoldersAuthor::rotate_paywall_grant`` wire
    (``mls-group-key-material.md`` § M2: *"Rotation appends the new generation's
    wrap via fauna.capabilities.renew"*): the appended wrap is built by the
    ``fauna_ffi.build_folder_scope_wrap`` twin of the per-generation wraps that
    orchestration re-provisions. Renewing IS a client-driven mutation, but the
    behaviour under test is the SERVING transition a browser visitor exercises;
    the append is fixture setup arranging the world the ``rotate_paywall_grant``
    orchestration arranges (E2E rule 8 carve-out (b))."""
    url = nest_instance["url"]
    port = nest_instance["port"]
    admin = nest_instance["admin"]
    admin_sk = admin["signing_key"]

    creator = create_actor_and_register(port, admin_signing_key=admin_sk)
    creator_id: bytes = bytes(creator["actor_id_bytes"])
    now = int(time.time())

    # A paywalled web set + a write-capable device.
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
                "encrypted_upload": fauna_ffi.build_tier_birth_upload(bytes(creator["signing_key"]), TIER, b"\x5a" * 32),
            },
        )
        fauna_ffi.harness_create_set(
            url, bytes(creator["signing_key"]),
            {"name": PAYWALLED_SET},
        )
        # Phase 4: the `web_files` fan-out keys on the website toggle.
        ws.call("fauna.folders.update", {"name": PAYWALLED_SET, "website_enabled": True})
        ws.call("fauna.folders.set_web_paywall", {"name": PAYWALLED_SET, "tier": TIER})
        ws.call(
            "fauna.sync.register",
            {"device_id": DEVICE_ID.hex(), "label": "seed", "capabilities": "read,write"},
        )

    # Seed a generation-1 file and a generation-2 file (a content-key rotation ⇒
    # new uploads seal under the advanced generation).
    _seed_web_file(url, port, creator, PAYWALLED_SET, SEALED_PATH, SEALED_BODY, CONTENT_KEY, VERSION)
    _seed_web_file(url, port, creator, PAYWALLED_SET, GEN2_PATH, GEN2_BODY, GEN2_KEY, GEN2_VERSION)

    admin_ws = WsRpcAdminClient(
        url, actor_id=bytes(admin_sk.verify_key), signing_key=bytes(admin_sk)
    )
    with admin_ws:
        admin_ws.call("fauna.web.set_apex_actor", {"actor_id": creator_id})

    try:
        # Grant the holder ONLY generation 1, then mint a token per path.
        with _actor_client(url, creator) as ws:
            holder = ws.call(
                "fauna.bridges.fetch_bridge_pubkey",
                {"bridge_role": "content-processor", "bridge_id": "web-serve"},
            )
            holder_x25519 = bytes(holder["x25519_pubkey"])
            holder_ek = bytes(holder["mlkem_ek"]) if holder.get("mlkem_ek") else None
            grant_id = b"\x09" * 16
            grant_blob = fauna_ffi.build_folder_grant(
                creator_id,
                grant_id,
                holder_x25519,
                holder_ek,
                now,
                now + 3600,
                PAYWALLED_SET,
                VERSION,
                CONTENT_KEY,
            )
            assert ws.call("fauna.capabilities.mint", {"grant_blob": grant_blob})["ok"] is True
            token_v1 = ws.call("fauna.web.paywall.mint_token", {"path": SEALED_PATH})["token"]
            token_v2 = ws.call("fauna.web.paywall.mint_token", {"path": GEN2_PATH})["token"]

        # Generation 1 serves; generation 2 is DARK — the grant lacks the gen-2
        # key, so `keys_for_folder(set, 2)` is empty and it falls to the teaser.
        status, body, _ = _get(url, f"/{SEALED_PATH}?token={token_v1}")
        assert status == 200 and SEALED_MARKER in body, f"gen-1 must serve: {body}"
        status, body, _ = _get(url, f"/{GEN2_PATH}?token={token_v2}")
        assert status == 402 and GEN2_MARKER not in body, (
            f"gen-2 bytes must be dark BEFORE the grant carries gen-2: {body}"
        )

        # The rotation leg: append generation 2's wrap to the standing grant via
        # `fauna.capabilities.renew` (the nest folds it in, deduped by (scope, epoch)).
        with _actor_client(url, creator) as ws:
            gen2_wrap = fauna_ffi.build_folder_scope_wrap(
                creator_id, holder_x25519, holder_ek, PAYWALLED_SET, GEN2_VERSION, GEN2_KEY
            )
            renewed = ws.call(
                "fauna.capabilities.renew",
                {"grant_id": grant_id, "new_epoch_end": now + 7200, "appended_keys": [gen2_wrap]},
            )
            assert renewed.get("ok") is True, renewed

        # Generation 2 now serves (the fresh token opens the newest-sealed bytes),
        # private + uncacheable; generation 1 still serves (history preserved).
        status, body, headers = _get(url, f"/{GEN2_PATH}?token={token_v2}")
        assert status == 200, body
        _assert_no_cookie(headers, "gen-2 serve")
        assert GEN2_MARKER in body, f"gen-2 must serve AFTER renew appends its wrap: {body}"
        cc = {k.lower(): v for k, v in headers.items()}.get("cache-control", "")
        assert cc == "private, no-store", f"entitled content must be private, got {cc!r}"

        status, body, _ = _get(url, f"/{SEALED_PATH}?token={token_v1}")
        assert status == 200 and SEALED_MARKER in body, (
            f"gen-1 must keep serving after the rotation appended gen-2: {body}"
        )
    finally:
        with admin_ws:
            admin_ws.call("fauna.web.set_apex_actor", {"actor_id": None})


@pytest.mark.feature("paid-posts-and-tips")
def test_web_paywall_folder_expired_token_serves_teaser(nest_instance):
    """An EXPIRED capability-URL token must serve the teaser, never the sealed
    content and never an error page (web-content-hosting.md § Sealed static
    files) — previously only unit-pinned in ``web_content::token``
    (``expired_token_is_rejected``), which never drove the HTTP serve path a
    browser visitor actually exercises."""
    url = nest_instance["url"]
    port = nest_instance["port"]
    admin = nest_instance["admin"]
    admin_sk = admin["signing_key"]

    creator = create_actor_and_register(port, admin_signing_key=admin_sk)
    creator_id: bytes = bytes(creator["actor_id_bytes"])
    now = int(time.time())

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
                "encrypted_upload": fauna_ffi.build_tier_birth_upload(bytes(creator["signing_key"]), TIER, b"\x5a" * 32),
            },
        )
        fauna_ffi.harness_create_set(
            url, bytes(creator["signing_key"]),
            {"name": PAYWALLED_SET},
        )
        # Phase 4: the `web_files` fan-out and the serve walk key on the
        # website toggle.
        ws.call("fauna.folders.update", {"name": PAYWALLED_SET, "website_enabled": True})
        ws.call("fauna.folders.set_web_paywall", {"name": PAYWALLED_SET, "tier": TIER})
        ws.call(
            "fauna.sync.register",
            {"device_id": DEVICE_ID.hex(), "label": "seed", "capabilities": "read,write"},
        )

    _seed_web_file(url, port, creator, PAYWALLED_SET, SEALED_PATH, SEALED_BODY, CONTENT_KEY)

    admin_ws = WsRpcAdminClient(
        url, actor_id=bytes(admin_sk.verify_key), signing_key=bytes(admin_sk)
    )
    with admin_ws:
        admin_ws.call("fauna.web.set_apex_actor", {"actor_id": creator_id})

    try:
        # Grant the holder the content key, exactly as a valid-token test would —
        # an expired token must be refused on expiry alone, not for lack of a grant.
        with _actor_client(url, creator) as ws:
            holder = ws.call(
                "fauna.bridges.fetch_bridge_pubkey",
                {"bridge_role": "content-processor", "bridge_id": "web-serve"},
            )
            holder_x25519 = bytes(holder["x25519_pubkey"])
            holder_ek = bytes(holder["mlkem_ek"]) if holder.get("mlkem_ek") else None
            grant_id = b"\x0a" * 16
            grant_blob = fauna_ffi.build_folder_grant(
                creator_id,
                grant_id,
                holder_x25519,
                holder_ek,
                now,
                now + 3600,
                PAYWALLED_SET,
                VERSION,
                CONTENT_KEY,
            )
            assert ws.call("fauna.capabilities.mint", {"grant_blob": grant_blob})["ok"] is True

        expired_token = _mint_expired_token(port, creator_id.hex(), SEALED_PATH)

        status, body, headers = _get(url, f"/{SEALED_PATH}?token={expired_token}")
        assert status == 402, f"an expired token must serve the teaser, got {status}: {body}"
        _assert_no_cookie(headers, "expired token")
        assert SEALED_MARKER not in body, f"an expired token must not open the seal: {body}"
        assert TIER in body and PRICE in body, f"the teaser box must still render: {body}"
    finally:
        with admin_ws:
            admin_ws.call("fauna.web.set_apex_actor", {"actor_id": None})
