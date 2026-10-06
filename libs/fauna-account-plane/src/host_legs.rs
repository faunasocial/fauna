//! The host seams' shared vocabulary: what the native legs REPORT, and the
//! engine-singleton election every host answers.
//!
//! Ruling (2) of web's account-plane hosting (`account-data-plane.md` § The
//! client-side lifecycle → *The trigger fired*) splits the pump into a
//! platform-generic driver ([`crate::account_driver`]) and a native host. The
//! host supplies the legs the driver runs between the fleet walk and the
//! device-endpoints step — the peer leg (iroh) and the custody leg — through
//! [`crate::account_driver::HostLegs`]; web supplies none. What a pass DID is
//! one report type on every host (`PumpReport`), so the legs' report shapes
//! live here, beside the driver that carries them, while the legs themselves
//! stay in `fauna_sync_engine::{peer_leg, custody_leg}`, which re-export these
//! at their old paths. A host without a leg reports `None` in its slot.
//!
//! The election seam is here for the same reason: the driver re-tries the
//! engine-singleton role on the backstop tick and on `reconcile_now`, and the
//! lock behind it is the host's (`flock` on `<store dir>/engine.lock` natively,
//! the Web Locks API keyed by store name on web —
//! `account-runtime.md` § Multi-instance concurrency → *Election mechanics*).
//! The seed-leg role sits on the same seam, behind a second lock with the
//! same mechanics (`<store dir>/seed-legs.lock`, a second Web Locks name —
//! same section → *The seed-leg role*).

use fauna_account_store::backend::StoreBackend;
use fauna_account_store::store::AccountStore;
use fauna_core::custodies_held::CustodyHeld;
use fauna_core::device_endpoints::DeviceEndpoints;
use serde::{Deserialize, Serialize};

/// The last `fauna.nest.info` facts the peer leg cares about, cached in the
/// store's meta table ([`META_NEST_FACTS`]) so an offline start binds from
/// the last-known advertisement — the offline brake evidence rule 7 names
/// (`p2p.md` § Wormability walk). dag-cbor via the store's canonical
/// encoding; additive at rest. One cache, one owner: the native peer leg
/// writes it on every pass; the driver's handle only re-serves the read
/// (`AccountStoreHandle::cached_nest_capabilities`), so the row's shape lives
/// beside the driver.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerLegNestFacts {
    pub capabilities: Vec<String>,
    pub iroh_relay_url: Option<String>,
}

/// The meta-table key of [`PeerLegNestFacts`].
pub const META_NEST_FACTS: &str = "peer_leg/nest_facts";

/// The cached facts, or `None` for no evidence at all — never fetched, or a
/// cache that does not decode (corrupt cache = no evidence, the brake-on
/// posture, never a crash; the next successful fetch overwrites it).
pub async fn cached_nest_facts<B: StoreBackend>(
    store: &AccountStore<B>,
) -> Option<PeerLegNestFacts> {
    match store.backend().meta_get(META_NEST_FACTS).await {
        Ok(Some(bytes)) => match fauna_core::encoding::canonical_decode(&bytes) {
            Ok(facts) => Some(facts),
            Err(e) => {
                tracing::warn!("peer leg: cached brake unreadable ({e}) — treating as no evidence");
                None
            }
        },
        Ok(None) => None,
        Err(e) => {
            tracing::debug!("peer leg: brake cache unreadable: {e:#}");
            None
        }
    }
}

/// How a host answers an engine-singleton election try
/// ([`EngineElection::try_acquire`]) — the store crate's `EngineLockOutcome`,
/// arm for arm, over the host's own held-role type.
#[derive(Debug)]
pub enum ElectionOutcome<H> {
    /// This runtime now holds the role; it releases when the value drops.
    Held(H),
    /// Another holder has the role — run as a plain reader/writer and re-try
    /// on the backstop cadence.
    Refused,
    /// The lock could not be asked for (an I/O failure on the lock file, a
    /// context without the Web Locks API). The driver owns what that means:
    /// degrade-open at start, stay a non-holder on a re-try.
    Degraded(String),
}

/// The engine-singleton election, as the host runs it: a **try**, never a
/// wait, kernel-arbitrated (or lock-manager-arbitrated) so a crashed holder
/// releases on its own. Natively `EngineLock::try_acquire(&store_dir)`; on
/// web the same call over the Web Locks API, which is why the seam is async.
///
/// The same host answers for the store's other role, the **seed-leg role**
/// ([`Self::try_acquire_seed_legs`]) — a second lock, independent of the
/// first: in the steady state it exists for, a seedless agent holds the engine
/// role and the signed-in app beside it holds this one.
pub trait EngineElection {
    /// A held role — RAII, dropped to release.
    type Held;
    fn try_acquire(&self) -> impl std::future::Future<Output = ElectionOutcome<Self::Held>>;

    /// A held seed-leg role — RAII, dropped to release.
    type SeedLegs;
    /// Try the seed-leg role (`account-runtime.md` § Multi-instance
    /// concurrency → *The seed-leg role*, part 1). The driver asks only for a
    /// seed-holding runtime; a seedless one never takes it.
    fn try_acquire_seed_legs(
        &self,
    ) -> impl std::future::Future<Output = ElectionOutcome<Self::SeedLegs>>;
}

/// What the pump's peer-leg ensure step concluded (the `PumpReport::peer_leg`
/// slot). Failures (factory error, serve-store open, bind refusal) surface in
/// `PumpReport::errors` instead and retry next pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerLegPass {
    /// The node is already up — nothing to do.
    AlreadyBound,
    /// This pass brought the listener up (and composed its facts).
    Bound,
    /// No factory supplied — the leg is structurally off for this caller.
    NoTransport,
    /// The T10 slot carries no enrollment witness — not enrolled yet; a
    /// signed-in ceremony heals it, so the skip is quiet.
    NotEnrolled,
    /// No brake evidence at all: the nest was unreachable and the store has
    /// no cached advertisement. The door refuses by default.
    NoBrakeEvidence,
    /// The best evidence says the nest does not advertise `peer-sync` — the
    /// fleet brake is on.
    BrakeOn,
    /// This device's own participation is off (`crate::p2p_participation`;
    /// `p2p.md` § Per-device participation): no listener, and a node that
    /// was up is dropped this pass — rule 5's "p2p disabled ⇒ no socket".
    ParticipationOff,
}

/// What one dial pass did (the pump's `peer_dial` report slot).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DialPass {
    /// Sibling dial targets the store's device-endpoints entries yielded.
    pub targets: usize,
    /// Siblings that completed mutual admission this pass.
    pub admitted: usize,
    /// Wire rows applied across every scope walked over dialed channels.
    pub applied: usize,
    /// Content blocks fetched over dialed channels (want-list pull).
    pub blocks_fetched: usize,
    /// Wanted content blocks left to the nest path because the dialed
    /// connection ran over a relay while the nest answered this pass
    /// (`fauna_transport::bytes_may_ride`; `p2p.md` § The relay, ruling 4)
    /// — deferred, never failed.
    pub blocks_relay_deferred: usize,
    /// Targets that failed anywhere in dial → admit → walk (absorbed —
    /// per-target isolation; the next pass retries).
    pub failed: usize,
    /// What each admitted peer carried about itself (T13 step 4), keyed by
    /// the channel-proven key. The pump folds these into the
    /// `custodian-endpoints` rows; a sibling's entry is ignored there, its
    /// own fleet-only `device-endpoints` publish being the fresher source.
    pub observed: std::collections::HashMap<[u8; 32], DeviceEndpoints>,
}

/// What the custody serve/dial steps did this pass (the pump's report
/// slots). The revocation snapshot is not this step's: it refreshes earlier
/// in the pass, beside the removed-device one
/// ([`CustodyLegState::refresh_revoked`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CustodyServePass {
    /// Custodied accounts in the serve registry after the refresh.
    pub served: usize,
}

/// The custodian NEST pass's tally. Per-custody isolation: an owner's
/// nest being unreachable, or its custody row revoked, is ordinary weather for
/// every OTHER custody this machine holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CustodyNestPass {
    /// Held custodies whose row names an `owner_nest_url` — the ones this leg
    /// applies to at all.
    pub custodies: usize,
    /// Of those, the ones whose pull completed this pass.
    pub pulled: usize,
    /// Relay rows recorded across those pulls (the account-state legs; a
    /// content walk's own tally rides its `ContentWalkReport`).
    pub recorded: usize,
    /// Custodies that failed anywhere in handshake → pull (absorbed). A
    /// revoked custody lands here every pass, which is the honest reading.
    pub failed: usize,
    /// Custodies whose `owner_nest_url` the dial POLICY refused — a counterparty-supplied URL that fails
    /// `fauna_core::counterparty_url::validate_counterparty_nest_url` never
    /// reaches `connect()`. Distinct from [`Self::failed`] so a policy
    /// refusal is never mistaken for weather.
    pub refused_url: usize,
    /// Custodies the T15 ingest brake skipped this pass: at floor,
    /// or metering persistently failing. Holding and serving continue; only
    /// accumulation stops, and the budget pass's next successful meter
    /// releases it.
    pub braked: usize,
    /// Segment files **adopted** across this pass's bulk legs — the bytes the
    /// coordinate walk's CIDs name.
    ///
    /// Zero is an honest steady state, not a fault: adoption is idempotent, so
    /// a custody that already holds every offered segment reports zero forever
    /// after its first pass. It is also what a custody covering no *reachable*
    /// kind reports — every kind is admissible by
    /// `fauna_account_store::segments::ADOPTABLE_KINDS` since the 2026-08-17
    /// cutover legs, but only `post` and `mail` are also served by the nest.
    pub adopted_segments: usize,
}

/// The custodian dial pass's tally (per-target isolation like the sibling
/// dial pass — an unreachable owner fleet is ordinary weather).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CustodyDialPass {
    /// Custodies with at least one dialable target this pass.
    pub custodies: usize,
    /// Owner devices that completed admission this pass.
    pub admitted: usize,
    /// Relay rows recorded across all custody pulls this pass.
    pub recorded: usize,
    /// Targets that failed anywhere in dial → admit → pull (absorbed).
    pub failed: usize,
    /// Payload bytes held across every custody after this pass's eviction —
    /// what the T15 budget bounds, and what a receipt reports.
    pub held_bytes: u64,
    /// Relay rows whose payload this pass dropped under budget pressure. The
    /// rows themselves are still held: only the sealed envelope went.
    pub evicted_rows: u64,
    /// Payload bytes this pass freed.
    pub evicted_bytes: u64,
    /// Custodies still over budget after evicting every evictable byte — T15's
    /// honest `AtFloor`. Non-zero here is degraded redundancy the owner must be
    /// told about, never absorbed.
    pub at_floor: usize,
    /// Custodies the T15 ingest brake kept from DIALING this pass.
    /// Their budget arm still ran — that is how the brake releases.
    pub braked: usize,
    /// Custodies whose budget arm FAILED to meter this pass. Non-zero is the
    /// fail-open arm's surfacing duty: repeated failure both engages the
    /// brake and must reach the report a human can see.
    pub metering_failed: usize,
    /// Held rows whose `owner_devices` an owner device refreshed over the
    /// admit exchange this pass (T13 step 4). The pump writes them back
    /// through the generation writer door — the dial pass itself never seals.
    pub refreshed: Vec<CustodyHeld>,
    /// Each custody's own budget outcome this pass — what the aggregate
    /// numbers above are summed from, kept per custody because the A7
    /// receipt attests per custody: the pump's mint step
    /// ([`mint_due_receipts`]) reads these, never the sums.
    pub outcomes: Vec<CustodyPassOutcome>,
}

/// One custody's identity plus its budget pass — the mint step's unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustodyPassOutcome {
    /// The custody grant id (16 bytes) — how the mint finds the ceremony
    /// record that holds the channel and the cadence marks.
    pub grant_id: Vec<u8>,
    /// The custodied account — the receipt's addressee-of-record.
    pub owner: [u8; 32],
    pub outcome: CustodyBudgetOutcome,
}

/// What one custody's budget pass did — the numbers the T16 facet renders
/// host-side and the A7 receipt reports owner-side (leg 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustodyBudgetOutcome {
    /// The meter as it stood *before* eviction — what the budget was judged
    /// against. Diagnostic: the receipt reports [`Self::held`].
    pub judged: fauna_core::custody_policy::CustodyMeter,
    /// The meter as it stands **now**, re-measured after any eviction — what
    /// this custodian actually holds, and what its receipt attests to.
    pub held: fauna_core::custody_policy::CustodyMeter,
    pub state: fauna_core::custody_policy::CustodyBudgetState,
    /// What eviction actually freed.
    pub evicted: fauna_account_store::backend::RelayEvicted,
    /// Payload bytes still held after eviction (`held.held_bytes()`).
    pub held_bytes: u64,
    /// Bytes still above the cap once everything evictable was evicted. T15:
    /// reported, never absorbed.
    pub unreclaimable: u64,
    /// The budget this pass judged against — the accepted cap, or the ceremony
    /// default when the row carried none.
    pub cap: u64,
}

impl CustodyBudgetOutcome {
    /// Mint the A7 receipt for this pass.
    ///
    /// The receipt is derived from the pass's own post-eviction meter, which is
    /// what makes T15's "eviction is always receipt-visible" structural rather
    /// than a discipline: there is no path that reports coverage without having
    /// measured it, and none that drops payload without the drop reaching this
    /// number.
    pub fn receipt(
        &self,
        grant_id: Vec<u8>,
        owner: [u8; 32],
        custodian_key: [u8; 32],
        attested_at: fauna_core::data::Timestamp,
    ) -> fauna_core::custody_receipt::CustodyReceipt {
        fauna_core::custody_receipt::CustodyReceipt::from_meter(
            grant_id,
            owner,
            custodian_key,
            &self.held,
            self.cap,
            self.evicted.bytes,
            self.state,
            self.unreclaimable,
            attested_at,
        )
    }
}
