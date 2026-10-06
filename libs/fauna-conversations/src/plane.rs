//! Where a conversation message sits on the **account data plane** — the one
//! derivation that lets an app report the T1 body-rendered observation.
//!
//! Owner: `docs/goal/architecture/account-data-plane.md` § The replica boundary
//! → T1 and its producer decomposition. The intake
//! (`fauna_sync_engine::observation_intake`) owns the entire rule downstream of
//! the report — classification, coordinate resolution, dedup, the class-2 put —
//! but it is keyed by the record's plane identity, and *that* is the one thing
//! only this layer can compute.
//!
//! # Why the derivation lives here
//!
//! A conversation message reaches an app over `fauna.conversations.channel.
//! fetch`, which carries `(seq, envelope)` — not the record CID the plane keys
//! on. The nest mints that CID when it appends the record
//! (`bins/fauna-nest/src/segments/conv.rs`) as the **content hash of the record
//! envelope**, whose sole input is the sealed payload the client holds at
//! ingest (`message-segment-store.md` § *Record identity per kind*, ruled
//! 2026-08-17). So the identity is *derivable*, never fetchable — through the
//! very function the nest files under
//! ([`fauna_mls::segments::derive_record_cid`], which routes to the same
//! `encode_record` mint). This module is the client-side seam that stamps the
//! result onto the message, so no app re-derives it and no app hand-parses a
//! scope string.
//!
//! **`seq` is not part of the identity** — it left the pre-image with the
//! retired `derive_record_id(channel_id, seq, body)`. It remains the
//! cross-member coordinate everything else about a conversation keys on; it is
//! simply not what names the record on the data plane.
//!
//! **The codec half is not carried.** `record_digest` is the CID's 32-byte
//! BLAKE3 digest; the codec is `dag-cbor`, because every record on this plane
//! is dag-cbor-coded, so the intake rebuilds the full CID with
//! `Cid::from_digest_dag_cbor`. That is the same assumption the replica's own
//! content-scope walk makes rebuilding a CID from a feed row's `path_hash`.
//!
//! **An old client derives the old digest and is dropped, deliberately.** A
//! client from before the cutover reports observations against pre-cutover
//! digests; the intake's coordinate resolution finds nothing and fails soft to
//! a dropped observation (the seen-set is grow-only). Accepted within the major
//! — no wire shape changed, and the user-visible effect is at most one
//! unrecorded "rendered" mark until the app updates.

use crate::message::PlaneRef;

/// The content-scope family prefix, spelled as a literal rather than taken from
/// `fauna_protocol::scope` — this crate is deliberately **wire-type-free** and
/// takes no protocol dependency (the boundary recorded on `ConvRpcError` in
/// `Cargo.toml`).
///
/// The duplication is pinned where the two crates actually meet: an app's
/// reporter parses this string back through `fauna_protocol::scope::ContentScope`
/// (tui: `crate::observation`'s `a_painted_bubbles_plane_ref_parses_into_an_
/// observation`), so a divergence reds there instead of silently producing scope
/// strings the intake resolves nothing for.
const CONTENT_FAMILY: &str = "content";

/// The member-scope kind an MLS channel's conversation records live under.
const CONV_KIND: &str = "conv";

/// The plane identity of one conversation record.
///
/// `channel_id_hex` is the channel's lowercase hex (the same form
/// [`crate::keying::ThreadKey::Channel`] keys on) and `envelope` the sealed
/// record bytes exactly as the nest stored them — the `envelope` field of the
/// fetch entry, or the body a `send` posted. (No `seq`: it left the identity
/// pre-image with the record-identity cutover — see the module docs.)
///
/// Returns `None` for a channel id that is not 32 hex bytes, or for a payload
/// whose envelope will not encode: neither can name a record, and a message
/// with no plane ref simply reports no observation (the seen-set is grow-only,
/// so nothing is lost that a later, well-formed render cannot add).
pub fn plane_ref(channel_id_hex: &str, envelope: &[u8]) -> Option<PlaneRef> {
    let channel = decode_hex32(channel_id_hex)?;
    let cid = fauna_mls::segments::derive_record_cid(envelope).ok()?;
    Some(PlaneRef {
        scope: format!("{CONTENT_FAMILY}:{CONV_KIND}:{}", hex::encode(channel)),
        record_digest: hex::encode(cid.digest()),
    })
}

fn decode_hex32(hex_str: &str) -> Option<[u8; 32]> {
    let bytes = hex::decode(hex_str).ok()?;
    bytes.as_slice().try_into().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHANNEL_HEX: &str = "1111111111111111111111111111111111111111111111111111111111111111";

    /// The client's derivation is the nest's filing identity — the property the
    /// whole seam rests on, asserted against the shared mint the nest itself
    /// files under rather than against a re-implementation here. The digest
    /// half is what travels; the intake re-adds the dag-cbor codec.
    #[test]
    fn the_digest_is_the_nests_own_filing_cid() {
        let r = plane_ref(CHANNEL_HEX, b"sealed-envelope").expect("plane ref");
        let (minted, _bytes) = fauna_mls::segments::encode_record(
            &fauna_mls::segments::ConvRecordEnvelope::new(b"sealed-envelope".to_vec()),
        )
        .expect("mint");
        assert_eq!(r.record_digest, hex::encode(minted.digest()));
    }

    /// The envelope discriminates: two different messages never collide, so an
    /// observation can never be credited to the wrong one. Identical bytes on
    /// one channel ARE one record post-cutover — the nest's scoped dedup
    /// collapses the replay rather than storing it twice.
    #[test]
    fn a_different_body_is_a_different_record_and_the_same_body_is_the_same_one() {
        let a = plane_ref(CHANNEL_HEX, b"body").expect("a");
        let b = plane_ref(CHANNEL_HEX, b"other").expect("b");
        let a_again = plane_ref(CHANNEL_HEX, b"body").expect("a again");
        assert_ne!(a.record_digest, b.record_digest, "the body discriminates");
        assert_eq!(a.record_digest, a_again.record_digest, "content-addressed");
        assert_eq!(a.scope, b.scope, "same channel, same scope");
    }

    /// A malformed channel id yields no ref rather than a scope string the
    /// intake would refuse — the message just reports nothing.
    #[test]
    fn a_channel_id_that_is_not_32_hex_bytes_has_no_plane_ref() {
        assert!(plane_ref("", b"body").is_none());
        assert!(plane_ref("zz", b"body").is_none());
        assert!(plane_ref("11223344", b"body").is_none(), "too short");
    }

    /// The scope string this crate writes, spelled out — the local half of the
    /// cross-crate pin the module docs name (the app-side reporter parses this
    /// same string back through `fauna_protocol::scope::ContentScope`).
    #[test]
    fn the_scope_string_is_the_canonical_content_scope_spelling() {
        let r = plane_ref(CHANNEL_HEX, b"body").expect("plane ref");
        assert_eq!(r.scope, format!("content:conv:{CHANNEL_HEX}"));
    }
}
