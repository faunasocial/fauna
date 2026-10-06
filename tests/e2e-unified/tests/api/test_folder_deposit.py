"""tier_3 API e2e: **third-party folder deposit**, the nest side
(``docs/goal/behavior/file-sync.md`` § Third-party deposit ingress; the scope
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
  ``deposit`` grant (the production ``mint_folder_deposit_grant``, through
  ``fauna_ffi``) and approves choosing the folder — the card's choice,
  headless (§ Consent → *The card chooses the folder*) — and the token's
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
from tests.api.test_third_party_session import _alice, _refusal_code, _Session, _upgrade

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
    with _alice(nest) as ws:
        pending = ws.call("fauna.bridges.atproto.list_pending_consents", {})["consents"]
        consent = next((c for c in pending if c["code"] == browser_code), None)
        assert consent, f"no pending consent carries {browser_code!r}: {pending!r}"
        assert bytes(consent["holder_x25519"]) == holder_pub, consent
        assert consent["scopes"] == [scope], (
            f"the card carries the request's scope as PAR stored it: {consent!r}"
        )
        blob = fauna_ffi.build_folder_deposit_grant(
            bytes(alice["signing_key"]), secrets.token_bytes(16), holder_pub,
            now - 60, now + 3600, folder_id,
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
    with _alice(nest) as ws:
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
    session = _Session(_upgrade(nest, client, token))
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
                "to the owner (file-sync.md § Third-party deposit ingress)"
            )

    # ── Not yet a folder entry: adoption is the owner's next sync. ──
    with _alice(nest) as ws:
        files = ws.call("fauna.sync.files", {"folder": "drop-box"})
    assert files.get("files") == [], (
        f"a parked deposit is not a folder entry before a seat adopts it: {files!r}"
    )

    # ── Refusals. ──
    ok, reply = session.call(
        "fauna.folders.deposit",
        {"folder_id": other, "name": "x.txt", "body": b"x"},
    )
    assert not ok and _refusal_code(reply) == "fauna.folders.permission_denied", (
        f"a folder outside the scope is refused: {reply!r}"
    )
    status, body = _http_deposit(nest, client, token, other, "x.txt", b"x")
    assert status == 403 and body.get("error") == "fauna.folders.permission_denied", (
        f"the HTTP door refuses it alike: {status} {body!r}"
    )
    assert _inbox(nest, other) == [], "nothing parked in the out-of-scope folder"

    with _alice(nest) as ws:
        ws.call("fauna.folders.update", {"name": "drop-box", "residency": "metadata_only"})
    ok, reply = session.call(
        "fauna.folders.deposit",
        {"folder_id": folder, "name": "late.txt", "body": b"late"},
    )
    assert not ok and _refusal_code(reply) == "fauna.folders.permission_denied", (
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
    with _alice(nest) as ws:
        folder = ws.call("fauna.folders.create", {"name": "re-consent"})["id"]
    scope = f"{BARE_SCOPE}:{folder}"
    client, holder_pub = _client(nest, metadata_server, scope)
    tokens = _approve_with_grant(nest, client, holder_pub, folder, scope, choose=False)
    assert tokens.get("scope") == scope, tokens
    token = tokens["access_token"]

    session = _Session(_upgrade(nest, client, token))
    ok, reply = session.call(
        "fauna.folders.deposit",
        {"folder_id": folder, "name": "again.txt", "content_type": "text/plain", "body": b"a"},
    )
    assert ok and reply == {"accepted": True}, reply
    status, body = _http_deposit(nest, client, token, folder, "again-http.txt", b"b")
    assert status == 202 and body == {"accepted": True}, (status, body)
    assert len(_inbox(nest, folder)) == 2, "both deposits rest in the folder's inbox"
    session.ws.close()
