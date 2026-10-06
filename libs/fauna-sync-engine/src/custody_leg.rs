//! The custodian's schedule-free pull (W8.5 (account-data-plane.md § Workstreams) P3 —
//! `docs/goal/architecture/account-data-plane.md` § Replica posture: "What a
//! custodian holds: per-writer journal rows verbatim, sealed canonical
//! forms, tombstones, frontiers" — and what it cannot do: "read, merge,
//! author").
//!
//! [`custody_pull`] is the account-state walk's relay half
//! (`account_state_plane.rs`'s unconditional `record_relay_row`) **without
//! the reading walk above it**: page `fauna.sync.changes.list` for one
//! admitted scope, coordinate-validate each row, keep the verbatim wire row
//! in the relay plane, advance a durable pull cursor. No key schedule, no
//! trial-open, no merge, no projections — structurally keyless (R6 (account-data-plane.md § The ratified decisions):
//! receivers account the walk as if direct, because the rows are verbatim).
//!
//! # The cursor is META, deliberately not the frontier table
//!
//! The store's frontier accounting law ("never advance past a row the store
//! does not hold") counts **journal** rows, and a custodian never journals —
//! its held-set IS the relay plane. So the pull keeps its own per-scope
//! cursor in the store's meta table (the `peer_leg` brake-cache precedent),
//! persisted per page so a crashed pull resumes where it stopped. The serve
//! side never consults this cursor — it answers a dialer's OWN frontier
//! straight from the relay plane (`AccountStore::relay_rows`), which is what
//! makes "owner device B pulls from custodian C" converge on B's ordinary
//! accounting.
//!
//! The **nest arm** pages by the owner's nest's serve order on top of it
//! (`account-sync-plane.md` § Feeds and cursors → *Compaction is a serve-order
//! watermark*): it banks the nest's echo under the custodied store's own
//! watermark key (`AccountStore::nest_watermark`, never inside this cursor)
//! and, once one is banked, names no writer in its requests. It still raises
//! this shared cursor with every row it records, keeping it complete — the
//! owner-device arm is a store-served leg with no watermark, and pages with
//! the cursor whole.

use anyhow::{Context, Result};
use fauna_account_store::backend::StoreBackend;
use fauna_account_store::store::AccountStore;
use fauna_account_store::types::RelayRow;
// Runtime-gated, not deleted: `open_custodied` names it under
// `account-runtime`, and the test module has its own `use`. Dropping the
// import outright silences the default-build warning by breaking both other
// builds.
#[cfg(feature = "account-runtime")]
use fauna_account_store::types::WriterId;
use fauna_protocol::RpcRequester;
use fauna_protocol::account_state::ItemClass;
use fauna_protocol::sync::{SyncChange, SyncChangesListReply, SyncChangesListRequest};
use std::collections::BTreeMap;

// The `since` slot is unused by the class-2 arm (the frontier is the cursor) —
// the walk's own constant, not a second copy of it.
use crate::account_state_plane::{NEST_SLOT_UNUSED, item_key_of, row_coordinates};
use crate::page_walk::{FrontierCursor, PageCursor, WatermarkCursor};

/// Meta key for one scope's durable pull cursor.
fn cursor_key(scope: &str) -> String {
    format!("custody_pull/frontier/{scope}")
}

/// What one custody pull did for one scope.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CustodyPullReport {
    /// Pages fetched (empty final page excluded).
    pub pages: usize,
    /// Rows kept verbatim in the relay plane this pull.
    pub recorded: usize,
}

impl crate::page_walk::PageTally for CustodyPullReport {
    fn page_fetched(&mut self) {
        self.pages += 1;
    }
}

/// Pull one admitted scope's state-entry rows from an admitted channel's
/// requester into the custodied store's relay plane, verbatim. Resumes from
/// the durable meta cursor; idempotent (a re-served row re-puts the same
/// verbatim bytes).
///
/// `of_owner` picks the leg, and with it the cursor. `None` is the
/// **owner-device arm**, a store-served leg where the serve-order watermark is
/// deferred: it pages with the shared pull cursor, whole, as ever. `Some` is
/// the **nest arm**, which pages by the owner's nest's serve
/// order (charter § Feeds and cursors → *Compaction is a serve-order
/// watermark*): it sends the watermark banked under this custodied store's own
/// key for the scope ([`AccountStore::nest_watermark`]) and, once one is
/// banked, names no writer at all — while still raising the shared cursor with
/// every row it records, so the owner-device arm never re-pulls what the nest
/// arm already holds.
pub async fn custody_pull<B, R>(
    store: &AccountStore<B>,
    rpc: &R,
    scope: &str,
    of_owner: Option<&str>,
) -> Result<CustodyPullReport>
where
    B: StoreBackend,
    R: RpcRequester,
{
    let shared = stored_cursor(store, scope).await?;
    let Some(of_owner) = of_owner else {
        return pull_pages(
            store,
            rpc,
            scope,
            None,
            shared,
            async |cursor: &BTreeMap<String, i64>| {
                persist_shared_cursor(store, scope, cursor).await
            },
        )
        .await;
    };
    // Nobody is pinned: a keyless custodian holds exactly what its relay
    // plane holds, and the pull already moves its cursor past a row it cannot
    // hold (no entry bytes), so it never leaves a row seen-but-unaccounted the
    // way a reading walk's unopened row is.
    let held = store
        .nest_watermark(scope)
        .await?
        .map(i64::try_from)
        .transpose()
        .context("custody pull: the stored watermark exceeds i64")?;
    let banked_replica = store.nest_watermark_replica(scope).await?;
    let cursor = NestArmCursor {
        paging: WatermarkCursor::open(held, banked_replica, shared.clone(), BTreeMap::new()),
        shared,
    };
    pull_pages(
        store,
        rpc,
        scope,
        Some(of_owner),
        cursor,
        async |cursor: &NestArmCursor| {
            persist_shared_cursor(store, scope, &cursor.shared).await?;
            if let Some(held) = cursor.paging.held_through_seq() {
                let held = u64::try_from(held).context("custody pull: negative watermark")?;
                store
                    .raise_nest_watermark(scope, cursor.paging.replica_id(), held)
                    .await?;
            } else if cursor.paging.watermark_dishonoured() {
                // The nest that banked this arm's watermark stopped honouring
                // it (rolled back to a pre-watermark image) — clear the
                // persisted bank so the next pull reopens with the whole
                // shared cursor instead of narrow (nobody named) against the
                // same non-honouring nest.
                store.clear_nest_watermark(scope).await?;
            }
            Ok(())
        },
    )
    .await
}

/// The page loop both arms share: coordinate-validate each row, keep it
/// verbatim in the relay plane, checkpoint per page.
async fn pull_pages<B, R, C>(
    store: &AccountStore<B>,
    rpc: &R,
    scope: &str,
    of_owner: Option<&str>,
    cursor: C,
    checkpoint: impl AsyncFnMut(&C) -> Result<()>,
) -> Result<CustodyPullReport>
where
    B: StoreBackend,
    R: RpcRequester,
    C: FrontierCursor,
{
    // The walk's page loop, now literally rather than "mirrored from" it —
    // spin refusal included (`crate::page_walk`). The one thing this pull adds
    // to it is the per-page checkpoint.
    crate::page_walk::drive_checkpointed(
        cursor,
        CustodyPullReport::default(),
        &format!("custody pull ({scope})"),
        async |cursor: &C| {
            let reply: SyncChangesListReply = rpc
                .request(
                    "fauna.sync.changes.list",
                    SyncChangesListRequest {
                        since: NEST_SLOT_UNUSED,
                        item_class: Some(ItemClass::StateEntry.as_wire().to_string()),
                        scope: Some(scope.to_string()),
                        frontier: Some(cursor.frontier().clone()),
                        held_through_seq: cursor.held_through_seq(),
                        // The nest leg addresses the custodied owner explicitly
                        // (W8.6 pin N3); the peer leg routes by the connection
                        // verdict and passes None.
                        of_owner: of_owner.map(str::to_string),
                        ..Default::default()
                    },
                )
                .await
                .map_err(|e| anyhow::anyhow!("custody pull changes.list ({scope}): {e}"))?;
            Ok(reply)
        },
        async |change: &SyncChange, report: &mut CustodyPullReport, cursor: &mut C| {
            let (writer, origin_seq) = row_coordinates(change)?;
            cursor.saw(writer.to_hex(), origin_seq as i64);
            let Some(envelope) = change.entry.as_ref() else {
                // A row with no entry bytes cannot be relayed verbatim; the
                // cursor moved (seen), so the pull does not spin on it —
                // and a reconcile-from-zero can re-present it later.
                return Ok(());
            };
            let item_key = item_key_of(change)?;
            store
                .record_relay_row(&RelayRow {
                    scope: scope.to_string(),
                    item_class: ItemClass::StateEntry.as_wire().to_string(),
                    writer,
                    writer_seq: origin_seq,
                    item_key: item_key.to_vec(),
                    op: change.change_type.clone(),
                    entry: Some(envelope.to_vec()),
                    feed_seq: u64::try_from(change.seq).ok(),
                })
                .await
                .context("custody pull: relay plane")?;
            report.recorded += 1;
            Ok(())
        },
        // Durable per page — a crashed pull resumes here, and rows already
        // recorded re-put idempotently if the cursor write raced the crash.
        // The driver runs this only past the spin refusal, so a page that
        // moved nothing is never checkpointed as progress.
        checkpoint,
    )
    .await
}

async fn persist_shared_cursor<B: StoreBackend>(
    store: &AccountStore<B>,
    scope: &str,
    cursor: &BTreeMap<String, i64>,
) -> Result<()> {
    store
        .backend()
        .meta_put(
            &cursor_key(scope),
            &fauna_core::encoding::canonical_encode(cursor)?,
        )
        .await
        .context("custody pull: cursor persist")
}

/// The nest arm's paging cursor: the watermark pair its requests carry, plus
/// the shared pull cursor it keeps complete for the owner-device arm.
#[derive(Clone)]
struct NestArmCursor {
    paging: WatermarkCursor,
    /// `custody_pull/frontier/{scope}` — every writer this store has recorded,
    /// raised exactly as the owner-device arm raises it. Never sent by this
    /// arm, so neither progress nor the ceiling is judged on it here.
    shared: BTreeMap<String, i64>,
}

impl PageCursor for NestArmCursor {
    fn advanced_from(&self, before: &Self) -> bool {
        self.paging.advanced_from(&before.paging)
    }

    fn stall_detail(&self, rows: usize) -> String {
        self.paging.stall_detail(rows)
    }

    fn overgrown(&self) -> Option<String> {
        self.paging.overgrown()
    }

    fn absorb_reply(&mut self, reply: &SyncChangesListReply) {
        self.paging.absorb_reply(reply);
    }

    fn voided_since(&self, before: &Self) -> bool {
        self.paging.voided_since(&before.paging)
    }
}

impl FrontierCursor for NestArmCursor {
    fn saw(&mut self, writer_hex: String, seq: i64) {
        self.shared.saw(writer_hex.clone(), seq);
        self.paging.saw(writer_hex, seq);
    }

    fn frontier(&self) -> &BTreeMap<String, i64> {
        self.paging.frontier()
    }

    fn held_through_seq(&self) -> Option<i64> {
        self.paging.held_through_seq()
    }

    fn watermark_dishonoured(&self) -> bool {
        self.paging.watermark_dishonoured()
    }

    fn replica_id(&self) -> Option<&[u8]> {
        self.paging.replica_id()
    }
}

async fn stored_cursor<B: StoreBackend>(
    store: &AccountStore<B>,
    scope: &str,
) -> Result<BTreeMap<String, i64>> {
    match store.backend().meta_get(&cursor_key(scope)).await? {
        Some(bytes) => fauna_core::encoding::canonical_decode(&bytes)
            .context("custody pull: cursor decode (delete the meta row to re-pull from zero)"),
        None => Ok(BTreeMap::new()),
    }
}

// ── The custodian runtime leg (W8.5 P4/P5) ───────────────────────────────────
//
// Feature-gated with the runtime that drives it: the peer-sync / transport /
// config / capabilities crates ride the `account-runtime` feature table.

/// The custody passes' report shapes — the driver's vocabulary
/// (`fauna_account_plane::host_legs`), re-exported at their old paths.
pub use fauna_account_plane::host_legs::{
    CustodyBudgetOutcome, CustodyDialPass, CustodyNestPass, CustodyPassOutcome, CustodyServePass,
};
#[cfg(feature = "account-runtime")]
use fauna_account_store::root::StoreRoot;
#[cfg(feature = "account-runtime")]
use fauna_account_store::sqlite::SqliteBackend;
#[cfg(feature = "account-runtime")]
#[cfg(feature = "account-runtime")]
use fauna_core::custodies_held::CustodyHeld;
use fauna_core::custody_grant::CustodyGrant;
#[cfg(feature = "account-runtime")]
use fauna_core::device_endpoints::DeviceEndpoints;
#[cfg(feature = "account-runtime")]
use fauna_core::identity::ActorId;
#[cfg(feature = "account-runtime")]
use fauna_peer_sync::server::ServeStoreHandle;
use fauna_protocol::account_state::{ACCOUNT_STATE_FLEET_SCOPE, ACCOUNT_STATE_SCOPE};
#[cfg(feature = "account-runtime")]
use fauna_protocol::merge_policy::KIND_CUSTODIES_HELD;
#[cfg(feature = "account-runtime")]
use std::collections::{HashMap, HashSet};
#[cfg(feature = "account-runtime")]
use std::sync::Arc;

#[cfg(feature = "account-runtime")]
/// One custodied account's two store connections: the pull side (this
/// worker's, written by [`custody_pull`]) and the serve side (its own
/// thread + connection — the own-store serve shape, P4).
struct CustodiedStores {
    pull: AccountStore<SqliteBackend>,
    serve: ServeStoreHandle,
    /// The held row's data, as of the last refresh — the dial pass's
    /// witness + targets source.
    row: CustodyHeld,
    /// The nest leg's session to the OWNER's nest, kept across passes (W8.6).
    /// `None` until the first pass builds it, and dropped again on any error
    /// so the next pass re-handshakes — which is how a revoked custody stops:
    /// the door refuses the live session's next request, and the fresh mint
    /// then refuses too. This bearer carries no revocation view of its own by
    /// design (charter: the verdict re-derives per dispatch).
    nest: Option<NestLegSession>,
    /// The T15 ingest brake — see [`IngestBrake`].
    brake: IngestBrake,
}

/// The T15 ingest brake: the cap gains an INGEST side.
/// Per custody, in-memory (a restart re-meters on its first pass — the
/// honest reset), written only by the budget arm and read by both pull legs:
///
/// - **At floor** — the last budget pass reported unreclaimable overage
///   (over cap and unable to evict back under it). Pulling more while at the
///   floor can only deepen the overage, so both pull halves skip the custody
///   until a pass reports otherwise. The budget pass itself ALWAYS runs
///   (`dial_pass` meters every held custody unconditionally), which is
///   exactly how the brake releases: cap raised or eviction possible →
///   `at_floor` clears → the next pass pulls again.
/// - **Metering failure** — the one enforcement point used to fail OPEN
///   (warn + keep holding + keep ingesting), and its likeliest cause — a
///   full disk or a wedged store — is exactly the condition the cap exists
///   for. One failure is weather (still open: hold, serve, retry next pass);
///   [`METERING_FAILURES_BRAKE`] consecutive failures close the ingest side
///   until a pass meters successfully. Holding and SERVING are never braked
///   — the brake bounds accumulation, not availability.
///
/// Atomics so the `&self` dial pass can record outcomes without widening its
/// signature; the passes are sequential, the atomics are just interior
/// mutability.
#[derive(Default)]
#[cfg(feature = "account-runtime")]
struct IngestBrake {
    at_floor: std::sync::atomic::AtomicBool,
    metering_failures: std::sync::atomic::AtomicU32,
}

/// Consecutive budget-pass failures after which a custody's ingest stops
/// until a pass meters successfully. A Rust constant — never a knob.
#[cfg(feature = "account-runtime")]
const METERING_FAILURES_BRAKE: u32 = 3;

/// A generous per-owner-device ceiling for the whole dial → admit → pull
/// interaction, and the custody twin of `peer_leg`'s `PER_SIBLING_BUDGET`
/// (same duration, same reason, same shape) — the sibling dial has had one
/// since it was written; this pass ran without one.
///
/// It matters more here than there. [`CustodyLegState::dial_pass`] walks
/// `self.held` **sequentially** and meters each custody's T15 byte budget only
/// after that custody's dial returns, so one owner device that answers slowly
/// forever — or feeds `custody_pull` a page loop that always advances by the
/// minimum — used to hold up every *other* account this custodian holds, and
/// their eviction with it. `custody_pull`'s own page budget
/// (`crate::page_walk`'s step 7) guarantees the loop terminates; only a clock
/// can say *when*, and this is the caller that owns one — the driver is
/// deliberately runtime-agnostic (no `Send` bound, no timer named), and
/// `PeerChannel::request` puts the deadline on the caller by contract.
#[cfg(feature = "account-runtime")]
const PER_CUSTODY_OWNER_BUDGET: std::time::Duration = std::time::Duration::from_secs(120);

#[cfg(feature = "account-runtime")]
impl IngestBrake {
    /// May the pull halves ingest for this custody this pass?
    fn admits_ingest(&self) -> bool {
        use std::sync::atomic::Ordering;
        !self.at_floor.load(Ordering::Relaxed)
            && self.metering_failures.load(Ordering::Relaxed) < METERING_FAILURES_BRAKE
    }

    /// The budget arm metered successfully; `at_floor` is what it reported.
    fn record_metered(&self, at_floor: bool) {
        use std::sync::atomic::Ordering;
        self.at_floor.store(at_floor, Ordering::Relaxed);
        self.metering_failures.store(0, Ordering::Relaxed);
    }

    /// The budget arm failed to meter at all.
    fn record_metering_failure(&self) {
        use std::sync::atomic::Ordering;
        self.metering_failures.fetch_add(1, Ordering::Relaxed);
    }
}

/// One live custody session to an owner's nest, plus the URL it was built
/// for — a ceremony can move the anchor, and a session pointed at the old
/// URL must not outlive the row that named it.
///
/// Ungated (not `account-runtime`): the custodian-NEST runtime
/// (`bins/fauna-nest`'s custody-hosting pump, item 6 stage (b)) drives the
/// same pull core over the same session shape, and its deps (`fauna-client`,
/// `nest_client`) are unconditional in this crate.
pub struct NestLegSession {
    pub(crate) url: String,
    pub(crate) client: std::sync::Arc<fauna_client::NestClient>,
    /// The **bulk** half of the same session: the byte plane the
    /// segment pair is fetched over, built from the control plane's own
    /// `AuthClient` so both ride one custody bearer.
    ///
    /// Held here rather than rebuilt per pass so the two share a lifecycle
    /// exactly — the error path below drops the whole session, and a byte
    /// plane outliving the control plane it was minted from would keep a
    /// revoked custody's bearer usable for one pass longer than the row.
    pub(crate) bytes: crate::nest_client::SyncClient,
}

impl NestLegSession {
    /// Build and connect one custody session to an owner's nest. The custody
    /// handshake bearer mints ON CONNECT, so a revoked or dead row fails
    /// here — the same place the tier_3 door proof puts it — and the bulk
    /// plane is built from the control plane's own `AuthClient` so both ride
    /// one custody bearer and neither can outlive the other.
    ///
    /// The caller owns the dial-policy check — this
    /// constructor assumes `url` was already validated this pass.
    pub async fn connect(
        url: &str,
        owner: [u8; 32],
        custodian_signing_key: ed25519_dalek::SigningKey,
        witness: fauna_core::encoding::EmbedAsBytes,
    ) -> Result<Self> {
        let device_id = custodian_signing_key.verifying_key().to_bytes();
        let client = fauna_client::ws_custody_handshake_bearer::custody_nest_client(
            url,
            owner,
            custodian_signing_key,
            witness,
        );
        client
            .connect()
            .await
            .map_err(|e| anyhow::anyhow!("custody handshake/connect refused: {e}"))?;
        let bytes =
            crate::nest_client::SyncClient::new(std::sync::Arc::clone(client.auth()), &device_id);
        Ok(Self {
            url: url.to_string(),
            client,
            bytes,
        })
    }

    /// The owner-nest URL this session was built for — the anchor-move
    /// invalidation key (a ceremony can move the anchor, and a session
    /// pointed at the old URL must not outlive the row that named it).
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The session's control plane — the custody-bearer `NestClient` itself,
    /// for requests beyond the pull core (the custodian-nest pump's receipt
    /// deposit rides the SAME bearer the pull does, so revocation severs
    /// both at once).
    pub fn client(&self) -> &std::sync::Arc<fauna_client::NestClient> {
        &self.client
    }

    /// Disconnect the control plane (the bulk plane rides the same bearer and
    /// dies with it). Callers drop the session after a failed pull so the
    /// next pass re-handshakes — a revoked custody then takes its honest
    /// refusal at the mint.
    pub async fn disconnect(&self) {
        self.client.disconnect().await;
    }
}

#[cfg(feature = "account-runtime")]
/// Worker-owned custodian state (P4/P5): the custodied stores, keyed by
/// owner account, opened lazily from the machine's own `custodies-held`
/// rows; plus the account the revocation refresh folds the grant log as.
pub(crate) struct CustodyLegState {
    store_root: StoreRoot,
    actor_id: ActorId,
    /// The machine's device principal — the custodied stores' (never-
    /// authoring) writer identity tag, the NodeId self-dial guard, and (as a
    /// signing key) the PoP the nest leg's custody handshake is minted with.
    /// Held as the KEY, with [`Self::device_id`] derived from it, so the id
    /// the stores are tagged with and the key the owner's nest verifies
    /// cannot drift apart.
    device_key: ed25519_dalek::SigningKey,
    held: HashMap<[u8; 32], CustodiedStores>,
}

/// The ops whose relay payload is T15's always-present floor: "journal,
/// frontiers, tombstones, and the index floor are always-present". The
/// coordinate columns are floor by construction (eviction only ever clears
/// `entry`); this names the one *op* whose envelope must survive too, because
/// the sealed envelope carries the authoritative tombstone marker
/// (`fauna_account_store::types::RelayRow::op`).
const CUSTODY_FLOOR_OPS: &[&str] = &[fauna_protocol::account_state::OP_TOMBSTONE];

#[cfg(feature = "account-runtime")]
impl CustodyLegState {
    pub(crate) fn new(
        store_root: StoreRoot,
        actor_id: ActorId,
        device_key: ed25519_dalek::SigningKey,
    ) -> Self {
        Self {
            store_root,
            actor_id,
            device_key,
            held: HashMap::new(),
        }
    }

    /// The device principal's public bytes — the custodied stores' writer tag
    /// and the witness's `custodian_key`, derived rather than stored.
    fn device_id(&self) -> [u8; 32] {
        self.device_key.verifying_key().to_bytes()
    }

    /// Re-derive the custody-revocation snapshot (P2's pump half) from the
    /// account's grant-event log — the succession ledger's event rows
    /// (`fauna.state.succession-ledger`), read off this runtime's own store as
    /// its identity folds them: revoked = custody-class grant ids whose latest
    /// logged verb is not a live mint — "seen in the log but not current". An
    /// id the log has never seen stays admitted: log-first mint means a store
    /// the owner's event has not reached yet MAY serve for one plane-sync
    /// window — T13's ratified honest bound — and so does an empty log
    /// (nothing seen, an empty set, derived).
    ///
    /// Run by the pump beside the removed-device refresh, right after the
    /// fleet walk and BEFORE the peer-leg ensure step, so a first bind's
    /// server and this pass's dial read a derived set. A ledger row the fold
    /// refuses is this call's error and leaves the snapshot as it was —
    /// underived before a first success, so every custody admission is
    /// refused; the last derived set after one
    /// ([`crate::peer_leg::WithdrawalSnapshot`] owns that posture).
    ///
    /// Only the READ fold's events count — those the folded chain signed — so a
    /// stranger's `Revoke` row, which any tip holder could write, never
    /// refuses a live grant here.
    pub(crate) async fn refresh_revoked<B: StoreBackend>(
        &self,
        store: &AccountStore<B>,
        revoked: &crate::peer_leg::WithdrawalSnapshot<Vec<u8>>,
    ) -> Result<usize> {
        let ledger = fauna_account_plane::succession_ledger_rows::read_succession_ledger(
            store,
            self.actor_id,
        )
        .await
        .context("the account's grant log")?;
        let current: HashSet<Vec<u8>> = fauna_core::grant_event::current_grants(&ledger)
            .into_iter()
            .filter(|g| fauna_client_capabilities::custody_grants::is_custody_grant(&g.scope))
            .map(|g| g.grant_id.clone())
            .collect();
        let seen: HashSet<Vec<u8>> = ledger
            .grant_events
            .iter()
            .filter(|e| {
                fauna_client_capabilities::custody_grants::is_custody_grant(&e.scope)
                    || e.scope.is_empty() // a Revoke event is keyless/scope-empty
            })
            .map(|e| e.grant_id.clone())
            .collect();
        let next: HashSet<Vec<u8>> = seen.difference(&current).cloned().collect();
        let n = next.len();
        revoked.replace(next);
        Ok(n)
    }

    /// Refresh the serve registry (P1's pump half): read this machine's own
    /// `custodies-held` rows, ensure a custodied store per owner, and feed
    /// the serve handles to the live server. Runs after the fleet walk (rows
    /// fresh) and after the peer-leg ensure (server up); with no live server
    /// (leg off/unbound) only the stores are ensured, so a later bind starts
    /// with them warm.
    ///
    /// The same rows feed `exclusions` — the custodian's removed-device map
    /// (`fauna_peer_sync::admission::CustodiedExclusions`, the owner-signed
    /// lists of every grant held, unioned per account) — installed BEFORE the
    /// registry, so an account is never served under a map that predates its
    /// grant. Every held row contributes, stopped ones included: a list is the
    /// owner's signed statement of a removal, which never reverses.
    pub(crate) async fn serve_refresh<B: StoreBackend>(
        &mut self,
        own_store: &AccountStore<B>,
        server: Option<&Arc<fauna_peer_sync::server::PeerSyncServer>>,
        exclusions: &fauna_peer_sync::admission::CustodiedExclusions,
    ) -> Result<CustodyServePass> {
        let mut pass = CustodyServePass::default();

        // The serve registry (P1): one custodied store per held custody.
        let mut handles = HashMap::new();
        let mut held_witnesses: Vec<fauna_core::encoding::EmbedAsBytes> = Vec::new();
        for entry in own_store.states_of_kind(KIND_CUSTODIES_HELD).await? {
            let row: CustodyHeld = match fauna_core::encoding::canonical_decode(&entry.value) {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!("custody leg: unreadable custodies-held row ({e}) — skipped");
                    continue;
                }
            };
            if row.witness.is_empty() {
                continue;
            }
            if let Ok(w) = fauna_core::encoding::canonical_decode(&row.witness) {
                held_witnesses.push(w);
            }
            // T16's stop control: a stopped custody neither serves nor pulls
            // — the leg simply drops it from the registry, and the owner
            // sees receipts go stale (degraded redundancy, the honest
            // signal). The row stays at rest; a resumed hold is a fresh put.
            if row.stopped {
                continue;
            }
            let owner = row.owner;
            if !self.held.contains_key(&owner) {
                match self.open_custodied(&owner, &row).await {
                    Ok(stores) => {
                        self.held.insert(owner, stores);
                    }
                    Err(e) => {
                        tracing::warn!(
                            owner = %fauna_core::hex32::encode(&owner),
                            "custody leg: custodied store unavailable this pass: {e:#}"
                        );
                        continue;
                    }
                }
            } else if let Some(held) = self.held.get_mut(&owner) {
                held.row = row.clone();
            }
            if let Some(held) = self.held.get(&owner) {
                // The reply witness for this served account is THE CUSTODY
                // GRANT: an owner-fleet device dialing this custodian
                // verifies its entitlement by exactly that witness.
                let reply_witness: fauna_core::encoding::EmbedAsBytes =
                    match fauna_core::encoding::canonical_decode(&held.row.witness) {
                        Ok(w) => w,
                        Err(e) => {
                            tracing::warn!(
                                "custody leg: held witness unreadable ({e}) — not serving"
                            );
                            continue;
                        }
                    };
                handles.insert(
                    owner,
                    fauna_peer_sync::server::ServedAccount {
                        store: held.serve.clone(),
                        reply_witness,
                        reply_witness_kind: fauna_protocol::peer_sync::WITNESS_CUSTODY_GRANT
                            .to_string(),
                    },
                );
            }
        }
        pass.served = handles.len();
        exclusions.replace(fauna_peer_sync::admission::CustodiedExclusions::derive(
            &held_witnesses,
        ));
        if let Some(server) = server {
            server.set_custodied(handles);
        }
        Ok(pass)
    }

    async fn open_custodied(&self, owner: &[u8; 32], row: &CustodyHeld) -> Result<CustodiedStores> {
        let owner_hex = fauna_core::hex32::encode(owner);
        let dir = self
            .store_root
            .store_dir(&owner_hex)
            .context("custody leg: custodied store dir")?;
        // The custodian's own device principal is the (never-authoring)
        // writer identity — a custodian holds and serves, never journals
        // (P4).
        let writer = WriterId(self.device_id());
        let pull = AccountStore::open(SqliteBackend::open(&dir)?, &owner_hex, writer)
            .await
            .context("custody leg: open custodied store (pull)")?;
        let serve_store = AccountStore::open(SqliteBackend::open(&dir)?, &owner_hex, writer)
            .await
            .context("custody leg: open custodied store (serve)")?;
        Ok(CustodiedStores {
            pull,
            serve: ServeStoreHandle::spawn(serve_store),
            row: row.clone(),
            nest: None,
            brake: IngestBrake::default(),
        })
    }

    /// The custodian **nest** pass (W8.6's pump half): for every held custody
    /// whose row names an `owner_nest_url`, pull the owner's sealed planes from
    /// the OWNER'S NEST over the custody handshake — the leg that needs no
    /// owner device to be awake, reachable, or even known.
    ///
    /// This is the peer dial pass's sibling, and the two are deliberately
    /// independent: a custody may have both legs, either, or (a row with
    /// neither a URL nor devices) none. Per-custody isolation, exactly as the
    /// dial pass isolates per target — an owner's nest being down is ordinary
    /// weather.
    ///
    /// **Coordinates AND bytes** (the composition W8.6 left open, and
    /// what separates "real redundancy" from a restore-from-custody). Each
    /// content scope gets two steps over the one custody session: the bulk half
    /// adopts the owner's segment files, then the walk records the record-CID
    /// coordinates. Bulk runs FIRST — the bootstrap contract's order
    /// (`account-data-plane.md` § *How nest CARv2 segments map onto the local
    /// log*: adopt the segments, then walk the feed).
    ///
    /// ⚠ This doc comment used to say the door served a custodian no blocks at
    /// all. That was true for about two hours: a follow-up slice opened `fauna.segments.list` and the segment byte pair to
    /// `CallerClass::Custodian` the same day W8.6 landed, and the two halves
    /// were never joined until this leg did it.
    ///
    /// **Adoption never decrypts.** `fauna_account_store::segments::admit`
    /// verifies CARv2 framing, re-hashes every block against the CID it is
    /// filed under, and binds the sidecar's actor to this store's — so a
    /// keyless custodian ends the pass holding the owner's *sealed* record
    /// bytes and still unable to read one. That is the whole posture, and the
    /// reason the bulk half needs no key material the custodian does not have.
    ///
    /// The peer leg's equivalent is `pull_missing_blocks`, a peer-channel
    /// mechanism with no nest-side twin; the segment plane is the nest-side
    /// answer, so this is where the two legs stop differing in what they land.
    pub(crate) async fn nest_pass(&mut self) -> Result<CustodyNestPass> {
        let mut report = CustodyNestPass::default();
        let owners: Vec<[u8; 32]> = self.held.keys().copied().collect();
        for owner in owners {
            let device_key = self.device_key.clone();
            let Some(held) = self.held.get_mut(&owner) else {
                continue;
            };
            // The T15 ingest brake: an at-floor or meter-dark
            // custody accumulates nothing — no session work, no pull. The
            // dial pass's unconditional budget arm is what releases it.
            if !held.brake.admits_ingest() {
                report.braked += 1;
                continue;
            }
            let Some(url) = held.row.owner_nest_url.clone() else {
                continue; // no anchor on this row — the peer leg is its only route
            };
            report.custodies += 1;
            // The dial-policy backstop: the URL is the
            // COUNTERPARTY's string, so it is validated at every dial site,
            // not only at ingest — a row carrying a bad URL must never reach
            // `connect()` either. The ruling:
            // `account-data-plane.md` § The custody grant + ceremony → *What
            // a custodian will dial*.
            if let Err(reason) = fauna_core::counterparty_url::validate_counterparty_nest_url(&url)
            {
                tracing::warn!(
                    owner = %fauna_core::hex32::encode(&owner),
                    "custody nest leg: owner_nest_url refused by dial policy ({reason}) — not dialed"
                );
                report.refused_url += 1;
                continue;
            }
            let witness: fauna_core::encoding::EmbedAsBytes =
                match fauna_core::encoding::canonical_decode(&held.row.witness) {
                    Ok(w) => w,
                    Err(e) => {
                        tracing::warn!("custody nest leg: held witness unreadable ({e}) — skipped");
                        report.failed += 1;
                        continue;
                    }
                };
            let scopes = match pullable_scopes(&witness) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("custody nest leg: held grant unreadable ({e}) — skipped");
                    report.failed += 1;
                    continue;
                }
            };
            // A ceremony can move the anchor; a session built for the old URL
            // must not survive the row that named it.
            if held.nest.as_ref().is_some_and(|s| s.url != url)
                && let Some(stale) = held.nest.take()
            {
                stale.client.disconnect().await;
            }
            if held.nest.is_none() {
                // The custody handshake mints inside `connect`, so a dead row
                // fails at connect — the same place the tier_3 door proof
                // puts it.
                match NestLegSession::connect(&url, owner, device_key, witness).await {
                    Ok(session) => held.nest = Some(session),
                    Err(e) => {
                        tracing::debug!(
                            owner = %fauna_core::hex32::encode(&owner),
                            "custody nest leg: the owner's nest refused or is unreachable: {e}"
                        );
                        report.failed += 1;
                        continue;
                    }
                }
            }
            let session = held.nest.as_ref().expect("just set");
            // The row's cap, in `meter_and_evict`'s terms (0 = none recorded).
            let cap = (held.row.retained_bytes_cap != 0).then_some(held.row.retained_bytes_cap);
            match pull_from_owner_nest(&held.pull, session, &owner, &scopes, cap).await {
                Ok(pulled) => {
                    report.pulled += 1;
                    report.recorded += pulled.recorded;
                    report.adopted_segments += pulled.adopted_segments;
                }
                Err(e) => {
                    report.failed += 1;
                    tracing::debug!(
                        owner = %fauna_core::hex32::encode(&owner),
                        "custody nest leg: pull failed: {e:#}"
                    );
                    // Drop the session so the next pass re-handshakes. A revoked
                    // custody then takes its honest refusal at the mint, and a
                    // merely-flaky connection costs one extra handshake.
                    if let Some(dead) = held.nest.take() {
                        dead.client.disconnect().await;
                    }
                }
            }
        }
        Ok(report)
    }

    /// The custodian dial pass (P5): for every held custody, dial the owner
    /// fleet's candidates from the row, admit with the custody witness (the
    /// reply is the owner device's `DeviceAuthorization` — verified with no
    /// revocation view, correct for that kind), and run the schedule-free
    /// pulls for every scope the witness names. Per-target isolation.
    ///
    /// **`transport` is optional, and the pass runs either way** — because
    /// T15's budget is not a peer-plane concern. The nest leg
    /// ([`Self::nest_pass`]) lands bytes on a machine that may have no peer
    /// transport bound at all, and a row with no `owner_devices` has no dial
    /// targets even when one is; if the budget were reachable only through a
    /// dial, those custodies would grow unmetered forever. So the dial half is
    /// what the transport gates, and every held custody is metered regardless.
    pub(crate) async fn dial_pass(
        &self,
        transport: Option<&Arc<dyn fauna_transport::PeerTransport>>,
        lan_ips: &[std::net::Ipv4Addr],
        now_secs: u64,
        own_endpoints: Option<&DeviceEndpoints>,
        exclusions: &fauna_peer_sync::admission::CustodiedExclusions,
    ) -> Result<CustodyDialPass> {
        let mut report = CustodyDialPass::default();
        for (owner, held) in &self.held {
            // T13 step 4: what the owner devices told us about themselves this
            // pass. Keyed by the channel-proven key, and folded in by the ONE
            // rule both write-back paths share — a device that did not answer
            // keeps its previous entry (a dial failure must never erase the
            // only address we hold for it), and a device the row does not
            // already name is never added.
            let mut observed: HashMap<[u8; 32], DeviceEndpoints> = HashMap::new();
            // The dial half. Every early exit here (no transport, an unreadable
            // witness, no targets) skips DIALING only — the budget below still
            // runs, which is the whole point of hoisting it out of this block.
            if !held.brake.admits_ingest() {
                // The brake stops the PULL half only — the budget arm below
                // still runs, which is how the brake releases.
                report.braked += 1;
            } else if let Some(transport) = transport
                && let Some((witness, scopes)) = self.dialable(held)
            {
                {
                    let targets: Vec<_> = held
                        .row
                        .owner_devices
                        .iter()
                        .cloned()
                        .map(|d| fauna_peer_sync::dial_target_from(d, lan_ips))
                        .collect();
                    if !targets.is_empty() {
                        report.custodies += 1;
                    }
                    for target in targets {
                        let attempt = tokio::time::timeout(
                            PER_CUSTODY_OWNER_BUDGET,
                            self.dial_one_owner(
                                transport,
                                held,
                                owner,
                                &witness,
                                &scopes,
                                &target,
                                now_secs,
                                own_endpoints,
                                exclusions,
                            ),
                        )
                        .await;
                        match attempt {
                            Ok(Ok((recorded, peer_endpoints))) => {
                                report.admitted += 1;
                                report.recorded += recorded;
                                if let Some(fresh) = peer_endpoints {
                                    observed.insert(fresh.node_id, fresh);
                                }
                            }
                            Ok(Err(e)) => {
                                report.failed += 1;
                                tracing::debug!(
                                    node = %fauna_core::hex32::encode(&target.node_id),
                                    "custody dial: owner device unreachable or refused: {e:#}"
                                );
                            }
                            Err(_) => {
                                report.failed += 1;
                                tracing::debug!(
                                    node = %fauna_core::hex32::encode(&target.node_id),
                                    "custody dial: owner device blew its budget — abandoned this pass"
                                );
                            }
                        }
                    }
                }
            }

            // T15's budget, applied where the bytes just landed — from EITHER
            // leg. Unreachable owners above are ordinary weather; the budget is
            // not — a custody that pulled nothing this pass may still be over
            // cap from the last one, so this runs per custody unconditionally.
            // The DEVICE leg's row carries a 0-means-absent field (`#[serde(default)]`
            // on `CustodyHeld`), so the translation happens here, where that
            // field's history is visible — not inside `meter_and_evict`, whose
            // `Some(0)` means a real "hold the floor" budget.
            let cap = (held.row.retained_bytes_cap != 0).then_some(held.row.retained_bytes_cap);
            match meter_and_evict(&held.pull, cap).await {
                Ok(outcome) => {
                    held.brake.record_metered(outcome.unreclaimable > 0);
                    report.held_bytes = report.held_bytes.saturating_add(outcome.held_bytes);
                    report.evicted_rows += outcome.evicted.rows;
                    report.evicted_bytes =
                        report.evicted_bytes.saturating_add(outcome.evicted.bytes);
                    if outcome.unreclaimable > 0 {
                        report.at_floor += 1;
                    }
                    report.outcomes.push(CustodyPassOutcome {
                        grant_id: held.row.grant_id.clone(),
                        owner: *owner,
                        outcome,
                    });
                }
                Err(e) => {
                    // Metering failure is not a SERVING failure: keep holding
                    // and keep serving. But it is no longer fail-open on the
                    // ingest side: consecutive failures engage the
                    // brake above, because the likeliest cause — a full disk,
                    // a wedged store — is exactly the condition the cap
                    // exists to bound.
                    held.brake.record_metering_failure();
                    report.metering_failed += 1;
                    tracing::warn!(
                        owner = %fauna_core::hex32::encode(owner),
                        "custody budget pass failed (still holding and serving; \
                         ingest brakes after {METERING_FAILURES_BRAKE} consecutive failures): {e:#}"
                    );
                }
            }
            report
                .refreshed
                .extend(crate::custody_rows::held_rows_to_refresh(
                    [held.row.clone()],
                    &observed,
                ));
        }
        Ok(report)
    }

    /// The witness + the scopes it lets us pull, or `None` with a warn when
    /// the row's own artifact is unreadable. A custody whose witness cannot be
    /// decoded can still be metered and still be served the bytes it already
    /// holds — this answers the DIAL question only.
    fn dialable(
        &self,
        held: &CustodiedStores,
    ) -> Option<(fauna_core::encoding::EmbedAsBytes, PullableScopes)> {
        let witness: fauna_core::encoding::EmbedAsBytes =
            match fauna_core::encoding::canonical_decode(&held.row.witness) {
                Ok(w) => w,
                Err(e) => {
                    tracing::warn!("custody leg: held witness unreadable ({e}) — skipped");
                    return None;
                }
            };
        match pullable_scopes(&witness) {
            Ok(s) => Some((witness, s)),
            Err(e) => {
                tracing::warn!("custody leg: held grant unreadable ({e}) — skipped");
                None
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn dial_one_owner(
        &self,
        transport: &Arc<dyn fauna_transport::PeerTransport>,
        held: &CustodiedStores,
        owner: &[u8; 32],
        witness: &fauna_core::encoding::EmbedAsBytes,
        scopes: &PullableScopes,
        target: &fauna_peer_sync::PeerDialTarget,
        now_secs: u64,
        own_endpoints: Option<&DeviceEndpoints>,
        exclusions: &fauna_peer_sync::admission::CustodiedExclusions,
    ) -> Result<(usize, Option<DeviceEndpoints>)> {
        let channel = crate::peer_leg::dial_and_open_channel(transport, target).await?;
        // The reply witness is the owner device's `DeviceAuthorization`;
        // custody replies are fail-closed-refused (no revocation view), which
        // is correct — an owner device never answers with a custody grant.
        // The removed-device half reads the SAME exclusion map the serve side
        // does: this custodian cannot read the owner account's sealed
        // `state-fleet` device-set rows, so the owner-signed lists on the
        // grants it holds are its only exclusion set, and a device they name
        // is walked in neither direction (`fauna_peer_sync::server::
        // DeviceRemovedFn` states the bound).
        let removed_view = exclusions.view();
        // T13 step 4 in both directions: carry this custodian's own candidates
        // (the owner fleet cannot read our `device-endpoints` kind either) and
        // take the owner device's back, bound to the channel-proven key.
        let outcome = fauna_peer_sync::admit_over_as(
            &channel,
            fauna_protocol::peer_sync::WITNESS_CUSTODY_GRANT,
            witness.clone(),
            owner,
            now_secs,
            fauna_peer_sync::AdmissionViews {
                custody_revoked: None,
                device_removed: Some(&removed_view),
            },
            own_endpoints.cloned(),
        )
        .await
        .context("custody admission")?;
        let requester = fauna_peer_sync::PeerRequester::new(Arc::clone(&channel));
        let mut recorded = 0usize;
        for scope in &scopes.account_state {
            recorded += custody_pull(&held.pull, &requester, scope, None)
                .await?
                .recorded;
        }
        for cs in &scopes.content {
            crate::peer_leg::walk_and_pull_content_scope(
                &held.pull,
                &requester,
                &channel,
                cs,
                "custody content walk",
                "custody blocks pull",
                // Ruling 4 names the same-account peer leg and the share
                // pump; the custody leg's nest is the OWNER's, whose
                // per-custody reachability is not threaded here, so it
                // pulls on any path as before — `Unavailable` is "not
                // known to be available", the ruling's own exception.
                fauna_transport::NestPath::Unavailable,
            )
            .await?;
        }
        Ok((recorded, outcome.peer_endpoints))
    }
}

/// What one custody's nest pull landed, split by plane — the relay rows the
/// account-state legs recorded, and the segment files the bulk legs adopted.
#[derive(Debug, Clone, Copy, Default)]
pub struct NestPullTally {
    pub recorded: usize,
    pub adopted_segments: usize,
}

/// The bulk half of one content scope: adopt the owner's segment files for the
/// bytes the coordinate walk's CIDs name.
///
/// **Absorbing, by design — it returns a count, never an error.** The
/// coordinate walk that follows is this leg's authoritative verdict on the
/// session: a revoked custody refuses BOTH halves, and letting the walk be the
/// one that says so keeps a single error path instead of two racing to drop the
/// session. What that buys is the honest partial — coordinates held, bytes not
/// yet — which is a state a later pass simply retries into, and is strictly
/// better than failing a pull that did land the index.
///
/// Gated on **adoptability, not on what the nest happens to serve**: a kind
/// whose records are not content-addressed cannot pass
/// `fauna_account_store::segments::admit`, so enumerating it would spend a
/// round trip to earn a verification failure. Since the 2026-08-17 cutover
/// legs + the serve-plane wiring, `post`, `mail`, `calendar` and
/// `card` are both admissible and served. `conv` joined `ADOPTABLE_KINDS` with
/// its own cutover leg the same day, and joined the serve plane 2026-08-18
/// (ruled + built): addressed by its scope id (the channel — the
/// binding derives the fetch address from the scope, so this walk needs no
/// conv-specific arm), under the explicit-list member-mint rule
/// (`message-segment-store.md` § *Which kinds the two planes serve*). The two
/// lists still deliberately answer different questions.
///
/// **Budgeted:** `budget` is the headroom the custody's cap leaves
/// over what the store already holds, spent as pairs are adopted — so adoption
/// stops at the cap instead of overrunning it, and it doubles as the download
/// bound ([`AccountStore::bootstrap_scope_segments_within`]).
async fn adopt_scope_segments<B: StoreBackend>(
    store: &AccountStore<B>,
    session: &NestLegSession,
    cs: &fauna_protocol::scope::ContentScope,
    budget: &mut u64,
) -> usize {
    if !fauna_account_store::segments::ADOPTABLE_KINDS.contains(&cs.kind()) {
        return 0;
    }
    // The binding derives its fetch address from the scope itself — the owner
    // for single-principal kinds, the CHANNEL for conv (the serve plane's
    // actor field carries the scope id for every kind).
    let binding = crate::content_scope_plane::binding_for(cs);
    let source = crate::bootstrap_source::NestBootstrapSource::new(
        session.client.as_ref(),
        &session.bytes,
        binding,
    );
    match store
        .bootstrap_scope_segments_within(&cs.to_string(), &source, budget)
        .await
    {
        Ok(report) => report.adopted,
        Err(e) => {
            tracing::debug!(
                scope = %cs,
                "custody nest leg: bulk segment adoption did not complete: {e:#}"
            );
            0
        }
    }
}

/// One custody's pull from the owner's nest: every account-state scope the
/// witness names, then every content scope's coordinate walk — both addressed
/// with `of_owner`, which is what makes the nest re-derive the custody verdict
/// from the live capability row on each request.
///
/// The two legs are sequenced, not isolated from each other: a refusal on the
/// first scope is the caller's signal to drop the session (a revoke refuses
/// every scope, and retrying the rest just collects the same error N times).
///
/// Each content scope is walked in the bootstrap contract's order — **bulk
/// segments first, then the coordinate walk**.
///
/// `cap_bytes` is the custody's byte budget, in [`meter_and_evict`]'s terms
/// (`None` = no cap recorded → the ceremony default). The segment half adopts
/// only within the headroom it leaves, metered once the account-state legs
/// have landed: the relay plane is re-bounded by the budget pass that follows,
/// but a segment adopted past the cap would be bytes on the host's disk the
/// cap exists to prevent.
pub async fn pull_from_owner_nest<B: StoreBackend>(
    store: &AccountStore<B>,
    session: &NestLegSession,
    owner: &[u8; 32],
    scopes: &PullableScopes,
    cap_bytes: Option<u64>,
) -> Result<NestPullTally> {
    let owner_hex = fauna_core::hex32::encode(owner);
    let client = &session.client;
    let mut tally = NestPullTally::default();
    for scope in &scopes.account_state {
        tally.recorded += custody_pull(store, client, scope, Some(&owner_hex))
            .await?
            .recorded;
    }
    let cap = cap_bytes.unwrap_or(fauna_core::custody_ceremony::DEFAULT_RETAINED_BYTES_CAP);
    let mut budget = if scopes.content.is_empty() {
        0
    } else {
        let held = store
            .custody_meter(CUSTODY_FLOOR_OPS)
            .await
            .context("custody nest leg: meter before segment adoption")?
            .held_bytes();
        cap.saturating_sub(held)
    };
    for cs in &scopes.content {
        tally.adopted_segments += adopt_scope_segments(store, session, cs, &mut budget).await;
        crate::content_scope_plane::ContentScopePlane::new(store, client, cs.clone())
            .of_owner(*owner)
            .walk()
            .await
            .with_context(|| format!("custody nest content walk {cs}"))?;
    }
    Ok(tally)
}

/// Apply T15's byte budget to one custodied store: meter it, plan, evict
/// payload only, and report the honest result.
///
/// `cap_bytes` is `None` for **no cap recorded** — the `CustodyHeld` row's field
/// is `#[serde(default)]`, so a row carrying no accepted cap (0 means
/// default, per `effective_hosting_cap`) decodes as 0, and treating that as a zero budget would
/// evict a custody's whole payload on the strength of a missing field. `None`
/// falls back to the ceremony default
/// ([`fauna_core::custody_ceremony::DEFAULT_RETAINED_BYTES_CAP`]); a host that
/// genuinely wants no payload retained narrows scopes instead.
///
/// ⚠ **`Some(0)` is a REAL budget — "hold the floor, no payload"** — which is
/// why this takes an `Option` rather than reading 0 as absent.
/// The nest's hosting pump squeezes a row's cap to the host's remaining tier
/// headroom, and that squeeze legitimately reaches zero; under the old
/// 0-means-absent signature such a row silently got the 8 GiB default instead of
/// nothing, so the bound failed exactly at its limit. Callers that hold a
/// 0-means-absent wire field translate at the call site, where the field's own
/// history is visible.
pub async fn meter_and_evict<B: StoreBackend>(
    store: &AccountStore<B>,
    cap_bytes: Option<u64>,
) -> Result<CustodyBudgetOutcome> {
    let cap = cap_bytes.unwrap_or(fauna_core::custody_ceremony::DEFAULT_RETAINED_BYTES_CAP);
    let meter = store
        .custody_meter(CUSTODY_FLOOR_OPS)
        .await
        .context("custody budget: meter")?;
    let plan = fauna_core::custody_policy::plan_custody_eviction(&meter, cap);
    let evicted = store
        .apply_custody_eviction(&plan, CUSTODY_FLOOR_OPS)
        .await
        .context("custody budget: evict")?;
    // Re-meter rather than subtract. A receipt is an attestation, and an
    // attestation assembled by arithmetic over a stale snapshot would report
    // coverage nobody measured — the exact gap between "what I planned to hold"
    // and "what I hold". Cheap (one indexed scan) and only paid when eviction
    // actually ran.
    let held = if evicted.bytes > 0 {
        tracing::info!(
            rows = evicted.rows,
            bytes = evicted.bytes,
            cap,
            "custody budget: evicted payload (coordinates, tombstones and the \
             index floor untouched)"
        );
        store
            .custody_meter(CUSTODY_FLOOR_OPS)
            .await
            .context("custody budget: re-meter")?
    } else {
        meter.clone()
    };
    Ok(CustodyBudgetOutcome {
        held_bytes: held.held_bytes(),
        state: plan.state,
        unreclaimable: plan.unreclaimable,
        cap,
        judged: meter,
        held,
        evicted,
    })
}

#[cfg(feature = "account-runtime")]
/// The custody-ceremony state as a pump pass reaches it — the pass's own
/// store and fleet plane, on the store thread, where the handle's `Send` seam
/// cannot run. The same fold and per-record join as the handle's door
/// (`fauna_account_plane::custody_ceremony_rows`), so a receipt recorded here
/// lands exactly as one recorded by an app.
pub struct PassCeremonyRecords<'a, B: StoreBackend, R: RpcRequester> {
    pub store: &'a AccountStore<B>,
    pub fleet: &'a crate::account_state_plane::AccountStatePlane<'a, B, R>,
}

#[cfg(feature = "account-runtime")]
impl<B: StoreBackend, R: RpcRequester> fauna_client_capabilities::custody_ceremony::CeremonyRecords
    for PassCeremonyRecords<'_, B, R>
{
    async fn snapshot(
        &self,
    ) -> Result<
        fauna_core::custody_ceremony::CustodyConfig,
        fauna_client_capabilities::custody_ceremony::CustodyCeremonyError,
    > {
        fauna_account_plane::custody_ceremony_rows::read_custody(self.store)
            .await
            .map_err(|e| fauna_client_config::StoreError::Load(format!("{e:#}")).into())
    }

    async fn update<T>(
        &self,
        f: impl FnOnce(&mut fauna_core::custody_ceremony::CustodyConfig) -> T + Send,
    ) -> Result<T, fauna_client_capabilities::custody_ceremony::CustodyCeremonyError> {
        let mut state = self.snapshot().await?;
        let out = f(&mut state);
        fauna_account_plane::custody_ceremony_rows::merge_custody(self.store, self.fleet, &state)
            .await
            .map_err(|e| fauna_client_config::StoreError::Save(format!("{e:#}")))?;
        Ok(out)
    }
}

#[cfg(feature = "account-runtime")]
/// The check-in cadence's "record" half (W8.7 arc 2): for each custody the
/// budget pass just judged, mint the A7 receipt when one is due and record
/// it on the held ceremony record — `drive_ceremonies`' receipt arm posts it
/// over the ceremony's own channel. Runs in the pump right after the
/// custodian dial pass, so an app-dead agent keeps attesting; the owed
/// receipt then posts whenever a conversations-capable session next drives.
///
/// Two self-checks before any mint:
/// - **only the accept-bound device attests** — the owner verifies receipts
///   against exactly the key its accept bound, and this device's meter
///   describes this device's store, so a fleet sibling minting would be both
///   refused and wrong;
/// - **only when [`fauna_core::custody_receipt::receipt_due`] says so** —
///   never attested / interval elapsed / evicted this pass / degraded flip.
///
/// Failures are absorbed per custody (log + skip): a mint that cannot happen
/// this pass happens on a later one — the cadence's own retry.
pub async fn mint_due_receipts<U>(
    door: &U,
    writer_key: &ed25519_dalek::SigningKey,
    outcomes: &[CustodyPassOutcome],
    now: fauna_core::data::Timestamp,
) -> usize
where
    U: fauna_client_capabilities::custody_ceremony::CeremonyRecords,
{
    use fauna_client_capabilities::custody_ceremony as ceremony;
    if outcomes.is_empty() {
        return 0;
    }
    let custody = match door.snapshot().await {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!(
                "custody receipt mint: ceremony state unavailable ({e}) — next pass retries"
            );
            return 0;
        }
    };
    let custodian = fauna_core::identity::ActorKeypair::from_secret(writer_key.to_bytes());
    let own_key = custodian.actor_id().0;
    let mut minted = 0usize;
    for entry in outcomes {
        let Some(rec) = custody.held.iter().find(|h| h.grant_id == entry.grant_id) else {
            // A registry row with no ceremony record (its record row has not
            // walked in yet) — nothing to hang a channel or cadence on; the
            // record's arrival re-opens minting.
            continue;
        };
        match ceremony::accept_bound_custodian_key(rec) {
            Some(bound) if bound == own_key => {}
            _ => continue,
        }
        let receipt = entry
            .outcome
            .receipt(entry.grant_id.clone(), entry.owner, own_key, now);
        let degraded = receipt.is_degraded();
        if !fauna_core::custody_receipt::receipt_due(
            now,
            rec.receipt_minted_at,
            entry.outcome.evicted.bytes > 0,
            degraded,
            rec.receipt_degraded,
        ) {
            continue;
        }
        let env = match fauna_core::custody_receipt::sign_custody_receipt(&custodian, &receipt) {
            Ok(env) => env,
            Err(e) => {
                tracing::warn!("custody receipt mint: sign failed ({e}) — skipped");
                continue;
            }
        };
        let bytes = match fauna_core::encoding::canonical_encode(&env) {
            Ok(b) => b.to_vec(),
            Err(e) => {
                tracing::warn!("custody receipt mint: encode failed ({e}) — skipped");
                continue;
            }
        };
        ceremony::record_minted_receipt(door, &entry.grant_id, bytes, now, degraded).await;
        minted += 1;
    }
    minted
}

/// The scopes a held witness lets the custodian PULL — derived from the
/// grant it holds (no verification here: the serving side verifies; this is
/// the custodian reading its own artifact).
pub struct PullableScopes {
    account_state: Vec<String>,
    content: Vec<fauna_protocol::scope::ContentScope>,
}

/// `Account` form → the two account-state scopes plus the own-actor content
/// scopes derived from the witness's own `owner` field (the coverage-
/// enumeration ruling, charter § The custody grant + ceremony: the covered
/// content set is a pure function of the owner, so the custodian names it
/// locally with zero disclosure — co-authored planes are outside the form by
/// the shared-audience carve-out and never appear here); explicit list →
/// partitioned into account-state pulls and content walks.
pub fn pullable_scopes(witness: &fauna_core::encoding::EmbedAsBytes) -> Result<PullableScopes> {
    let (bytes, _env) = witness.clone().into_signed()?;
    let grant: CustodyGrant = fauna_core::encoding::decode_signed_bytes(&bytes)?;
    let mut scopes = PullableScopes {
        account_state: Vec::new(),
        content: Vec::new(),
    };
    match grant.scopes {
        fauna_core::custody_grant::CustodyScopeSet::Account => {
            scopes.account_state = vec![
                ACCOUNT_STATE_SCOPE.to_string(),
                ACCOUNT_STATE_FLEET_SCOPE.to_string(),
            ];
            scopes.content = crate::scope_set::derive_own_actor_scopes(grant.owner.0)?;
        }
        fauna_core::custody_grant::CustodyScopeSet::Scopes(list) => {
            for s in list {
                if fauna_protocol::account_state::is_served_scope(&s) {
                    scopes.account_state.push(s);
                } else if let Ok(fauna_protocol::scope::Scope::Content(cs)) = s.parse() {
                    scopes.content.push(cs);
                }
            }
        }
        // A set a newer build minted covers no scope here: nothing to pull.
        fauna_core::custody_grant::CustodyScopeSet::Unknown(_) => {}
    }
    Ok(scopes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_account_store::sqlite::SqliteBackend;
    use fauna_account_store::types::WriterId;
    use fauna_protocol::sync::SyncChange;
    use std::sync::Mutex;

    const OWNER: [u8; 32] = [0xA1; 32];

    fn row(writer: u8, seq: u64, bytes: &[u8]) -> SyncChange {
        SyncChange {
            seq: seq as i64,
            path_hash: "1b".repeat(32),
            manifest_hash: None,
            size_bytes: bytes.len() as i64,
            change_type: "put".into(),
            created_at: 0,
            path: None,
            entry: Some(fauna_protocol::ByteBuf::from(bytes.to_vec())),
            origin_writer: Some(hex::encode([writer; 32])),
            origin_seq: Some(seq as i64),
            ..Default::default()
        }
    }

    /// A fake admitted channel: serves the configured pages in order, then
    /// empties. Records how many list calls arrived.
    struct PageServer {
        pages: Mutex<Vec<Vec<SyncChange>>>,
        calls: Mutex<Vec<BTreeMap<String, i64>>>,
    }

    impl RpcRequester for &PageServer {
        type Error = std::convert::Infallible;

        async fn request<Req, Reply>(
            &self,
            _kind: &'static str,
            payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            let req: SyncChangesListRequest =
                fauna_protocol::decode_strict(&fauna_protocol::encode_canonical(&payload).unwrap())
                    .unwrap();
            self.calls
                .lock()
                .unwrap()
                .push(req.frontier.clone().unwrap_or_default());
            let mut pages = self.pages.lock().unwrap();
            let changes = if pages.is_empty() {
                Vec::new()
            } else {
                pages.remove(0)
            };
            let reply = SyncChangesListReply {
                changes,
                ..Default::default()
            };
            Ok(
                fauna_protocol::decode_strict(&fauna_protocol::encode_canonical(&reply).unwrap())
                    .unwrap(),
            )
        }
    }

    async fn keyless_store() -> (tempfile::TempDir, AccountStore<SqliteBackend>) {
        let dir = tempfile::tempdir().unwrap();
        let store = AccountStore::open(
            SqliteBackend::open(dir.path()).unwrap(),
            &hex::encode(OWNER),
            WriterId([0xC5; 32]), // the custodian's own device — never authors
        )
        .await
        .unwrap();
        (dir, store)
    }

    /// The pull keeps rows VERBATIM in the relay plane, journals nothing
    /// (structurally keyless — no merge, no projections), and its durable
    /// cursor makes the next pull incremental, surviving the analogue of a
    /// process restart.
    #[tokio::test]
    async fn a_custody_pull_records_verbatim_and_resumes_from_its_cursor() {
        let (_dir, store) = keyless_store().await;
        let sealed = vec![0xEE; 48]; // sealed bytes — opaque to the custodian
        let server = PageServer {
            pages: Mutex::new(vec![vec![row(0x0A, 1, &sealed), row(0x0B, 1, &sealed)]]),
            calls: Mutex::new(Vec::new()),
        };

        let report = custody_pull(&store, &&server, "state", None).await.unwrap();
        assert_eq!(report.recorded, 2);

        // Verbatim in the relay plane…
        let rows = store
            .relay_rows("state", ItemClass::StateEntry.as_wire(), &[], u32::MAX)
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.entry.as_deref() == Some(&sealed[..])));
        // …and NOTHING in the journal or the merged state (keyless by
        // construction: the custodian holds, never reads).
        assert_eq!(
            store
                .max_held_seq("state", &WriterId([0x0A; 32]))
                .await
                .unwrap(),
            None,
            "a custody pull must never journal"
        );

        // The next pull resumes past what it holds: the served frontier
        // names both writers at seq 1.
        let server2 = PageServer {
            pages: Mutex::new(vec![vec![row(0x0A, 2, &sealed)]]),
            calls: Mutex::new(Vec::new()),
        };
        let report = custody_pull(&store, &&server2, "state", None)
            .await
            .unwrap();
        assert_eq!(report.recorded, 1);
        let asked = server2.calls.lock().unwrap()[0].clone();
        assert_eq!(asked.get(&hex::encode([0x0A; 32])), Some(&1));
        assert_eq!(asked.get(&hex::encode([0x0B; 32])), Some(&1));
    }

    /// The walk's spin refusal holds here too: a non-empty page whose rows
    /// move no cursor is a loud error, never an infinite re-fetch.
    #[tokio::test]
    async fn a_page_that_advances_nothing_refuses_loudly() {
        let (_dir, store) = keyless_store().await;
        // Serve the same row forever: after the first page records it, the
        // second page's cursor cannot move.
        let stuck = row(0x0A, 1, &[0xEE; 8]);
        let server = PageServer {
            pages: Mutex::new(vec![vec![stuck.clone()], vec![stuck.clone()]]),
            calls: Mutex::new(Vec::new()),
        };
        // First page advances (fresh cursor); the repeat page does not.
        let err = custody_pull(&store, &&server, "state", None)
            .await
            .expect_err("a stuck page must refuse");
        assert!(
            err.to_string().contains("advanced no writer cursor"),
            "{err}"
        );
    }

    // ── The nest arm's serve-order watermark ────────────────────────────────

    /// A fake counterpart that echoes: serves its pages in order, each with the
    /// `complete_through_seq` configured beside it, then empty pages repeating
    /// the last echo. Records every request's `(named writers,
    /// held_through_seq)`.
    struct EchoingFeed {
        pages: Mutex<Vec<(Vec<SyncChange>, Option<i64>)>>,
        last_echo: Mutex<Option<i64>>,
        asked: Mutex<Vec<(usize, Option<i64>)>>,
    }

    impl EchoingFeed {
        fn new(pages: Vec<(Vec<SyncChange>, Option<i64>)>) -> Self {
            Self {
                pages: Mutex::new(pages),
                last_echo: Mutex::new(None),
                asked: Mutex::new(Vec::new()),
            }
        }

        fn asked(&self) -> Vec<(usize, Option<i64>)> {
            self.asked.lock().unwrap().clone()
        }
    }

    impl RpcRequester for &EchoingFeed {
        type Error = std::convert::Infallible;

        async fn request<Req, Reply>(
            &self,
            _kind: &'static str,
            payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            let req: SyncChangesListRequest =
                fauna_protocol::decode_strict(&fauna_protocol::encode_canonical(&payload).unwrap())
                    .unwrap();
            self.asked.lock().unwrap().push((
                req.frontier.map_or(0, |frontier| frontier.len()),
                req.held_through_seq,
            ));
            let mut pages = self.pages.lock().unwrap();
            let mut last_echo = self.last_echo.lock().unwrap();
            let (changes, echo) = if pages.is_empty() {
                (Vec::new(), *last_echo)
            } else {
                pages.remove(0)
            };
            *last_echo = echo;
            let reply = SyncChangesListReply {
                changes,
                complete_through_seq: echo,
                ..Default::default()
            };
            Ok(
                fauna_protocol::decode_strict(&fauna_protocol::encode_canonical(&reply).unwrap())
                    .unwrap(),
            )
        }
    }

    /// **The nest arm pages by serve order** (charter § Feeds and cursors →
    /// *Compaction is a serve-order watermark*): the first pull banks the
    /// owner's nest's echo under the store's watermark key — never inside the
    /// shared cursor — and the next pull sends it and names nobody, while the
    /// shared cursor the owner-device arm pages with still holds every writer.
    #[tokio::test]
    async fn the_nest_arm_banks_the_echo_and_stops_naming_writers() {
        let (_dir, store) = keyless_store().await;
        let owner = hex::encode(OWNER);
        let sealed = [0xEE; 16];

        let first = EchoingFeed::new(vec![(
            vec![row(0x0A, 1, &sealed), row(0x0B, 2, &sealed)],
            Some(2),
        )]);
        custody_pull(&store, &&first, "state", Some(&owner))
            .await
            .unwrap();
        assert_eq!(
            first.asked(),
            [(0, None), (0, Some(2))],
            "the echo projects the page's writers out before the next request"
        );
        assert_eq!(store.nest_watermark("state").await.unwrap(), Some(2));

        let second = EchoingFeed::new(vec![(vec![row(0x0C, 3, &sealed)], Some(3))]);
        let report = custody_pull(&store, &&second, "state", Some(&owner))
            .await
            .unwrap();
        assert_eq!(report.recorded, 1);
        assert_eq!(second.asked(), [(0, Some(2)), (0, Some(3))]);
        assert_eq!(store.nest_watermark("state").await.unwrap(), Some(3));

        let shared = stored_cursor(&store, "state").await.unwrap();
        assert_eq!(
            shared.len(),
            3,
            "the shared cursor stays complete for the owner-device arm: {shared:?}"
        );
    }

    /// **The owner-device arm is untouched** — a store-served leg, where the
    /// ruling defers the watermark: a banked watermark is never sent on it, a
    /// counterpart's echo is never taken, and it pages with the shared cursor
    /// whole.
    #[tokio::test]
    async fn the_owner_device_arm_never_sends_or_takes_a_watermark() {
        let (_dir, store) = keyless_store().await;
        store.raise_nest_watermark("state", None, 40).await.unwrap();

        let device = EchoingFeed::new(vec![(vec![row(0x0A, 1, &[0xEE; 8])], Some(99))]);
        custody_pull(&store, &&device, "state", None).await.unwrap();

        assert_eq!(
            device.asked(),
            [(0, None), (1, None)],
            "the whole cursor, grown by the page, and never a watermark"
        );
        assert_eq!(
            store.nest_watermark("state").await.unwrap(),
            Some(40),
            "a store-served leg's echo is never banked"
        );
    }

    /// The ceiling on the custodian's nest arm: one page naming more writers
    /// than a frontier may carry is refused against a nest that never echoes —
    /// today's contract, with nothing checkpointed — and paged through against
    /// one that does, banking the tip while the shared cursor records every
    /// writer.
    #[tokio::test]
    async fn the_nest_arm_pages_past_the_ceiling_only_on_an_echo() {
        let owner = hex::encode(OWNER);
        let writers = crate::page_walk::MAX_FRONTIER_WRITERS as u32 + 1;
        let page: Vec<SyncChange> = (0..writers)
            .map(|i| {
                let mut writer = [0u8; 32];
                writer[..4].copy_from_slice(&i.to_be_bytes());
                SyncChange {
                    origin_writer: Some(hex::encode(writer)),
                    ..row(0x0A, u64::from(i) + 1, &[0xEE; 4])
                }
            })
            .collect();
        let tip = i64::from(writers);

        let (_older_dir, older_store) = keyless_store().await;
        let older = EchoingFeed::new(vec![(page.clone(), None)]);
        let err = custody_pull(&older_store, &&older, "state", Some(&owner))
            .await
            .expect_err("no echo, no projection");
        assert!(err.to_string().contains("past the"), "{err}");
        assert!(
            stored_cursor(&older_store, "state")
                .await
                .unwrap()
                .is_empty(),
            "the unsendable frontier was never checkpointed"
        );

        let (_dir, store) = keyless_store().await;
        let honouring = EchoingFeed::new(vec![(page, Some(tip))]);
        let report = custody_pull(&store, &&honouring, "state", Some(&owner))
            .await
            .expect("the echo pages what the frontier could not carry");
        assert_eq!(report.recorded, writers as usize);
        assert_eq!(
            store.nest_watermark("state").await.unwrap(),
            Some(u64::from(writers))
        );
        assert_eq!(
            stored_cursor(&store, "state").await.unwrap().len(),
            writers as usize
        );
        assert!(honouring.asked().iter().all(|(named, _)| *named == 0));
    }

    /// **A nest that stops honouring a banked watermark is un-banked after
    /// one full re-serve, on the custodian's nest arm too**: reaching
    /// `custody_leg`'s own persisted clear (`:140-146`) and the shared
    /// `page_walk.rs::absorb_reply` fallback neither that walk-arm test nor
    /// `the_nest_arm_banks_the_echo_and_stops_naming_writers` above pulls
    /// through. Bank a watermark against a honouring nest, then roll it back —
    /// same rows, no echo at all, exactly what a pre-watermark image answers.
    /// The first post-rollback pull still opens narrow (the store had no way
    /// to know before asking) and pays one full re-serve, but the reply's
    /// missing echo falls the shared cursor back to whole for the rest of
    /// this pull AND clears the persisted watermark — so the SECOND
    /// post-rollback pull reopens with the whole shared cursor, exactly as an
    /// nest that never echoes was always pulled from, and takes zero rows.
    #[tokio::test]
    async fn the_nest_arm_unbanks_a_rolled_back_nest_after_one_full_re_serve() {
        let (_dir, store) = keyless_store().await;
        let owner = hex::encode(OWNER);
        let sealed = [0xEE; 16];
        let writers: u8 = 3;
        let bank_rows: Vec<SyncChange> = (0..writers).map(|i| row(0x0A + i, 1, &sealed)).collect();

        let honouring = EchoingFeed::new(vec![(bank_rows.clone(), Some(writers as i64))]);
        custody_pull(&store, &&honouring, "state", Some(&owner))
            .await
            .expect("banks a watermark");
        assert_eq!(
            store.nest_watermark("state").await.unwrap(),
            Some(writers as u64)
        );

        // Same rows, no echo at all — a rolled-back nest re-serving from a
        // pre-watermark image.
        let rolled_back = EchoingFeed::new(vec![(bank_rows.clone(), None)]);
        let report = custody_pull(&store, &&rolled_back, "state", Some(&owner))
            .await
            .expect("the first post-rollback pull still converges");
        assert_eq!(
            report.recorded, writers as usize,
            "the stale bank starts the pull narrow, so the rolled-back nest re-serves \
             everyone once"
        );
        assert_eq!(
            store.nest_watermark("state").await.unwrap(),
            None,
            "the unechoed bank is cleared, not left stale for the next pull"
        );
        assert_eq!(
            rolled_back.asked(),
            [(0, Some(writers as i64)), (writers as usize, None)],
            "the opening request is still narrow (the stale bank), and the request after \
             the first non-echoed reply names the whole shared cursor with no watermark: {:?}",
            rolled_back.asked()
        );

        let after_rollback = EchoingFeed::new(vec![]);
        let second = custody_pull(&store, &&after_rollback, "state", Some(&owner))
            .await
            .expect("the second post-rollback pull converges too");
        assert_eq!(
            second.recorded, 0,
            "no re-serve: the pull reopens with the whole shared cursor, matching what the \
             nest already holds"
        );
        assert_eq!(
            after_rollback.asked(),
            [(writers as usize, None)],
            "one request, the whole shared cursor, never a watermark"
        );

        assert_eq!(
            stored_cursor(&store, "state").await.unwrap().len(),
            writers as usize,
            "the shared cursor stays complete for the owner-device arm"
        );
    }

    // ── T15's budget over the real pull path ────────────────────────────────

    fn tombstone_row(writer: u8, seq: u64, bytes: &[u8]) -> SyncChange {
        SyncChange {
            change_type: fauna_protocol::account_state::OP_TOMBSTONE.into(),
            ..row(writer, seq, bytes)
        }
    }

    /// The segment plane is inside the budget: segment
    /// pairs a custodian adopted reach the meter the budget pass judges, a cap
    /// they overrun reads `OverBudget`, and eviction frees whole `.dat` files,
    /// oldest first, until the custody is back under. Mutate: drop the segment
    /// half from `AccountStore::custody_meter` and this reds — the pass reads
    /// `Ok` with the segment bytes invisible.
    #[tokio::test]
    async fn adopted_segments_count_against_the_cap_and_are_evicted_whole() {
        use fauna_account_store::conformance::fixtures::{
            SEG_ACTOR, real_segment, seg_actor_hex, seg_post_scope,
        };
        let dir = tempfile::tempdir().unwrap();
        let store = AccountStore::open(
            SqliteBackend::open(dir.path()).unwrap(),
            &seg_actor_hex(),
            WriterId([0xC5; 32]),
        )
        .await
        .unwrap();
        // The account store's pinned writer output (`PINNED_SEGMENTS`).
        let (dat1, meta1) = real_segment("post", 1, SEG_ACTOR, &[b"p1", b"p2"]);
        let (dat2, meta2) = real_segment("post", 2, SEG_ACTOR, &[b"p3"]);
        for (dat, meta) in [(&dat1, &meta1), (&dat2, &meta2)] {
            store
                .adopt_segment(&seg_post_scope(), dat, meta)
                .await
                .unwrap()
                .unwrap();
        }
        let held = (dat1.len() + meta1.len() + dat2.len() + meta2.len()) as u64;

        let cap = held - 1;
        let outcome = meter_and_evict(&store, Some(cap)).await.unwrap();
        assert_eq!(
            outcome.judged.held_bytes(),
            held,
            "the meter sees the segments"
        );
        assert_eq!(
            outcome.state,
            fauna_core::custody_policy::CustodyBudgetState::OverBudget
        );
        assert_eq!(outcome.evicted.rows, 1, "one whole segment");
        assert_eq!(outcome.evicted.bytes, dat1.len() as u64, "the older .dat");
        assert_eq!(outcome.held_bytes, held - dat1.len() as u64);
        assert!(outcome.held_bytes <= cap, "back under the cap");
    }

    /// The whole T15 contract over bytes a real pull put there: an over-budget
    /// custody evicts **payload only**, oldest first; the coordinate floor and
    /// the tombstone's envelope survive; and the custodian still serves the
    /// same shape afterwards.
    #[tokio::test]
    async fn an_over_budget_custody_evicts_payload_and_keeps_its_floor() {
        let (_dir, store) = keyless_store().await;
        let sealed = vec![0xEE; 100];
        let server = PageServer {
            pages: Mutex::new(vec![vec![
                row(0x0A, 1, &sealed),
                row(0x0B, 2, &sealed),
                tombstone_row(0x0C, 3, &sealed),
            ]]),
            calls: Mutex::new(Vec::new()),
        };
        assert_eq!(
            custody_pull(&store, &&server, "state", None)
                .await
                .unwrap()
                .recorded,
            3
        );

        // 300 held, cap 250 → free 50, which costs exactly the oldest
        // non-tombstone row's whole payload (100 > 50; a row is the eviction
        // grain, so freeing 50 frees the row that covers it).
        let outcome = meter_and_evict(&store, Some(250)).await.unwrap();
        assert_eq!(
            outcome.state,
            fauna_core::custody_policy::CustodyBudgetState::OverBudget
        );
        assert_eq!(
            outcome.judged.held_bytes(),
            300,
            "judged against what was held"
        );
        assert_eq!(outcome.judged.floor_bytes(), 100, "the tombstone is floor");
        assert_eq!(
            outcome.held.held_bytes(),
            200,
            "and re-measured after eviction, not subtracted from a stale snapshot"
        );
        assert_eq!(outcome.evicted.rows, 1);
        assert_eq!(outcome.evicted.bytes, 100);
        assert_eq!(outcome.held_bytes, 200);
        assert_eq!(outcome.unreclaimable, 0);

        let rows = store
            .relay_rows("state", ItemClass::StateEntry.as_wire(), &[], u32::MAX)
            .await
            .unwrap();
        assert_eq!(rows.len(), 3, "every coordinate row is still held");
        let seq_of = |w: u8| rows.iter().find(|r| r.writer.0 == [w; 32]).unwrap();
        assert_eq!(seq_of(0x0A).entry, None, "oldest payload evicted");
        assert_eq!(seq_of(0x0A).writer_seq, 1, "its coordinate untouched");
        assert!(seq_of(0x0B).entry.is_some());
        assert!(
            seq_of(0x0C).entry.is_some(),
            "T15: a tombstone's envelope is always-present floor"
        );
    }

    /// Under budget the pass is a no-op — it must not evict "just in case".
    #[tokio::test]
    async fn a_custody_within_budget_evicts_nothing() {
        let (_dir, store) = keyless_store().await;
        let server = PageServer {
            pages: Mutex::new(vec![vec![row(0x0A, 1, &[0xEE; 40])]]),
            calls: Mutex::new(Vec::new()),
        };
        custody_pull(&store, &&server, "state", None).await.unwrap();

        let outcome = meter_and_evict(&store, Some(1000)).await.unwrap();
        assert_eq!(
            outcome.state,
            fauna_core::custody_policy::CustodyBudgetState::Ok
        );
        assert_eq!(outcome.evicted.bytes, 0);
        assert_eq!(outcome.held_bytes, 40);
    }

    /// An ABSENT cap (`None`) is not a zero budget — it must never be read as
    /// "evict this custody's entire payload". A row carrying no accepted cap
    /// decodes its `#[serde(default)]` field as 0 (0 means default), which
    /// the device leg's call site translates to `None`; it falls back to the
    /// ceremony default and evicts nothing.
    #[tokio::test]
    async fn an_absent_cap_is_not_an_evict_everything_order() {
        let (_dir, store) = keyless_store().await;
        let server = PageServer {
            pages: Mutex::new(vec![vec![row(0x0A, 1, &[0xEE; 64])]]),
            calls: Mutex::new(Vec::new()),
        };
        custody_pull(&store, &&server, "state", None).await.unwrap();

        let outcome = meter_and_evict(&store, None).await.unwrap();
        assert_eq!(
            outcome.state,
            fauna_core::custody_policy::CustodyBudgetState::Ok
        );
        assert_eq!(
            outcome.evicted.bytes, 0,
            "a defaulted row keeps its payload"
        );
        assert_eq!(outcome.held_bytes, 64);
    }

    /// The counterpart, and the reason this takes an `Option` at all: **`Some(0)` IS a real budget** — "hold the floor, no payload". The
    /// nest's hosting pump squeezes a row's cap to the host's remaining tier
    /// headroom and that legitimately reaches zero; while a bare `0` meant
    /// "absent", such a row silently inherited the 8 GiB default and the bound
    /// failed exactly at its limit.
    #[tokio::test]
    async fn an_explicit_zero_cap_evicts_the_payload_it_may_no_longer_hold() {
        let (_dir, store) = keyless_store().await;
        let server = PageServer {
            pages: Mutex::new(vec![vec![row(0x0A, 1, &[0xEE; 64])]]),
            calls: Mutex::new(Vec::new()),
        };
        custody_pull(&store, &&server, "state", None).await.unwrap();

        let outcome = meter_and_evict(&store, Some(0)).await.unwrap();
        assert_ne!(
            outcome.held_bytes, 64,
            "a zero budget must not leave the payload in place — that is the \
             regression the Option signature exists to prevent"
        );
        assert_eq!(
            outcome.cap, 0,
            "and it must be reported as a zero cap, not as the 8 GiB default"
        );
    }

    /// An all-tombstone custody over its cap reports `AtFloor` with a non-zero
    /// unreclaimable overage and evicts nothing — the state that must reach a
    /// receipt rather than be absorbed into a false "healthy".
    #[tokio::test]
    async fn an_all_floor_custody_over_cap_reports_at_floor() {
        let (_dir, store) = keyless_store().await;
        let server = PageServer {
            pages: Mutex::new(vec![vec![
                tombstone_row(0x0A, 1, &[0xEE; 80]),
                tombstone_row(0x0B, 2, &[0xEE; 80]),
            ]]),
            calls: Mutex::new(Vec::new()),
        };
        custody_pull(&store, &&server, "state", None).await.unwrap();

        let outcome = meter_and_evict(&store, Some(100)).await.unwrap();
        assert_eq!(
            outcome.state,
            fauna_core::custody_policy::CustodyBudgetState::AtFloor
        );
        assert_eq!(outcome.evicted.bytes, 0, "the floor is never eaten");
        assert_eq!(outcome.unreclaimable, 60);
        assert_eq!(outcome.held_bytes, 160, "still holding, honestly over cap");
    }

    /// Eviction does not break the pull's resumption: the cursor is META, not
    /// derived from held payload, so the next pull still asks from where it
    /// left off rather than re-fetching the evicted rows forever.
    #[tokio::test]
    async fn eviction_does_not_rewind_the_pull_cursor() {
        let (_dir, store) = keyless_store().await;
        let server = PageServer {
            pages: Mutex::new(vec![vec![row(0x0A, 1, &[0xEE; 100])]]),
            calls: Mutex::new(Vec::new()),
        };
        custody_pull(&store, &&server, "state", None).await.unwrap();
        let outcome = meter_and_evict(&store, Some(10)).await.unwrap();
        assert_eq!(outcome.evicted.rows, 1);

        let next = PageServer {
            pages: Mutex::new(vec![vec![row(0x0A, 2, &[0xEE; 10])]]),
            calls: Mutex::new(Vec::new()),
        };
        custody_pull(&store, &&next, "state", None).await.unwrap();
        let asked = next.calls.lock().unwrap()[0].clone();
        assert_eq!(
            asked.get(&hex::encode([0x0A; 32])),
            Some(&1),
            "the cursor survived eviction — no re-pull from zero"
        );
    }

    /// T15's "eviction is always receipt-visible", end to end over real bytes:
    /// a custody that dropped payload mints a receipt that says so, signed by
    /// the custodian's device principal and verifying under the key the owner's
    /// grant named. This is the assertion that makes the honest-failure-mode
    /// rule checkable rather than aspirational.
    #[tokio::test]
    async fn an_eviction_reaches_the_owner_through_a_signed_receipt() {
        let (_dir, store) = keyless_store().await;
        let server = PageServer {
            pages: Mutex::new(vec![vec![
                row(0x0A, 1, &[0xEE; 100]),
                row(0x0B, 2, &[0xEE; 100]),
            ]]),
            calls: Mutex::new(Vec::new()),
        };
        custody_pull(&store, &&server, "state", None).await.unwrap();

        let outcome = meter_and_evict(&store, Some(150)).await.unwrap();
        assert_eq!(outcome.evicted.bytes, 100);

        let custodian = fauna_core::identity::ActorKeypair::from_secret([0xC5; 32]);
        let receipt = outcome.receipt(
            vec![0x1D; 16],
            OWNER,
            custodian.actor_id().0,
            fauna_core::data::Timestamp(1_700_000_000),
        );
        assert_eq!(receipt.evicted_bytes, 100, "the drop is IN the attestation");
        assert!(receipt.is_degraded(), "and reads as degraded redundancy");
        assert_eq!(
            receipt.held_bytes, 100,
            "coverage reports what is held now, re-measured"
        );
        assert_eq!(receipt.retained_bytes_cap, 150, "against the host's bound");
        assert_eq!(receipt.owner, OWNER);

        // And it survives the wire: signed by the device principal, verified
        // against exactly the key a grant would have named.
        let env = fauna_core::custody_receipt::sign_custody_receipt(&custodian, &receipt).unwrap();
        let back =
            fauna_core::custody_receipt::verify_custody_receipt(&env, &custodian.actor_id().0)
                .unwrap();
        assert_eq!(back, receipt);
    }

    /// The other half of the same rule: a healthy custody's receipt is NOT
    /// degraded, so the signal means something when it does fire.
    #[tokio::test]
    async fn a_within_budget_custody_mints_an_undegraded_receipt() {
        let (_dir, store) = keyless_store().await;
        let server = PageServer {
            pages: Mutex::new(vec![vec![row(0x0A, 1, &[0xEE; 40])]]),
            calls: Mutex::new(Vec::new()),
        };
        custody_pull(&store, &&server, "state", None).await.unwrap();

        let outcome = meter_and_evict(&store, Some(1000)).await.unwrap();
        let receipt = outcome.receipt(
            vec![0x1D; 16],
            OWNER,
            [0xC5; 32],
            fauna_core::data::Timestamp(1),
        );
        assert!(!receipt.is_degraded());
        assert_eq!(receipt.held_bytes, 40);
        assert_eq!(receipt.covered.len(), 1);
        assert_eq!(receipt.covered[0].scope, "state");
        assert_eq!(receipt.covered[0].rows, 1);
    }

    /// An `AtFloor` custody's receipt carries the overage it cannot free — the
    /// owner learns the custodian is permanently short, rather than reading a
    /// plausible held-bytes number and assuming coverage is fine.
    #[tokio::test]
    async fn an_at_floor_receipt_carries_the_unreclaimable_overage() {
        let (_dir, store) = keyless_store().await;
        let server = PageServer {
            pages: Mutex::new(vec![vec![tombstone_row(0x0A, 1, &[0xEE; 200])]]),
            calls: Mutex::new(Vec::new()),
        };
        custody_pull(&store, &&server, "state", None).await.unwrap();

        let outcome = meter_and_evict(&store, Some(50)).await.unwrap();
        let receipt = outcome.receipt(
            vec![0x1D; 16],
            OWNER,
            [0xC5; 32],
            fauna_core::data::Timestamp(1),
        );
        assert_eq!(receipt.unreclaimable_bytes, 150);
        assert_eq!(receipt.evicted_bytes, 0);
        assert!(receipt.is_degraded());
    }

    /// Coverage-enumeration ruling (charter § The custody grant + ceremony):
    /// the `Account` form's covered content set is a pure function of the
    /// witness's own `owner` field — the custodian derives it locally, so an
    /// `Account`-form custody pulls the own-actor content planes with zero
    /// wire disclosure, and never names a co-authored plane.
    #[cfg(feature = "account-runtime")]
    #[test]
    fn account_form_witness_names_the_own_actor_content_scopes() {
        let owner = fauna_core::identity::ActorKeypair::from_secret([0x5A; 32]);
        let witness = fauna_core::custody_grant::sign_custody_grant(
            &owner,
            &fauna_core::custody_grant::CustodyGrant {
                grant_id: vec![7; fauna_core::custody_grant::CUSTODY_GRANT_ID_LEN],
                owner: owner.actor_id(),
                custodian_key: [0xC3; 32],
                scopes: fauna_core::custody_grant::CustodyScopeSet::Account,
                minted_at: fauna_core::data::Timestamp(1_000),
                expires_at: fauna_core::data::Timestamp(2_000),
                removed_devices: Vec::new(),
            },
        )
        .expect("sign witness");
        let scopes = pullable_scopes(&witness).expect("pullable");
        assert_eq!(
            scopes.account_state,
            vec![
                ACCOUNT_STATE_SCOPE.to_string(),
                ACCOUNT_STATE_FLEET_SCOPE.to_string()
            ]
        );
        let derived =
            crate::scope_set::derive_own_actor_scopes(owner.actor_id().0).expect("derive");
        assert!(!derived.is_empty());
        assert_eq!(scopes.content, derived);
        assert!(
            scopes
                .content
                .iter()
                .all(|cs| !fauna_protocol::scope::is_co_authored_scope(&cs.to_string())),
            "the Account form must never name a co-authored plane"
        );
    }

    /// The check-in mint (W8.7 arc 2) — pure-clock tier_1 over
    /// [`mint_due_receipts`]'s gates and record writes, over the shared
    /// ceremony-state fake (which joins every write as the handle's door
    /// does); the real door and the channel post are the tier_3's job.
    #[cfg(feature = "account-runtime")]
    mod mint_tests {
        use super::super::{CustodyBudgetOutcome, CustodyPassOutcome, mint_due_receipts};
        use fauna_account_store::backend::RelayEvicted;
        use fauna_client_capabilities::custody_ceremony::CeremonyRecords;
        use fauna_client_config::test_helpers::FakeCustodyCeremonyStore as MemDoor;
        use fauna_core::custody_ceremony::{CustodyAccept, HeldCustody, sign_custody_accept};
        use fauna_core::custody_policy::{CustodyBudgetState, CustodyMeter, ScopeMeter};
        use fauna_core::custody_receipt::{
            CUSTODY_RECEIPT_INTERVAL_MICROS, verify_custody_receipt,
        };
        use fauna_core::data::Timestamp;
        use fauna_core::device_endpoints::DeviceEndpoints;
        use fauna_core::identity::ActorKeypair;

        const GRANT: [u8; 16] = [0x4A; 16];
        const DEVICE_SECRET: [u8; 32] = [0xC5; 32];

        /// A held ceremony record whose accept binds `DEVICE_SECRET`'s key.
        fn door_with_bound_record() -> MemDoor {
            let host = ActorKeypair::from_secret([0x99; 32]);
            let device_key = ActorKeypair::from_secret(DEVICE_SECRET).actor_id().0;
            let accept = CustodyAccept {
                grant_id: GRANT.to_vec(),
                offer_digest: [0u8; 32],
                host: host.actor_id(),
                custodian_key: device_key,
                custodian_endpoints: DeviceEndpoints {
                    node_id: device_key,
                    ..Default::default()
                },
                retained_bytes_cap: 1_000,
                narrowed_scopes: None,
                accepted_at: Timestamp(1),
                custodian_nest_url: None,
            };
            let env = sign_custody_accept(&host, &accept).expect("sign accept");
            let accept_bytes = fauna_core::encoding::canonical_encode(&env)
                .expect("encode accept")
                .to_vec();
            let door = MemDoor::empty();
            door.mutate(|c| {
                c.held.push(HeldCustody {
                    grant_id: GRANT.to_vec(),
                    owner: [0xA1; 32],
                    channel_hex: "cc".repeat(32),
                    accept: accept_bytes,
                    ..Default::default()
                })
            });
            door
        }

        fn pass(held_bytes: u64, evicted_bytes: u64) -> Vec<CustodyPassOutcome> {
            let meter = CustodyMeter {
                scopes: vec![ScopeMeter {
                    scope: "state".into(),
                    item_class: "state-entry".into(),
                    rows: 3,
                    payload_bytes: held_bytes,
                    ..Default::default()
                }],
            };
            vec![CustodyPassOutcome {
                grant_id: GRANT.to_vec(),
                owner: [0xA1; 32],
                outcome: CustodyBudgetOutcome {
                    judged: meter.clone(),
                    held: meter,
                    state: if evicted_bytes > 0 {
                        CustodyBudgetState::OverBudget
                    } else {
                        CustodyBudgetState::Ok
                    },
                    evicted: RelayEvicted {
                        rows: u64::from(evicted_bytes > 0),
                        bytes: evicted_bytes,
                    },
                    held_bytes,
                    unreclaimable: 0,
                    cap: 1_000,
                },
            }]
        }

        #[tokio::test]
        async fn the_mint_fires_on_its_triggers_and_records_a_verifiable_receipt() {
            let door = door_with_bound_record();
            let key = ed25519_dalek::SigningKey::from_bytes(&DEVICE_SECRET);
            let device_key = key.verifying_key().to_bytes();
            let t0 = Timestamp(1_000_000);

            // First pass: never attested → minted, recorded unposted, and the
            // recorded bytes verify as this device's attestation of THIS pass.
            assert_eq!(mint_due_receipts(&door, &key, &pass(700, 0), t0).await, 1);
            let rec = door.snapshot().await.unwrap().held[0].clone();
            assert!(!rec.receipt.is_empty() && !rec.receipt_posted);
            assert_eq!(rec.receipt_minted_at, t0);
            assert!(!rec.receipt_degraded);
            let env: fauna_core::encoding::EmbedAsBytes =
                fauna_core::encoding::canonical_decode(&rec.receipt).expect("envelope bytes");
            let receipt = verify_custody_receipt(&env, &device_key).expect("verifies as bound");
            assert_eq!(receipt.held_bytes, 700);
            assert_eq!(receipt.attested_at, t0);

            // Same numbers, within the interval → nothing due.
            let t1 = Timestamp(t0.0 + 1);
            assert_eq!(mint_due_receipts(&door, &key, &pass(700, 0), t1).await, 0);

            // An evicting pass → due NOW, and the degraded verdict rides.
            let t2 = Timestamp(t0.0 + 2);
            assert_eq!(mint_due_receipts(&door, &key, &pass(600, 100), t2).await, 1);
            let rec = door.snapshot().await.unwrap().held[0].clone();
            assert!(rec.receipt_degraded && !rec.receipt_posted);
            assert_eq!(rec.receipt_minted_at, t2);

            // Healing (degraded → healthy) is a flip too → due now.
            let t3 = Timestamp(t0.0 + 3);
            assert_eq!(mint_due_receipts(&door, &key, &pass(600, 0), t3).await, 1);
            assert!(
                !door.snapshot().await.unwrap().held[0].receipt_degraded,
                "the owner must stop reading degraded promptly"
            );

            // Steady state within the interval → quiet; past it → the
            // periodic check-in.
            let t4 = Timestamp(t3.0 + 10);
            assert_eq!(mint_due_receipts(&door, &key, &pass(600, 0), t4).await, 0);
            let t5 = Timestamp(t3.0 + CUSTODY_RECEIPT_INTERVAL_MICROS);
            assert_eq!(mint_due_receipts(&door, &key, &pass(600, 0), t5).await, 1);
        }

        #[tokio::test]
        async fn only_the_accept_bound_device_mints() {
            let door = door_with_bound_record();
            let sibling = ed25519_dalek::SigningKey::from_bytes(&[0xC6; 32]);
            assert_eq!(
                mint_due_receipts(&door, &sibling, &pass(700, 0), Timestamp(9)).await,
                0,
                "a fleet sibling that is not the bound custodian must not attest"
            );
            // And an outcome naming an unknown grant finds no record to mint on.
            let key = ed25519_dalek::SigningKey::from_bytes(&DEVICE_SECRET);
            let mut stray = pass(700, 0);
            stray[0].grant_id = vec![0x5B; 16];
            assert_eq!(
                mint_due_receipts(&door, &key, &stray, Timestamp(9)).await,
                0
            );
            assert!(
                door.snapshot().await.unwrap().held[0].receipt.is_empty(),
                "no mint means no record write"
            );
        }
    }

    /// The nest pass's dial-policy gate fires BEFORE any
    /// witness handling or client construction, and a policy-accepted
    /// loopback dev address proceeds past it. Both rows carry an
    /// undecodable witness, so the report discriminates the ordering: the
    /// refused URL lands in `refused_url` (gate first), the accepted one
    /// falls through to the witness decode and lands in `failed` — if the
    /// gate ran after the witness work, both would land in `failed`.
    ///
    /// Feature-gated like everything it touches: `CustodyLegState` and the
    /// whole custodian leg are `#[cfg(feature = "account-runtime")]` (the
    /// feature is NOT default), so an ungated test here does not merely skip
    /// — it fails to COMPILE, taking the crate's entire default-feature lib
    /// test target with it. That is how this one shipped un-run.
    #[cfg(feature = "account-runtime")]
    #[tokio::test]
    async fn nest_pass_refuses_a_policy_violating_owner_url_before_any_dial_work() {
        let store_dir = tempfile::tempdir().unwrap();
        let mut leg = CustodyLegState::new(
            fauna_account_store::root::StoreRoot::at(store_dir.path()),
            ActorId([0xC5; 32]),
            ed25519_dalek::SigningKey::from_bytes(&[0x0D; 32]),
        );

        // The custodian's OWN store, holding two custodies-held rows.
        let own_dir = tempfile::tempdir().unwrap();
        let own = AccountStore::open(
            SqliteBackend::open(own_dir.path()).unwrap(),
            &hex::encode([0xC5; 32]),
            WriterId([0xC5; 32]),
        )
        .await
        .unwrap();
        let kind = fauna_protocol::merge_policy::KIND_CUSTODIES_HELD;
        for (grant_byte, owner_byte, url) in [
            (0x11u8, 0xA1u8, "http://192.168.1.1"), // the finding's canonical target
            (0x22u8, 0xA2u8, "http://127.0.0.1:1"), // accepted local-dev shape
        ] {
            let grant_id = vec![grant_byte; 16];
            own.put_state(fauna_account_store::types::StateEntry {
                kind: kind.into(),
                key: fauna_core::custody_grant::custody_entry_key(&grant_id),
                scope: fauna_protocol::merge_policy::home_scope_for_kind(kind)
                    .unwrap()
                    .into(),
                value: fauna_core::encoding::canonical_encode(
                    &fauna_core::custodies_held::CustodyHeld {
                        grant_id,
                        owner: [owner_byte; 32],
                        witness: vec![0xEE; 8], // non-empty, undecodable
                        owner_devices: Vec::new(),
                        owner_nest_url: Some(url.to_string()),
                        retained_bytes_cap: 1024,
                        ..Default::default()
                    },
                )
                .unwrap()
                .to_vec(),
                merge_meta: None,
                entry_version: 0,
                tombstone: false,
            })
            .await
            .unwrap();
        }

        leg.serve_refresh(&own, None, &Default::default())
            .await
            .unwrap();
        let report = leg.nest_pass().await.unwrap();
        assert_eq!(report.custodies, 2);
        assert_eq!(
            report.refused_url, 1,
            "the policy-violating URL is refused at the gate"
        );
        assert_eq!(
            report.failed, 1,
            "the accepted loopback URL proceeds past the gate (and dies at \
             the garbage witness — proving gate-before-witness ordering)"
        );
    }

    /// The custodian's exclusion map is derived from its own `custodies-held`
    /// rows at the serve refresh — every held witness's owner-signed
    /// removed-device list, unioned per signing account across that
    /// account's grants (an older, empty-listed grant beside a re-minted one
    /// masks nothing), with an unverifiable witness contributing nothing.
    #[cfg(feature = "account-runtime")]
    #[tokio::test]
    async fn the_serve_refresh_derives_the_exclusion_map_from_held_rows_unioned_per_account() {
        let store_dir = tempfile::tempdir().unwrap();
        let mut leg = CustodyLegState::new(
            fauna_account_store::root::StoreRoot::at(store_dir.path()),
            ActorId([0xC5; 32]),
            ed25519_dalek::SigningKey::from_bytes(&[0x0D; 32]),
        );
        let own_dir = tempfile::tempdir().unwrap();
        let own = AccountStore::open(
            SqliteBackend::open(own_dir.path()).unwrap(),
            &hex::encode([0xC5; 32]),
            WriterId([0xC5; 32]),
        )
        .await
        .unwrap();
        let owner = fauna_core::identity::ActorKeypair::from_secret([0x5A; 32]);
        let other = fauna_core::identity::ActorKeypair::from_secret([0x5B; 32]);
        let (a, b, c) = ([0xA0u8; 32], [0xB0u8; 32], [0xC0u8; 32]);
        let kind = fauna_protocol::merge_policy::KIND_CUSTODIES_HELD;
        for (grant_byte, signer, removed, stopped) in [
            (0x11u8, &owner, vec![b], false),
            (0x12u8, &owner, vec![a], false),
            (0x13u8, &owner, vec![], false),
            (0x14u8, &other, vec![c], true),
        ] {
            let grant_id = vec![grant_byte; 16];
            let witness = fauna_core::custody_grant::sign_custody_grant(
                signer,
                &fauna_core::custody_grant::CustodyGrant {
                    grant_id: grant_id.clone(),
                    owner: signer.actor_id(),
                    custodian_key: [0xC3; 32],
                    scopes: fauna_core::custody_grant::CustodyScopeSet::Account,
                    minted_at: fauna_core::data::Timestamp(1_000),
                    expires_at: fauna_core::data::Timestamp(2_000),
                    removed_devices: removed,
                },
            )
            .unwrap();
            own.put_state(fauna_account_store::types::StateEntry {
                kind: kind.into(),
                key: fauna_core::custody_grant::custody_entry_key(&grant_id),
                scope: fauna_protocol::merge_policy::home_scope_for_kind(kind)
                    .unwrap()
                    .into(),
                value: fauna_core::encoding::canonical_encode(
                    &fauna_core::custodies_held::CustodyHeld {
                        grant_id,
                        owner: signer.actor_id().0,
                        witness: fauna_core::encoding::canonical_encode(&witness)
                            .unwrap()
                            .to_vec(),
                        stopped,
                        ..Default::default()
                    },
                )
                .unwrap()
                .to_vec(),
                merge_meta: None,
                entry_version: 0,
                tombstone: false,
            })
            .await
            .unwrap();
        }

        let exclusions = fauna_peer_sync::admission::CustodiedExclusions::default();
        leg.serve_refresh(&own, None, &exclusions).await.unwrap();
        let o = owner.actor_id().0;
        assert!(exclusions.excludes(&o, &a));
        assert!(
            exclusions.excludes(&o, &b),
            "the union across the owner's grants"
        );
        assert!(!exclusions.excludes(&o, &c), "a list binds only its signer");
        assert!(
            exclusions.excludes(&other.actor_id().0, &c),
            "a stopped custody's list is still the owner's signed statement"
        );
    }

    /// The custody-revocation refresh's
    /// failure path. A ledger row the fold refuses is this refresh's error and
    /// leaves the snapshot untouched — underived before any success, so the
    /// view the serve side and the dialer read refuses EVERY custody grant
    /// rather than answering from an empty set (which would admit every
    /// revoked one). An empty log is not a failure: nothing has been seen, so
    /// nothing is revoked, and the snapshot derives empty — T13's honest bound.
    /// And a grant the log revoked is refused once a pass derives it.
    #[cfg(feature = "account-runtime")]
    #[test]
    fn an_unreadable_ledger_leaves_the_revocation_view_refusing_until_one_reads() {
        fauna_client_testkit::block_on(async {
            let store_dir = tempfile::tempdir().unwrap();
            let owner = fauna_core::identity::ActorKeypair::from_secret([0xC5; 32]);
            let actor = owner.actor_id();
            let leg = CustodyLegState::new(
                fauna_account_store::root::StoreRoot::at(store_dir.path()),
                actor,
                ed25519_dalek::SigningKey::from_bytes(&[0x0D; 32]),
            );
            let store = AccountStore::open(
                SqliteBackend::open(store_dir.path()).unwrap(),
                &actor.to_hex(),
                WriterId([0x0D; 32]),
            )
            .await
            .unwrap();
            let snapshot = crate::peer_leg::WithdrawalSnapshot::<Vec<u8>>::underived();
            let view = snapshot.custody_revoked_view();
            let grant = [0x1Du8; 16];
            let put = |key: String, value: Vec<u8>| fauna_account_store::types::StateEntry {
                kind: fauna_protocol::merge_policy::KIND_SUCCESSION_LEDGER.to_string(),
                key,
                scope: ACCOUNT_STATE_FLEET_SCOPE.to_string(),
                value,
                merge_meta: None,
                entry_version: 0,
                tombstone: false,
            };

            store
                .put_state(put(
                    fauna_core::succession_ledger::CHAIN_KEY.to_string(),
                    b"not a chain row".to_vec(),
                ))
                .await
                .unwrap();
            leg.refresh_revoked(&store, &snapshot)
                .await
                .expect_err("a row the fold refuses is the refresh's error");
            assert!(
                !snapshot.is_derived(),
                "a failed first refresh derives nothing"
            );
            assert!(
                view(&grant),
                "an underived revocation view refuses every grant"
            );

            // The owner's log: the custody grant minted, then revoked.
            let mut log = fauna_core::succession_ledger::SuccessionLedger::empty(actor);
            let scopes = fauna_client_capabilities::custody_grants::custody_event_scopes(
                &fauna_core::custody_grant::CustodyScopeSet::Account,
            );
            fauna_client_capabilities::grant_log::record_mint(
                &mut log,
                owner.signing_key(),
                grant,
                [0x2E; 32],
                scopes,
                1,
                2,
                1,
            )
            .unwrap();
            fauna_client_capabilities::grant_log::record_revoke(
                &mut log,
                owner.signing_key(),
                grant,
                [0x2E; 32],
                3,
            )
            .unwrap();
            store
                .put_state(put(
                    fauna_core::succession_ledger::CHAIN_KEY.to_string(),
                    fauna_core::succession_ledger::SuccessionLedgerRecord::Chain(
                        fauna_core::succession_ledger::ChainState::of(actor),
                    )
                    .encode()
                    .unwrap(),
                ))
                .await
                .unwrap();
            for (key, record) in log.rows().unwrap() {
                store
                    .put_state(put(key, record.encode().unwrap()))
                    .await
                    .unwrap();
            }
            assert_eq!(
                leg.refresh_revoked(&store, &snapshot)
                    .await
                    .expect("a readable log is an answer"),
                1
            );
            assert!(view(&grant), "the log's revoked grant stays refused");
            assert!(!view(&[0x3Fu8; 16]), "an id the log never saw is admitted");
        });
    }

    // ── The T15 ingest brake ────────────────────────────────────────

    /// The brake's state machine alone: one metering failure is weather (still
    /// open), the configured consecutive count closes it, one success reopens
    /// and resets, and `at_floor` closes/reopens by what the meter reported.
    #[test]
    #[cfg(feature = "account-runtime")]
    fn the_ingest_brake_closes_on_floor_or_persistent_metering_failure() {
        let brake = IngestBrake::default();
        assert!(brake.admits_ingest(), "a fresh custody ingests");

        brake.record_metered(true);
        assert!(!brake.admits_ingest(), "at floor: accumulation stops");
        brake.record_metered(false);
        assert!(brake.admits_ingest(), "off the floor: pulls resume");

        for i in 0..METERING_FAILURES_BRAKE {
            assert!(
                brake.admits_ingest() || i == METERING_FAILURES_BRAKE,
                "failure #{i} is still weather"
            );
            brake.record_metering_failure();
        }
        assert!(
            !brake.admits_ingest(),
            "{METERING_FAILURES_BRAKE} consecutive metering failures close the ingest side"
        );
        brake.record_metered(false);
        assert!(brake.admits_ingest(), "one successful meter reopens");
    }

    /// The success criterion, driven through the REAL passes: a custody
    /// whose budget pass reports `at_floor` stops pulling on the next pass —
    /// and resumes once the meter clears (here: the owner raises the cap).
    ///
    /// The garbage witness is the observable: an un-braked nest pass reaches
    /// the witness decode and lands in `failed`; a braked one lands in
    /// `braked` and never does the work. Same harness family as the
    /// ordering test above, and feature-gated the same way.
    #[cfg(feature = "account-runtime")]
    #[tokio::test]
    async fn an_at_floor_custody_stops_pulling_until_the_meter_clears() {
        let store_dir = tempfile::tempdir().unwrap();
        let mut leg = CustodyLegState::new(
            fauna_account_store::root::StoreRoot::at(store_dir.path()),
            ActorId([0xC6; 32]),
            ed25519_dalek::SigningKey::from_bytes(&[0x0E; 32]),
        );
        let owner = [0xA7u8; 32];
        let grant_id = vec![0x33u8; 16];

        let own_dir = tempfile::tempdir().unwrap();
        let own = AccountStore::open(
            SqliteBackend::open(own_dir.path()).unwrap(),
            &hex::encode([0xC6; 32]),
            WriterId([0xC6; 32]),
        )
        .await
        .unwrap();
        let kind = fauna_protocol::merge_policy::KIND_CUSTODIES_HELD;
        let put_row = |cap: u64| {
            fauna_account_store::types::StateEntry {
                kind: kind.into(),
                key: fauna_core::custody_grant::custody_entry_key(&grant_id),
                scope: fauna_protocol::merge_policy::home_scope_for_kind(kind)
                    .unwrap()
                    .into(),
                value: fauna_core::encoding::canonical_encode(
                    &fauna_core::custodies_held::CustodyHeld {
                        grant_id: grant_id.clone(),
                        owner,
                        witness: vec![0xEE; 8], // non-empty, undecodable — the observable
                        owner_devices: Vec::new(),
                        owner_nest_url: Some("http://127.0.0.1:1".to_string()),
                        retained_bytes_cap: cap,
                        ..Default::default()
                    },
                )
                .unwrap()
                .to_vec(),
                merge_meta: None,
                entry_version: 0,
                tombstone: false,
            }
        };
        own.put_state(put_row(100)).await.unwrap();

        leg.serve_refresh(&own, None, &Default::default())
            .await
            .unwrap();

        // Seed the custodied pull store with tombstone rows over the cap —
        // 160 unreclaimable bytes against a 100-byte cap (the same floor
        // fixture as `an_all_floor_custody_over_cap_reports_at_floor`).
        {
            let held = leg.held.get(&owner).expect("held");
            let server = PageServer {
                pages: Mutex::new(vec![vec![
                    tombstone_row(0x0A, 1, &[0xEE; 80]),
                    tombstone_row(0x0B, 2, &[0xEE; 80]),
                ]]),
                calls: Mutex::new(Vec::new()),
            };
            custody_pull(&held.pull, &&server, "state", None)
                .await
                .unwrap();
        }

        // Baseline: un-braked, the nest pass does the work (and dies at the
        // garbage witness — the `failed` observable).
        let before = leg.nest_pass().await.unwrap();
        assert_eq!(before.failed, 1, "un-braked: the pull half runs");
        assert_eq!(before.braked, 0);

        // The budget pass reports the floor and engages the brake.
        let budget = leg
            .dial_pass(None, &[], 1_000, None, &Default::default())
            .await
            .unwrap();
        assert_eq!(budget.at_floor, 1, "the meter reports the floor");

        // Braked: accumulation stops — the pull half never runs.
        let braked = leg.nest_pass().await.unwrap();
        assert_eq!(braked.braked, 1, "at floor: the custody stops pulling");
        assert_eq!(braked.failed, 0, "the witness decode was never reached");

        // The owner raises the cap; the (always-running) budget arm meters
        // clean and RELEASES the brake — pulls resume.
        own.put_state(fauna_account_store::types::StateEntry {
            entry_version: 1,
            ..put_row(100_000)
        })
        .await
        .unwrap();
        leg.serve_refresh(&own, None, &Default::default())
            .await
            .unwrap();
        let budget = leg
            .dial_pass(None, &[], 1_000, None, &Default::default())
            .await
            .unwrap();
        assert_eq!(budget.at_floor, 0, "off the floor under the raised cap");
        let after = leg.nest_pass().await.unwrap();
        assert_eq!(after.braked, 0);
        assert_eq!(after.failed, 1, "the pull half runs again");
    }

    // ──  PER_CUSTODY_OWNER_BUDGET's witness ──

    /// `PER_CUSTODY_OWNER_BUDGET`'s elapsed arm must not stop the sequential
    /// `owner_devices` loop: an owner device that blows its budget is counted
    /// `failed`, and the NEXT device in the same custody is still dialed —
    /// the cross-principal wedge propertyclosed for the pull, now
    /// witnessed on this leg too (`peer_leg`'s sibling test below is the
    /// twin).
    ///
    /// `#[tokio::test(start_paused = true)]` turns the 120 s budget into an
    /// instant clock advance (convention 14: no wall-clock wait). The OUTER
    /// `tokio::time::timeout` is the mutation-verification guard: delete the
    /// inner wrapper from `dial_pass` and the slow device's dial hangs with
    /// no timer anywhere else in the test for the paused clock to advance
    /// to — the outer guard is what turns that into a red `.expect()` panic
    /// instead of a hung test.
    #[cfg(feature = "account-runtime")]
    #[tokio::test(start_paused = true)]
    async fn a_stalled_owner_device_does_not_block_the_next_owner_device() {
        struct HangsForOneNode {
            hangs: [u8; 32],
            dialed: Arc<Mutex<Vec<[u8; 32]>>>,
        }

        #[async_trait::async_trait]
        impl fauna_transport::PeerTransport for HangsForOneNode {
            async fn dial(
                &self,
                peer: fauna_transport::EndpointKey,
                _candidates: fauna_transport::PathCandidates,
            ) -> Result<Box<dyn fauna_transport::PeerConn>, fauna_transport::TransportError>
            {
                self.dialed.lock().unwrap().push(*peer.as_bytes());
                if *peer.as_bytes() == self.hangs {
                    // Never resolves — the slow owner device. No timer of its
                    // own; only the (inner, or absent-under-mutation outer)
                    // budget can ever make this `.await` return.
                    std::future::pending::<()>().await;
                    unreachable!("a pending future never resolves");
                }
                Err(fauna_transport::TransportError::NoPath)
            }

            async fn listen(
                &self,
            ) -> Result<fauna_transport::IncomingConns, fauna_transport::TransportError>
            {
                Err(fauna_transport::TransportError::Unsupported)
            }

            fn local_identity(&self) -> fauna_transport::EndpointKey {
                fauna_transport::EndpointKey::from_bytes([0; 32])
            }
        }

        let store_dir = tempfile::tempdir().unwrap();
        let mut leg = CustodyLegState::new(
            fauna_account_store::root::StoreRoot::at(store_dir.path()),
            ActorId([0xC8; 32]),
            ed25519_dalek::SigningKey::from_bytes(&[0x1A; 32]),
        );

        let owner_kp = fauna_core::identity::ActorKeypair::from_secret([0x5B; 32]);
        let owner = owner_kp.actor_id().0;
        let grant_id = vec![0x44u8; 16];
        let witness_env = fauna_core::custody_grant::sign_custody_grant(
            &owner_kp,
            &fauna_core::custody_grant::CustodyGrant {
                grant_id: grant_id.clone(),
                owner: owner_kp.actor_id(),
                custodian_key: [0xC8; 32],
                scopes: fauna_core::custody_grant::CustodyScopeSet::Account,
                minted_at: fauna_core::data::Timestamp(1_000),
                expires_at: fauna_core::data::Timestamp(2_000),
                removed_devices: Vec::new(),
            },
        )
        .expect("sign witness");
        let witness_bytes = fauna_core::encoding::canonical_encode(&witness_env)
            .unwrap()
            .to_vec();

        let slow_node = [0x51u8; 32];
        let fast_node = [0x52u8; 32];

        let own_dir = tempfile::tempdir().unwrap();
        let own = AccountStore::open(
            SqliteBackend::open(own_dir.path()).unwrap(),
            &hex::encode([0xC8; 32]),
            WriterId([0xC8; 32]),
        )
        .await
        .unwrap();
        own.put_state(fauna_account_store::types::StateEntry {
            kind: KIND_CUSTODIES_HELD.into(),
            key: fauna_core::custody_grant::custody_entry_key(&grant_id),
            scope: fauna_protocol::merge_policy::home_scope_for_kind(KIND_CUSTODIES_HELD)
                .unwrap()
                .into(),
            value: fauna_core::encoding::canonical_encode(&CustodyHeld {
                grant_id: grant_id.clone(),
                owner,
                witness: witness_bytes,
                // An ORDERED Vec, both owner devices of the SAME custody —
                // this sidesteps `self.held`'s HashMap iteration order
                // entirely: the property under test is sequencing WITHIN one
                // custody's dial loop, not across custodies.
                owner_devices: vec![
                    DeviceEndpoints {
                        node_id: slow_node,
                        ..Default::default()
                    },
                    DeviceEndpoints {
                        node_id: fast_node,
                        ..Default::default()
                    },
                ],
                owner_nest_url: None,
                retained_bytes_cap: 1_000_000,
                ..Default::default()
            })
            .unwrap()
            .to_vec(),
            merge_meta: None,
            entry_version: 0,
            tombstone: false,
        })
        .await
        .unwrap();

        leg.serve_refresh(&own, None, &Default::default())
            .await
            .unwrap();

        let dialed: Arc<Mutex<Vec<[u8; 32]>>> = Arc::new(Mutex::new(Vec::new()));
        let transport: Arc<dyn fauna_transport::PeerTransport> = Arc::new(HangsForOneNode {
            hangs: slow_node,
            dialed: Arc::clone(&dialed),
        });

        let report = tokio::time::timeout(
            PER_CUSTODY_OWNER_BUDGET * 3,
            leg.dial_pass(Some(&transport), &[], 1_000, None, &Default::default()),
        )
        .await
        .expect(
            "dial_pass must return within an outer budget — if this fires, the inner \
             PER_CUSTODY_OWNER_BUDGET timeout is gone and the slow device hung the whole pass",
        )
        .unwrap();

        assert_eq!(report.custodies, 1);
        assert_eq!(
            report.failed, 2,
            "the elapsed slow device and the fast refusal both count as failed"
        );
        assert_eq!(report.admitted, 0);
        assert_eq!(
            *dialed.lock().unwrap(),
            vec![slow_node, fast_node],
            "the fast device must still be dialed after the slow one blows its budget"
        );
    }
}
