"""tier_3 API e2e: **third-party kinds** — a connected app's own ``ext.*``
records, end to end over the principal session
(``docs/goal/architecture/third-party-kinds.md`` § The manifest, § Principal
write authority, § The record doors; the scope arm,
``docs/goal/behavior/authorization-server.md`` § Scope grammar → *The third
arm*).

The client is a real ``https`` document, ``https://example.com/…``, served on
loopback under a throwaway CA (``helpers.client_metadata_server``) and carrying
a two-kind manifest signed in Python — so the nest's manifest verify runs
against a signer that is not its own code. One journey:

- the client attests an X25519 holder key and an Ed25519 writer key at PAR
  and asks for ``fauna:records:rw:ext.example.com.*``;
- the owner approves: the owner's consent-time grant (the production
  ``mint_ext_kinds_grant``, through ``fauna_ffi``) is deposited, then the
  consent resolves — the order the approving machines run;
- the principal fetches its grant, opens the kind's pair, seals a row under
  ``ext:ext.example.com.notes`` as its writer, puts it, and lists it back;
- three refusals: a put as another ``writer_id``, a list of ``state``, a put
  under ``ext:ext.other.org.thing``;
- revoke closes the session ``4401``, and the owner's own feed still serves
  the row — the user's data outlives the app that wrote it.

A second test holds TP4 (``encryption-at-rest.md`` § Capability tiering →
*Third-party holders* (1)): a *remote*-form principal — a confidential client,
authenticated by its ``private_key_jwt`` assertion — that asks for a read of
the account's mail is refused before any card renders. No arm can name a
pre-existing sealed scope at all; the grammar is the bound.

The plane-row write and the ledger ``Mint`` of the owner's entry point are
pinned in Rust (``fauna_client_capabilities::ext_consent``); this journey is
the wire.
"""

import json
import secrets
import time
import urllib.request

import pytest
from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric import ec, ed25519, x25519

import fauna_ffi
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
from tests.api.test_third_party_session import (
    CLOSE_AUTH_EXPIRED,
    _alice,
    _refusal_code,
    _Session,
    _upgrade,
)

pytestmark = pytest.mark.tier_3

PUBLISHER = "example.com"
NOTES = f"ext.{PUBLISHER}.notes"
TASKS = f"ext.{PUBLISHER}.tasks"
RECORDS = f"fauna:records:rw:ext.{PUBLISHER}.*"
REDIRECT_URI = "http://127.0.0.1:17775/callback"
REMOTE_PATH = "/remote-client.json"
STATE_FEED = "state-entry"
#: The manifest's one `service_auth` entry (`third-party.md` § The manifest) —
#: what the consent saves on the roster row and `fauna.principals.list` serves.
SERVICE_AUTH = [
    {"aud": "did:web:api.bsky.app#bsky_appview", "lxm": ["app.bsky.feed.getFeedSkeleton"]}
]


@pytest.fixture(scope="module")
def metadata_server(tmp_path_factory):
    server = ClientMetadataServer(PUBLISHER, tmp_path_factory.mktemp("client-metadata"))
    publisher_key = ed25519.Ed25519PrivateKey.generate()
    payload = manifest_payload(PUBLISHER, publisher_key, ["notes", "tasks"])
    payload["service_auth"] = SERVICE_AUTH
    manifest = sign_manifest(publisher_key, payload)
    server.publisher_key = _raw(publisher_key.public_key())
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


def _approve_with_grant(
    nest, client, holder_pub: bytes, writer_pub: bytes, grant_id: bytes, kinds=(NOTES, TASKS)
):
    """PAR → the browser page → the owner deposits the consent-time grant and
    approves → the code is redeemed. Returns the token reply."""
    request_uri = client.push_authorization_request()
    browser_code, flow_token = client.open_consent_page(request_uri)
    alice = nest["user"]
    now = int(time.time())
    with _alice(nest) as ws:
        pending = ws.call("fauna.bridges.atproto.list_pending_consents", {})["consents"]
        consent = next((c for c in pending if c["code"] == browser_code), None)
        assert consent, f"no pending consent carries {browser_code!r}: {pending!r}"
        assert bytes(consent["holder_x25519"]) == holder_pub, consent
        assert bytes(consent["writer_ed25519"]) == writer_pub, consent
        assert consent.get("fauna_manifest"), f"the consent carries the verified manifest: {consent!r}"

        blob = fauna_ffi.build_ext_kinds_grant(
            bytes(alice["signing_key"]), grant_id, holder_pub, now, now + 3600,
            list(kinds), writer_pub,
        )
        minted = ws.call("fauna.capabilities.mint", {"grant_blob": blob})
        assert minted.get("ok") is True, minted
        resolved = ws.call(
            "fauna.bridges.atproto.resolve_consent",
            {"consent_id": consent["consent_id"], "approved": True},
        )
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


def _put(session, scope: str, writer_id: bytes, item_key: bytes, envelope: bytes, seq: int = 1):
    return session.call(
        "fauna.account.state.put",
        {
            "scope": scope,
            "writer_id": writer_id.hex(),
            "writer_seq": seq,
            "item_key": item_key,
            "op": "state-put",
            "entry": envelope,
        },
    )


@pytest.mark.feature("connected-apps")
def test_a_connected_app_writes_and_reads_exactly_its_own_kinds(
    consent_nest, consent_bridge, metadata_server
):
    nest = consent_nest
    holder = x25519.X25519PrivateKey.generate()
    writer = ed25519.Ed25519PrivateKey.generate()
    holder_pub, writer_pub = _raw(holder.public_key()), _raw(writer.public_key())
    client = OAuthClient(
        base=nest["url"],
        htu_origin=f"https://{HANDLE_DOMAIN}",
        redirect_uri=REDIRECT_URI,
        scope=RECORDS,
        holder_x25519=holder_pub,
        writer_ed25519=writer_pub,
        client_id_url=metadata_server.client_id(),
    )
    grant_id = secrets.token_bytes(16)
    tokens = _approve_with_grant(nest, client, holder_pub, writer_pub, grant_id)

    with _alice(nest) as ws:
        rows = [
            p for p in ws.call("fauna.principals.list", {})["principals"]
            if p["client_id"] == client.client_id
        ]
    assert len(rows) == 1, rows
    principal = rows[0]
    assert principal["execution_form"] == "device", "a public client is a device-form principal"
    assert bytes(principal["writer_ed25519"]) == writer_pub, principal
    # What the verified manifest declared rides to the roster row.
    assert sorted(principal["declared_kinds"]) == [NOTES, TASKS], principal
    assert bytes(principal["publisher_key"]) == metadata_server.publisher_key, principal
    assert principal.get("service_auth") == SERVICE_AUTH, principal

    # ── The principal's side: its grant, its pair, its row. ──
    session = _Session(_upgrade(nest, client, tokens["access_token"]))
    ok, reply = session.call("fauna.capabilities.fetch", {})
    assert ok, reply
    grants = [bytes(g) for g in reply.get("grants", [])]
    assert len(grants) == 1, f"the principal holds exactly the consent-time grant: {reply!r}"

    item_key, envelope = fauna_ffi.ext_kind_seal_row(
        grants[0], _secret(holder), _secret(writer), NOTES, "first-note", b"hello",
        1, int(time.time() * 1000),
    )
    notes_scope = f"ext:{NOTES}"
    ok, reply = _put(session, notes_scope, writer_pub, item_key, envelope)
    assert ok, f"a principal writes its own kind as its own writer: {reply!r}"

    ok, reply = session.call(
        "fauna.sync.changes.list", {"scope": notes_scope, "item_class": STATE_FEED}
    )
    assert ok, reply
    listed = [c for c in reply["changes"] if c.get("path_hash") == item_key.hex()]
    assert len(listed) == 1, f"the principal lists the row back: {reply!r}"
    assert listed[0].get("origin_writer") == writer_pub.hex(), listed[0]
    assert bytes(listed[0]["entry"]) == envelope, "the nest echoes the sealed envelope verbatim"

    # ── Three refusals. ──
    stranger = ed25519.Ed25519PrivateKey.generate()
    s_key, s_env = fauna_ffi.ext_kind_seal_row(
        grants[0], _secret(holder), _secret(stranger), NOTES, "forged", b"x",
        1, int(time.time() * 1000),
    )
    ok, reply = _put(session, notes_scope, _raw(stranger.public_key()), s_key, s_env)
    assert not ok and _refusal_code(reply).endswith("permission_denied"), (
        f"a principal writes only as the writer key it attested: {reply!r}"
    )
    ok, reply = session.call("fauna.sync.changes.list", {"scope": "state", "item_class": STATE_FEED})
    assert not ok and _refusal_code(reply).endswith("permission_denied"), (
        f"the account's own state is outside every records scope: {reply!r}"
    )
    ok, reply = _put(session, "ext:ext.other.org.thing", writer_pub, item_key, envelope, seq=2)
    assert not ok and _refusal_code(reply).endswith("permission_denied"), (
        f"another publisher's kind is outside this grant: {reply!r}"
    )

    # ── Revoke: the session closes; the owner still reads the row. ──
    with _alice(nest) as ws:
        revoked = ws.call(
            "fauna.principals.revoke", {"principal_id": bytes(principal["principal_id"])}
        )
        assert revoked["revoked"] is True, revoked
        assert session.close_code() == CLOSE_AUTH_EXPIRED, "the revoke closes the live socket 4401"
        owned = ws.call("fauna.sync.changes.list", {"scope": notes_scope, "item_class": STATE_FEED})
    kept = [c for c in owned["changes"] if c.get("path_hash") == item_key.hex()]
    assert len(kept) == 1, f"the owner's feed still serves the app's row after revoke: {owned!r}"


def _issuer(nest) -> str:
    with urllib.request.urlopen(
        f"{nest['url']}/.well-known/oauth-authorization-server", timeout=RPC_ROUNDTRIP_S
    ) as resp:
        return json.loads(resp.read())["issuer"]


@pytest.mark.feature("connected-apps")
def test_a_remote_principal_cannot_ask_for_the_accounts_mail(
    consent_nest, consent_bridge, metadata_server
):
    """TP4: a remote-form principal is grantable only over scopes it created.
    The confidential client authenticates (its assertion is valid) and its
    request is still refused — no ``fauna:`` arm names ``mail``, so a read of
    the account's pre-existing sealed scopes is not a card anyone can see."""
    nest = consent_nest
    mail_scope = "fauna:records:rw:mail"
    client = OAuthClient(
        base=nest["url"],
        htu_origin=f"https://{HANDLE_DOMAIN}",
        redirect_uri=REDIRECT_URI,
        scope=mail_scope,
        holder_x25519=_raw(x25519.X25519PrivateKey.generate().public_key()),
        client_id_url=metadata_server.client_id(REMOTE_PATH),
        assertion_key=ec.generate_private_key(ec.SECP256R1()),
        assertion_kid="k1",
        issuer=_issuer(nest),
    )
    metadata_server.documents[REMOTE_PATH] = client_document(
        client.client_id, REDIRECT_URI, mail_scope, jwk=client.assertion_jwk()
    )
    status, body = client.push_authorization_request_expecting_refusal()
    assert status == 400 and body.get("error") == "invalid_scope", (
        f"a remote principal's read of the account's mail is refused at the push: {status} {body!r}"
    )
    assert "can grant" in body.get("error_description", ""), (
        f"refused for the scope, after the client authenticated — not for its assertion: {body!r}"
    )
