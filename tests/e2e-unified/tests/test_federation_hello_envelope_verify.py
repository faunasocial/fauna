"""tier_1: the harness's federation sign-over-CID VERIFIER, pinned both ways.

Pure Python — no nest, no driver, no cargo. This is the Python half of D1 ("the
nest↔nest federation wire signs over **canonical dag-cbor**, not hand-built
JSON"; `docs/goal/architecture/serialization.md` § Sign-over-CID), and the twin
of the Rust unit tests in `bins/fauna-nest/src/federation_sig.rs`.

**Why a second implementation of a verifier that already exists in Rust.** The
Rust handshake test (`bins/fauna-nest/tests/conformance_federation_channel.rs::
channel_handshake_succeeds_between_two_nests`) drives two real nests through the
real door and proves they agree — but both ends encode with the *same*
`serde_ipld_dagcbor`, so it would stay green if the wire silently moved to JSON:
they would drift together. Only an independent encoder can pin the
representation. `common.envelope` builds its bytes with `cbor2.dumps(...,
canonical=True)`, so a green verify here means the signer really did produce
canonical dag-cbor.

The consumer that makes this load-bearing rather than decorative:
`clients/ws_rpc_federation_client.py::_connect` now verifies the listener's
`fauna.federation.hello` **reply** envelope with `verify_dagcbor_envelope`
before returning a connected channel. That reply half had no cross-implementation
witness at all until 2026-08-17 — the Python client
checked only that the reply's `listener_nest_id` string matched.

Its predecessor, `bins/fauna-nest/tests/federation_dagcbor.rs`, was deleted in the
same commit: it POSTed `/api/v1/pair`, a route the WS-RPC-everywhere endgame
removed, AND it built the **untagged** envelope that
`federation_sig.rs` now deliberately rejects. It had asserted nothing for months.
"""

import json

import pytest

pytestmark = pytest.mark.tier_1

cbor2 = pytest.importorskip("cbor2")
nacl_signing = pytest.importorskip("nacl.signing")

from common.envelope import (  # noqa: E402
    FEDERATION_HELLO_V1,
    cid_of_dag_cbor,
    sign_dagcbor_envelope,
    sign_over_cid,
    verify_dagcbor_envelope,
)

# A *different* deployment-key context, to prove the tag is structural rather
# than decorative (`fauna_protocol::sig_domain`).
CERT_BINDING_V1 = b"fauna.cert.binding.v1\x00"


def _keypair(seed_byte: int):
    sk = nacl_signing.SigningKey(bytes([seed_byte]) * 32)
    return sk, bytes(sk.verify_key)


# The real signed tuple: `federation_channel.rs::FederationHelloSig`.
HELLO_SIG = {
    "initiator_nest_id": "aa" * 32,
    "listener_nest_id": "bb" * 32,
    "channel_nonce": "cc" * 32,
    "spki_sha256": "dd" * 32,
}


def test_tagged_round_trip_verifies():
    sk, pk = _keypair(0x11)
    env = sign_dagcbor_envelope(HELLO_SIG, sk, domain_tag=FEDERATION_HELLO_V1)
    assert verify_dagcbor_envelope(HELLO_SIG, env, pk, FEDERATION_HELLO_V1)


def test_a_tampered_field_is_rejected():
    sk, pk = _keypair(0x11)
    env = sign_dagcbor_envelope(HELLO_SIG, sk, domain_tag=FEDERATION_HELLO_V1)
    tampered = {**HELLO_SIG, "channel_nonce": "ce" * 32}
    assert not verify_dagcbor_envelope(tampered, env, pk, FEDERATION_HELLO_V1)


def test_a_different_signer_is_rejected():
    sk, _ = _keypair(0x11)
    _, other_pk = _keypair(0x22)
    env = sign_dagcbor_envelope(HELLO_SIG, sk, domain_tag=FEDERATION_HELLO_V1)
    assert not verify_dagcbor_envelope(HELLO_SIG, env, other_pk, FEDERATION_HELLO_V1)


def test_the_untagged_pre_nid_cross_proto_format_is_rejected():
    """The shape the deleted `federation_dagcbor.rs` built: a bare-CID signature.

    Twin of `federation_sig.rs::verify_rejects_untagged_federation_sig`. Federation
    is not live in alpha, so the tag is enforced with no legacy fallback.
    """
    sk, pk = _keypair(0x11)
    untagged = sign_dagcbor_envelope(HELLO_SIG, sk, domain_tag=b"")
    assert not verify_dagcbor_envelope(HELLO_SIG, untagged, pk, FEDERATION_HELLO_V1)


def test_a_signature_from_another_key_context_is_rejected():
    """Twin of `federation_sig.rs::verify_rejects_cross_context_tag`."""
    sk, pk = _keypair(0x11)
    cross = sign_dagcbor_envelope(HELLO_SIG, sk, domain_tag=CERT_BINDING_V1)
    assert not verify_dagcbor_envelope(HELLO_SIG, cross, pk, FEDERATION_HELLO_V1)


def test_an_envelope_signed_over_json_bytes_is_rejected():
    """**This is D1 itself** — twin of `verify_rejects_legacy_json_envelope`.

    An envelope built the old way (sign over `json.dumps` bytes under a dag-cbor
    codec byte) must not verify against the canonical-dag-cbor payload, proving
    the wire encoding genuinely changed and not merely the codec byte.
    """
    sk, pk = _keypair(0x11)
    json_bytes = json.dumps(HELLO_SIG, sort_keys=True).encode()
    legacy = sign_over_cid(json_bytes, sk, domain_tag=FEDERATION_HELLO_V1)
    assert not verify_dagcbor_envelope(HELLO_SIG, legacy, pk, FEDERATION_HELLO_V1)
    # ...and the two encodings really are different bytes, so the assertion above
    # cannot pass vacuously.
    assert cbor2.dumps(HELLO_SIG, canonical=True) != json_bytes


@pytest.mark.parametrize(
    "bad",
    ["", "zz", "ab" * 10, "ab" * 200, "0x" + "ab" * 99],
    ids=["empty", "not-hex", "too-short", "too-long", "hex-prefixed"],
)
def test_malformed_envelopes_return_false_rather_than_raising(bad):
    """A verifier a test asserts on must never raise on attacker-shaped input."""
    _, pk = _keypair(0x11)
    assert verify_dagcbor_envelope(HELLO_SIG, bad, pk, FEDERATION_HELLO_V1) is False


def test_the_cid_half_pins_the_dag_cbor_codec_prefix():
    """Step 1 of the two-step recipe compares all 36 CID bytes, prefix included."""
    cid = cid_of_dag_cbor(b"x")
    assert len(cid) == 36
    # v1 (0x01) + dag-cbor (0x71) + blake3-256 (0x1e) + length 32 (0x20).
    assert cid[:4] == bytes([0x01, 0x71, 0x1E, 0x20])
