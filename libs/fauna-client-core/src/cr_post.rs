//! Decode and verify a `(ContactRequest, Post)` inbox envelope pair.
//!
//! Every inbox-delivered item — an email — travels as a canonical dag-cbor
//! tuple of two independently-signed embed-as-bytes envelopes: a
//! [`ContactRequest`] (routing/summary: sender, post_id, sender_node) and the
//! [`Post`] it announces. [`decode_verified_cr_post_pair`] is the one place
//! that splits, decodes, and verifies both — email.rs projects its typed
//! fields from the returned pair.

use fauna_core::data::{ContactRequest, Post};
use fauna_core::encoding::{
    EmbedAsBytes, canonical_decode, compute_post_id, decode_signed_bytes, verify_envelope,
};

use crate::ClientError;

/// Decode a canonical dag-cbor `(EmbedAsBytes-cr, EmbedAsBytes-post)` payload
/// and verify it: each envelope's own signature, that the `ContactRequest`'s
/// sender matches the `Post`'s author, and that the `ContactRequest`'s
/// `post_id` matches the `Post`'s own computed id.
///
/// Returns `(post, cr, valid, post_id_bytes)` — `valid` is the AND of all
/// four checks (a decode failure is a hard `Err`, never folded into `valid`;
/// only signature/cross-reference mismatches degrade to `valid = false`, so
/// callers can still surface the sender/timestamp of an inbox item whose
/// claims don't check out). Callers project the typed fields they need
/// (email subject/body) from `post`/`cr` themselves — a fix to one of these
/// four validity conditions is compiler-enforced to reach every caller,
/// unlike hand-copied bodies.
pub(crate) fn decode_verified_cr_post_pair(
    payload: &[u8],
) -> Result<(Post, ContactRequest, bool, Vec<u8>), ClientError> {
    let (cr_wire, post_wire): (EmbedAsBytes, EmbedAsBytes) =
        canonical_decode(payload).map_err(|e| ClientError(format!("decode: {e}")))?;

    let (post_bytes, post_env) = post_wire
        .into_signed()
        .map_err(|e| ClientError(format!("split post envelope: {e}")))?;
    let post: Post =
        decode_signed_bytes(&post_bytes).map_err(|e| ClientError(format!("decode post: {e}")))?;
    let (cr_bytes, cr_env) = cr_wire
        .into_signed()
        .map_err(|e| ClientError(format!("split CR envelope: {e}")))?;
    let cr: ContactRequest =
        decode_signed_bytes(&cr_bytes).map_err(|e| ClientError(format!("decode CR: {e}")))?;

    let post_valid = verify_envelope(&post, &post_bytes, &post_env).is_ok();
    let cr_valid = verify_envelope(&cr, &cr_bytes, &cr_env).is_ok();
    let sender_match = cr.sender == post.author;
    let post_id_match = compute_post_id(&post)
        .map(|id| id == cr.post_id)
        .unwrap_or(false);
    let valid = post_valid && cr_valid && sender_match && post_id_match;

    let post_id = compute_post_id(&post)
        .map(|id| id.as_bytes().to_vec())
        .unwrap_or_default();

    Ok((post, cr, valid, post_id))
}
