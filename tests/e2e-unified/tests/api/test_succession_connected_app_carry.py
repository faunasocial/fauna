"""A connected app's data comes with the account through a succession —
full-stack (tier_3) API E2E.

Witnesses `take-your-account-back` outcome **36** — *"Data a connected app
kept under your account comes with it, and an app you authorize again finds it
where it was."* — owned by `docs/goal/behavior/succession-aftermath.md` § Re-key
scope → *The account-state plane's generation-0 delegable rows are carried by
the successor's walk*, its `ext:<kind>` sentence. The carry is CLIENT-side
shared Rust (the account driver's `bound_ext_plane` walk under the attested
predecessors' delegable schedules, then `retire_behind_carry`), so the journey
drives it through the harness door that runs that very funnel —
``fauna_ffi.harness_successor_pass``: the production runtime handed the
registry a ceremony leaves, as every app hands it — and reads the outcome from
outside, as the app itself would: listing its kind over its own principal
session and opening what it lists with the grant the successor minted it.

One journey:

1. the owner approves a connected app (a real ``https`` client document whose
   manifest declares ``ext.example.com.notes``) through the production approve
   path (``harness_answer_consent``); the app seals a note under
   ``ext:ext.example.com.notes`` as its own writer and puts it;
2. the owner takes the account back — the real ceremony
   (``helpers/succession.py``), a genuine RecoveryKey and statement;
3. the successor authorizes the same app again — attesting the app's ORIGINAL
   writer key, which the token endpoint admits although the recovery burned
   the app's roster row (the nest keeps the app's former writer and moves it
   with the account); before the successor's device
   has walked, the new grant opens nothing the app lists — the note still
   rests sealed under the predecessor's keys;
4. the successor's device runs its passes; afterwards the app lists its kind
   again and the new grant opens the note, now re-authored by the successor's
   device.

Red-verified: with the successor's device running its driver passes but not
the store-ready ledger pass (the grant log's chain re-point), the walk opened
the app's note and the writer door turned it away every pass
(``unmergeable: 3``) — the app's original writer is named only by the
predecessor's grant events — and the app found nothing.

**This module owns its nest** (the consent nest fixture is module-scoped): a
succession revokes every session of the old identity.

Latency-independent (convention 14): every call is a request with a reply and
every assertion is on returned state. The one bounded wait is the consent
page's own long-poll, the mechanism `test_ext_kinds.py` uses.

Process safety: no ``pkill``/``killall``; the nest and bridge are started and
torn down by the shared consent fixtures.
"""

import secrets
import time

import pytest
from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric import ed25519, x25519
from nacl.signing import SigningKey

import fauna_ffi
from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.atproto_consent import (  # noqa: F401 — consent_nest/consent_bridge are pytest fixtures adopted by import
    HANDLE_DOMAIN,
    consent_bridge,
    consent_nest,
)
from helpers.budgets import RPC_ROUNDTRIP_S
from helpers.client_metadata_server import (
    ClientMetadataServer,
    DOCUMENT_PATH,
    client_document,
    manifest_payload,
    sign_manifest,
)
from helpers.oauth_client import OAuthClient
from helpers.principal_session import PrincipalSession, upgrade
from helpers.succession import register_recovery_kit, succeed_identity

pytestmark = pytest.mark.tier_3

PUBLISHER = "example.com"
NOTES = f"ext.{PUBLISHER}.notes"
NOTES_SCOPE = f"ext:{NOTES}"
RECORDS = f"fauna:records:rw:ext.{PUBLISHER}.*"
REDIRECT_URI = "http://127.0.0.1:17776/callback"
STATE_FEED = "state-entry"
NOTE_KEY = "note-1"
NOTE = b"a note the app kept before the recovery"


@pytest.fixture(scope="module")
def metadata_server(tmp_path_factory):
    server = ClientMetadataServer(PUBLISHER, tmp_path_factory.mktemp("client-metadata"))
    publisher_key = ed25519.Ed25519PrivateKey.generate()
    manifest = sign_manifest(publisher_key, manifest_payload(PUBLISHER, publisher_key, ["notes"]))
    server.documents[DOCUMENT_PATH] = client_document(
        server.client_id(), REDIRECT_URI, RECORDS, manifest_jws=manifest
    )
    yield server
    server.close()


@pytest.fixture(scope="module")
def consent_nest_env(metadata_server):
    """The shared consent nest's env: its metadata fetcher pointed at
    ``metadata_server``."""
    return metadata_server.nest_env()


def _raw(public_key) -> bytes:
    return public_key.public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw)


def _secret(private_key) -> bytes:
    return private_key.private_bytes(
        serialization.Encoding.Raw, serialization.PrivateFormat.Raw, serialization.NoEncryption()
    )


def _ws(nest, seed: bytes) -> WsRpcAdminClient:
    sk = SigningKey(seed)
    return WsRpcAdminClient(nest["url"], actor_id=bytes(sk.verify_key), signing_key=seed)


def _authorize(nest, metadata_server, owner_seed: bytes, holder_pub: bytes, writer_pub: bytes):
    """The app asks (PAR → the consent page), the owner approves through the
    production approve path, the app redeems its code and opens its session.
    Returns ``(session, grant_blob)``."""
    client = OAuthClient(
        base=nest["url"],
        htu_origin=f"https://{HANDLE_DOMAIN}",
        redirect_uri=REDIRECT_URI,
        scope=RECORDS,
        holder_x25519=holder_pub,
        writer_ed25519=writer_pub,
        client_id_url=metadata_server.client_id(),
    )
    request_uri = client.push_authorization_request()
    browser_code, flow_token = client.open_consent_page(request_uri)
    with _ws(nest, owner_seed) as ws:
        pending = ws.call("fauna.bridges.atproto.list_pending_consents", {})["consents"]
    consent = next((c for c in pending if c["code"] == browser_code), None)
    assert consent, (
        f"the owner {SigningKey(owner_seed).verify_key.encode().hex()} sees no pending "
        f"consent carrying {browser_code!r}: {pending!r}"
    )
    answer = fauna_ffi.harness_answer_consent(nest["url"], owner_seed, bytes(consent["consent_id"]))
    assert answer.get("resolved") is True, f"the approve must resolve: {answer!r}"
    redirect = ""
    deadline = time.monotonic() + RPC_ROUNDTRIP_S
    while time.monotonic() < deadline:
        status, redirect = client.poll(flow_token)
        if status == "resolved":
            break
    assert redirect and "code=" in redirect, f"approval must release a code: {redirect!r}"
    tokens = client.exchange_code(redirect)
    assert tokens.get("access_token"), tokens
    session = PrincipalSession(upgrade(nest, client, tokens["access_token"]))
    ok, reply = session.call("fauna.capabilities.fetch", {})
    assert ok, reply
    grants = [bytes(g) for g in reply.get("grants", [])]
    assert len(grants) == 1, f"the app holds exactly its consent-time grant: {reply!r}"
    return session, grants[0]


def _listed(session) -> list[dict]:
    ok, reply = session.call("fauna.sync.changes.list", {"scope": NOTES_SCOPE, "item_class": STATE_FEED})
    assert ok, f"the app lists its own kind: {reply!r}"
    return [c for c in reply["changes"] if c.get("entry") is not None]


def _opened(rows: list[dict], grant: bytes, holder_secret: bytes) -> list[tuple[str, str, bytes]]:
    """``(writer hex, key, value)`` of every listed row the grant opens."""
    opened = []
    for row in rows:
        try:
            key, value = fauna_ffi.ext_kind_open_row(
                grant, holder_secret, NOTES,
                bytes.fromhex(row["origin_writer"]), int(row["origin_seq"]),
                bytes.fromhex(row["path_hash"]), bytes(row["entry"]),
            )
        except RuntimeError:
            continue
        opened.append((row["origin_writer"], key, value))
    return opened


@pytest.mark.feature("take-your-account-back")
def test_a_connected_apps_data_comes_with_the_account_and_the_app_authorized_again_finds_it(
    consent_nest, consent_bridge, metadata_server
):
    nest = consent_nest
    owner = nest["user"]
    old_seed = bytes(owner["signing_key"])
    holder = x25519.X25519PrivateKey.generate()
    writer = ed25519.Ed25519PrivateKey.generate()
    holder_pub, writer_pub = _raw(holder.public_key()), _raw(writer.public_key())

    # ── 1. The app keeps a note under the account. ──
    session, grant = _authorize(nest, metadata_server, old_seed, holder_pub, writer_pub)
    item_key, envelope = fauna_ffi.ext_kind_seal_row(
        grant, _secret(holder), _secret(writer), NOTES, NOTE_KEY, NOTE, 1, int(time.time() * 1000),
    )
    ok, reply = session.call(
        "fauna.account.state.put",
        {
            "scope": NOTES_SCOPE,
            "writer_id": writer_pub.hex(),
            "writer_seq": 1,
            "item_key": item_key,
            "op": "state-put",
            "entry": envelope,
        },
    )
    assert ok, f"the app writes its note: {reply!r}"
    before = _opened(_listed(session), grant, _secret(holder))
    assert before == [(writer_pub.hex(), NOTE_KEY, NOTE)], (
        f"before the recovery the app reads back its own note: {before!r}"
    )

    # ── 2. The owner takes the account back. ──
    new_seed = secrets.token_bytes(32)
    kit_secret = register_recovery_kit(
        nest["url"], actor_id_hex=owner["actor_id_hex"], identity_seed_hex=old_seed.hex()
    )
    successor_hex = succeed_identity(
        nest["url"],
        old_actor_id_hex=owner["actor_id_hex"],
        recovery_secret_hex=kit_secret,
        successor_seed_hex=new_seed.hex(),
        old_seed_hex=old_seed.hex(),
    )
    assert successor_hex == SigningKey(new_seed).verify_key.encode().hex(), (
        "the ceremony names the successor key the statement carried"
    )

    # ── 3. The successor authorizes the same app again, and the app attests
    # its ORIGINAL writer key. The recovery burned the app's roster row
    # (`actor_tables.rs`, `third_party_principals`), but the nest recorded the
    # row's writer before the burn and moved the record with the account
    # (`third_party_former_writers`), so `/oauth/token` admits the same app's
    # own key — `_authorize` asserts the token endpoint issues an access token
    # (`third-party-kinds.md` § Principal write authority → *A reconnecting
    # app keeps its writer*). The holder key is fresh, as a reinstall's. ──
    holder = x25519.X25519PrivateKey.generate()
    session, grant = _authorize(
        nest, metadata_server, new_seed, _raw(holder.public_key()), writer_pub
    )
    listed = _listed(session)
    assert any(r.get("origin_writer") == writer_pub.hex() for r in listed), (
        f"the app's note still rests on the nest after the recovery: {listed!r}"
    )
    assert _opened(listed, grant, _secret(holder)) == [], (
        "before the successor's device walks, the grant minted by the successor opens "
        f"nothing — the note is sealed under the predecessor's keys: {listed!r}"
    )

    # ── 4. The successor's device runs its passes. ──
    report = fauna_ffi.harness_successor_pass(nest["url"], new_seed, old_seed)
    assert report["attested"] == [owner["actor_id_hex"]], (
        f"the successor's registry attests the predecessor: {report!r}"
    )
    assert NOTES_SCOPE in report["ext_scopes"], (
        f"the successor's device walks the app's kind: {report!r}"
    )
    after = _opened(_listed(session), grant, _secret(holder))
    assert [(key, value) for _, key, value in after] == [(NOTE_KEY, NOTE)], (
        f"the app authorized again finds its note where it was: {after!r} "
        f"(the successor's passes: {report!r})"
    )
    # The app reconnected with `writer_pub`, yet the carried copy is authored by
    # the successor's DEVICE (the carry re-authors), never by the app's key.
    assert after[0][0] != writer_pub.hex(), (
        f"the note the grant opens is the successor device's re-authored copy: {after!r}"
    )
