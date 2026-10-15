"""tier_3 API e2e: the oracle's **``atproto.service_auth``** class, end to end
against the real Go PDS bridge (``docs/goal/architecture/key-material-hierarchy.md``
§ Audience: deployment infrastructure → *The oracle* → *The bridge-held
custodian*).

The repo signing key is sealed to the ``atproto.pds`` bridge and nowhere else,
so the class runs in two halves: the nest's door
``fauna.atproto.oracle.service_auth`` decides, and pushes the request to the
bridge, which mints with the key and answers over
``fauna.bridges.atproto.deliver_service_auth``. The policy arms and the
correlated round trip are tier_1 in ``bins/fauna-nest/src/atproto_oracle``; the
mint itself can only be proven here, because the bridge cannot run inside a
Rust test. One journey:

- a third-party client whose signed manifest declares one ``service_auth``
  entry asks for ``fauna:identity:op:atproto.service_auth`` and attests an
  X25519 holder key;
- the owner deposits the keyless ``identity.op`` grant to that key and
  approves;
- on the principal session, the declared ``(aud, lxm)`` returns a token that
  verifies ES256K against the signing key alice's DID document publishes —
  ``iss`` her DID, the pair as asked, ``exp`` within the 60 s cap — and the
  owner's roster read shows the one recorded operation;
- an undeclared ``aud``, an undeclared ``lxm`` and a denied ``lxm`` are refused;
- the owner revokes the grant, and the very next call is refused.
"""

import base64
import json
import secrets
import time

import pytest
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import ec, ed25519, x25519
from cryptography.hazmat.primitives.asymmetric.utils import encode_dss_signature

from helpers.atproto_consent import (  # noqa: F401 — consent_nest/consent_bridge are pytest fixtures adopted by import
    HANDLE_DOMAIN,
    consent_bridge,
    consent_nest,
)
from helpers.atproto_fakes import did_key_public_key
from helpers.client_metadata_server import (
    ClientMetadataServer,
    DOCUMENT_PATH,
    client_document,
    manifest_payload,
    sign_manifest,
)
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
SCOPE = "fauna:identity:op:atproto.service_auth"
REDIRECT_URI = "http://127.0.0.1:17776/callback"
KIND = "fauna.atproto.oracle.service_auth"
AUD = "did:web:feed.example.com#bsky_fg"
LXM = "app.bsky.feed.getFeedSkeleton"
#: The manifest's one `service_auth` entry (`third-party.md` § The manifest).
SERVICE_AUTH = [{"aud": AUD, "lxm": [LXM]}]
#: The class's lifetime cap (`fauna_bridge_atproto::service_auth`).
CAP_SECS = 60
REFUSED = "fauna.atproto.permission_denied"


@pytest.fixture(scope="module")
def metadata_server(tmp_path_factory):
    server = ClientMetadataServer(PUBLISHER, tmp_path_factory.mktemp("client-metadata"))
    publisher_key = ed25519.Ed25519PrivateKey.generate()
    payload = manifest_payload(PUBLISHER, publisher_key, ["notes"])
    payload["service_auth"] = SERVICE_AUTH
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


def _b64url(part: str) -> bytes:
    return base64.urlsafe_b64decode(part + "=" * (-len(part) % 4))


def _verify_es256k(token: str, did_key: str) -> dict:
    """Verify the compact JWS against ``did_key`` the way a service does —
    an independent verifier, never the minter's own code — and return its
    claims."""
    header_b64, claims_b64, sig_b64 = token.split(".")
    header = json.loads(_b64url(header_b64))
    assert header == {"typ": "JWT", "alg": "ES256K"}, header
    sig = _b64url(sig_b64)
    assert len(sig) == 64, f"an ES256K JWS signature is the 64-byte r‖s pair, got {len(sig)}"
    der = encode_dss_signature(int.from_bytes(sig[:32], "big"), int.from_bytes(sig[32:], "big"))
    did_key_public_key(did_key).verify(
        der, f"{header_b64}.{claims_b64}".encode(), ec.ECDSA(hashes.SHA256())
    )
    return json.loads(_b64url(claims_b64))


@pytest.mark.feature("connected-apps")
def test_a_principal_gets_a_service_token_minted_by_the_bridge_for_its_declared_pair_only(
    consent_nest, consent_bridge, metadata_server
):
    nest = consent_nest
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
        nest, client, holder_pub, grant_id, "atproto.service_auth"
    )

    with alice_client(nest) as ws:
        rows = [
            p for p in ws.call("fauna.principals.list", {})["principals"]
            if p["client_id"] == client.client_id
        ]
    assert len(rows) == 1, rows
    principal = rows[0]
    assert principal.get("service_auth") == SERVICE_AUTH, principal

    session = PrincipalSession(upgrade(nest, client, tokens["access_token"]))

    # ── The declared pair: the bridge mints with alice's repo signing key. ──
    before = int(time.time())
    ok, reply = session.call(KIND, {"aud": AUD, "lxm": LXM})
    assert ok, f"the declared (aud, lxm) must mint: {reply!r}\n{consent_bridge['tail']()}"
    did = consent_bridge["did"]
    signing_key = consent_bridge["directory"].standing_signing_key(did)
    assert signing_key, f"alice's DID document publishes no signing key: {did}"
    claims = _verify_es256k(reply["token"], signing_key)
    after = int(time.time())
    assert claims["iss"] == did, claims
    assert (claims["aud"], claims["lxm"]) == (AUD, LXM), claims
    assert claims.get("jti"), claims
    assert before < claims["exp"] <= after + CAP_SECS, f"exp outside the cap: {claims}"

    with alice_client(nest) as ws:
        ops = ws.call(
            "fauna.principals.ops", {"principal_id": principal["principal_id"]}
        )["ops"]
    assert [(o["class"], o["detail"]) for o in ops] == [
        ("atproto.service_auth", f"{AUD} {LXM}")
    ], ops

    # ── Outside the declared set, or on the deny list: refused. ──
    for payload, why in [
        ({"aud": "did:web:other.example.com", "lxm": LXM}, "an undeclared aud"),
        ({"aud": AUD, "lxm": "app.bsky.feed.getTimeline"}, "an undeclared lxm"),
        ({"aud": AUD, "lxm": "com.atproto.identity.submitPlcOperation"}, "a denied lxm"),
    ]:
        ok, reply = session.call(KIND, payload)
        assert not ok and refusal_code(reply) == REFUSED, f"{why}: {reply!r}"

    # ── The owner revokes the grant: the very next call is refused. ──
    with alice_client(nest) as ws:
        revoked = ws.call("fauna.capabilities.revoke", {"grant_id": grant_id})
        assert revoked["ok"] is True, revoked
    ok, reply = session.call(KIND, {"aud": AUD, "lxm": LXM})
    assert not ok and refusal_code(reply) == REFUSED, f"after the revoke: {reply!r}"
