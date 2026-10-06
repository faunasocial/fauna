"""Cross-nest post forwarding over the federation channel.

Post ingest moved from the deleted HTTP twin ``POST /api/v1/forward`` to
``fauna.federation.post.forward`` on the nest↔nest federation channel
(``bins/fauna-nest/src/federation_handlers.rs``). On the channel the peer is
verified once at handshake, so the request drops the HTTP twin's relay
``signature`` / ``private_nest_id`` / ``timestamp`` / ``blobs`` body fields —
only ``post_bytes`` + the 100-byte ``post_envelope`` ride (the post's *own*
author sign-over-CID is still verified end-to-end). The byte fields ride the
dag-cbor wire as byte strings, like every raw-byte field — pass raw ``bytes``.

Read-back stays on the surviving HTTP route ``GET /api/v1/posts/{id}`` — a
cross-nest post-body fetch route kept as federation residue
(``bins/fauna-nest/src/lib.rs`` build_router; ``transport.md`` § HTTP residue).
The stored key is ``blake3(canonical_encode(EmbedAsBytes{envelope, bytes}))``,
reproduced by ``common.forwarded_post_id``.
"""
import time
import urllib.request

from nacl.signing import SigningKey

from clients.ws_rpc_federation_client import FederationChannelClient, RpcCallError
from common import (
    create_actor_and_register,
    encode_post_dagcbor,
    forwarded_post_id,
    sign_post_envelope,
)

import pytest


# ── Fixtures ─────────────────────────────────────────────────────────

pytestmark = pytest.mark.tier_3


@pytest.fixture()
def public_nest(request, nest_mode, tmp_path_factory):
    """The listener nest, of this run's mode."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "forward-public")
    yield nest
    cleanup()


@pytest.fixture()
def initiator_nest(request, nest_mode, tmp_path_factory):
    """A second nest that dials the federation channel (signs the hello).

    The forward handler runs under ``SubmissionPolicy::Open`` (default.toml), so
    it verifies the post's author envelope but does **not** gate on pairing — any
    well-formed peer nest can dial and forward.

    "Dials" is the harness's act, not this nest's: the channel client runs in
    the pytest process and signs the hello *as* this nest, so nothing here is
    class (8) and both nests can be containers.
    """
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "forward-initiator")
    yield nest
    cleanup()


# ── Tests ────────────────────────────────────────────────────────────

def test_forward_rejects_invalid_post(public_nest, initiator_nest):
    """post.forward rejects a post with an invalid author signature.

    Sends a well-formed ``FedPostForwardRequest`` — the inner ``post_bytes`` is a
    valid canonical-dag-cbor Post and ``post_envelope`` is a 100-byte
    ``CID || sig`` whose CID matches those bytes, but the author's sign-over-CID
    signature is zeroed → ``verify_envelope`` fails and the handler returns
    ``fauna.federation.invalid_params`` ("invalid post signature").
    """
    author = SigningKey.generate()
    post_bytes = encode_post_dagcbor(
        bytes(author.verify_key),
        int(time.time() * 1_000_000),
        "Hello",
    )
    import blake3
    cid = bytes([0x01, 0x71, 0x1E, 0x20]) + blake3.blake3(post_bytes).digest()
    bad_envelope = cid + b"\x00" * 64  # valid CID prefix, zeroed signature

    with FederationChannelClient(initiator=initiator_nest, target=public_nest) as fed:
        with pytest.raises(RpcCallError) as exc_info:
            fed.call("fauna.federation.post.forward", {
                "post_bytes": post_bytes,
                "post_envelope": bad_envelope,
            })
    assert exc_info.value.code == "fauna.federation.invalid_params"


def test_forward_rejects_malformed_payload(public_nest, initiator_nest):
    """post.forward rejects a structurally malformed payload.

    The WS-RPC analogue of the HTTP twin's "malformed JSON" 400: a payload that
    cannot decode into ``FedPostForwardRequest`` (here ``post_bytes`` is a string
    where a ``Vec<u8>`` int array is required) → ``fauna.protocol.malformed``.
    """
    with FederationChannelClient(initiator=initiator_nest, target=public_nest) as fed:
        with pytest.raises(RpcCallError) as exc_info:
            fed.call("fauna.federation.post.forward", {
                "post_bytes": "not-a-byte-array",
            })
    assert exc_info.value.code == "fauna.protocol.malformed"


def test_forwarding_roundtrip(public_nest, initiator_nest):
    """A signed post forwarded over the channel is retrievable via HTTP read-back.

    Drives ``fauna.federation.post.forward`` with a correctly sign-over-CID-signed
    post (the wire shape a private nest's outbox worker emits), then reads it back
    via the surviving ``GET /api/v1/posts/{id}``. Exercises the full sign-over-CID
    verification on the receiver: BLAKE3(post_bytes) == envelope.cid.digest, then
    Ed25519(sig, cid, author_pubkey).

    (Public nest runs ``default.toml`` → ``SubmissionPolicy::Open``, so the
    handler verifies the post envelope but skips the paired-nest gate. The post is
    stored keyed by ``blake3(canonical_encode(EmbedAsBytes))``, which
    ``forwarded_post_id`` reproduces.)
    """
    pub, initiator = public_nest, initiator_nest

    # Author registered on the public nest.
    actor = create_actor_and_register(pub["port"], admin_signing_key=pub["admin"]["signing_key"])

    # Sign-over-CID a dag-cbor Post as the author; build the 100-byte
    # envelope and forward it over the federation channel.
    post_bytes_hex, post_envelope_hex = sign_post_envelope(
        actor["signing_key"], "Hello from private nest!"
    )
    post_bytes = bytes.fromhex(post_bytes_hex)
    envelope_raw = bytes.fromhex(post_envelope_hex)
    expected_id = forwarded_post_id(post_bytes, envelope_raw)

    with FederationChannelClient(initiator=initiator, target=pub) as fed:
        fed.call("fauna.federation.post.forward", {
            "post_bytes": post_bytes,
            "post_envelope": envelope_raw,
        })

    # Verify the post appears on the public nest, keyed by the stored-wire
    # hash, via the kept federation-residue HTTP read route.
    read_req = urllib.request.Request(f"{pub['url']}/api/v1/posts/{expected_id}")
    read_resp = urllib.request.urlopen(read_req)
    assert read_resp.status == 200, "forwarded post should be available on public nest"
