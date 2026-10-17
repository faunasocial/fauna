"""tier_3 API e2e: the oracle's **``post.author``** class, end to end across a
real nest, a real PAR ceremony and the principal session
(``docs/goal/architecture/key-material-hierarchy.md`` § Audience: deployment
infrastructure → *The oracle*, the ``post.author`` row; the key:
``atproto-pds-full.md`` § D10).

The policy arms, the reader-side chain verify of the stored wire and the
roster record are proven in process (``bins/fauna-nest/tests/post_author_oracle.rs``);
approving the class provisions the delegation before the grant is minted
(``authorization-server.md`` § Scope grammar → *The planes ruled 2026-10-02*),
proven by the shared consent tests. What only a live nest proves is the
journey:

- a third-party client asks for ``fauna:identity:op:post.author
  fauna:feed:read`` and attests an X25519 holder key;
- the owner approves the way the shared approving path does — the account has
  no authoring delegation, so the D10 ceremony provisions one (the SAME
  shared-Rust minter all 7 apps use), then the keyless ``identity.op`` grant to
  the holder key is deposited, then the consent resolves;
- on the principal session, a public post is authored as the account and the
  account's local feed serves it; a create carrying an audience member
  (``gated``) is refused;
- the principal retracts its own post; an id it never made is refused;
- the owner revokes the delegation, and the very next create is refused.
"""

import secrets
import time

import pytest
from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric import ed25519, x25519

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
SCOPE = "fauna:identity:op:post.author fauna:feed:read"
REDIRECT_URI = "http://127.0.0.1:17778/callback"
CREATE = "fauna.posts.author.create"
DELETE = "fauna.posts.author.delete"
REFUSED = "fauna.posts.permission_denied"
#: The window the approving path provisions
#: (`fauna_client_bridges::atproto_delegation::DELEGATION_WINDOW_SECS`).
DELEGATION_WINDOW_MICROS = 90 * 24 * 60 * 60 * 1_000_000


@pytest.fixture(scope="module")
def metadata_server(tmp_path_factory):
    server = ClientMetadataServer(PUBLISHER, tmp_path_factory.mktemp("client-metadata"))
    publisher_key = ed25519.Ed25519PrivateKey.generate()
    payload = manifest_payload(PUBLISHER, publisher_key, ["notes"])
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


def _text(content: str) -> dict:
    return {"body": {"Text": {"content": content, "facets": []}}}


def _provision_delegation(ws, identity_secret: bytes) -> None:
    """The approving path's D10 ceremony when the account holds no live
    delegation (``consent_grant::provision_delegation``): fetch (mint) ``K_pub``,
    sign the cert over it with the shared minter, provision it."""
    import fauna_ffi

    k_pub = bytes(ws.call("fauna.bridges.atproto.fetch_authoring_key", {})["k_pub"])
    assert len(k_pub) == 32, k_pub
    now_micros = int(time.time() * 1_000_000)
    cert = fauna_ffi.build_authoring_delegation_cert(
        identity_secret, k_pub, ["Post", "UpdateProfile"],
        created_at_micros=now_micros,
        expires_at_micros=now_micros + DELEGATION_WINDOW_MICROS,
    )
    provisioned = ws.call("fauna.bridges.atproto.provision_authoring_delegation", {"cert": cert})
    assert provisioned.get("ok") is True, provisioned


@pytest.mark.feature("connected-apps")
def test_a_principal_authors_and_retracts_public_posts_until_the_delegation_is_revoked(
    consent_nest, consent_bridge, metadata_server
):
    nest = consent_nest
    identity_secret = bytes(nest["user"]["signing_key"])
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

    # ── The owner approves: provision (none held) → mint → resolve. ──
    with alice_client(nest) as ws:
        before = ws.call("fauna.bridges.atproto.fetch_authoring_delegation", {})
        assert not before.get("cert"), f"a fresh account holds no delegation: {before!r}"
        _provision_delegation(ws, identity_secret)
        after = ws.call("fauna.bridges.atproto.fetch_authoring_delegation", {})
        assert after.get("cert"), f"approving the class provisions the delegation: {after!r}"
    tokens = approve_with_identity_op_grant(
        nest, client, holder_pub, secrets.token_bytes(16), "post.author"
    )

    session = PrincipalSession(upgrade(nest, client, tokens["access_token"]))

    # ── A public post, authored as the account, served by its local feed. ──
    ok, reply = session.call(CREATE, _text("posted through the oracle"))
    assert ok, f"a public post is authored under the grant and the delegation: {reply!r}"
    post_id = reply["post_id"]
    ok, feed = session.call("fauna.feed.local.posts", {})
    assert ok, feed
    assert post_id in {p["post_id"] for p in feed["posts"]}, (
        f"the account's local feed serves the authored post: {feed!r}"
    )

    # ── An audience member: refused. ──
    ok, reply = session.call(CREATE, {**_text("for subscribers"), "gated": True})
    assert not ok and refusal_code(reply) == REFUSED, f"a gated create: {reply!r}"

    # ── Retraction: an id it never made is refused; its own is deleted. ──
    ok, reply = session.call(DELETE, {"post_id": "ab" * 32})
    assert not ok and refusal_code(reply) == REFUSED, f"an unknown id: {reply!r}"
    ok, reply = session.call(DELETE, {"post_id": post_id})
    assert ok and reply.get("deleted") is True, f"its own post: {reply!r}"
    ok, feed = session.call("fauna.feed.local.posts", {})
    assert ok, feed
    assert post_id not in {p["post_id"] for p in feed["posts"]}, feed

    # ── The owner revokes the delegation: the very next create is refused. ──
    with alice_client(nest) as ws:
        revoked = ws.call("fauna.bridges.atproto.revoke_authoring_delegation", {})
        assert revoked.get("revoked") is True, revoked
    ok, reply = session.call(CREATE, _text("after the revoke"))
    assert not ok and refusal_code(reply) == REFUSED, f"after the revoke: {reply!r}"
