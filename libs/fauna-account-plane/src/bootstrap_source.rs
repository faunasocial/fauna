//! The production [`BootstrapSource`] — a fresh replica pulling one content
//! scope's segments off a real nest, on every host.
//!
//! Owner: `docs/goal/architecture/account-data-plane.md` § Store logical
//! schema → *How nest CARv2 segments map onto the local log (the bootstrap
//! contract)*: a fresh replica bootstraps per scope by adopting pulled segment
//! files verbatim, rebuilding its record index from each sidecar's
//! `record_order`, then walking the scope's feed from a zero frontier.
//!
//! # The byte plane is the host's seam
//!
//! The listing (`fauna.segments.list`, over the host's requester) and the
//! in-transit BLAKE3 check are byte-plane-agnostic and live here, once; the
//! byte fetch is [`SegmentBytes`] — one segment half streamed into a
//! [`SegmentSink`] under a byte bound. Natively the nest client's byte plane
//! (`fauna_sync_engine::nest_client::SyncClient`, whose crate re-exports this
//! source at its old path); on web a streaming `fetch` of the segment byte
//! route (`fauna_rpc_wasm::segment_http`) — `account-replica-posture.md`
//! § *Corollary — the cold device*, the web clause's bulk half. Nothing in
//! this module is adoption *policy* — every check that decides whether bytes
//! are admissible is [`fauna_account_store::segments::admit`]'s, behind
//! [`AccountStore::adopt_segment`](fauna_account_store::store::AccountStore::adopt_segment).
//!
//! # Why this is not the backup arm's `SegmentSource`
//!
//! That trait (`fauna_sync_engine::segment_backup`) is keyed `(kind,
//! scope_hex)`, the pair opaque. Adoption is keyed on the plane's **scope** — a
//! unit of subscription that spans kinds — and *opens* the `.meta`, because the
//! sidecar's `record_order` is what rebuilds the index in append order. The two
//! seams stay apart.
//!
//! # The scope binding
//!
//! The source takes the `(kind, scope_id)` mapping from its caller
//! ([`ScopeBinding`], derived from a
//! [`ContentScope`](fauna_protocol::scope::ContentScope) by
//! [`crate::content_scope_plane::binding_for`]). A source refuses any scope but
//! the one it was built for, so a caller that mixes two scopes gets an error
//! instead of a scope's records filed under its neighbour's name.

use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::{Context, Result, anyhow, bail};
use fauna_account_store::segments::{SegmentHalf, SegmentSink};
use fauna_account_store::store::{BootstrapSource, SegmentOffer};
use fauna_protocol::RpcRequester;
use fauna_protocol::segments::{
    KIND_SEGMENTS_LIST, SegmentRef, SegmentsListReply, SegmentsListRequest,
};

pub use crate::content_scope_plane::ScopeBinding;

/// A nest's segment byte route — `GET /api/v1/segments/{kind}/{actor}/{id}`
/// and its `/meta` sidecar under the session's bearer — as one host fetches
/// it. One method: stream `half` of `(kind, actor_hex, segment_id)` into
/// `into` a chunk at a time, refusing a body past `max_bytes` *while reading
/// it* (`fauna_account_store::segments::SegmentBodyTooLarge`, typed so a
/// caller can tell an over-bound refusal from a transport failure). The
/// transfer holds one transport chunk, never the half: a segment has no size
/// ceiling (`message-segment-store.md` § Segment size).
#[allow(async_fn_in_trait)] // static dispatch only; per-impl Send inference
pub trait SegmentBytes {
    async fn fetch_half(
        &self,
        kind: &str,
        actor_hex: &str,
        segment_id: u32,
        half: SegmentHalf,
        max_bytes: u64,
        into: &mut impl SegmentSink,
    ) -> Result<()>;
}

/// A [`BootstrapSource`] over one nest, for one scope.
pub struct NestBootstrapSource<'a, R: RpcRequester, S: SegmentBytes> {
    /// The nest's WS-RPC surface — `fauna.segments.list`.
    rpc: &'a R,
    /// The nest's byte plane — the segment pair.
    bytes: &'a S,
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

impl<'a, R: RpcRequester, S: SegmentBytes> NestBootstrapSource<'a, R, S> {
    pub fn new(rpc: &'a R, bytes: &'a S, binding: ScopeBinding) -> Self {
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

    /// Stream one half through the host's byte plane into `into`, hashing as
    /// it goes; returns the half's BLAKE3 (hex) for the in-transit check.
    async fn stream_half(
        &self,
        offer: &SegmentOffer,
        half: SegmentHalf,
        max_bytes: u64,
        into: &mut impl SegmentSink,
    ) -> Result<String> {
        let mut hashing = Hashing {
            inner: into,
            hasher: blake3::Hasher::new(),
        };
        self.bytes
            .fetch_half(
                &offer.kind,
                &self.binding.actor_hex,
                offer.segment_id,
                half,
                max_bytes,
                &mut hashing,
            )
            .await?;
        Ok(hex::encode(hashing.hasher.finalize().as_bytes()))
    }
}

/// A sink that hashes every chunk on its way to the real one.
struct Hashing<'s, K> {
    inner: &'s mut K,
    hasher: blake3::Hasher,
}

impl<K: SegmentSink> SegmentSink for Hashing<'_, K> {
    async fn write(&mut self, half: SegmentHalf, chunk: &[u8]) -> Result<()> {
        self.hasher.update(chunk);
        self.inner.write(half, chunk).await
    }
}

impl<R: RpcRequester, S: SegmentBytes> BootstrapSource for NestBootstrapSource<'_, R, S> {
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
                .request(KIND_SEGMENTS_LIST, req)
                .await
                .map_err(|e| anyhow!("{KIND_SEGMENTS_LIST} (kind={kind}): {e}"))?;
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
        let dat_hash = self
            .stream_half(offer, SegmentHalf::Dat, dat_bound, into)
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

        let meta_hash = self
            .stream_half(offer, SegmentHalf::Meta, max_bytes, into)
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
