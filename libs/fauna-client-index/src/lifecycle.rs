//! The builder's **production lifecycle**: what turns a resumed
//! [`IndexBuilder`] into a thing that actually publishes.
//!
//! A builder on its own only *stages* — that split is deliberate, because the
//! receive-hook seam's contract is that it must not block
//! (`fauna_conversations::index_sink`), so all the CPU and I/O live in
//! [`IndexBuilder::flush`] and something has to call it. This module is that
//! something: the observer app glue registers, plus the task that drives flush.
//!
//! **Debounce, not flush-per-message** (`docs/goal/behavior/content-index.md`
//! § Ingest triggers, v1 — *drives `flush` from the debounce*). One flush per
//! arriving message would seal and publish a one-doc segment per mail, which is
//! precisely the unbounded segment chain the ruled compactor exists to clean up.
//! Coalescing a burst into one segment is what makes the common case — a launch
//! re-walking a whole mailbox — publish a handful of segments instead of
//! thousands. The shape is `segment_backup.rs`'s: a `Notify` pulsed by staging,
//! a quiescence window that a fresh pulse extends, and a periodic backstop for
//! the trickle case, all cancellable so logout never waits out a window.
//!
//! **Nothing here is load-bearing for correctness.** A flush that never happens
//! loses no user data: the docs stay pending, and the next launch re-walks the
//! same mailbox and re-stages them (the client keeps no restart-durable mail
//! cursor). That is why every failure below logs and continues rather than
//! propagating — the index converges across launches by construction.

use std::sync::Arc;
use std::time::Duration;

use fauna_conversations::index_sink::{IndexableKind, IndexableMessage, MessageIndexObserver};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::index_builder::IndexBuilder;

/// Quiescence window: staging must go quiet this long before a flush seals.
///
/// A mailbox re-walk stages pages back to back, so each page's pulse extends the
/// window and the whole walk lands in as few segments as it fits in — the
/// behaviour the segment ceiling then bounds. Matched to
/// `segment_backup.rs`'s `PUSH_DEBOUNCE` (the other "a push burst became one
/// unit of work" driver) rather than picked fresh.
const STAGE_DEBOUNCE: Duration = Duration::from_secs(5);

/// Backstop cadence, for content that arrives one message at a time with long
/// gaps: the debounce alone would still flush it, but this bounds how long a
/// single staged doc can sit unpublished if pulses are lost.
const PERIODIC_INTERVAL: Duration = Duration::from_secs(60);

/// The observer app glue registers: stages into the builder, then pulses the
/// debounce.
///
/// Wraps rather than extends [`IndexBuilder`] so the builder keeps its own
/// trigger-free contract (it is driven identically by the flow tests, the tier_3
/// conformance harness, and this debounce) and so the S4 master-key builder can
/// reuse the same driver without inheriting a mail-shaped observer.
struct DebouncedObserver {
    builder: Arc<IndexBuilder>,
    notify: Arc<Notify>,
}

impl MessageIndexObserver for DebouncedObserver {
    fn observe_indexable_message(&self, msg: IndexableMessage<'_>) {
        self.builder.observe_indexable_message(msg);
        // Pulse unconditionally, including for a message the builder's re-index
        // guard just dropped: whether the doc was new is the builder's business,
        // and a spurious pulse costs one quiet flush that finds nothing staged
        // and writes nothing (`plan_flush` returns `None`). Reaching into the
        // builder to find out would trade that for a lock on the receive path.
        self.notify.notify_one();
    }

    /// Forwarded verbatim — the wrapper adds triggering, never policy, and the
    /// catch-up boundary is the builder's own state (it decides what a closed
    /// lease gate withholds). Dropping it here would silently leave every
    /// production builder — the debounce is the *only* observer app glue ever
    /// registers — permanently inside its catch-up window, so a stood-down seat
    /// would gate its whole session's trickle.
    fn observe_catch_up_complete(&self, kind: IndexableKind) {
        self.builder.observe_catch_up_complete(kind);
    }

    /// Forwarded verbatim, the boundary's twin: a wrapper that dropped it would
    /// stage a community room's post-key-in backlog as trickle on a stood-down
    /// seat — the whole room's history republished once per device.
    fn observe_catch_up_reopened(&self, kind: IndexableKind) {
        self.builder.observe_catch_up_reopened(kind);
    }

    /// Forwarded **and pulsed**, for the same reason the boundary is forwarded:
    /// this wrapper is the only observer app glue ever registers, so the seam's
    /// no-op default — which exists for observers that do not index drafts —
    /// would otherwise swallow every corpus and leave the drafts arm staging
    /// nothing on every seat, with no error anywhere and the builder's own unit
    /// tests still green.
    ///
    /// The pulse is not optional here the way it is for a dropped message. A
    /// snapshot corpus that only the 60-second backstop ever flushes means the
    /// user searches for text they just typed and does not find it; and an
    /// *empty* corpus — the discard of the last draft — carries no doc at all,
    /// so the backstop is the only other thing that would ever retire the
    /// segment holding the text they deleted.
    fn observe_draft_corpus(&self, drafts: &[fauna_conversations::index_sink::IndexableDraft]) {
        self.builder.observe_draft_corpus(drafts);
        self.notify.notify_one();
    }
}

/// The door for a producer that has **no observer seam to arrive through** — the
/// contacts reconcile walk, and any future third-ingest-class kind
/// (`content-index.md` § Ingest triggers, v1 → the class template). It holds the
/// same `(builder, notify)` pair [`DebouncedObserver`] does, and exposes staging
/// **only** in forms that pulse.
///
/// **Why a type rather than a convention.** What this replaced was a bare
/// `Arc<IndexBuilder>` handed to the launcher, and a walk staging through it
/// pulsed nothing: the corpus sat until the 60-second backstop, with no error
/// anywhere and every unit test green. That is the drafts arm's dark-wrapper
/// failure one level up — there a *wrapper* dropped a seam method, here a
/// *producer* bypassed the wrapper entirely — and the lesson the drafts fix
/// recorded (the wrapper is not total, so a third producer will land the same
/// way) is answered here by making the pulse unreachable-to-omit rather than by
/// remembering to call it. `observe_draft_corpus` states the cost in full: a
/// snapshot corpus only the backstop flushes means the user searches for text
/// they can see and does not find it, and an **empty** corpus carries no doc at
/// all, so the backstop is the only other thing that would ever retire the
/// segments holding what they deleted.
pub struct DirectStager {
    builder: Arc<IndexBuilder>,
    notify: Arc<Notify>,
}

impl DirectStager {
    /// This kind's last-staged corpus marker — the walk's change precheck. A
    /// pure read: it stages nothing, so it pulses nothing.
    #[must_use]
    pub fn corpus_marker(&self, kind: crate::ContentKind) -> Option<Vec<u8>> {
        self.builder.corpus_marker(kind)
    }

    /// Stage a whole address-book corpus as the `Contact` kind's snapshot, stamp
    /// the corpus marker it was read at, and pulse the flush driver — one call,
    /// because the three are not independently correct.
    ///
    /// Staging without the marker republishes an identical corpus on every
    /// launch; the marker without the stage suppresses the corpus **forever**;
    /// and either without the pulse waits out the backstop. The `Result` is the
    /// marker write's (`IndexBuilder::note_corpus_marker`) — the corpus is
    /// staged and pulsed either way, so a failed stamp costs one redundant
    /// re-read on the next attach and never a missed change.
    ///
    /// **A withheld stage notes nothing.** A stood-down seat's walk reaches
    /// here with the lease closed, and `stage_contact_corpus` drops the corpus;
    /// noting the marker anyway would record a corpus this seat never staged —
    /// in memory at once, and **at rest** the moment any other kind's doc
    /// flushes the manifest — suppressing every later walk until the next real
    /// ctag change. The marker means "the corpus I already staged", so it moves
    /// only when a stage actually lands.
    pub fn stage_contact_corpus(
        &self,
        contacts: &[crate::IndexableContact],
        marker: Vec<u8>,
    ) -> Result<(), crate::IndexBuildError> {
        if !self.builder.stage_contact_corpus(contacts) {
            return Ok(());
        }
        let noted = self
            .builder
            .note_corpus_marker(crate::ContentKind::Contact, marker);
        self.notify.notify_one();
        noted
    }

    /// Stage a posts reconcile walk's unseen rows, stamp the corpus marker the
    /// enumeration was read at, and pulse the flush driver — the append-shaped
    /// sibling of [`Self::stage_contact_corpus`], with the identical
    /// three-in-one-call and withheld-notes-nothing contracts
    /// (`content-index.md` § Ingest triggers, v1 → the class template, piece 2).
    ///
    /// `posts` may be empty (an unchanged tail after the marker's early-stop):
    /// the marker still advances, because the pass legitimately observed the
    /// corpus and the marker is what spares the next sweep the re-page.
    pub fn stage_posts_walk(
        &self,
        posts: &[crate::IndexablePost],
        marker: Vec<u8>,
    ) -> Result<(), crate::IndexBuildError> {
        if !self.builder.stage_posts_walk(posts) {
            return Ok(());
        }
        let noted = self
            .builder
            .note_corpus_marker(crate::ContentKind::Post, marker);
        self.notify.notify_one();
        noted
    }

    /// Stage a files reconcile walk's rows and pulse the flush driver — the File
    /// arm's staging door (`content-index.md` § Ingest triggers, v1 → *The
    /// files/media arms are SCOPED*).
    ///
    /// **No marker, and no `Result`**, which is what makes this the shortest of
    /// the three walk doors: File keeps no corpus marker in v1 (the stage-time
    /// guard is the ruled suppression for an append kind), so there is no stamp
    /// to fail and no withheld-notes-nothing hazard to guard against. What
    /// survives from its siblings is the part that is not optional — staging and
    /// pulsing in **one call**, so a walk can no more publish-at-the-backstop
    /// than it can forget the marker it does not have.
    ///
    /// A failed drain may stage its prefix harmlessly: appends are guarded per
    /// doc and there is no marker to mis-advance, so the next walk simply
    /// re-reads and stages the rest.
    pub fn stage_files_walk(&self, files: &[crate::IndexableFile]) {
        self.builder.stage_files_walk(files);
        self.notify.notify_one();
    }

    /// Stage the user's own just-created post and pulse — the trickle door,
    /// lease-free by construction (`IndexBuilder::stage_post_trickle`).
    ///
    /// No marker: the marker is the **walk's** completion record, and a trickle
    /// that advanced it would declare corpus territory covered that the walk
    /// never paged. The guard makes the walk's later re-encounter of this id a
    /// no-op, so the only cost of leaving the marker alone is one short
    /// already-guarded page walk on the next sweep.
    pub fn stage_own_post(&self, post: &crate::IndexablePost) {
        self.builder.stage_post_trickle(post);
        self.notify.notify_one();
    }
}

/// Wire a resumed builder into a running lifecycle and hand back the observer to
/// register, plus the [`DirectStager`] for producers with no seam.
///
/// Spawns the flush driver on the current runtime and returns both handles;
/// `cancel` ends the driver at logout. Either returned handle holds the builder
/// alive, so the caller may drop everything else.
pub fn spawn_flush_debounce(
    builder: Arc<IndexBuilder>,
    cancel: CancellationToken,
) -> (Arc<dyn MessageIndexObserver>, DirectStager) {
    let notify = Arc::new(Notify::new());
    let driver_builder = Arc::clone(&builder);
    let driver_notify = Arc::clone(&notify);
    tokio::spawn(async move {
        run_flush_driver(driver_builder, driver_notify, cancel).await;
    });
    let stager = DirectStager {
        builder: Arc::clone(&builder),
        notify: Arc::clone(&notify),
    };
    (Arc::new(DebouncedObserver { builder, notify }), stager)
}

/// The driver loop: wait for a pulse (or the backstop tick), let the burst
/// settle, flush.
async fn run_flush_driver(
    builder: Arc<IndexBuilder>,
    notify: Arc<Notify>,
    cancel: CancellationToken,
) {
    let mut interval = tokio::time::interval(PERIODIC_INTERVAL);
    // The first tick completes immediately; take it here so the backstop does
    // not fire a pointless flush the instant the session starts.
    interval.tick().await;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = notify.notified() => fauna_core::debounce::absorb_burst(&notify, &cancel, STAGE_DEBOUNCE).await,
            _ = interval.tick() => {}
        }
        if cancel.is_cancelled() {
            break;
        }
        flush_once(&builder).await;
    }

    // Logout flush: a burst that was still inside its debounce window when the
    // session ended would otherwise wait for the next launch. Best-effort and
    // unwaited-for by anything — losing it costs a re-walk, never data.
    flush_once(&builder).await;
}

/// One flush attempt, with the failure posture this whole module is built on.
async fn flush_once(builder: &IndexBuilder) {
    match builder.flush().await {
        // One entry per kind this flush touched — a builder spans every kind of
        // its class, so a single flush can publish several segments under one
        // manifest write.
        Ok(segments) => {
            for segment in segments {
                tracing::debug!(
                    kind = ?segment.kind,
                    path = %segment.path,
                    docs = segment.doc_count,
                    bytes = segment.byte_len,
                    "index: published a segment"
                );
            }
        }
        // Retryable by construction: the docs are re-derivable from content the
        // nest still holds and the advisory cursor never advanced, so the next
        // pulse (or the next launch) tries again.
        Err(e) => tracing::warn!(error = %e, "index: flush failed, retrying on the next trigger"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index_builder::{IndexBuildError, SegmentRail};
    use fauna_conversations::index_sink::IndexableKind;
    use fauna_conversations::message::MessageId;
    use fauna_conversations::thread::ThreadId;
    use std::sync::Mutex;

    const MSEK: [u8; 32] = [7u8; 32];

    #[derive(Default)]
    struct Recorder {
        published: Mutex<Vec<String>>,
        notify: Notify,
    }

    #[async_trait::async_trait]
    impl SegmentRail for Recorder {
        /// Empty: these tests drive the debounce, never a fold. An empty
        /// listing makes that structural — with no candidates the planner
        /// cannot choose a fold, so a debounce assertion can never be perturbed
        /// by compaction. (Fold behaviour is covered in `index_builder`'s tests,
        /// against a rail that really serves back what it stored.)
        async fn list_entries(
            &self,
        ) -> Result<Vec<crate::index_builder::RailEntry>, IndexBuildError> {
            Ok(Vec::new())
        }

        async fn fetch_blob(&self, blob_hash: &str) -> Result<Vec<u8>, IndexBuildError> {
            Err(IndexBuildError::Publish {
                path: blob_hash.to_string(),
                reason: "the debounce recorder keeps no bytes".into(),
            })
        }

        async fn publish(&self, path: &str, _bytes: &[u8]) -> Result<(), IndexBuildError> {
            self.published.lock().unwrap().push(path.to_string());
            self.notify.notify_waiters();
            Ok(())
        }
    }

    fn observe(observer: &dyn MessageIndexObserver, id: &str) {
        let thread = ThreadId("t-1".into());
        let message = MessageId(id.to_string());
        observer.observe_indexable_message(IndexableMessage {
            kind: IndexableKind::Mail,
            thread_id: &thread,
            message_id: &message,
            subject: Some("quarterly report"),
            body: "the numbers are in",
            sender_actor_id: None,
            nest_message_id: None,
            timestamp_ms: 1_700_000_000_000,
            is_own: false,
        });
    }

    /// **The lifecycle contract.** A doc observed through the registered
    /// observer reaches the publisher with no caller ever touching `flush` —
    /// that is the whole difference between the pieces already built and a
    /// builder that actually runs.
    ///
    /// Latency-independent per convention 14: the assert waits on the
    /// publisher's own signal under a named generous budget, so it neither
    /// sleeps for the debounce nor fails when a loaded box stretches it.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn an_observed_message_reaches_the_publisher_without_an_explicit_flush() {
        let recorder = Arc::new(Recorder::default());
        let builder = Arc::new(IndexBuilder::mail(&MSEK, recorder.clone()));
        let cancel = CancellationToken::new();
        let (observer, _stager) = spawn_flush_debounce(builder, cancel.clone());

        observe(observer.as_ref(), "<a@example.com>");

        // Generous budget, not a sleep: with the clock paused, tokio
        // auto-advances to the next timer, so this resolves as soon as the
        // debounce is the only thing left to wait on.
        tokio::time::timeout(Duration::from_secs(600), recorder.notify.notified())
            .await
            .expect("the debounce should have flushed the staged doc");

        let paths = recorder.published.lock().unwrap().clone();
        assert!(
            paths.iter().any(|p| p.contains("seg-")),
            "a segment should have been published, got {paths:?}"
        );
        cancel.cancel();
    }

    /// Cancellation ends the driver rather than leaving a task pulsing against a
    /// dead session — and the logout flush is what keeps a burst caught inside
    /// its debounce window from waiting for the next launch.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn cancelling_flushes_what_was_still_in_the_debounce_window() {
        let recorder = Arc::new(Recorder::default());
        let builder = Arc::new(IndexBuilder::mail(&MSEK, recorder.clone()));
        let cancel = CancellationToken::new();
        let (observer, _stager) = spawn_flush_debounce(builder, cancel.clone());

        observe(observer.as_ref(), "<b@example.com>");
        // Cancel while the doc is still staged, before the window elapses.
        cancel.cancel();

        tokio::time::timeout(Duration::from_secs(600), recorder.notify.notified())
            .await
            .expect("the logout flush should have published the staged doc");
    }

    /// **The catch-up boundary must survive this wrapper.** `spawn_flush_debounce`
    /// returns the *only* observer app glue ever registers, so a wrapper that
    /// swallowed `observe_catch_up_complete` would leave every production builder
    /// permanently inside its catch-up window — and a stood-down seat would then
    /// withhold its whole session's trickle, silently losing the searchability of
    /// mail the user can see (`content-index.md` § Where the index is built).
    ///
    /// Asserted through the observer the caller actually holds, not the builder,
    /// because the forwarding is the thing under test: reaching past the wrapper
    /// would pass with the wrapper's method deleted.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn the_catch_up_boundary_reaches_the_builder_through_the_debounce() {
        let recorder = Arc::new(Recorder::default());
        let builder = Arc::new(IndexBuilder::mail(&MSEK, recorder));
        let cancel = CancellationToken::new();
        let (observer, _stager) = spawn_flush_debounce(Arc::clone(&builder), cancel.clone());

        assert!(
            builder.is_catching_up(fauna_index::ContentKind::Mail),
            "a fresh builder starts in catch-up"
        );
        // Another kind's boundary says nothing about this builder's backlog, and
        // both kinds' signals reach every arm (the launcher fans out to all).
        // Acting on this one would end the mail catch-up window while the mail
        // sweep is still walking — the early-close the boundary exists to
        // prevent, arriving from an unrelated rail.
        observer.observe_catch_up_complete(IndexableKind::Conversation);
        assert!(
            builder.is_catching_up(fauna_index::ContentKind::Mail),
            "a mail builder must ignore the Conversation boundary — it is a fact about the \
             conversation refold, not about this builder's mailbox walk"
        );
        observer.observe_catch_up_complete(IndexableKind::Mail);
        assert!(
            !builder.is_catching_up(fauna_index::ContentKind::Mail),
            "the debounce wrapper must forward the boundary to the builder it wraps"
        );
        // And its inverse, through the same wrapper: the seam's no-op default
        // would otherwise swallow the reopen silently.
        observer.observe_catch_up_reopened(IndexableKind::Mail);
        assert!(
            builder.is_catching_up(fauna_index::ContentKind::Mail),
            "the debounce wrapper must forward a reopened window to the builder it wraps"
        );
        cancel.cancel();
    }

    /// **The draft corpus must survive this wrapper too — the exact twin of the
    /// boundary test above, and it was NOT covered by it.**
    ///
    /// `observe_draft_corpus` has a no-op default on the seam (for the observers
    /// that do not index drafts), so a wrapper that fails to override it does not
    /// fail to compile — it silently swallows every corpus. And since
    /// `spawn_flush_debounce` returns the *only* observer app glue ever registers
    /// (`FanOutObserver` fans out to exactly these), swallowing it here means the
    /// drafts arm stages nothing on any seat, while `IndexBuilder`'s own unit
    /// tests — which hold the raw builder — keep passing.
    ///
    /// Asserted through the observer the caller actually holds, for the same
    /// reason the boundary test gives: reaching past the wrapper would pass with
    /// the wrapper's method deleted.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn the_draft_corpus_reaches_the_builder_through_the_debounce() {
        let recorder = Arc::new(Recorder::default());
        let builder = Arc::new(IndexBuilder::master(
            crate::IndexMasterKey::from_bytes([9u8; 32]),
            [fauna_index::ContentKind::Draft],
            recorder.clone(),
        ));
        let cancel = CancellationToken::new();
        let (observer, _stager) = spawn_flush_debounce(Arc::clone(&builder), cancel.clone());

        observer.observe_draft_corpus(&[fauna_conversations::index_sink::IndexableDraft {
            content_id: "thread-a".into(),
            thread_id: Some(ThreadId("thread-a".into())),
            subject: Some("provisions".into()),
            body: "pemmican for the crossing".into(),
        }]);

        // The corpus must also *pulse* the debounce, not merely land in the
        // builder: a staged snapshot nobody pulses waits for the 60s backstop,
        // which on a snapshot kind is a user typing into a search box that
        // cannot see what they just wrote.
        //
        // Pinned on the **paused** clock rather than a wall-clock measurement
        // (convention 14 — timer-driven behaviour via a fake clock): with time
        // auto-advancing, a pulsed corpus flushes one `STAGE_DEBOUNCE` after the
        // stage, an unpulsed one waits for `PERIODIC_INTERVAL`. Asserting on
        // that virtual gap is what makes deleting the pulse a red — a bare
        // "did it eventually flush" assert passes either way, because the
        // backstop always gets there in the end.
        let staged_at = tokio::time::Instant::now();
        tokio::time::timeout(Duration::from_secs(600), recorder.notify.notified())
            .await
            .expect("the draft corpus should have reached the builder and flushed");
        let waited = tokio::time::Instant::now() - staged_at;
        assert!(
            waited < PERIODIC_INTERVAL,
            "the corpus must pulse the debounce, not wait for the {PERIODIC_INTERVAL:?} \
             backstop — waited {waited:?}"
        );

        let paths = recorder.published.lock().unwrap().clone();
        assert!(
            paths.iter().any(|p| p.contains("seg-")),
            "the debounce wrapper must forward the draft corpus to the builder it wraps — \
             got {paths:?}"
        );
        cancel.cancel();
    }

    /// **The seam-less producer's corpus must pulse too — and nothing forwarded
    /// it, because it does not arrive through the observer at all.**
    ///
    /// The contacts walk stages by holding a handle the launcher kept, so both
    /// tests above are blind to it: the observer they drive is not on its path.
    /// What it held used to be the raw `Arc<IndexBuilder>`, which stages and
    /// pulses nothing — so an address book read at attach sat unpublished until
    /// the 60-second backstop, with the arm's own unit tests green throughout.
    /// [`DirectStager`] is the fix, and this is the pin that grades it.
    ///
    /// Same paused-clock shape as the drafts pulse for the same reason: only the
    /// virtual gap separates "pulsed" from "the backstop got there eventually".
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn a_contact_corpus_staged_off_the_seam_pulses_the_debounce() {
        let recorder = Arc::new(Recorder::default());
        let builder = Arc::new(IndexBuilder::master(
            crate::IndexMasterKey::from_bytes([9u8; 32]),
            [fauna_index::ContentKind::Contact],
            recorder.clone(),
        ));
        let cancel = CancellationToken::new();
        let (_observer, stager) = spawn_flush_debounce(Arc::clone(&builder), cancel.clone());

        let staged_at = tokio::time::Instant::now();
        stager
            .stage_contact_corpus(
                &[crate::IndexableContact {
                    uid_hash: "aa".into(),
                    display_name: Some("Ingrid Saltmarsh".into()),
                    text: "Ingrid Saltmarsh".into(),
                }],
                b"ctag-1".to_vec(),
            )
            .expect("stamp the corpus marker");

        tokio::time::timeout(Duration::from_secs(600), recorder.notify.notified())
            .await
            .expect("the contact corpus should have reached the builder and flushed");
        let waited = tokio::time::Instant::now() - staged_at;
        assert!(
            waited < PERIODIC_INTERVAL,
            "the walk's corpus must pulse the debounce, not wait for the \
             {PERIODIC_INTERVAL:?} backstop — waited {waited:?}"
        );

        let paths = recorder.published.lock().unwrap().clone();
        assert!(
            paths.iter().any(|p| p.contains("seg-")),
            "the staged corpus must have been published — got {paths:?}"
        );
        // The marker rides the same flush as the corpus it describes, which is
        // what makes the next launch's ctag precheck able to suppress a re-read.
        assert_eq!(
            stager.corpus_marker(fauna_index::ContentKind::Contact),
            Some(b"ctag-1".to_vec()),
            "staging must stamp the marker it was read at — a corpus without one \
             republishes on every launch"
        );
        cancel.cancel();
    }

    /// **A withheld stage notes no marker — either kind, either door.** A
    /// stood-down seat's walk stages nothing, and the pre-fix door stamped the
    /// marker anyway: in memory at once, and at rest the moment any other
    /// kind's doc flushed the manifest — recording a corpus this seat never
    /// staged, and suppressing every later walk until the next real corpus
    /// change. The marker means "the corpus I already staged"; it moves only
    /// when a stage lands.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn a_withheld_walk_notes_no_corpus_marker() {
        let recorder = Arc::new(Recorder::default());
        let gate = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let builder = Arc::new(
            IndexBuilder::master(
                crate::IndexMasterKey::from_bytes([9u8; 32]),
                [
                    fauna_index::ContentKind::Contact,
                    fauna_index::ContentKind::Post,
                ],
                recorder.clone(),
            )
            .with_lease_gate(Arc::clone(&gate)),
        );
        let cancel = CancellationToken::new();
        let (_observer, stager) = spawn_flush_debounce(Arc::clone(&builder), cancel.clone());

        stager
            .stage_contact_corpus(
                &[crate::IndexableContact {
                    uid_hash: "aa".into(),
                    display_name: Some("Ingrid Saltmarsh".into()),
                    text: "Ingrid Saltmarsh".into(),
                }],
                b"ctag-1".to_vec(),
            )
            .expect("the door reports Ok — there is nothing to retry");
        stager
            .stage_posts_walk(
                &[crate::IndexablePost {
                    post_id: "bb".into(),
                    created_at_micros: 1_000,
                    text: "the albatross".into(),
                }],
                b"posts-1".to_vec(),
            )
            .expect("the door reports Ok — there is nothing to retry");

        assert_eq!(
            stager.corpus_marker(fauna_index::ContentKind::Contact),
            None,
            "a marker over a corpus this seat never staged suppresses the walk \
             until the next real ctag change"
        );
        assert_eq!(
            stager.corpus_marker(fauna_index::ContentKind::Post),
            None,
            "same rule, append door: the next walk must retry, not early-stop"
        );

        // Gaining the lease, the identical walks land — stage, marker and all.
        gate.store(true, std::sync::atomic::Ordering::Release);
        stager
            .stage_posts_walk(
                &[crate::IndexablePost {
                    post_id: "bb".into(),
                    created_at_micros: 1_000,
                    text: "the albatross".into(),
                }],
                b"posts-1".to_vec(),
            )
            .expect("stamp the marker");
        assert_eq!(
            stager.corpus_marker(fauna_index::ContentKind::Post),
            Some(b"posts-1".to_vec()),
        );
        cancel.cancel();
    }

    /// The posts walk door pulses — same paused-clock grading as the contacts
    /// pin above, and for the same reason: the walk stages off the observer
    /// seam, so nothing else would stop its rows waiting out the backstop.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn a_posts_walk_staged_off_the_seam_pulses_the_debounce() {
        let recorder = Arc::new(Recorder::default());
        let builder = Arc::new(IndexBuilder::master(
            crate::IndexMasterKey::from_bytes([9u8; 32]),
            [fauna_index::ContentKind::Post],
            recorder.clone(),
        ));
        let cancel = CancellationToken::new();
        let (_observer, stager) = spawn_flush_debounce(Arc::clone(&builder), cancel.clone());

        let staged_at = tokio::time::Instant::now();
        stager
            .stage_posts_walk(
                &[crate::IndexablePost {
                    post_id: "aa".into(),
                    created_at_micros: 1_000,
                    text: "the albatross crossed the meridian".into(),
                }],
                b"posts-1".to_vec(),
            )
            .expect("stamp the corpus marker");

        tokio::time::timeout(Duration::from_secs(600), recorder.notify.notified())
            .await
            .expect("the walk's rows should have reached the builder and flushed");
        let waited = tokio::time::Instant::now() - staged_at;
        assert!(
            waited < PERIODIC_INTERVAL,
            "the walk must pulse the debounce, not wait for the {PERIODIC_INTERVAL:?} \
             backstop — waited {waited:?}"
        );
        assert_eq!(
            stager.corpus_marker(fauna_index::ContentKind::Post),
            Some(b"posts-1".to_vec()),
            "the marker rides the same flush as the rows it covers"
        );
        cancel.cancel();
    }

    /// The trickle door pulses too — a just-composed post the backstop alone
    /// would publish is a user searching for words they can see and not
    /// finding them, the exact cost `observe_draft_corpus` names.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn an_own_post_staged_through_the_trickle_door_pulses_the_debounce() {
        let recorder = Arc::new(Recorder::default());
        let builder = Arc::new(IndexBuilder::master(
            crate::IndexMasterKey::from_bytes([9u8; 32]),
            [fauna_index::ContentKind::Post],
            recorder.clone(),
        ));
        let cancel = CancellationToken::new();
        let (_observer, stager) = spawn_flush_debounce(Arc::clone(&builder), cancel.clone());

        let staged_at = tokio::time::Instant::now();
        stager.stage_own_post(&crate::IndexablePost {
            post_id: "cc".into(),
            created_at_micros: 2_000,
            text: "my own words, just composed".into(),
        });

        tokio::time::timeout(Duration::from_secs(600), recorder.notify.notified())
            .await
            .expect("the trickle doc should have reached the builder and flushed");
        let waited = tokio::time::Instant::now() - staged_at;
        assert!(
            waited < PERIODIC_INTERVAL,
            "the trickle must pulse the debounce, not wait for the \
             {PERIODIC_INTERVAL:?} backstop — waited {waited:?}"
        );
        assert_eq!(
            stager.corpus_marker(fauna_index::ContentKind::Post),
            None,
            "the trickle never advances the walk's marker — it would declare \
             corpus territory covered that no walk paged"
        );
        cancel.cancel();
    }

    /// The files walk door pulses — same grading and same reason as its two
    /// siblings: a walk stages off the observer seam, so nothing else would stop
    /// its rows waiting out the backstop.
    ///
    /// And it stamps **no marker**, which is the ruled suppression rather than an
    /// omission (File is append-shaped, so the stage-time guard already makes an
    /// unchanged corpus stage nothing). Asserted so a later edit adding a marker
    /// has to come with the withheld-notes-nothing discipline its siblings carry.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn a_files_walk_staged_off_the_seam_pulses_the_debounce_and_marks_nothing() {
        let recorder = Arc::new(Recorder::default());
        let builder = Arc::new(IndexBuilder::master(
            crate::IndexMasterKey::from_bytes([9u8; 32]),
            [fauna_index::ContentKind::File],
            recorder.clone(),
        ));
        let cancel = CancellationToken::new();
        let (_observer, stager) = spawn_flush_debounce(Arc::clone(&builder), cancel.clone());

        let staged_at = tokio::time::Instant::now();
        stager.stage_files_walk(&[crate::IndexableFile {
            folder_id: 42,
            path_hash_hex: "aa".into(),
            path: "holidays/albatross.jpg".into(),
            updated_at_secs: 1_700_000_000,
        }]);

        tokio::time::timeout(Duration::from_secs(600), recorder.notify.notified())
            .await
            .expect("the walk's rows should have reached the builder and flushed");
        let waited = tokio::time::Instant::now() - staged_at;
        assert!(
            waited < PERIODIC_INTERVAL,
            "the files walk must pulse the debounce, not wait for the \
             {PERIODIC_INTERVAL:?} backstop — waited {waited:?}"
        );
        assert_eq!(
            stager.corpus_marker(fauna_index::ContentKind::File),
            None,
            "File keeps no corpus marker in v1 — the stage-time guard is the \
             ruled suppression for an append kind"
        );
        cancel.cancel();
    }
}
