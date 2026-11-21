//! The custody-ceremony conversations glue (W8.4 (account-data-plane.md § Workstreams)) — the two adapters that
//! close the loop between the carriage seam (`fauna_conversations`) and the
//! ceremony machine (`fauna_client_capabilities::custody_ceremony`):
//!
//! * [`StoreCustodyCeremonySink`] implements the session's
//!   [`CustodyCeremonySink`] receive seam over the machine's
//!   `ingest_payload` + the account store's custody seam
//!   (`fauna.state.custody-ceremony`) — every received offer/accept/deliver
//!   is verified and durably captured in ONE read-join before the poll moves
//!   on (record-then-act's "record").
//! * [`BackendCustodyPoster`] implements the machine's
//!   [`CustodyPayloadPoster`] post door over
//!   `FaunaMlsBackend::send_custody_payload` — how `drive_ceremonies`
//!   posts offers, accepts and witness deliveries.
//!
//! Wiring (an app or the tier_3 rig): register the sink via
//! `session.set_custody_ceremony_sink(...)`, then call `drive_ceremonies`
//! after conversation polls with this poster + the runtime handle (the
//! `CustodyRegistryWriter` impl on `AccountStoreHandle`) + the
//! `CapabilitiesClient` depositor. The `NestFolderCustodySink` /
//! `SchedulingSink` priority-#2 pattern: the conversations crate keeps no
//! custody semantics, this crate wires them once for all seven apps (web registers the same sink and poster on its rail).

use std::sync::Arc;

use async_trait::async_trait;
use fauna_client_capabilities::custody_ceremony::{
    CeremonyRecords, CustodyPayloadPoster, IngestOutcome, ReceiptOutcome, ingest_payload,
    ingest_receipt,
};
use fauna_client_config::{BackgroundCustodyWrites, CustodyCeremonyStore};
#[cfg(not(target_arch = "wasm32"))]
use fauna_conversations::ConversationsSession;
use fauna_conversations::ThreadSummary;
use fauna_conversations::backend::CustodyCeremonySink;
use fauna_conversations::backends::fauna_mls::{CustodyRedrive, FaunaMlsBackend};
use fauna_core::MaybeSendSync;
use fauna_core::data::Timestamp;
use fauna_core::identity::ActorId;

/// The ceremony-moved edge — fired by [`StoreCustodyCeremonySink`] whenever
/// an ingest actually advanced ceremony state (an offer newly pending, an
/// accept bound, a witness held), never on a duplicate. The narrowest
/// possible signal (no payload, no config handle), the
/// `FolderCustodyObserver` idiom: apps wire it to schedule a
/// `drive_ceremonies` pass; the tier_3 rig drives explicitly and registers
/// none. Called from the poll's async context — implementors notify, never
/// work inline.
pub trait CustodyCeremonyObserver: MaybeSendSync {
    fn ceremony_moved(&self, grant_id: &[u8]);

    /// The owed-drive retry edge: a drive pass left work owed and the receive
    /// cycle says its retry is due
    /// (`fauna_conversations::backends::fauna_mls::CustodyRedrive` owns when).
    /// Schedule one drive pass, exactly as for [`Self::ceremony_moved`] — but
    /// no state moved, so there is nothing to repaint; the pass's own marks
    /// repaint through the store-change notice when the retry lands.
    fn drive_owed(&self);
}

/// The store-backed [`CustodyCeremonySink`]: decode + verify + durably
/// capture one received ceremony payload into `fauna.state.custody-ceremony`
/// through the account store's custody seam.
pub struct StoreCustodyCeremonySink<S: CustodyCeremonyStore> {
    store: S,
    own_actor: ActorId,
    observer: Option<Arc<dyn CustodyCeremonyObserver>>,
}

impl<S: CustodyCeremonyStore> StoreCustodyCeremonySink<S> {
    /// `store` is the account's custody-ceremony seam — in an app the
    /// resolving seam over its account-store handle, read per payload
    /// because the store lands independently of the session; `own_actor`
    /// the reading account — the addressee bind for offers.
    pub fn new(store: S, own_actor: ActorId) -> Self {
        Self {
            store,
            own_actor,
            observer: None,
        }
    }

    /// Register the ceremony-moved observer. Optional — a sink without one
    /// only captures, and the app's next explicit drive picks the work up.
    pub fn with_observer(mut self, observer: Arc<dyn CustodyCeremonyObserver>) -> Self {
        self.observer = Some(observer);
        self
    }
}

/// What this process's [`StoreCustodyCeremonySink`]s did with each received
/// ceremony payload (offer / accept / deliver; receipts are not counted) —
/// the counts behind `fauna_e2e_agent::CUSTODY_CEREMONY_INGEST_KEY`, which
/// owns the contract. Every field only ever increases, from 0 at process start.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CustodyCeremonyIngestCounts {
    /// Payloads handed to a sink.
    pub seen: u64,
    /// Captured and advanced a ceremony (an offer newly pending, an accept
    /// bound, a witness held or refreshed).
    pub moved: u64,
    /// Captured as a re-delivery of what is already on record.
    pub duplicate: u64,
    /// Refused by verification — among them a deliver for a custody this
    /// account declined or removed.
    pub refused: u64,
    /// Not captured because the store door failed (not up, no generation tip).
    pub uncaptured: u64,
}

/// Process-wide, like the share-endpoints tally: every session's sink counts
/// into one place, so a relaunch-free reader needs no session handle.
static INGEST_COUNTS: [std::sync::atomic::AtomicU64; 5] =
    [const { std::sync::atomic::AtomicU64::new(0) }; 5];

fn count_ingest(slot: usize) {
    use std::sync::atomic::Ordering::Relaxed;
    INGEST_COUNTS[0].fetch_add(1, Relaxed);
    INGEST_COUNTS[slot].fetch_add(1, Relaxed);
}

/// Read [`CustodyCeremonyIngestCounts`] — five atomic loads, no lock or I/O,
/// so it is legal on an e2e state path (convention 11).
pub fn custody_ceremony_ingest_counts() -> CustodyCeremonyIngestCounts {
    use std::sync::atomic::Ordering::Relaxed;
    let read = |i: usize| INGEST_COUNTS[i].load(Relaxed);
    CustodyCeremonyIngestCounts {
        seen: read(0),
        moved: read(1),
        duplicate: read(2),
        refused: read(3),
        uncaptured: read(4),
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl<S: CustodyCeremonyStore> CustodyCeremonySink for StoreCustodyCeremonySink<S> {
    async fn custody_payload(&self, channel_hex: &str, sender: ActorId, bytes: &[u8]) -> bool {
        // Verify + capture in one read-join: `ingest_payload` mutates the
        // state only on its success paths, so a refused payload joins back
        // an unchanged state (the door puts nothing) and answers false. The
        // join keeps whatever another device captured meanwhile — the
        // upserts are idempotent per grant id by construction.
        let now = Timestamp::now();
        let own = self.own_actor;
        // The machinery's write, never a gesture: a background write, so an
        // open Devices or Nests page repaints the capture (the store-change
        // notice — `CustodyCeremonyStore::merge_custody_background`).
        let result = BackgroundCustodyWrites(&self.store)
            .update(|cfg| ingest_payload(cfg, &own, &sender, channel_hex, bytes, now))
            .await;
        match result {
            Ok(Ok(outcome)) => {
                count_ingest(match &outcome {
                    IngestOutcome::Duplicate => 2,
                    _ => 1,
                });
                if let Some(observer) = &self.observer {
                    match &outcome {
                        IngestOutcome::OfferPending { grant_id }
                        | IngestOutcome::AcceptBound { grant_id }
                        | IngestOutcome::WitnessHeld { grant_id } => {
                            observer.ceremony_moved(grant_id);
                        }
                        IngestOutcome::Duplicate => {}
                    }
                }
                true
            }
            Ok(Err(refused)) => {
                // A refused payload is a verification verdict, not a retry
                // candidate — `false` here is honest ("not captured") and the
                // debug line is the diagnosis (a duplicate grant id with
                // different bytes is the interesting one).
                count_ingest(3);
                tracing::debug!("custody ceremony payload refused: {refused}");
                false
            }
            Err(e) => {
                // The store door failed (not up yet, no generation tip) —
                // genuinely uncaptured; the per-launch re-walk and the
                // ceremony decay both heal.
                count_ingest(4);
                tracing::debug!("custody ceremony capture failed: {e}");
                false
            }
        }
    }

    /// The A7 receipt's owner-side leg (W8.7 leg 2): verify against the
    /// custodian key this account's own accept bound, and record — same one-join
    /// record-then-act shape as the ceremony above, with the registry-row write
    /// left to `drive_ceremonies` (this sink holds no writer, and the row is
    /// whole-record LWW, so one door is the correct number).
    ///
    /// `sender` is unused on purpose: the MLS-authenticated transport sender is
    /// the host *account*, while a receipt is signed by the custodian *device*
    /// the accept bound. Checking the account would be strictly weaker than the
    /// check `ingest_receipt` already makes, and would wrongly refuse the legal
    /// case of a host whose serving device differs from the device that ran the
    /// ceremony chat.
    async fn custody_receipt(&self, channel_hex: &str, _sender: ActorId, bytes: &[u8]) -> bool {
        let result = BackgroundCustodyWrites(&self.store)
            .update(|cfg| ingest_receipt(cfg, channel_hex, bytes))
            .await;
        match result {
            Ok(Ok(outcome)) => {
                if let (ReceiptOutcome::Recorded { grant_id }, Some(observer)) =
                    (&outcome, &self.observer)
                {
                    // Same edge as a ceremony move: the row write is now owed,
                    // and the observer is how an app schedules the drive.
                    observer.ceremony_moved(grant_id);
                }
                true
            }
            Ok(Err(refused)) => {
                // A verification verdict, not a retry candidate. This is the
                // line that names a lying or misconfigured custodian.
                tracing::debug!("custody receipt refused: {refused}");
                false
            }
            Err(e) => {
                tracing::debug!("custody receipt capture failed: {e}");
                false
            }
        }
    }

    fn drive_owed(&self) {
        if let Some(observer) = &self.observer {
            observer.drive_owed();
        }
    }
}

/// One place the owner could send a custody offer — a 1:1 conversation with
/// the account that would host. The ceremony rides an existing conversation
/// channel (`OfferParams.channel_hex`'s contract: creating the DM is the
/// shipped user act, not ceremony business), so the candidates ARE the 1:1
/// channels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustodyMintCandidate {
    /// The account the offer would address.
    pub host: ActorId,
    /// The conversation channel (hex) the ceremony would ride.
    pub channel_hex: String,
    /// The conversation's rendered label (the counterpart's name/handle as
    /// the thread list shows it), falling back to the channel hex when the
    /// thread has no summary yet.
    pub label: String,
}

impl CustodyMintCandidate {
    /// The shared boundary row (`custody-mint-host-select`'s option) — the one
    /// projection both faces (UniFFI and wasm) hand the page.
    pub fn view(&self) -> fauna_client_capabilities::custody_view::CustodyMintCandidateView {
        fauna_client_capabilities::custody_view::CustodyMintCandidateView {
            host: self.host.0.to_vec(),
            channel_hex: self.channel_hex.clone(),
            label: self.label.clone(),
        }
    }
}

/// Whether `custody-mint-button` can open the offer flow now
/// (`docs/goal/ui/README.md` § Offer only what is possible, rules 3–5): the
/// gate is at the entry, never at the confirm. `candidates` is
/// [`custody_mint_candidates`]' answer, or `None` before the conversations
/// it reads have loaded (not enabled on a guess). With no 1:1 conversation
/// the offer has nothing to travel over: blocked, the reason naming the step
/// (start a conversation first). Generic over the row, so an app holding the
/// boundary projection (`CustodyMintCandidateView`, what every UniFFI and wasm
/// face hands out) asks the same function as one holding the session's rows.
pub fn custody_mint_possibility<T>(
    candidates: Option<&[T]>,
) -> fauna_core::possibility::Possibility {
    use fauna_core::possibility::{Possibility, ReadState};
    match candidates {
        None => Possibility::from_read(ReadState::Pending),
        Some([]) => Possibility::blocked(fauna_core::localized::LocalizedText::key(
            "devices.custody_mint_no_contacts",
        )),
        Some(_) => Possibility::Possible,
    }
}

/// Whether the open offer flow's confirm (`custody-mint-confirm-button`) can
/// send: blocked, with its reason, until a host is picked.
pub fn custody_mint_confirm_possibility(host_picked: bool) -> fauna_core::possibility::Possibility {
    if host_picked {
        fauna_core::possibility::Possibility::Possible
    } else {
        fauna_core::possibility::Possibility::blocked(fauna_core::localized::LocalizedText::key(
            "devices.custody_mint_pick_host",
        ))
    }
}

/// Derive the owner's custody-offer host candidates from the live session:
/// every conversation channel whose MLS group has exactly two members — this
/// account and one counterpart. Deduped per counterpart (the most recently
/// listed thread wins is NOT promised; the first bound channel per host is
/// kept — offering twice to one account is a fresh grant id either way).
/// The native apps' entry; it reads the session's backend and thread list and
/// hands them to [`custody_mint_candidates_over`], which web calls directly.
#[cfg(not(target_arch = "wasm32"))]
pub fn custody_mint_candidates(
    session: &ConversationsSession,
    own: &ActorId,
) -> Vec<CustodyMintCandidate> {
    custody_mint_candidates_over(
        &session.backend(),
        &session.manager().snapshot().threads,
        own,
    )
}

/// [`custody_mint_candidates`] over the rail backend itself and the thread
/// list the manager renders — the shape both targets hold (web's conversations
/// rail has no `ConversationsSession`, only the backend and the manager).
/// Shared here so all seven apps derive the same list (priority #2).
pub fn custody_mint_candidates_over(
    backend: &FaunaMlsBackend,
    summaries: &[ThreadSummary],
    own: &ActorId,
) -> Vec<CustodyMintCandidate> {
    let engine = backend.engine();
    let channels = backend
        .conv_channels()
        .into_iter()
        .map(|channel| {
            let members = engine.group_members(&channel);
            let label = backend.thread_for_channel(&channel).and_then(|tid| {
                summaries
                    .iter()
                    .find(|t| t.thread_id == tid)
                    .map(|t| t.label.clone())
            });
            (channel.to_string(), members, label)
        })
        .collect();
    mint_candidates_from_channels(channels, own)
}

/// The **pure core** of [`custody_mint_candidates`] — every rule that decides
/// the list, with the session reads already done.
///
/// Split out because the rules are the part a leg can get wrong and the session
/// is the part that cannot be built in a unit test: the two-member filter (a
/// group thread is not a 1:1 channel), the self-exclusion, the per-host dedup
/// (offering twice to one account is a fresh grant id either way, so the first
/// bound channel per host wins), the label fallback to the channel hex for a
/// thread with no summary yet, and the sort by label.
///
/// Each entry is `(channel_hex, the channel's MLS members, the thread's
/// rendered label if it has one)`.
pub fn mint_candidates_from_channels(
    channels: Vec<(String, Vec<ActorId>, Option<String>)>,
    own: &ActorId,
) -> Vec<CustodyMintCandidate> {
    let mut seen_hosts: Vec<ActorId> = Vec::new();
    let mut out = Vec::new();
    for (channel_hex, members, label) in channels {
        if members.len() != 2 {
            continue;
        }
        let Some(host) = members.into_iter().find(|m| m != own) else {
            continue;
        };
        if seen_hosts.contains(&host) {
            continue;
        }
        let label = label
            .filter(|l| !l.is_empty())
            .unwrap_or_else(|| channel_hex.clone());
        seen_hosts.push(host);
        out.push(CustodyMintCandidate {
            host,
            channel_hex,
            label,
        });
    }
    out.sort_by(|a, b| a.label.cmp(&b.label));
    out
}

/// The machine's post door over the conversations rail's backend — the one
/// poster every app drives with: a native app passes its session's
/// `backend()`, web the backend its rail registered.
pub struct BackendCustodyPoster(pub Arc<FaunaMlsBackend>);

impl CustodyPayloadPoster for BackendCustodyPoster {
    async fn post(&self, channel_hex: &str, bytes: Vec<u8>) -> bool {
        match self.0.send_custody_payload(channel_hex, bytes).await {
            Ok(()) => true,
            // `warn`, not `debug` (e2e convention 6): a post that stays owed
            // waits for the next drive edge, and with nothing new arriving on
            // the channel that edge may never come — the reason is the one
            // line a stalled ceremony's reader has.
            Err(e) => {
                tracing::warn!("custody ceremony post owed: {e}");
                false
            }
        }
    }

    async fn post_receipt(&self, channel_hex: &str, bytes: Vec<u8>) -> bool {
        match self.0.send_custody_receipt(channel_hex, bytes).await {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!("custody receipt post owed: {e}");
                false
            }
        }
    }
}

/// The hosting-deposit door with the refusal logged — the bare client impl
/// answers only `false`, and a host whose row never lands needs the reason.
struct LoggedHostingDepositor<R: fauna_protocol::RpcRequester>(
    fauna_client_capabilities::custody_hosting::CustodyHostingClient<R>,
);

impl<R> fauna_client_capabilities::custody_ceremony::CustodyHostingDepositor
    for LoggedHostingDepositor<R>
where
    R: fauna_protocol::RpcRequester,
{
    async fn register(
        &self,
        deposit: &fauna_client_capabilities::custody_hosting::HostingDeposit,
    ) -> bool {
        match self.0.register(deposit).await {
            Ok(reply) if reply.ok => true,
            Ok(_) => {
                tracing::warn!("custody hosting register: the nest answered ok=false");
                false
            }
            Err(e) => {
                tracing::warn!("custody hosting register refused: {e}");
                false
            }
        }
    }
}

/// One custody-ceremony drive pass — the body every app's drive edge runs
/// (session start, store ready, a ceremony-moved edge, the Devices page's
/// load, a mint): fetch the receipts custodian nests deposited (stage (c)'s
/// owner-side leg), then `drive_ceremonies` with this backend's poster, the
/// account store as records + ledger + registry writer, and the nest as the
/// two deposit doors. Shared so the native spawn
/// (`fauna_client_custody::spawn_drive`) and web's `spawn_local` run one
/// composition (priority #2); the caller owns only the executor.
///
/// **A pass that leaves work owed is retried by the receive cycle.** The pass
/// reports its outcome to the rail's [`CustodyRedrive`]; the cycle asks it
/// once per sweep and, when a retry is due, fires
/// [`CustodyCeremonyObserver::drive_owed`] — the same executor edge as a
/// ceremony move. Without it a once-failed act waits for an ingest that never
/// comes when the counterpart is waiting on that very act
/// (`account-data-plane.md` § Implementation status today → *The owed-drive
/// retry edge*).
pub async fn drive_ceremony_pass<R, S>(
    nest: R,
    store: &S,
    owner: &fauna_core::identity::ActorKeypair,
    backend: Arc<FaunaMlsBackend>,
) where
    R: fauna_protocol::RpcRequester + Clone + MaybeSendSync,
    S: CustodyCeremonyStore
        + fauna_client_config::SuccessionLedgerStore
        + fauna_client_capabilities::custody_ceremony::CustodyRegistryWriter,
{
    let redrive = backend.custody_redrive();
    let poster = BackendCustodyPoster(backend);
    drive_ceremony_pass_over(nest, store, store, store, owner, &poster, &redrive).await;
}

/// [`drive_ceremony_pass`] over its doors and the retry state themselves — the
/// whole pass, split from the backend and the one account-store handle only so
/// a tier_1 test can drive it with a poster that fails and in-memory doors.
async fn drive_ceremony_pass_over<R, S, W, P>(
    nest: R,
    store: &S,
    ledger: &dyn fauna_client_config::SuccessionLedgerStore,
    writer: &W,
    owner: &fauna_core::identity::ActorKeypair,
    poster: &P,
    redrive: &CustodyRedrive,
) where
    R: fauna_protocol::RpcRequester + Clone + MaybeSendSync,
    S: CustodyCeremonyStore,
    W: fauna_client_capabilities::custody_ceremony::CustodyRegistryWriter,
    P: CustodyPayloadPoster,
{
    // The pass's record writes (a nest-door receipt ingested; the posted,
    // minted and delivered marks) are the machinery's background writes, so
    // an open Devices or Nests page repaints them through the store-change
    // notice; the ledger and registry doors are their own.
    let records = BackgroundCustodyWrites(store);
    for skipped in
        fauna_client_capabilities::custody_ceremony::ingest_nest_receipts(nest.clone(), &records)
            .await
    {
        tracing::debug!("{skipped}");
    }
    let depositor = fauna_client_capabilities::rpc::CapabilitiesClient::new(nest.clone());
    let hosting = LoggedHostingDepositor(
        fauna_client_capabilities::custody_hosting::CustodyHostingClient::new(nest),
    );
    let report = fauna_client_capabilities::custody_ceremony::drive_ceremonies(
        &records,
        ledger,
        owner,
        poster,
        writer,
        &depositor,
        &hosting,
        Timestamp::now(),
    )
    .await;
    // Owed work, or a pass that could not read its records at all, is retried
    // from the receive cycle; a settled pass clears the retry.
    redrive.note(!matches!(&report, Ok(r) if r.still_owed == 0));
    match report {
        Ok(r) if r.posted + r.minted + r.rows_written + r.still_owed > 0 => {
            tracing::info!(
                posted = r.posted,
                minted = r.minted,
                rows_written = r.rows_written,
                still_owed = r.still_owed,
                "custody ceremony drive"
            );
        }
        Ok(_) => {}
        Err(e) => tracing::warn!("custody ceremony drive aborted: {e}"),
    }
}

#[cfg(test)]
mod owed_redrive_tests {
    use super::*;
    use fauna_client_capabilities::custody_ceremony::{
        CustodyRegistryWriter, OfferParams, begin_offer, build_accept,
    };
    use fauna_client_config::test_helpers::{FakeCustodyCeremonyStore, FakeSuccessionLedgerStore};
    use fauna_client_testkit::{FailingRequester, block_on};
    use fauna_core::custody_ceremony::{CustodyConfig, DEFAULT_RETAINED_BYTES_CAP};
    use fauna_core::custody_grant::CustodyScopeSet;
    use fauna_core::device_endpoints::DeviceEndpoints;
    use fauna_core::identity::ActorKeypair;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};

    /// A post door that refuses while `down`.
    #[derive(Default)]
    struct FlakyPoster {
        down: AtomicBool,
        posted: Mutex<Vec<Vec<u8>>>,
    }

    impl CustodyPayloadPoster for FlakyPoster {
        async fn post(&self, _channel_hex: &str, bytes: Vec<u8>) -> bool {
            if self.down.load(Relaxed) {
                return false;
            }
            self.posted.lock().unwrap().push(bytes);
            true
        }

        async fn post_receipt(&self, _channel_hex: &str, _bytes: Vec<u8>) -> bool {
            !self.down.load(Relaxed)
        }
    }

    /// A registry door nothing in this test reaches (no witness is held).
    struct NoRows;

    impl CustodyRegistryWriter for NoRows {
        async fn put_custodian_endpoints(
            &self,
            _value: &fauna_core::custodian_endpoints::CustodianEndpoints,
        ) -> bool {
            false
        }

        async fn put_custodies_held(
            &self,
            _value: &fauna_core::custodies_held::CustodyHeld,
        ) -> bool {
            false
        }

        async fn removed_device_ids(&self) -> Vec<[u8; 32]> {
            Vec::new()
        }
    }

    /// Counts each edge an app would answer with one drive pass.
    #[derive(Default)]
    struct CountingObserver {
        moved: AtomicUsize,
        owed: AtomicUsize,
    }

    impl CustodyCeremonyObserver for CountingObserver {
        fn ceremony_moved(&self, _grant_id: &[u8]) {
            self.moved.fetch_add(1, Relaxed);
        }

        fn drive_owed(&self) {
            self.owed.fetch_add(1, Relaxed);
        }
    }

    /// The stall this edge exists for: the host's accept is recorded, the one
    /// drive pass its gesture ran fails to post it, and the owner — waiting on
    /// that very accept — sends nothing more. The receive cycle alone must
    /// retry the pass, and the accept goes out with no new ingest.
    #[test]
    fn a_once_failed_accept_post_is_retried_by_the_receive_cycle_alone() {
        let (owner, host) = (ActorKeypair::generate(), ActorKeypair::generate());
        let channel_hex = "aa".repeat(32);
        let grant_id = [0x1D; 16];
        let now = Timestamp::now();
        let offer = begin_offer(
            &mut CustodyConfig::default(),
            &owner,
            OfferParams {
                host: host.actor_id(),
                channel_hex: channel_hex.clone(),
                scopes: CustodyScopeSet::Account,
                duration_secs: fauna_client_capabilities::DEFAULT_GRANT_WINDOW_SECS,
                owner_devices: vec![DeviceEndpoints {
                    node_id: [7u8; 32],
                    ..Default::default()
                }],
                owner_nest_url: Some("https://nest.example/".into()),
                grant_id,
            },
            now,
        )
        .expect("the owner's offer");

        // The host's side: the production sink over an in-memory store, its
        // observer counting the edges.
        let store = FakeCustodyCeremonyStore::empty();
        let ledger = FakeSuccessionLedgerStore::empty(host.actor_id());
        let observer = Arc::new(CountingObserver::default());
        let sink = StoreCustodyCeremonySink::new(store.clone(), host.actor_id())
            .with_observer(observer.clone());
        assert!(block_on(sink.custody_payload(
            &channel_hex,
            owner.actor_id(),
            &offer
        )));
        assert_eq!(observer.moved.load(Relaxed), 1, "the offer's ingest");

        // The consent gesture records the accept; its one drive pass meets a
        // post door that refuses.
        store
            .mutate(|cfg| {
                build_accept(
                    cfg,
                    &host,
                    &grant_id,
                    [0xC5; 32],
                    DeviceEndpoints {
                        node_id: [0xC5; 32],
                        public_addrs: vec!["198.51.100.7:4433".into()],
                        ..Default::default()
                    },
                    DEFAULT_RETAINED_BYTES_CAP,
                    None,
                    now,
                )
            })
            .expect("the host's accept");
        let nest = FailingRequester::new("offline");
        let poster = FlakyPoster::default();
        poster.down.store(true, Relaxed);
        let redrive = CustodyRedrive::default();
        let pass = || {
            block_on(drive_ceremony_pass_over(
                &nest, &store, &ledger, &NoRows, &host, &poster, &redrive,
            ))
        };
        pass();
        assert!(!store.current().held[0].accept_posted, "the post failed");

        // One receive cycle, as `FaunaMlsBackend::redrive_owed_custody` runs it
        // after every sweep (pinned over the real sweep in fauna-conversations'
        // `a_receive_cycle_retries_an_owed_custody_drive`).
        let cycle = || {
            if redrive.due() {
                fauna_conversations::backend::CustodyCeremonySink::drive_owed(&sink);
            }
        };
        poster.down.store(false, Relaxed);
        cycle();
        assert_eq!(
            observer.owed.load(Relaxed),
            1,
            "the cycle alone asks for the retry"
        );
        // The app answers the edge with one pass.
        pass();
        assert!(store.current().held[0].accept_posted, "the retry posted");
        assert_eq!(poster.posted.lock().unwrap().len(), 1, "exactly once");
        assert_eq!(observer.moved.load(Relaxed), 1, "and no new ingest came");

        // Settled: no further cycle asks again.
        for _ in 0..64 {
            cycle();
        }
        assert_eq!(observer.owed.load(Relaxed), 1);
    }

    /// A pass that keeps failing is retried at a widening spacing, never every
    /// cycle: 1, 3, 7, then 15 cycles between retries.
    #[test]
    fn a_post_that_stays_refused_is_retried_at_a_widening_spacing() {
        let redrive = CustodyRedrive::default();
        assert!(!redrive.due(), "nothing owed, nothing due");
        redrive.note(true);
        let mut gaps = Vec::new();
        for _ in 0..6 {
            let mut waited = 0;
            while !redrive.due() {
                waited += 1;
            }
            gaps.push(waited);
            redrive.note(true); // the retried pass failed again
        }
        assert_eq!(gaps, vec![0, 1, 3, 7, 15, 15]);
        redrive.note(false);
        assert!(!redrive.due(), "a settled pass clears the retry");
    }
}

#[cfg(test)]
mod mint_candidate_tests {
    use super::*;

    fn actor(b: u8) -> ActorId {
        ActorId([b; 32])
    }

    /// A candidate is a 1:1 channel: the counterpart is the member that is not
    /// this account, and the thread's label rides along.
    #[test]
    fn a_two_member_channel_yields_its_counterpart() {
        let own = actor(1);
        let got = mint_candidates_from_channels(
            vec![(
                "aa".to_string(),
                vec![own, actor(2)],
                Some("Bo".to_string()),
            )],
            &own,
        );
        assert_eq!(
            got,
            vec![CustodyMintCandidate {
                host: actor(2),
                channel_hex: "aa".to_string(),
                label: "Bo".to_string(),
            }]
        );
    }

    /// A group thread is not an offer channel — the ceremony rides a 1:1.
    /// Anything but exactly two members is skipped, in both directions.
    #[test]
    fn channels_that_are_not_one_to_one_are_skipped() {
        let own = actor(1);
        let got = mint_candidates_from_channels(
            vec![
                ("group".to_string(), vec![own, actor(2), actor(3)], None),
                ("solo".to_string(), vec![own], None),
            ],
            &own,
        );
        assert!(got.is_empty(), "got {got:?}");
    }

    /// A two-member channel that does not contain this account has no
    /// counterpart to offer to — it must not yield `own` back as the host.
    #[test]
    fn a_channel_without_this_account_yields_nothing() {
        let own = actor(1);
        let got = mint_candidates_from_channels(
            vec![("x".to_string(), vec![actor(2), actor(3)], None)],
            &own,
        );
        assert_eq!(got.len(), 1, "two other members is still a 1:1 for them");
        assert_ne!(got[0].host, own, "never offer custody to yourself");
    }

    /// Two channels with the same counterpart collapse to one candidate —
    /// the FIRST bound channel wins, whatever the labels sort to.
    #[test]
    fn one_candidate_per_host_and_the_first_channel_wins() {
        let own = actor(1);
        let got = mint_candidates_from_channels(
            vec![
                (
                    "first".to_string(),
                    vec![own, actor(2)],
                    Some("zed".to_string()),
                ),
                (
                    "second".to_string(),
                    vec![own, actor(2)],
                    Some("abe".to_string()),
                ),
            ],
            &own,
        );
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].channel_hex, "first");
        assert_eq!(got[0].label, "zed");
    }

    /// A thread with no summary yet — or an empty one — falls back to the
    /// channel hex rather than rendering a blank option the user cannot tell
    /// apart from another blank one.
    #[test]
    fn a_label_less_thread_falls_back_to_the_channel_hex() {
        let own = actor(1);
        let got = mint_candidates_from_channels(
            vec![
                ("cafe".to_string(), vec![own, actor(2)], None),
                ("beef".to_string(), vec![own, actor(3)], Some(String::new())),
            ],
            &own,
        );
        let labels: Vec<&str> = got.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, vec!["beef", "cafe"]);
    }

    /// The list is sorted by label, so the picker's options do not reshuffle
    /// between opens as channel order changes.
    #[test]
    fn candidates_are_sorted_by_label() {
        let own = actor(1);
        let got = mint_candidates_from_channels(
            vec![
                (
                    "c1".to_string(),
                    vec![own, actor(2)],
                    Some("Zoe".to_string()),
                ),
                (
                    "c2".to_string(),
                    vec![own, actor(3)],
                    Some("Ada".to_string()),
                ),
                (
                    "c3".to_string(),
                    vec![own, actor(4)],
                    Some("Mel".to_string()),
                ),
            ],
            &own,
        );
        let labels: Vec<&str> = got.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, vec!["Ada", "Mel", "Zoe"]);
    }
}

#[cfg(test)]
mod mint_possibility_tests {
    use super::*;
    use fauna_core::localized::LocalizedText;
    use fauna_core::possibility::Possibility;

    /// The mint is gated at its entry: not enabled before the conversations
    /// load, blocked with the start-a-conversation step with no 1:1
    /// conversation, possible with one; the confirm waits for a host.
    #[test]
    fn the_mint_is_gated_at_the_entry_and_the_confirm_waits_for_a_host() {
        assert_eq!(
            custody_mint_possibility::<CustodyMintCandidate>(None),
            Possibility::blocked(LocalizedText::key("common.still_loading"))
        );
        assert_eq!(
            custody_mint_possibility::<CustodyMintCandidate>(Some(&[])),
            Possibility::blocked(LocalizedText::key("devices.custody_mint_no_contacts"))
        );
        let one = CustodyMintCandidate {
            host: ActorId([1u8; 32]),
            channel_hex: "ab".into(),
            label: "alice".into(),
        };
        assert_eq!(
            custody_mint_possibility(Some(&[one])),
            Possibility::Possible
        );
        assert_eq!(
            custody_mint_confirm_possibility(false),
            Possibility::blocked(LocalizedText::key("devices.custody_mint_pick_host"))
        );
        assert_eq!(
            custody_mint_confirm_possibility(true),
            Possibility::Possible
        );
    }
}
