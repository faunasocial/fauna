"""tier_3 API e2e: the oracle's **``activitypub.http_sig``** class, end to end
across a real nest, a real PAR ceremony and the principal session
(``docs/goal/architecture/key-material-hierarchy.md`` § Audience: deployment
infrastructure → *The oracle*, the ``activitypub.http_sig`` row).

The account's actor key never leaves the nest: a third-party principal asks the
custodian, over ``fauna.activitypub.oracle.http_sig``, for an HTTP Signature
over one outbound request and sends the request itself. The policy arms — the
JSON-LD spellings of a sovereign shape, the record's lifetime with the
principal — are proven in process (``bins/fauna-nest/tests/activitypub_oracle.rs``);
what only a live nest proves is the journey:

- the owner enables federation, so the account has a published actor key;
- a third-party client whose signed manifest declares one ``activitypub_peers``
  host asks for ``fauna:identity:op:activitypub.http_sig`` and attests an
  X25519 holder key; the owner deposits the keyless ``identity.op`` grant to
  that key and approves;
- on the principal session, an inbox ``POST`` of the account's own
  ``Create{Note}`` to the declared host is signed — the ``Signature`` verifies
  against the key the actor document publishes, over exactly the ``host``,
  ``date`` and ``digest`` the reply hands back — and the owner's roster read
  shows the one recorded operation;
- a host outside the declared set is refused, and so are the account-sovereign
  shapes: a ``Move`` of the actor and a ``Delete`` of the actor;
- the owner revokes the grant, and the very next call is refused.
"""

import base64
import hashlib
import json
import secrets
import urllib.request

import pytest
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import ed25519, padding, x25519

from helpers.atproto_consent import (  # noqa: F401 — consent_nest/consent_bridge are pytest fixtures adopted by import
    HANDLE_DOMAIN,
    consent_bridge,
    consent_nest,
)
from helpers.client_metadata_server import (
    ClientMetadataServer,
    DOCUMENT_PATH,
    client_document,
    manifest_payload,
    sign_manifest,
)
from helpers.fake_follower import parse_signature_header
from helpers.oauth_client import OAuthClient
from helpers.principal_session import (
    PrincipalSession,
    alice_client,
    approve_with_identity_op_grant,
    refusal_code,
    upgrade,
)

pytestmark = pytest.mark.tier_3

PUBLISHER = "example.com"
SCOPE = "fauna:identity:op:activitypub.http_sig"
REDIRECT_URI = "http://127.0.0.1:17779/callback"
KIND = "fauna.activitypub.oracle.http_sig"
#: The manifest's one declared fediverse host (`third-party.md` § The manifest).
PEER = "mastodon.example"
INBOX_PATH = "/users/bob/inbox"
REFUSED = "fauna.activitypub.permission_denied"


@pytest.fixture(scope="module")
def consent_nest_binary(ap_binary):
    """The consent nest WITH the ``activitypub`` feature: the default build
    registers no ActivityPub provider, so federation cannot be enabled and the
    custodian has no actor key to sign with."""
    return ap_binary


@pytest.fixture(scope="module")
def metadata_server(tmp_path_factory):
    server = ClientMetadataServer(PUBLISHER, tmp_path_factory.mktemp("client-metadata"))
    publisher_key = ed25519.Ed25519PrivateKey.generate()
    payload = manifest_payload(PUBLISHER, publisher_key, ["notes"])
    payload["activitypub_peers"] = [PEER]
    server.documents[DOCUMENT_PATH] = client_document(
        server.client_id(), REDIRECT_URI, SCOPE, manifest_jws=sign_manifest(publisher_key, payload)
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


def _activity(actor_url: str, kind: str, obj) -> bytes:
    return json.dumps({
        "@context": "https://www.w3.org/ns/activitystreams",
        "id": f"{actor_url}/activities/{secrets.token_hex(4)}",
        "type": kind,
        "actor": actor_url,
        "object": obj,
    }).encode()


def _published_key(nest, username: str) -> str:
    """The RSA public key the account's actor document publishes — what a
    receiving server verifies against."""
    req = urllib.request.Request(
        f"{nest['url']}/ap/users/{username}", headers={"Accept": "application/activity+json"}
    )
    with urllib.request.urlopen(req) as resp:
        assert resp.status == 200, f"actor document unavailable: {resp.status}"
        return json.loads(resp.read())["publicKey"]["publicKeyPem"]


def _verify(reply: dict, public_pem: str, method: str, path: str, body: bytes) -> None:
    """Verify the reply's draft-cavage ``Signature`` the way a receiving inbox
    does — an independent verifier, never the custodian's own code — over the
    header values the reply hands back."""
    parsed = parse_signature_header(reply["signature"])
    assert parsed.get("algorithm") == "rsa-sha256", parsed
    assert parsed["keyId"] == reply["key_id"], parsed
    names = parsed["headers"].split()
    for covered in ("(request-target)", "host", "date", "digest"):
        assert covered in names, f"{covered} is not covered: {names}"
    digest = "SHA-256=" + base64.b64encode(hashlib.sha256(body).digest()).decode()
    assert reply["digest"] == digest, "the digest does not cover the body shown"
    values = {
        "(request-target)": f"{method} {path}",
        "host": reply["host"],
        "date": reply["date"],
        "digest": reply["digest"],
    }
    signing_string = "\n".join(f"{n}: {values[n]}" for n in names)
    serialization.load_pem_public_key(public_pem.encode()).verify(
        base64.b64decode(parsed["signature"]),
        signing_string.encode(),
        padding.PKCS1v15(),
        hashes.SHA256(),
    )  # raises InvalidSignature on mismatch


@pytest.mark.feature("connected-apps")
def test_a_principal_gets_signatures_for_its_declared_servers_only_never_a_move_or_delete(
    consent_nest, consent_bridge, metadata_server
):
    nest = consent_nest

    # ── The owner enables federation: the account gets a published actor key. ──
    with alice_client(nest) as ws:
        linked = ws.call(
            "fauna.bridges.link", {"bridge_id": "activitypub", "mode": "enable", "params": {}}
        )
    assert linked["linked"] is True, linked
    actor_url = linked["identity"]["value"]
    public_pem = _published_key(nest, actor_url.rsplit("/", 1)[-1])

    # ── The consent ceremony with the keyless identity.op grant. ──
    holder = x25519.X25519PrivateKey.generate()
    holder_pub = _raw(holder.public_key())
    client = OAuthClient(
        base=nest["url"],
        htu_origin=f"https://{HANDLE_DOMAIN}",
        redirect_uri=REDIRECT_URI,
        scope=SCOPE,
        holder_x25519=holder_pub,
        client_id_url=metadata_server.client_id(),
    )
    grant_id = secrets.token_bytes(16)
    tokens = approve_with_identity_op_grant(
        nest, client, holder_pub, grant_id, "activitypub.http_sig"
    )
    with alice_client(nest) as ws:
        rows = [
            p for p in ws.call("fauna.principals.list", {})["principals"]
            if p["client_id"] == client.client_id
        ]
    assert len(rows) == 1, rows
    principal = rows[0]
    assert principal.get("activitypub_peers") == [PEER], principal

    session = PrincipalSession(upgrade(nest, client, tokens["access_token"]))
    inbox = f"https://{PEER}{INBOX_PATH}"
    note = _activity(actor_url, "Create", {
        "id": f"{actor_url}/notes/{secrets.token_hex(4)}",
        "type": "Note",
        "attributedTo": actor_url,
        "content": "signed through the oracle",
    })

    # ── The declared server: signed with the account's published actor key. ──
    ok, reply = session.call(KIND, {"method": "post", "target": inbox, "body": note})
    assert ok, f"a declared server must be signed for: {reply!r}"
    assert reply["host"] == PEER, reply
    assert reply["key_id"] == f"{actor_url}#main-key", reply
    _verify(reply, public_pem, "post", INBOX_PATH, note)

    with alice_client(nest) as ws:
        ops = ws.call("fauna.principals.ops", {"principal_id": principal["principal_id"]})["ops"]
    assert [(o["class"], o["detail"]) for o in ops] == [
        ("activitypub.http_sig", f"post {PEER}")
    ], ops

    # ── An undeclared server, a Move, a Delete of the actor: refused. ──
    for payload, why in [
        ({"method": "post", "target": "https://elsewhere.example/inbox", "body": note},
         "an undeclared server"),
        ({"method": "post", "target": inbox,
          "body": _activity(actor_url, "Move", actor_url)}, "a Move of the account"),
        ({"method": "post", "target": inbox,
          "body": _activity(actor_url, "Delete", actor_url)}, "a Delete of the account"),
    ]:
        ok, reply = session.call(KIND, payload)
        assert not ok and refusal_code(reply) == REFUSED, f"{why}: {reply!r}"

    with alice_client(nest) as ws:
        ops = ws.call("fauna.principals.ops", {"principal_id": principal["principal_id"]})["ops"]
    assert len(ops) == 1, f"a refusal records no operation: {ops}"

    # ── The owner revokes the grant: the very next call is refused. ──
    with alice_client(nest) as ws:
        revoked = ws.call("fauna.capabilities.revoke", {"grant_id": grant_id})
        assert revoked["ok"] is True, revoked
    ok, reply = session.call(KIND, {"method": "post", "target": inbox, "body": note})
    assert not ok and refusal_code(reply) == REFUSED, f"after the revoke: {reply!r}"
