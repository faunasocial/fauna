"""Sign-over-CID envelope helpers for Fauna E2E tests.

The nest↔nest federation sync wire (`/api/v1/nest-sync/*`, `/api/v1/forward`)
uses the **sign-over-CID** scheme described in
``docs/goal/architecture/serialization.md`` — the signer does NOT sign the
payload bytes directly; it signs the payload's *CID*. The wire envelope is a
fixed 100-byte buffer: ``36-byte CID || 64-byte Ed25519 signature``,
hex-encoded.

CID layout (``libs/fauna-cbor/src/cid.rs``), 36 bytes::

    byte 0:      0x01   version (CIDv1)
    byte 1:      0x71   codec (dag-cbor)
    byte 2:      0x1e   multihash code (blake3-256)
    byte 3:      0x20   digest length (32)
    bytes 4-35:  32-byte BLAKE3-256 digest of the payload bytes

The signer signs the 36 CID bytes (``SignedEnvelope::sign`` in
``libs/fauna-cbor/src/envelope.rs``):

    cid    = 0x01 0x71 0x1e 0x20 || blake3_256(payload_bytes)
    sig    = ed25519_sign(signing_key, cid)            # 64 bytes
    wire   = cid || sig                                # 100 bytes

All routes feed the CID with **canonical dag-cbor**:

* **Pairing / revoke + nest-sync** — the nest reconstructs the signed payload
  into a shared typed struct (``bins/fauna-nest/src/federation_sig.rs``) and runs
  ``Cid::of_dag_cbor`` over its ``encode_canonical`` (canonical dag-cbor) bytes.
  ``cbor2.dumps(payload, canonical=True)`` is byte-identical to
  ``encode_canonical(&struct)`` for these string/int/bool maps — canonical CBOR
  sorts map keys **length-first then bytewise** (NOT JSON's lexicographic
  ``sort_keys``). This closed Layer-4 debt #1 (the wire previously hashed
  ``serde_json::to_vec`` of a ``json!`` literal under a dag-cbor codec byte).

* **Forwarded post** — ``post_bytes`` is the *canonical dag-cbor* of the
  inner ``fauna_core::data::Post`` value (``encode_canonical`` =
  ``serde_ipld_dagcbor::to_vec``). ``post_envelope`` is the sign-over-CID
  envelope of those dag-cbor bytes. ``cbor2.dumps(value, canonical=True)``
  is byte-identical to ``serde_ipld_dagcbor::to_vec`` for the Post shape —
  verified against a Rust reference. Note ``ActorId([u8; 32])`` rides as a
  32-byte CBOR **byte string**, like every fixed-width id (serialization.md
  § "Fixed-size byte arrays"); ``Timestamp(u64)`` as a uint; and ``PostBody`` as an
  externally-tagged single-key map (``{"Text": {...}}``).
"""

from __future__ import annotations

import time
from typing import Any, Optional

import blake3
import cbor2

# CID prefix: v1 (0x01) + dag-cbor codec (0x71) + blake3-256 (0x1e) + len 32 (0x20).
CID_DAG_CBOR_PREFIX = bytes([0x01, 0x71, 0x1E, 0x20])
# The same under the raw codec (0x55) — a `ContentHash` (chunk / blob content
# address; `pub type ContentHash = fauna_cbor::Cid` in `fauna-core/src/data.rs`).
CID_RAW_PREFIX = bytes([0x01, 0x55, 0x1E, 0x20])


def cid_link(cid: bytes) -> cbor2.CBORTag:
    """A 36-byte ``fauna_cbor::Cid`` as the dag-cbor value its serde impl emits.

    Every ``Cid``/``ContentHash`` field rides as an IPLD link: tag 42 over a
    byte string of ``0x00`` followed by the 36 CID bytes, and the nest's strict
    decoder refuses the bare byte string
    (``docs/goal/architecture/serialization.md`` § Canonical IPLD dag-cbor).
    ``cbor2.dumps(..., canonical=True)`` encodes the tag exactly as
    ``encode_canonical`` does.
    """
    assert len(cid) == 36, f"a Fauna CID is 36 bytes, got {len(cid)}"
    return cbor2.CBORTag(42, b"\x00" + bytes(cid))


def cid_from_link(value: Any) -> bytes:
    """The 36 CID bytes of a decoded ``Cid``/``ContentHash`` field (inverse of
    :func:`cid_link`); cbor2 hands a tag-42 link back as a ``CBORTag``."""
    assert isinstance(value, cbor2.CBORTag) and value.tag == 42, (
        f"expected a tag-42 CID link, got {value!r}"
    )
    raw = bytes(value.value)
    assert len(raw) == 37 and raw[0] == 0, f"malformed CID link payload: {raw.hex()}"
    return raw[1:]

# Domain-separation tag for the nest↔nest federation handshake
# signature. The nest's deployment key is its single identity and signs
# in several contexts; the federation hello signs over ``TAG || cid`` so a
# federation signature can never be reinterpreted in another context. Byte-for-
# byte mirror of the Rust source of truth ``fauna_protocol::sig_domain::
# FEDERATION_HELLO_V1`` (consumed by ``bins/fauna-nest/src/federation_sig.rs``);
# convention ``b"fauna.<context>.v<n>\0"`` — the trailing NUL makes every tag
# self-delimiting. Only ``sign_as_nest`` (the deployment-key signer) passes it;
# the actor-key post/contact-request signers below stay untagged.
FEDERATION_HELLO_V1 = b"fauna.federation.hello.v1\x00"


# ── CID + envelope primitives ──────────────────────────────────────────────


def canonical_dagcbor_bytes(payload: dict[str, Any]) -> bytes:
    """The bytes the nest hashes for the federation sign-over-CID routes.

    Byte-identical to ``encode_canonical(&struct)`` (= ``serde_ipld_dagcbor``)
    for the string/int/bool maps the pairing + nest-sync routes sign
    (``bins/fauna-nest/src/federation_sig.rs``). cbor2's ``canonical=True`` sorts
    map keys length-first then bytewise — matching dag-cbor, NOT JSON's
    lexicographic ``sort_keys``.
    """
    return cbor2.dumps(payload, canonical=True)


def cid_of_dag_cbor(payload_bytes: bytes) -> bytes:
    """The 36-byte CID of ``payload_bytes`` under the dag-cbor codec.

    Matches ``fauna_cbor::Cid::of_dag_cbor`` — the codec byte is dag-cbor
    regardless of whether ``payload_bytes`` is JSON or dag-cbor; the receiver
    only re-hashes the same bytes, so the codec byte is informational here.
    """
    digest = blake3.blake3(payload_bytes).digest()  # 32 bytes
    return CID_DAG_CBOR_PREFIX + digest


def sign_over_cid(payload_bytes: bytes, signing_key, domain_tag: bytes = b"") -> str:
    """Build the hex 100-byte envelope (CID || sig) over ``payload_bytes``.

    ``signing_key`` is a PyNaCl ``nacl.signing.SigningKey``. The signature is
    Ed25519 over the 36 CID bytes (sign-over-CID), matching
    ``SignedEnvelope::sign`` (``sk.sign(cid.as_bytes())``). The **wire** is still
    ``cid || sig``; only the signed message changes.

    ``domain_tag`` (default empty = the generic untagged primitive, unchanged for
    actor-key post / contact-request signers) prepends a domain-separation prefix
    to the signed message — the signature then covers ``domain_tag || cid``. The
    nest deployment-key federation signer passes :data:`FEDERATION_HELLO_V1`
    (mirrors ``federation_sig.rs`` `sign_payload`).
    """
    cid = cid_of_dag_cbor(payload_bytes)
    sig = signing_key.sign(domain_tag + cid).signature  # 64 bytes, over TAG||cid
    assert len(cid) == 36 and len(sig) == 64
    return (cid + sig).hex()


def sign_dagcbor_envelope(
    payload: dict[str, Any], signing_key, domain_tag: bytes = b""
) -> str:
    """Convenience: canonical-dag-cbor-encode ``payload`` then sign-over-CID.

    Returns the hex 100-byte envelope. Use for the federation hello handshake
    (``sign_as_nest`` passes ``domain_tag=FEDERATION_HELLO_V1``); the nest-side
    payload is reconstructed into a typed struct and re-encoded with
    ``encode_canonical``. ``domain_tag`` defaults to empty (untagged).
    """
    return sign_over_cid(canonical_dagcbor_bytes(payload), signing_key, domain_tag)


def verify_over_cid(
    payload_bytes: bytes,
    envelope_hex: str,
    verify_key_bytes: bytes,
    domain_tag: bytes = b"",
) -> bool:
    """Verify a hex 100-byte ``CID || sig`` envelope over ``payload_bytes``.

    The independent-implementation twin of ``federation_sig.rs`` `verify_payload`,
    running the same two checks the generic recipe specifies
    (``serialization.md`` § Sign-over-CID) with **no CBOR decoder in the path**:

    1. the 36 CID bytes equal ``CID_DAG_CBOR_PREFIX || blake3(payload_bytes)`` —
       a re-hash of the exact bytes the caller re-derived, so a signer that
       encoded something other than canonical dag-cbor fails here;
    2. Ed25519 verify of the 64-byte signature over ``domain_tag || cid``.

    Returns ``False`` on any shape / hash / signature failure — never raises, so
    a caller can assert on the boolean.
    """
    # Lazy nacl import, mirroring `common.helpers.sign_as_nest` — this module is
    # imported at collection time on machines that may not have PyNaCl yet.
    from nacl.exceptions import BadSignatureError
    from nacl.signing import VerifyKey

    try:
        raw = bytes.fromhex(envelope_hex)
    except (ValueError, TypeError):
        return False
    if len(raw) != 100:
        return False
    cid, sig = raw[:36], raw[36:]
    # (1) the CID half. Comparing the whole 36 bytes also pins the codec prefix.
    if cid != cid_of_dag_cbor(payload_bytes):
        return False
    # (2) the signature half, over the domain-tagged CID.
    try:
        VerifyKey(verify_key_bytes).verify(domain_tag + cid, sig)
    except (BadSignatureError, ValueError, TypeError):
        return False
    return True


def verify_dagcbor_envelope(
    payload: dict[str, Any],
    envelope_hex: str,
    verify_key_bytes: bytes,
    domain_tag: bytes = b"",
) -> bool:
    """Convenience: canonical-dag-cbor-encode ``payload``, then verify-over-CID.

    The verifying twin of :func:`sign_dagcbor_envelope`. Because it re-derives the
    signed bytes with cbor2's canonical encoder instead of ``serde_ipld_dagcbor``,
    a green call is a **cross-implementation** pin that the signer really did sign
    canonical dag-cbor — which a same-encoder round trip cannot prove, since both
    ends would drift together.
    """
    return verify_over_cid(
        canonical_dagcbor_bytes(payload), envelope_hex, verify_key_bytes, domain_tag
    )


# ── inner Post (dag-cbor) for /api/v1/forward ───────────────────────────────


def encode_post_dagcbor(
    author_pubkey: bytes,
    created_at_us: int,
    body_text: str,
    *,
    facets: Optional[list] = None,
    references: Optional[list] = None,
    expires_at: Optional[int] = None,
    gated: Optional[Any] = None,
    content_warning: Optional[str] = None,
) -> bytes:
    """Canonical dag-cbor of a ``fauna_core::data::Post`` with a Text body.

    Byte-identical to ``encode_canonical(&post)`` (= ``serde_ipld_dagcbor``):
    fields ride as a string-keyed map (canonical-sorted by cbor2), ``author``
    as a 32-byte byte string, ``created_at`` as a uint, ``body`` as an
    externally-tagged ``{"Text": {...}}`` map. Sign-over-CID puts no signature
    on the struct.
    """
    post: dict[str, Any] = {
        "author": bytes(author_pubkey),
        "created_at": created_at_us,
        "body": {"Text": {"content": body_text, "facets": facets or []}},
        "references": references or [],
        "expires_at": expires_at,
        "gated": gated,
        "content_warning": content_warning,
    }
    return cbor2.dumps(post, canonical=True)


def sign_post_envelope(signing_key, body_text: str, *, created_at_us: Optional[int] = None):
    """Produce ``(post_bytes_hex, post_envelope_hex)`` for ``/api/v1/forward``.

    The inner Post is canonical dag-cbor; the envelope is sign-over-CID by the
    post author. Returns hex strings ready for the ``post_bytes`` /
    ``post_envelope`` fields of a ``ForwardedPost``.
    """
    pubkey = bytes(signing_key.verify_key)
    if created_at_us is None:
        created_at_us = int(time.time() * 1_000_000)
    post_bytes = encode_post_dagcbor(pubkey, created_at_us, body_text)
    post_envelope = sign_over_cid(post_bytes, signing_key)
    return post_bytes.hex(), post_envelope


def wrap_embed_as_bytes(envelope_raw: bytes, inner_bytes: bytes) -> bytes:
    """Canonical dag-cbor of an ``EmbedAsBytes { envelope, bytes }`` struct.

    Both fields ride as CBOR byte strings (``serde_bytes``); canonical key
    order is length-first, so ``bytes`` precedes ``envelope``. Byte-identical
    to ``encoding::canonical_encode(&EmbedAsBytes { .. })`` — the CBOR-DAG-
    everywhere Layer-6 at-rest flip moved this wrapper from BARE to canonical
    dag-cbor. Public: ``tests.api.bare.sign_and_encode_post`` composes it with
    ``cid_of_dag_cbor`` to build the signed-post wire shape
    ``fauna.posts.create`` verifies.
    """
    return cbor2.dumps({"bytes": inner_bytes, "envelope": envelope_raw}, canonical=True)


def forwarded_post_id(post_bytes: bytes, envelope_raw: bytes) -> str:
    """The hex post_id the ``/api/v1/forward`` route assigns a forwarded post.

    The route stores the post keyed by
    ``blake3(canonical_encode(EmbedAsBytes{ envelope, bytes }))``
    (``forward_routes.rs`` step 3). ``GET /api/v1/posts/{id}`` reads back by
    this same key.
    """
    return blake3.blake3(wrap_embed_as_bytes(envelope_raw, post_bytes)).digest().hex()


# ── format-2 (ContactRequest, Post) tuples (sign-over-CID dag-cbor) ──
#
# Mirrors ``fauna_client_core::email::build_signed_email`` on the sign-over-CID
# rails: the payload is a canonical-dag-cbor 2-element array
# ``(EmbedAsBytes-cr, EmbedAsBytes-post)`` where each member wraps a sign-over-CID
# canonical-dag-cbor ContactRequest / Post (the old BARE-with-embedded-signature
# `Signable` shape is gone). Verified byte-for-byte against an ``encode_canonical``
# / ``sign_envelope`` Rust reference.


def encode_structured_post_dagcbor(
    author_pubkey: bytes,
    created_at_us: int,
    schema: str,
    fields: list[tuple[str, str]],
    content: Optional[str] = None,
) -> bytes:
    """Canonical dag-cbor of a ``Post`` with a ``PostBody::Structured`` body.

    ``fields`` is a list of ``(key, value)`` → ``[{"key": k, "value": v}]``.
    Matches ``encode_canonical(&post)``: ``body`` is the externally-tagged
    ``{"Structured": {schema, fields, content, facets, items}}`` map (cbor2
    canonical-sorts the keys), ``author`` a 32-byte byte string, ``created_at`` a uint.
    """
    post: dict[str, Any] = {
        "author": bytes(author_pubkey),
        "created_at": created_at_us,
        "body": {"Structured": {
            "schema": schema,
            "fields": [{"key": k, "value": v} for k, v in fields],
            "content": content,
            "facets": [],
            "items": [],
        }},
        "references": [],
        "expires_at": None,
        "gated": None,
        "content_warning": None,
    }
    return cbor2.dumps(post, canonical=True)


def encode_contact_request_dagcbor(
    sender_pubkey: bytes,
    post_cid: bytes,
    sender_node: bytes,
    summary: str,
    created_at_us: int,
) -> bytes:
    """Canonical dag-cbor of a ``ContactRequest``.

    Matches ``encode_canonical(&cr)``: ``sender`` a 32-byte byte string,
    ``post_id`` the 36-byte CID as a tag-42 link (:func:`cid_link`),
    ``sender_node`` a byte string (``serde_bytes``),
    ``summary`` a string, ``created_at`` a uint.
    """
    cr: dict[str, Any] = {
        "sender": bytes(sender_pubkey),
        "post_id": cid_link(post_cid),
        "sender_node": bytes(sender_node),
        "summary": summary,
        "created_at": created_at_us,
    }
    return cbor2.dumps(cr, canonical=True)


def _build_signed_cr_post_tuple(
    signing_key,
    *,
    schema: str,
    fields: list[tuple[str, str]],
    summary: str,
    node_url: str,
    content: Optional[str] = None,
    created_at_us: Optional[int] = None,
):
    """Compose the canonical-dag-cbor ``(EmbedAsBytes-cr, EmbedAsBytes-post)``
    tuple every fauna-native social payload shares — today the inbox email
    send (``email/v1``). A sign-over-CID ``PostBody::Structured`` Post, wrapped
    by a sign-over-CID ``ContactRequest`` that carries the post's CID as its
    ``post_id``. The byte-for-byte mirror of ``fauna_client_core``'s Rust
    composer (``email::build_signed_email``).

    Returns ``(payload_bytes, post_id_hex)`` — the canonical-dag-cbor 2-array
    for the kind's ``payload`` field, and the post CID's 32-byte digest (what
    the nest echoes back as ``post_id``). ``schema`` selects the post-body
    schema; ``summary`` is the ContactRequest summary string.
    """
    pubkey = bytes(signing_key.verify_key)
    if created_at_us is None:
        created_at_us = int(time.time() * 1_000_000)

    # The post travels as canonical dag-cbor (the envelope signs its dag-cbor
    # CID, and the receiver re-hashes the dag-cbor bytes to verify the CID).
    post_bytes = encode_structured_post_dagcbor(
        pubkey, created_at_us, schema, fields, content
    )
    post_cid = cid_of_dag_cbor(post_bytes)
    post_env = post_cid + signing_key.sign(post_cid).signature

    # `post_id` (the ContactRequest's post_id field AND the value the nest
    # echoes) is `compute_post_id`, which after the Layer-6 at-rest flip is the
    # CID over the *canonical dag-cbor* of the Post — byte-identical to the CID
    # the envelope signs (`post_cid`).
    cr_bytes = encode_contact_request_dagcbor(
        pubkey, post_cid, node_url.encode("utf-8"), summary, created_at_us
    )
    cr_cid = cid_of_dag_cbor(cr_bytes)
    cr_env = cr_cid + signing_key.sign(cr_cid).signature

    # The `(EmbedAsBytes-cr, EmbedAsBytes-post)` tuple is a canonical-dag-cbor
    # 2-element array; each member is the `{bytes, envelope}` byte-string map.
    payload = cbor2.dumps(
        [
            {"bytes": cr_bytes, "envelope": cr_env},
            {"bytes": post_bytes, "envelope": post_env},
        ],
        canonical=True,
    )
    # The echoed post_id is the post CID's digest (bytes 4..36).
    return payload, post_cid[4:].hex()


def build_email_inbox_payload(
    signing_key,
    recipient_actor_hex: str,
    subject: str,
    body: str,
    *,
    node_url: str,
    created_at_us: Optional[int] = None,
):
    """Build the signed ``(ContactRequest, Post)`` email tuple for
    ``fauna.inbox.send`` — the Python mirror of
    ``fauna_client_core::email::build_signed_email``.

    The Post is a ``PostBody::Structured`` with the ``email/v1`` schema and the
    ``subject`` / ``to`` (hex recipient actor id) / ``priority`` (``"normal"``)
    fields in that order, carrying ``body`` as its ``content``; the
    ContactRequest's ``summary`` is the subject truncated to 80 chars (matching
    the Rust ``truncate``). Returns ``(payload_bytes, post_id_hex)``.
    """
    fields = [
        ("subject", subject),
        ("to", recipient_actor_hex),
        ("priority", "normal"),
    ]
    return _build_signed_cr_post_tuple(
        signing_key,
        schema="email/v1",
        fields=fields,
        summary=subject[:80],
        node_url=node_url,
        content=body,
        created_at_us=created_at_us,
    )
