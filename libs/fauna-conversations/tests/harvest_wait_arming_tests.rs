//! **The receive loop arms the succession witness's harvest wait — in its
//! prologue, and only when a sweep was launched.**
//!
//! `identity-succession.md` § The succession statement → *the harvest wait*: in
//! a session that runs a peer-anchor harvest sweep, a held chain head settles no
//! statement until the sweep has settled that peer. The witness learns that a
//! sweep runs from exactly one call, `SuccessionWitness::harvest_armed`, and
//! that call has two ways to rot silently, because the wait **fails open**:
//!
//! - **dropped or moved below the poll spawn** — the witness never waits (or
//!   starts waiting after the first statement was already decoded), every pin on
//!   the witness itself stays green, and the window the rule closes is simply
//!   open again;
//! - **made unconditional** — a session with no sweep tells its witness to wait
//!   for a settle nothing will ever announce, and no statement settles offline
//!   for the rest of that session.
//!
//! Both are ordering facts about `start_receive_loop`, so they are pinned here
//! as an order over one recorded sequence. The barrier is the index-builder
//! launch, which the prologue awaits *after* the sweep launch and *before* it
//! spawns the poll task: once that launch has been entered, everything the
//! prologue does about the sweep has already happened, and no poll has run.
//! Latency-independent (convention 14): one deadline-polled positive wait, and
//! the verdict is the recorded order, never a settle-sleep.

mod common;
use common::SilentNest;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use fauna_conversations::ConversationsSession;
use fauna_conversations::backend::{
    IndexBuilderLauncher, NestCorpus, PeerAnchorSweepLauncher, SuccessionWitness,
};
use fauna_conversations::index_sink::MessageIndexObserver;
use fauna_conversations::manager::ConversationsManager;
use fauna_core::identity::ActorKeypair;
use fauna_mls::engine::MlsEngine;

/// Generous ceiling for "the loop reached the end of its prologue". A green run
/// pays none of it.
const PROLOGUE_BUDGET: Duration = Duration::from_secs(120);

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Event {
    SweepLaunched,
    WitnessArmed,
    /// The barrier: the prologue is past the sweep launch, the poll task is not
    /// yet spawned.
    IndexLaunchEntered,
}

#[derive(Default)]
struct Journal(Mutex<Vec<Event>>);

impl Journal {
    fn push(&self, e: Event) {
        self.0.lock().unwrap().push(e);
    }
    fn events(&self) -> Vec<Event> {
        self.0.lock().unwrap().clone()
    }
}

struct RecordingSweep(Arc<Journal>);

#[async_trait]
impl PeerAnchorSweepLauncher for RecordingSweep {
    async fn launch(&self) {
        self.0.push(Event::SweepLaunched);
    }
}

struct RecordingWitness(Arc<Journal>);

#[async_trait]
impl SuccessionWitness for RecordingWitness {
    async fn verify(
        &self,
        _statement: fauna_core::recovery::SignedIdentitySuccession,
    ) -> Option<fauna_core::recovery::VerifiedSuccession> {
        None
    }

    async fn harvest_armed(&self) {
        self.0.push(Event::WitnessArmed);
    }
}

struct BarrierLauncher(Arc<Journal>);

#[async_trait]
impl IndexBuilderLauncher for BarrierLauncher {
    async fn launch(&self) -> Option<Arc<dyn MessageIndexObserver>> {
        self.0.push(Event::IndexLaunchEntered);
        None
    }

    async fn corpus_changed(&self, _corpus: NestCorpus) {}
}

/// Run the receive loop's prologue over a session wired with a witness, the
/// barrier, and — iff `with_sweep` — a sweep launcher; return what happened up
/// to the barrier.
async fn prologue_events(with_sweep: bool) -> Vec<Event> {
    let journal = Arc::new(Journal::default());
    let manager = ConversationsManager::new();
    let engine = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let self_actor = engine.identity_actor_id();
    let session = ConversationsSession::from_manager(
        Arc::clone(&manager),
        engine,
        Arc::new(SilentNest {
            label: "harvest-wait arming test",
            permissive: false,
        }),
        "me@example.com".to_string(),
        self_actor,
        None,
    );
    session.set_succession_witness(Arc::new(RecordingWitness(Arc::clone(&journal))));
    if with_sweep {
        session.set_peer_anchor_sweep_launcher(Arc::new(RecordingSweep(Arc::clone(&journal))));
    }
    session.set_index_builder_launcher(Arc::new(BarrierLauncher(Arc::clone(&journal))));

    let loop_session = Arc::clone(&session);
    tokio::spawn(async move { loop_session.start_receive_loop().await });

    let deadline = tokio::time::Instant::now() + PROLOGUE_BUDGET;
    while !journal.events().contains(&Event::IndexLaunchEntered) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for the prologue to reach the index launch; journal: {:?}",
            journal.events()
        );
        // sleep-ok: the poll interval of a deadline wait, not the assertion.
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    journal.events()
}

/// A launched sweep arms the witness — after the launch, before the barrier,
/// i.e. before any inbound poll can decode a statement.
#[tokio::test(flavor = "multi_thread")]
async fn a_launched_sweep_arms_the_witness_before_the_first_poll() {
    assert_eq!(
        prologue_events(true).await,
        vec![
            Event::SweepLaunched,
            Event::WitnessArmed,
            Event::IndexLaunchEntered
        ],
        "the wait is armed in the prologue, right behind the sweep it waits on"
    );
}

/// No sweep, no wait: nothing would ever announce a settle, so an armed witness
/// here would never settle a statement offline again.
#[tokio::test(flavor = "multi_thread")]
async fn a_session_with_no_sweep_never_arms_the_wait() {
    assert_eq!(
        prologue_events(false).await,
        vec![Event::IndexLaunchEntered],
        "a session that launched no sweep must leave its witness un-armed"
    );
}
