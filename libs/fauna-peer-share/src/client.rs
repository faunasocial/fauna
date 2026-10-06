//! The pull side: dial an admitted member of a shared set, then read that set's
//! bytes and rows over the peer channel (`p2p-shared-set-build.md` § Cross-user shared-set
//! transfer → *Build contract*; row 59 slice B).
//!
//! # Bytes ride the EXISTING walk
//!
//! [`PeerShareBlobFetcher`] implements
//! [`fauna_core::file_download::BlobFetcher`] — the same seam the nest-backed
//! and browser-backed fetchers implement. So a peer-served file opens through
//! `download_file_bytes_by_manifest` with **no new verification code and no new
//! key handling**: one walk, one integrity policy, one place the M2 content key
//! is applied. That is what "the same machinery serves the contact-plane case"
//! means concretely (`account-data-plane.md` § The peer leg → *The cross-account
//! twin*), and it is why the multi-source claim costs nothing here: a manifest
//! or chunk is addressed by its own hash, so a member serving it is
//! interchangeable with the nest.
//!
//! This fetcher also independently **re-hashes every body against the address
//! it asked for, before returning it** (wormability rule 4 — received content
//! inert at the transfer layer). `fauna_core::file_download::fetch_manifest`
//! makes the same check for every fetcher now, so this is redundant with the
//! walk's own guard — kept because it fails earlier, names the peer, and keeps
//! the rule visible at the transfer boundary it belongs to. A hostile member's substituted bytes
//! therefore die at the transfer boundary with a precise error, rather than
//! surviving to a confusing whole-file mismatch at the end of a large
//! download.
//!
//! # Rows do NOT ride the existing walk
//!
//! Change rows have no content address, so [`fetch_share_changes`] screens
//! every row through [`crate::provenance::screen_peer_row`]: a writer-signed
//! row passes on to the reader's judgement (it verifies self-contained, whoever
//! served it); an unsigned row must be the channel-proven serving peer's own,
//! from a **cached writer**. Screened rows are for a **provisional read-side
//! overlay** only, after the state writer's authoritative
//! [`crate::provenance::judge_peer_row`] — the nest stays the log's arbiter,
//! and a provisional delete never destroys local bytes (the consumer's
//! obligations, owned by `p2p.md` § *Peer-served change-row provenance*).

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use fauna_core::chunker::MAX_STORED_CHUNK_BODY;
use fauna_core::data::ContentHash;
use fauna_core::file_download::TransferredBeforeFailure;
use fauna_peer_channel::PeerChannel;
// The peer-channel `RpcRequester` is the same-account leg's, reused verbatim:
// "the same sync contract over a different transport" is transport-level, so a
// second copy for the share plane would be pure divergence.
use fauna_peer_sync::admission::AdmissionVerdict;
use fauna_peer_sync::client::PeerRequester;
use fauna_protocol::RpcRequester;
use fauna_protocol::peer_share::{
    KIND_PEER_SHARE_ADMIT, KIND_PEER_SHARE_CHANGES_LIST, KIND_PEER_SHARE_CHUNKS_PULL,
    KIND_PEER_SHARE_MANIFESTS_GET, PeerShareAdmitReply, PeerShareAdmitRequest, PeerShareChange,
    PeerShareChangesListReply, PeerShareChangesListRequest, PeerShareChunkWant,
    PeerShareChunksPullReply, PeerShareChunksPullRequest, PeerShareGroupWitness,
    PeerShareManifestsGetReply, PeerShareManifestsGetRequest, WITNESS_GROUP_MEMBERSHIP,
    WITNESS_M2_MEMBERSHIP,
};
use fauna_protocol::sync::SyncChange;
use serde_bytes::ByteBuf;

use crate::admission::{SetMembership, evaluate_share_witness};
use crate::provenance::{RowRefusal, screen_peer_row};

/// The most pull rounds [`PeerShareBlobFetcher::fetch_chunks`] will run for one
/// want list. Each round moves up to one reply's byte budget (~700 KiB) per
/// call, and a chunk body caps at 8 MiB (`MAX_STORED_CHUNK_BODY`, which each
/// slice's declared length is checked against), so
/// a single body needs ~12 rounds and a batch of them proportionally more —
/// hence a cap sized for a full want list rather than a single body. It exists
/// only to stop a peer that never advances from wedging the puller; the
/// no-progress bail below catches that case far sooner in practice. A Rust
/// constant — never a knob.
const MAX_ROUNDS: usize = 512;

/// What one share admission exchange yielded. Both halves are here because the
/// seam's "sides admit independently" rule means neither implies the other.
#[derive(Debug, Clone, PartialEq)]
pub struct ShareAdmission {
    /// The sets the responder admitted **us** to — what this side may now pull.
    /// Advisory in origin (the responder's own verdict is the enforcement) but
    /// authoritative for *planning*: pulling outside it just earns
    /// `fauna.peer.share.not_admitted`.
    pub admitted_by_peer: Vec<[u8; 32]>,
    /// **Our** verdict about the responder, from evaluating its claim against
    /// *our* roster — what this side would serve it. Held by the caller for its
    /// own serve path; never derived from anything the peer said about itself.
    pub our_verdict: AdmissionVerdict,
    /// The sets our own evaluation admitted the responder to.
    pub we_admitted: Vec<[u8; 32]>,
}

/// The client half of the share admission exchange: claim `own_claimed_sets`,
/// and **independently** evaluate the responder's claim against `membership`.
///
/// `proven_peer_actor` is the channel-proven peer identity (PT-1b: on the
/// contact plane the NodeId *is* the actor key). It is passed in rather than
/// read from the reply for the obvious reason — a reply cannot be its own
/// evidence.
///
/// A responder that admits nothing answers `ERR_WITNESS_REFUSED` and this
/// returns the error; a responder whose own claim *we* refuse is not fatal to
/// the pull direction, so `our_verdict` failing is reported as an error only
/// when the caller asked for mutual admission — which is why the two halves
/// are separate fields rather than one boolean.
pub async fn admit_share_over(
    channel: Arc<PeerChannel>,
    own_claimed_sets: &[[u8; 32]],
    membership: &dyn SetMembership,
    proven_peer_actor: &[u8; 32],
) -> Result<ShareAdmission> {
    let requester = PeerRequester::new(channel);
    let req = PeerShareAdmitRequest {
        witness_kind: WITNESS_M2_MEMBERSHIP.to_string(),
        claimed_sets: own_claimed_sets
            .iter()
            .map(|s| ByteBuf::from(s.to_vec()))
            .collect(),
        group_witnesses: Vec::new(),
        extra: Default::default(),
    };
    let reply: PeerShareAdmitReply = requester
        .request(KIND_PEER_SHARE_ADMIT, req)
        .await
        .context("peer-share admit exchange")?;

    let admitted_by_peer = reply
        .admitted_sets
        .iter()
        .map(|s| {
            <[u8; 32]>::try_from(s.as_ref())
                .map_err(|_| anyhow::anyhow!("the peer named a malformed admitted set id"))
        })
        .collect::<Result<Vec<_>>>()?;
    // Our own half: evaluate what the peer claimed against OUR roster. The
    // claimed ids are the peer's assertion; the verdict is ours.
    let (our_verdict, we_admitted) = evaluate_share_witness(
        &reply.witness_kind,
        &reply.claimed_sets,
        membership,
        proven_peer_actor,
    )
    .context("evaluating the responder's own share claim")?;

    Ok(ShareAdmission {
        admitted_by_peer,
        our_verdict,
        we_admitted,
    })
}

/// Present carried group-membership certificates to a peer
/// ([`WITNESS_GROUP_MEMBERSHIP`]): one `(scope id, Enrolled entry bytes)` per
/// group scope. Returns the scope ids the peer admitted.
///
/// Deliberately one-directional (dialer presents, responder holds the
/// verdict): the post-ceremony pull is the dialer's, and the mutual half of
/// the exchange stays the M2 claim vocabulary — a mutual *certificate*
/// exchange is a recorded residual, added when a flow needs it rather than
/// speculatively.
pub async fn admit_group_over(
    channel: Arc<PeerChannel>,
    witnesses: &[([u8; 32], Vec<u8>)],
) -> Result<Vec<[u8; 32]>> {
    let requester = PeerRequester::new(channel);
    let req = PeerShareAdmitRequest {
        witness_kind: WITNESS_GROUP_MEMBERSHIP.to_string(),
        claimed_sets: Vec::new(),
        group_witnesses: witnesses
            .iter()
            .map(|(scope, entry)| PeerShareGroupWitness {
                scope_id: ByteBuf::from(scope.to_vec()),
                entry: ByteBuf::from(entry.clone()),
                extra: Default::default(),
            })
            .collect(),
        extra: Default::default(),
    };
    let reply: PeerShareAdmitReply = requester
        .request(KIND_PEER_SHARE_ADMIT, req)
        .await
        .context("peer-share group admit exchange")?;
    reply
        .admitted_sets
        .iter()
        .map(|s| {
            <[u8; 32]>::try_from(s.as_ref())
                .map_err(|_| anyhow::anyhow!("the peer named a malformed admitted scope id"))
        })
        .collect()
}

/// One page of a peer's change rows, split by the ingest screen's verdict.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ShareChangesPage {
    /// Rows the screen passed, each keeping its wire `sequenced` marking and
    /// inline cert. The consumer judges each ([`crate::provenance::judge_peer_row`])
    /// and overlays the admitted ones **provisionally**; an un-sequenced row is
    /// never treated as converged log.
    pub accepted: Vec<PeerShareChange>,
    /// Rows refused, with the reason. Returned rather than dropped: a silent
    /// drop would make a misbehaving (or merely stale-rostered) peer look like
    /// a converged one.
    pub refused: Vec<(SyncChange, RowRefusal)>,
    /// The page stopped at the serve bound with rows still to come — re-request
    /// from the highest `seq` seen.
    pub more: bool,
}

/// Read one page of the set's change rows from an admitted peer, applying the
/// provenance ruling's binding-free ingest screen.
///
/// `peer_is_cached_writer` is this side's cached-roster answer for the serving
/// peer on this set — `false` whenever there is no cached role at all, which is
/// the ruling's fail-closed arm for UNSIGNED rows: refused, bytes unaffected. A
/// signed row is the reader's to judge, so it passes the screen whatever the
/// serving peer's role.
pub async fn fetch_share_changes(
    channel: Arc<PeerChannel>,
    set: &[u8; 32],
    since: i64,
    proven_peer_actor: &[u8; 32],
    peer_is_cached_writer: bool,
) -> Result<ShareChangesPage> {
    let requester = PeerRequester::new(channel);
    let reply: PeerShareChangesListReply = requester
        .request(
            KIND_PEER_SHARE_CHANGES_LIST,
            PeerShareChangesListRequest {
                set: ByteBuf::from(set.to_vec()),
                since,
                extra: Default::default(),
            },
        )
        .await
        .context("peer-share changes.list")?;

    let proven_hex = hex::encode(proven_peer_actor);
    let mut page = ShareChangesPage {
        more: reply.more,
        ..Default::default()
    };
    for row in reply.changes {
        match screen_peer_row(
            &row.change,
            row.sequenced,
            &proven_hex,
            peer_is_cached_writer,
        ) {
            Ok(()) => page.accepted.push(row),
            Err(refusal) => page.refused.push((row.change, refusal)),
        }
    }
    Ok(page)
}

/// A [`BlobFetcher`](fauna_core::file_download::BlobFetcher) that reads one
/// shared set's manifests and chunks from one admitted peer.
///
/// Scoped to a single set because that is the unit the peer's verdict admits;
/// a caller pulling several sets holds one fetcher per set, which also keeps
/// the wrong set's id from ever reaching a request.
pub struct PeerShareBlobFetcher {
    requester: PeerRequester,
    set: [u8; 32],
}

impl PeerShareBlobFetcher {
    pub fn new(channel: Arc<PeerChannel>, set: [u8; 32]) -> Self {
        Self {
            requester: PeerRequester::new(channel),
            set,
        }
    }

    fn set_bytes(&self) -> ByteBuf {
        ByteBuf::from(self.set.to_vec())
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl fauna_core::file_download::BlobFetcher for PeerShareBlobFetcher {
    async fn fetch_manifest(&self, hash: &ContentHash) -> Result<Vec<u8>> {
        let reply: PeerShareManifestsGetReply = self
            .requester
            .request(
                KIND_PEER_SHARE_MANIFESTS_GET,
                PeerShareManifestsGetRequest {
                    set: self.set_bytes(),
                    manifest_hashes: vec![ByteBuf::from(hash.digest().to_vec())],
                    extra: Default::default(),
                },
            )
            .await
            .context("peer-share manifests.get")?;
        let Some(served) = reply.manifests.into_iter().next() else {
            // The two empty-handed answers mean DIFFERENT things and must not be
            // conflated. `missing` is recoverable — ask another member, or the
            // nest. `deferred` on a **single-hash** request cannot be: there is
            // no smaller want list to retry with, so it says the manifest body
            // itself exceeds one reply's budget. Reporting that as "the peer does
            // not hold it" would send a puller round the whole membership
            // chasing a manifest every member holds.
            //
            // It is reachable only for an enormous file: a manifest is ~64 bytes
            // per chunk and a chunk is up to 8 MiB, so the budget bites around
            // ~11k chunks ≈ 87 GB. Naming it honestly is what tells a future
            // slice to make manifest fetches ranged the way chunk pulls now are,
            // rather than leaving a phantom bug to be re-diagnosed.
            if !reply.deferred.is_empty() {
                bail!(
                    "the peer holds manifest {} but it exceeds one reply's byte budget — \
                     manifest fetches are not ranged yet (only chunk pulls are), so a file \
                     this large cannot transfer over the share leg",
                    hex::encode(hash.digest())
                );
            }
            bail!(
                "the peer does not hold manifest {}",
                hex::encode(hash.digest())
            );
        };
        // Rule 4 at the transfer boundary: the body must hash to the address we
        // asked for. `fauna_core::file_download::fetch_manifest` now makes the
        // same check for every fetcher, so this one is redundant — kept because
        // it fails earlier, names the peer, and keeps the rule visible at the
        // transfer boundary it belongs to.
        let actual = ContentHash::of_raw(&served.bytes);
        if actual != *hash {
            bail!(
                "peer-served manifest hash mismatch: asked for {}, got bytes hashing to {}",
                hex::encode(hash.digest()),
                hex::encode(actual.digest())
            );
        }
        Ok(served.bytes.into_vec())
    }

    async fn fetch_chunks(
        &self,
        store_keys: &[ContentHash],
        relative_path: &str,
    ) -> Result<Vec<Vec<u8>>> {
        pull_chunk_bodies(&self.requester, &self.set, store_keys, relative_path).await
    }
}

/// [`PeerShareBlobFetcher::fetch_chunks`] over any requester, so a hostile
/// peer can be scripted without a channel. A failed pull carries the body
/// bytes that had arrived as [`TransferredBeforeFailure`] context: they
/// crossed the wire, so the caller charges them to the transfer ledger (the
/// share pump's `spool_planned`).
async fn pull_chunk_bodies<R>(
    requester: &R,
    set: &[u8; 32],
    store_keys: &[ContentHash],
    relative_path: &str,
) -> Result<Vec<Vec<u8>>>
where
    R: RpcRequester,
    R::Error: Into<anyhow::Error>,
{
    let mut arrived = 0u64;
    pull_rounds(requester, set, store_keys, relative_path, &mut arrived)
        .await
        .map_err(|e| e.context(TransferredBeforeFailure(arrived)))
}

/// The round loop behind [`pull_chunk_bodies`]. `arrived` counts every body
/// byte a reply carried, **before** that reply is validated — bytes a refused
/// slice brought still crossed the wire.
async fn pull_rounds<R>(
    requester: &R,
    set: &[u8; 32],
    store_keys: &[ContentHash],
    relative_path: &str,
    arrived: &mut u64,
) -> Result<Vec<Vec<u8>>>
where
    R: RpcRequester,
    R::Error: Into<anyhow::Error>,
{
    // Bodies keyed by store key, filled across deferral rounds, then
    // re-ordered to match `store_keys` — the seam's contract is "parallel to
    // store_keys", and a duplicate key in the want list must yield the same
    // body twice rather than shifting the list.
    // Per store key: the bytes assembled so far, and the body's total length
    // once a slice has told us. Distinct keys only — a duplicate key in the
    // want list is pulled once and handed back twice at the end (the seam's
    // contract is "parallel to store_keys", not "one fetch per element").
    struct Assembling {
        bytes: Vec<u8>,
        total_len: Option<u64>,
    }
    let mut building: std::collections::HashMap<ContentHash, Assembling> =
        std::collections::HashMap::new();
    let mut distinct: Vec<ContentHash> = Vec::new();
    for key in store_keys {
        if !distinct.contains(key) {
            distinct.push(*key);
            building.insert(
                *key,
                Assembling {
                    bytes: Vec::new(),
                    total_len: None,
                },
            );
        }
    }

    // A body is whole when its assembled length reaches the served
    // `total_len`; until then it is re-wanted from that length.
    let incomplete = |building: &std::collections::HashMap<ContentHash, Assembling>| {
        distinct
            .iter()
            .filter(|k| {
                let a = &building[k];
                a.total_len != Some(a.bytes.len() as u64)
            })
            .copied()
            .collect::<Vec<_>>()
    };

    for _round in 0..MAX_ROUNDS {
        let outstanding = incomplete(&building);
        if outstanding.is_empty() {
            break;
        }
        let reply: PeerShareChunksPullReply = requester
            .request(
                KIND_PEER_SHARE_CHUNKS_PULL,
                PeerShareChunksPullRequest {
                    set: ByteBuf::from(set.to_vec()),
                    wants: outstanding
                        .iter()
                        .map(|k| PeerShareChunkWant {
                            store_key: ByteBuf::from(k.digest().to_vec()),
                            offset: building[k].bytes.len() as u64,
                            extra: Default::default(),
                        })
                        .collect(),
                    extra: Default::default(),
                },
            )
            .await
            .map_err(Into::<anyhow::Error>::into)
            .with_context(|| format!("peer-share chunks.pull for {relative_path}"))?;
        // Counted before any slice is judged: a slice refused below still
        // crossed the wire, and the caller charges it.
        *arrived = reply
            .chunks
            .iter()
            .fold(*arrived, |acc, c| acc.saturating_add(c.bytes.len() as u64));

        if let Some(missing) = reply.missing.first() {
            bail!(
                "the peer does not hold chunk {} of {relative_path}",
                hex::encode(missing)
            );
        }
        let mut advanced = 0usize;
        for chunk in reply.chunks {
            let claimed = <[u8; 32]>::try_from(chunk.store_key.as_ref())
                .map_err(|_| anyhow::anyhow!("a store key must be exactly 32 bytes"))?;
            let key = ContentHash::from_digest_raw(claimed);
            let Some(slot) = building.get_mut(&key) else {
                bail!(
                    "the peer served chunk {} of {relative_path}, which was never wanted",
                    hex::encode(claimed)
                );
            };
            // A slice must continue exactly where this side left off.
            // Anything else (a gap, a rewind, an overlap) would silently
            // corrupt the assembled body, so it is refused rather than
            // patched.
            if chunk.offset != slot.bytes.len() as u64 {
                bail!(
                    "peer-served chunk {} of {relative_path} arrived at offset {} but this \
                     side holds {} bytes — a non-contiguous slice",
                    hex::encode(claimed),
                    chunk.offset,
                    slot.bytes.len()
                );
            }
            // The declared length is the peer's word, and it decides how many
            // rounds this side keeps buffering for. No honest body is longer
            // than a sealed maximum-size chunk, so a longer claim is refused
            // on the slice that makes it — before a byte of it is kept.
            // Without this, a hostile member could declare an enormous length
            // and feed one reply's budget per round for `MAX_ROUNDS` rounds,
            // hundreds of MiB held here, then stall before the hash check at
            // the end ever runs.
            if chunk.total_len > MAX_STORED_CHUNK_BODY {
                bail!(
                    "peer-served chunk {} of {relative_path} declares a {}-byte body, over the \
                     {MAX_STORED_CHUNK_BODY}-byte ceiling for any stored chunk",
                    hex::encode(claimed),
                    chunk.total_len
                );
            }
            if let Some(known) = slot.total_len {
                if known != chunk.total_len {
                    bail!(
                        "peer-served chunk {} of {relative_path} changed its total length \
                         mid-transfer ({known} then {})",
                        hex::encode(claimed),
                        chunk.total_len
                    );
                }
            } else {
                slot.total_len = Some(chunk.total_len);
            }
            if slot.bytes.len() as u64 + chunk.bytes.len() as u64 > chunk.total_len {
                bail!(
                    "peer-served chunk {} of {relative_path} overran its declared length",
                    hex::encode(claimed)
                );
            }
            advanced += chunk.bytes.len();
            slot.bytes.extend_from_slice(&chunk.bytes);
        }
        if advanced == 0 && !incomplete(&building).is_empty() {
            // Nothing moved and something is still wanted: no later round
            // can do better, so say so instead of spinning to the cap.
            bail!(
                "the peer served no bytes for the outstanding chunks of {relative_path} \
                 — no progress is possible"
            );
        }
    }
    let stalled = incomplete(&building);
    if !stalled.is_empty() {
        bail!(
            "{} chunk(s) of {relative_path} still incomplete after {MAX_ROUNDS} pull rounds",
            stalled.len()
        );
    }

    // Rule 4, on completion: the assembled body must hash to the key it was
    // served under. A slice has no address of its own, so this is the first
    // and only point the check is meaningful — and it happens before any
    // body is handed to the walk.
    for key in &distinct {
        let assembled = &building[key].bytes;
        let actual = ContentHash::of_raw(assembled);
        if actual != *key {
            bail!(
                "peer-served chunk hash mismatch in {relative_path}: served under {}, \
                 assembled bytes hash to {}",
                hex::encode(key.digest()),
                hex::encode(actual.digest())
            );
        }
    }

    Ok(store_keys
        .iter()
        .map(|key| building[key].bytes.clone())
        .collect())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use fauna_core::file_download::bytes_transferred_before_failure;
    use fauna_protocol::peer_share::PeerShareChunk;
    use fauna_protocol::{decode_strict, encode_canonical};

    use super::*;

    /// One reply's byte budget on the share plane (`MAX_ROUNDS`' doc): what a
    /// member can make this side hold per round. The 1 MiB frame is the
    /// transport's ceiling above it.
    const REPLY_BUDGET: usize = 700 * 1024;

    /// A member that answers every `chunks.pull` from `body`, at the offset the
    /// puller asked for, one `slice`-sized piece per reply — while declaring
    /// whatever `total_len` it likes.
    struct ScriptedPeer {
        body: Vec<u8>,
        declared_len: u64,
        slice: usize,
        calls: Mutex<usize>,
    }

    impl ScriptedPeer {
        fn new(body: Vec<u8>, declared_len: u64, slice: usize) -> Self {
            Self {
                body,
                declared_len,
                slice,
                calls: Mutex::new(0),
            }
        }

        fn calls(&self) -> usize {
            *self.calls.lock().unwrap()
        }
    }

    #[derive(Debug)]
    struct NeverFails;
    impl std::fmt::Display for NeverFails {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("the scripted peer never fails a request")
        }
    }
    impl std::error::Error for NeverFails {}

    impl RpcRequester for ScriptedPeer {
        type Error = NeverFails;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, NeverFails>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            assert_eq!(kind, KIND_PEER_SHARE_CHUNKS_PULL);
            *self.calls.lock().unwrap() += 1;
            let req: PeerShareChunksPullRequest =
                decode_strict(&encode_canonical(&payload).expect("encode")).expect("a pull");
            let chunks = req
                .wants
                .iter()
                .map(|want| {
                    let from = (want.offset as usize).min(self.body.len());
                    let to = (from + self.slice).min(self.body.len());
                    PeerShareChunk {
                        store_key: want.store_key.clone(),
                        offset: want.offset,
                        bytes: ByteBuf::from(self.body[from..to].to_vec()),
                        total_len: self.declared_len,
                        extra: Default::default(),
                    }
                })
                .collect();
            let reply = PeerShareChunksPullReply {
                chunks,
                ..Default::default()
            };
            Ok(decode_strict(&encode_canonical(&reply).expect("encode")).expect("decode"))
        }
    }

    /// Deterministic filler; the bytes only ever need a hash.
    fn filler(len: u64) -> Vec<u8> {
        (0..len as usize)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect()
    }

    /// ⚠ a member declaring a body longer than any stored chunk
    /// is refused on the FIRST slice. Without the guard the puller keeps
    /// asking for more — one reply's budget per round, up to `MAX_ROUNDS`
    /// rounds of it held in memory — and fails only at the stall, before the
    /// hash check that would have caught it ever runs.
    #[tokio::test]
    async fn a_declared_length_over_the_largest_stored_chunk_is_refused_on_the_first_slice() {
        let declared = MAX_STORED_CHUNK_BODY + 1;
        let slice = 256 * 1024;
        let peer = ScriptedPeer::new(filler(declared), declared, slice);
        let key = ContentHash::of_raw(b"any address - the refusal comes before the hash");

        let err = pull_chunk_bodies(&peer, &[1u8; 32], &[key], "big/file")
            .await
            .expect_err("an over-long declared body must be refused");

        assert!(
            format!("{err:#}").contains("ceiling for any stored chunk"),
            "refused for the declared length, not something else: {err:#}"
        );
        assert_eq!(
            peer.calls(),
            1,
            "refused on the first slice — no second round was asked for"
        );
        let arrived = bytes_transferred_before_failure(&err);
        assert_eq!(
            arrived, slice as u64,
            "the refused slice's bytes are counted"
        );
        assert!(
            arrived <= REPLY_BUDGET as u64,
            "at most one reply's budget crossed before the refusal"
        );
    }

    /// The bound is not a byte too low: a body of exactly
    /// `MAX_STORED_CHUNK_BODY` (a sealed maximum-size incompressible chunk)
    /// pulls whole across ranged rounds.
    #[tokio::test]
    async fn a_body_of_exactly_the_largest_stored_chunk_pulls_whole() {
        let body = filler(MAX_STORED_CHUNK_BODY);
        let key = ContentHash::of_raw(&body);
        let peer = ScriptedPeer::new(body.clone(), MAX_STORED_CHUNK_BODY, REPLY_BUDGET);

        let bodies = pull_chunk_bodies(&peer, &[1u8; 32], &[key], "big/file")
            .await
            .expect("a legitimate maximum-size body transfers");

        assert_eq!(bodies, vec![body]);
    }

    /// A failed pull reports what arrived through its error, so the pump can
    /// charge it: here a member serves an honest-length body that does not
    /// hash to its address, and the call fails only at completion — after
    /// every byte crossed.
    #[tokio::test]
    async fn a_failed_pull_carries_the_bytes_that_arrived() {
        let len = 3 * 100 * 1024;
        let peer = ScriptedPeer::new(filler(len), len, 100 * 1024);
        let key = ContentHash::of_raw(b"not the body's hash");

        let err = pull_chunk_bodies(&peer, &[1u8; 32], &[key], "some/file")
            .await
            .expect_err("a body that does not hash to its address is refused");

        assert_eq!(peer.calls(), 3);
        assert_eq!(bytes_transferred_before_failure(&err), len);
    }
}
