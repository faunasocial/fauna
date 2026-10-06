//! The in-group succession-statement **witness** — the verification half of
//! `identity-succession.md` § Propagation → *MLS groups*.
//!
//! A member that receives `GroupMetaMessage::Succession` holds a **claim**: a
//! seed thief authors a structurally perfect statement, so the carried bytes
//! authorize nothing on their own. [`ChainWitness`] applies § The succession
//! statement's verification rule to that claim and hands the conversations
//! backend either a [`VerifiedSuccession`] (re-point the participant, render
//! continuity) or `None` (render the bare add, the
//! unverified-succession reading).
//!
//! **Why the policy lives here and not in `fauna-conversations`.** The rule's
//! inputs are recovery-plane objects — a registration chain, a chain head, the
//! anchored walk [`crate::resolve_successor`] already implements — and the
//! conversations crate deliberately carries no recovery dependency (the
//! `SchedulingSink` / `FolderCustodySink` seam pattern, priority #2). So the
//! seam is declared there and satisfied here, once, for all 7 apps.
//!
//! **The two things an app still supplies**, because neither can be shared:
//! [`SuccessionAnchors`] (what *this* consumer independently knows about the
//! succeeded identity) and [`SuccessionChainSource`] (how *this* platform
//! reaches a foreign nest — native tokio vs. wasm websocket).
//! [`ThreadParticipantAnchors`] covers the anchors for every app that keeps a
//! conversations thread store, which is all of them.
//!
//! Only the two items that name conversations types — the `SuccessionWitness`
//! impl and [`ThreadParticipantAnchors`] — sit behind the
//! `conversations-witness` feature (the anchors rest their heads on the
//! account plane, reached through the manager's peer-anchor store seam). The
//! policy itself **compiles** under
//! default features, so a build that never enables the feature still type-checks
//! the verification rule; the crate's *tests* always run with the feature on (a
//! self dev-dependency turns it on — see `Cargo.toml`), because an integration
//! test is its own crate and a `#[cfg(feature)]` gate there would silently skip
//! rather than fail.

use std::collections::HashMap;
use std::sync::Mutex;

use fauna_core::MaybeSendSync;
use fauna_core::identity::ActorId;
use fauna_core::recovery::{ChainHead, SignedIdentitySuccession, VerifiedSuccession};
use fauna_protocol::requester::{RpcErrorClass, RpcRequester};

use crate::nest::RecoveryClient;
use crate::succession::resolve_successor;

use async_trait::async_trait;

/// What the consumer **independently knows** about a succeeded identity.
///
/// This trait is the whole of the anchoring contract at this seam, and its
/// read methods are deliberately the only sources § The succession statement
/// admits for a member client: a chain head seen earlier, a handle learned
/// when the peer entered the conversation, and — the 2026-08-10
/// ratification — a home domain harvested from the peer's **own signed
/// profile** on an ordinary read path (§ The succession statement → *the
/// peer-profile harvest*). Nothing here may be derived from the statement
/// being verified — that is the "delivered chain" hole the rule exists to
/// close, and an implementation that reads `statement.recovery_pubkey` or a
/// home-nest URL off the carrier has reopened it.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait SuccessionAnchors: MaybeSendSync {
    /// The chain head this consumer has **previously seen** for `actor` — a
    /// cached `Profile.recovery_head`, a persisted head row, or one
    /// [`Self::remember_head`] recorded on an earlier walk.
    ///
    /// `None` is honest first contact, which § The succession statement grades
    /// TOFU ("strictly better than today, where the thief simply *is* the
    /// user"). It is **never** a reason to accept the statement's own head:
    /// with no anchor the walk still runs against the identity's home nest.
    async fn known_head(&self, actor: &ActorId) -> Option<ChainHead>;

    /// Whether the head [`Self::known_head`] answers has been **outrun** — the
    /// peer's own directly-signed profile, read on an ordinary read path, has
    /// claimed a head past it (`identity-succession.md` § The succession
    /// statement → *What a held head may settle offline*).
    ///
    /// An outrun head is still held and still the walk's rewrite/truncation
    /// guard; what it loses is tier 1. A RecoveryKey the owner rotated away
    /// signs valid statements for ever, so a head naming it would otherwise go
    /// on settling them with no dial at all. Answered from whatever
    /// [`Self::known_head`] already read — **no I/O of its own**, and never a
    /// fetch: the mark is written by the harvest (rule 4), only read here.
    /// Default `false` — additive, so an implementation that holds no such
    /// mark keeps its exact behavior.
    async fn known_head_is_outrun(&self, actor: &ActorId) -> bool {
        let _ = actor;
        false
    }

    /// The handle **the owner's own gesture** named for `actor`, canonical
    /// `localpart@domain` (`login.md` — a handle carries its `@domain`). Its
    /// domain is what locates the old identity's home nest, and it is
    /// independent evidence precisely because the owner chose it — a
    /// recipient they typed and accepted, a member they added — long before
    /// any statement arrived. **Only that provenance answers here**
    /// (`identity-succession.md` § The succession statement → *which
    /// participant handles anchor tier 2*): a name the room home served for a
    /// Welcome-joined row, or one copied off such a row at seat time, is the
    /// channel host's choice of dial target and reads as no handle at all.
    async fn known_handle(&self, actor: &ActorId) -> Option<String>;

    /// The home domain harvested from `actor`'s own signed profile, if any —
    /// the fallback anchor for a member whose roster row carries no
    /// anchor-grade handle (a Welcome-joined roster never does, however the
    /// room home has since named it). Consulted only when
    /// [`Self::known_handle`] answers nothing: an owner-typed handle is the
    /// owner's own gesture and always outranks a harvested self-assertion.
    /// TOFU-grade by ratification (§ The succession statement → *the
    /// peer-profile harvest*); the walk it feeds still verifies the chain and
    /// still passes the held head as the rewrite/truncation guard. Default
    /// `None` — additive, so pre-harvest implementations keep compiling and
    /// keep their exact behavior.
    async fn known_home_domain(&self, actor: &ActorId) -> Option<String> {
        let _ = actor;
        None
    }

    /// What this consumer's durable anchor store looked like at its last read
    /// — reported, never acted on.
    ///
    /// [`Self::known_head`] and [`Self::known_home_domain`] both answer `None`
    /// for two completely different reasons: nothing was ever seeded for this
    /// peer, or the place seeds rest could not be read at all. The second is a
    /// broken (or not yet lent) anchor store and the first is a missing producer, and no
    /// consumer of this trait can tell them apart from the `None`. Default
    /// [`AnchorStoreState::NotRead`] — additive, so existing implementations
    /// keep compiling and simply report nothing.
    ///
    /// **Must not perform I/O.** Implementations report what their last read
    /// already established; the receive path this trait sits on is inline, and
    /// a diagnostic that added a round trip to it would be a regression of
    /// exactly the kind row 52 filed.
    async fn anchor_store_state(&self) -> AnchorStoreState {
        AnchorStoreState::NotRead
    }

    /// Record the head a completed walk established, so the next statement for
    /// this identity is checked against something rather than TOFU'd again.
    ///
    /// Monotonic by contract — an implementation must keep the **higher** `seq`
    /// and never let a later call lower it, since the whole value of a
    /// remembered head is refusing a chain that rewrites or truncates what was
    /// already seen. Default: forget it (a consumer with nowhere to persist
    /// stays at TOFU grade, which is the pre-witness behaviour, not a
    /// regression).
    async fn remember_head(&self, actor: &ActorId, head: ChainHead) {
        let _ = (actor, head);
    }

    /// A peer-anchor harvest seeded something new — invalidate whatever this
    /// implementation caches about what the durable store holds.
    ///
    /// The counterpart of [`Self::anchor_store_state`]'s **must not perform
    /// I/O** rule: an implementation that answers [`Self::known_home_domain`]
    /// from a fresh read every time cannot be driven by an inline receive path,
    /// and one that caches cannot see the harvest — a separate producer writing
    /// the same store through a different handle. This method is how the second
    /// shape stays correct, and it is delivered by the ratified re-drive
    /// (`fauna_conversations::backend::SuccessionWitness::anchor_seed_landed`),
    /// never by a per-app call an app could forget. Default: no-op.
    async fn anchor_seed_landed(&self) {}
}

/// How this platform reaches the **old identity's home nest** and runs the
/// anchored walk there.
///
/// Anonymous by necessity: the two kinds the walk needs
/// (`fauna.recovery.succession.lookup`, `fauna.recovery.registration.chain`)
/// are pre-identity by design, and the verifying member holds no account on the
/// succeeded peer's nest — there is no session it *could* use.
///
/// **Why the walk is behind the seam and not above it.** `RpcRequester::request`
/// is an async fn in trait, deliberately without a `Send` bound so a wasm
/// requester may be `!Send` — which means a generic caller's future is `Send`
/// only once the requester is a *concrete* type. A witness boxed behind
/// `dyn SuccessionWitness` on the multi-threaded native receive loop is exactly
/// such a caller, so the monomorphized step has to sit on the app's side of the
/// boundary. Implementations are three lines of genuinely platform-divergent
/// content — resolve the domain, dial it, hand the requester to [`walk`], which
/// is where all the shared verification lives.
///
/// **Bound the dial.** [`ChainWitness::verify_statement`] is called inline by
/// the inbound poll, so an unreachable anchor must cost bounded time and then
/// degrade — never stall the feed. Implementations own that timeout; the
/// witness owns the promise not to retry.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait SuccessionChainSource: MaybeSendSync {
    /// Resolve + dial the nest serving `handle_domain`, then return
    /// [`walk`]'s verdict for `old`. `None` for unreachable, unresolvable,
    /// never-succeeded, or a chain that fails the rule — every one of which
    /// leaves the statement a claim.
    async fn walk_from_domain(
        &self,
        handle_domain: &str,
        old: ActorId,
        known_head: Option<ChainHead>,
    ) -> Option<VerifiedSuccession>;

    /// [`Self::walk_from_domain`] with every verified hop returned, oldest
    /// first ([`walk_line`]) — for [`ChainWitness::resolve_line`]. `Some` of an
    /// **empty** line is the home nest's verified "never succeeded"; `None` is
    /// everything that establishes nothing (unreachable, unresolvable, a chain
    /// that fails the rule). Same bound-the-dial duty. Default `None` —
    /// additive, so a source that walks no lines resolves none and its
    /// consumers fail closed.
    async fn walk_line_from_domain(
        &self,
        handle_domain: &str,
        old: ActorId,
        known_head: Option<ChainHead>,
    ) -> Option<Vec<VerifiedSuccession>> {
        let _ = (handle_domain, old, known_head);
        None
    }
}

/// [`walk`] with every verified hop returned — the shared body of
/// [`SuccessionChainSource::walk_line_from_domain`]. Same anchoring contract on
/// `requester`.
pub async fn walk_line<R>(
    requester: R,
    old: ActorId,
    known_head: Option<ChainHead>,
) -> Option<Vec<VerifiedSuccession>>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    let client = RecoveryClient::new(requester);
    match crate::succession::resolve_succession_line(&client, old, known_head).await {
        Ok(line) => Some(line),
        Err(e) => {
            tracing::debug!(
                actor = %old.to_hex(),
                error = %e,
                "no succession line is resolved: the anchored walk failed"
            );
            None
        }
    }
}

/// What [`ChainWitness::resolve_line`] made of one identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LineResolution {
    /// The anchored walk verified the line: every successor, oldest first —
    /// empty when the identity's home nest answered that it never succeeded.
    Verified(Vec<ActorId>),
    /// Nothing is established *yet*: this consumer holds no independent anchor
    /// for the identity, or the anchor could not be reached or did not verify.
    NotYet,
}

/// The anchored walk itself — the shared body every [`SuccessionChainSource`]
/// implementation delegates to once it holds a connection.
///
/// `requester` must already be connected to the nest the caller resolved from
/// the identity's **own handle domain**; that is the anchoring contract
/// [`crate::resolve_successor`] documents, and passing a connection obtained
/// any other way (a URL off the statement, the member's own nest) is the
/// "delivered chain" hole § The succession statement forbids.
pub async fn walk<R>(
    requester: R,
    old: ActorId,
    known_head: Option<ChainHead>,
) -> Option<VerifiedSuccession>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    walk_outcome(requester, old, known_head).await.successor()
}

/// What one anchored walk concluded, for a caller that must tell a definitive
/// "never succeeded" from a walk that settled nothing — the calendar's
/// per-session succession memo remembers only the second (`caldav-server.md`
/// § Who may mutate an existing event over the inbound rail → *A succeeded
/// organizer*). [`walk`] folds the last two into `None`, which is all the
/// in-group witness needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnchoredWalk {
    /// The walk verified: where `old` ended up.
    Succeeded(VerifiedSuccession),
    /// The anchor answered, and its answer is that `old` never succeeded — an
    /// empty lookup, the honest answer of a nest that is up.
    NeverSucceeded,
    /// Nothing was settled: the anchor would not dial or timed out, or the walk
    /// failed — a transport error, or a chain that fails the rule.
    Unsettled,
}

impl AnchoredWalk {
    /// The verified successor, if the walk found one.
    pub fn successor(self) -> Option<VerifiedSuccession> {
        match self {
            Self::Succeeded(step) => Some(step),
            Self::NeverSucceeded | Self::Unsettled => None,
        }
    }
}

/// [`walk`] with its two `None`s told apart — see [`AnchoredWalk`]. Same
/// anchoring contract: `requester` is connected to the nest the caller already
/// trusts for `old`.
pub async fn walk_outcome<R>(
    requester: R,
    old: ActorId,
    known_head: Option<ChainHead>,
) -> AnchoredWalk
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    let client = RecoveryClient::new(requester);
    match resolve_successor(&client, old, known_head).await {
        Ok(Some(step)) => AnchoredWalk::Succeeded(step),
        Ok(None) => AnchoredWalk::NeverSucceeded,
        Err(e) => {
            tracing::debug!(
                actor = %old.to_hex(),
                error = %e,
                "no succession is verified: the anchored walk failed"
            );
            AnchoredWalk::Unsettled
        }
    }
}

/// One identity's verdict, remembered for this session.
#[derive(Clone, Copy)]
enum Verdict {
    /// The walk verified; re-delivery re-applies from here with no dial.
    Verified(VerifiedSuccession),
    /// The walk was attempted and could not be completed (unreachable anchor,
    /// empty or hostile chain). Remembered so a re-delivered statement — a
    /// Rule-2 heal re-walk, a resumed sweep's second post — does not re-dial on
    /// every poll. Session-scoped: a relaunch tries again, which is the
    /// recovery path for an anchor that was merely down. ⚠ The **no-anchor**
    /// case is deliberately *not* recorded here (2026-08-10): no dial happened,
    /// and memoizing it would wedge the session against a harvest that
    /// completes after the statement's first delivery.
    Unproven,
}

/// Whether the durable store the anchors rest in could be read, and what was
/// in it — see [`SuccessionAnchors::anchor_store_state`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AnchorStoreState {
    /// No read has been attempted yet, or this implementation does not report.
    #[default]
    NotRead,
    /// The store could not be read. Every anchor answers `None` from here on
    /// for reasons that have nothing to do with what was seeded.
    Unreadable,
    /// The store was read. `heads`/`domains` are what it held **in total**, not
    /// for the peer under test: zero of both on a readable store says the
    /// producer never ran, which is the reading no per-peer `None` can give.
    Read { heads: usize, domains: usize },
}

/// Which arm of the verification rule decided an identity, for a **driver** to
/// read — the outward-facing twin of [`Verdict`], which only has to be enough
/// for the memo.
///
/// The four arms are indistinguishable from anywhere else in the system: a
/// statement that never arrived, one with nothing to anchor on, one whose dial
/// failed and one that verified all leave the same un-re-pointed participant
/// row, and the `debug!` lines that would tell them apart go nowhere in
/// production (no app installs a tracing subscriber). Their remedies are
/// completely different — seed an anchor, reach a nest, fix the poll — so
/// naming the arm is what turns a member-side failure into a one-run
/// diagnosis instead of a rebuild.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WitnessOutcome {
    /// Tier 1 — a head this consumer already held authorized the statement.
    /// No dial, works offline.
    SettledByHeldHead,
    /// Tier 2 — the anchored walk to the identity's own home nest verified it.
    SettledByWalk,
    /// Neither tier could run: no held head authorized the statement, the
    /// roster offered no handle, and no domain had been harvested. **No dial
    /// was made** — the distinguishing fact, and the one that says the missing
    /// piece is a *producer* (the peer-profile harvest), not the network.
    NoAnchor,
    /// A dial happened and did not come back with a verified chain —
    /// unreachable nest, never-succeeded identity, or a chain that failed the
    /// rule. The network, not the anchor.
    WalkFailed,
    /// A held, un-demoted head WOULD have settled it, and the witness held it
    /// back because this session's harvest of that peer has not settled yet
    /// (the harvest wait). No dial, no verdict recorded: the statement is
    /// parked and the sweep's settle re-drives it. A row still reading this
    /// long after login names a sweep that never reached the peer.
    AwaitingHarvest,
}

/// What one identity's statements did to this witness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerWitnessObservation {
    /// The identity the statement named (`old_actor_id`) — the participant row
    /// a verified verdict re-points.
    pub actor: ActorId,
    /// Statements naming this identity this session, memo hits included.
    pub statements_seen: u64,
    /// How many of those were answered from the session memo without re-running
    /// the rule.
    pub memo_hits: u64,
    /// The `seq` of the head held when the rule last actually ran; `None` means
    /// tier 1 had nothing to check against. A `Some` that still did not settle
    /// says the head is *stale*, not missing — a different producer problem.
    pub held_head_seq: Option<u64>,
    /// The held head was marked outrun when the rule last ran, so tier 1 was
    /// skipped on purpose — what separates "the head is stale" from "the head
    /// was demoted" behind one identical fall-through to the walk.
    pub held_head_outrun: bool,
    /// The domain taken from a roster handle, when there was one.
    pub handle_domain: Option<String>,
    /// The domain the peer-profile harvest had seeded, consulted only when the
    /// handle answered nothing (a Welcome-joined roster never has one).
    pub harvested_domain: Option<String>,
    /// The arm that decided this identity. A memo hit never overwrites it —
    /// the decisive answer is the diagnostic one.
    pub outcome: WitnessOutcome,
}

/// Everything [`ChainWitness`] has been asked and answered this session.
///
/// Sorted by actor id so a driver's assertion is order-independent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WitnessObservation {
    /// Every statement handed to the witness this session, across all peers.
    /// **Zero is the load-bearing reading**: it says the member's inbound poll
    /// never delivered a statement at all, which is a different bug entirely
    /// from any verdict below.
    pub statements_seen: u64,
    /// The durable anchor store as of the last time the rule ran — what
    /// separates "nothing was seeded for this peer" from "the store is
    /// unreadable", both of which surface as [`WitnessOutcome::NoAnchor`].
    pub anchor_store: AnchorStoreState,
    pub peers: Vec<PeerWitnessObservation>,
}

/// The shared [`SuccessionWitness`] implementation: cached head first, then the
/// anchored walk to the old identity's home nest.
///
/// [`SuccessionWitness`]: fauna_conversations::backend::SuccessionWitness
pub struct ChainWitness<A, C> {
    anchors: A,
    chain_source: C,
    verdicts: Mutex<HashMap<ActorId, Verdict>>,
    /// [`Self::resolve_line`]'s memo, under the verdict memo's own rule: one
    /// entry per **dial**, so a line costs its walk once a session and the
    /// no-anchor arm — which dialled nothing — records nothing.
    lines: Mutex<HashMap<ActorId, LineResolution>>,
    /// Convention-6 report, session-scoped like the memo beside it. Never read
    /// by the policy — a witness whose behaviour depended on its own report
    /// could not be trusted as evidence about itself.
    observed: Mutex<HashMap<ActorId, PeerWitnessObservation>>,
    /// The anchors' own last-read report, sampled where the rule consults them
    /// (no I/O of its own — see [`SuccessionAnchors::anchor_store_state`]).
    anchor_store: Mutex<AnchorStoreState>,
    /// The harvest wait (`identity-succession.md` § The succession statement →
    /// *the harvest wait*). In memory and session-scoped like everything else
    /// here, and written only by the sweep — never by a statement — so it is
    /// bounded by the member's rosters, not by what a peer sends.
    harvest_wait: Mutex<HarvestWait>,
}

/// Which peers this session's harvest sweep has settled — what a held head
/// waits on before it may settle a statement offline.
#[derive(Default)]
struct HarvestWait {
    /// A sweep runs this session. `false` — no sweep, so nothing would ever
    /// end a wait — keeps the immediate tier 1: a consumer outside any
    /// conversations session (the contact re-point), or a test double.
    armed: bool,
    settled: std::collections::HashSet<ActorId>,
}

/// What a held head did with one statement.
enum HeldHead {
    /// It authorizes the statement and the harvest has settled: tier 1.
    Settled(VerifiedSuccession),
    /// It authorizes the statement, but this session's harvest of the peer is
    /// still owed — the read that would demote a rotated-away head. Refused
    /// for now, with nothing recorded.
    AwaitingHarvest,
    /// It does not authorize the statement. Not a refusal — the walk decides.
    Undecided,
}

impl<A, C> ChainWitness<A, C> {
    pub fn new(anchors: A, chain_source: C) -> Self {
        Self {
            anchors,
            chain_source,
            verdicts: Mutex::new(HashMap::new()),
            lines: Mutex::new(HashMap::new()),
            observed: Mutex::new(HashMap::new()),
            anchor_store: Mutex::new(AnchorStoreState::NotRead),
            harvest_wait: Mutex::new(HarvestWait::default()),
        }
    }

    /// A harvest sweep runs this session — see
    /// `SuccessionWitness::harvest_armed`, which is how every session says so.
    pub fn arm_harvest_wait(&self) {
        self.harvest_wait.lock().expect("lock poisoned").armed = true;
    }

    /// The sweep settled `actor` — see `SuccessionWitness::harvest_settled`.
    pub fn harvest_settled_for(&self, actor: &ActorId) {
        self.harvest_wait
            .lock()
            .expect("lock poisoned")
            .settled
            .insert(*actor);
    }

    fn awaits_harvest_of(&self, actor: &ActorId) -> bool {
        let wait = self.harvest_wait.lock().expect("lock poisoned");
        wait.armed && !wait.settled.contains(actor)
    }

    /// What this witness has been asked and what it answered — the member-side
    /// diagnosis surface every app renders into its state contract.
    pub fn observation(&self) -> WitnessObservation {
        let observed = self.observed.lock().expect("lock poisoned");
        let mut peers: Vec<PeerWitnessObservation> = observed.values().cloned().collect();
        peers.sort_by_key(|p| p.actor.to_hex());
        WitnessObservation {
            statements_seen: peers.iter().map(|p| p.statements_seen).sum(),
            anchor_store: *self.anchor_store.lock().expect("lock poisoned"),
            peers,
        }
    }
}

impl<A, C> ChainWitness<A, C>
where
    A: SuccessionAnchors,
    C: SuccessionChainSource,
{
    /// The verification rule, in the order § The succession statement lists its
    /// sources. Inherent (not only the trait method) so a consumer that holds
    /// no conversations backend — a contact-store re-point, § Propagation's
    /// *Contacts* bullet — can run the identical policy.
    pub async fn verify_statement(
        &self,
        statement: SignedIdentitySuccession,
    ) -> Option<VerifiedSuccession> {
        let old = statement.statement.old_actor_id;
        self.saw_statement(old);

        match self.verdicts.lock().expect("lock poisoned").get(&old) {
            Some(Verdict::Verified(v)) => {
                self.saw_memo_hit(old);
                return Some(*v);
            }
            Some(Verdict::Unproven) => {
                self.saw_memo_hit(old);
                return None;
            }
            None => {}
        }

        let known = self.anchors.known_head(&old).await;
        self.observe(old, |o| o.held_head_seq = known.map(|h| h.seq));
        // Sampled here — after the anchors' once-per-generation read, which is the
        // read that decided `known` — and again on the no-anchor arm below,
        // whose own store consult is fresher. Neither adds I/O.
        self.sample_anchor_store().await;

        // Tier 1 — a head we already hold settles it with no dial at all, which
        // is what the coupled-head profile mirror was reshaped for (a
        // pubkey-only mirror could not carry the seq this check needs). Also the
        // only tier that works offline.
        //
        // ⚠ Unless the head is OUTRUN. `verify` checks the key, the signatures
        // and an advancing `seq` — nothing about whether the key is still the
        // registered one — and a rotated-away RecoveryKey signs for ever, so a
        // head naming it would settle its holder's statement right here. A head
        // the peer's own profile has claimed past keeps its guard duty below
        // and loses only this shortcut.
        //
        // ⚠ And unless this session's harvest of the peer is still OWED. The
        // mark above is only as fresh as the last profile read, and the sweep
        // that reads runs beside this path, not before it — so a statement
        // already waiting in the channel reaches here ahead of the fetch that
        // would have demoted the head. It is refused *for now*: no dial, and
        // deliberately NO verdict — this sits after the memo on purpose, and a
        // memoized `Unproven` would answer the settle's own re-drive for the
        // rest of the session (the wedge the no-anchor arm documents below).
        let outrun = known.is_some() && self.anchors.known_head_is_outrun(&old).await;
        self.observe(old, |o| o.held_head_outrun = outrun);
        if let Some(head) = known
            && !outrun
        {
            match self.settle_by_held_head(old, &statement, head) {
                HeldHead::Settled(verified) => return Some(verified),
                HeldHead::AwaitingHarvest => return None,
                HeldHead::Undecided => {}
            }
        }
        // A head that does *not* authorize this statement is not a refusal: the
        // chain may legitimately have advanced past it (a RecoveryKey
        // replacement lands a new head), and the walk below re-checks against a
        // freshly fetched chain while still passing `known` as the
        // rewrite/truncation guard. Falling through is what keeps a stale cache
        // from permanently blinding this member.

        // The dial target, in trust order: the anchor-grade handle's domain
        // first (the owner's own gesture put it on the roster row — and ONLY
        // that gesture: since the room home started naming Welcome-joined
        // rows, a row rendering by name is not a row the owner named, so
        // `known_handle` reads provenance, never the rendered string), then
        // the domain harvested from the peer's own signed profile (TOFU-grade,
        // the ratification — a Welcome-joined roster has no handle of
        // its own to offer).
        let handle = self.anchors.known_handle(&old).await;
        let domain = match handle.as_ref().and_then(|h| h.rsplit_once('@')) {
            Some((_, d)) => {
                let d = d.to_string();
                self.observe(old, |o| o.handle_domain = Some(d.clone()));
                Some(d)
            }
            None => {
                let harvested = self.anchors.known_home_domain(&old).await;
                self.observe(old, |o| o.harvested_domain = harvested.clone());
                // That consult read the anchor store FRESH and may have folded
                // in a head the up-front read predates — the
                // harvest landing after this session's first refusal. Re-check
                // tier 1 before refusing or walking, so a post-harvest verify
                // settles on its FIRST call: the parked-statement re-drive
                // (`redrive_parked_successions`) re-parks on refusal, and no
                // second harvest event ever comes for a peer whose harvest
                // already seeded.
                if let Some(head) = self.anchors.known_head(&old).await
                    && !self.anchors.known_head_is_outrun(&old).await
                {
                    match self.settle_by_held_head(old, &statement, head) {
                        HeldHead::Settled(verified) => return Some(verified),
                        HeldHead::AwaitingHarvest => return None,
                        HeldHead::Undecided => {}
                    }
                }
                harvested
            }
        };
        let Some(domain) = domain else {
            // Nothing independent to anchor on. § Propagation's federation leg
            // makes the same call — "refuses an identity it holds no
            // addressable anchor for, never TOFU'd from pushed bytes".
            // ⚠ Deliberately NOT memoized: no dial happened, so there is
            // nothing to save, and a `Unproven` here would wedge the session —
            // a harvest completing after this statement's first delivery could
            // never be consulted for its re-delivery (a Rule-2 heal re-walk, a
            // resumed sweep's second post). The memo exists to bound *dials*,
            // and this arm made none.
            self.observe(old, |o| o.outcome = WitnessOutcome::NoAnchor);
            self.sample_anchor_store().await;
            return None;
        };
        let domain = domain.as_str();

        // Tier 2 — the anchored walk. One dial, no retry: `verify` runs inline
        // on the receive path, so a second attempt would double the feed's
        // worst case to buy an anchor that was already unreachable once. An
        // empty lookup (never succeeded), an unreachable nest and a chain that
        // fails the rule are the same answer here: the statement stays a claim.
        let Some(walked) = self.chain_source.walk_from_domain(domain, old, known).await else {
            self.record(old, Verdict::Unproven);
            self.observe(old, |o| o.outcome = WitnessOutcome::WalkFailed);
            return None;
        };

        // `resolve_successor` returns the **terminal** hop of the path, so on a
        // twice-succeeded identity `walked.old_actor_id` is the *second* hop's
        // old id — which no participant row bears. The pair the consumer must
        // act on is therefore (the identity we asked about) → (where the chain
        // ends), never the terminal step verbatim; the walk's own contiguity
        // check is what makes the first half exact.
        let verified = VerifiedSuccession {
            old_actor_id: old,
            new_actor_id: walked.new_actor_id,
            seq: walked.seq,
            chain_head: walked.chain_head,
        };
        // The terminal hop's head is the head of the chain *it* was authorized
        // under — `old`'s own only on a one-hop path. On a longer one it is the
        // middle identity's, and remembering it under `old` would hand the next
        // walk a rewrite guard from another identity's chain, which refuses
        // `old`'s true chain for good. Such a peer stays TOFU-grade instead.
        if walked.old_actor_id == old {
            self.anchors.remember_head(&old, walked.chain_head).await;
        }
        self.record(old, Verdict::Verified(verified));
        self.observe(old, |o| o.outcome = WitnessOutcome::SettledByWalk);
        Some(verified)
    }

    /// The verified succession line of `name` — every successor, oldest first —
    /// for a consumer holding a bare identity and **no statement**: a community
    /// room's policy names its owner and admins, and a member re-proving the
    /// policy chain must learn whom each name has since become
    /// (`conversation-rooms.md` § Roles and authorization → *A name designates
    /// its verified line*).
    ///
    /// There is no tier 1 here — a held head settles a statement, and there is
    /// none — so this is always the anchored walk, with the held head as its
    /// rewrite/truncation guard. **The dial target is exactly
    /// [`Self::verify_statement`]'s**: the owner's own anchor-grade handle,
    /// else the domain harvested from the identity's own signed profile. Never
    /// anything the asking context supplies — the room's home nest is the very
    /// party the policy chain exists to distrust, and a domain it served would
    /// let it choose the nest that "verifies" a line it forged with a stolen
    /// seed. No anchor is [`LineResolution::NotYet`] and unmemoized (a harvest
    /// may still seed one); a dial is memoized either way, one per identity per
    /// session — until [`Self::recheck_line`] walks it again, since a verified
    /// line holds only so far.
    pub async fn resolve_line(&self, name: &ActorId) -> LineResolution {
        if let Some(memo) = self.lines.lock().expect("lock poisoned").get(name) {
            return memo.clone();
        }
        let handle = self.anchors.known_handle(name).await;
        let domain = match handle.as_ref().and_then(|h| h.rsplit_once('@')) {
            Some((_, d)) => Some(d.to_string()),
            None => self.anchors.known_home_domain(name).await,
        };
        let Some(domain) = domain else {
            return LineResolution::NotYet;
        };
        let known = self.anchors.known_head(name).await;
        let resolution = match self
            .chain_source
            .walk_line_from_domain(&domain, *name, known)
            .await
        {
            Some(hops) => {
                if let Some(first) = hops.first() {
                    self.anchors.remember_head(name, first.chain_head).await;
                }
                LineResolution::Verified(hops.iter().map(|hop| hop.new_actor_id).collect())
            }
            None => LineResolution::NotYet,
        };
        self.lines
            .lock()
            .expect("lock poisoned")
            .insert(*name, resolution.clone());
        resolution
    }

    /// [`Self::resolve_line`], walked again whatever the memo holds for `name`
    /// — an empty line, a positive one, a walk that failed — since a verified
    /// line holds only *so far*: the identity may succeed later in the
    /// session, and so may the newest holder of a positive line
    /// (`conversation-rooms.md` § Roles and authorization → *A name designates
    /// its verified line* → *A verified line holds only so far*). The memo is
    /// the last dial's answer and nothing more: which answers a room keeps is
    /// the designation's rule (`SuccessionLines::insert` — a held line only
    /// grows). How often a caller may recheck is the caller's bound — the
    /// room's `LINE_REASK_PASSES` — since each recheck may dial.
    pub async fn recheck_line(&self, name: &ActorId) -> LineResolution {
        self.lines.lock().expect("lock poisoned").remove(name);
        self.resolve_line(name).await
    }

    /// Settle at tier 1 iff `head` authorizes `statement` — the one arm shared
    /// by the two places a held head can decide: the up-front cached read,
    /// and the fresh fold a harvested-domain consult may have just performed.
    /// Records the verdict, the deciding head's `seq` and the arm;
    /// [`HeldHead::Undecided`] means "this head does not decide it", never a
    /// refusal of the statement.
    ///
    /// The harvest wait is asked only once the head is known to authorize the
    /// statement, so a head that decides nothing still falls through to the
    /// walk at once — the walk is the path the wait exists to force, and
    /// holding it back would buy nothing.
    fn settle_by_held_head(
        &self,
        old: ActorId,
        statement: &SignedIdentitySuccession,
        head: ChainHead,
    ) -> HeldHead {
        if statement.verify(&head).is_err() {
            return HeldHead::Undecided;
        }
        if self.awaits_harvest_of(&old) {
            self.observe(old, |o| {
                o.held_head_seq = Some(head.seq);
                o.outcome = WitnessOutcome::AwaitingHarvest;
            });
            return HeldHead::AwaitingHarvest;
        }
        let verified = VerifiedSuccession {
            old_actor_id: old,
            new_actor_id: statement.statement.new_actor_id,
            seq: statement.statement.seq,
            chain_head: head,
        };
        self.record(old, Verdict::Verified(verified));
        self.observe(old, |o| {
            o.held_head_seq = Some(head.seq);
            o.outcome = WitnessOutcome::SettledByHeldHead;
        });
        HeldHead::Settled(verified)
    }

    fn record(&self, actor: ActorId, verdict: Verdict) {
        self.verdicts
            .lock()
            .expect("lock poisoned")
            .insert(actor, verdict);
    }

    /// Mutate this identity's report row, creating it on first sight.
    ///
    /// A fresh row starts at [`WitnessOutcome::NoAnchor`] deliberately: it is
    /// the state a statement is in before any arm has claimed it, so a row that
    /// never reached an arm — a panic, an await that never returned — reads as
    /// "nothing anchored it" rather than as a verdict it never earned.
    fn observe(&self, actor: ActorId, f: impl FnOnce(&mut PeerWitnessObservation)) {
        let mut observed = self.observed.lock().expect("lock poisoned");
        let row = observed
            .entry(actor)
            .or_insert_with(|| PeerWitnessObservation {
                actor,
                statements_seen: 0,
                memo_hits: 0,
                held_head_seq: None,
                held_head_outrun: false,
                handle_domain: None,
                harvested_domain: None,
                outcome: WitnessOutcome::NoAnchor,
            });
        f(row);
    }

    fn saw_statement(&self, actor: ActorId) {
        self.observe(actor, |o| o.statements_seen += 1);
    }

    fn saw_memo_hit(&self, actor: ActorId) {
        self.observe(actor, |o| o.memo_hits += 1);
    }

    async fn sample_anchor_store(&self) {
        let state = self.anchors.anchor_store_state().await;
        *self.anchor_store.lock().expect("lock poisoned") = state;
    }
}

#[cfg(feature = "conversations-witness")]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl<A, C> fauna_conversations::backend::SuccessionWitness for ChainWitness<A, C>
where
    A: SuccessionAnchors,
    C: SuccessionChainSource,
{
    async fn verify(&self, statement: SignedIdentitySuccession) -> Option<VerifiedSuccession> {
        self.verify_statement(statement).await
    }

    async fn succession_line(
        &self,
        name: &ActorId,
    ) -> fauna_conversations::backend::SuccessionLine {
        match self.resolve_line(name).await {
            LineResolution::Verified(successors) => {
                fauna_conversations::backend::SuccessionLine::Verified(successors)
            }
            LineResolution::NotYet => fauna_conversations::backend::SuccessionLine::NotYet,
        }
    }

    async fn recheck_line(&self, name: &ActorId) -> fauna_conversations::backend::SuccessionLine {
        match self.recheck_line(name).await {
            LineResolution::Verified(successors) => {
                fauna_conversations::backend::SuccessionLine::Verified(successors)
            }
            LineResolution::NotYet => fauna_conversations::backend::SuccessionLine::NotYet,
        }
    }

    async fn anchor_seed_landed(&self) {
        // Straight through to the anchors: the policy holds no store state of
        // its own, and the verdict memo deliberately has nothing to invalidate
        // — the no-anchor arm records none (see `Verdict::Unproven`).
        self.anchors.anchor_seed_landed().await;
    }

    async fn harvest_armed(&self) {
        self.arm_harvest_wait();
    }

    async fn harvest_settled(&self, actor: &ActorId) {
        // The witness's own state and nothing else: NOT forwarded to the
        // anchors, whose only announcement is a landed seed — a settle is not
        // one, and must not buy a store read.
        self.harvest_settled_for(actor);
    }
}

/// [`SuccessionAnchors`] over the app's own conversations thread store and the
/// account's peer anchors (`fauna.state.peer-anchors`) — the shared
/// implementation for every app, since a member that receives a statement is by
/// construction in a thread with the identity it names, and every app that hosts
/// the account runtime registers the anchors' store on that same thread store's
/// manager ([`fauna_conversations::backend::PeerAnchorStore`]).
///
/// ⚠ **Being in the thread is not the same as holding a handle (found
/// 2026-08-10), and the answer is the peer-profile harvest (ratified the same
/// day).** This type used to claim the thread *is* where the handle comes
/// from. It is — but only on the side that **ran the add**, which resolved an
/// address to get there. A member who joined by **Welcome** has a roster built
/// from `MlsEngine::group_members` (`backends::fauna_mls::ingest_welcome`), and
/// a leaf credential carries no handle, so every participant rests with
/// `handle: String::new()` and [`Self::known_handle`] answers `None` for them
/// forever — and that member is precisely the audience the in-group statement
/// is posted for. The ratified close (`identity-succession.md` § The
/// succession statement → *the peer-profile harvest*): a harvest on ordinary
/// read paths seeds the account's held chain heads (tier 1) and harvested
/// anchor domains (the tier-2 fallback this type serves via
/// [`SuccessionAnchors::known_home_domain`]), so this type answers for a
/// Welcome-joined member from the same store the head half already rests in.
/// The channel's recorded home-nest URL stays **disqualified as an anchor**
/// (delivered by the party that later asserts the succession); it is
/// admissible only as fetch transport for the signed profile, which is the
/// harvest's whole point.
///
/// **The head half is durable, and that is the whole point of it.** A head this
/// consumer saw before the compromise window is what lets it refuse a chain that
/// *rewrites or truncates* what it already knows; an in-memory-only cache
/// re-TOFUs on every relaunch, so the guard is unreachable from a cold start —
/// which is the state a member is in most of the time. It rests on the account
/// plane (`fauna.state.peer-anchors`, fleet-only) rather than in a device-local
/// file because the principal the guard protects is the **user**, not one
/// device: a head seen on the phone should anchor the laptop. The in-memory map
/// is a pure read-through cache in front of it, so the inline receive path pays
/// **one anchor-store read per seed generation** —
/// one for the session, plus one after each harvest that seeds something new.
/// Nothing a peer sends can add to that count, which is the bound;
/// [`SuccessionAnchors::anchor_seed_landed`] is what advances the generation.
///
/// ⚠ **The store is lent late, and never by the caller.** The account store
/// resolves after login, so this type reads it through the manager at each read
/// (`ConversationsManager::peer_anchor_store`), which the ONE shared
/// store-ready registration (`fauna_account_seams::conversation_seams::wire_parts`)
/// fills for every app — no host threads a store in, so none can forget one.
/// Until it is lent, and after an identity change retires it, the store reads
/// as **unreadable** ([`AnchorStoreState::Unreadable`], the backoff below),
/// never as empty: an empty answer would be TOFU on evidence that does not
/// exist.
///
/// It deliberately never invents a head from a fetched profile — a `Profile` is
/// signed by the identity key, which is exactly what a seed thief holds, so
/// re-reading one at verify time would anchor the check on the attacker. The
/// only writers are a completed walk ([`SuccessionAnchors::remember_head`],
/// below) and an ordinary peer-profile read from *before* any compromise window,
/// which is a separate producer this type does not own.
///
/// ⚠ **The manager is held `Weak`, and that is load-bearing** (2026-08-27). This
/// type is registered *on* the session it reads from — the app hands the
/// witness to `ConversationsSession::set_succession_witness`, which parks it in
/// the FaunaMls backend the manager owns — so a strong handle here closes a
/// cycle (manager → backend → witness → anchors → manager) with no `Drop`
/// anywhere on the ring and a `OnceLock` that cannot be cleared. Measured on
/// tui: the retired identity's `MlsEngine`, and the one-engine-per-store lock
/// it holds on `mls_state.db`, lived for the process's life after an account
/// switch, so the post-succession sweep retry refused the ceremony's own device
/// and an A → B → A switch found A's store held by A's zombie
/// (`account-scoping.md` § Implementation status → the `tui (in-memory)`
/// ledger row). A failed upgrade is the no-anchor arm: a session that is gone
/// holds no thread store to anchor from.
#[cfg(feature = "conversations-witness")]
pub struct ThreadParticipantAnchors {
    manager: std::sync::Weak<fauna_conversations::ConversationsManager>,
    /// Read-through cache over the anchor store. Never the authority:
    /// the store is read once per seed generation (see [`Self::refresh`]).
    heads: Mutex<HashMap<ActorId, ChainHead>>,
    /// The cached heads the store marks `outrun` — held, still the walk's
    /// guard, no longer a tier-1 anchor. Follows the store at each
    /// [`Self::refresh`]; an in-session walk advancing a head clears its entry,
    /// exactly as `PeerAnchors::remember_chain_head` clears the mark at rest.
    outrun: Mutex<std::collections::HashSet<ActorId>>,
    /// What the last anchor-store read found — recorded at each read
    /// site so [`SuccessionAnchors::anchor_store_state`] can answer with no I/O
    /// of its own.
    last_store_read: Mutex<AnchorStoreState>,
    /// Bumped by [`SuccessionAnchors::anchor_seed_landed`], compared by
    /// [`Self::refresh`]. The **only** thing that can make a harvested domain,
    /// a harvested head or an `outrun` mark appear at rest is a harvest, and the
    /// ratified re-drive routes every one through that signal, so a generation
    /// that has not moved is proof the last read is still current.
    seed_generation: std::sync::atomic::AtomicU64,
    /// The harvested domains as of [`Self::seed_generation`]'s value at the read
    /// that built it — and, by its stamp, the record of which generation
    /// [`Self::heads`] and [`Self::outrun`] were last folded at. `None` until
    /// the first successful read, and left untouched by a failed one so an
    /// offline relaunch retries rather than caching an empty view of the store
    /// — a wrongly-empty cache degrades exactly to TOFU, which is the thing
    /// this type exists to stop being permanent.
    anchor_domains: Mutex<Option<AnchorDomains>>,
    /// When the last anchor-store read failed, while it still
    /// fails — the [`UNREADABLE_STORE_BACKOFF_SECS`] window [`Self::refresh`]
    /// waits out. Cleared by a successful read and by a landed seed.
    failed_read_at: Mutex<Option<fauna_core::data::Timestamp>>,
    /// The clock the backoff reads: [`fauna_core::data::Timestamp::now`] (the
    /// wasm-safe wall-clock door) in production, a hand-stepped one under test
    /// ([`Self::with_clock`]).
    clock: std::sync::Arc<dyn Fn() -> fauna_core::data::Timestamp + Send + Sync>,
}

/// How long [`ThreadParticipantAnchors`] waits after a **failed** anchor-store
/// read before it reads again — a backoff on the failing read, never a cache
/// of it. A store that could not be read has said nothing about what it holds,
/// so the empty view is never stamped for the generation; but every consult
/// re-reading it made the amplification conditional rather than gone:
/// a room's parked floor delete records re-ask their not-yet policy names on
/// every inbound pass, so a device whose store was broken paid rooms ×
/// parked records × names nest round trips a poll, inline on the receive path.
/// The bound: **one read per this interval while the store fails**, whatever
/// drives the consults; a store that comes back is seen on the first consult
/// after the interval, and at once after a landed seed
/// ([`SuccessionAnchors::anchor_seed_landed`] — the harvest just wrote it).
/// Hard-coded: nobody chooses how often a broken store is retried.
#[cfg(feature = "conversations-witness")]
pub const UNREADABLE_STORE_BACKOFF_SECS: u64 = 10;

/// [`ThreadParticipantAnchors::anchor_domains`]' contents — the harvested
/// per-actor domains, stamped with the seed generation they were read at.
///
/// Sized by the account's own anchors, never by anything a peer can send: the
/// harvest seeds only roster participants it fetched a signed profile for.
#[cfg(feature = "conversations-witness")]
struct AnchorDomains {
    generation: u64,
    domains: HashMap<ActorId, String>,
}

#[cfg(feature = "conversations-witness")]
impl ThreadParticipantAnchors {
    /// `manager` is the session's own manager, downgraded by the caller
    /// (`Arc::downgrade`) — the type is `Weak` on purpose, see the struct doc.
    ///
    /// The anchors themselves are read through `manager`'s peer-anchor store
    /// (the struct doc owns why it is not an argument).
    pub fn new(manager: std::sync::Weak<fauna_conversations::ConversationsManager>) -> Self {
        Self::with_clock(
            manager,
            std::sync::Arc::new(fauna_core::data::Timestamp::now),
        )
    }

    /// [`Self::new`] reading `clock` for the failed-read backoff
    /// ([`UNREADABLE_STORE_BACKOFF_SECS`]) — so a test asserts the window by
    /// stepping a clock, never by waiting one out.
    pub fn with_clock(
        manager: std::sync::Weak<fauna_conversations::ConversationsManager>,
        clock: std::sync::Arc<dyn Fn() -> fauna_core::data::Timestamp + Send + Sync>,
    ) -> Self {
        Self {
            manager,
            heads: Mutex::new(HashMap::new()),
            outrun: Mutex::new(std::collections::HashSet::new()),
            last_store_read: Mutex::new(AnchorStoreState::NotRead),
            seed_generation: std::sync::atomic::AtomicU64::new(0),
            anchor_domains: Mutex::new(None),
            failed_read_at: Mutex::new(None),
            clock,
        }
    }

    /// The store the anchors rest in, resolved per read through the manager —
    /// `Err` while none is lent (before the store-ready edge, after an identity
    /// change) or once the session is gone. The manager is let go before the
    /// caller awaits anything.
    fn anchor_store(
        &self,
    ) -> Result<std::sync::Arc<dyn fauna_conversations::backend::PeerAnchorStore>, String> {
        self.manager
            .upgrade()
            .ok_or_else(|| "the conversations session is gone".to_string())?
            .peer_anchor_store()
            .ok_or_else(|| "the account store is not ready".to_string())
    }

    /// Every anchor the account holds, folded.
    async fn load_anchors(&self) -> Result<fauna_core::data::PeerAnchors, String> {
        self.anchor_store()?.peer_anchors().await
    }

    /// Record what an anchor-store read found. Called at every read site, so the
    /// report never lags the state the anchors actually answered from.
    fn note_store_read(&self, state: AnchorStoreState) {
        *self.last_store_read.lock().expect("lock poisoned") = state;
    }

    fn note_store_contents(&self, anchors: &fauna_core::data::PeerAnchors) {
        self.note_store_read(AnchorStoreState::Read {
            heads: anchors.chain_heads.len(),
            domains: anchors.anchor_domains.len(),
        });
    }

    /// Read the durable store **once per seed generation** and fold everything
    /// it holds into the caches — heads, their `outrun` marks, and the
    /// harvested domains — so [`SuccessionAnchors::known_head`] and
    /// [`SuccessionAnchors::known_home_domain`] share ONE read rather than
    /// paying one each.
    ///
    /// The head half used to hydrate once per session and never look again,
    /// which was enough while a harvest could only *fill an empty slot* (a miss
    /// re-read through the domain consult). It stopped being enough when the
    /// harvest learned to **demote** a held head: a mark landing mid-session
    /// has to reach a head this cache already answers for, or a long-running
    /// session keeps settling a rotated-away kit's statements offline until
    /// its next launch.
    ///
    /// ⚠ The per-consult read this guard replaced was a resource-amplification
    /// hole. The comment it carried —
    /// "the rare path right before a network dial, so a store read is
    /// proportionally cheap" — described the honest flow and missed the hostile
    /// one: the witness is consulted for *every* `Succession` GroupMeta body,
    /// and `poll_inbound_conv` consults it before the roster gate that bounds
    /// parking — so an in-group member forging `old_actor_id`s drove one nest
    /// RPC + one config unseal each, inline on the receive path, with no bound
    /// at all. (Before the `__config` rail retired at closure step (6), the store's
    /// load was a nest round trip; the account plane's is a local read, and the
    /// bound holds either way; see `config-dissolution.md`.)
    ///
    /// The guard keeps the property the read existed for. What the store holds
    /// for a peer changes only when a harvest writes it (or this type's own
    /// [`SuccessionAnchors::remember_head`], which updates the cache itself),
    /// and the ratified re-drive routes every harvest write through
    /// `anchor_seed_landed`, so an unmoved generation *proves* the cached view
    /// is current — memoization of a fact, not a staleness window. The
    /// generation is sampled BEFORE the read so a seed landing mid-read stamps
    /// stale and the next consult re-reads. Concurrent callers may both read —
    /// harmless, and cheaper than holding a lock across a round trip.
    ///
    /// A **failed** read is not cached, but it is backed off: no re-read until
    /// [`UNREADABLE_STORE_BACKOFF_SECS`] have passed since it failed, or a seed
    /// lands. Only the failing arm waits — a readable store is governed by
    /// the generation alone.
    async fn refresh(&self) {
        use std::sync::atomic::Ordering;
        let generation = self.seed_generation.load(Ordering::Acquire);
        if self
            .anchor_domains
            .lock()
            .expect("lock poisoned")
            .as_ref()
            .is_some_and(|view| view.generation == generation)
        {
            return;
        }
        let now = (self.clock)();
        // A clock stepped backwards past the failure reads as due: one extra
        // read, never a wait that cannot end.
        if self
            .failed_read_at
            .lock()
            .expect("lock poisoned")
            .is_some_and(|failed| {
                now >= failed && now.0 - failed.0 < UNREADABLE_STORE_BACKOFF_SECS * 1_000_000
            })
        {
            return;
        }
        let anchors = match self.load_anchors().await {
            Ok(anchors) => anchors,
            Err(e) => {
                // Not an error to surface: a member that cannot reach its own
                // anchor store still verifies, at TOFU grade, exactly as it did
                // before this store existed. Deliberately not cached either — a
                // store that could not be read has told us nothing about what
                // it holds, and stamping the empty view would answer `None` for
                // the rest of the generation on evidence that does not exist.
                tracing::debug!(
                    error = %e,
                    "the succession witness could not read its durable anchors"
                );
                self.note_store_read(AnchorStoreState::Unreadable);
                *self.failed_read_at.lock().expect("lock poisoned") = Some(now);
                return;
            }
        };
        *self.failed_read_at.lock().expect("lock poisoned") = None;
        self.note_store_contents(&anchors);
        self.fold_heads(&anchors);
        // First-write-wins per actor, exactly as `known_anchor_domain` resolves
        // it — asked of the anchors themselves rather than re-derived here.
        let mut domains = HashMap::new();
        for entry in &anchors.anchor_domains {
            if let Some(domain) = anchors.known_anchor_domain(&entry.actor) {
                domains.entry(entry.actor).or_insert(domain);
            }
        }
        *self.anchor_domains.lock().expect("lock poisoned") = Some(AnchorDomains {
            generation,
            domains,
        });
    }

    /// Fold every head a freshly-read store holds into the cache, monotonically
    /// — a stored entry never lowers one this session already learned from a
    /// walk — and bring the `outrun` set into line with the store for every
    /// head the cache is not already past. A freshly harvested head thus settles
    /// the very statement that provoked the read at tier 1, and a freshly
    /// demoted one stops settling from the same read on.
    fn fold_heads(&self, anchors: &fauna_core::data::PeerAnchors) {
        let mut heads = self.heads.lock().expect("lock poisoned");
        let mut outrun = self.outrun.lock().expect("lock poisoned");
        for entry in &anchors.chain_heads {
            let Some(head) = anchors.known_chain_head(&entry.actor) else {
                continue;
            };
            match heads.get(&entry.actor) {
                // This session walked past what is at rest; the store's mark is
                // about a head the cache no longer answers with.
                Some(existing) if existing.seq > head.seq => continue,
                Some(existing) if existing.seq == head.seq => {}
                _ => {
                    heads.insert(entry.actor, head);
                }
            }
            if entry.outrun {
                outrun.insert(entry.actor);
            } else {
                outrun.remove(&entry.actor);
            }
        }
    }

    /// Update the cache monotonically. Returns whether anything changed, which
    /// is what keeps a re-delivered statement from provoking a store write
    /// that would store identical bytes. An advance drops the actor's `outrun`
    /// entry: the mark was about the head this one replaces.
    fn cache(&self, actor: &ActorId, head: ChainHead) -> bool {
        let mut heads = self.heads.lock().expect("lock poisoned");
        match heads.get(actor) {
            Some(existing) if existing.seq >= head.seq => false,
            _ => {
                heads.insert(*actor, head);
                self.outrun.lock().expect("lock poisoned").remove(actor);
                true
            }
        }
    }
}

#[cfg(feature = "conversations-witness")]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl SuccessionAnchors for ThreadParticipantAnchors {
    async fn known_head(&self, actor: &ActorId) -> Option<ChainHead> {
        // Generation-gated, so a cache hit costs an atomic load and a lock —
        // and a head the harvest demoted mid-session is seen as demoted.
        self.refresh().await;
        self.heads
            .lock()
            .expect("lock poisoned")
            .get(actor)
            .copied()
    }

    async fn known_head_is_outrun(&self, actor: &ActorId) -> bool {
        // No read of its own: `known_head` is always asked first and its
        // `refresh` is what brought this set into line with the store.
        self.outrun.lock().expect("lock poisoned").contains(actor)
    }

    async fn known_handle(&self, actor: &ActorId) -> Option<String> {
        // A session that is gone holds no thread store: the no-anchor arm.
        let manager = self.manager.upgrade()?;
        // Provenance, not the rendered string: the thread store answers only
        // for a handle the owner's own gesture put on a row. A row the room
        // home named (`ConversationsManager::apply_resolved_handles`) renders
        // by name and still answers `None` here — the trait doc owns why.
        manager
            .anchor_grade_handle_for(actor)
            .filter(|handle| handle.contains('@'))
    }

    async fn known_home_domain(&self, actor: &ActorId) -> Option<String> {
        // The store is read **once per seed generation**, never once per
        // consult — [`Self::refresh`] owns the guard and why it is sound. While
        // the config is in hand it folds the heads in too, which is what lets
        // `verify_statement`'s re-check right below its call site settle the
        // very statement that provoked a post-harvest read at tier 1.
        self.refresh().await;
        self.anchor_domains
            .lock()
            .expect("lock poisoned")
            .as_ref()
            .and_then(|view| view.domains.get(actor).cloned())
    }

    async fn anchor_seed_landed(&self) {
        // One bump invalidates the whole view rather than one actor's entry:
        // the read it gates is a whole-config load either way, so per-actor
        // bookkeeping would buy nothing and would grow with what a peer can
        // name. `Release` pairs with the `Acquire` above.
        self.seed_generation
            .fetch_add(1, std::sync::atomic::Ordering::Release);
        // A harvest just wrote the store, so it is reachable: whatever backoff
        // a failed read started is over.
        *self.failed_read_at.lock().expect("lock poisoned") = None;
    }

    async fn anchor_store_state(&self) -> AnchorStoreState {
        *self.last_store_read.lock().expect("lock poisoned")
    }

    async fn remember_head(&self, actor: &ActorId, head: ChainHead) {
        // Read what is already at rest before deciding this is news: on the
        // first walk of a session the cache is empty, and writing without
        // reading would compare against nothing and re-put a head another
        // device already advanced past.
        self.refresh().await;
        // Monotonic: a lower seq never displaces a higher one, so a served
        // chain can extend what this member saw but never rewind it. ⚠ This
        // early return saves a **round trip**, nothing more — it is deliberately
        // not where the monotonic guarantee lives, and mutation testing says so
        // (deleting it leaves every test green, because the store below refuses
        // the same write). The correctness rule is the store's, because only the
        // store sees what another of the user's devices has advanced past since
        // this session last read it.
        if !self.cache(actor, head) {
            return;
        }
        let persisted = async {
            let store = self.anchor_store()?;
            let mut anchors = store.peer_anchors().await?;
            // The same monotonic rule again, against what the store holds —
            // every head another device has advanced past and a walk has
            // merged in since this session last read. Its answer, not ours,
            // decides whether a write happens at all; and the write is a JOIN
            // on the store thread, so a device that advanced further while
            // this one read still keeps its higher head.
            if anchors.remember_chain_head(*actor, head) {
                store.merge_peer_anchors(anchors).await?;
            }
            Ok::<(), String>(())
        };
        if let Err(e) = persisted.await {
            // Best-effort, and the cost of losing it is one relaunch back at
            // TOFU grade for this identity, never a wrong verdict.
            tracing::debug!(
                error = %e,
                "persisting a remembered chain head failed; it stays session-scoped"
            );
        }
    }
}

/// How long the whole witness round trip — resolve, dial, `succession.lookup`,
/// one `registration.chain` per hop — may take before the statement is left as
/// a claim.
///
/// It is generous on purpose: a cross-nest dial under a loaded box is the
/// normal case, and rendering "someone added a stranger" for a peer who really
/// did recover their account is the worse failure. It is *bounded* on purpose
/// too: `verify` is called inline by the inbound poll, so an unreachable anchor
/// must cost this much once and then degrade — never stall the feed. The
/// witness's own per-session verdict memo is what keeps it to once.
#[cfg(not(target_arch = "wasm32"))]
const WITNESS_ROUND_TRIP_BUDGET: std::time::Duration = std::time::Duration::from_secs(15);

/// The **native** leg of the in-group succession witness: resolve the peer's
/// handle domain, dial it anonymously, and hand the connection to the shared
/// [`walk`].
///
/// One type for every direct-Rust app (tui, linux, and the native side the FFI
/// apps drive) rather than the per-app copy this module's "how *this* platform
/// reaches a foreign nest" once implied: the seam is per-**platform**, and
/// every native platform reaches a foreign nest by exactly this route. Only
/// web's wasm dialer is genuinely different, and it stays app-side.
///
/// **Anonymous is the only option, not a shortcut.** The verifying member holds
/// no account on the succeeded peer's nest; it works because
/// `succession.lookup` and `registration.chain` are pre-identity kinds
/// (`identity-succession.md` § Enforcement on the home nest). Same reason, same
/// shape as the launch-side `verify_superseded_successor`.
///
/// **The domain is the anchor.** It comes from the handle the thread's
/// participant row has carried since the peer joined — never from the statement
/// under test, which is the "delivered chain" hole § The succession statement
/// forbids.
#[cfg(not(target_arch = "wasm32"))]
pub struct NativeSuccessionChainSource;

/// The one native anonymous dial every anchored walk shares: connect to `url`
/// and run [`walk`] there. Unbounded on its own — each caller wraps it in
/// [`WITNESS_ROUND_TRIP_BUDGET`] together with whatever resolution it does first.
#[cfg(not(target_arch = "wasm32"))]
async fn dial_and_walk(url: &str, old: ActorId, known_head: Option<ChainHead>) -> AnchoredWalk {
    let Ok(anon) = fauna_anon_client::AnonymousNestClient::connect(url).await else {
        return AnchoredWalk::Unsettled;
    };
    walk_outcome(anon, old, known_head).await
}

/// The native anchored walk for a consumer whose anchor is a nest **base URL it
/// already holds**, not a handle domain — where `old` verifiably ended up
/// ([`AnchoredWalk::Succeeded`]), that the anchor answered it never succeeded
/// ([`AnchoredWalk::NeverSucceeded`]), or nothing settled — unreachable, timed
/// out, or a chain that fails the rule ([`AnchoredWalk::Unsettled`]).
///
/// Same dial, same budget and same verification as
/// [`NativeSuccessionChainSource`]; only the resolution step is absent, because
/// there is nothing to resolve. The caller is an inline inbound apply too (the
/// calendar's organizer binding, `caldav-server.md` § Who may mutate an existing
/// event over the inbound rail → *A succeeded organizer*), so an unreachable
/// anchor costs the budget and then degrades to the refusal that was already
/// standing.
///
/// **`anchor_nest_url` must be one the caller recorded itself**, before the
/// succession was ever asserted — the anchoring contract [`walk`] documents. A
/// URL read off the message that claims the succession is the "delivered chain"
/// hole, whatever type it arrives in.
#[cfg(not(target_arch = "wasm32"))]
pub async fn walk_at_nest_url(
    anchor_nest_url: &str,
    old: ActorId,
    known_head: Option<ChainHead>,
) -> AnchoredWalk {
    tokio::time::timeout(
        WITNESS_ROUND_TRIP_BUDGET,
        dial_and_walk(anchor_nest_url, old, known_head),
    )
    .await
    .unwrap_or(AnchoredWalk::Unsettled)
}

#[cfg(not(target_arch = "wasm32"))]
#[async_trait]
impl SuccessionChainSource for NativeSuccessionChainSource {
    async fn walk_from_domain(
        &self,
        handle_domain: &str,
        old: ActorId,
        known_head: Option<ChainHead>,
    ) -> Option<VerifiedSuccession> {
        let domain = handle_domain.to_string();
        tokio::time::timeout(WITNESS_ROUND_TRIP_BUDGET, async move {
            // SRV-aware, so a peer nest on a non-standard port stays reachable
            // without the port ever being typed anywhere (the same
            // `resolve_full_url` semantics onboarding's handle probe uses).
            let url = fauna_core::resolve::resolve_full_url(&format!("https://{domain}")).await;
            dial_and_walk(&url, old, known_head).await
        })
        .await
        .ok()
        .and_then(AnchoredWalk::successor)
    }

    async fn walk_line_from_domain(
        &self,
        handle_domain: &str,
        old: ActorId,
        known_head: Option<ChainHead>,
    ) -> Option<Vec<VerifiedSuccession>> {
        let domain = handle_domain.to_string();
        // The same route and the same budget as the terminal walk above: only
        // what the shared body returns differs.
        tokio::time::timeout(WITNESS_ROUND_TRIP_BUDGET, async move {
            let url = fauna_core::resolve::resolve_full_url(&format!("https://{domain}")).await;
            let anon = fauna_anon_client::AnonymousNestClient::connect(&url)
                .await
                .ok()?;
            walk_line(anon, old, known_head).await
        })
        .await
        .ok()
        .flatten()
    }
}

/// The member side of a succession as a **driver** reads it: what the inbound
/// poll did with the statements it saw, what the harvest producer managed per
/// peer, and what the witness made of each identity.
///
/// The one shared renderer behind every app's `data.succession_witness` — the
/// receive-side twin of `data.succession_sweep`. Each app owes only the field
/// reads that feed it: its own witness handle, its own [`HarvestLog`], and its
/// backend's [`SuccessionStatementCounts`]. Convention 11's second corollary is
/// why this takes values *already computed* rather than the session itself: the
/// state provider is the ack path, so every input here must be a field read and
/// never a round trip.
///
/// The three halves are deliberately reported together and cannot be derived
/// from each other. Read them in order:
///
/// 1. `statements.seen == 0` on a channel that demonstrably grew indicts the
///    inbound poll and exonerates every anchor question below it.
/// 2. `harvest` is the **producer**: a peer absent from it was never attempted,
///    which is a different failure from any outcome it could carry.
/// 3. `anchor_store` separates "nothing was seeded" from "the store is
///    unreadable" — both of which surface per-peer as `no_anchor`.
///
/// [`HarvestLog`]: crate::harvest::HarvestLog
/// [`SuccessionStatementCounts`]: fauna_conversations::backends::fauna_mls::SuccessionStatementCounts
#[cfg(feature = "conversations-witness")]
pub fn state_json(
    observation: &WitnessObservation,
    harvest: &crate::harvest::HarvestLog,
    counts: &fauna_conversations::backends::fauna_mls::SuccessionStatementCounts,
) -> serde_json::Value {
    serde_json::json!({
        // The PRODUCER half: what the sweep did per peer. A peer absent here
        // was never attempted, which is a different failure from any outcome.
        "harvest": harvest.entries().iter().map(|e| serde_json::json!({
            "actor_id": e.actor.to_hex(),
            "attempts": e.attempts,
            "outcome": format!("{:?}", e.last),
        })).collect::<Vec<_>>(),
        // Whether the place anchors REST could be read, and what it held —
        // an empty-but-readable store indicts the producer above, an
        // unreadable one indicts the member's own anchor store (or one not lent yet).
        "anchor_store": match observation.anchor_store {
            AnchorStoreState::NotRead => serde_json::json!({"state": "not_read"}),
            AnchorStoreState::Unreadable => serde_json::json!({"state": "unreadable"}),
            AnchorStoreState::Read { heads, domains } => serde_json::json!({
                "state": "read", "heads": heads, "domains": domains,
            }),
        },
        "statements": {
            "seen": counts.seen,
            "undecodable": counts.undecodable,
            "no_witness": counts.no_witness,
            "repointed": counts.repointed,
            "parked": counts.parked,
            // The roster-pair gate's two arms. Both render exactly like every
            // other silent arm above (the row does not move), so without their
            // own counts a journey cannot tell "the ceremony is mid-flight
            // here" from "this statement was replayed into a group it never
            // ran in" — and those want opposite responses.
            "awaiting_remove_old": counts.awaiting_remove_old,
            "not_in_this_group": counts.not_in_this_group,
            // The folder commit walk's hold behind a parked statement
            // (`federation.md` § Cross-nest shared folders + channel append
            // → *The folder commit walk inherits the harvest wait*): a count
            // climbing while `parked` stays up is a set's commit rail
            // waiting on the sweep to settle its owner.
            "held_commits": counts.held_commits,
        },
        "verified_statements": observation.statements_seen,
        "peers": observation.peers.iter().map(|p| serde_json::json!({
            "actor_id": p.actor.to_hex(),
            "statements_seen": p.statements_seen,
            "memo_hits": p.memo_hits,
            "held_head_seq": p.held_head_seq,
            "held_head_outrun": p.held_head_outrun,
            "handle_domain": p.handle_domain,
            "harvested_domain": p.harvested_domain,
            "outcome": match p.outcome {
                WitnessOutcome::SettledByHeldHead => "settled_by_held_head",
                WitnessOutcome::SettledByWalk => "settled_by_walk",
                WitnessOutcome::NoAnchor => "no_anchor",
                WitnessOutcome::WalkFailed => "walk_failed",
                WitnessOutcome::AwaitingHarvest => "awaiting_harvest",
            },
        })).collect::<Vec<_>>(),
    })
}
