"""tier_3 API e2e: **third-party folder deposit**, the nest side
(``docs/goal/behavior/folder-deposit.md``; the scope
arm, ``docs/goal/behavior/authorization-server.md`` § Scope grammar; the
keyless ``deposit`` class, ``docs/goal/architecture/encryption-at-rest.md``
§ Capability tiering → *Third-party holders*).

The app's client document declares the BARE ``fauna:folder:deposit`` — the
folder plane's qualifier is the user's, so no static document can name one
(``authorization-server.md`` § Scope grammar → *The folder plane's qualifier
is the user's*). Two journeys over a real consent ceremony:

- the owner makes a folder and publishes a recipient seal key it holds;
- a connected app attests an X25519 holder key and asks for the bare
  ``fauna:folder:deposit``; the owner deposits the keyless consent-time
  ``deposit`` grant — the production consent-time mint's deposit arm run over
  the card's own scopes and the chosen folder
  (``ext_consent::prepare_ext_consent_grant``, through ``fauna_ffi``) — and
  approves choosing the folder — the card's choice, headless (§ Consent →
  *The card chooses the folder*) — and the token's
  ``scope`` comes back ``fauna:folder:deposit:<that folder>``; a choice that is
  no folder of the account's resolves nothing first;
- the app deposits one file over each door — ``fauna.folders.deposit`` on its
  principal session, and ``POST /api/v1/folders/{id}/deposit`` with its
  DPoP-bound token — and learns ``accepted`` and nothing else;
- at rest: the folder's inbox segment holds two sealed items, and neither the
  file's bytes nor its name appears anywhere on the nest's disk;
- the deposits are not folder entries yet: the owner's ``fauna.sync.files``
  lists neither (adoption is the owner's next sync's);
- refusals: a deposit into another of the owner's folders (outside the
  scope), and into the scoped folder once it is metadata-only — nothing to
  park.
- the re-consent path: the same document, a request naming one folder's
  qualified string, approved with no choice; it deposits over both doors.
- a deposit larger than one message over each door — the session's staged
  upload and the HTTP door's streamed body — rests as one item of sealed
  parts, nothing of it in clear on disk, and a third is refused once the
  owner's inbox quota (the tier's ``max_inbox_bytes``) is reached.

That the parked item opens under the owner's recipient key is pinned in Rust
(``folder_deposit::tests``); this journey is the wire and the disk.
"""

import json
import pathlib
import secrets
import sqlite3
import time
import urllib.error
import urllib.parse
import urllib.request

import pytest
from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric import x25519

import fauna_ffi
from helpers.atproto_consent import (  # noqa: F401 — consent_nest/consent_bridge are pytest fixtures adopted by import
    HANDLE_DOMAIN,
    consent_bridge,
    consent_nest,
)
from helpers.budgets import RPC_ROUNDTRIP_S
from helpers.client_metadata_server import ClientMetadataServer, client_document
from helpers.oauth_client import NONCE_HEADER, OAuthClient
from helpers.recipient_seal_key import provision_recipient_seal_key
from helpers.principal_session import alice_client, PrincipalSession, refusal_code, upgrade

pytestmark = pytest.mark.tier_3

PUBLISHER = "example.com"
DOCUMENT = "/deposit-client.json"
BARE_SCOPE = "fauna:folder:deposit"
REDIRECT_URI = "http://127.0.0.1:17776/callback"
WS_MARKER = b"ws-deposit-plaintext-7c1f"
HTTP_MARKER = b"http-deposit-plaintext-a93e"
WS_NAME = "first-deposit-name-5d.txt"
HTTP_NAME = "second-deposit-name-b2.txt"


@pytest.fixture(scope="module")
def metadata_server(tmp_path_factory):
    server = ClientMetadataServer(PUBLISHER, tmp_path_factory.mktemp("client-metadata"))
    yield server
    server.close()


@pytest.fixture(scope="module")
def consent_nest_env(metadata_server):
    """The shared consent nest's env: its metadata fetcher pointed at
    ``metadata_server``."""
    return metadata_server.nest_env()


def _raw(public_key) -> bytes:
    return public_key.public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw)


def _approve_with_grant(
    nest, client, holder_pub: bytes, folder_id: int, scope: str, choose: bool
):
    """PAR → the browser page → the owner deposits the keyless ``deposit``
    grant and approves — sending ``folder_id`` as the card's choice when
    ``choose`` (a bare request), none for a qualified one — → the code is
    redeemed. Returns the token reply."""
    request_uri = client.push_authorization_request()
    browser_code, flow_token = client.open_consent_page(request_uri)
    alice = nest["user"]
    now = int(time.time())
    with alice_client(nest) as ws:
        pending = ws.call("fauna.bridges.atproto.list_pending_consents", {})["consents"]
        consent = next((c for c in pending if c["code"] == browser_code), None)
        assert consent, f"no pending consent carries {browser_code!r}: {pending!r}"
        assert bytes(consent["holder_x25519"]) == holder_pub, consent
        assert consent["scopes"] == [scope], (
            f"the card carries the request's scope as PAR stored it: {consent!r}"
        )
        blob = fauna_ffi.build_consent_deposit_grant(
            bytes(alice["signing_key"]), secrets.token_bytes(16), holder_pub,
            now - 60, now + 3600, consent["scopes"], folder_id if choose else None,
        )
        minted = ws.call("fauna.capabilities.mint", {"grant_blob": blob})
        assert minted.get("ok") is True, minted
        answer = {"consent_id": consent["consent_id"], "approved": True}
        if choose:
            # A choice that is no folder of this account's resolves nothing,
            # and leaves the card live.
            missed = ws.call(
                "fauna.bridges.atproto.resolve_consent",
                {**answer, "folder": folder_id + 1_000_000},
            )
            assert missed["resolved"] is False, missed
            answer["folder"] = folder_id
        resolved = ws.call("fauna.bridges.atproto.resolve_consent", answer)
        assert resolved["resolved"], resolved
    redirect = ""
    deadline = time.monotonic() + RPC_ROUNDTRIP_S
    while time.monotonic() < deadline:
        status, redirect = client.poll(flow_token)
        if status == "resolved":
            break
    assert redirect and "code=" in redirect, f"approval must release a code: {redirect!r}"
    tokens = client.exchange_code(redirect)
    assert tokens.get("access_token"), tokens
    return tokens


def _http_deposit(nest, client, token: str, folder_id: int, name: str, body: bytes):
    """``POST /api/v1/folders/{id}/deposit`` with the token and a fresh DPoP
    proof; one ``use_dpop_nonce`` retry, the protocol's own. Returns
    ``(status, json body)``."""
    path = f"/api/v1/folders/{folder_id}/deposit"
    htu = f"https://{HANDLE_DOMAIN}{path}"
    url = f"{nest['url']}{path}?{urllib.parse.urlencode({'name': name})}"

    def send():
        request = urllib.request.Request(
            url,
            data=body,
            method="POST",
            headers={
                "Authorization": f"DPoP {token}",
                "DPoP": client.dpop_proof("POST", htu, access_token=token),
                "Content-Type": "text/plain",
            },
        )
        try:
            with urllib.request.urlopen(request, timeout=RPC_ROUNDTRIP_S) as resp:
                return resp.status, dict(resp.headers), json.loads(resp.read() or b"{}")
        except urllib.error.HTTPError as refused:
            raw = refused.read()
            try:
                parsed = json.loads(raw) if raw else {}
            except ValueError:
                parsed = {"raw": raw.decode(errors="replace")}
            return refused.code, dict(refused.headers), parsed

    status, headers, parsed = send()
    lowered = {k.lower(): v for k, v in headers.items()}
    if status == 401 and "use_dpop_nonce" in lowered.get("www-authenticate", ""):
        client.nonce = lowered.get(NONCE_HEADER)
        status, _, parsed = send()
    return status, parsed


def _inbox(nest, folder_id: int) -> list[bytes]:
    conn = sqlite3.connect(f"file:{nest['db_path']}?mode=ro", uri=True, timeout=10.0)
    try:
        return [
            bytes(row[0])
            for row in conn.execute(
                "SELECT sealed FROM folder_deposit_inbox WHERE folder_id = ? ORDER BY id",
                (folder_id,),
            )
        ]
    finally:
        conn.close()


def _client(nest, metadata_server, scope: str):
    """A connected app whose document declares the bare folder scope, asking
    for ``scope``, with a fresh attested holder key."""
    metadata_server.documents[DOCUMENT] = client_document(
        metadata_server.client_id(DOCUMENT), REDIRECT_URI, BARE_SCOPE
    )
    holder_pub = _raw(x25519.X25519PrivateKey.generate().public_key())
    client = OAuthClient(
        base=nest["url"],
        htu_origin=f"https://{HANDLE_DOMAIN}",
        redirect_uri=REDIRECT_URI,
        scope=scope,
        holder_x25519=holder_pub,
        client_id_url=metadata_server.client_id(DOCUMENT),
    )
    return client, holder_pub


def _everything_on_disk(nest) -> list[tuple[pathlib.Path, bytes]]:
    """Every file under the nest's data directory — the database, its WAL,
    every segment store — so a marker is looked for everywhere it could rest."""
    root = pathlib.Path(nest["db_path"]).parent
    return [(p, p.read_bytes()) for p in root.rglob("*") if p.is_file()]


@pytest.mark.feature("connected-apps")
def test_a_connected_app_deposits_into_a_folder_blind_over_both_doors(
    consent_nest, consent_bridge, metadata_server
):
    nest = consent_nest
    alice = nest["user"]
    with alice_client(nest) as ws:
        folder = ws.call("fauna.folders.create", {"name": "drop-box"})["id"]
        other = ws.call("fauna.folders.create", {"name": "private"})["id"]
        # The owner's own recipient seal key, as the app publishes it — this
        # module's alice, never a run-shared identity.
        provision_recipient_seal_key(ws, alice["actor_id_bytes"])

    client, holder_pub = _client(nest, metadata_server, BARE_SCOPE)
    tokens = _approve_with_grant(nest, client, holder_pub, folder, BARE_SCOPE, choose=True)
    assert tokens.get("scope") == f"{BARE_SCOPE}:{folder}", (
        f"the ceremony qualified the bare scope with the card's choice: {tokens!r}"
    )
    token = tokens["access_token"]

    # ── Door one: the principal session. ──
    session = PrincipalSession(upgrade(nest, client, token))
    ok, reply = session.call(
        "fauna.folders.deposit",
        {"folder_id": folder, "name": WS_NAME, "content_type": "text/plain", "body": WS_MARKER},
    )
    assert ok, f"the session door accepts a deposit into the scoped folder: {reply!r}"
    assert reply == {"accepted": True}, f"the depositor learns 'accepted' and nothing else: {reply!r}"

    # ── Door two: the remote server's HTTP door, the same gate. ──
    status, body = _http_deposit(nest, client, token, folder, HTTP_NAME, HTTP_MARKER)
    assert status == 202 and body == {"accepted": True}, (
        f"the HTTP door accepts the same deposit: {status} {body!r}"
    )

    # ── At rest: two items in the inbox segment, sealed; nothing in clear. ──
    parked = _inbox(nest, folder)
    assert len(parked) == 2, f"both deposits rest in the folder's inbox segment: {len(parked)}"
    for path, raw in _everything_on_disk(nest):
        for marker in (WS_MARKER, HTTP_MARKER, WS_NAME.encode(), HTTP_NAME.encode()):
            assert marker not in raw, (
                f"{marker!r} rests in cleartext in {path} — a deposit must rest sealed "
                "to the owner (folder-deposit.md)"
            )

    # ── Not yet a folder entry: adoption is the owner's next sync. ──
    with alice_client(nest) as ws:
        files = ws.call("fauna.sync.files", {"folder": "drop-box"})
    assert files.get("files") == [], (
        f"a parked deposit is not a folder entry before a seat adopts it: {files!r}"
    )

    # ── Refusals. ──
    ok, reply = session.call(
        "fauna.folders.deposit",
        {"folder_id": other, "name": "x.txt", "body": b"x"},
    )
    assert not ok and refusal_code(reply) == "fauna.folders.permission_denied", (
        f"a folder outside the scope is refused: {reply!r}"
    )
    status, body = _http_deposit(nest, client, token, other, "x.txt", b"x")
    assert status == 403 and body.get("error") == "fauna.folders.permission_denied", (
        f"the HTTP door refuses it alike: {status} {body!r}"
    )
    assert _inbox(nest, other) == [], "nothing parked in the out-of-scope folder"

    with alice_client(nest) as ws:
        ws.call("fauna.folders.update", {"name": "drop-box", "residency": "metadata_only"})
    ok, reply = session.call(
        "fauna.folders.deposit",
        {"folder_id": folder, "name": "late.txt", "body": b"late"},
    )
    assert not ok and refusal_code(reply) == "fauna.folders.permission_denied", (
        f"a metadata-only folder keeps nothing here to park a deposit in: {reply!r}"
    )
    assert "metadata-only" in json.dumps(reply, default=str), (
        f"the refusal names why, so the owner's app can say what to change: {reply!r}"
    )
    assert len(_inbox(nest, folder)) == 2, "the refused deposit parked nothing"
    session.ws.close()


@pytest.mark.feature("connected-apps")
def test_a_qualified_request_against_the_bare_declaration_deposits_without_a_choice(
    consent_nest, consent_bridge, metadata_server
):
    """The re-consent path: a request naming one folder's qualified string
    narrows the document's bare declaration, so PAR admits it and the card
    has nothing to choose."""
    nest = consent_nest
    with alice_client(nest) as ws:
        folder = ws.call("fauna.folders.create", {"name": "re-consent"})["id"]
    scope = f"{BARE_SCOPE}:{folder}"
    client, holder_pub = _client(nest, metadata_server, scope)
    tokens = _approve_with_grant(nest, client, holder_pub, folder, scope, choose=False)
    assert tokens.get("scope") == scope, tokens
    token = tokens["access_token"]

    session = PrincipalSession(upgrade(nest, client, token))
    ok, reply = session.call(
        "fauna.folders.deposit",
        {"folder_id": folder, "name": "again.txt", "content_type": "text/plain", "body": b"a"},
    )
    assert ok and reply == {"accepted": True}, reply
    status, body = _http_deposit(nest, client, token, folder, "again-http.txt", b"b")
    assert status == 202 and body == {"accepted": True}, (status, body)
    assert len(_inbox(nest, folder)) == 2, "both deposits rest in the folder's inbox"
    session.ws.close()


# ── Deposits larger than one message, and the inbox quota ────────────────────

MIB = 1 << 20
#: Three and a half messages' worth — four parts on either door.
LARGE_BYTES = 3 * MIB + MIB // 2
#: Room for the two large deposits below (sealed overhead included), not a
#: third: the quota the journey runs into.
INBOX_QUOTA = 9 * MIB


def _large_body(tag: bytes) -> tuple[bytes, list[bytes]]:
    """``LARGE_BYTES`` of random bytes with a distinct marker at the start of
    every MiB, so a part resting in clear anywhere is caught whichever part
    it is. Returns the body and its markers."""
    body = bytearray(secrets.token_bytes(LARGE_BYTES))
    markers = []
    for i, offset in enumerate(range(0, LARGE_BYTES, MIB)):
        marker = b"%s-part-%d-%s" % (tag, i, secrets.token_hex(6).encode())
        body[offset : offset + len(marker)] = marker
        markers.append(marker)
    return bytes(body), markers


def _ws_large_deposit(session, folder: int, name: str, body: bytes):
    """``fauna.folders.deposit.{begin,part,commit}`` — the body in parts of
    one message's worth. Returns the first refusal as ``(False, reply)``, or
    ``(True, commit reply)``."""
    ok, begun = session.call("fauna.folders.deposit.begin", {"folder_id": folder})
    if not ok:
        return ok, begun
    upload = bytes(begun["upload"])
    parts = [body[i : i + MIB] for i in range(0, len(body), MIB)]
    for index, part in enumerate(parts):
        ok, reply = session.call(
            "fauna.folders.deposit.part",
            {"folder_id": folder, "upload": upload, "index": index, "body": part},
        )
        if not ok:
            return ok, reply
        assert reply == {"accepted": True}, reply
    return session.call(
        "fauna.folders.deposit.commit",
        {
            "folder_id": folder,
            "upload": upload,
            "name": name,
            "content_type": "application/octet-stream",
            "parts": len(parts),
        },
    )


def _committed_parts(nest, folder_id: int) -> list[int]:
    """Per committed item in the folder's inbox, oldest first: how many sealed
    parts rest beside it."""
    conn = sqlite3.connect(f"file:{nest['db_path']}?mode=ro", uri=True, timeout=10.0)
    try:
        return [
            row[0]
            for row in conn.execute(
                "SELECT (SELECT COUNT(*) FROM folder_deposit_parts p WHERE p.deposit_id = i.id)"
                "  FROM folder_deposit_inbox i"
                " WHERE i.folder_id = ? AND i.committed = 1 ORDER BY i.id",
                (folder_id,),
            )
        ]
    finally:
        conn.close()


def _admin(nest):
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    admin = nest["admin"]
    return WsRpcAdminClient(
        nest["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )


@pytest.fixture
def small_inbox_tier(consent_nest):
    """Arrangement, not the mutation under test: the admin puts the owner on
    a tier whose inbox quota is ``INBOX_QUOTA`` — the tier editor's own kinds
    — and puts them back afterwards, since the module shares the nest."""
    nest = consent_nest
    actor = nest["user"]["actor_id_bytes"]
    name = f"small-inbox-{secrets.token_hex(3)}"
    with _admin(nest) as ws:
        user = ws.call("fauna.admin.users.get", {"actor_id": actor})["user"]
        ws.call(
            "fauna.admin.tiers.create",
            {
                "name": name,
                "max_inbox_bytes": INBOX_QUOTA,
                "max_storage_bytes": 1 << 30,
                "max_devices": 5,
                "max_blob_size": 100 * MIB,
                "max_feeds": 10,
            },
        )
        ws.call(
            "fauna.admin.users.update",
            {"actor_id": actor, "tier": name, "label": user.get("label", "")},
        )
    yield
    with _admin(nest) as ws:
        ws.call(
            "fauna.admin.users.update",
            {"actor_id": actor, "tier": user["tier"], "label": user.get("label", "")},
        )


@pytest.mark.feature("connected-apps")
def test_a_deposit_larger_than_one_message_rests_sealed_and_the_inbox_quota_binds(
    consent_nest, consent_bridge, metadata_server, small_inbox_tier
):
    """A file of several MiB over each door: the session's staged upload
    (begin → parts → commit) and the HTTP door's streamed body, which the nest
    cuts into the same sealed parts. Both are accepted, rest as one item each
    whose every part is sealed (no part's bytes, nor the name, anywhere on the
    nest's disk), and a third is refused once the owner's inbox quota — the
    tier's ``max_inbox_bytes`` — is reached (``file-sync.md`` § Third-party
    deposit ingress → *Deposits larger than one message*, *The inbox quota*)."""
    nest = consent_nest
    with alice_client(nest) as ws:
        folder = ws.call("fauna.folders.create", {"name": "large-drop"})["id"]
        provision_recipient_seal_key(ws, nest["user"]["actor_id_bytes"])
    scope = f"{BARE_SCOPE}:{folder}"
    client, holder_pub = _client(nest, metadata_server, scope)
    tokens = _approve_with_grant(nest, client, holder_pub, folder, scope, choose=False)
    token = tokens["access_token"]
    session = PrincipalSession(upgrade(nest, client, token))

    ws_body, ws_markers = _large_body(b"ws-large")
    http_body, http_markers = _large_body(b"http-large")
    ws_name, http_name = "large-ws-scan-41.bin", "large-http-scan-8e.bin"

    # ── Door one: the staged upload on the principal session. ──
    ok, reply = _ws_large_deposit(session, folder, ws_name, ws_body)
    assert ok and reply == {"accepted": True}, (
        f"the session door accepts a deposit larger than one message: {reply!r}"
    )

    # ── Door two: one streamed POST, cut into parts nest-side. ──
    status, body = _http_deposit(nest, client, token, folder, http_name, http_body)
    assert status == 202 and body == {"accepted": True}, (
        f"the HTTP door accepts a body larger than one message: {status} {body!r}"
    )

    # ── At rest: two items of four sealed parts each; nothing in clear. ──
    assert _committed_parts(nest, folder) == [4, 4], (
        f"each deposit rests as one item and its four parts: {_committed_parts(nest, folder)}"
    )
    for path, raw in _everything_on_disk(nest):
        for marker in [*ws_markers, *http_markers, ws_name.encode(), http_name.encode()]:
            assert marker not in raw, (
                f"{marker!r} rests in cleartext in {path} — every part of a deposit "
                "rests sealed to the owner (folder-deposit.md)"
            )

    # ── The quota: a third large deposit does not fit, over either door. ──
    third, _ = _large_body(b"third")
    ok, reply = _ws_large_deposit(session, folder, "third.bin", third)
    assert not ok and refusal_code(reply) == "fauna.folders.inbox_full", (
        f"a part past the owner's inbox quota is refused: {reply!r}"
    )
    status, body = _http_deposit(nest, client, token, folder, "third-http.bin", third)
    assert status == 507 and body.get("error") == "fauna.folders.inbox_full", (
        f"the HTTP door refuses it alike: {status} {body!r}"
    )
    assert _committed_parts(nest, folder) == [4, 4], (
        "a refused deposit commits nothing — nothing half-adoptable rests"
    )
    session.ws.close()
