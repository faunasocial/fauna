//! The production [`BootstrapSource`](fauna_account_store::store::BootstrapSource)
//! over a native nest — the shared source
//! ([`fauna_account_plane::bootstrap_source`], which owns the listing and the
//! in-transit check) over this crate's byte plane ([`SyncClient`] as its
//! [`SegmentBytes`]). Re-exported at its old path, so
//! `bootstrap_source::NestBootstrapSource::new(rpc, &sync_client, binding)`
//! reads unchanged.

use anyhow::Result;
use fauna_account_store::segments::{SegmentHalf, SegmentSink};

pub use fauna_account_plane::bootstrap_source::{ScopeBinding, SegmentBytes};

use crate::nest_client::{SegmentBody, SyncClient};

/// The shared source over the native byte plane.
pub type NestBootstrapSource<'a, R> =
    fauna_account_plane::bootstrap_source::NestBootstrapSource<'a, R, SyncClient>;

/// The native byte plane ([`SyncClient::segment_body`] +
/// [`SyncClient::segment_meta_body`] — the same pair the custodian pull ships)
/// as the shared source's seam.
impl SegmentBytes for SyncClient {
    async fn fetch_half(
        &self,
        kind: &str,
        actor_hex: &str,
        segment_id: u32,
        half: SegmentHalf,
        max_bytes: u64,
        into: &mut impl SegmentSink,
    ) -> Result<()> {
        let body = match half {
            SegmentHalf::Dat => {
                self.segment_body(kind, actor_hex, segment_id, 0, max_bytes)
                    .await
            }
            SegmentHalf::Meta => {
                self.segment_meta_body(kind, actor_hex, segment_id, max_bytes)
                    .await
            }
        };
        stream_half(body, half, into).await.map(drop)
    }
}

/// Stream one half's body into `into` a chunk at a time, hashing as it goes —
/// the transfer holds one transport chunk, never the half. Returns the half's
/// BLAKE3 (hex) for the in-transit check. The owner's backup pass stages its
/// pairs through it too ([`crate::segment_backup::SourceBinding`]).
pub(crate) async fn stream_half(
    body: Result<SegmentBody<'_>>,
    half: SegmentHalf,
    into: &mut impl SegmentSink,
) -> Result<String> {
    let mut body = body?;
    let mut hasher = blake3::Hasher::new();
    while let Some(chunk) = body.next_chunk().await? {
        hasher.update(&chunk);
        into.write(half, &chunk).await?;
    }
    Ok(hex::encode(hasher.finalize().as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use fauna_account_store::store::{BootstrapSource, SegmentOffer};
    use fauna_protocol::RpcRequester;
    use fauna_protocol::segments::{SegmentRef, SegmentsListReply};

    use fauna_nest_http::StaticBearer;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// The tier_3 proof (`bins/fauna-nest/tests/conformance_account_bootstrap.rs`)
    /// runs against an *honest* nest, so the in-transit check has nothing to
    /// bite there by construction. These two drive the dishonest side: a byte
    /// plane whose bytes disagree with the control plane's own listing.
    const KIND: &str = "post";
    const ACTOR: [u8; 32] = [0xAB; 32];

    fn actor_hex() -> String {
        hex::encode(ACTOR)
    }

    /// The canonical scope string for `(KIND, ACTOR)` — via the ratified
    /// constructor, so these fixtures can't drift from the convention
    /// (`fauna_protocol::scope`).
    fn scope() -> String {
        fauna_protocol::scope::ContentScope::new(KIND, ACTOR)
            .unwrap()
            .to_string()
    }

    /// A control plane that answers `fauna.segments.list` with one segment,
    /// advertising `blake3_hex`. Not a nest stand-in for anything else: the
    /// only kind it serves is the list, which is all `list_segments` calls.
    struct ListingOnly {
        blake3_hex: String,
        /// What the listing advertises for the sidecar.
        meta_blake3_hex: String,
    }

    #[derive(Debug)]
    struct Never;
    impl std::fmt::Display for Never {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "unreachable")
        }
    }

    impl RpcRequester for ListingOnly {
        type Error = Never;

        async fn request<Req, Reply>(
            &self,
            _kind: &'static str,
            _payload: Req,
        ) -> Result<Reply, Never>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            let reply = SegmentsListReply {
                segments: vec![SegmentRef {
                    segment_id: 0,
                    blake3_hex: self.blake3_hex.clone(),
                    bucket: "2026-08".into(),
                    record_count: 1,
                    tombstone_count: 0,
                    size_bytes: 4,
                    created_at_secs: 0,
                    is_open: false,
                    meta_blake3_hex: self.meta_blake3_hex.clone(),
                    extra: Default::default(),
                }],
                next_segment_id: 1,
                extra: Default::default(),
            };
            let bytes = fauna_protocol::encode_canonical(&reply).unwrap();
            Ok(fauna_protocol::decode_strict(&bytes).unwrap())
        }
    }

    /// A sink that keeps what it was handed, per half.
    #[derive(Default)]
    struct Collected {
        dat: Vec<u8>,
        meta: Vec<u8>,
    }

    impl SegmentSink for Collected {
        async fn write(&mut self, half: SegmentHalf, chunk: &[u8]) -> Result<()> {
            match half {
                SegmentHalf::Dat => self.dat.extend_from_slice(chunk),
                SegmentHalf::Meta => self.meta.extend_from_slice(chunk),
            }
            Ok(())
        }
    }

    fn byte_plane(server_uri: &str) -> SyncClient {
        let bearer: Arc<dyn fauna_nest_http::BearerSource> =
            Arc::new(StaticBearer("test.bearer".to_string()));
        let auth = Arc::new(fauna_client::AuthClient::with_bearer_source(
            server_uri.to_string(),
            fauna_core::identity::ActorKeypair::generate(),
            bearer,
            reqwest::Client::new(),
        ));
        SyncClient::new(auth, &[0u8; 32])
    }

    fn binding() -> ScopeBinding {
        ScopeBinding {
            scope: scope(),
            kinds: vec![KIND.into()],
            actor_hex: actor_hex(),
        }
    }

    fn offer() -> SegmentOffer {
        SegmentOffer {
            kind: KIND.into(),
            segment_id: 0,
            dat_size: Some(4),
        }
    }

    async fn serving_dat(server: &MockServer, body: &'static [u8]) {
        Mock::given(method("GET"))
            .and(path(format!("/api/v1/segments/{KIND}/{}/0", actor_hex())))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(server)
            .await;
    }

    /// A `.dat` that does not hash to the digest the same nest advertised is
    /// refused **at the transport**, before adoption ever sees it — so the
    /// error names a transfer to re-try, not a malformed container.
    #[tokio::test]
    async fn a_dat_that_contradicts_the_listing_is_refused() {
        let server = MockServer::start().await;
        serving_dat(&server, b"real").await;
        let rpc = ListingOnly {
            blake3_hex: hex::encode(blake3::hash(b"different").as_bytes()),
            meta_blake3_hex: hex::encode(blake3::hash(b"sidecar").as_bytes()),
        };
        let bytes = byte_plane(&server.uri());
        let source = NestBootstrapSource::new(&rpc, &bytes, binding());

        source.list_segments(&scope()).await.unwrap();
        let err = source
            .fetch_segment(&scope(), &offer(), u64::MAX, &mut Collected::default())
            .await
            .expect_err("the served bytes contradict the listing");
        let s = format!("{err:#}");
        assert!(
            s.contains("advertised"),
            "want the transport's own complaint, got: {s}"
        );
    }

    /// The same fetch with an honest listing gets as far as the sidecar — the
    /// check passes on matching bytes rather than refusing everything.
    #[tokio::test]
    async fn a_matching_dat_passes_the_transit_check() {
        let server = MockServer::start().await;
        serving_dat(&server, b"real").await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/api/v1/segments/{KIND}/{}/0/meta",
                actor_hex()
            )))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"sidecar".to_vec()))
            .mount(&server)
            .await;
        let rpc = ListingOnly {
            blake3_hex: hex::encode(blake3::hash(b"real").as_bytes()),
            meta_blake3_hex: hex::encode(blake3::hash(b"sidecar").as_bytes()),
        };
        let bytes = byte_plane(&server.uri());
        let source = NestBootstrapSource::new(&rpc, &bytes, binding());

        source.list_segments(&scope()).await.unwrap();
        let mut got = Collected::default();
        source
            .fetch_segment(&scope(), &offer(), u64::MAX, &mut got)
            .await
            .unwrap();
        assert_eq!(got.dat, b"real");
        assert_eq!(got.meta, b"sidecar", "the pair's other half comes back too");
    }

    /// A `.meta` that contradicts the listing's sidecar hash is refused at
    /// the transport too — the pair is checked
    /// half by half, not `.dat` only.
    #[tokio::test]
    async fn a_meta_that_contradicts_the_listing_is_refused() {
        let server = MockServer::start().await;
        serving_dat(&server, b"real").await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/api/v1/segments/{KIND}/{}/0/meta",
                actor_hex()
            )))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"sidecar".to_vec()))
            .mount(&server)
            .await;
        let rpc = ListingOnly {
            blake3_hex: hex::encode(blake3::hash(b"real").as_bytes()),
            meta_blake3_hex: hex::encode(blake3::hash(b"a different sidecar").as_bytes()),
        };
        let bytes = byte_plane(&server.uri());
        let source = NestBootstrapSource::new(&rpc, &bytes, binding());

        source.list_segments(&scope()).await.unwrap();
        let err = source
            .fetch_segment(&scope(), &offer(), u64::MAX, &mut Collected::default())
            .await
            .expect_err("the served sidecar contradicts the listing");
        let s = format!("{err:#}");
        assert!(
            s.contains(".meta") && s.contains("advertised"),
            "want the transport's own complaint about the sidecar, got: {s}"
        );
    }

    /// A `.dat` longer than the size its own listing declared is refused while
    /// it streams — the declared size bounds the read even when the caller
    /// sets no budget (the listing says 4 bytes; the byte plane serves 5).
    /// Mutate: pass `max_bytes` instead of the declared-size bound and this
    /// reds on the blake3 complaint instead.
    #[tokio::test]
    async fn a_dat_longer_than_its_declared_size_is_refused_mid_read() {
        let server = MockServer::start().await;
        serving_dat(&server, b"reals").await;
        let rpc = ListingOnly {
            blake3_hex: hex::encode(blake3::hash(b"reals").as_bytes()),
            meta_blake3_hex: hex::encode(blake3::hash(b"sidecar").as_bytes()),
        };
        let bytes = byte_plane(&server.uri());
        let source = NestBootstrapSource::new(&rpc, &bytes, binding());

        source.list_segments(&scope()).await.unwrap();
        let err = source
            .fetch_segment(&scope(), &offer(), u64::MAX, &mut Collected::default())
            .await
            .expect_err("five bytes against a declared four");
        assert!(
            err.downcast_ref::<crate::nest_client::SegmentBodyTooLarge>()
                .is_some(),
            "got: {err:#}"
        );
    }

    /// The caller's byte bound reaches the `.meta` half too: a sidecar longer
    /// than `max_bytes` is refused mid-read as `SegmentBodyTooLarge` naming the
    /// `.meta`, while the `.dat` before it (within the bound) streamed fine.
    /// Mutate: fetch the sidecar with `u64::MAX` instead of `max_bytes` and the
    /// fetch succeeds.
    #[tokio::test]
    async fn a_meta_longer_than_the_bound_is_refused_mid_read() {
        let server = MockServer::start().await;
        serving_dat(&server, b"real").await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/api/v1/segments/{KIND}/{}/0/meta",
                actor_hex()
            )))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"sidecar".to_vec()))
            .mount(&server)
            .await;
        let rpc = ListingOnly {
            blake3_hex: hex::encode(blake3::hash(b"real").as_bytes()),
            meta_blake3_hex: hex::encode(blake3::hash(b"sidecar").as_bytes()),
        };
        let bytes = byte_plane(&server.uri());
        let source = NestBootstrapSource::new(&rpc, &bytes, binding());

        source.list_segments(&scope()).await.unwrap();
        // Four bytes of `.dat` fit; the seven-byte sidecar does not.
        let err = source
            .fetch_segment(&scope(), &offer(), 5, &mut Collected::default())
            .await
            .expect_err("seven sidecar bytes against a five-byte bound");
        let too_large = err
            .downcast_ref::<crate::nest_client::SegmentBodyTooLarge>()
            .unwrap_or_else(|| panic!("got: {err:#}"));
        assert_eq!(too_large.half, ".meta");
        assert_eq!(too_large.max_bytes, 5);
    }
}
