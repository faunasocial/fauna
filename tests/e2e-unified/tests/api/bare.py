"""Signed canonical dag-cbor Post encoding for API tests.

Single source of truth for encoding `fauna_core::data::Post` into the wire
format accepted by the `fauna.posts.create` WS-RPC kind (the post bytes the
`tests.api.ws_api.create_post` helper sends as its `body`). When the `Post`
struct gains or loses a field, update this module — the API test files that
publish posts all import `sign_and_encode_post` from here.

The wire shape is the embed-as-bytes envelope every signed Fauna kind rides
(`fauna_core::encoding::EmbedAsBytes` — `serialization.md` § Embed-as-bytes):
canonical dag-cbor `{"bytes": post_bytes, "envelope": cid || sig}` where
`post_bytes` is the canonical dag-cbor of the Post, `cid` is its 36-byte
dag-cbor/blake3 CID, and `sig` is the author's Ed25519 signature over the CID
(sign-over-CID, untagged — `SignedEnvelope::sign`). `cbor2.dumps(value,
canonical=True)` is byte-identical to `serde_ipld_dagcbor::to_vec` for these
shapes (verified against a Rust reference — see `tests/common/envelope.py`,
whose `cid_of_dag_cbor` + `wrap_embed_as_bytes` this module composes).

**Signing is the only path.** `SealedStorage::classify_encrypted_post`
(`bins/fauna-nest/src/storage/sealed.rs`) decodes the embed-as-bytes wrapper
and runs `verify_envelope` unconditionally on every nest — the S8 storage-mode
demolition deleted the plaintext-mode signature skip, so the old unsigned
`encode_post` builder could no longer seed a post anywhere and was removed
with it (its raw bytes failed at the very first `canonical_decode` with
`ingest rejected: post_decode`). There is deliberately no unsigned escape
hatch left for a new test to reach for; the signer must be the post's author
(`verify_envelope` checks against `post.author`).

Notes on the Post shape:

* `author` (`ActorId([u8; 32])`) rides as a 32-byte CBOR **byte string**, like
  every fixed-width id — sent as raw `bytes`.
* `created_at` (`Timestamp(u64)`) is a uint.
* `body` is the externally-tagged single-key map (`{"Text": {...}}`).
* The sign-over-CID `Post` carries **no signature field** — the signature
  lives in the envelope half of the embed-as-bytes wrapper.
"""

from __future__ import annotations

import cbor2

from common.envelope import (
    CID_DAG_CBOR_PREFIX,
    CID_RAW_PREFIX,
    cid_link,
    cid_of_dag_cbor,
    wrap_embed_as_bytes,
)

# `PostId = fauna_cbor::Cid` (`libs/fauna-cbor/src/cid.rs`) is a 36-byte CID:
# v1 (0x01) + dag-cbor codec (0x71) + blake3-256 multihash (0x1e) + length
# (0x20 = 32) + the 32-byte digest, riding as a tag-42 link (`cid_link`).
# `ContentHash = fauna_cbor::Cid` with the **raw** codec (0x55) instead
# (`libs/fauna-core/src/data.rs` — "payloads whose canonical form IS the byte
# sequence itself"): same layout, same link; only the codec byte differs, and
# getting it wrong is a decode error at ingest rather than a silent mismatch.


def media_item(blob_hash_hex: str, mime: str, size_bytes: int) -> dict:
    """One `fauna_core::data::MediaItem` — a post's attachment by blob hash.

    The shape every app's composer produces for a photo post
    (`fauna_client_core::post::build_post_with_media`): the blob is uploaded
    first through `POST /api/v1/blob`, and the post then *names* its hash,
    which is also what pins the blob against the nest's GC sweep
    (`docs/goal/ui/media.md` § Public-post-attached media).

    `dimensions` / `thumbnail` / `remote_url` are `None` exactly as the
    composer leaves them for a plain image attachment.
    """
    digest = bytes.fromhex(blob_hash_hex)
    assert len(digest) == 32, f"blob hash must be a 32-byte digest, got {len(digest)}"
    return {
        "blob_hash": cid_link(CID_RAW_PREFIX + digest),
        "media_type": mime,
        "size_bytes": size_bytes,
        "dimensions": None,
        "thumbnail": None,
        "remote_url": None,
    }


def post_reference(kind: str, target_post_id_hex: str) -> dict:
    """A `Post` `Reference::{Reply,Repost,Quote}` to a target post.

    `kind` ∈ {"Reply", "Repost", "Quote"}. The target is identified by its
    32-byte content digest — the hex ``post_id`` ``ws_api.create_post`` returns —
    which we wrap in the 36-byte dag-cbor CID shape `PostId` deserializes; the
    nest strips it back to the digest to key `content_meta` (`cid_to_digest` in
    `db/posts.rs`). Each is an externally-tagged single-key map carrying only
    `post_id` (React/Upvote/Downvote carry more and aren't interaction counts).
    """
    digest = bytes.fromhex(target_post_id_hex)
    assert len(digest) == 32, f"target post id must be a 32-byte digest, got {len(digest)}"
    return {kind: {"post_id": cid_link(CID_DAG_CBOR_PREFIX + digest)}}


def sign_and_encode_tombstone(
    author_signing_key,
    post_id_hex: str,
    created_at_us: int,
) -> bytes:
    """Signed embed-as-bytes wire bytes of a `fauna_core::data::Tombstone`.

    The body `fauna.posts.delete` takes (`PostDeleteRequest.body` —
    `libs/fauna-protocol/src/posts.rs`). Same sign-over-CID envelope as
    `sign_and_encode_post`: the nest's `decode_tombstone`
    (`libs/fauna-core/src/encoding.rs`) verifies the signature against
    `tombstone.author`, and the handler additionally requires the connection
    actor to BE that author — so the signer must own the post.

    Args:
        author_signing_key: The author's PyNaCl ``nacl.signing.SigningKey``.
        post_id_hex: Hex 32-byte post digest (what ``ws_api.create_post``
            returns); wrapped in the 36-byte CID shape `PostId` deserializes,
            exactly as `post_reference` does.
        created_at_us: Microseconds since epoch (u64).
    """
    digest = bytes.fromhex(post_id_hex)
    assert len(digest) == 32, f"post id must be a 32-byte digest, got {len(digest)}"

    tombstone = {
        "author": bytes(author_signing_key.verify_key),
        "post_id": cid_link(CID_DAG_CBOR_PREFIX + digest),
        "created_at": created_at_us,
    }
    tombstone_bytes = cbor2.dumps(tombstone, canonical=True)

    cid = cid_of_dag_cbor(tombstone_bytes)
    sig = author_signing_key.sign(cid).signature
    assert len(cid) == 36 and len(sig) == 64
    return wrap_embed_as_bytes(cid + sig, tombstone_bytes)


def sign_and_encode_post(
    author_signing_key,
    created_at_us: int,
    body_text: str,
    *,
    tags: list[tuple[str, int, int]] | None = None,
    references: list[dict] | None = None,
    media_items: list[dict] | None = None,
) -> bytes:
    """Signed embed-as-bytes wire bytes of a `Post` with a `PostBody::Text` body.

    Args:
        author_signing_key: The author's PyNaCl ``nacl.signing.SigningKey``
            (the ``signing_key`` entry of a ``create_actor_and_register`` /
            ``register_handled_actor`` actor dict). The post's ``author`` is
            derived from it — ``verify_envelope`` rejects any other signer.
        created_at_us: Microseconds since epoch (u64).
        body_text: Post content.
        tags: Optional list of (tag_name, byte_start, byte_end) `FacetFeature::Tag` facets.
        references: Optional list of `Reference` maps (build with `post_reference`)
            — a reply/repost/quote post carries the target it references, which
            bumps the target's interaction counter at ingest (`feed.md` §
            Interaction bar).
        media_items: Optional list of `MediaItem` maps (build with
            `media_item`). Present → the body is `PostBody::TextWithMedia`
            instead of `PostBody::Text`, which is precisely what every app's
            composer emits for a photo post; the blobs must already be
            uploaded, since the post is what names (and so pins) them.

    Returns the canonical dag-cbor ``EmbedAsBytes`` wrapper ready for
    ``fauna.posts.create``'s ``body`` — see the module docstring for the shape.
    """
    facets: list[dict] = []
    for tag_name, byte_start, byte_end in tags or []:
        facets.append(
            {
                "byte_start": byte_start,
                "byte_end": byte_end,
                "feature": {"Tag": {"name": tag_name}},
            }
        )

    if media_items:
        body = {
            "TextWithMedia": {
                "content": body_text,
                "facets": facets,
                "items": media_items,
            }
        }
    else:
        body = {"Text": {"content": body_text, "facets": facets}}

    post = {
        "author": bytes(author_signing_key.verify_key),
        "created_at": created_at_us,
        "body": body,
        "references": references or [],
        "expires_at": None,
        "gated": None,
        "content_warning": None,
    }
    post_bytes = cbor2.dumps(post, canonical=True)

    # Sign-over-CID, untagged (`SignedEnvelope::sign`): Ed25519 over the
    # 36-byte CID; the 100-byte envelope is cid || sig.
    cid = cid_of_dag_cbor(post_bytes)
    sig = author_signing_key.sign(cid).signature
    assert len(cid) == 36 and len(sig) == 64
    return wrap_embed_as_bytes(cid + sig, post_bytes)
