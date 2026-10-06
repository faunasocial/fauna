//! The production [`BootstrapSource`] — a fresh replica pulling one content
//! scope's segments off a real nest.
//!
//! Owner: `docs/goal/architecture/account-data-plane.md` § Store logical
//! schema → *How nest CARv2 segments map onto the local log (the bootstrap
//! contract)*: a fresh replica bootstraps per scope by adopting pulled segment
//! files verbatim, rebuilding its record index from each sidecar's
//! `record_order`, then walking the scope's feed from a zero frontier.
//!
//! # It lives here, not in the store
//!
//! `fauna-account-store` is a shared floor that must keep compiling for
//! wasm32, so it may hold no network dependency and declares the seam instead
//! ([`BootstrapSource`]). This crate may hold one. Nothing in this module is
//! adoption *policy* — every check that decides whether bytes are admissible
//! is [`fauna_account_store::segments::admit`]'s, behind
//! [`AccountStore::adopt_segment`](fauna_account_store::store::AccountStore::adopt_segment).
//!
//! # It adapts the custodian-pull mechanics; it does not fork them
//!
//! Enumeration is the same `fauna.segments.list` control plane the backup arm
//! reads ([`crate::segment_backup::SEGMENTS_LIST_KIND`]), and bytes come off
//! the same [`SyncClient`] byte plane ([`SyncClient::segment_body`] +
//! [`SyncClient::segment_meta_body`] — the same pair the custodian pull
//! ships, since 2026-08-29). What differs is the *sink*: a backup pass seals
//! the opaque pair for elsewhere; this streams the pair, a chunk at a time,
//! into the store's staging slot, which verifies it and becomes a replica.
//!
//! # Why this is not [`crate::segment_backup::SegmentSource`]
//!
//! That trait is the backup arm's: keyed `(kind, scope_hex)`, the pair opaque.
//! Adoption is keyed on the plane's **scope** — a unit of subscription that
//! spans kinds — and *opens* the `.meta`, because the sidecar's `record_order`
//! is what rebuilds the index in append order. The two seams are
//! cross-referenced at both sites and stay apart.
//!
//! # The scope string is ratified; this seam predates it
//!
//! A content scope's string is now ratified as `content:<kind>:<scope-id-hex>`
//! (charter § Feeds and cursors → *The scope string*; constructor/parser
//! `fauna_protocol::scope`). This source still takes the `(kind, scope_id)`
//! mapping **from its caller** ([`ScopeBinding`]) — it was built before the
//! ruling, when minting a convention here would have frozen an unratified
//! name into at-rest journal rows. The production caller the feed-walk slice
//! builds should derive the binding from a
//! [`ContentScope`](fauna_protocol::scope::ContentScope) rather than spelling
//! strings by hand (the charter's § Implementation status names this gap). A
//! source still refuses any scope but the one it was built for, so a caller
//! that mixes two scopes gets an error instead of a scope's records filed
//! under its neighbour's name.

use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::{Context, Result, anyhow, bail};
use fauna_account_store::segments::{SegmentHalf, SegmentSink};
use fauna_account_store::store::{BootstrapSource, SegmentOffer};
use fauna_protocol::RpcRequester;
use fauna_protocol::segments::{SegmentRef, SegmentsListReply, SegmentsListRequest};

use crate::nest_client::{SegmentBody, SyncClient};
use crate::segment_backup::SEGMENTS_LIST_KIND;

// The binding moved with the feed-walk half that derives it
// (`content_scope_plane::binding_for`) into the wasm-capable plane crate;
// re-exported so `bootstrap_source::ScopeBinding` reads unchanged.
pub use fauna_account_plane::content_scope_plane::ScopeBinding;

/// A [`BootstrapSource`] over one nest, for one scope.
pub struct NestBootstrapSource<'a, R: RpcRequester> {
    /// The nest's WS-RPC surface — `fauna.segments.list`.
    rpc: &'a R,
    /// The nest's byte plane — the segment pair.
    bytes: &'a SyncClient,
    binding: ScopeBinding,
    /// What the last enumeration advertised, keyed `(kind, segment_id)`.
    ///
    /// [`SegmentOffer`] deliberately carries only what the *store* reads (the
    /// hydration axis and an advisory size), so the listed BLAKE3 — which is
    /// what makes the in-transit check possible — is kept here instead of
    /// widening a type the store owns. Listed segments are finalized by
    /// construction (the list handler finalizes before it reports, and a
    /// further append rotates a new segment), so a digest recorded here stays
    /// true for the fetch that follows it.
    listed: Mutex<HashMap<(String, u32), SegmentRef>>,
}

impl<'a, R: RpcRequester> NestBootstrapSource<'a, R> {
    pub fn new(rpc: &'a R, bytes: &'a SyncClient, binding: ScopeBinding) -> Self {
        Self {
            rpc,
            bytes,
            binding,
            listed: Mutex::new(HashMap::new()),
        }
    }

    fn check_scope(&self, scope: &str) -> Result<()> {
        if scope != self.binding.scope {
            bail!(
                "this source serves scope {:?}, asked for {scope:?} — a source is built for \
                 one scope's (kind, scope_id) mapping and cannot guess another's",
                self.binding.scope
            );
        }
        Ok(())
    }
}

impl<R: RpcRequester> BootstrapSource for NestBootstrapSource<'_, R> {
    async fn list_segments(&self, scope: &str) -> Result<Vec<SegmentOffer>> {
        self.check_scope(scope)?;

        let mut offers = Vec::new();
        let mut listed = HashMap::new();
        for kind in &self.binding.kinds {
            let req = SegmentsListRequest {
                kind: kind.clone(),
                actor_id: self.binding.actor_hex.clone(),
                extra: Default::default(),
            };
            let reply: SegmentsListReply = self
                .rpc
                .request(SEGMENTS_LIST_KIND, req)
                .await
                .map_err(|e| anyhow!("{SEGMENTS_LIST_KIND} (kind={kind}): {e}"))?;
            for seg in reply.segments {
                offers.push(SegmentOffer {
                    kind: kind.clone(),
                    segment_id: seg.segment_id,
                    dat_size: Some(seg.size_bytes),
                });
                listed.insert((kind.clone(), seg.segment_id), seg);
            }
        }
        *self.listed.lock().expect("listed map is never poisoned") = listed;
        Ok(offers)
    }

    async fn fetch_segment(
        &self,
        scope: &str,
        offer: &SegmentOffer,
        max_bytes: u64,
        into: &mut impl SegmentSink,
    ) -> Result<()> {
        self.check_scope(scope)?;

        // The `.dat` is bounded by its own listing too: the nest declared its
        // size, and a body longer than that is refused mid-stream — the
        // declared size is the one ceiling a segment has
        // (`message-segment-store.md` § Segment size).
        let dat_bound = offer
            .dat_size
            .map_or(max_bytes, |declared| declared.min(max_bytes));

        // `.dat` first: the route finalizes the open segment on read, and the
        // sidecar only exists once a segment is finalized. Fetching the pair
        // in the other order would be a race against a scope still being
        // appended to on the nest.
        let dat = self
            .bytes
            .segment_body(
                &offer.kind,
                &self.binding.actor_hex,
                offer.segment_id,
                0,
                dat_bound,
            )
            .await;
        let dat_hash = stream_half(dat, SegmentHalf::Dat, into)
            .await
            .with_context(|| {
                format!(
                    "bootstrap fetch .dat (kind={}, segment={})",
                    offer.kind, offer.segment_id
                )
            })?;

        // The in-transit check the charter asks for ("CID-verified"). Adoption
        // re-verifies every block against its own CID afterwards, so this is
        // not the security boundary — it is what makes a *truncated or
        // substituted transfer* say so at the transport, where the fix is a
        // re-fetch, instead of surfacing as "this container is malformed".
        if let Some(expected) = self
            .listed
            .lock()
            .expect("listed map is never poisoned")
            .get(&(offer.kind.clone(), offer.segment_id))
            .map(|seg| seg.blake3_hex.clone())
            && dat_hash != expected
        {
            bail!(
                "bootstrap fetch (kind={}, segment={}): the .dat this nest served hashes to \
                 {dat_hash}, but its own listing advertised {expected}",
                offer.kind,
                offer.segment_id
            );
        }

        let meta = self
            .bytes
            .segment_meta_body(
                &offer.kind,
                &self.binding.actor_hex,
                offer.segment_id,
                max_bytes,
            )
            .await;
        let meta_hash = stream_half(meta, SegmentHalf::Meta, into)
            .await
            .with_context(|| {
                format!(
                    "bootstrap fetch .meta (kind={}, segment={})",
                    offer.kind, offer.segment_id
                )
            })?;

        // The sidecar's own in-transit check (`SegmentRef.meta_blake3_hex`).
        // Same reasoning as the `.dat` above.
        if let Some(expected) = self
            .listed
            .lock()
            .expect("listed map is never poisoned")
            .get(&(offer.kind.clone(), offer.segment_id))
            .map(|seg| seg.meta_blake3_hex.clone())
            && meta_hash != expected
        {
            bail!(
                "bootstrap fetch (kind={}, segment={}): the .meta this nest served hashes to \
                 {meta_hash}, but its own listing advertised {expected}",
                offer.kind,
                offer.segment_id
            );
        }

        Ok(())
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
}
