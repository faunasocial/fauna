//! tier_1: the **index observer is registered before the first mail is
//! ingested** — the ordering contract `start_receive_loop` exists to guarantee.
//!
//! Proof obligation (`docs/goal/behavior/content-index.md` § Ingest triggers,
//! v1): app glue must register the index observer *before the receive loop
//! starts*. The reason is specific and the failure is silent — the client keeps
//! no restart-durable mail cursor (`Session::mail_cursors` is per-session and
//! starts at `(0, 0)`), so every launch re-pages the entire mailbox from UID 0
//! and the index seam fires for all of it. An observer registered even slightly
//! late misses part or all of that walk, and *that launch's mail is simply not
//! searchable* — with no error anywhere, on any layer.
//!
//! That is exactly the class of contract a comment cannot hold: the ordering is
//! two statements apart in one function, and any future edit that moves the
//! launch after the `tokio::spawn` compiles, runs, and passes every other test.
//! So it is pinned here as a **flow assertion** rather than asserted by reading
//! the source:
//!
//! ```text
//! session.set_index_builder_launcher(..)
//!   -> start_receive_loop() prologue
//!     -> launcher.launch().await          (resume the builder)
//!       -> manager.set_index_observer(..) (register)
//!         -> [poll task spawns]           (mailbox re-walk begins)
//!           -> ingest_inbound -> observe_indexable_message   <- must be seen
//! ```
//!
//! **Why this test holds a gate rather than just watching the mail arrive.** The
//! obvious version — register a recording observer, wait for the first message —
//! passes whether or not the ordering holds, because a stub launcher returns so
//! fast it wins the race even when the launch is (wrongly) spawned. It was
//! written that way first, and the mutation that models the exact regression
//! (move the launch into the spawned task) left it **green**. So the launcher
//! here **blocks on a gate the test opens**, which turns a race into a causal
//! fact: with the launch correctly in the prologue, a fetch before the gate
//! opens is not unlikely, it is *impossible* — the loop is suspended inside
//! `launch()`. With the launch spawned, the poll task is already running and
//! fetches while the gate is held.
//!
//! The assert is latency-independent (convention 14): the positive waits use
//! named generous budgets with deadline polls, and the negative assert ("no
//! fetch happened yet") is anchored to the gate — a causal barrier — not to a
//! settle-sleep.

mod common;
use common::SilentNest;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use fauna_conversations::ConversationsSession;
use fauna_conversations::backend::{
    InboundMailPage, InboundMailRecord, InboundMailSource, IndexBuilderLauncher, NestCorpus,
    OutboundMailSink,
};
use fauna_conversations::index_sink::{IndexableMessage, MessageIndexObserver};
use fauna_conversations::manager::ConversationsManager;
use fauna_core::identity::ActorKeypair;
use fauna_mls::engine::MlsEngine;

/// Generous ceiling for "the loop got through its prologue and one poll pass".
/// Sized far above any non-pathological delay so a busy machine never flips the
/// verdict; a green run pays none of it.
const OBSERVED_BUDGET: Duration = Duration::from_secs(120);

/// The single ordered log both sides write to — the ordering claim is a
/// statement about this sequence, so it is recorded rather than inferred.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Event {
    /// `launch()` was entered — the loop is now inside the prologue.
    LaunchEntered,
    /// The test released the gate.
    GateOpened,
    /// `launch()` returned its observer.
    LaunchReturned,
    /// The mail source was polled — this is what must never precede the launch.
    Fetched,
    /// The index seam offered a message to the observer.
    Observed,
}

#[derive(Default)]
struct Journal {
    events: Mutex<Vec<Event>>,
    seen: Mutex<Vec<String>>,
}

impl Journal {
    fn push(&self, e: Event) {
        self.events.lock().unwrap().push(e);
    }
    fn events(&self) -> Vec<Event> {
        self.events.lock().unwrap().clone()
    }
    fn contains(&self, e: Event) -> bool {
        self.events.lock().unwrap().contains(&e)
    }
}

/// Records every message the index seam offered, so the test can assert the
/// first-pass mail was seen at all.
struct RecordingObserver {
    journal: Arc<Journal>,
}

impl MessageIndexObserver for RecordingObserver {
    fn observe_indexable_message(&self, msg: IndexableMessage<'_>) {
        self.journal.push(Event::Observed);
        self.journal
            .seen
            .lock()
            .unwrap()
            .push(msg.message_id.0.clone());
    }
}

/// Stands in for `NestMailIndexLauncher` — its real work (load the MSEK, resume
/// off the `__index` rail) is irrelevant here, but its **latency** is the whole
/// point: a real launcher does network I/O, so it cannot be assumed to complete
/// before the first poll. The gate models that latency causally instead of with
/// a sleep.
struct GatedLauncher {
    journal: Arc<Journal>,
    gate: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl IndexBuilderLauncher for GatedLauncher {
    async fn launch(&self) -> Option<Arc<dyn MessageIndexObserver>> {
        self.journal.push(Event::LaunchEntered);
        self.gate.notified().await;
        self.journal.push(Event::LaunchReturned);
        Some(Arc::new(RecordingObserver {
            journal: Arc::clone(&self.journal),
        }) as Arc<dyn MessageIndexObserver>)
    }

    /// No walk to re-run — this fake launcher has no third-ingest-class arm.
    /// Written out rather than inherited: the trait requires the method so a
    /// real launcher cannot silently skip it (see its doc comment).
    async fn corpus_changed(&self, _corpus: NestCorpus) {}
}

/// One inbound message on the first page, then nothing — the shape of a launch
/// that re-walks a one-message mailbox. Every poll is journalled, because "was
/// the mailbox read before the index was ready?" is the question.
struct OneMessageMailbox {
    journal: Arc<Journal>,
}

#[async_trait]
impl InboundMailSource for OneMessageMailbox {
    async fn fetch(&self, after_uid: u32, _limit: u32) -> Result<InboundMailPage, String> {
        self.journal.push(Event::Fetched);
        if after_uid > 0 {
            return Ok(InboundMailPage {
                skipped: Vec::new(),
                highest_modseq: 0,
                records: vec![],
                more: false,
            });
        }
        Ok(InboundMailPage {
            skipped: Vec::new(),
            highest_modseq: 0,
            records: vec![InboundMailRecord {
                uid: 1,
                message_id: b"first-pass-message".to_vec(),
                internal_date_ms: 1_700_000_000_000,
                rfc5322: concat!(
                    "From: sender@example.com\r\n",
                    "To: me@example.com\r\n",
                    "Subject: quarterly report\r\n",
                    "Message-ID: <first-pass@example.com>\r\n",
                    "\r\n",
                    "the numbers are in\r\n",
                )
                .as_bytes()
                .to_vec(),
                mailbox: fauna_conversations::backend::MailFeed::Inbox,
                suppress_from_view: false,
                has_seen_flag: false,
            }],
            more: false,
        })
    }
}

/// The send half the mail rail needs registered before it can receive — this
/// test never sends. (`register_smtp` is what puts the `SmtpBackend` on the
/// manager, and `ingest_inbound` routes through it; the app glue registers it
/// before the read feeds for exactly this reason.)
struct NoSend;

#[async_trait]
impl OutboundMailSink for NoSend {
    async fn submit(&self, _recipients: Vec<String>, _raw: Vec<u8>) -> Result<(), String> {
        Err("this test never sends".into())
    }
}

/// Deadline-poll for a journalled event. Positive wait, generous budget, no
/// wall-clock dependency in the verdict.
async fn await_event(journal: &Journal, want: Event, what: &str) {
    let deadline = tokio::time::Instant::now() + OBSERVED_BUDGET;
    while !journal.contains(want) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}; journal so far: {:?}",
            journal.events()
        );
        // sleep-ok: the poll interval of a deadline wait, not the assertion.
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// **The pin.** The receive loop does not read the mailbox until the index
/// launch has completed and its observer is registered.
///
/// Break the ordering — move the launch below the `tokio::spawn`, or spawn the
/// resume instead of awaiting it — and this reddens on the `Fetched`-before-
/// `LaunchReturned` assertion: the poll task starts reading mail while the
/// launcher is still gated, which in production is a launch whose mail is
/// silently never indexed.
#[tokio::test(flavor = "multi_thread")]
async fn the_mailbox_is_not_read_until_the_index_observer_is_registered() {
    let journal = Arc::new(Journal::default());
    let gate = Arc::new(tokio::sync::Notify::new());

    let manager = ConversationsManager::new();
    let engine = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let self_actor = engine.identity_actor_id();
    let session = ConversationsSession::from_manager(
        Arc::clone(&manager),
        engine,
        Arc::new(SilentNest {
            label: "index-launch ordering test",
            permissive: false,
        }),
        "me@example.com".to_string(),
        self_actor,
        None,
    );

    // Send rail first — the receive path's `ingest_inbound` routes through the
    // `SmtpBackend` this registers (the same ordering the tui/linux glue uses).
    session.register_smtp(Arc::new(NoSend));
    session.set_index_builder_launcher(Arc::new(GatedLauncher {
        journal: Arc::clone(&journal),
        gate: Arc::clone(&gate),
    }));
    // Only the INBOX feed matters here; the Sent twin would prove the same edge.
    session.register_mail_receive(
        Arc::new(OneMessageMailbox {
            journal: Arc::clone(&journal),
        }),
        Arc::new(OneMessageMailbox {
            journal: Arc::clone(&journal),
        }),
    );

    let loop_session = Arc::clone(&session);
    tokio::spawn(async move { loop_session.start_receive_loop().await });

    // The loop has reached the launch and is now suspended inside it.
    await_event(&journal, Event::LaunchEntered, "the index launch to begin").await;

    // The causal barrier: while the gate is held, correct code *cannot* fetch —
    // the loop is parked inside `launch()`. Code that spawned the launch instead
    // is already polling the mailbox, and this is where that shows up.
    assert!(
        !journal.contains(Event::Fetched),
        "the mailbox was read while the index launch was still in flight — the launch \
         is no longer awaited before the poll task starts, so a real (network-bound) \
         launcher would miss part or all of this launch's mailbox walk. Journal: {:?}",
        journal.events()
    );

    journal.push(Event::GateOpened);
    gate.notify_waiters();

    await_event(
        &journal,
        Event::Observed,
        "the first-pass mail to be indexed",
    )
    .await;

    // The whole claim, as an order over one recorded sequence.
    let events = journal.events();
    let launched = events
        .iter()
        .position(|e| *e == Event::LaunchReturned)
        .expect("the launch should have returned an observer");
    let first_fetch = events
        .iter()
        .position(|e| *e == Event::Fetched)
        .expect("the mailbox should have been polled");
    assert!(
        launched < first_fetch,
        "the index observer must be registered before the first mailbox read; got {events:?}"
    );

    let seen = journal.seen.lock().unwrap().clone();
    assert_eq!(
        seen,
        vec!["<first-pass@example.com>".to_string()],
        "the observer should have seen exactly the first-pass message"
    );
}
