//! tier_1: the **Conversation kind's launch catch-up** — the two idempotent legs
//! that put restored history into the content index, and the boundary that says
//! where that backlog ends.
//!
//! Proof obligation (`docs/goal/behavior/content-index.md` § Ingest triggers, v1
//! → *The Conversation kind's catch-up*, ruled 2026-08-04):
//!
//! > the thread store is in-memory, rebuilt every launch from two sources — the
//! > `__mls` history-slice restore […] which **bypasses the seam** […] So
//! > restored history — cross-device history, the owner's own sent plaintext —
//! > never reaches a builder today. The ruling is **both** of the obvious
//! > halves […] **(1)** the manager's restore path offers each restored message
//! > to the same seam the ingest chokepoints fire; **(2)** the builder walks the
//! > thread store through the seam when it attaches.
//!
//! …and the boundary: *"only a restore + initial refold that completed **without
//! error** closes the Conversation catch-up boundary"*.
//!
//! **What each leg is worth today, stated once so no reader has to re-derive
//! it.** Leg 2 is the one that does the work: every seat completes the restore
//! *before* the index observer exists (tui/FFI await `mls_sync_launcher.launch()`
//! in `start_receive_loop`'s prologue, two statements above the index launch;
//! linux awaits `wire_mls_state_sync` before calling `start_receive_loop` at
//! all), so leg 1 fires into an empty observer slot and drops. Leg 1 is kept
//! because that is a *scheduling* fact one edit away from changing — and this
//! file pins both, so a future reorder flips which test would catch a regression
//! rather than leaving restored history silently unsearchable.
//!
//! Every claim here is stated over the journal's **order**, never an end state,
//! for the reason the sibling `index_catch_up_boundary_tests.rs` documents: on a
//! single seat the *set* of indexed messages is identical whether the boundary
//! is right, early, or missing, so an end-state assertion is a zero pin. Waits
//! are deadline polls over causal facts (convention 14) — no settle-sleeps, and
//! a green run pays none of the budget.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use fauna_conversations::ConversationsSession;
use fauna_conversations::address::TypedAddress;
use fauna_conversations::backend::{
    ConvPushEvent, ConvRpcError, ConversationsPush, ConversationsRpc, IndexBuilderLauncher,
    NestCorpus, WelcomeChannelKind,
};
use fauna_conversations::index_sink::{IndexableKind, IndexableMessage, MessageIndexObserver};
use fauna_conversations::manager::ConversationsManager;
use fauna_conversations::message::{MessageBadges, MessageId, MessageSnapshot};
use fauna_conversations::store::history::ChannelHistorySlice;
use fauna_conversations::thread::ThreadFlavor;
use fauna_core::identity::ActorKeypair;
use fauna_core::render::RenderDocument;
use fauna_mls::engine::MlsEngine;
use fauna_mls::types::ChannelId;

/// Generous ceiling for "the loop got through a sweep". Sized far above any
/// non-pathological delay so a busy machine never flips the verdict.
const SWEEP_BUDGET: Duration = Duration::from_secs(120);

/// A 32-byte channel id, hex — the shape `ChannelId::from_hex` accepts and the
/// key `restore_channel_slice` threads by.
const CHANNEL_HEX: &str = "aa00000000000000000000000000000000000000000000000000000000000001";

/// The ordered log the claims are stated over.
#[derive(Debug, PartialEq, Eq, Clone)]
enum Event {
    /// The index seam offered a message: which kind, and which id.
    Observed(IndexableKind, String),
    /// A bound channel was fetched — and whether it answered with an error.
    Folded { failed: bool },
    /// `observe_catch_up_complete` fired for this kind.
    CatchUpComplete(IndexableKind),
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
        self.journal
            .push(Event::Observed(msg.kind, msg.message_id.0.clone()));
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

    /// No walk to re-run — this fake launcher has no third-ingest-class arm.
    /// Written out rather than inherited: the trait requires the method so a
    /// real launcher cannot silently skip it (see its doc comment).
    async fn corpus_changed(&self, _corpus: NestCorpus) {}
}

/// A nest whose channel fetch can be scripted to fail once, so a test can put an
/// *incomplete* refold in front of the boundary — the conversation twin of the
/// sibling file's `ScriptedMailbox`. On the retry the same seq space is
/// re-served, exactly as production re-pages after a failed poll (a failed fetch
/// never advances the cursor).
struct ScriptedNest {
    journal: Arc<Journal>,
    fail_next: AtomicBool,
}

fn unused(what: &str) -> ConvRpcError {
    ConvRpcError::Rejected {
        message: format!("conversation catch-up test drives the fold only ({what})"),
    }
}

#[async_trait]
impl ConversationsRpc for ScriptedNest {
    async fn channel_fetch(
        &self,
        _c: String,
        _a: i64,
        _l: i64,
        _h: Option<String>,
    ) -> Result<Vec<fauna_conversations::backend::FetchedRecord>, ConvRpcError> {
        if self.fail_next.swap(false, Ordering::SeqCst) {
            self.journal.push(Event::Folded { failed: true });
            return Err(ConvRpcError::Rejected {
                message: "scripted transport failure mid-refold".to_string(),
            });
        }
        self.journal.push(Event::Folded { failed: false });
        Ok(vec![])
    }
    async fn channel_send(
        &self,
        _c: String,
        _e: Vec<u8>,
        _expect_no_commit_since: Option<i64>,
        _attachment_refs: Vec<String>,
    ) -> Result<i64, ConvRpcError> {
        Err(unused("channel_send"))
    }
    async fn channel_send_remote(
        &self,
        _c: String,
        _u: String,
        _e: Vec<u8>,
        _expect_no_commit_since: Option<i64>,
        _attachment_refs: Vec<String>,
    ) -> Result<i64, ConvRpcError> {
        Err(unused("channel_send_remote"))
    }
    async fn keypackage_count(&self, _a: String) -> Result<u64, ConvRpcError> {
        Err(unused("keypackage_count"))
    }
    async fn actor_by_handle(
        &self,
        _h: String,
    ) -> Result<Option<fauna_conversations::backend::ResolvedHandle>, ConvRpcError> {
        Err(unused("actor_by_handle"))
    }
    async fn actor_by_handle_remote(
        &self,
        _d: String,
        _l: String,
    ) -> Result<Option<fauna_conversations::backend::ResolvedHandle>, ConvRpcError> {
        Err(unused("actor_by_handle_remote"))
    }
    async fn keypackage_fetch(
        &self,
        _a: String,
        _p: Option<String>,
    ) -> Result<Option<Vec<u8>>, ConvRpcError> {
        Err(unused("keypackage_fetch"))
    }
    async fn keypackage_upload(&self, _p: Vec<Vec<u8>>, _l: bool) -> Result<u64, ConvRpcError> {
        Err(unused("keypackage_upload"))
    }
    async fn welcome_deliver(
        &self,
        _r: String,
        _c: String,
        _w: Vec<u8>,
        _k: WelcomeChannelKind,
        _p: Option<String>,
    ) -> Result<(), ConvRpcError> {
        Err(unused("welcome_deliver"))
    }
    async fn blob_put(
        &self,
        _c: String,
        _h: Option<String>,
        _s: String,
        _b: Vec<u8>,
    ) -> Result<(), ConvRpcError> {
        Err(unused("blob_put"))
    }
    async fn blob_get(
        &self,
        _c: String,
        _h: Option<String>,
        _s: String,
    ) -> Result<Option<Vec<u8>>, ConvRpcError> {
        Err(unused("blob_get"))
    }
}

/// The push seam, driven by the test rather than a nest: each `next_event` hands
/// out the next scripted wake and then parks forever, so every sweep here is
/// **caused** by the test instead of awaited on the loop's 30 s backstop ticker.
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
    fn new() -> Arc<Self> {
        Arc::new(Self {
            events: Mutex::new(std::collections::VecDeque::new()),
            ready: Arc::new(tokio::sync::Notify::new()),
        })
    }
    fn send(&self, event: ConvPushEvent) {
        self.events.lock().unwrap().push_back(event);
        self.ready.notify_waiters();
    }
}

/// Two restored messages in one channel slice — the cross-device history and own
/// sent plaintext the ruling names, in the shape the replica restore hands over.
fn restored_slice() -> ChannelHistorySlice {
    let addr = |n: &str| TypedAddress::Email {
        email_address: format!("{n}@example.com"),
    };
    let msg = |seq: i64, body: &str, own: bool| MessageSnapshot {
        message_id: MessageId(format!("conv:{CHANNEL_HEX}:{seq}")),
        sender: if own { addr("me") } else { addr("peer") },
        sender_display: String::new(),
        body: body.into(),
        document: RenderDocument::default(),
        timestamp_ms: seq * 10,
        subject_line: None,
        badges: MessageBadges::default(),
        reply_to: None,
        reactions: vec![],
        deleted: false,
        is_own: own,
        legal_takedown_ref: None,
        labels: vec![],
        plane_ref: None,
        can_delete: false,
    };
    ChannelHistorySlice {
        channel_id_hex: CHANNEL_HEX.to_string(),
        label: "peer".to_string(),
        flavor: ThreadFlavor::OneToOne,
        participants: vec![addr("me"), addr("peer")],
        // One inbound, one the owner sent — the second is the case the
        // nest-ordered log can never reconstruct on a second device, so it
        // reaches the index through the restore or not at all.
        messages: vec![
            msg(1, "the numbers are in", false),
            msg(2, "thanks, filing it now", true),
        ],
        watermark: 2,
        ..Default::default()
    }
}

fn restored_ids() -> Vec<String> {
    restored_slice()
        .messages
        .iter()
        .map(|m| m.message_id.0.clone())
        .collect()
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

/// Assemble a session whose thread store has already been filled by the replica
/// restore — production's ordering exactly (`restore_and_wire` runs, and only
/// then does the index observer come into existence inside `start_receive_loop`).
///
/// `bind` binds the restored channel so the refold actually polls it; the tests
/// that only care about the walk leave it unbound, which makes `poll_bound`
/// trivially clean and keeps them independent of the fold.
fn session_with(
    journal: &Arc<Journal>,
    fail_first_fold: bool,
    bind: bool,
) -> (Arc<ConversationsSession>, Arc<ScriptedPush>) {
    let push = ScriptedPush::new();
    let manager = ConversationsManager::new();
    let engine = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let self_actor = engine.identity_actor_id();
    let session = ConversationsSession::from_manager(
        manager,
        engine,
        Arc::new(ScriptedNest {
            journal: Arc::clone(journal),
            fail_next: AtomicBool::new(fail_first_fold),
        }),
        "me@example.com".to_string(),
        self_actor,
        Some(Arc::clone(&push) as Arc<dyn ConversationsPush>),
    );

    // The restore, in its production position: **before** any index observer
    // exists. Nothing is registered on the seam yet, which is exactly the gap
    // leg 2 closes.
    let thread_id = session.manager().restore_channel_slice(&restored_slice());
    if bind {
        session
            .backend()
            .bind_channel(thread_id, ChannelId::from_hex(CHANNEL_HEX).unwrap());
    }

    session.set_index_builder_launcher(Arc::new(RecordingLauncher {
        journal: Arc::clone(journal),
    }));
    (session, push)
}

/// **Leg 2, the one that carries the corpus today.** History restored before the
/// builder attached must reach the seam, and must reach it *inside* the catch-up
/// window — i.e. before the Conversation boundary closes.
///
/// Both halves matter and fail differently. Without the walk, restored history —
/// cross-device history and the owner's own sent plaintext — is simply never
/// indexed, with no error anywhere: the user searches for a message they can see
/// on screen and finds nothing. With the walk but a boundary that closed first,
/// the corpus is right on this seat but a stood-down seat reclassifies its whole
/// backlog as trickle, which the lease never gates — the N× full-corpus
/// republish the boundary exists to prevent.
///
/// Mutations this reddens under: deleting the
/// `walk_conversations_for_index()` call from `start_receive_loop` (restored ids
/// absent); moving it after the poll-task spawn (order flips); firing the
/// Conversation boundary in the prologue (boundary precedes the walk).
#[tokio::test(flavor = "multi_thread")]
async fn restored_history_reaches_the_seam_before_the_conversation_boundary_closes() {
    let journal = Arc::new(Journal::default());
    let (session, _push) = session_with(&journal, false, false);

    let loop_session = Arc::clone(&session);
    tokio::spawn(async move { loop_session.start_receive_loop().await });

    await_journal(
        &journal,
        |events| events.contains(&Event::CatchUpComplete(IndexableKind::Conversation)),
        "the first clean refold to close the conversation boundary",
    )
    .await;

    let events = journal.events();
    let boundary = events
        .iter()
        .position(|e| *e == Event::CatchUpComplete(IndexableKind::Conversation))
        .expect("the conversation boundary should have been signalled");

    for id in restored_ids() {
        let at = events
            .iter()
            .position(|e| *e == Event::Observed(IndexableKind::Conversation, id.clone()))
            .unwrap_or_else(|| {
                panic!(
                    "restored message {id} never reached the index seam — the attach-time walk \
                     is what indexes history the restore put in the store without an observer. \
                     Journal: {events:?}"
                )
            });
        assert!(
            at < boundary,
            "restored message {id} reached the seam AFTER the boundary closed, so a stood-down \
             seat would treat its own launch backlog as trickle. Journal: {events:?}"
        );
    }
}

/// The walk offers each restored message **once**, and tags it `Conversation`.
///
/// The kind is not cosmetic: a doc's kind decides which class key seals it, so a
/// mis-kinded offer asks the master builder to seal mail under the mail-calendar
/// class (or the reverse) — `content-index.md` § Don't do these. And the count
/// pins that the walk is not also being driven from some second site: duplicate
/// offers are harmless to the corpus (the builder's stage-time
/// `(kind, content_id)` guard drops them) but a doubling here would mean the
/// walk runs on every sweep rather than once at attach, which is real work on
/// every tick for the whole session.
#[tokio::test(flavor = "multi_thread")]
async fn the_walk_offers_each_restored_message_once_as_a_conversation() {
    let journal = Arc::new(Journal::default());
    // Bound, because this test's "the loop went round again" barrier is a
    // journalled fold — an unbound channel makes `poll_bound` iterate nothing
    // and there would be no causal event to wait on.
    let (session, push) = session_with(&journal, false, true);

    let loop_session = Arc::clone(&session);
    tokio::spawn(async move { loop_session.start_receive_loop().await });

    await_journal(
        &journal,
        |events| events.contains(&Event::CatchUpComplete(IndexableKind::Conversation)),
        "the conversation boundary",
    )
    .await;

    // A second sweep the test causes itself, so "once" is asserted against a
    // loop that has demonstrably gone round again rather than against a loop
    // that simply had no time to repeat the work.
    let folds_before = journal.count(&Event::Folded { failed: false });
    push.send(ConvPushEvent::ChannelMessage);
    await_journal(
        &journal,
        |events| {
            events
                .iter()
                .filter(|e| **e == Event::Folded { failed: false })
                .count()
                > folds_before
        },
        "a second refold",
    )
    .await;

    for id in restored_ids() {
        assert_eq!(
            journal.count(&Event::Observed(IndexableKind::Conversation, id.clone())),
            1,
            "the attach-time walk runs once per session, not once per sweep. Journal: {:?}",
            journal.events()
        );
    }
    assert!(
        !journal
            .events()
            .iter()
            .any(|e| matches!(e, Event::Observed(IndexableKind::Mail, _))),
        "a fauna-native thread must never be offered as Mail — the kind chooses the class key \
         that seals the doc. Journal: {:?}",
        journal.events()
    );
}

/// **The load-bearing half of the boundary.** A refold that *errored* has not
/// walked the backlog, so it must not close the Conversation boundary — the
/// channel's cursor did not advance, and the next sweep re-pages the same
/// entries.
///
/// This also pins the kind split end to end: the mail rail is silent in this
/// test and its sweep is therefore clean, so a kind-less boundary signal (what
/// this crate shipped before the two-arm builder existed) would close the
/// *conversation* window off the *mail* sweep and this test would go red. That
/// is the regression the `IndexableKind` parameter exists to make impossible.
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_refold_does_not_close_the_conversation_boundary() {
    let journal = Arc::new(Journal::default());
    let (session, push) = session_with(&journal, true, true);

    let loop_session = Arc::clone(&session);
    tokio::spawn(async move { loop_session.start_receive_loop().await });

    // Causal barrier: the failing fetch is journalled by the nest itself, so
    // "the first refold is over" is an observed fact, not an elapsed wait.
    await_journal(
        &journal,
        |events| events.contains(&Event::Folded { failed: true }),
        "the first refold to fail",
    )
    .await;
    assert!(
        !journal
            .events()
            .contains(&Event::CatchUpComplete(IndexableKind::Conversation)),
        "an unfinished refold must still count as backlog. Journal: {:?}",
        journal.events()
    );

    // The retry, again caused by the test rather than the backstop ticker.
    push.send(ConvPushEvent::ChannelMessage);
    await_journal(
        &journal,
        |events| events.contains(&Event::CatchUpComplete(IndexableKind::Conversation)),
        "the retried refold to close the boundary",
    )
    .await;
    assert_eq!(
        journal.count(&Event::CatchUpComplete(IndexableKind::Conversation)),
        1,
        "the boundary is crossed once per session; re-signalling would reopen the gate on work \
         that is trickle. Journal: {:?}",
        journal.events()
    );
}

/// **Leg 1** — the restore path itself offers to the seam.
///
/// Asserted directly on the manager rather than through the session, because on
/// every seat that exists today the restore runs before any observer is
/// registered, so the production path exercises this leg *zero* times (see the
/// module header). That is precisely why it needs its own pin: a leg with no
/// live caller is exactly the code a later refactor deletes as dead, and the day
/// the restore moves after attach — one `tokio::spawn` away, and tempting, since
/// the restore currently blocks the whole receive loop on an indefinite backoff
/// — it is the only thing standing between a user and silently unsearchable
/// history.
#[test]
fn the_restore_path_offers_its_messages_to_a_registered_seam() {
    let journal = Arc::new(Journal::default());
    let manager = ConversationsManager::new();
    manager.set_index_observer(Arc::new(RecordingObserver {
        journal: Arc::clone(&journal),
    }));

    manager.restore_channel_slice(&restored_slice());

    let events = journal.events();
    for id in restored_ids() {
        assert!(
            events.contains(&Event::Observed(IndexableKind::Conversation, id.clone())),
            "restored message {id} must be offered to the seam by the restore path itself. \
             Journal: {events:?}"
        );
    }
}

/// The pair is **idempotent**: with both legs live, a message offered by the
/// restore and again by the walk is offered twice and that is harmless — the
/// ruling's "whichever leg runs second completes the corpus, and overlap is
/// harmless under the stage-time `(kind, content_id)` guard".
///
/// Pinned because the tempting "fix" for the double offer is to make one leg
/// conditional on the other, which reintroduces exactly the ordering dependence
/// the two-leg design exists to remove.
#[test]
fn a_message_offered_by_both_legs_is_simply_offered_twice() {
    let journal = Arc::new(Journal::default());
    let manager = ConversationsManager::new();
    manager.set_index_observer(Arc::new(RecordingObserver {
        journal: Arc::clone(&journal),
    }));

    manager.restore_channel_slice(&restored_slice());
    manager.walk_conversations_for_index();

    for id in restored_ids() {
        assert_eq!(
            journal.count(&Event::Observed(IndexableKind::Conversation, id.clone())),
            2,
            "both legs offer, and neither suppresses the other — dedup is the builder's \
             stage-time guard, not an ordering rule here. Journal: {:?}",
            journal.events()
        );
    }
}

/// A walk with no observer registered is a no-op that cannot panic — the normal
/// state on a client with no local builder (web, by design; any seat whose
/// launcher answered `None` because the rail was unreachable at login).
#[test]
fn the_walk_is_a_no_op_when_no_builder_is_registered() {
    let manager = ConversationsManager::new();
    manager.restore_channel_slice(&restored_slice());
    manager.walk_conversations_for_index();
}
