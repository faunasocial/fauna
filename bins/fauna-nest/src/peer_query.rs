//! Peer query client: queries remote nests for matching posts.
//!
//! Used by following-scoped feeds to pull matching posts from the nests
//! where the user's followees are hosted.
//!
//! [`query_peer_channel_first`] queries a peer over the **federation WS-RPC
//! channel** (`fauna.federation.feed.query`, with the channel's mutual nest-key
//! auth + per-nest throttle — Spec Y2 slice 4 §6) — the sole Fauna↔Fauna carrier
//! since slice 5 retired the HTTP interim. It decodes the **shared**
//! [`crate::feed_routes::RemoteQueryResponse`] the serving handler emits and
//! converts it via [`parse_peer_candidate`] — one struct family for producer and
//! consumer, no duplicate.

use std::sync::Arc;

use fauna_core::data::Timestamp;
use fauna_core::feed::{
    CandidateMetadata, ContentAddress, ScoredCandidate, SourceTag, UnifiedIdentity,
};
use fauna_core::identity::ActorId;
use fauna_core::scoring::{FilterCombination, FilterRule};

use crate::feed_routes::{PostReferenceResponse, RemoteQueryCandidate, RemoteQueryRequest};
use crate::routes::AppState;

/// Build the shared `RemoteQueryRequest` both carriers send (hex-encoding the
/// author scope, mapping the combination enum to its `"all"`/`"any"` wire form).
fn build_remote_query_request(
    rules: &[FilterRule],
    combination: FilterCombination,
    authors: &[ActorId],
    limit: i64,
    cursor: Option<i64>,
) -> RemoteQueryRequest {
    let combination_str = match combination {
        FilterCombination::All => "all",
        FilterCombination::Any => "any",
    };
    let author_hexes: Vec<String> = authors.iter().map(|a| hex::encode(a.0)).collect();
    RemoteQueryRequest {
        rules: rules.to_vec(),
        combination: combination_str.to_string(),
        authors: if author_hexes.is_empty() {
            None
        } else {
            Some(author_hexes)
        },
        limit: Some(limit),
        cursor,
    }
}

/// Convert a served `RemoteQueryResponse` into the local `ScoredCandidate` feed
/// shape (shared by both the HTTP and channel carriers).
fn remote_response_to_candidates(
    resp: crate::feed_routes::RemoteQueryResponse,
    nest_url: &str,
) -> Vec<ScoredCandidate> {
    resp.candidates
        .into_iter()
        .filter_map(|c| parse_peer_candidate(c, nest_url))
        .collect()
}

/// Query a single remote nest over the **federation WS-RPC channel** (Spec Y2
/// slice 4 §6) — the sole Fauna↔Fauna carrier since slice 5. The channel carries
/// the query under the connection's mutual nest-key auth + per-nest throttle —
/// the hardening dividend over the unauthenticated HTTP twin it replaced
/// (`federation.md` §4.E).
///
/// A channel failure (peer offers no channel / dial hard-fails / unreachable
/// discovery surface) surfaces as a [`PeerQueryError`]; the caller treats peer
/// feed discovery as best-effort and drops that peer's contribution.
pub async fn query_peer_channel_first(
    state: &Arc<AppState>,
    nest_url: &str,
    rules: &[FilterRule],
    combination: FilterCombination,
    authors: &[ActorId],
    limit: i64,
    cursor: Option<i64>,
) -> Result<Vec<ScoredCandidate>, PeerQueryError> {
    let req = build_remote_query_request(rules, combination, authors, limit, cursor);
    let resp =
        crate::federation_pool::originate_feed_query(&state.federation_pool, state, nest_url, &req)
            .await
            .map_err(|e| PeerQueryError::Network(format!("feed.query channel: {e}")))?;
    Ok(remote_response_to_candidates(resp, nest_url))
}

/// Encode a post CID for the peer-query wire as lowercase hex of the full
/// 36-byte CID (version + codec + multihash + 32-byte digest). The codec
/// byte at position 1 (`dag-cbor 0x71` / `raw 0x55`) rides on the wire so
/// the querying nest decodes the codec faithfully rather than assuming
/// dag-cbor — the producing nest is the authority on its own posts' codec.
pub(crate) fn cid_to_wire_hex(cid: &fauna_cbor::Cid) -> String {
    hex::encode(cid.as_bytes())
}

/// Decode a post CID from the peer-query wire (lowercase hex of the full
/// 36-byte CID). Returns `None` on malformed hex, wrong length, or an
/// invalid CID prefix (validated via [`fauna_cbor::Cid::from_bytes`]).
pub(crate) fn cid_from_wire_hex(s: &str) -> Option<fauna_cbor::Cid> {
    let bytes = hex::decode(s).ok()?;
    let arr: [u8; 36] = bytes.try_into().ok()?;
    fauna_cbor::Cid::from_bytes(arr).ok()
}

/// Parse a single remote candidate off the peer-query wire into a
/// `ScoredCandidate`. Drops the candidate (`None`) on any malformed field.
fn parse_peer_candidate(pc: RemoteQueryCandidate, nest_url: &str) -> Option<ScoredCandidate> {
    // Peers exchange the full 36-byte CID (codec byte included) — the
    // querying nest decodes it faithfully instead of assuming dag-cbor.
    let post_cid = cid_from_wire_hex(&pc.post_id)?;
    let author: [u8; 32] = fauna_core::hex32::decode(&pc.author).ok()?;

    // The peer's wire `source` (post 633, `feed_routes.rs`'s
    // `remote_query_feed_core`) is its own indexed protocol token, not a URL
    // — `fauna_core::source::normalize` bounds it (one lowercase word, ≤32
    // bytes) exactly as any other peer-chosen string headed for a local
    // index column; malformed input (empty, oversized, an old un-upgraded
    // peer still echoing its fetch URL, which can never pass this charset)
    // drops the whole candidate, matching this function's existing contract
    // for every other malformed field. A peer legitimately claiming `"fauna"`
    // is accepted as-is once normalized: `source` is a PROTOCOL classification
    // (`docs/goal/ui/feed.md:220`/`:280` — `classify_sources()`), and a real
    // Fauna-protocol post fetched from a peer nest genuinely is one — the
    // separate hazard of trusting an unverified native/bridge claim for
    // INTERACTION ROUTING is closed independently by `origin_nest_url`
    // (`interact_routes.rs`), which refuses every discovery-indexed post
    // regardless of its token.
    let source_token = fauna_core::source::normalize(&pc.source)?;

    let references = pc
        .references
        .into_iter()
        .filter_map(parse_peer_reference)
        .collect();

    Some(ScoredCandidate {
        post_id: ContentAddress::Fauna { post_id: post_cid },
        author: UnifiedIdentity::Fauna {
            actor_id: ActorId(author),
        },
        source: SourceTag::Fauna,
        source_token,
        created_at: Timestamp(pc.created_at as u64),
        score: pc.score,
        scorer_version: pc.scorer_version,
        fetch_url: format!(
            "{}/api/v1/posts/{}",
            nest_url.trim_end_matches('/'),
            // The cross-nest post-body GET addresses the post by its
            // 32-byte BLAKE3 digest hex.
            hex::encode(post_cid.digest())
        ),
        metadata: CandidateMetadata {
            tags: pc.metadata.tags,
            has_media: pc.metadata.has_media,
            is_reply: pc.metadata.is_reply,
            body_hint: None,
        },
        references,
    })
}

/// Parse a single post-to-post reference off the peer-query wire. Drops
/// the reference (`None`) on a malformed field or an unknown ref type.
fn parse_peer_reference(r: PostReferenceResponse) -> Option<fauna_core::feed::PostReference> {
    // References already carry the full 36-byte CID on the wire
    // (`db::posts::get_post_references` returns it); decode through the
    // same codec-validating helper as the candidate post_id.
    let ref_cid = cid_from_wire_hex(&r.post_id)?;
    let author_bytes = hex::decode(&r.author).ok()?;
    let ref_type = match r.ref_type.as_str() {
        "repost" => fauna_core::feed::PostReferenceType::Repost,
        "quote" => fauna_core::feed::PostReferenceType::Quote,
        "reply" => fauna_core::feed::PostReferenceType::Reply,
        _ => return None, // skip unknown ref types
    };
    Some(fauna_core::feed::PostReference {
        post_id: ref_cid.as_bytes().to_vec(),
        author: author_bytes,
        nest_url: r.nest_url,
        ref_type,
    })
}

#[derive(Debug)]
pub enum PeerQueryError {
    Network(String),
    RemoteError { status: u16, body: String },
    ParseError(String),
}

impl std::fmt::Display for PeerQueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Network(e) => write!(f, "network error: {e}"),
            Self::RemoteError { status, body } => write!(f, "remote error {status}: {body}"),
            Self::ParseError(e) => write!(f, "parse error: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feed_routes::RemoteQueryMetadata;

    fn empty_metadata() -> RemoteQueryMetadata {
        RemoteQueryMetadata {
            tags: vec![],
            has_media: false,
            is_reply: false,
        }
    }

    #[test]
    fn peer_query_wire_carries_the_full_cid_codec() {
        // A contributor nest content-addresses a post with the RAW codec
        // (e.g. an imported opaque payload). The peer-query wire must carry
        // the full 36-byte CID so the querying nest reconstructs RAW —
        // not an assumed dag-cbor CID. The home nest is the authority on
        // its own posts' codec; the consumer must not guess.
        let raw_cid = fauna_cbor::Cid::of_raw(b"opaque imported payload");
        assert_eq!(raw_cid.codec(), fauna_cbor::Cid::RAW);

        let pc = RemoteQueryCandidate {
            post_id: cid_to_wire_hex(&raw_cid),
            author: hex::encode([7u8; 32]),
            source: "fauna".to_string(),
            created_at: 123,
            score: None,
            scorer_version: None,
            metadata: empty_metadata(),
            references: vec![],
        };

        let scored = parse_peer_candidate(pc, "https://peer.example")
            .expect("a candidate with a full-CID wire post_id must parse");

        match scored.post_id {
            ContentAddress::Fauna { post_id } => {
                assert_eq!(
                    post_id, raw_cid,
                    "the wire CID must round-trip byte-for-byte"
                );
                assert_eq!(
                    post_id.codec(),
                    fauna_cbor::Cid::RAW,
                    "codec must be carried on the wire, not assumed dag-cbor"
                );
            }
            _ => panic!("expected ContentAddress::Fauna"),
        }

        // fetch_url still addresses the post by its 32-byte digest hex
        // (the cross-nest post-body GET takes a 32-byte digest).
        assert_eq!(
            scored.fetch_url,
            format!(
                "https://peer.example/api/v1/posts/{}",
                hex::encode(raw_cid.digest())
            ),
        );
    }

    #[test]
    fn source_token_is_threaded_from_the_peer_wire_not_hardcoded() {
        // Row 740: before this, every parsed candidate got a hard-coded
        // "fauna" written wherever `source_token` is threaded to, discarding
        // whatever the peer actually advertised (post 633's own wire fix).
        let pc = RemoteQueryCandidate {
            post_id: cid_to_wire_hex(&fauna_cbor::Cid::of_dag_cbor(b"a post")),
            author: hex::encode([3u8; 32]),
            source: "facebook".to_string(),
            created_at: 1,
            score: None,
            scorer_version: None,
            metadata: empty_metadata(),
            references: vec![],
        };
        let scored = parse_peer_candidate(pc, "https://peer.example").unwrap();
        assert_eq!(scored.source_token, "facebook");
    }

    #[test]
    fn a_peer_may_claim_fauna_once_normalized() {
        // A genuinely Fauna-protocol post fetched from a peer nest is a real
        // "fauna" post — feed.md's `PostSummary.source` is a PROTOCOL
        // classification, not a locality one. The interact-routing hazard of
        // trusting this claim is closed separately, via `origin_nest_url`
        // (`interact_routes.rs`), not by refusing the token here.
        let pc = RemoteQueryCandidate {
            post_id: cid_to_wire_hex(&fauna_cbor::Cid::of_dag_cbor(b"a post")),
            author: hex::encode([4u8; 32]),
            source: " Fauna ".to_string(),
            created_at: 1,
            score: None,
            scorer_version: None,
            metadata: empty_metadata(),
            references: vec![],
        };
        let scored = parse_peer_candidate(pc, "https://peer.example").unwrap();
        assert_eq!(scored.source_token, "fauna");
    }

    #[test]
    fn a_malformed_or_url_shaped_source_token_drops_the_candidate() {
        // An un-upgraded peer still echoing a fetch URL as `source` (the
        // pre-740 bug on ITS end) can never pass `normalize` — `/`, `:` and
        // `.` are outside the token charset — so the candidate is dropped
        // rather than indexed under garbage, matching this function's
        // existing contract for every other malformed field.
        for bad_source in ["https://peer.example/api/v1/posts/abc", "", "a b"] {
            let pc = RemoteQueryCandidate {
                post_id: cid_to_wire_hex(&fauna_cbor::Cid::of_dag_cbor(b"a post")),
                author: hex::encode([5u8; 32]),
                source: bad_source.to_string(),
                created_at: 1,
                score: None,
                scorer_version: None,
                metadata: empty_metadata(),
                references: vec![],
            };
            assert!(
                parse_peer_candidate(pc, "https://peer.example").is_none(),
                "{bad_source:?} must not parse into a candidate"
            );
        }
    }

    #[test]
    fn cid_wire_hex_round_trips_both_codecs() {
        for cid in [
            fauna_cbor::Cid::of_dag_cbor(b"dag-cbor payload"),
            fauna_cbor::Cid::of_raw(b"raw payload"),
        ] {
            let wire = cid_to_wire_hex(&cid);
            let decoded = cid_from_wire_hex(&wire).expect("full-CID hex must round-trip");
            assert_eq!(decoded, cid);
            assert_eq!(decoded.codec(), cid.codec());
        }
        // A bare 32-byte digest hex (the legacy wire) is rejected — it is
        // not a full CID.
        assert!(cid_from_wire_hex(&hex::encode([9u8; 32])).is_none());
    }
}
