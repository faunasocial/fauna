//! tier_1: the **catch-up boundary** the content-index builder's advisory lease
//! is defined against — where the launch backlog ends and the receive-path
//! trickle begins.
//!
//! Proof obligation (`docs/goal/behavior/content-index.md` § Where the index is
//! built → *The builder and the advisory task lease*): a stand-down may bind
//! only build work whose queue **re-presents**. The launch catch-up walk does
//! (every launch re-pages the mailbox from UID 0); the trickle does not (a doc
//! flows past the seam once per session). So the builder needs to know which
//! side of the line it is on, and the goal doc names the line precisely:
//!
//! > The catch-up boundary is the session's first mail sweep — already a
//! > structural fact of the receive loop, not new machinery. […] The build adds
//! > the *signal*, not the boundary.
//!
//! This file pins the signal, as an assertion about an **ordered journal**
//! rather than an end state. That distinction is the lesson of the sibling
//! `index_launch_ordering_tests.rs`: on a single seat, "gated" and "ungated"
//! reach an identical end state once the walk finishes, so any test that looks
//! only at what got indexed is a zero pin — it passes under a mutation that
//! deletes the whole mechanism.
//!
//! Both waits here are causal barriers, never settle-sleeps (convention 14): the
//! test drives each sweep itself through the push seam and waits on a journalled
//! event, so nothing depends on the loop's 30 s backstop ticker or on how loaded
//! the machine is.

mod common;
use common::SilentNest;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use fauna_conversations::ConversationsSession;
use fauna_conversations::backend::{
    ConvPushEvent, ConversationsPush, InboundMailPage, InboundMailRecord, InboundMailSource,
    IndexBuilderLauncher, NestCorpus, OutboundMailSink,
};
use fauna_conversations::index_sink::{IndexableKind, IndexableMessage, MessageIndexObserver};
use fauna_conversations::manager::ConversationsManager;
use fauna_core::identity::ActorKeypair;
use fauna_mls::engine::MlsEngine;

/// Generous ceiling for "the loop got through a sweep". Sized far above any
/// non-pathological delay so a busy machine never flips the verdict; a green run
/// pays none of it.
const SWEEP_BUDGET: Duration = Duration::from_secs(120);

/// The ordered log the claim is stated over.
#[derive(Debug, PartialEq, Eq, Clone)]
enum Event {
    /// A mail feed was polled — and whether it answered with an error.
    Fetched { failed: bool },
    /// The index seam offered a message, identified so the order is readable.
    Observed(String),
    /// `observe_catch_up_complete` fired: the loop declared **that kind's**
    /// backlog done. Carries the kind, because the two kinds' boundaries close
    /// on different events and a kind-less record could not tell the mail
    /// boundary from the conversation one.
    CatchUpComplete(IndexableKind),
    /// An arm was pushed into the launcher's observer container, named so the
    /// late-attach tests can tell "the mail arm appeared" from "everything was
    /// rebuilt" — the latter shows up as a *second* record for an arm that was
    /// already there.
    Attached(&'static str),
    /// An arm took a message. Arm-tagged (unlike [`Self::Observed`]) because the
    /// late-attach tests assert about *which* arm saw what.
    ArmObserved { arm: &'static str, id: String },
    /// An arm took a catch-up boundary for its own kind.
    ArmCatchUp {
        arm: &'static str,
        kind: IndexableKind,
    },
    /// `corpus_changed` fired: the loop routed a third-ingest-class change
    /// signal to the launcher. Carries the corpus, because the signal names what
    /// moved **on the nest** rather than what a local seam observed.
    CorpusChanged(NestCorpus),
}

#[derive(Default)]
struct Journal {
    events: Mutex<Vec<Event>>,
}

impl Journal {
    fn push(&self, e: Event) {
        self.events.lock().unwrap().push(e);
    }
    fn events(&self) -> Vec<Event> {
        self.events.lock().unwrap().clone()
    }
    fn count(&self, want: &Event) -> usize {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| *e == want)
            .count()
    }
}

struct RecordingObserver {
    journal: Arc<Journal>,
}

impl MessageIndexObserver for RecordingObserver {
    fn observe_indexable_message(&self, msg: IndexableMessage<'_>) {
        self.journal.push(Event::Observed(msg.message_id.0.clone()));
    }
    fn observe_catch_up_complete(&self, kind: IndexableKind) {
        self.journal.push(Event::CatchUpComplete(kind));
    }
}

struct RecordingLauncher {
    journal: Arc<Journal>,
}

#[async_trait]
impl IndexBuilderLauncher for RecordingLauncher {
    async fn launch(&self) -> Option<Arc<dyn MessageIndexObserver>> {
        Some(Arc::new(RecordingObserver {
            journal: Arc::clone(&self.journal),
        }) as Arc<dyn MessageIndexObserver>)
    }

    async fn corpus_changed(&self, corpus: NestCorpus) {
        self.journal.push(Event::CorpusChanged(corpus));
    }
}

/// A mailbox whose first `fetch` can be scripted to fail, so a test can put an
/// *incomplete* walk in front of the boundary. `fail_first` models the ordinary
/// production case the rule exists for: a transport blip mid-backlog. On the
/// retry the same UID space is re-served — which is exactly what production does,
/// since a failed poll never advances the cursor.
struct ScriptedMailbox {
    journal: Arc<Journal>,
    fail_next: AtomicBool,
    message_id: &'static str,
}

impl ScriptedMailbox {
    fn new(journal: Arc<Journal>, message_id: &'static str, fail_first: bool) -> Arc<Self> {
        Arc::new(Self {
            journal,
            fail_next: AtomicBool::new(fail_first),
            message_id,
        })
    }
}

#[async_trait]
impl InboundMailSource for ScriptedMailbox {
    async fn fetch(&self, after_uid: u32, _limit: u32) -> Result<InboundMailPage, String> {
        if self.fail_next.swap(false, Ordering::SeqCst) {
            self.journal.push(Event::Fetched { failed: true });
            return Err("scripted transport failure mid-backlog".to_string());
        }
        self.journal.push(Event::Fetched { failed: false });
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
                message_id: self.message_id.as_bytes().to_vec(),
                internal_date_ms: 1_700_000_000_000,
                rfc5322: format!(
                    "From: sender@example.com\r\n\
                     To: me@example.com\r\n\
                     Subject: quarterly report\r\n\
                     Message-ID: <{}>\r\n\
                     \r\n\
                     the numbers are in\r\n",
                    self.message_id
                )
                .into_bytes(),
                mailbox: fauna_conversations::backend::MailFeed::Inbox,
                suppress_from_view: false,
                has_seen_flag: false,
            }],
            more: false,
        })
    }
}

/// An empty second feed — the loop always drives INBOX *and* Sent, and this test
/// only scripts INBOX.
struct EmptyMailbox;

#[async_trait]
impl InboundMailSource for EmptyMailbox {
    async fn fetch(&self, _after_uid: u32, _limit: u32) -> Result<InboundMailPage, String> {
        Ok(InboundMailPage {
            skipped: Vec::new(),
            highest_modseq: 0,
            records: vec![],
            more: false,
        })
    }
}

/// The push seam, driven by the test rather than a nest: each `next_event` hands
/// out the next scripted wake and then parks forever. That is what makes every
/// sweep in this file **caused** by the test instead of awaited on the loop's
/// 30 s backstop ticker.
struct ScriptedPush {
    events: Mutex<std::collections::VecDeque<ConvPushEvent>>,
    ready: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl ConversationsPush for ScriptedPush {
    async fn next_event(&self) -> Option<ConvPushEvent> {
        loop {
            if let Some(e) = self.events.lock().unwrap().pop_front() {
                return Some(e);
            }
            self.ready.notified().await;
        }
    }
}

impl ScriptedPush {
    fn new() -> (Arc<Self>, Arc<tokio::sync::Notify>) {
        let ready = Arc::new(tokio::sync::Notify::new());
        (
            Arc::new(Self {
                events: Mutex::new(std::collections::VecDeque::new()),
                ready: Arc::clone(&ready),
            }),
            ready,
        )
    }
    fn send(&self, event: ConvPushEvent) {
        self.events.lock().unwrap().push_back(event);
        self.ready.notify_waiters();
    }
}

struct NoSend;

#[async_trait]
impl OutboundMailSink for NoSend {
    async fn submit(&self, _recipients: Vec<String>, _raw: Vec<u8>) -> Result<(), String> {
        Err("this test never sends".into())
    }
}

/// Deadline-poll for a journalled predicate. Positive wait, generous budget, no
/// wall-clock dependency in the verdict.
async fn await_journal(journal: &Journal, mut done: impl FnMut(&[Event]) -> bool, what: &str) {
    let deadline = tokio::time::Instant::now() + SWEEP_BUDGET;
    loop {
        let events = journal.events();
        if done(&events) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}; journal so far: {events:?}"
        );
        // sleep-ok: the poll interval of a deadline wait, not the assertion.
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Build a session wired to the mail rail only, with the test holding the push
/// seam. Returns the pieces each test drives.
fn session_with(
    journal: &Arc<Journal>,
    inbox: Arc<dyn InboundMailSource>,
) -> (Arc<ConversationsSession>, Arc<ScriptedPush>) {
    session_with_launcher(
        Arc::new(RecordingLauncher {
            journal: Arc::clone(journal),
        }),
        inbox,
    )
}

/// The general form: the caller supplies the launcher, so the late-attach tests
/// can script an arm that is unavailable at `launch()` and appears later.
fn session_with_launcher(
    launcher: Arc<dyn IndexBuilderLauncher>,
    inbox: Arc<dyn InboundMailSource>,
) -> (Arc<ConversationsSession>, Arc<ScriptedPush>) {
    let (push, _ready) = ScriptedPush::new();
    let manager = ConversationsManager::new();
    let engine = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let self_actor = engine.identity_actor_id();
    let session = ConversationsSession::from_manager(
        manager,
        engine,
        Arc::new(SilentNest {
            label: "catch-up boundary test",
            permissive: false,
        }),
        "me@example.com".to_string(),
        self_actor,
        Some(Arc::clone(&push) as Arc<dyn ConversationsPush>),
    );
    session.register_smtp(Arc::new(NoSend));
    session.set_index_builder_launcher(launcher);
    session.register_mail_receive(inbox, Arc::new(EmptyMailbox));
    (session, push)
}

/// **The boundary pin.** The signal fires *after* the whole first sweep's mail
/// has reached the seam, and exactly once — so a builder that consults its lease
/// gate before it and ignores the gate after it is gating precisely the launch
/// backlog.
///
/// Stated over the journal's **order**, not its contents: on one seat the set of
/// indexed messages is identical whether the signal is right, early, or missing
/// entirely, which is what makes an end-state assertion here worthless.
///
/// Mutations this reddens under: signalling in the prologue instead of after the
/// sweep (`CatchUpComplete` precedes `Observed`); never signalling (absent);
/// signalling on every sweep (count > 1 after the second wake).
#[tokio::test(flavor = "multi_thread")]
async fn the_boundary_is_signalled_once_after_the_first_sweep_completes() {
    let journal = Arc::new(Journal::default());
    let inbox = ScriptedMailbox::new(Arc::clone(&journal), "first-pass@example.com", false);
    let (session, push) = session_with(&journal, Arc::clone(&inbox) as Arc<dyn InboundMailSource>);

    let loop_session = Arc::clone(&session);
    tokio::spawn(async move { loop_session.start_receive_loop().await });

    await_journal(
        &journal,
        |events| events.contains(&Event::CatchUpComplete(IndexableKind::Mail)),
        "the first sweep to close the catch-up boundary",
    )
    .await;

    let events = journal.events();
    let observed = events
        .iter()
        .position(|e| matches!(e, Event::Observed(_)))
        .expect("the first-pass mail should have reached the index seam");
    let boundary = events
        .iter()
        .position(|e| *e == Event::CatchUpComplete(IndexableKind::Mail))
        .expect("the boundary should have been signalled");
    assert!(
        observed < boundary,
        "the backlog must reach the seam BEFORE the boundary closes — a signal that fires \
         first would let a stood-down seat treat its own launch backlog as trickle and \
         republish the whole corpus. Journal: {events:?}"
    );

    // A second sweep the test causes itself: the boundary is a once-per-session
    // fact, so this must add mail-poll events and no second signal.
    let fetches_before = events
        .iter()
        .filter(|e| matches!(e, Event::Fetched { .. }))
        .count();
    push.send(ConvPushEvent::MailReceived);
    await_journal(
        &journal,
        |events| {
            events
                .iter()
                .filter(|e| matches!(e, Event::Fetched { .. }))
                .count()
                > fetches_before
        },
        "the second sweep to poll the mailbox",
    )
    .await;

    assert_eq!(
        journal.count(&Event::CatchUpComplete(IndexableKind::Mail)),
        1,
        "the boundary is crossed once per session; re-signalling would reopen the gate on \
         work that is trickle. Journal: {:?}",
        journal.events()
    );
}

/// **The load-bearing half.** A sweep that *errored* has not walked the backlog,
/// so it must not close the boundary — the cursor did not advance, and the next
/// sweep re-pages the same mail.
///
/// Close it anyway and a stood-down seat reclassifies its entire remaining
/// backlog as trickle, which the lease never gates: the N× full-corpus republish
/// the ruling exists to prevent, arriving precisely on the unlucky launches.
///
/// This is the test the "signal after the first poll, unconditionally" shortcut
/// fails and the sibling test above passes — which is why both exist.
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_sweep_does_not_close_the_boundary() {
    let journal = Arc::new(Journal::default());
    let inbox = ScriptedMailbox::new(Arc::clone(&journal), "retried@example.com", true);
    let (session, push) = session_with(&journal, Arc::clone(&inbox) as Arc<dyn InboundMailSource>);

    let loop_session = Arc::clone(&session);
    tokio::spawn(async move { loop_session.start_receive_loop().await });

    // Causal barrier: the failing fetch is journalled by the feed itself, so
    // "the first sweep is over" is an observed fact rather than an elapsed wait.
    await_journal(
        &journal,
        |events| events.contains(&Event::Fetched { failed: true }),
        "the first sweep's mail poll to fail",
    )
    .await;
    assert!(
        !journal
            .events()
            .contains(&Event::CatchUpComplete(IndexableKind::Mail)),
        "an unfinished walk must still count as backlog. Journal: {:?}",
        journal.events()
    );

    // The retry — again caused by the test, not by the backstop ticker. The same
    // UID space is re-served, exactly as production re-pages after a failed poll.
    push.send(ConvPushEvent::MailReceived);
    await_journal(
        &journal,
        |events| events.contains(&Event::CatchUpComplete(IndexableKind::Mail)),
        "the retried sweep to close the boundary",
    )
    .await;

    let events = journal.events();
    let boundary = events
        .iter()
        .position(|e| *e == Event::CatchUpComplete(IndexableKind::Mail))
        .expect("the retried sweep should have closed the boundary");
    let retried_mail = events
        .iter()
        .position(|e| matches!(e, Event::Observed(id) if id == "<retried@example.com>"))
        .expect("the retried sweep should have delivered the mail the failed one missed");
    assert!(
        retried_mail < boundary,
        "the mail the failed sweep never reached must be inside the backlog, not after it. \
         Journal: {events:?}"
    );
}

// ── The late arm: a kind whose precondition arrives mid-session ───────────────
//
// `IndexBuilderLauncher::launch` runs exactly once, in the receive loop's
// prologue. Everything below is about the arms it could *not* build then — mail
// enabled after login (no MSEK yet) being the case a live user hits, a transient
// rail failure at login being the same shape arriving by accident. Before
// `ensure_arm` those arms never existed again for the life of the process: the
// mail arrived, rendered in Conversations, and was silently never staged for
// search until a restart (`content-index.md` § Ingest triggers, v1 → *Where the
// builder lives*).

/// The test's stand-in for the production `FanOutObserver`: a **stable**
/// container the launcher mutates in place, registered with the manager exactly
/// once.
///
/// Each arm filters for its own kind, precisely as the real builders do
/// (`IndexBuilder::observe_catch_up_complete`) — without that, this harness
/// could not tell a reopened *mail* window from a disturbed *conversation* one.
struct ArmSet {
    journal: Arc<Journal>,
    arms: Mutex<Vec<(&'static str, IndexableKind)>>,
}

impl ArmSet {
    fn new(journal: Arc<Journal>) -> Arc<Self> {
        Arc::new(Self {
            journal,
            arms: Mutex::new(Vec::new()),
        })
    }

    fn attach(&self, name: &'static str, kind: IndexableKind) {
        self.arms.lock().unwrap().push((name, kind));
        self.journal.push(Event::Attached(name));
    }

    fn has(&self, name: &'static str) -> bool {
        self.arms.lock().unwrap().iter().any(|(n, _)| *n == name)
    }
}

impl MessageIndexObserver for ArmSet {
    fn observe_indexable_message(&self, msg: IndexableMessage<'_>) {
        for (name, kind) in self.arms.lock().unwrap().iter() {
            if *kind == msg.kind {
                self.journal.push(Event::ArmObserved {
                    arm: name,
                    id: msg.message_id.0.clone(),
                });
            }
        }
    }
    fn observe_catch_up_complete(&self, kind: IndexableKind) {
        // The seam-level fact first — *that* the loop closed this kind's window,
        // independently of who was there to hear it. Without it a test could not
        // observe the very precondition the late-attach pins rest on: the mail
        // window closes on sweep 1, while there is still no mail arm to take it.
        self.journal.push(Event::CatchUpComplete(kind));
        for (name, arm_kind) in self.arms.lock().unwrap().iter() {
            if *arm_kind == kind {
                self.journal.push(Event::ArmCatchUp { arm: name, kind });
            }
        }
    }
}

/// A launcher whose **conversation** arm always resumes and whose **mail** arm
/// only can once the test flips `mail_ready` — the mid-session enablement the
/// live gap is about, modelled at the one seam that observes it.
struct LateArmLauncher {
    observer: Arc<ArmSet>,
    mail_ready: Arc<AtomicBool>,
}

#[async_trait]
impl IndexBuilderLauncher for LateArmLauncher {
    async fn launch(&self) -> Option<Arc<dyn MessageIndexObserver>> {
        self.observer
            .attach("conversation", IndexableKind::Conversation);
        if self.mail_ready.load(Ordering::SeqCst) {
            self.observer.attach("mail", IndexableKind::Mail);
        }
        Some(Arc::clone(&self.observer) as Arc<dyn MessageIndexObserver>)
    }

    async fn ensure_arm(&self, kind: IndexableKind) -> bool {
        if kind != IndexableKind::Mail
            || self.observer.has("mail")
            || !self.mail_ready.load(Ordering::SeqCst)
        {
            return false;
        }
        self.observer.attach("mail", IndexableKind::Mail);
        true
    }

    /// No walk to re-run — this fake launcher has no third-ingest-class arm.
    /// Written out rather than inherited: the trait requires the method so a
    /// real launcher cannot silently skip it (see its doc comment).
    async fn corpus_changed(&self, _corpus: NestCorpus) {}
}

/// A mailbox that answers empty until the test enables mail, then serves one
/// record from UID 0.
///
/// That is the production shape rather than a convenience: with no MSEK the
/// fetch is a graceful no-op returning an empty page, so the session cursor
/// never advances and the first *enabled* sweep re-pages from the start.
struct EnableableMailbox {
    journal: Arc<Journal>,
    ready: Arc<AtomicBool>,
}

#[async_trait]
impl InboundMailSource for EnableableMailbox {
    async fn fetch(&self, after_uid: u32, _limit: u32) -> Result<InboundMailPage, String> {
        self.journal.push(Event::Fetched { failed: false });
        if !self.ready.load(Ordering::SeqCst) || after_uid > 0 {
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
                message_id: b"late-arm@example.com".to_vec(),
                internal_date_ms: 1_700_000_000_000,
                rfc5322: b"From: sender@example.com\r\n\
                     To: me@example.com\r\n\
                     Subject: enabled mid session\r\n\
                     Message-ID: <late-arm@example.com>\r\n\
                     \r\n\
                     the numbers are in\r\n"
                    .to_vec(),
                mailbox: fauna_conversations::backend::MailFeed::Inbox,
                suppress_from_view: false,
                has_seen_flag: false,
            }],
            more: false,
        })
    }
}

/// Wire a session whose mail arm is gated on `mail_ready`.
fn late_arm_session(
    journal: &Arc<Journal>,
) -> (
    Arc<ConversationsSession>,
    Arc<ScriptedPush>,
    Arc<AtomicBool>,
    Arc<ArmSet>,
) {
    let mail_ready = Arc::new(AtomicBool::new(false));
    let observer = ArmSet::new(Arc::clone(journal));
    let launcher = Arc::new(LateArmLauncher {
        observer: Arc::clone(&observer),
        mail_ready: Arc::clone(&mail_ready),
    });
    let inbox = Arc::new(EnableableMailbox {
        journal: Arc::clone(journal),
        ready: Arc::clone(&mail_ready),
    });
    let (session, push) = session_with_launcher(launcher, inbox as Arc<dyn InboundMailSource>);
    (session, push, mail_ready, observer)
}

/// **The gap this closes.** Mail enabled *after* login must be staged in that
/// same session — not only after the user restarts the app.
///
/// The live strict XFAIL
/// (`tests/e2e-unified/tests/test_search_local_index.py`) is the end-to-end
/// statement of this; this is its tier_1 twin, and the one that can see *why*
/// (the arm's existence, not just the search result).
///
/// Mutations this reddens under: never calling `ensure_arm`; calling it after
/// the poll instead of before it (the sweep's mail is gone — the cursor advanced
/// past it and nothing re-presents it until the next launch).
#[tokio::test(flavor = "multi_thread")]
async fn mail_enabled_mid_session_attaches_its_arm_and_indexes_that_session_s_mail() {
    let journal = Arc::new(Journal::default());
    let (session, push, mail_ready, observer) = late_arm_session(&journal);

    let loop_session = Arc::clone(&session);
    tokio::spawn(async move { loop_session.start_receive_loop().await });

    // Causal barrier: the loop has swept once with mail still disabled.
    await_journal(
        &journal,
        |events| events.iter().any(|e| matches!(e, Event::Fetched { .. })),
        "the first sweep to poll the mailbox with mail disabled",
    )
    .await;
    assert!(
        !observer.has("mail"),
        "no mail arm can exist before mail is enabled — that is the precondition \
         the gap is about. Journal: {:?}",
        journal.events()
    );

    // The user enables mail, then mail arrives (the push the nest sends).
    mail_ready.store(true, Ordering::SeqCst);
    push.send(ConvPushEvent::MailReceived);

    await_journal(
        &journal,
        |events| {
            events.iter().any(
                |e| matches!(e, Event::ArmObserved { arm, id } if *arm == "mail" && id == "<late-arm@example.com>"),
            )
        },
        "the late-attached mail arm to take this session's mail",
    )
    .await;
}

/// **The fresh window.** A late arm's catch-up boundary must be *reopened*, not
/// inherited.
///
/// With mail disabled a sweep is a graceful no-op that answers "complete", so
/// the loop closes the mail boundary on sweep 1 — before mail exists. An arm
/// attaching later would therefore inherit a closure that says nothing about its
/// own backlog. Inheriting it the wrong way in either direction is a real
/// defect: replay the stale closure and the whole mailbox re-walk is
/// misclassified as trickle, which the lease never gates (the N× republish);
/// leave the window shut and a stood-down seat withholds that arm's backlog for
/// the rest of the session.
///
/// So: the arm sees its boundary *after* its backlog, exactly as a launch-time
/// arm does.
#[tokio::test(flavor = "multi_thread")]
async fn a_late_arm_gets_a_fresh_catch_up_window_not_the_one_that_closed_without_it() {
    let journal = Arc::new(Journal::default());
    let (session, push, mail_ready, _observer) = late_arm_session(&journal);

    let loop_session = Arc::clone(&session);
    tokio::spawn(async move { loop_session.start_receive_loop().await });

    // The loop closes the mail boundary on this sweep, with no mail arm attached
    // to hear it.
    await_journal(
        &journal,
        |events| events.contains(&Event::CatchUpComplete(IndexableKind::Mail)),
        "the first sweep to close the mail boundary with no mail arm attached",
    )
    .await;

    mail_ready.store(true, Ordering::SeqCst);
    push.send(ConvPushEvent::MailReceived);

    await_journal(
        &journal,
        |events| {
            events.iter().any(
                |e| matches!(e, Event::ArmCatchUp { arm, kind } if *arm == "mail" && *kind == IndexableKind::Mail),
            )
        },
        "the reopened mail window to close again, this time at the arm",
    )
    .await;

    let events = journal.events();
    let attached = events
        .iter()
        .position(|e| *e == Event::Attached("mail"))
        .expect("the mail arm should have attached");
    let observed = events
        .iter()
        .position(|e| matches!(e, Event::ArmObserved { arm, .. } if *arm == "mail"))
        .expect("the late arm should have taken this session's mail");
    let boundary = events
        .iter()
        .position(|e| matches!(e, Event::ArmCatchUp { arm, .. } if *arm == "mail"))
        .expect("the late arm should have been told its backlog ended");
    assert!(
        attached < observed && observed < boundary,
        "a late arm must attach, then take its backlog, then be told the backlog \
         ended — the same order a launch-time arm sees. Journal: {events:?}"
    );
}

/// **The arm already running must not be disturbed.** Attaching mail late may
/// not cost the conversation arm anything.
///
/// The tempting fix — re-run `launch()` on the transition — passes every
/// end-state assertion on one seat while silently discarding the conversation
/// builder's live state: its seeded `(kind, content_id)` re-index guard and
/// anything staged but not yet flushed. Neither is visible in a search result,
/// which is why this pin is stated over the arm's **identity and continuity**
/// instead: the container is mutated, so the conversation arm is attached
/// exactly once and keeps taking its own kind across the attach.
///
/// Mutations this reddens under: rebuilding the container on attach; calling
/// `launch()` again; replacing the manager's observer slot.
#[tokio::test(flavor = "multi_thread")]
async fn a_late_mail_attach_leaves_the_conversation_arm_untouched() {
    let journal = Arc::new(Journal::default());
    let (session, push, mail_ready, _observer) = late_arm_session(&journal);

    let loop_session = Arc::clone(&session);
    tokio::spawn(async move { loop_session.start_receive_loop().await });

    await_journal(
        &journal,
        |events| events.contains(&Event::CatchUpComplete(IndexableKind::Mail)),
        "the first sweep to complete",
    )
    .await;

    mail_ready.store(true, Ordering::SeqCst);
    push.send(ConvPushEvent::MailReceived);

    await_journal(
        &journal,
        |events| events.contains(&Event::Attached("mail")),
        "the mail arm to attach",
    )
    .await;

    assert_eq!(
        journal.count(&Event::Attached("conversation")),
        1,
        "the conversation arm must be attached exactly once for the life of the \
         session — a second record means the arm set was rebuilt rather than \
         mutated, which discards the running builder's re-index guard and its \
         unflushed staged docs. Journal: {:?}",
        journal.events()
    );
}

/// **The third ingest class's freshness signal reaches its walk.**
///
/// A card written through the MDA while the app runs raises
/// `fauna.addressbook.changed`; the loop must route it to the index launcher,
/// whose contacts reconcile walk is the only thing that can act on it. Without
/// this arm the change waits for the next master-kind sweep — the arm still
/// converges (the walk is the correctness carrier), but a user who edits a
/// contact and immediately searches for it does not find it.
///
/// Stated over the journal for the same reason the boundary pins are: the end
/// state is identical whether the signal arrived or a later sweep did the work,
/// so only the causal record distinguishes them. The wait is a causal barrier —
/// the test itself sends the wake and waits on the journalled consequence.
///
/// Mutations this reddens under: dropping the `AddressBookChanged` match arm
/// (never recorded); routing it to a rail sweep instead of the launcher (same);
/// giving `corpus_changed` a defaulted no-op body on the trait and forgetting to
/// override it in a launcher (same — which is precisely why the trait requires
/// it rather than defaulting it).
#[tokio::test(flavor = "multi_thread")]
async fn an_addressbook_change_push_reaches_the_launchers_reconcile_walk() {
    let journal = Arc::new(Journal::default());
    let inbox = ScriptedMailbox::new(Arc::clone(&journal), "unrelated@example.com", false);
    let (session, push) = session_with(&journal, Arc::clone(&inbox) as Arc<dyn InboundMailSource>);

    let loop_session = Arc::clone(&session);
    tokio::spawn(async move { loop_session.start_receive_loop().await });

    // Let the session finish its launch sweep first, so the event under test is
    // unambiguously the cause of what follows rather than a race with startup.
    await_journal(
        &journal,
        |events| events.contains(&Event::CatchUpComplete(IndexableKind::Mail)),
        "the launch sweep to settle",
    )
    .await;
    assert_eq!(
        journal.count(&Event::CorpusChanged(NestCorpus::AddressBook)),
        0,
        "nothing may have signalled a corpus change before one was sent — the walk at \
         attach is `launch`'s own business, not this signal's. Journal: {:?}",
        journal.events()
    );

    push.send(ConvPushEvent::AddressBookChanged);

    await_journal(
        &journal,
        |events| events.contains(&Event::CorpusChanged(NestCorpus::AddressBook)),
        "the address-book change to reach the launcher",
    )
    .await;

    // A second wake reconciles again rather than being suppressed: the walk is
    // idempotent and ctag-suppressed on its own, so de-duplicating here would
    // only add a second, weaker copy of a decision the walk already makes — and
    // would drop a real change that landed between the two events.
    push.send(ConvPushEvent::AddressBookChanged);
    await_journal(
        &journal,
        |events| {
            events
                .iter()
                .filter(|e| **e == Event::CorpusChanged(NestCorpus::AddressBook))
                .count()
                == 2
        },
        "the second address-book change to reach the launcher too",
    )
    .await;
}

/// The File arm's freshness half: a `fauna.sync.changed` nudge — a file landing
/// in any of this actor's sets, from any device or any member of a shared set —
/// reaches the launcher's reconcile walk without a relaunch
/// (`content-index.md` § Ingest triggers, v1 → *The files/media arms are
/// SCOPED*: the change signal already exists, so this kind starts one signal
/// ahead of where contacts did).
///
/// Same journal-over-end-state shape and same mutation coverage as its
/// address-book twin above, and the same routing argument: no rail owns the file
/// corpus, so the cross-set `fauna.media.list` drain *is* the reconcile.
///
/// **The distinct-corpus assertion is the load-bearing half.** Both events route
/// through the one `corpus_changed` seam, so wiring the file nudge to
/// `NestCorpus::AddressBook` — a one-token slip — would leave this test passing
/// on the arrival while silently running the wrong walk. Asserting the *file*
/// corpus, and that the address book was never signalled, is what catches it.
#[tokio::test(flavor = "multi_thread")]
async fn a_sync_files_change_push_reaches_the_launchers_reconcile_walk() {
    let journal = Arc::new(Journal::default());
    let inbox = ScriptedMailbox::new(Arc::clone(&journal), "unrelated@example.com", false);
    let (session, push) = session_with(&journal, Arc::clone(&inbox) as Arc<dyn InboundMailSource>);

    let loop_session = Arc::clone(&session);
    tokio::spawn(async move { loop_session.start_receive_loop().await });

    await_journal(
        &journal,
        |events| events.contains(&Event::CatchUpComplete(IndexableKind::Mail)),
        "the launch sweep to settle",
    )
    .await;
    assert_eq!(
        journal.count(&Event::CorpusChanged(NestCorpus::Files)),
        0,
        "nothing may have signalled a file-corpus change before one was sent — the \
         walk at attach is `launch`'s own business. Journal: {:?}",
        journal.events()
    );

    push.send(ConvPushEvent::SyncFilesChanged);

    await_journal(
        &journal,
        |events| events.contains(&Event::CorpusChanged(NestCorpus::Files)),
        "the folder change to reach the launcher",
    )
    .await;
    assert_eq!(
        journal.count(&Event::CorpusChanged(NestCorpus::AddressBook)),
        0,
        "a file nudge must run the FILE walk — routing it to the address book \
         would reconcile the wrong corpus with nothing reporting it. Journal: {:?}",
        journal.events()
    );

    // Not de-duplicated, for the reason the address-book twin is not: the walk is
    // idempotent and guarded on its own, so suppressing a repeat here would only
    // drop a real change that landed between two nudges.
    push.send(ConvPushEvent::SyncFilesChanged);
    await_journal(
        &journal,
        |events| {
            events
                .iter()
                .filter(|e| **e == Event::CorpusChanged(NestCorpus::Files))
                .count()
                == 2
        },
        "the second folder change to reach the launcher too",
    )
    .await;
}
