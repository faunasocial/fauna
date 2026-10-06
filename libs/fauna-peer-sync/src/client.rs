//! The pull side: run the same client machinery against a peer instead of
//! the nest — an [`RpcRequester`] over the peer channel (so
//! `fauna_sync_engine::account_state_plane`'s walk runs store↔store
//! unchanged, constructed pull-only), the client half of the admission
//! exchange, and the want-list block pull.

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use fauna_account_store::backend::StoreBackend;
use fauna_account_store::store::AccountStore;
use fauna_core::data::ContentHash;
use fauna_core::device_endpoints::DeviceEndpoints;
use fauna_core::encoding::EmbedAsBytes;
use fauna_peer_channel::PeerChannel;
use fauna_protocol::RpcRequester;
use fauna_protocol::peer_sync::{
    KIND_PEER_SYNC_ADMIT, KIND_PEER_SYNC_BLOCKS_PULL, PeerSyncAdmitReply, PeerSyncAdmitRequest,
    PeerSyncBlocksPullReply, PeerSyncBlocksPullRequest, WITNESS_DEVICE_AUTHORIZATION,
};
use fauna_protocol::{Value, decode_strict, encode_canonical};
use fauna_transport::{NestPath, bytes_may_ride};

use crate::admission::{AdmissionVerdict, evaluate_witness};

/// An [`RpcRequester`] over one [`PeerChannel`] — what makes the peer leg
/// "the same sync contract over a different transport" literal: hand it to
/// `AccountStatePlane::new_pull_only` and the W2.4 (account-data-plane.md § Workstreams) walk pages the peer's
/// relay plane exactly as it pages the nest feed.
pub struct PeerRequester {
    channel: Arc<PeerChannel>,
}

impl PeerRequester {
    pub fn new(channel: Arc<PeerChannel>) -> Self {
        Self { channel }
    }
}

impl RpcRequester for PeerRequester {
    type Error = anyhow::Error;

    async fn request<Req, Reply>(&self, kind: &'static str, payload: Req) -> Result<Reply>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        let value: Value = decode_strict(&encode_canonical(&payload).context("encode request")?)
            .context("request as Value")?;
        let reply = self
            .channel
            .request(kind, value)
            .await
            .with_context(|| format!("peer request {kind}"))?;
        decode_strict(&encode_canonical(&reply).context("encode reply")?).context("decode reply")
    }
}

/// What one admission exchange yielded: the verdict, plus the peer's half of
/// the per-session endpoint re-exchange (T13 step 4).
///
/// The endpoints are already bound to the channel-proven key
/// ([`crate::discovery::bind_carried_endpoints`]) — `None` means the peer
/// carried nothing, or named someone it had not proven itself to be.
#[derive(Debug, Clone, PartialEq)]
pub struct AdmissionOutcome {
    pub verdict: AdmissionVerdict,
    pub peer_endpoints: Option<DeviceEndpoints>,
}

/// A borrowed custody-revocation view for one admission exchange (`true` =
/// revoked) — the pull-side twin of the serve config's
/// `crate::server::CustodyRevocationFn`, borrowed rather than shared because
/// an exchange is one call, not a held server.
pub type CustodyRevocationView<'a> = &'a (dyn Fn(&[u8]) -> bool + Sync);

/// A borrowed removed-device view for one admission exchange (`true` = that
/// account has withdrawn that device key) — the pull-side twin of the serve
/// config's `crate::server::DeviceRemovedFn`, whose doc owns the rule
/// (refuse on `Removed` only; an absent row admits; a custodied account
/// answers from the held grants' lists).
pub type DeviceRemovedView<'a> = &'a (dyn Fn(&[u8; 32], &[u8; 32]) -> bool + Sync);

/// What THIS side answers about the witness it receives — the evaluator's
/// own synced state, which no witness can carry because every witness is
/// self-contained. One struct rather than a growing positional tail: the
/// seam accepts three witness kinds and each brings its own withdrawal
/// question.
///
/// `Default` (both `None`) is the no-view posture: a custody-grant reply is
/// **refused** (it cannot be evaluated at all without a revocation view) and
/// a `DeviceAuthorization` reply is **admitted** (it is fully evaluated by
/// its verifier; the view only ever withdraws one). Both postures are the
/// serve side's, verbatim.
#[derive(Clone, Copy, Default)]
pub struct AdmissionViews<'a> {
    pub custody_revoked: Option<CustodyRevocationView<'a>>,
    pub device_removed: Option<DeviceRemovedView<'a>>,
}

/// The client half of the admission exchange, same-account form: present a
/// `DeviceAuthorization` witness and expect one back. Delegates to
/// [`admit_over_as`] with no views at all, which fail-closed-refuses a
/// custody-grant reply — correct for the sibling legs this form serves (they
/// dial their own fleet and expect a fleet witness) — and admits a
/// `DeviceAuthorization` reply without consulting device removal. Reach for
/// [`admit_over_as`] where either view matters; the production sibling dial
/// does (`fauna-sync-engine`'s `peer_leg::dial_one`).
///
/// Carries no endpoints and drops the peer's: a same-account dialer already
/// learns its fleet's candidates from the fleet-only `device-endpoints` kind,
/// which is exactly what a non-fleet custody session cannot do (T13 step 4).
/// Reach for [`admit_over_as`] where the re-exchange matters.
pub async fn admit_over(
    channel: &PeerChannel,
    own_witness: EmbedAsBytes,
    expected_account: &[u8; 32],
    now_secs: u64,
) -> Result<AdmissionVerdict> {
    admit_over_as(
        channel,
        WITNESS_DEVICE_AUTHORIZATION,
        own_witness,
        expected_account,
        now_secs,
        AdmissionViews::default(),
        None,
    )
    .await
    .map(|outcome| outcome.verdict)
}

/// The client half of the admission exchange, full form: present
/// `own_witness` (of `own_witness_kind`), and **independently** verify the
/// responder's witness against the channel-proven peer identity — "mutual"
/// means both directions admitted on their own evidence, and the sides'
/// witness kinds may differ (a custodian presents a custody grant and
/// receives a `DeviceAuthorization`; an owner device dialing its custodian
/// does the reverse).
///
/// `views` is THIS side's own synced state about the witness it receives:
/// the revocation view over custody grant ids (a fleet replica answers it
/// from the synced grant-event log; `None` fail-closed-refuses a
/// custody-grant reply, since an evaluator with no revocation view cannot
/// perform T13's custody admission evaluation), and the removed-device view
/// over `(account, device key)` — which is what severs a *sibling's*
/// admission, the fleet cert carrying no expiry of its own. The serve side
/// holds both rules identically, so a removal or a revoke severs **both
/// directions** at the next evaluation.
///
/// `own_endpoints` is this side's current dial identity + candidates for the
/// per-session re-exchange (T13 step 4). Pass the caller's live transport
/// facts; `None` keeps the pre-slot bytes exactly. The peer's own half comes
/// back on [`AdmissionOutcome::peer_endpoints`], already bound to the
/// channel-proven key.
pub async fn admit_over_as(
    channel: &PeerChannel,
    own_witness_kind: &str,
    own_witness: EmbedAsBytes,
    expected_account: &[u8; 32],
    now_secs: u64,
    views: AdmissionViews<'_>,
    own_endpoints: Option<DeviceEndpoints>,
) -> Result<AdmissionOutcome> {
    let req: Value = decode_strict(
        &encode_canonical(&PeerSyncAdmitRequest {
            witness_kind: own_witness_kind.to_string(),
            witness: own_witness,
            // The per-session endpoint re-exchange slot (T13 step 4): the
            // fleet-only `device-endpoints` kind never reaches a non-fleet
            // peer, so a custody session's candidates travel here or nowhere.
            endpoints: own_endpoints,
            extra: Default::default(),
        })
        .context("encode admit request")?,
    )
    .context("admit request as Value")?;
    let reply = channel
        .request(KIND_PEER_SYNC_ADMIT, req)
        .await
        .context("the peer refused the admission exchange")?;
    let reply: PeerSyncAdmitReply =
        decode_strict(&encode_canonical(&reply).context("encode admit reply")?)
            .context("decode admit reply")?;
    let evaluated = evaluate_witness(
        &reply.witness_kind,
        &reply.witness,
        channel.peer_identity().as_bytes(),
        expected_account,
        now_secs,
    )
    .context("the peer's own witness did not verify")?;
    if let Some(grant_id) = &evaluated.custody_grant_id {
        match views.custody_revoked {
            None => bail!(
                "the peer presented a custody grant but this side has no \
                 custody-revocation view to evaluate it against"
            ),
            Some(revoked) if revoked(grant_id) => {
                bail!("the peer's custody grant is revoked")
            }
            Some(_) => {}
        }
    }
    // The device twin: a sibling that the fleet removed must not be walked
    // either — "sides admit independently" cuts both ways, and this side's
    // own merged device-set state is the only thing that can sever a
    // never-expiring fleet cert. No view (or no `Removed` row) admits; the
    // view's own doc owns that rule.
    if let Some(device_key) = &evaluated.device_key
        && let Some(removed) = views.device_removed
        && removed(expected_account, device_key)
    {
        bail!("the peer device is removed from this account's fleet");
    }
    Ok(AdmissionOutcome {
        verdict: evaluated.verdict,
        peer_endpoints: crate::discovery::bind_carried_endpoints(
            reply.endpoints,
            channel.peer_identity().as_bytes(),
        ),
    })
}

/// What one [`pull_missing_blocks`] pass did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PullReport {
    /// Blocks fetched and hydrated (content-address-verified on write).
    pub fetched: usize,
    /// Blocks the peer does not hold (or does not admit) — try another peer
    /// or the nest.
    pub missing: usize,
    /// Wanted blocks left to the nest path because the connection ran over a
    /// relay while the nest could carry them (`fauna_transport::bytes_may_ride`)
    /// — deferred, never failed: the nest path, or a later pass on a direct
    /// path, lands them.
    pub relay_deferred: usize,
}

/// How many CIDs one `blocks.pull` request names.
const WANT_LIST_CHUNK: usize = 32;

/// The want-list pull: fetch every indexed-but-absent block of `scope` from
/// the admitted peer, hydrating each through the store's content-address
/// check (a poisoned block fails there and is refused — wormability rule 4).
/// `deferred` blocks (held but over the peer's frame budget) are re-requested
/// in smaller chunks until the peer stops yielding.
///
/// Bytes ride direct paths only (`p2p.md` § The relay, ruling 4): the
/// channel's path is re-read before every `blocks.pull` request, and while
/// it is relayed and `nest` is [`NestPath::Reachable`] the rest of the
/// want-list is left to the nest path and counted in
/// [`PullReport::relay_deferred`].
pub async fn pull_missing_blocks<B: StoreBackend>(
    store: &AccountStore<B>,
    channel: &PeerChannel,
    scope: &str,
    nest: NestPath,
) -> Result<PullReport> {
    // The want-list: indexed records whose bytes this replica lacks.
    let mut wanted = Vec::new();
    let mut after: Option<ContentHash> = None;
    loop {
        let page = store.records_in_scope(scope, after.as_ref(), 256).await?;
        let Some(last) = page.last() else { break };
        after = Some(last.cid);
        for entry in &page {
            if !store.is_present(&entry.cid).await? {
                wanted.push(entry.cid);
            }
        }
    }

    let mut report = PullReport::default();
    while !wanted.is_empty() {
        let path = channel.path();
        if !bytes_may_ride(path, nest) {
            tracing::debug!(
                scope,
                path = path.label(),
                deferred = wanted.len(),
                "peer blocks pull: relayed connection while the nest path is reachable; \
                 the bytes are left to the nest path"
            );
            report.relay_deferred += wanted.len();
            break;
        }
        let chunk: Vec<ContentHash> = wanted.drain(..wanted.len().min(WANT_LIST_CHUNK)).collect();
        let req: Value = decode_strict(
            &encode_canonical(&PeerSyncBlocksPullRequest {
                cids: chunk
                    .iter()
                    .map(|c| serde_bytes::ByteBuf::from(c.as_bytes().to_vec()))
                    .collect(),
                extra: Default::default(),
            })
            .context("encode blocks.pull")?,
        )
        .context("blocks.pull as Value")?;
        let reply = channel
            .request(KIND_PEER_SYNC_BLOCKS_PULL, req)
            .await
            .context("peer blocks.pull")?;
        let reply: PeerSyncBlocksPullReply =
            decode_strict(&encode_canonical(&reply).context("encode pull reply")?)
                .context("decode pull reply")?;

        let served = reply.blocks.len();
        for block in reply.blocks {
            let arr: [u8; 36] = block
                .cid
                .as_slice()
                .try_into()
                .ok()
                .context("peer served a non-canonical CID")?;
            let cid = ContentHash::from_bytes(arr)?;
            // hydrate → put_block verifies the content address; a forged
            // block errors here and is never stored.
            store
                .hydrate(&cid, &block.bytes)
                .await
                .context("hydrate a peer-served block")?;
            report.fetched += 1;
        }
        report.missing += reply.missing.len();

        // Deferred = held but over the frame budget: re-request, in smaller
        // chunks. A peer that serves nothing while deferring everything makes
        // no progress — refuse the loop rather than spin.
        if !reply.deferred.is_empty() {
            if served == 0 {
                bail!(
                    "peer deferred {} block(s) while serving none — no forward progress",
                    reply.deferred.len()
                );
            }
            for cid in reply.deferred {
                let arr: [u8; 36] = cid
                    .as_slice()
                    .try_into()
                    .ok()
                    .context("peer deferred a non-canonical CID")?;
                wanted.push(ContentHash::from_bytes(arr)?);
            }
        }
    }
    Ok(report)
}
