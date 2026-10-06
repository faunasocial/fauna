//! The peer-profile **anchor harvest** — the producer half of the
//! ratification (`identity-succession.md` § The succession statement → *the
//! peer-profile harvest*).
//!
//! A member who joined a group by Welcome holds neither anchor the witness
//! admits: the roster carries no handle and nothing populated the cached-head
//! store. This module closes that from the consumer's side: fetch the peer's
//! profile over **any** transport — the envelope signature, not the transport,
//! carries the trust — and seed the two anchor stores through the one door
//! that enforces the ratified rules ([`PeerAnchors::seed_from_peer_profile_bytes`]:
//! signed-path only, actor match, seed-never-advance).
//!
//! **The sweep attempts every Fauna participant, named or not** (ratified
//! 2026-09-15, `identity-succession.md` § The succession statement → *which
//! participant handles anchor tier 2*). It used to skip a row whose handle
//! carried an `@` as "already anchoring tier 2", and that was wrong twice
//! over: since the room home started naming Welcome-joined rows, a name is
//! not evidence the owner typed it; and the chain head this seeds is the
//! **offline** tier-1 anchor, which no handle replaces — an owner-typed
//! same-nest peer had no offline path at all. Reach, not the skip, bounds
//! the store: `fauna.profile.get` answers only for identities homed on the
//! member's own nest, so a cross-nest row costs one `NoProfile` round trip
//! per session and settles.
//!
//! **Rule 4 is the caller's**: run this on ordinary read paths (a group join,
//! a roster refresh, a profile page) — **never** from a verification path. A
//! profile is signed by the identity key, which is exactly what a seed thief
//! holds, so a fetch triggered by the statement being verified would anchor
//! the check on the attacker.
//!
//! Behind the `conversations-witness` feature because that is what carries the
//! store seam this module writes through
//! ([`fauna_conversations::backend::PeerAnchorStore`], the account's
//! `fauna.state.peer-anchors` rows, reached through the manager the sweep
//! already holds).

use fauna_core::data::{PeerAnchorRefusal, PeerAnchorSeed};
use fauna_core::identity::ActorId;
use fauna_protocol::profile::{ProfileGetReply, ProfileGetRequest};
use fauna_protocol::requester::{RpcErrorClass, RpcRequester};

use fauna_conversations::backend::PeerAnchorStore;

/// What one harvest attempt did. Only [`Unreachable`](Self::Unreachable) and
/// [`StoreFailed`](Self::StoreFailed) are worth retrying later in a session;
/// every other arm is settled for these bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarvestOutcome {
    /// A verified profile seeded at least one empty anchor slot.
    Seeded(PeerAnchorSeed),
    /// A verified profile, but nothing new — the slots were already filled
    /// (the ordinary re-harvest case) or the profile carries neither a
    /// `recovery_head` nor a usable `nests` entry (a peer with no kit has no
    /// chain to anchor; correct, not a failure).
    NothingNew,
    /// The nest answered a rejection — `fauna.profile.not_found` for an
    /// identity not homed there, or any other refusal. Honest absence at this
    /// source; another source may still serve the profile.
    NoProfile,
    /// The bytes were refused by the one-door seeding gate (unsigned legacy
    /// shape, wrong actor, undecodable). Nothing was seeded.
    Refused(PeerAnchorRefusal),
    /// Transport fault before any reply — the one arm where retrying later
    /// can help.
    Unreachable,
    /// The anchor store could not be read or written — or is still not lent
    /// past [`UNLENT_STORE_GRACE_PASSES`]; the harvest is lost for now and
    /// a later read path repeats it (idempotent by construction —
    /// seeding is fill-empty-only).
    StoreFailed,
    /// The write **reported success** and an immediate re-read could not find
    /// the seed.
    ///
    /// A store whose write silently does not stick is a completely different
    /// failure from one that refuses the write, and only a read-back can tell
    /// them apart — the same lesson `SecretStore::set` taught (an
    /// infallible-looking success proves the call was made and nothing more).
    /// Retryable: the next tick tries again, because the alternative is a
    /// member who is permanently un-anchored while every log line says the
    /// harvest succeeded.
    SeedLost,
}

/// One peer's harvest history this session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarvestLogEntry {
    pub actor: ActorId,
    /// How many times the sweep called [`harvest_peer_anchor`] for this peer.
    /// More than one means a retryable arm was hit (`Unreachable`,
    /// `StoreFailed`); zero entries for a peer means the sweep never reached
    /// them at all, which is a *different* bug from any outcome below.
    pub attempts: u64,
    pub last: HarvestOutcome,
}

/// What an app's harvest sweep did, per peer — the **producer half** of the
/// member-path diagnosis.
///
/// Its consumer half ([`crate::witness::ChainWitness::observation`]) can only
/// ever report the absence of an anchor, never why: a head that was never
/// mirrored into the peer's profile, an anchor store that could not be
/// written, a sweep that never ran and a seed that did not survive the round
/// trip all read as one `no_anchor`. This log names which, and the four
/// answers have four different owners. Session-scoped and shared, so an app's
/// sweep owes only `record` — every app inherits the report with the sweep
/// (`identity-succession.md` § The succession statement → *the peer-profile
/// harvest*).
#[derive(Debug, Default)]
pub struct HarvestLog {
    entries: std::sync::Mutex<std::collections::HashMap<ActorId, HarvestLogEntry>>,
}

impl HarvestLog {
    /// Fold one [`harvest_peer_anchor`] result in. The outcome is *last*, not
    /// first: a retried peer's current state is what a reader needs, and the
    /// attempt count is what preserves the fact that it took more than one.
    pub fn record(&self, actor: &ActorId, outcome: HarvestOutcome) {
        let mut entries = self.entries.lock().expect("lock poisoned");
        let entry = entries.entry(*actor).or_insert(HarvestLogEntry {
            actor: *actor,
            attempts: 0,
            last: outcome,
        });
        entry.attempts += 1;
        entry.last = outcome;
    }

    /// Every peer the sweep has attempted, sorted by actor id so a driver's
    /// assertion is order-independent.
    pub fn entries(&self) -> Vec<HarvestLogEntry> {
        let entries = self.entries.lock().expect("lock poisoned");
        let mut out: Vec<HarvestLogEntry> = entries.values().cloned().collect();
        out.sort_by_key(|e| e.actor.to_hex());
        out
    }
}

/// Fetch `peer`'s profile via `requester` (`fauna.profile.get`) and seed the
/// account's anchor store.
///
/// `requester` may be **any** transport that can reach a copy of the profile —
/// the member's own authenticated nest connection (the same-nest case, the one
/// that works today) or an anonymous dial to a channel-delivered URL (those
/// URLs are disqualified as *anchors* but admissible as *transport*, because
/// the envelope verification inside the seeding gate is what carries the
/// trust). Idempotent and safe to call unconditionally: an already-anchored
/// peer costs one fetch and writes nothing — unless its profile now claims a
/// head **past** the one held, which demotes the held head exactly once
/// (`PeerAnchorSeed::marked_outrun`; `identity-succession.md` § The succession
/// statement → *What a held head may settle offline*). That re-read of an
/// anchored peer, every session, is what carries an owner's kit rotation to
/// the members who anchored them before it.
pub async fn harvest_peer_anchor<R>(
    requester: &R,
    peer: &ActorId,
    store: &dyn PeerAnchorStore,
) -> HarvestOutcome
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    let reply: ProfileGetReply = match requester
        .request(
            "fauna.profile.get",
            ProfileGetRequest {
                actor_id: peer.to_hex(),
                extra: Default::default(),
            },
        )
        .await
    {
        Ok(reply) => reply,
        Err(e) if e.is_rejection() => return HarvestOutcome::NoProfile,
        Err(e) => {
            tracing::debug!(
                actor = %peer.to_hex(),
                error = %e,
                "peer-anchor harvest could not reach the profile source"
            );
            return HarvestOutcome::Unreachable;
        }
    };

    seed_profile_bytes(peer, &reply.body, store).await
}

/// Seed the anchor stores from profile `bytes` the caller **already holds** —
/// the page-read arm of rule 4: a profile page fetched the bytes to render
/// them, so harvesting them costs no second fetch. Same one-door seeding gate,
/// same rules, same read-back verification as [`harvest_peer_anchor`], which
/// delegates here after its own fetch.
///
/// Two caller duties travel with it: run it on **read paths only** (rule 4 is
/// unenforceable from here), and — when a conversations session is live —
/// follow a [`HarvestOutcome::Seeded`] with the parked-statement re-drive,
/// exactly as the roster sweep does (`identity-succession.md`'s re-drive
/// rules: the announcement is what keeps the witness's once-per-seed-
/// generation anchor read fresh).
pub async fn seed_profile_bytes(
    peer: &ActorId,
    bytes: &[u8],
    store: &dyn PeerAnchorStore,
) -> HarvestOutcome {
    let mut anchors = match store.peer_anchors().await {
        Ok(anchors) => anchors,
        Err(e) => {
            tracing::debug!(error = %e, "peer-anchor harvest could not read the anchor store");
            return HarvestOutcome::StoreFailed;
        }
    };
    // The door's rules — rule 5's ceiling included — run against the FOLDED
    // anchors, so the count a seed is refused at is the one the store holds.
    let seed = match anchors.seed_from_peer_profile_bytes(peer, bytes) {
        Ok(seed) => seed,
        Err(refusal) => {
            tracing::debug!(
                actor = %peer.to_hex(),
                ?refusal,
                "peer-anchor harvest refused the served profile bytes"
            );
            return HarvestOutcome::Refused(refusal);
        }
    };
    if !seed.changed() {
        return HarvestOutcome::NothingNew;
    }
    // The write is a JOIN on the store thread, and its answer is the store
    // read back AFTER the puts — so the verification below reads what rests,
    // not what was sent.
    let stored = match store.merge_peer_anchors(anchors).await {
        Ok(stored) => stored,
        Err(e) => {
            tracing::debug!(error = %e, "peer-anchor harvest could not persist its seed");
            return HarvestOutcome::StoreFailed;
        }
    };
    // Verify by READ-BACK, not by the write's own return. The anchor this seed
    // becomes is consulted inline on a receive path that cannot investigate,
    // and its absence renders as an ordinary un-re-pointed row — so a write
    // that reports success and does not stick (a ceiling that cut it, a
    // put that never landed) is invisible everywhere else in the system.
    let head_ok = !seed.seeded_head || stored.known_chain_head(peer).is_some();
    let domain_ok = !seed.seeded_domain || stored.known_anchor_domain(peer).is_some();
    // The demotion is read back for the same reason: a mark that does not
    // stick leaves a rotated-away kit settling statements offline while the
    // log reports the head demoted.
    let outrun_ok = !seed.marked_outrun || stored.chain_head_is_outrun(peer);
    if head_ok && domain_ok && outrun_ok {
        HarvestOutcome::Seeded(seed)
    } else {
        tracing::debug!(
            actor = %peer.to_hex(),
            ?seed,
            "peer-anchor harvest wrote a seed the store does not read back"
        );
        HarvestOutcome::SeedLost
    }
}

/// How many times the sweep re-attempts ONE peer after a retryable failure
/// before settling it for the session.
///
/// The retryable arms (`Unreachable`, `StoreFailed`, `SeedLost`) used to be
/// un-counted: a failure simply un-marked the peer, so the sweep re-fetched
/// its profile every [`PEER_ANCHOR_SWEEP_INTERVAL`] for the session's whole
/// life. That is fine for the case the classification was written for — a nest
/// briefly unreachable at app start — and unbounded for the case it was not: a
/// **permanent** refusal — an account store that never assembles or a
/// writer door that refuses while no generation tip resolves — which no amount
/// of retrying will change. The store's refusals arrive as one opaque error
/// string, so the sweep cannot tell a permanent refusal from a disconnect —
/// which is exactly why the
/// bound is a **budget** rather than a classification: no misclassification,
/// present or future, can produce an unbounded loop.
///
/// Five attempts on the ladder below spend ~75 seconds of waiting (1+2+4+8
/// ticks) before settling — long enough to ride out an app-start hiccup, short
/// enough that a peer whose write can never land stops costing a profile fetch
/// every five seconds for the rest of the session. The waits are not the whole
/// bill: an attempt against a nest that stalls rather than refuses also spends
/// its request deadline (5 s), so a down nest settles the sweep after roughly
/// 75 s + 5 × 5 s ≈ 100 s — for the whole roster at once, not per peer, because
/// [`PeerAnchorSweepState::run_pass`] lets a pass's first `Unreachable` answer
/// for the rest of that pass.
pub const MAX_HARVEST_ATTEMPTS: u32 = 5;

/// How many passes the sweep waits for the account's anchor store to be LENT
/// (the account-store-ready edge, which follows login) before it starts
/// charging due peers `StoreFailed`.
///
/// A store not lent yet is not a harvest attempt: the sweep launches first in
/// the receive loop's prologue, and the store resolves a moment later, so
/// charging that window would log a failure for every peer on every launch —
/// and spend the retry budget of the very peers the first real attempt should
/// seed. So a pass with no store does nothing at all (no fetch, no log entry,
/// no ageing) for this many passes. Bounded, never open-ended: an account
/// runtime that never assembles must still settle the harvest wait, so past
/// the grace an unlent store is every due peer's retryable `StoreFailed`,
/// charged to the ordinary budget. Sixteen passes is the retry ladder's own
/// span (1 + 2 + 4 + 8 ticks and the first attempt, ~80 s at
/// [`PEER_ANCHOR_SWEEP_INTERVAL`]). Hard-coded: nobody chooses it.
pub const UNLENT_STORE_GRACE_PASSES: u32 = 16;

/// Ticks to wait before re-attempting a peer that has failed `attempts` times
/// — an exponential ladder in units of [`PEER_ANCHOR_SWEEP_INTERVAL`]:
/// 1, 2, 4, 8 ticks, i.e. 5s → 10s → 20s → 40s between the five attempts.
///
/// Counted in **ticks, not instants**, deliberately: the sweep already wakes on
/// a fixed cadence, so a tick counter needs no clock read and cannot meet the
/// signed-arithmetic trap a wall-clock deadline has to guard against (a
/// `last_attempt` in the future reading as maximally overdue — the bug
/// `fauna_peer::contact::contacts_ready_to_probe` documents at length).
fn backoff_ticks(attempts: u32) -> u32 {
    1u32 << attempts.saturating_sub(1).min(3)
}

/// What the sweep remembers about one peer between ticks.
///
/// Replaces the plain `HashSet` "attempted" marker, which could only say
/// *whether* a peer had been tried — it had no room for how often, so the
/// retryable arms had nowhere to keep a budget.
#[derive(Debug, PartialEq, Eq)]
enum PeerSweepState {
    /// Nothing more to do this session: the peer seeded, was refused, or spent
    /// its [`MAX_HARVEST_ATTEMPTS`] budget.
    Settled,
    /// A retryable failure. Re-attempt once `due_in_ticks` reaches zero.
    Retrying { attempts: u32, due_in_ticks: u32 },
}

/// Where `outcome` leaves a peer that had already spent `attempts_so_far`
/// attempts — the sweep's whole retry policy, as a pure function.
///
/// Pure on purpose: this is the rule that stops an unbounded retry loop, and a
/// rule that can only be exercised by standing up a session, a manager and a
/// nest is a rule with no cheap witness. It also keeps the classification in
/// ONE place, which is what makes the budget's guarantee checkable by reading:
/// every non-retryable arm settles, and every retryable one either steps the
/// ladder or exhausts it.
fn state_after(attempts_so_far: u32, outcome: &HarvestOutcome) -> PeerSweepState {
    match outcome {
        HarvestOutcome::Unreachable | HarvestOutcome::StoreFailed | HarvestOutcome::SeedLost => {
            let attempts = attempts_so_far + 1;
            if attempts >= MAX_HARVEST_ATTEMPTS {
                PeerSweepState::Settled
            } else {
                PeerSweepState::Retrying {
                    attempts,
                    due_in_ticks: backoff_ticks(attempts),
                }
            }
        }
        // `NothingNew`, `NoProfile` and every `Refused(_)` arm — including the
        // count ceiling's `StoreFull` — are standing conditions, not hiccups.
        _ => PeerSweepState::Settled,
    }
}

/// How often the harvest sweep re-diffs the thread snapshot for Fauna
/// participants it has not yet attempted. Cheap on every tick (an in-memory
/// diff; actual harvests run at most once per peer per session), and short
/// enough that a member who just accepted a Welcome is anchored before the
/// ceremony's statement ordinarily arrives.
pub const PEER_ANCHOR_SWEEP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// Where the pass sends a peer whose seed just landed: re-drive the in-group
/// succession statements the witness parked for want of an anchor.
///
/// A seam rather than a direct call because the two drivers hold the receive
/// side by different handles — native has the whole
/// [`ConversationsSession`](fauna_conversations::ConversationsSession) (and
/// upgrades it lazily, so a torn-down session ends the sweep rather than being
/// kept alive across a pass), while web has the raw `FaunaMlsBackend` and
/// manager its JS-driven loop was built around and no session at all. The
/// *decision* — re-drive exactly on `Seeded`, never because a statement asked
/// (rule 4's corollary) — stays in [`PeerAnchorSweepState::run_pass`] where
/// both drivers inherit it.
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait ParkedStatementRedrive: fauna_core::MaybeSendSync {
    /// Re-drive `old_actor`'s parked statements; returns how many rows
    /// re-pointed. A driver whose receive side is already gone answers `0`.
    async fn redrive(&self, old_actor: &ActorId) -> u32;

    /// The pass settled `old_actor` **without seeding anything** — nothing
    /// new, a refusal, or a spent retry budget. Re-drive what was parked
    /// behind the harvest wait, and do NOT announce a seed: none landed, and
    /// the witness's anchor read is bounded per seed generation. Required
    /// rather than defaulted, so a third driver cannot inherit a silent settle
    /// — which would strand every statement that waited on it.
    async fn settled_unseeded(&self, old_actor: &ActorId) -> u32;
}

/// What one driver remembers about its peers **between** passes — the sweep's
/// once-per-peer-per-session guard and its retry ladder, as a value the driver
/// owns rather than a local inside a spawned loop.
///
/// This is the whole of the sweep's policy, and it is deliberately not a
/// `tokio` task: native parks between passes on a timer it spawns, web ticks
/// from the JS-owned receive pump it already runs (there is no `tokio::spawn`
/// on wasm32 and no `ConversationsSession::start_receive_loop` either). Both
/// call [`Self::run_pass`], so the once-per-peer guard, the retryable-arm
/// classification, the backoff ladder and the parked-statement re-drive have
/// exactly ONE implementation — the reason the loop was lifted out of an app
/// layer in the first place, applied one target further out.
#[derive(Default)]
pub struct PeerAnchorSweepState {
    seen: std::collections::HashMap<ActorId, PeerSweepState>,
    /// Passes skipped so far because no anchor store was lent — capped at
    /// [`UNLENT_STORE_GRACE_PASSES`].
    unlent_passes: u32,
}

impl PeerAnchorSweepState {
    /// A driver's fresh sweep state — nothing attempted yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// **One pass**: walk the manager's rosters for Fauna participants — every
    /// one, named or nameless (the module doc owns why) — then the
    /// community-room policy names it wants an anchor for, then the recorded
    /// owner of every folder channel the FaunaMls rail holds a seat on (the
    /// folder commit walk waits on that owner's settle —
    /// `ConversationsManager::harvest_walk_actors` owns the union); harvest
    /// each eligible one, and re-drive whatever its seed unparks.
    ///
    /// Transport-generic on purpose — the fetch rides whatever authenticated
    /// connection the driver already holds (native's `NestClient`, web's
    /// `WsRpcClient`), so the reachable set is peers homed on the same nest and
    /// a cross-nest peer answers `not_found` (an honest absence, retried next
    /// session). The concrete-`NestClient` constraint that shaped the native
    /// driver belongs to `tokio::spawn`'s `Send` bound, not to this pass, which
    /// is why the pass can be shared where the spawn could not.
    ///
    /// The caller holds the manager only across ONE call — never across its
    /// wait — so a re-login's torn-down session releases its manager, and the
    /// MLS engine behind it, at once rather than at the next tick.
    pub async fn run_pass<R>(
        &mut self,
        manager: &fauna_conversations::ConversationsManager,
        requester: &R,
        log: &HarvestLog,
        redrive: &dyn ParkedStatementRedrive,
    ) where
        R: RpcRequester,
        R::Error: RpcErrorClass,
    {
        // The account's anchor store, resolved once per pass through the
        // manager. Not lent yet → the whole pass waits, inside the grace
        // (`UNLENT_STORE_GRACE_PASSES` owns why): nothing is fetched, logged
        // or aged, so the first attempt after the lend is a peer's first.
        let store = manager.peer_anchor_store();
        if store.is_none() && self.unlent_passes < UNLENT_STORE_GRACE_PASSES {
            self.unlent_passes += 1;
            return;
        }
        // Age every waiting peer ONCE per pass, before the thread walk: a
        // peer can sit on several threads, and decrementing inside that
        // walk would charge it one tick per thread it appears on.
        for state in self.seen.values_mut() {
            if let PeerSweepState::Retrying { due_in_ticks, .. } = state {
                *due_in_ticks = due_in_ticks.saturating_sub(1);
            }
        }
        // The rosters themselves, never the conversations page's VIEW of them
        // (`snapshot()` applies the page's search filter, so a query typed
        // into the search box used to narrow which peers were ever harvested
        // — and, since the harvest wait, would hold a hidden peer's statement
        // for as long as the query stood). Then the community-room policy
        // names this device holds no anchor for — a retired owner or admin
        // the member never shared a thread with (identity-succession.md § The
        // succession statement → *a community policy's names join the
        // harvest's walk*). Same door, same grade, same once-per-session
        // guard: only the walk set is wider, and the room backend has already
        // cut it to names an ANCHORED policy version carries.
        // One connection serves every fetch of the pass — the member's own
        // nest — so the first transport failure answers for the rest of it.
        // A fetch that fails for transport has first waited out its whole
        // request deadline (`fauna.profile.get`: 5 s), the pass is serial, and
        // each peer owes five attempts: trying every due peer in turn made a
        // down nest cost ~25 s PER PEER, and the harvest wait — which ends at
        // this budget — scale with the roster for exactly the offline member
        // it exists to serve. Each remaining due peer still steps its own
        // ladder and is logged `Unreachable`, which is what it is; only the
        // fetch that would have said so again is skipped. `StoreFailed` is
        // deliberately not shared: it is the anchor store's failure, and costs no
        // deadline to find out.
        let mut nest_unreachable = false;
        // Past the grace an unlent store is every due peer's `StoreFailed` —
        // retryable, charged to the same budget, and no profile fetched for a
        // seed that could not be kept: a store that never arrives must still
        // settle the harvest wait, exactly as a store write that could never
        // succeed did.
        for actor_id in &manager.harvest_walk_actors() {
            // Named or not, every Fauna row is attempted (the module doc
            // owns why the name-keyed skip was retired): the handle is
            // not consulted here at all.
            // Eligible when never tried, or when a retrying peer's
            // backoff has run out. Marking it `Retrying` with a
            // non-zero wait below is also what keeps it from being
            // re-attempted later in THIS pass, the job the old
            // `HashSet::insert` did.
            let attempts_so_far = match self.seen.get(actor_id) {
                None => 0,
                Some(PeerSweepState::Retrying {
                    attempts,
                    due_in_ticks: 0,
                }) => *attempts,
                Some(_) => continue,
            };
            let outcome = match store.as_deref() {
                None => HarvestOutcome::StoreFailed,
                Some(_) if nest_unreachable => HarvestOutcome::Unreachable,
                Some(store) => harvest_peer_anchor(requester, actor_id, store).await,
            };
            nest_unreachable |= matches!(outcome, HarvestOutcome::Unreachable);
            // Record BEFORE branching: the retryable arms are exactly
            // the ones a reader most needs to see, and a report that
            // only kept settled outcomes would show a peer stuck on an
            // unreachable nest as never attempted at all.
            log.record(actor_id, outcome);
            match outcome {
                HarvestOutcome::Unreachable
                | HarvestOutcome::StoreFailed
                | HarvestOutcome::SeedLost => {
                    let next = state_after(attempts_so_far, &outcome);
                    let budget_spent = next == PeerSweepState::Settled;
                    self.seen.insert(*actor_id, next);
                    if budget_spent {
                        // Budget spent. A later session sweeps afresh,
                        // and the read paths the harvest also runs on
                        // remain — seeding is fill-empty-only, so
                        // stopping loses nothing, it only saves a
                        // profile fetch every tick.
                        tracing::debug!(
                            actor = %actor_id.to_hex(),
                            attempts = MAX_HARVEST_ATTEMPTS,
                            ?outcome,
                            "peer-anchor harvest spent its retry budget for this session"
                        );
                        // The settle arm the offline member reaches, and
                        // the reason the harvest wait ends at the BUDGET
                        // rather than at success: nothing more will be
                        // learned about this peer this session, so a
                        // statement held behind the wait settles now from
                        // the head the member holds. Only here — a retry
                        // still owed is not a settle, and releasing per
                        // attempt would end the wait on a nest that was
                        // merely slow to come up.
                        redrive.settled_unseeded(actor_id).await;
                    }
                }
                // A freshly seeded anchor may settle statements this
                // session already refused for want of one — re-drive
                // them (identity-succession.md § The succession
                // statement → the peer-profile harvest, the re-drive
                // rules: the harvest schedule stays blind to parked
                // statements; only its completion re-drives them).
                HarvestOutcome::Seeded(_) => {
                    self.seen.insert(*actor_id, PeerSweepState::Settled);
                    let repointed = redrive.redrive(actor_id).await;
                    if repointed > 0 {
                        tracing::debug!(
                            actor = %actor_id.to_hex(),
                            repointed,
                            "harvest re-drive settled parked succession statements"
                        );
                    }
                }
                // `NothingNew`, `NoProfile`, every `Refused(_)`: standing
                // conditions, settled at once — and a settle re-drives
                // even though nothing landed, because a statement parked
                // behind the harvest wait is released by the settle
                // itself (same §, *the harvest wait*). Still never
                // because a statement asked: this runs for every peer on
                // the sweep's own order, parked or not.
                outcome => {
                    self.seen
                        .insert(*actor_id, state_after(attempts_so_far, &outcome));
                    tracing::debug!(
                        actor = %actor_id.to_hex(),
                        ?outcome,
                        "peer-anchor harvest settled for this session"
                    );
                    redrive.settled_unseeded(actor_id).await;
                }
            }
        }
    }
}

/// The native driver's re-drive: the live [`ConversationsSession`], upgraded
/// only when a seed actually lands.
///
/// `Weak`, and upgraded per call rather than per pass, so a re-login's
/// torn-down session is released at its own drop — the lifetime the sweep's
/// doc comment promises.
///
/// [`ConversationsSession`]: fauna_conversations::ConversationsSession
#[cfg(not(target_arch = "wasm32"))]
struct SessionRedrive(std::sync::Weak<fauna_conversations::ConversationsSession>);

#[cfg(not(target_arch = "wasm32"))]
#[async_trait::async_trait]
impl ParkedStatementRedrive for SessionRedrive {
    async fn redrive(&self, old_actor: &ActorId) -> u32 {
        match self.0.upgrade() {
            Some(session) => session.redrive_parked_successions(old_actor).await,
            None => 0,
        }
    }

    async fn settled_unseeded(&self, old_actor: &ActorId) -> u32 {
        match self.0.upgrade() {
            Some(session) => session.settle_parked_successions(old_actor).await,
            None => 0,
        }
    }
}

/// The harvest's **native driver**: park on a timer, run
/// [`PeerAnchorSweepState::run_pass`], repeat until the session closes.
///
/// One sweep for every native app rather than one per app layer — the loop, the
/// once-per-peer-per-session guard, the retryable-arm classification and the
/// re-drive are all policy, and policy that lives in an app layer gets
/// re-derived (and re-mis-derived) once per app that adopts it. What stays
/// app-side is the three handles it takes and the runtime it is spawned onto.
/// Since 2026-09-09 the *pass* is shared one level further out
/// ([`PeerAnchorSweepState`]) so web's JS-ticked driver runs the same policy;
/// what remains here is the tokio half — the spawn, the timer, the session's
/// close signal.
///
/// The fetch rides the member's **own authenticated nest connection**, so the
/// reachable set is peers homed on the same nest; a cross-nest peer answers
/// `not_found` (an honest absence, retried next session). Concrete rather than
/// generic over [`RpcRequester`] on purpose: that trait's `request` is an
/// async fn in trait with no `Send` bound, so a generic future cannot be
/// `tokio::spawn`ed at all — the same constraint that puts
/// [`SuccessionChainSource`](crate::SuccessionChainSource) behind a boxed seam.
/// [`harvest_peer_anchor`] and the pass itself stay transport-generic; only
/// this driver, whose transport is always the member's own session, is pinned.
///
/// Ends with the session, at the session's drop: `closed` is the receive loop's
/// own signal ([`ConversationsSession::closed`]), and the manager is held only
/// across ONE pass — never across the sleep — so a re-login's torn-down session
/// releases its manager, and the MLS engine behind it, the moment it is dropped
/// rather than at this sweep's next tick (`account-scoping.md` § Implementation
/// status). `interval` is [`PEER_ANCHOR_SWEEP_INTERVAL`] in production; a
/// release pin may mute it.
///
/// [`ConversationsSession::closed`]: fauna_conversations::ConversationsSession::closed
#[cfg(not(target_arch = "wasm32"))]
pub fn spawn_peer_anchor_harvest_sweep(
    session: std::sync::Weak<fauna_conversations::ConversationsSession>,
    manager: std::sync::Weak<fauna_conversations::ConversationsManager>,
    nest: std::sync::Arc<fauna_client::NestClient>,
    log: std::sync::Arc<HarvestLog>,
    interval: std::time::Duration,
    mut closed: fauna_conversations::SessionClosed,
) {
    tokio::spawn(async move {
        let redrive = SessionRedrive(session);
        let mut state = PeerAnchorSweepState::new();
        loop {
            let Some(manager) = manager.upgrade() else {
                return; // re-login — a fresh sweep rides the fresh session
            };
            state
                .run_pass(manager.as_ref(), nest.as_ref(), log.as_ref(), &redrive)
                .await;
            // The pass is over: let go of the manager BEFORE parking, and park
            // on the session's drop as well as the cadence.
            drop(manager);
            tokio::select! {
                _ = tokio::time::sleep(interval) => {}
                _ = closed.wait() => return,
            }
        }
    });
}

/// The [`PeerAnchorSweepLauncher`] every app injects — the sweep above, behind
/// the seam `fauna-conversations` declares so it can be launched from inside the
/// receive loop's own runtime.
///
/// It holds the three things the sweep needs and this crate can name, and the
/// session's own `Weak` handles are resolved at [`launch`](Self::launch) time
/// rather than construction time: the factory that builds a session cannot hand
/// itself an `Arc` to it, and a strong handle here would be a cycle that
/// outlives the login.
///
/// **Injection is synchronous; the launch is not.** That split is the whole
/// point of the seam — the native FFI session factory is a *sync* UniFFI export
/// with no runtime of its own, so it can construct this but could never
/// `tokio::spawn` from there.
///
/// [`PeerAnchorSweepLauncher`]: fauna_conversations::backend::PeerAnchorSweepLauncher
#[cfg(not(target_arch = "wasm32"))]
pub struct PeerAnchorSweep {
    session: std::sync::Weak<fauna_conversations::ConversationsSession>,
    nest: std::sync::Arc<fauna_client::NestClient>,
    log: std::sync::Arc<HarvestLog>,
    interval: std::time::Duration,
}

#[cfg(not(target_arch = "wasm32"))]
impl PeerAnchorSweep {
    /// `interval` is [`PEER_ANCHOR_SWEEP_INTERVAL`] in production; a release pin
    /// may stretch it to mute the sweep.
    pub fn new(
        session: std::sync::Weak<fauna_conversations::ConversationsSession>,
        nest: std::sync::Arc<fauna_client::NestClient>,
        log: std::sync::Arc<HarvestLog>,
        interval: std::time::Duration,
    ) -> Self {
        Self {
            session,
            nest,
            log,
            interval,
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[async_trait::async_trait]
impl fauna_conversations::backend::PeerAnchorSweepLauncher for PeerAnchorSweep {
    async fn launch(&self) {
        // A session already gone means a re-login raced this launch; the fresh
        // session carries its own launcher, so there is nothing to start.
        let Some(session) = self.session.upgrade() else {
            return;
        };
        spawn_peer_anchor_harvest_sweep(
            std::sync::Weak::clone(&self.session),
            std::sync::Arc::downgrade(&session.manager()),
            std::sync::Arc::clone(&self.nest),
            std::sync::Arc::clone(&self.log),
            self.interval,
            session.closed(),
        );
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod sweep_budget_tests {
    use super::*;
    use fauna_core::data::PeerAnchorRefusal;

    /// The retry STOPS. Drive the retryable arm forever and the ladder settles
    /// after [`MAX_HARVEST_ATTEMPTS`] — the pin on the defect this budget
    /// exists to close.
    ///
    /// Before the budget, `StoreFailed` simply un-marked the peer, so a
    /// permanently refused store write — a permanent `malformed`
    /// reject no retry can change — had the sweep re-fetching that peer's
    /// profile every 5 s for the session's whole life. This walks the same arm
    /// and asserts it terminates.
    #[test]
    fn the_retry_ladder_stops_after_its_budget() {
        let mut attempts = 0u32;
        let mut waits = Vec::new();
        for step in 0..50 {
            match state_after(attempts, &HarvestOutcome::StoreFailed) {
                PeerSweepState::Retrying {
                    attempts: a,
                    due_in_ticks,
                } => {
                    assert_eq!(a, attempts + 1, "each pass spends exactly one attempt");
                    waits.push(due_in_ticks);
                    attempts = a;
                }
                PeerSweepState::Settled => {
                    assert_eq!(
                        attempts + 1,
                        MAX_HARVEST_ATTEMPTS,
                        "the ladder settles on the budget, not before or after"
                    );
                    // 1 + 2 + 4 + 8 ticks of waiting across five attempts.
                    assert_eq!(waits, vec![1, 2, 4, 8], "the ladder is exponential");
                    return;
                }
            }
            assert!(step < 49, "unreachable while the budget is finite");
        }
        panic!("the retry ladder never settled — the unbounded loop is back");
    }

    /// A permanently-failing store cannot outlive its budget even if every
    /// retryable arm alternates — the budget counts attempts, not causes, and
    /// that is exactly why no misclassification can reopen the loop.
    #[test]
    fn the_budget_counts_attempts_rather_than_causes() {
        let arms = [
            HarvestOutcome::Unreachable,
            HarvestOutcome::StoreFailed,
            HarvestOutcome::SeedLost,
            HarvestOutcome::Unreachable,
            HarvestOutcome::StoreFailed,
        ];
        let mut attempts = 0u32;
        for (i, arm) in arms.iter().enumerate() {
            match state_after(attempts, arm) {
                PeerSweepState::Retrying { attempts: a, .. } => {
                    assert!(i + 1 < MAX_HARVEST_ATTEMPTS as usize);
                    attempts = a;
                }
                PeerSweepState::Settled => {
                    assert_eq!(i + 1, MAX_HARVEST_ATTEMPTS as usize);
                    return;
                }
            }
        }
        panic!("five retryable outcomes must exhaust a five-attempt budget");
    }

    /// Every non-retryable outcome settles on the FIRST attempt — a standing
    /// condition is not a hiccup. `StoreFull` is the count ceiling's refusal,
    /// and retrying it would be the same unbounded spin in a new costume.
    #[test]
    fn a_settled_outcome_never_enters_the_ladder() {
        for outcome in [
            HarvestOutcome::NothingNew,
            HarvestOutcome::NoProfile,
            HarvestOutcome::Refused(PeerAnchorRefusal::StoreFull),
            HarvestOutcome::Refused(PeerAnchorRefusal::HostTooLong),
            HarvestOutcome::Refused(PeerAnchorRefusal::Undecodable),
        ] {
            assert_eq!(
                state_after(0, &outcome),
                PeerSweepState::Settled,
                "{outcome:?} is a standing condition, not a retryable one"
            );
        }
    }

    /// The ladder is clamped: it cannot overflow into a wait no session ever
    /// serves, however the budget is later raised.
    #[test]
    fn the_backoff_ladder_is_clamped() {
        assert_eq!(backoff_ticks(1), 1);
        assert_eq!(backoff_ticks(2), 2);
        assert_eq!(backoff_ticks(3), 4);
        assert_eq!(backoff_ticks(4), 8);
        assert_eq!(backoff_ticks(99), 8, "clamped, never shifted off the end");
        assert_eq!(backoff_ticks(0), 1, "saturating_sub keeps attempt 0 sane");
    }
}
