//! The in-group succession **witness** policy — `identity-succession.md`
//! § The succession statement (verification rule) and § Propagation → *MLS
//! groups*.
//!
//! What these pin is the half a member client cannot get wrong without handing
//! a seed thief the group: *which* anchor the statement is checked against, and
//! what happens when there is none. The chain walk itself is already pinned in
//! `ceremonies.rs`; here the chain source is a spy, so every test asserts not
//! only the verdict but **whether a dial happened at all** and **what domain it
//! went to** — the two facts the rule is actually about.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fauna_client_recovery::harvest::{HarvestOutcome, harvest_peer_anchor};
use fauna_client_recovery::witness::{ChainWitness, SuccessionAnchors, SuccessionChainSource};
use fauna_conversations::backend::{PeerAnchorStore, SuccessionWitness};
use fauna_core::data::{PeerAnchorRefusal, PeerAnchorSeed, PeerAnchors, Timestamp};
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_core::recovery::{
    ChainHead, IdentitySuccession, RecoveryKey, SignedIdentitySuccession, VerifiedSuccession,
};

// ── fixtures ────────────────────────────────────────────────────────────────

struct Fixture {
    old: ActorId,
    successor: ActorId,
    signed: SignedIdentitySuccession,
    /// The head the statement was authorized under — what a consumer that
    /// cached the peer's `Profile.recovery_head` in time would hold.
    head: ChainHead,
}

fn fixture() -> Fixture {
    let old_kp = ActorKeypair::from_secret([11u8; 32]);
    let successor_kp = ActorKeypair::from_secret([22u8; 32]);
    let recovery = RecoveryKey::generate();
    let statement = IdentitySuccession {
        old_actor_id: old_kp.actor_id(),
        new_actor_id: successor_kp.actor_id(),
        recovery_pubkey: recovery.public(),
        seq: 2,
        created_at: Timestamp::now(),
    };
    let signed = statement
        .sign(
            &recovery,
            successor_kp.signing_key(),
            Some(old_kp.signing_key()),
        )
        .expect("the fixture statement signs");
    Fixture {
        old: old_kp.actor_id(),
        successor: successor_kp.actor_id(),
        signed,
        head: ChainHead::new(recovery.public(), 1),
    }
}

/// Anchors under test control: what this consumer claims to independently know.
struct StubAnchors {
    head: Option<ChainHead>,
    handle: Option<String>,
    /// The harvested fallback, consulted only when `handle` answers nothing.
    home_domain: Option<String>,
    remembered: Mutex<Vec<(ActorId, ChainHead)>>,
}

/// Local newtype so the forwarding impls below are legal (the orphan rule
/// refuses `impl ForeignTrait for Arc<Local>`).
struct Shared<T>(Arc<T>);

impl<T> Clone for Shared<T> {
    fn clone(&self) -> Self {
        Shared(Arc::clone(&self.0))
    }
}

#[async_trait]
impl SuccessionAnchors for Shared<StubAnchors> {
    async fn known_head(&self, _actor: &ActorId) -> Option<ChainHead> {
        self.0.head
    }
    async fn known_handle(&self, _actor: &ActorId) -> Option<String> {
        self.0.handle.clone()
    }
    async fn known_home_domain(&self, _actor: &ActorId) -> Option<String> {
        self.0.home_domain.clone()
    }
    async fn remember_head(&self, actor: &ActorId, head: ChainHead) {
        self.0.remembered.lock().unwrap().push((*actor, head));
    }
}

/// A chain source that records every dial and answers with a canned verdict —
/// so a test can assert "no dial happened", which is the whole point of tier 1.
struct SpyChainSource {
    answer: Option<VerifiedSuccession>,
    dials: Mutex<Vec<String>>,
}

#[async_trait]
impl SuccessionChainSource for Shared<SpyChainSource> {
    async fn walk_from_domain(
        &self,
        handle_domain: &str,
        _old: ActorId,
        _known_head: Option<ChainHead>,
    ) -> Option<VerifiedSuccession> {
        self.0.dials.lock().unwrap().push(handle_domain.to_string());
        self.0.answer
    }
}

type Witness = ChainWitness<Shared<StubAnchors>, Shared<SpyChainSource>>;

/// The witness takes its seams by value and exposes no state — correct for
/// production, awkward for a spy. So the tests hand it `Arc`s they keep a
/// second handle on rather than adding test-only accessors to a shipped type.
fn build(
    head: Option<ChainHead>,
    handle: Option<&str>,
    answer: Option<VerifiedSuccession>,
) -> (Witness, Arc<StubAnchors>, Arc<SpyChainSource>) {
    build_anchored(head, handle, None, answer)
}

/// [`build`] plus the harvested home-domain fallback — the Welcome-joined
/// member's only tier-2 anchor.
fn build_anchored(
    head: Option<ChainHead>,
    handle: Option<&str>,
    home_domain: Option<&str>,
    answer: Option<VerifiedSuccession>,
) -> (Witness, Arc<StubAnchors>, Arc<SpyChainSource>) {
    let anchors = Arc::new(StubAnchors {
        head,
        handle: handle.map(String::from),
        home_domain: home_domain.map(String::from),
        remembered: Mutex::new(Vec::new()),
    });
    let source = Arc::new(SpyChainSource {
        answer,
        dials: Mutex::new(Vec::new()),
    });
    (
        ChainWitness::new(Shared(Arc::clone(&anchors)), Shared(Arc::clone(&source))),
        anchors,
        source,
    )
}

fn dials(source: &SpyChainSource) -> Vec<String> {
    source.dials.lock().unwrap().clone()
}

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(f)
}

// ── tier 1: a head the consumer already holds ───────────────────────────────

/// The offline path the coupled-head profile mirror exists for: a member that
/// cached the peer's head verifies the statement with **no network at all**.
#[test]
fn a_cached_head_settles_the_statement_without_any_dial() {
    let f = fixture();
    let (witness, _anchors, source) = build(Some(f.head), Some("alice@example.test"), None);

    let verified = block_on(witness.verify_statement(f.signed)).expect("the cached head verifies");

    assert_eq!(verified.old_actor_id, f.old);
    assert_eq!(verified.new_actor_id, f.successor);
    assert!(
        dials(&source).is_empty(),
        "tier 1 must not dial — a cached head is a complete anchor, and dialling \
         anyway would leave an offline member unable to render continuity"
    );
}

/// A cached head that does *not* authorize this statement is not a refusal: a
/// RecoveryKey replacement legitimately advances the chain past what this
/// member saw. Falling through to the walk (which still receives the stale head
/// as the rewrite/truncation guard) is what stops one stale cache blinding a
/// member permanently.
#[test]
fn a_stale_cached_head_falls_through_to_the_walk_rather_than_refusing() {
    let f = fixture();
    let walked = VerifiedSuccession {
        old_actor_id: f.old,
        new_actor_id: f.successor,
        seq: 2,
        chain_head: f.head,
    };
    let (witness, _anchors, source) = build(
        Some(ChainHead::new([0x99; 32], 1)),
        Some("alice@example.test"),
        Some(walked),
    );

    let verified = block_on(witness.verify_statement(f.signed)).expect("the walk settles it");

    assert_eq!(verified.new_actor_id, f.successor);
    assert_eq!(
        dials(&source),
        vec!["example.test".to_string()],
        "a stale head must fall through to exactly one anchored walk"
    );
}

// ── tier 2: the anchored walk ───────────────────────────────────────────────

/// The anchor comes from the **handle the consumer already knew**, never from
/// anything the statement carried. Asserting the dialled domain is what makes
/// this a test of the rule rather than of the happy path.
#[test]
fn the_walk_is_anchored_on_the_handle_domain_the_consumer_already_knew() {
    let f = fixture();
    let walked = VerifiedSuccession {
        old_actor_id: f.old,
        new_actor_id: f.successor,
        seq: 2,
        chain_head: f.head,
    };
    let (witness, _anchors, source) = build(None, Some("alice@nest.example.test"), Some(walked));

    let verified = block_on(witness.verify_statement(f.signed)).expect("the walk verifies");

    assert_eq!(verified.new_actor_id, f.successor);
    assert_eq!(dials(&source), vec!["nest.example.test".to_string()]);
}

/// `resolve_successor` returns the **terminal** hop, whose `old_actor_id` on a
/// twice-succeeded identity is the *middle* id — an id no participant row
/// bears, so passing it through verbatim would silently re-point nothing. The
/// pair the consumer acts on must be (the identity asked about) → (where the
/// chain ends).
#[test]
fn a_multi_hop_path_repoints_from_the_identity_the_member_actually_holds() {
    let f = fixture();
    let middle = ActorKeypair::from_secret([33u8; 32]).actor_id();
    let terminal = ActorKeypair::from_secret([44u8; 32]).actor_id();
    // What the walk returns for a two-hop path old → middle → terminal.
    let walked = VerifiedSuccession {
        old_actor_id: middle,
        new_actor_id: terminal,
        seq: 5,
        chain_head: f.head,
    };
    let (witness, anchors, _source) = build(None, Some("alice@example.test"), Some(walked));

    let verified = block_on(witness.verify_statement(f.signed)).expect("the walk verifies");

    assert_eq!(
        verified.old_actor_id, f.old,
        "the re-point must start at the id the thread's participant row holds, \
         not at the terminal hop's predecessor"
    );
    assert_eq!(
        verified.new_actor_id, terminal,
        "and land on where the chain actually ends"
    );
    assert!(
        anchors.remembered.lock().unwrap().is_empty(),
        "the terminal hop's head is the MIDDLE identity's chain — remembered \
         under the identity asked about, it would refuse that identity's true \
         chain as a rewrite on every later walk"
    );
}

/// A completed walk teaches the consumer a head, so the *next* statement for
/// this identity is anchored instead of TOFU'd again.
#[test]
fn a_completed_walk_remembers_the_head_it_established() {
    let f = fixture();
    let established = ChainHead::new([0x5C; 32], 7);
    let walked = VerifiedSuccession {
        old_actor_id: f.old,
        new_actor_id: f.successor,
        seq: 2,
        chain_head: established,
    };
    let (witness, anchors, _source) = build(None, Some("alice@example.test"), Some(walked));

    block_on(witness.verify_statement(f.signed)).expect("the walk verifies");

    assert_eq!(
        *anchors.remembered.lock().unwrap(),
        vec![(f.old, established)]
    );
}

// ── degradation: everything that leaves the statement a claim ───────────────

/// No independent anchor at all ⇒ the statement stays a claim, and — the part
/// that matters — **no dial is attempted**, mirroring § Propagation's
/// federation rule that an identity with no addressable anchor is refused
/// rather than TOFU'd from the bytes that arrived.
#[test]
fn an_identity_with_no_handle_is_refused_without_dialling_anywhere() {
    let f = fixture();
    let (witness, _anchors, source) = build(None, None, None);

    assert!(block_on(witness.verify_statement(f.signed)).is_none());
    assert!(
        dials(&source).is_empty(),
        "with nothing to anchor on there is nowhere legitimate to dial"
    );
}

/// A handle with no domain is not a domain — it must not become one by accident
/// (an empty-string dial, or the whole handle used as a host).
#[test]
fn a_handle_without_a_domain_is_refused_rather_than_dialled_as_one() {
    let f = fixture();
    let (witness, _anchors, source) = build(None, Some("alice"), None);

    assert!(block_on(witness.verify_statement(f.signed)).is_none());
    assert!(dials(&source).is_empty());
}

/// An unreachable anchor degrades to the bare add — and is not re-dialled for
/// the rest of the session. `verify` runs inline on the inbound poll, so a
/// re-delivered statement (a Rule-2 heal re-walk, a resumed sweep's second
/// post) must not put another dial in front of the feed each time.
#[test]
fn an_unreachable_anchor_degrades_and_is_not_re_dialled() {
    let f = fixture();
    let (witness, _anchors, source) = build(None, Some("alice@example.test"), None);

    assert!(block_on(witness.verify_statement(f.signed.clone())).is_none());
    assert!(block_on(witness.verify_statement(f.signed)).is_none());

    assert_eq!(
        dials(&source).len(),
        1,
        "the second delivery must be answered from the session verdict, not a second dial"
    );
}

/// A verified identity is likewise answered from the memo — re-delivery is
/// idempotent and free.
#[test]
fn a_verified_identity_is_answered_from_the_memo_on_re_delivery() {
    let f = fixture();
    let (witness, _anchors, source) = build(Some(f.head), Some("alice@example.test"), None);

    let first = block_on(witness.verify_statement(f.signed.clone())).expect("verifies");
    let second = block_on(witness.verify_statement(f.signed)).expect("verifies again");

    assert_eq!(first.new_actor_id, second.new_actor_id);
    assert!(dials(&source).is_empty());
}

// ── the durable anchor: `ThreadParticipantAnchors` across a relaunch ─────────
//
// Everything above drives a stub, which is right for pinning the *policy*.
// These four drive the **real** anchors type over the real `PeerAnchorStore`
// seam, because the property they pin is not a policy at all: it is that the
// head a walk established is still there after the process died. An in-memory
// anchor makes every relaunch a first contact, which is precisely the state the
// rewrite/truncation guard cannot fire in.

/// The account's anchor store, in memory — a stand-in for the plane door
/// (`AccountStoreHandle::{peer_anchors, merge_peer_anchors}`), not for the
/// anchors. The door's join is pinned in `fauna-account-plane`; what matters
/// here is that two anchors instances see the same rows, and that a merge is
/// the shipped rule (`PeerAnchors::merge`) answering what now rests.
#[derive(Default)]
struct MemoryAnchors {
    anchors: Mutex<Option<PeerAnchors>>,
    saves: Mutex<usize>,
}

#[async_trait]
impl PeerAnchorStore for Shared<MemoryAnchors> {
    async fn peer_anchors(&self) -> Result<PeerAnchors, String> {
        Ok(self.0.anchors.lock().unwrap().clone().unwrap_or_default())
    }
    async fn merge_peer_anchors(&self, replica: PeerAnchors) -> Result<PeerAnchors, String> {
        let mut slot = self.0.anchors.lock().unwrap();
        let joined = slot.clone().unwrap_or_default().merge(&replica);
        *slot = Some(joined.clone());
        *self.0.saves.lock().unwrap() += 1;
        Ok(joined)
    }
}

/// Lend `store` to `manager` — what the account-store-ready edge does in
/// production (`conversation_seams::wire_parts`) — and hand the manager back.
fn lent(
    manager: &Arc<fauna_conversations::ConversationsManager>,
    store: Arc<dyn PeerAnchorStore>,
) -> &Arc<fauna_conversations::ConversationsManager> {
    manager.register_peer_anchor_store(Some(store));
    manager
}

/// Anchors over an EMPTY thread store — the head-at-rest tests below never
/// consult its threads (`known_handle` answers `None`, the no-anchor arm), but
/// the store is lent through a manager, so each call leaks one empty manager
/// to keep the anchors' `Weak` live for the test's life, the way a running
/// session keeps its own.
fn anchors_over(store: &Arc<MemoryAnchors>) -> fauna_client_recovery::ThreadParticipantAnchors {
    let manager = fauna_conversations::ConversationsManager::new();
    let anchors = anchors_over_manager(&manager, store);
    std::mem::forget(manager);
    anchors
}

/// **The regression this row exists to close.** A walk establishes a head; the
/// app dies; a fresh anchors instance over the same config still knows it. With
/// the pre-2026-08-06 in-memory map this assertion is unreachable by
/// construction — the second instance starts empty and TOFUs.
#[test]
fn a_remembered_head_survives_a_relaunch() {
    let f = fixture();
    let store = Arc::new(MemoryAnchors::default());

    block_on(anchors_over(&store).remember_head(&f.old, f.head));

    let relaunched = anchors_over(&store);
    assert_eq!(
        block_on(relaunched.known_head(&f.old)),
        Some(f.head),
        "a cold start must recover the head this fleet already saw"
    );
    assert_eq!(
        block_on(relaunched.known_head(&f.successor)),
        None,
        "and must still report honest first contact for an identity it never saw"
    );
}

/// The join, end to end: the durable head reaches the **policy**, settling a
/// statement at tier 1 with no dial at all — which is the offline path the whole
/// cache exists for, and the one an in-memory map never reaches after a
/// relaunch.
#[test]
fn a_relaunched_consumer_settles_the_statement_from_its_stored_head_without_dialling() {
    let f = fixture();
    let store = Arc::new(MemoryAnchors::default());
    block_on(anchors_over(&store).remember_head(&f.old, f.head));

    let source = Arc::new(SpyChainSource {
        answer: None,
        dials: Mutex::new(Vec::new()),
    });
    let witness = ChainWitness::new(anchors_over(&store), Shared(Arc::clone(&source)));

    let verified = block_on(witness.verify_statement(f.signed.clone()))
        .expect("the stored head authorizes the statement");
    assert_eq!(verified.new_actor_id, f.successor);
    assert!(
        dials(&source).is_empty(),
        "a head read from rest must settle it with no network: {:?}",
        dials(&source)
    );
}

/// Monotonic **at rest**, not merely in memory. A head that went backwards is
/// the reset an attacker needs, so a relaunched consumer that is handed a lower
/// `seq` keeps the higher one it already had.
#[test]
fn a_stored_head_never_rewinds() {
    let f = fixture();
    let store = Arc::new(MemoryAnchors::default());
    let high = ChainHead::new(f.head.recovery_pubkey, 9);
    let low = ChainHead::new([0xAB; 32], 3);

    block_on(anchors_over(&store).remember_head(&f.old, high));
    block_on(anchors_over(&store).remember_head(&f.old, low));

    assert_eq!(
        block_on(anchors_over(&store).known_head(&f.old)),
        Some(high),
        "the lower seq must not displace what was already seen"
    );
}

/// A re-delivered statement — a Rule-2 heal re-walk, a resumed sweep's second
/// post — re-establishes the same head. Writing it again would store identical
/// bytes on a plane every device merges, so it must not.
#[test]
fn re_remembering_the_same_head_writes_nothing() {
    let f = fixture();
    let store = Arc::new(MemoryAnchors::default());
    let anchors = anchors_over(&store);

    block_on(anchors.remember_head(&f.old, f.head));
    let after_first = *store.saves.lock().unwrap();
    block_on(anchors.remember_head(&f.old, f.head));

    assert_eq!(
        *store.saves.lock().unwrap(),
        after_first,
        "the second call learned nothing and must write nothing"
    );
}

/// The **second** monotonic guard, and the one the in-memory cache hides: a
/// concurrent device advances the stored head past what this session hydrated,
/// and this session then learns something newer-than-its-cache but older-than-
/// the-store. The cache says "news", the store must still say "no".
///
/// Without this the store-side check is unreachable — every path into it is
/// short-circuited by the cache, so deleting it would leave the whole suite
/// green while a behind replica could lower another device's anchor, which is
/// precisely the reset the guard exists to refuse.
#[test]
fn a_head_a_peer_device_already_advanced_past_is_not_written_back() {
    let f = fixture();
    let store = Arc::new(MemoryAnchors::default());
    let anchors = anchors_over(&store);

    // This session learns seq 3 and persists it.
    block_on(anchors.remember_head(&f.old, ChainHead::new([0xA1; 32], 3)));
    // Another of the user's devices then advances the same identity to seq 9.
    {
        let mut slot = store.anchors.lock().unwrap();
        let cfg = slot.as_mut().expect("the first write stored a config");
        cfg.remember_chain_head(f.old, ChainHead::new([0xB2; 32], 9));
    }
    let saves_before = *store.saves.lock().unwrap();

    // Now this session's walk establishes seq 5 — newer than its own cache (3),
    // older than what rests (9).
    block_on(anchors.remember_head(&f.old, ChainHead::new([0xC3; 32], 5)));

    assert_eq!(
        *store.saves.lock().unwrap(),
        saves_before,
        "a head the store already leads must not provoke a write"
    );
    assert_eq!(
        block_on(anchors_over(&store).known_head(&f.old)),
        Some(ChainHead::new([0xB2; 32], 9)),
        "and must not lower what the peer device recorded"
    );
}

// ── the handle half of the real anchors: who can anchor whom ────────────────
//
// Everything above drives the real anchors' **head** half over an EMPTY
// manager, so `known_handle` — the tier-2 anchor source — has never been
// exercised over a real thread by any test in this crate. The two below do
// exactly that, over the two rosters production actually writes, because the
// type's own doc comment rests on a claim about them: *"a member that receives
// a statement is by construction in a thread with the identity it names (which
// is where the handle comes from)"*.

/// The thread a member gets from `ingest_welcome` — the canonical member seat.
///
/// Built exactly as `backends::fauna_mls::ingest_welcome` builds it: the roster
/// comes from `MlsEngine::group_members`, and a leaf credential carries no
/// handle, so every participant is written with `handle: String::new()`.
fn welcome_joined_thread(peer: ActorId) -> Arc<fauna_conversations::ConversationsManager> {
    let manager = fauna_conversations::ConversationsManager::new();
    manager.materialize_conv_thread(
        "c0ffee".to_string(),
        vec![fauna_conversations::TypedAddress::Fauna {
            handle: String::new(),
            actor_id: peer,
        }],
    );
    manager
}

/// The thread the person who ran the *add* holds — a resolved address, so the
/// handle is there **and the owner's own gesture put it there**, which is the
/// provenance tier 2 reads (`create_mls_group` is the test-only stand-in for
/// the compose path and marks its seats exactly as `send_new_thread` does).
/// The control for the test below: it is what makes the failure a statement
/// about **which side of the Welcome** you are on, rather than about
/// `known_handle` being broken outright.
fn resolver_side_thread(
    peer: ActorId,
    handle: &str,
) -> Arc<fauna_conversations::ConversationsManager> {
    let manager = fauna_conversations::ConversationsManager::new();
    manager.create_mls_group(vec![fauna_conversations::TypedAddress::Fauna {
        handle: handle.to_string(),
        actor_id: peer,
    }]);
    manager
}

/// The Welcome-joined thread AFTER the room home named the peer — the row the
/// id-keyed roster read produces on a same-nest member since 2026-09-10
/// (`conversation-rooms.md` § Implementation status today). It renders by
/// name, and the name is the channel host's answer, not the owner's gesture.
fn roster_named_thread(
    peer: ActorId,
    handle: &str,
) -> Arc<fauna_conversations::ConversationsManager> {
    let manager = fauna_conversations::ConversationsManager::new();
    let thread = manager.materialize_conv_thread(
        "c0ffee".to_string(),
        vec![fauna_conversations::TypedAddress::Fauna {
            handle: String::new(),
            actor_id: peer,
        }],
    );
    manager.apply_resolved_handles(thread.clone(), &[(peer, handle.to_string())]);
    let detail = manager.thread_detail(thread).expect("the thread exists");
    assert_eq!(
        detail.participant_displays,
        vec![handle.to_string()],
        "the fixture's premise: the room-home name is what the row RENDERS"
    );
    manager
}

/// Anchors over a manager the CALLER keeps alive — the anchors hold it only
/// weakly (the ownership pin at the end of this file), so a test that let its
/// manager drop would be anchoring over an empty thread store.
fn anchors_over_manager(
    manager: &Arc<fauna_conversations::ConversationsManager>,
    store: &Arc<MemoryAnchors>,
) -> fauna_client_recovery::ThreadParticipantAnchors {
    fauna_client_recovery::ThreadParticipantAnchors::new(Arc::downgrade(lent(
        manager,
        Arc::new(Shared(Arc::clone(store))),
    )))
}

/// **The member who joined by Welcome must be able to anchor the peer who added
/// them.** That member is the whole audience of the in-group statement: the
/// ceremony posts it so *members* render continuity, and a member who cannot
/// anchor the succeeding identity renders the bare add instead — a stranger
/// where their correspondent was.
///
/// Formerly RED + `#[ignore]`d (the gate); the gate is answered by the
/// peer-profile harvest (`identity-succession.md` § The succession statement →
/// *the peer-profile harvest*), and this pin is re-anchored to the ratified
/// shape — **strictly stronger** than the `known_handle`-presence probe it
/// replaces: the real harvest runs against a stub nest serving the peer's
/// signed profile, the real anchors read the store it seeded, and the witness
/// settles the statement **at tier 1, with zero dials** — the member verifies
/// offline, exactly what the coupled-head profile mirror was built for.
#[test]
fn a_member_who_joined_by_welcome_can_anchor_the_peer_who_added_them() {
    let f = fixture();
    let store = Arc::new(MemoryAnchors::default());

    // The ordinary read path: the member's client harvests the peer's profile
    // at join. The transport is a stub nest — the envelope, not the transport,
    // carries the trust.
    let nest = StubProfileNest {
        body: Some(signed_profile_bytes(&old_keypair(), Some(f.head), None)),
    };
    let outcome = block_on(harvest_peer_anchor(
        &nest,
        &f.old,
        &Shared(Arc::clone(&store)),
    ));
    assert_eq!(
        outcome,
        HarvestOutcome::Seeded(PeerAnchorSeed {
            seeded_head: true,
            seeded_domain: false,
            marked_outrun: false,
        })
    );

    let manager = welcome_joined_thread(f.old);
    let anchors = anchors_over_manager(&manager, &store);
    let source = Arc::new(SpyChainSource {
        answer: None,
        dials: Mutex::new(Vec::new()),
    });
    let witness = ChainWitness::new(anchors, Shared(Arc::clone(&source)));

    let verified =
        block_on(witness.verify_statement(f.signed)).expect("the harvested head settles it");
    assert_eq!(verified.new_actor_id, f.successor);
    assert!(
        dials(&source).is_empty(),
        "a harvested head is a tier-1 anchor: the member verifies offline"
    );
}

/// **The page-read arm of harvest rule 4**: a profile page already fetched and
/// verified the peer's bytes to render them, so the harvest consumes those
/// bytes directly — no second fetch — through the same one-door gate, and the
/// witness then settles a statement about that peer at tier 1 with zero dials.
/// This is the shared face the per-app profile-page call sites ride
/// (`identity-succession.md` § the peer-profile harvest, "on the profile
/// page").
#[test]
fn a_page_read_seeds_the_store_from_bytes_it_already_holds() {
    let f = fixture();
    let store = Arc::new(MemoryAnchors::default());
    let bytes = signed_profile_bytes(&old_keypair(), Some(f.head), None);

    let outcome = block_on(fauna_client_recovery::harvest::seed_profile_bytes(
        &f.old,
        &bytes,
        &Shared(Arc::clone(&store)),
    ));
    assert_eq!(
        outcome,
        HarvestOutcome::Seeded(PeerAnchorSeed {
            seeded_head: true,
            seeded_domain: false,
            marked_outrun: false,
        })
    );

    let manager = welcome_joined_thread(f.old);
    let anchors = anchors_over_manager(&manager, &store);
    let source = Arc::new(SpyChainSource {
        answer: None,
        dials: Mutex::new(Vec::new()),
    });
    let witness = ChainWitness::new(anchors, Shared(Arc::clone(&source)));
    let verified =
        block_on(witness.verify_statement(f.signed)).expect("the page-read seed settles it");
    assert_eq!(verified.new_actor_id, f.successor);
    assert!(
        dials(&source).is_empty(),
        "a page-read seed is a tier-1 anchor: the member verifies offline"
    );
}

/// The control, and it is what localizes the failure above: the *resolver* side
/// — the person who typed the address and ran the add — does hold the handle.
/// So the anchor is available exactly on the side that needs it least, and
/// absent on the side the statement is addressed to.
#[test]
fn the_side_that_ran_the_add_can_anchor_the_peer_it_resolved() {
    let f = fixture();
    let store = Arc::new(MemoryAnchors::default());
    let manager = resolver_side_thread(f.old, "alice@example.test");
    let anchors = anchors_over_manager(&manager, &store);

    assert_eq!(
        block_on(anchors.known_handle(&f.old)).as_deref(),
        Some("alice@example.test"),
        "the resolved-address side must still anchor, or this pair says nothing \
         about which side of the Welcome the gap is on"
    );
}

/// **A name the room home served is not a tier-2 anchor** (ratified
/// 2026-09-15, `identity-succession.md` § The succession statement → *which
/// participant handles anchor tier 2*). Since the id-keyed roster read, a
/// same-nest member's Welcome-joined row renders by name before the sweep
/// reaches it — and that name is the channel host's answer, the party the
/// URL rule already refuses as an anchor. So the witness must read
/// provenance, never the rendered string: with the room-home name on the
/// row and a signed-profile domain in the store, the walk dials the
/// **harvested** domain, and the host-served one is never dialled.
///
/// The control beside it, `the_side_that_ran_the_add_can_anchor_the_peer_it_resolved`,
/// renders the same shape of string on a row the owner typed and DOES anchor
/// — so the pair pins that the difference is provenance, not the string.
#[test]
fn a_roster_named_handle_does_not_anchor_the_walk() {
    let f = fixture();
    let store = Arc::new(MemoryAnchors::default());
    let manager = roster_named_thread(f.old, "alice@host.example");
    let anchors = anchors_over_manager(&manager, &store);
    assert_eq!(
        block_on(anchors.known_handle(&f.old)),
        None,
        "a room-home name renders on the row and still answers no handle: the \
         witness reads provenance, not the string"
    );

    // Now the harvest lands the peer's own signed-profile domain, and the
    // walk must follow THAT — not the domain the room home put on the row.
    let nest = StubProfileNest {
        body: Some(signed_profile_bytes(
            &old_keypair(),
            None,
            Some("https://alice.example/"),
        )),
    };
    block_on(harvest_peer_anchor(
        &nest,
        &f.old,
        &Shared(Arc::clone(&store)),
    ));
    let verdict = VerifiedSuccession {
        old_actor_id: f.old,
        new_actor_id: f.successor,
        seq: 2,
        chain_head: ChainHead::new([1u8; 32], 2),
    };
    let source = Arc::new(SpyChainSource {
        answer: Some(verdict),
        dials: Mutex::new(Vec::new()),
    });
    let witness = ChainWitness::new(
        anchors_over_manager(&manager, &store),
        Shared(Arc::clone(&source)),
    );
    let verified = block_on(witness.verify_statement(f.signed)).expect("the walk verifies");
    assert_eq!(verified.new_actor_id, f.successor);
    assert_eq!(
        dials(&source),
        vec!["alice.example".to_string()],
        "the dial follows the signed profile's own domain; the room-home name's \
         `host.example` is never a dial target"
    );
}

/// **The sweep attempts a row the room home has named** (the same ruling's
/// harvest half). The name-keyed skip this retires was exactly the member-path
/// journey's red on linux and tui: the roster read named alice on bob's row
/// before his 5 s sweep reached it, the sweep skipped every named row, and
/// bob never held the offline tier-1 head the statement needed.
#[test]
fn the_sweep_attempts_a_roster_named_row() {
    let f = fixture();
    let store = Arc::new(MemoryAnchors::default());
    let manager = roster_named_thread(f.old, "alice@host.example");
    let nest = CountingProfileNest {
        inner: StubProfileNest {
            body: Some(signed_profile_bytes(&old_keypair(), Some(f.head), None)),
        },
        fetches: Default::default(),
    };
    let redrive = RecordingRedrive::default();
    let log = fauna_client_recovery::harvest::HarvestLog::default();
    let mut sweep = fauna_client_recovery::harvest::PeerAnchorSweepState::new();

    block_on(sweep.run_pass(
        lent(&manager, Arc::new(Shared(Arc::clone(&store)))),
        &nest,
        &log,
        &redrive,
    ));
    assert_eq!(
        nest.fetches.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "a named row is attempted like any other — the name is not a skip"
    );
    assert_eq!(
        stored_head(&store, &f.old),
        Some(f.head),
        "and the seed lands: the offline tier-1 head a rendered name never supplied"
    );
}

/// **An owner-typed handle is attempted too.** The retired skip spared every
/// named row, so an owner-typed same-nest peer never had an offline anchor;
/// the ruling attempts every Fauna row and lets reach bound the store instead.
#[test]
fn the_sweep_attempts_an_owner_typed_row_as_well() {
    let f = fixture();
    let store = Arc::new(MemoryAnchors::default());
    let manager = resolver_side_thread(f.old, "alice@example.test");
    let nest = CountingProfileNest {
        inner: StubProfileNest {
            body: Some(signed_profile_bytes(&old_keypair(), Some(f.head), None)),
        },
        fetches: Default::default(),
    };
    let redrive = RecordingRedrive::default();
    let log = fauna_client_recovery::harvest::HarvestLog::default();
    let mut sweep = fauna_client_recovery::harvest::PeerAnchorSweepState::new();

    block_on(sweep.run_pass(
        lent(&manager, Arc::new(Shared(Arc::clone(&store)))),
        &nest,
        &log,
        &redrive,
    ));
    assert_eq!(
        nest.fetches.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "the owner-typed row is fetched once"
    );
    assert_eq!(
        stored_head(&store, &f.old),
        Some(f.head),
        "and seeds the offline head the handle alone never gave this member"
    );
}

// ── the peer-profile harvest: the ratified member-path anchor source ─────────
//
// `identity-succession.md` § The succession statement → *the peer-profile
// harvest* (the gate). The pins below are the harvest's four rules made
// falsifiable: signed-path only, actor match, seed-never-advance, and the
// no-anchor-verdict-is-not-memoized rule that keeps a harvest landing after a
// statement's first delivery from being wedged out for the session.

/// The keypair behind `fixture()`'s `old` actor — the peer whose profile the
/// member harvests.
fn old_keypair() -> ActorKeypair {
    ActorKeypair::from_secret([11u8; 32])
}

/// A signed profile as the peer's own client would publish it: the
/// `sign_and_pack` envelope `decode_profile` verifies against the profile's
/// own actor id.
fn signed_profile_bytes(
    kp: &ActorKeypair,
    head: Option<ChainHead>,
    nest_url: Option<&str>,
) -> Vec<u8> {
    fauna_core::encoding::sign_and_pack(kp, &profile_of(kp.actor_id(), head, nest_url))
        .expect("the fixture profile signs")
}

fn profile_of(
    actor_id: ActorId,
    head: Option<ChainHead>,
    nest_url: Option<&str>,
) -> fauna_core::data::Profile {
    fauna_core::data::Profile {
        actor_id,
        display_name: Some("Alice".to_string()),
        bio: None,
        avatar: None,
        banner: None,
        links: Vec::new(),
        nests: nest_url
            .map(|url| {
                vec![fauna_core::data::NestEntry {
                    nest_id: vec![7u8; 32],
                    url: url.to_string(),
                    roles: Vec::new(),
                }]
            })
            .unwrap_or_default(),
        admin_nests: Vec::new(),
        load_hint: None,
        inbox_mode: fauna_core::data::InboxMode::Open,
        recovery_head: head,
        updated_at: Timestamp::now(),
    }
}

/// The transport for the harvest — a nest serving exactly one profile body (or
/// a rejection). Deliberately dumb: the harvest's trust must come from the
/// envelope inside the body, never from anything this stub does.
struct StubProfileNest {
    body: Option<Vec<u8>>,
}

#[derive(Debug)]
struct StubProfileError {
    rejection: bool,
}

impl std::fmt::Display for StubProfileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "stub profile error (rejection: {})", self.rejection)
    }
}

impl fauna_protocol::RpcErrorClass for StubProfileError {
    fn is_rejection(&self) -> bool {
        self.rejection
    }
}

impl fauna_protocol::RpcRequester for StubProfileNest {
    type Error = StubProfileError;

    async fn request<Req, Reply>(
        &self,
        kind: &'static str,
        _payload: Req,
    ) -> Result<Reply, Self::Error>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        assert_eq!(kind, "fauna.profile.get", "the harvest speaks one kind");
        let Some(body) = &self.body else {
            return Err(StubProfileError { rejection: true });
        };
        let reply = fauna_protocol::profile::ProfileGetReply {
            body: fauna_protocol::ByteBuf::from(body.clone()),
            extra: Default::default(),
        };
        let bytes = fauna_protocol::encode_canonical(&reply).unwrap();
        Ok(fauna_protocol::decode_strict(&bytes).unwrap())
    }
}

fn stored_head(store: &Arc<MemoryAnchors>, actor: &ActorId) -> Option<ChainHead> {
    store
        .anchors
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|cfg| cfg.known_chain_head(actor))
}

/// Harvest rule 1: an unsigned bare profile is any host's fabrication — it
/// does not even decode (`decode_profile` is signed-only), and anchors
/// **nothing**.
#[test]
fn an_unsigned_profile_is_refused_and_seeds_nothing() {
    let f = fixture();
    let store = Arc::new(MemoryAnchors::default());
    let bare = fauna_core::encoding::canonical_encode(&profile_of(
        f.old,
        Some(f.head),
        Some("https://alice.example/"),
    ))
    .unwrap();
    let nest = StubProfileNest { body: Some(bare) };

    let outcome = block_on(harvest_peer_anchor(
        &nest,
        &f.old,
        &Shared(Arc::clone(&store)),
    ));

    assert_eq!(
        outcome,
        HarvestOutcome::Refused(PeerAnchorRefusal::Undecodable)
    );
    assert!(stored_head(&store, &f.old).is_none());
}

/// Harvest rule 2: envelope verification is self-consistent, not
/// self-targeted — a host serving a *different* identity's genuine signed
/// profile is refused, or any nest could anchor any actor to any chain.
#[test]
fn a_profile_naming_a_different_identity_is_refused() {
    let f = fixture();
    let store = Arc::new(MemoryAnchors::default());
    let other = ActorKeypair::from_secret([33u8; 32]);
    let nest = StubProfileNest {
        body: Some(signed_profile_bytes(&other, Some(f.head), None)),
    };

    let outcome = block_on(harvest_peer_anchor(
        &nest,
        &f.old,
        &Shared(Arc::clone(&store)),
    ));

    assert_eq!(
        outcome,
        HarvestOutcome::Refused(PeerAnchorRefusal::WrongActor)
    );
    assert!(stored_head(&store, &f.old).is_none());
}

/// Harvest rule 5: a harvested host over RFC 1035's 253
/// bytes is refused at the one door, and NOTHING is seeded.
///
/// The vector this closes is availability, not impersonation.
/// `peer_anchor_domains` rests in a size-capped store with no
/// prune, no eviction and a per-actor UNION merge, and the sweep fills it from
/// peers' signed profiles with no human in the loop — so before this bound, a
/// few group peers each publishing one ~400 KiB host pushed a victim's sealed
/// config past the cap, after which every `save_cas` failed on every device,
/// permanently, with no app affordance to undo it. The assertion that matters
/// is therefore the second one: refusing while still storing the host would
/// close nothing.
#[test]
fn an_over_length_harvested_host_is_refused_and_seeds_nothing() {
    let f = fixture();
    let store = Arc::new(MemoryAnchors::default());
    // 254 bytes of host — one past the ceiling, so this pins the boundary
    // rather than an arbitrary "obviously huge" value.
    let long_host = "a".repeat(254);
    let nest = StubProfileNest {
        body: Some(signed_profile_bytes(
            &old_keypair(),
            Some(f.head),
            Some(&format!("https://{long_host}/")),
        )),
    };

    let outcome = block_on(harvest_peer_anchor(
        &nest,
        &f.old,
        &Shared(Arc::clone(&store)),
    ));

    assert_eq!(
        outcome,
        HarvestOutcome::Refused(PeerAnchorRefusal::HostTooLong)
    );
    let cfg = store.anchors.lock().unwrap().clone();
    if let Some(cfg) = cfg {
        assert_eq!(
            cfg.known_anchor_domain(&f.old),
            None,
            "the over-length host must not be stored — storing it IS the bug"
        );
    }
    assert!(
        stored_head(&store, &f.old).is_none(),
        "every PeerAnchorRefusal arm promises nothing was seeded, and this profile carried a chain head the door must not have written first"
    );
}

/// The boundary's other side: exactly 253 bytes is admitted, so the bound is a
/// ceiling rather than a blanket refusal of long-but-legal hosts.
#[test]
fn a_host_at_exactly_the_ceiling_is_still_harvested() {
    let f = fixture();
    let store = Arc::new(MemoryAnchors::default());
    let host = format!("{}.example", "a".repeat(245));
    assert_eq!(host.len(), 253, "the fixture must sit exactly on the bound");
    let nest = StubProfileNest {
        body: Some(signed_profile_bytes(
            &old_keypair(),
            None,
            Some(&format!("https://{host}/")),
        )),
    };

    let outcome = block_on(harvest_peer_anchor(
        &nest,
        &f.old,
        &Shared(Arc::clone(&store)),
    ));

    assert_eq!(
        outcome,
        HarvestOutcome::Seeded(fauna_core::data::PeerAnchorSeed {
            seeded_head: false,
            seeded_domain: true,
            marked_outrun: false,
        })
    );
    let cfg = store.anchors.lock().unwrap().clone().unwrap();
    assert_eq!(
        cfg.known_anchor_domain(&f.old).as_deref(),
        Some(host.as_str())
    );
}

/// Harvest rule 3, the head half: a harvest may **seed** an empty slot and
/// must never **advance** a held head — a window-doctored profile could stamp
/// an arbitrarily high `seq` and turn the rewrite/truncation guard against the
/// genuine chain. Advancing is the walk's privilege alone.
#[test]
fn a_harvested_head_seeds_an_empty_slot_and_never_advances_a_held_one() {
    let f = fixture();
    let store = Arc::new(MemoryAnchors::default());

    // Seed: empty slot fills.
    let nest = StubProfileNest {
        body: Some(signed_profile_bytes(&old_keypair(), Some(f.head), None)),
    };
    let outcome = block_on(harvest_peer_anchor(
        &nest,
        &f.old,
        &Shared(Arc::clone(&store)),
    ));
    assert_eq!(
        outcome,
        HarvestOutcome::Seeded(PeerAnchorSeed {
            seeded_head: true,
            seeded_domain: false,
            marked_outrun: false,
        })
    );
    assert_eq!(stored_head(&store, &f.old).map(|h| h.seq), Some(1));

    // Never advance: a re-harvest carrying a "newer" head (the doctored-window
    // shape) leaves the held head exactly where it was. What it may do — and
    // all it may do — is DEMOTE it (the outrun pins below own that half).
    let doctored = StubProfileNest {
        body: Some(signed_profile_bytes(
            &old_keypair(),
            Some(ChainHead::new([9u8; 32], 9)),
            None,
        )),
    };
    let outcome = block_on(harvest_peer_anchor(
        &doctored,
        &f.old,
        &Shared(Arc::clone(&store)),
    ));
    assert_eq!(
        outcome,
        HarvestOutcome::Seeded(PeerAnchorSeed {
            seeded_head: false,
            seeded_domain: false,
            marked_outrun: true,
        })
    );
    assert_eq!(
        stored_head(&store, &f.old),
        Some(f.head),
        "a harvest must never advance a held head — that is the walk's privilege"
    );
}

// ── the outrun rule: a rotated-away kit must not settle offline ─────────────
//
// `identity-succession.md` § The succession statement → *What a held head may
// settle offline*. `SignedIdentitySuccession::verify` checks the key, the two
// signatures and an advancing `seq` — nothing about whether the key is still
// the registered one — and a RecoveryKey the owner rotated away signs valid
// statements for ever. So tier 1 against a head naming that key is the one
// door a retired kit still opens (tier 2 verifies the chain first and refuses).

/// What the owner's client publishes when a kit rotation lands: the profile
/// mirror now names the NEW key at the next `seq`.
fn rotated_head(f: &Fixture) -> ChainHead {
    ChainHead::new([0x52; 32], f.head.seq + 1)
}

/// **The pin.** A member anchored the peer before the rotation; the owner
/// rotates the kit; the member's next ordinary harvest reads the profile that
/// now claims past the held head. From that read on, the retired kit's
/// statement — every signature genuine, `seq` advancing — does NOT settle at
/// tier 1: it has to be walked, and the walk (which verifies the chain first)
/// is where a rotated-away key fails.
///
/// The control inside the test is the residual the rule ratifies: before the
/// member's harvest has seen the rotation, the same statement settles.
#[test]
fn a_retired_kits_statement_does_not_settle_against_a_head_the_profile_outran() {
    let f = fixture();

    // Control — the ratified residual. No harvest has seen the rotation yet.
    {
        let store = Arc::new(MemoryAnchors::default());
        let seed = StubProfileNest {
            body: Some(signed_profile_bytes(&old_keypair(), Some(f.head), None)),
        };
        block_on(harvest_peer_anchor(
            &seed,
            &f.old,
            &Shared(Arc::clone(&store)),
        ));
        let source = Arc::new(SpyChainSource {
            answer: None,
            dials: Mutex::new(Vec::new()),
        });
        let manager = welcome_joined_thread(f.old);
        let witness = ChainWitness::new(
            anchors_over_manager(&manager, &store),
            Shared(Arc::clone(&source)),
        );
        assert!(
            block_on(witness.verify_statement(f.signed.clone())).is_some(),
            "the control: an un-demoted head settles this very statement, so the \
             refusal below is the mark's doing and nothing else's"
        );
    }

    let store = Arc::new(MemoryAnchors::default());
    let before = StubProfileNest {
        body: Some(signed_profile_bytes(
            &old_keypair(),
            Some(f.head),
            Some("https://home.example.test"),
        )),
    };
    block_on(harvest_peer_anchor(
        &before,
        &f.old,
        &Shared(Arc::clone(&store)),
    ));

    // The rotation lands; a later session's sweep re-reads the peer.
    let after = StubProfileNest {
        body: Some(signed_profile_bytes(
            &old_keypair(),
            Some(rotated_head(&f)),
            Some("https://home.example.test"),
        )),
    };
    assert_eq!(
        block_on(harvest_peer_anchor(
            &after,
            &f.old,
            &Shared(Arc::clone(&store))
        )),
        HarvestOutcome::Seeded(PeerAnchorSeed {
            seeded_head: false,
            seeded_domain: false,
            marked_outrun: true,
        })
    );

    let source = Arc::new(SpyChainSource {
        answer: None, // the real walk refuses: the chain's head is the new key
        dials: Mutex::new(Vec::new()),
    });
    let manager = welcome_joined_thread(f.old);
    let witness = ChainWitness::new(
        anchors_over_manager(&manager, &store),
        Shared(Arc::clone(&source)),
    );

    assert!(
        block_on(witness.verify_statement(f.signed.clone())).is_none(),
        "a statement signed by the rotated-away kit must not settle offline \
         against the head the peer's own profile has outrun"
    );
    assert_eq!(
        dials(&source),
        vec!["home.example.test".to_string()],
        "demoted is not refused: the statement is WALKED, against the domain the \
         member already held — the path an honest post-rotation statement takes"
    );
    let report = witness.observation();
    assert!(
        report.peers[0].held_head_outrun,
        "the report names the demotion"
    );
    assert_eq!(report.peers[0].outcome, WitnessOutcome::WalkFailed);
    assert_eq!(
        stored_head(&store, &f.old),
        Some(f.head),
        "and the held head is still at rest, still the walk's guard"
    );
}

/// The same pin with the anchor store FULL. The demotion flips a flag on an
/// entry the member already holds and takes no slot, so the count ceiling —
/// which refuses new seeds — must not refuse it. It used to: the door raised
/// `StoreFull` ahead of the head match, the sweep settled the peer as refused
/// and released the harvest wait, and the retired kit's statement settled at
/// tier 1. A big roster fills the store organically, and one room's policy
/// names can fill it on purpose, so "full" is a state an attacker can arrange.
#[test]
fn a_full_anchor_store_still_demotes_so_the_retired_kits_statement_is_walked() {
    let f = fixture();
    let store = Arc::new(MemoryAnchors::default());
    let before = StubProfileNest {
        body: Some(signed_profile_bytes(
            &old_keypair(),
            Some(f.head),
            Some("https://home.example.test"),
        )),
    };
    block_on(harvest_peer_anchor(
        &before,
        &f.old,
        &Shared(Arc::clone(&store)),
    ));

    // Strangers fill every remaining slot of both vectors.
    {
        let mut guard = store.anchors.lock().unwrap();
        let cfg = guard.as_mut().expect("the first harvest saved a config");
        for i in 1..fauna_core::data::MAX_PEER_ANCHOR_ENTRIES {
            let mut raw = [0xEEu8; 32];
            raw[..8].copy_from_slice(&(i as u64).to_be_bytes());
            assert!(cfg.seed_chain_head(ActorId(raw), ChainHead::new([3u8; 32], 1)));
            assert!(cfg.seed_anchor_domain(ActorId(raw), format!("peer{i}.example.test")));
        }
        assert_eq!(
            cfg.chain_heads.len(),
            fauna_core::data::MAX_PEER_ANCHOR_ENTRIES
        );
        assert_eq!(
            cfg.anchor_domains.len(),
            fauna_core::data::MAX_PEER_ANCHOR_ENTRIES
        );
    }

    // The rotation lands; a later session's sweep re-reads the peer.
    let after = StubProfileNest {
        body: Some(signed_profile_bytes(
            &old_keypair(),
            Some(rotated_head(&f)),
            Some("https://home.example.test"),
        )),
    };
    // Graded last: the settle below is the harm, this outcome only its cause.
    let outcome = block_on(harvest_peer_anchor(
        &after,
        &f.old,
        &Shared(Arc::clone(&store)),
    ));

    let source = Arc::new(SpyChainSource {
        answer: None, // the real walk refuses: the chain's head is the new key
        dials: Mutex::new(Vec::new()),
    });
    let manager = welcome_joined_thread(f.old);
    let witness = ChainWitness::new(
        anchors_over_manager(&manager, &store),
        Shared(Arc::clone(&source)),
    );
    assert!(
        block_on(witness.verify_statement(f.signed.clone())).is_none(),
        "with a full store the retired kit's statement still must not settle offline"
    );
    assert_eq!(
        dials(&source),
        vec!["home.example.test".to_string()],
        "it is walked, exactly as with room to spare"
    );
    assert!(witness.observation().peers[0].held_head_outrun);
    assert_eq!(
        outcome,
        HarvestOutcome::Seeded(PeerAnchorSeed {
            seeded_head: false,
            seeded_domain: false,
            marked_outrun: true,
        }),
        "a full store refuses new seeds, never the demotion of a held head"
    );
}

/// The mark has to reach a session that is ALREADY answering for the head —
/// sessions run for days, and a demotion that waited for the next launch would
/// leave the retired kit its whole window. The harvest's `Seeded` outcome is
/// what the shared re-drive announces; one announcement, one re-read.
#[test]
fn a_demotion_landing_mid_session_reaches_a_head_the_cache_already_answers_for() {
    let f = fixture();
    let store = Arc::new(MemoryAnchors::default());
    let before = StubProfileNest {
        body: Some(signed_profile_bytes(&old_keypair(), Some(f.head), None)),
    };
    block_on(harvest_peer_anchor(
        &before,
        &f.old,
        &Shared(Arc::clone(&store)),
    ));

    let source = Arc::new(SpyChainSource {
        answer: None,
        dials: Mutex::new(Vec::new()),
    });
    let manager = welcome_joined_thread(f.old);
    let anchors = anchors_over_manager(&manager, &store);
    // The session has already read the store and caches the un-demoted head.
    assert_eq!(block_on(anchors.known_head(&f.old)), Some(f.head));
    assert!(!block_on(anchors.known_head_is_outrun(&f.old)));
    let witness = ChainWitness::new(anchors, Shared(Arc::clone(&source)));

    let after = StubProfileNest {
        body: Some(signed_profile_bytes(
            &old_keypair(),
            Some(rotated_head(&f)),
            None,
        )),
    };
    assert!(matches!(
        block_on(harvest_peer_anchor(&after, &f.old, &Shared(Arc::clone(&store)))),
        HarvestOutcome::Seeded(seed) if seed.marked_outrun
    ));
    block_on(SuccessionWitness::anchor_seed_landed(&witness));

    assert!(
        block_on(witness.verify_statement(f.signed)).is_none(),
        "the same session that cached the head must stop settling on it"
    );
}

/// Only a verified walk clears the mark — by replacing the head it was about.
/// After it, tier 1 works again for the head the walk established.
#[test]
fn a_walk_that_advances_the_head_restores_the_offline_path() {
    let f = fixture();
    let store = Arc::new(MemoryAnchors::default());
    for head in [f.head, rotated_head(&f)] {
        let nest = StubProfileNest {
            body: Some(signed_profile_bytes(&old_keypair(), Some(head), None)),
        };
        block_on(harvest_peer_anchor(
            &nest,
            &f.old,
            &Shared(Arc::clone(&store)),
        ));
    }
    let manager = welcome_joined_thread(f.old);
    let anchors = anchors_over_manager(&manager, &store);
    assert!(block_on(anchors.known_head(&f.old)).is_some());
    assert!(block_on(anchors.known_head_is_outrun(&f.old)));

    block_on(anchors.remember_head(&f.old, rotated_head(&f)));

    assert_eq!(block_on(anchors.known_head(&f.old)), Some(rotated_head(&f)));
    assert!(!block_on(anchors.known_head_is_outrun(&f.old)));
    // And at rest, so the next session starts un-demoted too.
    let relaunched = anchors_over_manager(&manager, &store);
    assert_eq!(
        block_on(relaunched.known_head(&f.old)),
        Some(rotated_head(&f))
    );
    assert!(!block_on(relaunched.known_head_is_outrun(&f.old)));
}

/// Harvest rule 3, the domain half: first-write-wins — a later harvest cannot
/// re-point the dial target.
#[test]
fn a_harvested_domain_fills_once_and_is_never_repointed() {
    let f = fixture();
    let store = Arc::new(MemoryAnchors::default());

    let first = StubProfileNest {
        body: Some(signed_profile_bytes(
            &old_keypair(),
            None,
            Some("https://alice.example:443/"),
        )),
    };
    let outcome = block_on(harvest_peer_anchor(
        &first,
        &f.old,
        &Shared(Arc::clone(&store)),
    ));
    assert_eq!(
        outcome,
        HarvestOutcome::Seeded(PeerAnchorSeed {
            seeded_head: false,
            seeded_domain: true,
            marked_outrun: false,
        })
    );

    let repoint = StubProfileNest {
        body: Some(signed_profile_bytes(
            &old_keypair(),
            None,
            Some("https://thief.example/"),
        )),
    };
    let outcome = block_on(harvest_peer_anchor(
        &repoint,
        &f.old,
        &Shared(Arc::clone(&store)),
    ));
    assert_eq!(outcome, HarvestOutcome::NothingNew);

    let cfg = store.anchors.lock().unwrap().clone().unwrap();
    assert_eq!(
        cfg.known_anchor_domain(&f.old).as_deref(),
        Some("alice.example"),
        "the first-written domain stands (host extracted, port stripped)"
    );
}

/// The harvested domain is a real tier-2 anchor: with no handle on any roster
/// row, the witness dials it.
#[test]
fn a_harvested_domain_anchors_the_walk_when_no_handle_exists() {
    let f = fixture();
    let store = Arc::new(MemoryAnchors::default());
    let nest = StubProfileNest {
        body: Some(signed_profile_bytes(
            &old_keypair(),
            None,
            Some("https://alice.example/"),
        )),
    };
    block_on(harvest_peer_anchor(
        &nest,
        &f.old,
        &Shared(Arc::clone(&store)),
    ));

    let verdict = VerifiedSuccession {
        old_actor_id: f.old,
        new_actor_id: f.successor,
        seq: 2,
        chain_head: ChainHead::new([1u8; 32], 2),
    };
    let manager = welcome_joined_thread(f.old);
    let anchors = anchors_over_manager(&manager, &store);
    let source = Arc::new(SpyChainSource {
        answer: Some(verdict),
        dials: Mutex::new(Vec::new()),
    });
    let witness = ChainWitness::new(anchors, Shared(Arc::clone(&source)));

    let verified = block_on(witness.verify_statement(f.signed)).expect("the walk verifies");
    assert_eq!(verified.new_actor_id, f.successor);
    assert_eq!(
        dials(&source),
        vec!["alice.example".to_string()],
        "with no handle anywhere, the harvested domain is the dial target"
    );
}

/// A no-anchor verdict is **not memoized** — nothing was dialled, so there is
/// nothing to save, and memoizing it would wedge the session: a harvest
/// completing after a statement's first delivery could never be consulted for
/// its re-delivery. Convergence is at most one delivery behind the harvest:
/// the fresh `known_home_domain` read folds a seeded head into the cache, so
/// re-delivery N+1 settles at tier 2 (or tier 1 one delivery later), where the
/// pre-2026-08-10 memo answered `Unproven` forever.
#[test]
fn a_no_anchor_verdict_is_not_memoized_so_a_later_harvest_can_answer() {
    let f = fixture();
    let store = Arc::new(MemoryAnchors::default());
    let manager = welcome_joined_thread(f.old);
    let anchors = anchors_over_manager(&manager, &store);
    let source = Arc::new(SpyChainSource {
        answer: None,
        dials: Mutex::new(Vec::new()),
    });
    let witness = ChainWitness::new(anchors, Shared(Arc::clone(&source)));

    // First delivery: nothing to anchor on, no dial, no verdict…
    assert!(block_on(witness.verify_statement(f.signed.clone())).is_none());
    assert!(dials(&source).is_empty());

    // …then the harvest lands (the join-path sweep finishing after the poll).
    let nest = StubProfileNest {
        body: Some(signed_profile_bytes(&old_keypair(), Some(f.head), None)),
    };
    block_on(harvest_peer_anchor(
        &nest,
        &f.old,
        &Shared(Arc::clone(&store)),
    ));
    // The signal production sends on every `Seeded` outcome, from shared code
    // the app cannot skip: `redrive_parked_successions` calls this before it
    // touches the parked set (`SuccessionWitness::anchor_seed_landed`). Without
    // it the witness answers from the view it read at the first delivery, which
    // is the whole point of the generation guard — a witness that
    // re-read the store unprompted would be the unbounded shape again.
    block_on(SuccessionWitness::anchor_seed_landed(&witness));

    // The re-drive's one re-verify (`redrive_parked_successions` — the parked
    // statement's single second chance, since no re-delivery exists): the
    // seeded head must settle it on the FIRST post-harvest verify. A witness
    // that needed a fold-then-ask-again here would strand the re-drive — it
    // re-parks on refusal and no second harvest event ever comes for a peer
    // whose harvest already seeded.
    let verified = block_on(witness.verify_statement(f.signed))
        .expect("the seeded head answers the first post-harvest verify");
    assert_eq!(verified.new_actor_id, f.successor);
    assert!(
        dials(&source).is_empty(),
        "the seeded head settles it at tier 1 — no dial at any point"
    );
}

/// A [`StubProfileNest`] that counts its fetches — the witness for harvest
/// rule 4's corollary: nothing on the statement path may fetch a profile.
struct CountingProfileNest {
    inner: StubProfileNest,
    fetches: std::sync::atomic::AtomicU32,
}

impl fauna_protocol::RpcRequester for CountingProfileNest {
    type Error = StubProfileError;

    async fn request<Req, Reply>(
        &self,
        kind: &'static str,
        payload: Req,
    ) -> Result<Reply, Self::Error>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        self.fetches
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.inner.request(kind, payload).await
    }
}

/// Harvest rule 4, asserted rather than assumed across the whole race: the
/// statement's delivery and its re-drive cause **no profile fetch** — the one
/// fetch in the sequence is the harvest's own, on its own read-path schedule.
/// (Structurally the witness holds no profile transport at all; this pin is
/// what fails if a refactor ever hands it one.) The parked/re-point half of
/// the same race lives with the backend:
/// `fauna_mls_backend_tests.rs::a_refused_statement_is_parked_and_a_later_harvest_redrive_repoints_it`.
#[test]
fn the_harvest_race_converges_with_no_verify_time_fetch() {
    let f = fixture();
    let store = Arc::new(MemoryAnchors::default());
    let source = Arc::new(SpyChainSource {
        answer: None,
        dials: Mutex::new(Vec::new()),
    });
    let manager = welcome_joined_thread(f.old);
    let witness = ChainWitness::new(
        anchors_over_manager(&manager, &store),
        Shared(Arc::clone(&source)),
    );
    let nest = CountingProfileNest {
        inner: StubProfileNest {
            body: Some(signed_profile_bytes(&old_keypair(), Some(f.head), None)),
        },
        fetches: std::sync::atomic::AtomicU32::new(0),
    };
    let fetches = |n: &CountingProfileNest| n.fetches.load(std::sync::atomic::Ordering::Relaxed);

    // The delivery, losing the race: refused — and it caused no fetch.
    assert!(block_on(witness.verify_statement(f.signed.clone())).is_none());
    assert_eq!(
        fetches(&nest),
        0,
        "a refused statement must not reach for the profile — that fetch would \
         anchor the check on whoever holds the identity key right now (rule 4)"
    );

    // The harvest, on its own schedule: exactly one fetch.
    assert_eq!(
        block_on(harvest_peer_anchor(
            &nest,
            &f.old,
            &Shared(Arc::clone(&store))
        )),
        HarvestOutcome::Seeded(PeerAnchorSeed {
            seeded_head: true,
            seeded_domain: false,
            marked_outrun: false,
        })
    );
    assert_eq!(fetches(&nest), 1);

    // The re-drive's own first act (`redrive_parked_successions`): tell the
    // witness a seed landed. It reads the store, never a profile — the fetch
    // count below is what proves the distinction.
    block_on(SuccessionWitness::anchor_seed_landed(&witness));

    // The re-drive's re-verify: settles, and fetched nothing more — the
    // harvest's own fetch stays the only one in the whole sequence.
    let verified = block_on(witness.verify_statement(f.signed))
        .expect("the parked statement's re-verify settles from the harvested head");
    assert_eq!(verified.new_actor_id, f.successor);
    assert_eq!(
        fetches(&nest),
        1,
        "the re-drive fetches nothing — convergence must not add a verify-time fetch"
    );
    assert!(
        dials(&source).is_empty(),
        "tier 1 settles it offline: {:?}",
        dials(&source)
    );
}

// ──: what a forged statement may cost the member who receives it ─────

/// An anchor store that **counts its reads**.
///
/// Deliberately not [`MemoryAnchors`]: that double's read is free, and free is
/// exactly what let an unbounded per-statement round trip hide inside a green
/// suite for four days (before the `__config` rail retired at closure step
/// (6), every store read was a `KIND_GET` to the nest plus a config unseal; see
/// `config-dissolution.md`). The account plane's read is local, but the bound is the witness's, not the store's: every read counted
/// here happens on the **inline** receive path.
#[derive(Default)]
struct LoadCountingAnchors {
    anchors: Mutex<Option<PeerAnchors>>,
    reads: Mutex<usize>,
    /// A **transient** store outage — the flaky case, distinct from
    /// [`BrokenAnchors`]' permanent one, which no amount of retrying helps and
    /// so cannot tell a cached failure from an honest one.
    down: Mutex<bool>,
}

impl LoadCountingAnchors {
    fn reads(&self) -> usize {
        *self.reads.lock().unwrap()
    }
    fn set_down(&self, down: bool) {
        *self.down.lock().unwrap() = down;
    }
    /// Plant anchors directly, standing in for a harvest that ran earlier.
    fn plant(&self, anchors: PeerAnchors) {
        *self.anchors.lock().unwrap() = Some(anchors);
    }
}

#[async_trait]
impl PeerAnchorStore for Shared<LoadCountingAnchors> {
    async fn peer_anchors(&self) -> Result<PeerAnchors, String> {
        *self.0.reads.lock().unwrap() += 1;
        if *self.0.down.lock().unwrap() {
            return Err("anchor store down".into());
        }
        Ok(self.0.anchors.lock().unwrap().clone().unwrap_or_default())
    }
    async fn merge_peer_anchors(&self, replica: PeerAnchors) -> Result<PeerAnchors, String> {
        if *self.0.down.lock().unwrap() {
            return Err("anchor store down".into());
        }
        let mut slot = self.0.anchors.lock().unwrap();
        let joined = slot.clone().unwrap_or_default().merge(&replica);
        *slot = Some(joined.clone());
        Ok(joined)
    }
}

/// A clock a test steps by hand — the [`ThreadParticipantAnchors::with_clock`]
/// seam, so a backoff is asserted by the clock's position, never by waiting
/// (e2e convention 14).
///
/// [`ThreadParticipantAnchors::with_clock`]: fauna_client_recovery::ThreadParticipantAnchors::with_clock
#[derive(Clone, Default)]
struct TestClock(Arc<std::sync::atomic::AtomicU64>);

impl TestClock {
    fn advance_secs(&self, secs: u64) {
        self.0
            .fetch_add(secs * 1_000_000, std::sync::atomic::Ordering::SeqCst);
    }
    fn rewind_secs(&self, secs: u64) {
        self.0
            .fetch_sub(secs * 1_000_000, std::sync::atomic::Ordering::SeqCst);
    }
    fn reader(&self) -> Arc<dyn Fn() -> Timestamp + Send + Sync> {
        let now = Arc::clone(&self.0);
        Arc::new(move || Timestamp(now.load(std::sync::atomic::Ordering::SeqCst)))
    }
}

/// **A failing store read is retried once per backoff interval — however
/// many re-asks arrive inside it.**
///
/// A store that cannot be read is not cached as empty (the pin above), so
/// before the backoff every consult paid the full round trip: a room's parked
/// floor delete records re-ask their not-yet policy names on every inbound
/// pass, so a device with a broken store paid rooms × parked records ×
/// names nest round trips a poll, inline on the receive path — the
/// amplification, conditioned on a broken store. The bound now: one read per
/// [`UNREADABLE_STORE_BACKOFF_SECS`] while it fails; a store that comes back is
/// seen on the first consult after the interval, and at once after a landed
/// seed (which is itself proof the store was reachable).
///
/// [`UNREADABLE_STORE_BACKOFF_SECS`]: fauna_client_recovery::UNREADABLE_STORE_BACKOFF_SECS
#[test]
fn a_failing_store_read_is_retried_once_per_backoff_interval() {
    use fauna_client_recovery::{LineResolution, UNREADABLE_STORE_BACKOFF_SECS};
    let f = fixture();
    let store = Arc::new(LoadCountingAnchors::default());
    let source = Arc::new(SpyChainSource {
        answer: None,
        dials: Mutex::new(Vec::new()),
    });
    let manager = welcome_joined_thread(f.old);
    let clock = TestClock::default();
    clock.advance_secs(1_000);
    let witness = ChainWitness::new(
        fauna_client_recovery::ThreadParticipantAnchors::with_clock(
            Arc::downgrade(lent(&manager, Arc::new(Shared(Arc::clone(&store))))),
            clock.reader(),
        ),
        Shared(Arc::clone(&source)),
    );
    let unanchored = ActorId([0x5a; 32]);
    let ask = || block_on(witness.resolve_line(&unanchored));

    store.set_down(true);
    for _ in 0..10 {
        assert_eq!(ask(), LineResolution::NotYet);
    }
    assert_eq!(
        store.reads(),
        1,
        "ten re-asks inside one backoff window cost ONE read of a failing store"
    );
    clock.advance_secs(UNREADABLE_STORE_BACKOFF_SECS - 1);
    assert_eq!(ask(), LineResolution::NotYet);
    assert_eq!(store.reads(), 1, "still inside the window");
    clock.advance_secs(1);
    assert_eq!(ask(), LineResolution::NotYet);
    assert_eq!(store.reads(), 2, "the window over, one more read");

    // A landed seed ends the wait at once: the harvest just wrote the store.
    block_on(SuccessionWitness::anchor_seed_landed(&witness));
    store.set_down(false);
    {
        let mut anchors = PeerAnchors::default();
        assert!(anchors.seed_anchor_domain(f.old, "peer.example".to_string()));
        store.plant(anchors);
    }
    assert!(block_on(witness.verify_statement(f.signed.clone())).is_none());
    assert_eq!(store.reads(), 3, "a landed seed is read at once");
    assert_eq!(dials(&source), vec!["peer.example".to_string()]);
    for _ in 0..10 {
        ask();
    }
    assert_eq!(
        store.reads(),
        3,
        "and a readable store is back to the generation guard: no more reads"
    );
}

/// **A clock stepped backwards past a failed read ends the backoff** — one
/// extra read, never a wait that cannot end (or an underflow).
///
/// The backoff measures `now - failed`; a device clock corrected backwards
/// after the failure would otherwise read as a window that has not begun, and
/// the wait would last as long as the step.
#[test]
fn a_clock_stepped_back_past_a_failed_read_retries_at_once() {
    use fauna_client_recovery::LineResolution;
    let f = fixture();
    let store = Arc::new(LoadCountingAnchors::default());
    let source = Arc::new(SpyChainSource {
        answer: None,
        dials: Mutex::new(Vec::new()),
    });
    let manager = welcome_joined_thread(f.old);
    let clock = TestClock::default();
    clock.advance_secs(1_000);
    let witness = ChainWitness::new(
        fauna_client_recovery::ThreadParticipantAnchors::with_clock(
            Arc::downgrade(lent(&manager, Arc::new(Shared(Arc::clone(&store))))),
            clock.reader(),
        ),
        Shared(Arc::clone(&source)),
    );
    let unanchored = ActorId([0x5a; 32]);

    store.set_down(true);
    assert_eq!(
        block_on(witness.resolve_line(&unanchored)),
        LineResolution::NotYet
    );
    assert_eq!(store.reads(), 1);
    clock.rewind_secs(600);
    assert_eq!(
        block_on(witness.resolve_line(&unanchored)),
        LineResolution::NotYet
    );
    assert_eq!(
        store.reads(),
        2,
        "a clock stepped back past the failure reads as due, not as a fresh window"
    );
}

/// A structurally-valid statement naming `old` — what an **in-group member**
/// can mint for *any* identity, holding none of its keys.
///
/// `poll_inbound_conv` hands the witness every body that `canonical_decode`s
/// and checks no signature first, and it does so **before** the roster gate that
/// bounds parking — so the identity named here need not exist, need not be on
/// the roster, and the statement need not verify. That is the whole attacker
/// model: the cost must be bounded by what the *member* holds, never
/// by what a peer chose to send.
fn forged_statement_naming(old: ActorId, nonce: u8) -> SignedIdentitySuccession {
    let successor_kp = ActorKeypair::from_secret([nonce; 32]);
    let recovery = RecoveryKey::generate();
    IdentitySuccession {
        old_actor_id: old,
        new_actor_id: successor_kp.actor_id(),
        recovery_pubkey: recovery.public(),
        seq: 2,
        created_at: Timestamp::now(),
    }
    .sign(&recovery, successor_kp.signing_key(), None)
    .expect("a forged statement still signs — signing is not the guard")
}

/// **The bound.** A burst of forged statements — many identities, each
/// delivered repeatedly — must not drive the store read count with it.
///
/// Before the generation guard this arm re-read the store on **every**
/// consult: one nest RPC + one unseal per forged body, unmemoized (the
/// no-anchor arm deliberately records no verdict), inline on the receive path,
/// with no per-actor bound. The one read asserted below is the session's
/// floor — heads, their outrun marks and the anchor domains all fold from a
/// single generation-stamped read — and the point of the number is that it
/// does not move with the size of the burst.
#[test]
fn a_burst_of_forged_statements_cannot_drive_the_config_read_count() {
    let f = fixture();
    let store = Arc::new(LoadCountingAnchors::default());
    let source = Arc::new(SpyChainSource {
        answer: None,
        dials: Mutex::new(Vec::new()),
    });
    let manager = welcome_joined_thread(f.old);
    let witness = ChainWitness::new(
        fauna_client_recovery::ThreadParticipantAnchors::new(Arc::downgrade(lent(
            &manager,
            Arc::new(Shared(Arc::clone(&store))),
        ))),
        Shared(Arc::clone(&source)),
    );

    // 25 identities the member holds nothing for, three deliveries each: 75
    // trips through the no-anchor arm.
    for nonce in 0..25u8 {
        let old = ActorId([nonce.wrapping_add(100); 32]);
        for _ in 0..3 {
            assert!(
                block_on(witness.verify_statement(forged_statement_naming(old, nonce))).is_none(),
                "no anchor is held, so nothing verifies"
            );
        }
    }

    assert!(
        dials(&source).is_empty(),
        "no anchor means no dial — the cost under test is the store's, not the \
         network's: {:?}",
        dials(&source)
    );
    assert_eq!(
        store.reads(),
        1,
        "75 forged statements must cost the same single store read one \
         does — the session's one generation-stamped read. A count that \
         tracks the burst size is the amplification back again."
    );
}

/// **The other half: the guard memoizes a fact, it does not wedge.**
///
/// The read it replaces existed so a harvest completing *after* a statement's
/// first delivery could still be seen. The generation is what keeps that true —
/// and keeps it O(1) in the number of statements rather than in the number of
/// harvests: one landed seed buys exactly one more read, however many
/// statements arrive on either side of it.
#[test]
fn a_landed_seed_buys_exactly_one_more_config_read() {
    let f = fixture();
    let store = Arc::new(LoadCountingAnchors::default());
    let source = Arc::new(SpyChainSource {
        answer: None,
        dials: Mutex::new(Vec::new()),
    });
    let manager = welcome_joined_thread(f.old);
    let witness = ChainWitness::new(
        fauna_client_recovery::ThreadParticipantAnchors::new(Arc::downgrade(lent(
            &manager,
            Arc::new(Shared(Arc::clone(&store))),
        ))),
        Shared(Arc::clone(&source)),
    );

    for _ in 0..10 {
        assert!(block_on(witness.verify_statement(f.signed.clone())).is_none());
    }
    assert_eq!(store.reads(), 1, "the session's floor");

    // The harvest lands and the shared re-drive announces it.
    block_on(SuccessionWitness::anchor_seed_landed(&witness));
    for _ in 0..10 {
        assert!(block_on(witness.verify_statement(f.signed.clone())).is_none());
    }
    assert_eq!(
        store.reads(),
        2,
        "the seed costs one re-read, and the ten statements after it cost \
         nothing — the generation bounds reads by harvests, not by deliveries"
    );
}

/// **A store that could not be read is not an answer, so it is not cached.**
///
/// The generation guard is licensed by "only a harvest changes what is at
/// rest", and a *failed read* establishes nothing about what is at rest — so
/// caching its empty view would answer `None` for the whole generation on
/// evidence that does not exist, and a member whose store blinked would
/// degrade to permanent TOFU until the next seed. This is the same rule
/// `hydrate`'s `hydrated` flag already follows for the head half, which stays
/// `false` on a failed read for exactly this reason; the two halves must not
/// disagree.
///
/// The retry waits out [`UNREADABLE_STORE_BACKOFF_SECS`] (the injected clock
/// steps past it here) — a backoff, never a cache: see
/// `a_failing_store_read_is_retried_once_per_backoff_interval`.
///
/// [`UNREADABLE_STORE_BACKOFF_SECS`]: fauna_client_recovery::UNREADABLE_STORE_BACKOFF_SECS
#[test]
fn an_unreadable_store_is_retried_rather_than_cached_as_empty() {
    let f = fixture();
    let store = Arc::new(LoadCountingAnchors::default());
    let source = Arc::new(SpyChainSource {
        answer: None,
        dials: Mutex::new(Vec::new()),
    });
    let manager = welcome_joined_thread(f.old);
    let clock = TestClock::default();
    let witness = ChainWitness::new(
        fauna_client_recovery::ThreadParticipantAnchors::with_clock(
            Arc::downgrade(lent(&manager, Arc::new(Shared(Arc::clone(&store))))),
            clock.reader(),
        ),
        Shared(Arc::clone(&source)),
    );

    // The statement arrives during the outage: no anchor, and nothing learned.
    store.set_down(true);
    assert!(block_on(witness.verify_statement(f.signed.clone())).is_none());

    // The plane comes back, holding a domain the harvest seeded earlier. No
    // seed lands *now*, so no generation bump: only the refusal to cache a
    // failed read can make this next delivery see it.
    clock.advance_secs(fauna_client_recovery::UNREADABLE_STORE_BACKOFF_SECS);
    store.set_down(false);
    {
        let mut anchors = PeerAnchors::default();
        assert!(anchors.seed_anchor_domain(f.old, "peer.example".to_string()));
        store.plant(anchors);
    }
    assert!(block_on(witness.verify_statement(f.signed)).is_none());
    assert_eq!(
        dials(&source),
        vec!["peer.example".to_string()],
        "the recovered store's harvested domain must anchor the walk — a cached \
         failure would leave this member at no_anchor for the whole generation"
    );
}

/// **The real session's ordering**, which every pin above misses by
/// constructing its anchors *after* the harvest has already seeded the store.
///
/// A live member builds its witness once, at login (tui:
/// `conv_backend.rs::attach_real_session`), and only *then* does the harvest
/// sweep run and seed the peer's head. The statement arrives later still, and
/// — unlike a resumed sweep or a Rule-2 heal — it arrives **once**. So the
/// question this pin asks is the one production actually asks: does the very
/// first delivery see a head that was written to the store after the anchors
/// object already existed?
///
/// It is a distinct question because `ThreadParticipantAnchors` caches heads in
/// memory in front of the store and hydrates from rest at most once per
/// session; a pin that builds the object fresh can never observe that latch.
#[test]
fn a_head_harvested_after_login_settles_the_very_first_delivery() {
    let f = fixture();
    let store = Arc::new(MemoryAnchors::default());

    // Login: the witness exists before any harvest has run.
    let source = Arc::new(SpyChainSource {
        answer: None,
        dials: Mutex::new(Vec::new()),
    });
    let manager = welcome_joined_thread(f.old);
    let witness = ChainWitness::new(
        anchors_over_manager(&manager, &store),
        Shared(Arc::clone(&source)),
    );

    // The sweep's ordinary read path, some ticks later.
    let nest = StubProfileNest {
        body: Some(signed_profile_bytes(&old_keypair(), Some(f.head), None)),
    };
    assert_eq!(
        block_on(harvest_peer_anchor(
            &nest,
            &f.old,
            &Shared(Arc::clone(&store))
        )),
        HarvestOutcome::Seeded(PeerAnchorSeed {
            seeded_head: true,
            seeded_domain: false,
            marked_outrun: false,
        })
    );

    // The statement, delivered once.
    let verified = block_on(witness.verify_statement(f.signed)).expect(
        "the first delivery must settle from the harvested head — a member gets \
         exactly one statement, so a verdict that needs a second delivery is a \
         re-point that never happens",
    );
    assert_eq!(verified.new_actor_id, f.successor);
    assert!(
        dials(&source).is_empty(),
        "tier 1: a harvested head verifies offline, no dial: {:?}",
        dials(&source)
    );
}

// ── the observation: which arm the rule took, readable from outside ─────────
//
// Convention 6 ("failures must diagnose themselves") applied to a policy whose
// four outcomes are otherwise indistinguishable from any consumer: a statement
// that never arrived, one that arrived with nothing to anchor on, one whose
// dial failed and one that verified all render the *same* un-re-pointed row.
// The `debug!` lines above them are unreadable in production — no app installs
// a tracing subscriber — so a member-side failure could only ever be diagnosed
// by rebuilding the binary. [`ChainWitness::observation`] is the permanent
// answer, and these pin that each arm reports itself as the arm it was.

use fauna_client_recovery::witness::WitnessOutcome;

/// A witness nobody asked anything reports exactly that — the reading that
/// separates "the member's poll never delivered the statement" from every
/// verdict below it. Without this arm a silent inbound path and a refused
/// statement are the same observation.
#[test]
fn a_witness_that_was_never_consulted_says_so() {
    let (witness, _anchors, _source) = build(None, None, None);

    let obs = witness.observation();
    assert_eq!(obs.statements_seen, 0);
    assert!(
        obs.peers.is_empty(),
        "no statement, no peer rows: {:?}",
        obs.peers
    );
}

/// Tier 1, reported as tier 1 — and the held head's `seq` alongside, because
/// "a head was held" and "the held head authorized this statement" are
/// different states and only the second one re-points.
#[test]
fn the_held_head_arm_reports_the_head_it_settled_on() {
    let f = fixture();
    let (witness, _anchors, _source) = build(Some(f.head), Some("alice@example.test"), None);

    assert!(block_on(witness.verify_statement(f.signed)).is_some());

    let obs = witness.observation();
    assert_eq!(obs.statements_seen, 1);
    let peer = &obs.peers[0];
    assert_eq!(peer.actor, f.old);
    assert_eq!(peer.outcome, WitnessOutcome::SettledByHeldHead);
    assert_eq!(peer.held_head_seq, Some(f.head.seq));
    assert_eq!(peer.statements_seen, 1);
}

/// The no-anchor arm — the one the harvest exists to make rare, and the
/// one whose signature is *no dial at all*. It must be distinguishable from a
/// dial that failed, because the remedies are opposite: seed an anchor vs.
/// reach a nest.
#[test]
fn the_no_anchor_arm_reports_that_nothing_could_be_dialled() {
    let f = fixture();
    let (witness, _anchors, source) = build(None, None, None);

    assert!(block_on(witness.verify_statement(f.signed)).is_none());

    let peer = &witness.observation().peers[0];
    assert_eq!(peer.outcome, WitnessOutcome::NoAnchor);
    assert_eq!(peer.held_head_seq, None);
    assert_eq!(peer.handle_domain, None);
    assert_eq!(peer.harvested_domain, None);
    assert!(dials(&source).is_empty());
}

/// A dial that happened and did not verify — the arm the no-anchor one above
/// must never be confused with. The domain it went to is part of the report:
/// a walk against the *wrong* nest fails exactly like an unreachable one.
#[test]
fn a_failed_walk_reports_the_domain_it_dialled() {
    let f = fixture();
    let (witness, _anchors, _source) = build(None, Some("alice@example.test"), None);

    assert!(block_on(witness.verify_statement(f.signed)).is_none());

    let peer = &witness.observation().peers[0];
    assert_eq!(peer.outcome, WitnessOutcome::WalkFailed);
    assert_eq!(peer.handle_domain.as_deref(), Some("example.test"));
    assert_eq!(peer.harvested_domain, None);
}

/// The harvested fallback, reported as itself. A member anchored by the
/// harvest and one anchored by a roster handle take different code
/// paths for different reasons; a report that flattened them would send the
/// next reader to the wrong producer.
#[test]
fn the_harvested_domain_arm_reports_which_anchor_named_the_nest() {
    let f = fixture();
    let verdict = VerifiedSuccession {
        old_actor_id: f.old,
        new_actor_id: f.successor,
        seq: 2,
        chain_head: f.head,
    };
    let (witness, _anchors, source) =
        build_anchored(None, None, Some("harvested.test"), Some(verdict));

    assert!(block_on(witness.verify_statement(f.signed)).is_some());

    let peer = &witness.observation().peers[0];
    assert_eq!(peer.outcome, WitnessOutcome::SettledByWalk);
    assert_eq!(peer.handle_domain, None);
    assert_eq!(peer.harvested_domain.as_deref(), Some("harvested.test"));
    assert_eq!(dials(&source), vec!["harvested.test".to_string()]);
}

/// A memo hit must not erase the verdict that produced it. The decisive answer
/// is the diagnostic one — a report showing only "answered from the memo"
/// would hide *which* arm the session is now locked into, which is the whole
/// question a re-delivered statement raises.
#[test]
fn a_memo_hit_is_counted_without_overwriting_the_decisive_arm() {
    let f = fixture();
    let (witness, _anchors, _source) = build(None, Some("alice@example.test"), None);

    assert!(block_on(witness.verify_statement(f.signed.clone())).is_none());
    assert!(block_on(witness.verify_statement(f.signed)).is_none());

    let obs = witness.observation();
    assert_eq!(obs.statements_seen, 2);
    let peer = &obs.peers[0];
    assert_eq!(peer.statements_seen, 2);
    assert_eq!(peer.memo_hits, 1);
    assert_eq!(
        peer.outcome,
        WitnessOutcome::WalkFailed,
        "the second delivery was a memo hit; the report must still name the \
         arm that decided the session"
    );
}

/// A readable store that simply holds nothing must not look like an
/// unreachable one.
///
/// This is the reading that turns `no_anchor` from a symptom into an address.
/// Both states answer `known_head` with `None`, and their owners are opposite:
/// an empty-but-readable store indicts the **producer** (the harvest never
/// seeded), an unreadable one indicts the member's own store. The
/// member path's first localized failure was exactly this fork.
#[test]
fn an_empty_anchor_store_is_reported_as_read_not_as_unreachable() {
    use fauna_client_recovery::witness::AnchorStoreState;

    let f = fixture();
    let store = Arc::new(MemoryAnchors::default());
    let source = Arc::new(SpyChainSource {
        answer: None,
        dials: Mutex::new(Vec::new()),
    });
    let witness = ChainWitness::new(anchors_over(&store), Shared(Arc::clone(&source)));

    assert!(block_on(witness.verify_statement(f.signed)).is_none());

    let obs = witness.observation();
    assert_eq!(obs.peers[0].outcome, WitnessOutcome::NoAnchor);
    assert_eq!(
        obs.anchor_store,
        AnchorStoreState::Read {
            heads: 0,
            domains: 0
        },
        "a never-seeded but perfectly readable store must report itself as \
         READ and empty — reported as unreadable it would send the next reader \
         to debug the member's own config plane instead of the producer that \
         never wrote"
    );
}

/// And the converse: a store that is genuinely unreachable says so, rather
/// than reading as an honest empty one.
#[test]
fn an_unreadable_anchor_store_is_reported_as_unreadable() {
    use fauna_client_recovery::witness::AnchorStoreState;

    let f = fixture();
    let store = Arc::new(BrokenAnchors);
    let source = Arc::new(SpyChainSource {
        answer: None,
        dials: Mutex::new(Vec::new()),
    });
    let manager = welcome_joined_thread(f.old);
    let anchors = fauna_client_recovery::ThreadParticipantAnchors::new(Arc::downgrade(lent(
        &manager,
        Arc::clone(&store) as Arc<dyn PeerAnchorStore>,
    )));
    let witness = ChainWitness::new(anchors, Shared(Arc::clone(&source)));

    assert!(block_on(witness.verify_statement(f.signed)).is_none());

    let obs = witness.observation();
    assert_eq!(obs.peers[0].outcome, WitnessOutcome::NoAnchor);
    assert_eq!(obs.anchor_store, AnchorStoreState::Unreadable);
}

/// An anchor store that fails every read and write — the member whose own
/// store is down, which is indistinguishable from an empty one at every other
/// seam.
struct BrokenAnchors;

#[async_trait]
impl PeerAnchorStore for BrokenAnchors {
    async fn peer_anchors(&self) -> Result<PeerAnchors, String> {
        Err("anchor store down".into())
    }
    async fn merge_peer_anchors(&self, _replica: PeerAnchors) -> Result<PeerAnchors, String> {
        Err("anchor store down".into())
    }
}

/// A store that accepts the write and does not keep it must be reported, not
/// believed.
///
/// The merge returning `Ok` proves the call was made; it does not prove the
/// bytes stuck. The anchor this seed becomes is consulted inline on a receive
/// path that cannot investigate, and its absence renders as an ordinary
/// un-re-pointed participant row — so without a read-back the harvest reports
/// `Seeded` forever while the member stays permanently un-anchored, which is
/// the most expensive failure shape this file knows.
#[test]
fn a_seed_the_store_does_not_read_back_is_reported_rather_than_believed() {
    let f = fixture();
    let nest = StubProfileNest {
        body: Some(signed_profile_bytes(&old_keypair(), Some(f.head), None)),
    };

    assert_eq!(
        block_on(harvest_peer_anchor(&nest, &f.old, &AmnesiacAnchors)),
        HarvestOutcome::SeedLost,
        "a write that reports success and reads back empty must surface as its \
         own arm — reported as `Seeded` it is a lie the whole member path then \
         rests on"
    );
}

/// A store whose writes always succeed and never persist: the merge answers
/// what rests, which is nothing.
struct AmnesiacAnchors;

#[async_trait]
impl PeerAnchorStore for AmnesiacAnchors {
    async fn peer_anchors(&self) -> Result<PeerAnchors, String> {
        Ok(PeerAnchors::default())
    }
    async fn merge_peer_anchors(&self, _replica: PeerAnchors) -> Result<PeerAnchors, String> {
        Ok(PeerAnchors::default())
    }
}

/// The producer half: the harvest log names *which* non-seeding outcome a peer
/// hit, and counts the retried ones.
///
/// Without it the consumer's `no_anchor` has four possible owners — the head
/// mirror, the profile's shape, the member's store, and a sweep that
/// never reached the peer at all — and they are indistinguishable. Absence of
/// an entry is itself the fourth reading, which is why the log records
/// attempts rather than only outcomes.
#[test]
fn the_harvest_log_names_the_outcome_and_counts_the_retries() {
    use fauna_client_recovery::harvest::HarvestLog;

    let f = fixture();
    let log = HarvestLog::default();
    assert!(
        log.entries().is_empty(),
        "no attempt, no entry — a peer the sweep never reached must not be \
         reportable as any outcome"
    );

    log.record(&f.old, HarvestOutcome::Unreachable);
    log.record(&f.old, HarvestOutcome::NothingNew);

    let entries = log.entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].actor, f.old);
    assert_eq!(entries[0].attempts, 2);
    assert_eq!(
        entries[0].last,
        HarvestOutcome::NothingNew,
        "the CURRENT state is what a reader acts on; the retry survives as the \
         attempt count rather than by pinning the first answer"
    );
}

// ── the retired store is released with its session ──────────────────────────
//
// `account-scoping.md` § Implementation status → Isolation-contract gap ledger
// (`tui (in-memory)`), and `settings.md` § Recovery kit → *Finishing an
// unfinished group sweep*: the retry opens the retired identity's own
// conversation store, so the session that served that store must have LET GO
// of it — and an account switch's incoming session opens its own store under
// the same rule. Measured 2026-08-27 on tui: it had
// not. The manager held the FaunaMls backend, the backend held this witness,
// the witness's anchors held the manager, and the retired engine's file lock
// lived as long as the process — every press of the retry answered "served in
// another instance of this app" with no other instance anywhere.

/// A unique ON-DISK store per test. The production observable is the store's
/// file lock (`fauna-mls`' one-engine-per-store role lock), which an in-memory
/// engine never takes — so the pin has to open a real file.
fn a_store_path(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("fauna-witness-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir.join("mls_state.db")
}

/// **The cycle pin.** A session wired exactly as tui wires it — the witness
/// registered on the FaunaMls backend, its anchors reading the session's own
/// manager — must not keep that manager alive once the session is dropped,
/// and the store the session served must be openable again.
///
/// Reds under `ThreadParticipantAnchors.manager: Arc<_>` (the shape until
/// 2026-08-27): the strong cycle manager → backend → witness → anchors →
/// manager has no owner left to break it, so the first assertion fails and the
/// re-open answers `ServedElsewhere` against a session that is already over.
#[test]
fn the_witness_does_not_pin_the_session_it_serves() {
    let store = Arc::new(MemoryAnchors::default());
    let db = a_store_path("cycle");
    let engine = Arc::new(
        fauna_mls::engine::MlsEngine::new(ActorKeypair::from_secret([44u8; 32]), &db)
            .expect("the store opens"),
    );
    let self_actor = engine.identity_actor_id();
    let manager = fauna_conversations::ConversationsManager::new();
    let session = fauna_conversations::ConversationsSession::from_manager(
        Arc::clone(&manager),
        engine,
        // A nest the session never reaches: the pin is about ownership, not
        // the wire, and a session is built here only to register the witness
        // the way every app does (`ConversationsSession::set_succession_witness`).
        Arc::new(fauna_conversations::backends::mock::InertConversationsRpc),
        "me@example.test".to_string(),
        self_actor,
        None,
    );
    let source = Arc::new(SpyChainSource {
        answer: None,
        dials: Mutex::new(Vec::new()),
    });
    let witness = Arc::new(ChainWitness::new(
        anchors_over_manager(&manager, &store),
        Shared(Arc::clone(&source)),
    ));
    session.set_succession_witness(witness as Arc<dyn SuccessionWitness>);

    let manager_gone = Arc::downgrade(&manager);
    drop(session);
    drop(manager);
    assert!(
        manager_gone.upgrade().is_none(),
        "the witness's anchors keep the manager alive after its session is gone: \
         manager → FaunaMls backend → witness → anchors → manager is a strong cycle"
    );

    // The production consequence, stated on the store itself: the sweep retry
    // (and an account switch back to this identity) opens this file next.
    let reopened = fauna_mls::engine::MlsEngine::new(ActorKeypair::from_secret([44u8; 32]), &db);
    assert!(
        reopened.is_ok(),
        "the retired store must be openable once its session is gone — the retry's \
         whole precondition on the ceremony's own device; got {:?}",
        reopened.err()
    );
}

// ── the shared `data.succession_witness` renderer ────────────────────────────
//
// Lifted out of tui 2026-09-03 with linux's leg (`succession-aftermath.md`
// § Propagation → *MLS groups*, the ✅ witness bullet). Every app's driver reads
// these exact key names, so the shape is a cross-app contract and belongs to a
// pin rather than to whichever app happened to write it first — the state it
// renders was, until the lift, only ever asserted through a 120 s tier_3
// journey.

/// The three halves are reported together and each names its own owner: the
/// poll's tally, the producer's per-peer log, and the witness's own verdict.
/// A renderer that dropped any one of them would leave two failures wearing the
/// same face — the whole reason the observable exists.
#[test]
fn the_state_report_carries_the_poll_the_producer_and_the_verdict() {
    let f = fixture();
    let (witness, _anchors, _source) = build(Some(f.head), Some("alice@example.test"), None);
    assert!(block_on(witness.verify_statement(f.signed)).is_some());

    let log = fauna_client_recovery::harvest::HarvestLog::default();
    log.record(
        &f.old,
        HarvestOutcome::Seeded(PeerAnchorSeed {
            seeded_head: true,
            seeded_domain: false,
            marked_outrun: false,
        }),
    );
    let counts = fauna_conversations::backends::fauna_mls::SuccessionStatementCounts {
        seen: 3,
        undecodable: 1,
        no_witness: 0,
        repointed: 1,
        parked: 1,
        awaiting_remove_old: 1,
        not_in_this_group: 1,
        held_commits: 2,
    };

    let json = fauna_client_recovery::witness::state_json(&witness.observation(), &log, &counts);

    // The POLL half — counted by the backend, not by the witness, because a
    // statement that never reached the witness is exactly what `seen` reports.
    assert_eq!(json["statements"]["seen"], 3);
    assert_eq!(json["statements"]["undecodable"], 1);
    assert_eq!(json["statements"]["repointed"], 1);
    assert_eq!(json["statements"]["parked"], 1);
    // The roster-pair gate's two arms, which a journey needs rendered for the
    // same reason as the rest of this block: they move no row, so they are
    // invisible from outside the process unless the report names them.
    assert_eq!(json["statements"]["awaiting_remove_old"], 1);
    assert_eq!(json["statements"]["not_in_this_group"], 1);
    assert_eq!(json["statements"]["held_commits"], 2);

    // The PRODUCER half, keyed by actor so a driver can find its own peer.
    let harvest = json["harvest"].as_array().expect("harvest is a list");
    assert_eq!(harvest.len(), 1);
    assert_eq!(harvest[0]["actor_id"], f.old.to_hex());
    assert_eq!(harvest[0]["attempts"], 1);
    assert!(
        harvest[0]["outcome"]
            .as_str()
            .expect("the outcome renders as a string")
            .starts_with("Seeded"),
        "the journey asserts `outcome.startswith('Seeded')` — got {:?}",
        harvest[0]["outcome"]
    );

    // The VERDICT half, naming the arm rather than only the result.
    assert_eq!(json["verified_statements"], 1);
    let peers = json["peers"].as_array().expect("peers is a list");
    assert_eq!(peers.len(), 1);
    assert_eq!(peers[0]["actor_id"], f.old.to_hex());
    assert_eq!(peers[0]["outcome"], "settled_by_held_head");
    assert_eq!(peers[0]["held_head_seq"], f.head.seq);
    // …and NOT the handle domain, even though the roster carries one: tier 1
    // settles offline without ever consulting an anchor to dial with, so a
    // rendered `handle_domain` here would tell a reader a dial was prepared.
    assert!(peers[0]["handle_domain"].is_null());
}

/// A peer the producer never attempted must be *absent* from `harvest`, not
/// present with a zero count: absence is its own reading (the sweep never saw
/// the roster row), and it is the reading the journey's ordering barrier waits
/// on.
#[test]
fn a_peer_the_harvest_never_attempted_is_absent_rather_than_zeroed() {
    let f = fixture();
    let (witness, _anchors, _source) = build(Some(f.head), Some("alice@example.test"), None);
    assert!(block_on(witness.verify_statement(f.signed)).is_some());

    let json = fauna_client_recovery::witness::state_json(
        &witness.observation(),
        &fauna_client_recovery::harvest::HarvestLog::default(),
        &Default::default(),
    );

    assert_eq!(
        json["harvest"].as_array().expect("harvest is a list").len(),
        0,
        "an un-attempted peer must leave the list empty — a zero-attempt entry \
         would read as 'the sweep tried and got nothing'"
    );
    // …while the witness half still reports the peer it did settle.
    assert_eq!(json["peers"].as_array().unwrap().len(), 1);
}

/// `anchor_store` separates "nothing was seeded" from "the store is
/// unreadable". Both answer `None` per peer and both surface as `no_anchor`,
/// and they indict opposite halves of the system — the producer vs. the
/// member's own store.
#[test]
fn the_state_report_tells_an_empty_anchor_store_from_an_unreadable_one() {
    let f = fixture();

    // Never consulted: no rule has run, so no read has been attempted.
    let (fresh, _a, _s) = build(None, None, None);
    let json = fauna_client_recovery::witness::state_json(
        &fresh.observation(),
        &fauna_client_recovery::harvest::HarvestLog::default(),
        &Default::default(),
    );
    assert_eq!(json["anchor_store"]["state"], "not_read");

    // Read and empty — the producer never ran.
    let store = Arc::new(MemoryAnchors::default());
    let witness = ChainWitness::new(
        anchors_over(&store),
        Shared(Arc::new(SpyChainSource {
            answer: None,
            dials: Mutex::new(Vec::new()),
        })),
    );
    assert!(block_on(witness.verify_statement(f.signed)).is_none());
    let json = fauna_client_recovery::witness::state_json(
        &witness.observation(),
        &fauna_client_recovery::harvest::HarvestLog::default(),
        &Default::default(),
    );
    assert_eq!(json["anchor_store"]["state"], "read");
    assert_eq!(json["anchor_store"]["heads"], 0);
}

// ── The sweep's per-pass policy, driven with no runtime at all ─────────────
//
// `spawn_peer_anchor_harvest_sweep` is the tokio driver; these pins are on the
// *pass* underneath it, because as of 2026-09-09 there is a second driver —
// web's, ticked from the browser's JS-owned receive pump, which has no
// `tokio::spawn` and no `ConversationsSession` to hold. The whole point of
// lifting the pass out is that the once-per-peer-per-session guard, the retry
// ladder and the re-drive are run by both drivers rather than re-derived by the
// second; the pins below are what make that checkable without standing up
// either driver.

/// Records which peers the pass re-drove, in order.
#[derive(Default)]
struct RecordingRedrive {
    seen: Mutex<Vec<ActorId>>,
}

#[async_trait]
impl fauna_client_recovery::harvest::ParkedStatementRedrive for RecordingRedrive {
    async fn redrive(&self, old_actor: &ActorId) -> u32 {
        self.seen.lock().unwrap().push(*old_actor);
        0
    }

    // The un-seeded settle is pinned on its own double below
    // (`SettleRecordingRedrive`); this one records seeds only.
    async fn settled_unseeded(&self, _old_actor: &ActorId) -> u32 {
        0
    }
}

/// **A driver with no runtime runs the same pass.** One pass over a
/// Welcome-joined roster seeds the peer's anchor and re-drives whatever was
/// parked for it; a second pass over the same roster fetches nothing.
///
/// The guard is the load-bearing half: web ticks this from its receive pump,
/// which fires on every inbound push as well as a backstop timer, so a pass
/// that re-fetched each time it ran would put a profile fetch behind every
/// message the tab receives. Native's 5 s timer would have hidden that.
#[test]
fn the_shared_pass_harvests_once_per_peer_and_redrives_on_the_seed() {
    let f = fixture();
    let store = Arc::new(MemoryAnchors::default());
    let manager = welcome_joined_thread(f.old);
    let nest = CountingProfileNest {
        inner: StubProfileNest {
            body: Some(signed_profile_bytes(&old_keypair(), Some(f.head), None)),
        },
        fetches: Default::default(),
    };
    let redrive = RecordingRedrive::default();
    let log = fauna_client_recovery::harvest::HarvestLog::default();
    let mut sweep = fauna_client_recovery::harvest::PeerAnchorSweepState::new();

    block_on(sweep.run_pass(
        lent(&manager, Arc::new(Shared(Arc::clone(&store)))),
        &nest,
        &log,
        &redrive,
    ));
    assert_eq!(
        stored_head(&store, &f.old),
        Some(f.head),
        "the pass seeds the peer's chain head through the ordinary harvest door"
    );
    assert_eq!(
        redrive.seen.lock().unwrap().as_slice(),
        &[f.old],
        "a seed re-drives exactly the peer it landed for — never a statement's ask"
    );

    block_on(sweep.run_pass(
        lent(&manager, Arc::new(Shared(Arc::clone(&store)))),
        &nest,
        &log,
        &redrive,
    ));
    assert_eq!(
        nest.fetches.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "once per peer per session: the second pass over the same roster fetches nothing"
    );
    assert_eq!(
        redrive.seen.lock().unwrap().len(),
        1,
        "a settled peer is not re-driven on every later tick"
    );
}

/// **A peer whose nest is unreachable is retried on the ladder, not on every
/// tick** — the same budget `the_retry_ladder_stops_after_its_budget` pins as a
/// pure function, now observed through the pass a driver actually calls.
///
/// The first pass attempts; the second must not, because the ladder's first
/// step is one tick away. Without the shared pass this ordering lived only
/// inside the spawned loop, where a second driver could not see it.
#[test]
fn the_shared_pass_waits_out_the_backoff_before_re_attempting() {
    let f = fixture();
    let store = Arc::new(MemoryAnchors::default());
    let manager = welcome_joined_thread(f.old);
    // No body → the stub rejects; `is_rejection` makes that a *refusal*, so use
    // the transport-failure shape instead: a rejection settles, an unreachable
    // nest retries.
    let nest = CountingProfileNest {
        inner: StubProfileNest { body: None },
        fetches: Default::default(),
    };
    let redrive = RecordingRedrive::default();
    let log = fauna_client_recovery::harvest::HarvestLog::default();
    let mut sweep = fauna_client_recovery::harvest::PeerAnchorSweepState::new();

    block_on(sweep.run_pass(
        lent(&manager, Arc::new(Shared(Arc::clone(&store)))),
        &nest,
        &log,
        &redrive,
    ));
    let after_first = nest.fetches.load(std::sync::atomic::Ordering::Relaxed);
    block_on(sweep.run_pass(
        lent(&manager, Arc::new(Shared(Arc::clone(&store)))),
        &nest,
        &log,
        &redrive,
    ));
    assert_eq!(
        nest.fetches.load(std::sync::atomic::Ordering::Relaxed),
        after_first,
        "a settled-or-waiting peer costs no fetch on the very next pass"
    );
}

// ── the statement that beats the session's own harvest ─────────────────────
//
// The outrun rule only knows what a harvest has *read*, and the sweep that
// reads is fire-and-forget beside the receive loop — so a statement already
// waiting in the channel can reach tier 1 before this session's
// `fauna.profile.get` for that peer returns. `identity-succession.md` § The
// succession statement → *The statement that beats the session's own harvest
// — the harvest wait* closes that: in a session that runs a sweep, a held head
// settles nothing until the sweep has settled that peer, by any arm.

/// A nest the member cannot reach — a transport failure, never a rejection, so
/// the harvest classifies it `Unreachable` and the sweep retries on its ladder.
struct UnreachableProfileNest;

impl fauna_protocol::RpcRequester for UnreachableProfileNest {
    type Error = StubProfileError;

    async fn request<Req, Reply>(
        &self,
        _kind: &'static str,
        _payload: Req,
    ) -> Result<Reply, Self::Error>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        Err(StubProfileError { rejection: false })
    }
}

/// [`UnreachableProfileNest`], counting the fetches it fails.
#[derive(Default)]
struct CountingUnreachableNest {
    fetches: std::sync::atomic::AtomicU32,
}

impl fauna_protocol::RpcRequester for CountingUnreachableNest {
    type Error = StubProfileError;

    async fn request<Req, Reply>(
        &self,
        _kind: &'static str,
        _payload: Req,
    ) -> Result<Reply, Self::Error>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        self.fetches
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Err(StubProfileError { rejection: false })
    }
}

/// **An unreachable nest costs ONE fetch per pass, however many peers are
/// queued behind it.** Every fetch of a pass rides the member's one connection
/// to their own nest, and a fetch that fails for transport waits out its whole
/// request deadline first — so a serial pass that tried each peer in turn made
/// the harvest wait's budget scale with the roster (five deadlines per peer),
/// which for an offline member is the delay before ANY held statement settles.
/// The first `Unreachable` therefore answers for the rest of the pass: each
/// remaining due peer steps its ladder without a fetch of its own.
#[test]
fn an_unreachable_nest_costs_one_fetch_per_pass_not_one_per_peer() {
    let f = fixture();
    let store = Arc::new(MemoryAnchors::default());
    let manager = welcome_joined_thread(f.old);
    let others = [ActorId([0x61; 32]), ActorId([0x62; 32])];
    manager.set_policy_anchor_wants(fauna_mls::types::ChannelId([0x77; 32]), others.to_vec());
    let nest = CountingUnreachableNest::default();
    let redrive = SettleRecordingRedrive::default();
    let log = fauna_client_recovery::harvest::HarvestLog::default();
    let mut sweep = fauna_client_recovery::harvest::PeerAnchorSweepState::new();

    block_on(sweep.run_pass(
        lent(&manager, Arc::new(Shared(Arc::clone(&store)))),
        &nest,
        &log,
        &redrive,
    ));
    assert_eq!(
        nest.fetches.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "three peers due, one connection down: one fetch finds that out"
    );
    assert!(
        redrive.unseeded.lock().unwrap().is_empty(),
        "a retry still owed is not a settle"
    );

    // The ladder's five attempts fall on passes 1, 2, 4, 8 and 16.
    for _ in 1..16 {
        block_on(sweep.run_pass(
            lent(&manager, Arc::new(Shared(Arc::clone(&store)))),
            &nest,
            &log,
            &redrive,
        ));
    }
    assert_eq!(
        nest.fetches.load(std::sync::atomic::Ordering::Relaxed),
        fauna_client_recovery::harvest::MAX_HARVEST_ATTEMPTS,
        "the whole budget costs one fetch per attempt, not one per attempt per peer"
    );
    let mut settled = redrive.unseeded.lock().unwrap().clone();
    settled.sort_by_key(|actor| actor.0);
    let mut expected = vec![f.old, others[0], others[1]];
    expected.sort_by_key(|actor| actor.0);
    assert_eq!(
        settled, expected,
        "and every peer still settles at the budget, releasing what waited on it"
    );
}

/// Records both of the pass's re-drive calls, in order.
#[derive(Default)]
struct SettleRecordingRedrive {
    seeded: Mutex<Vec<ActorId>>,
    unseeded: Mutex<Vec<ActorId>>,
}

#[async_trait]
impl fauna_client_recovery::harvest::ParkedStatementRedrive for SettleRecordingRedrive {
    async fn redrive(&self, old_actor: &ActorId) -> u32 {
        self.seeded.lock().unwrap().push(*old_actor);
        0
    }

    async fn settled_unseeded(&self, old_actor: &ActorId) -> u32 {
        self.unseeded.lock().unwrap().push(*old_actor);
        0
    }
}

/// **The pin.** The member anchored the peer in an earlier session; the owner
/// rotated the kit while the member was away; the retired kit's statement is
/// already in the channel when the member's next session starts, and it is
/// decoded BEFORE the sweep's fetch for that peer returns. It must not settle —
/// and must not be refused for good either: no verdict is recorded, nothing is
/// dialled, and once the harvest has read the rotation the very same statement
/// goes to the walk, where a rotated-away key fails.
#[test]
fn a_statement_that_beats_the_sessions_harvest_waits_for_it() {
    let f = fixture();
    let store = Arc::new(MemoryAnchors::default());
    // An earlier session's harvest: the pre-rotation head, at rest.
    let before = StubProfileNest {
        body: Some(signed_profile_bytes(
            &old_keypair(),
            Some(f.head),
            Some("https://home.example.test"),
        )),
    };
    block_on(harvest_peer_anchor(
        &before,
        &f.old,
        &Shared(Arc::clone(&store)),
    ));

    // Control — a session that runs NO sweep keeps the immediate tier 1
    // (nothing would ever release a wait there).
    {
        let source = Arc::new(SpyChainSource {
            answer: None,
            dials: Mutex::new(Vec::new()),
        });
        let manager = welcome_joined_thread(f.old);
        let witness = ChainWitness::new(
            anchors_over_manager(&manager, &store),
            Shared(Arc::clone(&source)),
        );
        assert!(
            block_on(witness.verify_statement(f.signed.clone())).is_some(),
            "the control: un-armed, the held head settles this very statement"
        );
    }

    let source = Arc::new(SpyChainSource {
        answer: None, // the real walk refuses: the chain's head is the new key
        dials: Mutex::new(Vec::new()),
    });
    let manager = welcome_joined_thread(f.old);
    let witness = ChainWitness::new(
        anchors_over_manager(&manager, &store),
        Shared(Arc::clone(&source)),
    );
    // The receive loop's prologue launched a sweep for this session.
    block_on(SuccessionWitness::harvest_armed(&witness));

    // The statement wins the race against the sweep's fetch.
    assert!(
        block_on(witness.verify_statement(f.signed.clone())).is_none(),
        "a held head must not settle a statement before this session's harvest \
         of that peer has settled"
    );
    assert!(
        dials(&source).is_empty(),
        "waiting is passive: no dial, the statement is parked for the re-drive"
    );
    let report = witness.observation();
    assert_eq!(report.peers[0].outcome, WitnessOutcome::AwaitingHarvest);

    // The sweep's fetch returns: the profile claims past the held head.
    let after = StubProfileNest {
        body: Some(signed_profile_bytes(
            &old_keypair(),
            Some(rotated_head(&f)),
            Some("https://home.example.test"),
        )),
    };
    assert!(matches!(
        block_on(harvest_peer_anchor(&after, &f.old, &Shared(Arc::clone(&store)))),
        HarvestOutcome::Seeded(seed) if seed.marked_outrun
    ));
    // The re-drive's own first acts, in its order.
    block_on(SuccessionWitness::anchor_seed_landed(&witness));
    block_on(SuccessionWitness::harvest_settled(&witness, &f.old));

    assert!(
        block_on(witness.verify_statement(f.signed.clone())).is_none(),
        "re-driven, the retired kit's statement is walked and the walk refuses it"
    );
    assert_eq!(
        dials(&source),
        vec!["home.example.test".to_string()],
        "the settle re-drives the statement into the WALK, never into tier 1"
    );
    let report = witness.observation();
    assert_eq!(report.peers[0].outcome, WitnessOutcome::WalkFailed);
    assert_eq!(
        report.peers[0].memo_hits, 0,
        "and the wait memoized nothing — a recorded refusal would have answered \
         the re-drive's own verify, and no walk would ever have run"
    );
}

/// The wait ends at ANY settle — and an un-seeded one buys no store read.
/// `NothingNew` is the honest majority (the peer never rotated), and an
/// offline member's budget running out is the case tier 1 exists for; both
/// must release the held head, and neither may advance the seed generation,
/// or the ratified once-per-generation store read degrades to once per
/// settled peer. Nor may a burst of statements during the wait drive a read.
#[test]
fn an_unseeded_settle_releases_the_wait_without_another_store_read() {
    let f = fixture();
    let store = Arc::new(LoadCountingAnchors::default());
    // An earlier session's harvest put the head at rest; its own store traffic
    // is not under test, so the counts below are taken from here.
    let earlier = StubProfileNest {
        body: Some(signed_profile_bytes(&old_keypair(), Some(f.head), None)),
    };
    block_on(harvest_peer_anchor(
        &earlier,
        &f.old,
        &Shared(Arc::clone(&store)),
    ));
    let at_login = store.reads();

    let source = Arc::new(SpyChainSource {
        answer: None,
        dials: Mutex::new(Vec::new()),
    });
    let manager = welcome_joined_thread(f.old);
    let witness = ChainWitness::new(
        fauna_client_recovery::ThreadParticipantAnchors::new(Arc::downgrade(lent(
            &manager,
            Arc::new(Shared(Arc::clone(&store))),
        ))),
        Shared(Arc::clone(&source)),
    );
    block_on(SuccessionWitness::harvest_armed(&witness));

    for _ in 0..20 {
        assert!(block_on(witness.verify_statement(f.signed.clone())).is_none());
    }
    assert_eq!(
        store.reads(),
        at_login + 1,
        "twenty waiting statements cost the session's one read between them"
    );
    assert!(dials(&source).is_empty(), "and no dial");

    // The sweep settled the peer with nothing new to seed.
    block_on(SuccessionWitness::harvest_settled(&witness, &f.old));

    let verified = block_on(witness.verify_statement(f.signed.clone()))
        .expect("an un-demoted head settles offline once the harvest has settled");
    assert_eq!(verified.new_actor_id, f.successor);
    assert_eq!(
        store.reads(),
        at_login + 1,
        "a settle that seeded nothing is not a seed generation"
    );
    assert!(dials(&source).is_empty());
}

/// **Every settle arm re-drives, enumerated off the pass's three branches.**
/// Before the wait, only `Seeded` re-drove — enough when the only parked
/// statement was one with no anchor. A statement parked behind the wait is
/// released by the settle itself, so a settle that stays silent strands it for
/// the session. The retryable branch re-drives exactly once, when the budget
/// runs out — never per attempt, which would release the wait on a nest that
/// was merely slow to come up.
#[test]
fn the_pass_redrives_every_settle_arm_and_only_a_settle() {
    let f = fixture();
    let log = fauna_client_recovery::harvest::HarvestLog::default();

    // Branch 3 — a standing, non-retryable outcome. `NothingNew`: the store
    // already holds exactly what the profile claims.
    {
        let store = Arc::new(MemoryAnchors::default());
        let nest = StubProfileNest {
            body: Some(signed_profile_bytes(&old_keypair(), Some(f.head), None)),
        };
        block_on(harvest_peer_anchor(
            &nest,
            &f.old,
            &Shared(Arc::clone(&store)),
        ));
        let manager = welcome_joined_thread(f.old);
        let redrive = SettleRecordingRedrive::default();
        let mut sweep = fauna_client_recovery::harvest::PeerAnchorSweepState::new();
        block_on(sweep.run_pass(
            lent(&manager, Arc::new(Shared(Arc::clone(&store)))),
            &nest,
            &log,
            &redrive,
        ));
        assert_eq!(redrive.unseeded.lock().unwrap().as_slice(), &[f.old]);
        assert!(redrive.seeded.lock().unwrap().is_empty());
        // …and a rejection (`NoProfile` — the cross-nest peer) likewise.
        let redrive = SettleRecordingRedrive::default();
        let mut sweep = fauna_client_recovery::harvest::PeerAnchorSweepState::new();
        let absent = StubProfileNest { body: None };
        block_on(sweep.run_pass(
            lent(&manager, Arc::new(Shared(Arc::clone(&store)))),
            &absent,
            &log,
            &redrive,
        ));
        assert_eq!(redrive.unseeded.lock().unwrap().as_slice(), &[f.old]);
    }

    // Branch 1 — retryable. The offline member: five attempts on the 1+2+4+8
    // tick ladder, i.e. sixteen passes, and the settle re-drives on the last.
    let store = Arc::new(MemoryAnchors::default());
    let manager = welcome_joined_thread(f.old);
    let redrive = SettleRecordingRedrive::default();
    let mut sweep = fauna_client_recovery::harvest::PeerAnchorSweepState::new();
    for pass in 1..=15 {
        block_on(sweep.run_pass(
            lent(&manager, Arc::new(Shared(Arc::clone(&store)))),
            &UnreachableProfileNest,
            &log,
            &redrive,
        ));
        assert!(
            redrive.unseeded.lock().unwrap().is_empty(),
            "pass {pass}: a retry still owed is not a settle"
        );
    }
    block_on(sweep.run_pass(
        lent(&manager, Arc::new(Shared(Arc::clone(&store)))),
        &UnreachableProfileNest,
        &log,
        &redrive,
    ));
    assert_eq!(
        redrive.unseeded.lock().unwrap().as_slice(),
        &[f.old],
        "the budget running out IS the settle: tier 1 exists for the offline member"
    );
    assert!(redrive.seeded.lock().unwrap().is_empty());
}

/// The sweep walks the member's ROSTERS, not the conversations page's view of
/// them. It used to read `ConversationsManager::snapshot`, which applies the
/// page's search filter — so a query typed into the search box silently
/// narrowed which peers were ever harvested, and a statement waiting on one of
/// the hidden ones would wait for as long as the query stood.
#[test]
fn the_sweep_attempts_a_peer_the_search_box_is_hiding() {
    let f = fixture();
    let store = Arc::new(MemoryAnchors::default());
    let manager = welcome_joined_thread(f.old);
    manager.set_search_query(Some("matches no thread at all".to_string()));
    assert!(
        manager.snapshot().threads.is_empty(),
        "the view is filtered"
    );

    let nest = StubProfileNest {
        body: Some(signed_profile_bytes(&old_keypair(), Some(f.head), None)),
    };
    let redrive = SettleRecordingRedrive::default();
    let log = fauna_client_recovery::harvest::HarvestLog::default();
    let mut sweep = fauna_client_recovery::harvest::PeerAnchorSweepState::new();
    block_on(sweep.run_pass(
        lent(&manager, Arc::new(Shared(Arc::clone(&store)))),
        &nest,
        &log,
        &redrive,
    ));
    assert_eq!(stored_head(&store, &f.old), Some(f.head));
}

// ── a bare name's line: `resolve_line` ──────────────────────────────────────

/// Anchors a test can change mid-session — the harvest seeding a domain after
/// the first ask.
#[derive(Default)]
struct LateAnchors {
    handle: Mutex<Option<String>>,
    home_domain: Mutex<Option<String>>,
    head: Option<ChainHead>,
    remembered: Mutex<Vec<(ActorId, ChainHead)>>,
}

#[async_trait]
impl SuccessionAnchors for Shared<LateAnchors> {
    async fn known_head(&self, _actor: &ActorId) -> Option<ChainHead> {
        self.0.head
    }
    async fn known_handle(&self, _actor: &ActorId) -> Option<String> {
        self.0.handle.lock().unwrap().clone()
    }
    async fn known_home_domain(&self, _actor: &ActorId) -> Option<String> {
        self.0.home_domain.lock().unwrap().clone()
    }
    async fn remember_head(&self, actor: &ActorId, head: ChainHead) {
        self.0.remembered.lock().unwrap().push((*actor, head));
    }
}

/// A chain source that answers line walks, recording each dial's domain and
/// the guard head it was handed.
struct LineSpy {
    line: Mutex<Option<Vec<VerifiedSuccession>>>,
    dials: Mutex<Vec<(String, Option<ChainHead>)>>,
}

#[async_trait]
impl SuccessionChainSource for Shared<LineSpy> {
    async fn walk_from_domain(
        &self,
        _handle_domain: &str,
        _old: ActorId,
        _known_head: Option<ChainHead>,
    ) -> Option<VerifiedSuccession> {
        panic!("a bare name carries no statement: only the line walk is dialled");
    }
    async fn walk_line_from_domain(
        &self,
        handle_domain: &str,
        _old: ActorId,
        known_head: Option<ChainHead>,
    ) -> Option<Vec<VerifiedSuccession>> {
        self.0
            .dials
            .lock()
            .unwrap()
            .push((handle_domain.to_string(), known_head));
        self.0.line.lock().unwrap().clone()
    }
}

type LineWitness = ChainWitness<Shared<LateAnchors>, Shared<LineSpy>>;

fn line_witness(
    anchors: LateAnchors,
    line: Option<Vec<VerifiedSuccession>>,
) -> (LineWitness, Arc<LateAnchors>, Arc<LineSpy>) {
    let anchors = Arc::new(anchors);
    let spy = Arc::new(LineSpy {
        line: Mutex::new(line),
        dials: Mutex::new(Vec::new()),
    });
    (
        ChainWitness::new(Shared(Arc::clone(&anchors)), Shared(Arc::clone(&spy))),
        anchors,
        spy,
    )
}

/// A twice-succeeded identity's two verified hops, each under its own chain.
fn two_hops(f: &Fixture) -> (Vec<VerifiedSuccession>, ActorId) {
    let third = ActorKeypair::from_secret([33u8; 32]).actor_id();
    let second_head = ChainHead::new(RecoveryKey::generate().public(), 1);
    (
        vec![
            VerifiedSuccession {
                old_actor_id: f.old,
                new_actor_id: f.successor,
                seq: 2,
                chain_head: f.head,
            },
            VerifiedSuccession {
                old_actor_id: f.successor,
                new_actor_id: third,
                seq: 2,
                chain_head: second_head,
            },
        ],
        third,
    )
}

/// The whole line comes back — the intermediate successor included, which is
/// what a community policy chain needs and the terminal-only walk drops — from
/// ONE dial to the owner's own handle domain, guarded by the held head; and the
/// head remembered for the name is the FIRST hop's (the name's own chain), not
/// the terminal's, which belongs to another identity.
#[test]
fn a_bare_names_line_is_walked_once_from_the_owners_own_anchor() {
    use fauna_client_recovery::LineResolution;
    let f = fixture();
    let (hops, third) = two_hops(&f);
    let (witness, anchors, spy) = line_witness(
        LateAnchors {
            handle: Mutex::new(Some("alice@example.test".into())),
            home_domain: Mutex::new(Some("harvested.test".into())),
            head: Some(f.head),
            ..Default::default()
        },
        Some(hops),
    );

    for _ in 0..2 {
        assert_eq!(
            block_on(witness.resolve_line(&f.old)),
            LineResolution::Verified(vec![f.successor, third])
        );
    }
    assert_eq!(
        *spy.dials.lock().unwrap(),
        vec![("example.test".to_string(), Some(f.head))],
        "one dial a session, to the anchor-grade handle's domain (it outranks \
         the harvested one), with the held head as the rewrite guard"
    );
    assert_eq!(*anchors.remembered.lock().unwrap(), vec![(f.old, f.head)]);
}

/// Nothing independent to anchor on: no dial, nothing established — and
/// nothing memoized, so the harvest seeding a domain later is used at once. A
/// verified "never succeeded" is an answer of its own, not a failure.
#[test]
fn a_name_with_no_anchor_resolves_nothing_until_a_harvest_seeds_one() {
    use fauna_client_recovery::LineResolution;
    let f = fixture();
    let (witness, anchors, spy) = line_witness(LateAnchors::default(), Some(Vec::new()));

    assert_eq!(
        block_on(witness.resolve_line(&f.old)),
        LineResolution::NotYet
    );
    assert!(spy.dials.lock().unwrap().is_empty(), "no anchor, no dial");

    *anchors.home_domain.lock().unwrap() = Some("harvested.test".into());
    assert_eq!(
        block_on(witness.resolve_line(&f.old)),
        LineResolution::Verified(Vec::new())
    );
    assert_eq!(spy.dials.lock().unwrap().len(), 1);
    assert_eq!(spy.dials.lock().unwrap()[0].0, "harvested.test");
    assert!(anchors.remembered.lock().unwrap().is_empty());
}

/// **Widening who is harvested did not widen who may name a dial target.**
///
/// A community policy's never-met names now reach the harvest
/// (`identity-succession.md` § The succession statement → *a community
/// policy's names join the harvest's walk*), which is the change most likely
/// to tempt a later hand into threading a domain through from the asking
/// context — the room. `resolve_line` takes the name and nothing else, so the
/// only dial targets it can reach are the ones this device's own anchors hold.
/// A compile-time pin: growing a parameter breaks the coercion below.
#[test]
fn resolve_line_still_takes_no_caller_supplied_dial_target() {
    fn takes_the_name_and_nothing_else<'a, F>(_: fn(&'a LineWitness, &'a ActorId) -> F) {}
    takes_the_name_and_nothing_else(LineWitness::resolve_line);
}

/// **The sweep harvests a policy name no thread carries** — a retired owner the
/// member never shared a thread with, offered by the room backend — through
/// the ordinary door, once per session, re-driving on the seed like any peer.
#[test]
fn the_sweep_harvests_a_policy_name_no_thread_carries() {
    let f = fixture();
    let store = Arc::new(MemoryAnchors::default());
    let manager = fauna_conversations::ConversationsManager::new();
    assert!(manager.fauna_roster_actors().is_empty(), "no thread at all");
    manager.set_policy_anchor_wants(fauna_mls::types::ChannelId([0x77; 32]), vec![f.old]);
    let nest = CountingProfileNest {
        inner: StubProfileNest {
            body: Some(signed_profile_bytes(&old_keypair(), Some(f.head), None)),
        },
        fetches: Default::default(),
    };
    let redrive = RecordingRedrive::default();
    let log = fauna_client_recovery::harvest::HarvestLog::default();
    let mut sweep = fauna_client_recovery::harvest::PeerAnchorSweepState::new();

    block_on(sweep.run_pass(
        lent(&manager, Arc::new(Shared(Arc::clone(&store)))),
        &nest,
        &log,
        &redrive,
    ));
    assert_eq!(
        stored_head(&store, &f.old),
        Some(f.head),
        "a never-met policy name is seeded through the ordinary harvest door"
    );
    assert_eq!(
        redrive.seen.lock().unwrap().as_slice(),
        &[f.old],
        "the seed is announced, so the witness re-reads its anchors"
    );
    block_on(sweep.run_pass(
        lent(&manager, Arc::new(Shared(Arc::clone(&store)))),
        &nest,
        &log,
        &redrive,
    ));
    assert_eq!(
        nest.fetches.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "once per name per session, like any peer"
    );
}

/// An anchor that cannot be reached, or a chain that fails the rule,
/// establishes nothing — and is dialled once, not once per judgment.
#[test]
fn a_failed_line_walk_establishes_nothing_and_is_not_redialled() {
    use fauna_client_recovery::LineResolution;
    let f = fixture();
    let (witness, _anchors, spy) = line_witness(
        LateAnchors {
            home_domain: Mutex::new(Some("down.test".into())),
            ..Default::default()
        },
        None,
    );
    for _ in 0..3 {
        assert_eq!(
            block_on(witness.resolve_line(&f.old)),
            LineResolution::NotYet
        );
    }
    assert_eq!(spy.dials.lock().unwrap().len(), 1);
}

/// **A recheck walks again whatever the memo holds — an empty line, a
/// positive one, a walk that failed.**
///
/// `conversation-rooms.md` § Roles and authorization → *A name designates its
/// verified line* → *A verified line holds only so far*: a name that never
/// succeeded may succeed later in the session, and the newest holder of a
/// positive line may succeed in turn — a member that kept either answer for
/// good would refuse the successor's seat until it quit. So `recheck_line`
/// forgets the memo and walks again (how often is the caller's bound — the
/// room's re-ask cadence, inside its first-ask budget), while an ordinary ask
/// is still answered from the memo. The memo is the last dial's answer: which
/// answers a room keeps is `SuccessionLines`' rule, pinned in `room_policy.rs`.
#[test]
fn a_recheck_rewalks_whatever_the_memo_holds() {
    use fauna_client_recovery::LineResolution;
    let f = fixture();
    let (witness, _anchors, spy) = line_witness(
        LateAnchors {
            home_domain: Mutex::new(Some("home.test".into())),
            ..Default::default()
        },
        Some(Vec::new()),
    );

    assert_eq!(
        block_on(witness.resolve_line(&f.old)),
        LineResolution::Verified(Vec::new())
    );
    assert_eq!(
        block_on(witness.resolve_line(&f.old)),
        LineResolution::Verified(Vec::new()),
        "an ordinary ask is still memoized"
    );
    assert_eq!(spy.dials.lock().unwrap().len(), 1);

    // The identity succeeds after the first walk.
    *spy.line.lock().unwrap() = Some(vec![VerifiedSuccession {
        old_actor_id: f.old,
        new_actor_id: f.successor,
        seq: 2,
        chain_head: f.head,
    }]);
    assert_eq!(
        block_on(witness.recheck_line(&f.old)),
        LineResolution::Verified(vec![f.successor]),
        "a recheck forgets the empty memo and walks again"
    );
    assert_eq!(spy.dials.lock().unwrap().len(), 2);
    assert_eq!(
        block_on(witness.resolve_line(&f.old)),
        LineResolution::Verified(vec![f.successor]),
        "and the line it learned is the memo from then on"
    );
    assert_eq!(spy.dials.lock().unwrap().len(), 2);

    // The anchor is down at the next recheck: the walk fails …
    *spy.line.lock().unwrap() = None;
    assert_eq!(
        block_on(witness.recheck_line(&f.old)),
        LineResolution::NotYet,
        "a positive line is walked again on a recheck too"
    );
    assert_eq!(spy.dials.lock().unwrap().len(), 3);

    // … and the successor has succeeded in turn: the recheck after that
    // learns the whole line.
    let (hops, third) = two_hops(&f);
    *spy.line.lock().unwrap() = Some(hops);
    assert_eq!(
        block_on(witness.recheck_line(&f.old)),
        LineResolution::Verified(vec![f.successor, third]),
        "a failed walk does not stop the next recheck from dialling"
    );
    assert_eq!(spy.dials.lock().unwrap().len(), 4);
    assert_eq!(
        block_on(witness.resolve_line(&f.old)),
        LineResolution::Verified(vec![f.successor, third])
    );
    assert_eq!(
        spy.dials.lock().unwrap().len(),
        4,
        "an ordinary ask never dials past the memo"
    );
}

// ── the store is lent late: the account-store-ready edge ─────────────────────
//
// The anchors rest on the account plane, whose store resolves AFTER login, so
// the witness and the sweep are built before any store exists and read it
// through the manager (`ConversationsManager::peer_anchor_store`), which the
// shared store-ready registration fills. These pin both ends of that window.

/// Before the store is lent the anchors read as **unreadable**, never as
/// empty; once it is lent the next consult (after the failed read's backoff)
/// reads it and settles at tier 1 offline; an identity change retires it, so
/// the incoming account can never read — or write — the outgoing account's
/// heads.
#[test]
fn an_anchor_store_lent_after_login_is_read_and_an_identity_change_retires_it() {
    use fauna_client_recovery::witness::AnchorStoreState;
    let f = fixture();
    let manager = welcome_joined_thread(f.old);
    let source = Arc::new(SpyChainSource {
        answer: None,
        dials: Mutex::new(Vec::new()),
    });
    let clock = TestClock::default();
    clock.advance_secs(1_000);
    let witness = ChainWitness::new(
        fauna_client_recovery::ThreadParticipantAnchors::with_clock(
            Arc::downgrade(&manager),
            clock.reader(),
        ),
        Shared(Arc::clone(&source)),
    );

    assert!(block_on(witness.verify_statement(f.signed.clone())).is_none());
    assert_eq!(
        witness.observation().anchor_store,
        AnchorStoreState::Unreadable,
        "a store not lent yet is unreadable — an empty answer would be TOFU on \
         evidence that does not exist"
    );

    // The account-store-ready edge: a store already holding the head.
    let store = Arc::new(MemoryAnchors::default());
    {
        let mut anchors = PeerAnchors::default();
        assert!(anchors.remember_chain_head(f.old, f.head));
        *store.anchors.lock().unwrap() = Some(anchors);
    }
    lent(&manager, Arc::new(Shared(Arc::clone(&store))));
    clock.advance_secs(fauna_client_recovery::UNREADABLE_STORE_BACKOFF_SECS);

    let verified = block_on(witness.verify_statement(f.signed.clone()))
        .expect("the lent store's head settles the statement");
    assert_eq!(verified.new_actor_id, f.successor);
    assert!(
        dials(&source).is_empty(),
        "settled at tier 1 from the lent store, offline"
    );

    manager.clear_for_identity_change();
    assert!(
        manager.peer_anchor_store().is_none(),
        "an identity change retires the outgoing account's anchor store"
    );
}

/// A sweep pass before the store is lent does NOTHING — no fetch, no log
/// entry, no ageing: the store resolves a moment after the sweep launches, and
/// a failure logged for that window would be the peer's first recorded
/// outcome, spent against the budget of the very attempt that should seed it.
/// The first pass after the lend is the peer's first attempt, and seeds.
#[test]
fn a_sweep_before_the_store_is_lent_waits_and_its_first_attempt_after_seeds() {
    let f = fixture();
    let manager = welcome_joined_thread(f.old);
    let nest = CountingProfileNest {
        inner: StubProfileNest {
            body: Some(signed_profile_bytes(&old_keypair(), Some(f.head), None)),
        },
        fetches: Default::default(),
    };
    let redrive = RecordingRedrive::default();
    let log = fauna_client_recovery::harvest::HarvestLog::default();
    let mut sweep = fauna_client_recovery::harvest::PeerAnchorSweepState::new();

    for _ in 0..3 {
        block_on(sweep.run_pass(&manager, &nest, &log, &redrive));
    }
    assert_eq!(
        nest.fetches.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "no store, no fetch"
    );
    assert!(
        log.entries().is_empty(),
        "a store not lent yet is not a harvest attempt: {:?}",
        log.entries()
    );

    let store = Arc::new(MemoryAnchors::default());
    lent(&manager, Arc::new(Shared(Arc::clone(&store))));
    block_on(sweep.run_pass(&manager, &nest, &log, &redrive));
    let entries = log.entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].attempts, 1,
        "the first attempt after the lend is the first"
    );
    assert!(matches!(entries[0].last, HarvestOutcome::Seeded(_)));
    assert_eq!(stored_head(&store, &f.old), Some(f.head));
}

/// The wait is BOUNDED: an account runtime that never assembles must still
/// settle the harvest wait, so past `UNLENT_STORE_GRACE_PASSES` an unlent store
/// is every due peer's retryable `StoreFailed` — still with no fetch.
#[test]
fn past_the_grace_an_unlent_store_is_charged_as_store_failed() {
    use fauna_client_recovery::harvest::UNLENT_STORE_GRACE_PASSES;
    let f = fixture();
    let manager = welcome_joined_thread(f.old);
    let nest = CountingProfileNest {
        inner: StubProfileNest {
            body: Some(signed_profile_bytes(&old_keypair(), Some(f.head), None)),
        },
        fetches: Default::default(),
    };
    let redrive = RecordingRedrive::default();
    let log = fauna_client_recovery::harvest::HarvestLog::default();
    let mut sweep = fauna_client_recovery::harvest::PeerAnchorSweepState::new();

    for _ in 0..UNLENT_STORE_GRACE_PASSES {
        block_on(sweep.run_pass(&manager, &nest, &log, &redrive));
    }
    assert!(
        log.entries().is_empty(),
        "inside the grace nothing is charged"
    );
    block_on(sweep.run_pass(&manager, &nest, &log, &redrive));
    assert_eq!(log.entries()[0].last, HarvestOutcome::StoreFailed);
    assert_eq!(
        nest.fetches.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "and still no fetch for a seed that could not be kept"
    );
}
