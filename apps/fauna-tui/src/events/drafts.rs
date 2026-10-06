//! tui leg of draft-persistence v2 for the **events rail**
//! (`docs/goal/behavior/reserved-folders.md` § Drafts Sync;
//! `docs/goal/ui/events.md` § Persistence).
//!
//! Pure trigger glue over the shared core, and the **first app leg on this
//! rail** — the third and last constant of `fauna_protocol::drafts::DRAFT_RAILS`,
//! which the plane accepted from the start and no app wrote until this module.
//! The seal, the `fauna.drafts.{get,put}` calls, the launch gate and the
//! last-saved baseline all live in [`fauna_client_drafts::DraftsSync`]; the
//! at-rest shape lives in [`fauna_client_caldav::drafts::EventDrafts`].
//!
//! **Why this is NOT a third copy of the other two rails' glue.** The feed and
//! conversations legs hang off a shared *manager* that owns the compose state
//! and notifies observers, so their glue can re-read a fresh snapshot per save
//! behind a `Weak` handle. The Events page has no manager: its compose lives in
//! [`crate::events::EventsState`] on the UI thread, mutated by the action loop.
//! So the tick **carries** the draft (five owned `String`s — cheap) instead of
//! pointing at a manager, and the debounce task keeps the latest value it saw.
//! That is a genuinely different trigger shape, not a divergence to collapse.
//!
//! * **Load on launch** — `DraftsSync::load()` on a spawned task, handed back
//!   to the UI thread as an ordinary [`crate::events::Outcome`] through the
//!   page's existing `PageOutcome::Events` channel, so the restore lands in
//!   `apply_outcome` like every other async result on this page.
//! * **Debounced autosave** — [`crate::events::set_field`] ticks this channel on
//!   every compose-buffer edit; after [`autosave_debounce`] of quiescence the coalesced
//!   burst is saved via `DraftsSync::save_if_changed`.
//! * **Flush on teardown** — when the last sender drops (the session is over),
//!   the task saves the latest value it holds rather than discarding it. This is
//!   the windows conversations leg's "quit flush" (`reserved-folders.md`
//!   § Drafts Sync's per-app table), adopted here as the richer existing pattern
//!   (priority #4) — without it the final edit inside the debounce window is
//!   lost exactly when the user is quitting, which is when they most expect a
//!   draft to survive.
//!
//! Wired from the one post-auth hook (`session::establish`), beside
//! `crate::events::init` — and, like both older legs, outside any engine/MLS
//! gate: drafts seal under the owner's `BackupKey`, so nothing else failing may
//! cost the user their unsent event.

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_caldav::drafts::EventDrafts;
use fauna_client_drafts::{DraftsSync, autosave_debounce};
use fauna_core::identity::ActorKeypair;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::app::{DataMessage, PageOutcome, UiMessage};

/// The event composer's `__drafts` path (one combined blob, per
/// `reserved-folders.md` § Drafts Sync step 1). The enumeration is the wire's,
/// not this app's — a leg that minted its own rail name would round-trip only
/// with itself and silently lose every draft the user's other devices wrote.
const RAIL: &str = fauna_protocol::drafts::RAIL_EVENTS;

pub(crate) type EventDraftsSync = DraftsSync<Arc<NestClient>>;

/// Wire event-draft persistence for the just-authenticated actor: build the
/// shared [`DraftsSync`], kick the launch restore, and return the channel
/// [`EventsState`](crate::events::EventsState) ticks on every compose edit,
/// alongside the `DraftsSync` itself for `main.rs`'s leave-door flush
/// ([`flush_now`], stored on `EventsState::drafts_sync`).
///
/// A malformed secret is logged and skipped — `(None, None)` disables
/// persistence for the session rather than failing login, the same non-fatal
/// arm both older legs take.
pub fn start(
    nest: Arc<NestClient>,
    secret_hex: &str,
    succession_predecessors: &[fauna_client_drafts::BackupKey],
    session_generation: u64,
    tx: &UnboundedSender<UiMessage>,
) -> (
    Option<UnboundedSender<EventDrafts>>,
    Option<Arc<EventDraftsSync>>,
) {
    let keypair = match ActorKeypair::from_secret_hex(secret_hex) {
        Ok(k) => k,
        Err(e) => {
            tracing::warn!("event drafts: malformed identity secret, persistence disabled: {e}");
            return (None, None);
        }
    };
    let sync = Arc::new(build_sync(nest, &keypair, succession_predecessors));
    restore_on_launch(Arc::clone(&sync), session_generation, tx.clone());
    let drafts_tx = attach_autosave(Arc::clone(&sync));
    (Some(drafts_tx), Some(sync))
}

/// Force an immediate save of `draft` (the caller's current live compose
/// state — [`EventsState::event_draft`]), bypassing the debounce entirely —
/// the leave-door flush (`reserved-folders.md` § The leave-flush promise, row
/// 481), awaited directly in `main.rs` right before the process exits.
pub(crate) async fn flush_now(sync: &EventDraftsSync, draft: &EventDrafts) {
    save(sync, draft).await;
}

/// Assemble this rail's [`DraftsSync`] — factored out of [`start`] so the
/// call-site pin can call it, exactly as the feed leg factors its own.
///
/// ⚠ The retired keys are a READ fallback, never a seal root
/// ([`DraftsSync::with_predecessors`]). A successor's launch load beats the
/// post-auth re-seal pass more often than not, and without them it hard-errors,
/// the load gate never lifts, and the user's half-written event stays invisible
/// for the whole session. Resolved ONCE by the session hook and passed in.
fn build_sync(
    nest: Arc<NestClient>,
    keypair: &ActorKeypair,
    succession_predecessors: &[fauna_client_drafts::BackupKey],
) -> EventDraftsSync {
    DraftsSync::new(nest, keypair, RAIL).with_predecessors(succession_predecessors.to_vec())
}

/// Spawn the launch load: fetch + unseal this actor's event draft and hand it to
/// the UI thread. `Ok(None)` is first run (keep the empty form); a
/// transport/seal error is logged and left non-fatal — an unreachable nest must
/// not blank the form. An all-empty record is dropped rather than dispatched:
/// it is indistinguishable from no draft, and the form's default already is it.
///
/// **`session_generation` is the identity seam** (`account-scoping.md` § The
/// scoping taxonomy → the in-memory corollary: a loop that writes actor-scoped
/// state and holds no cancellation handle needs one, and the seam comes before
/// the drop). This task holds no handle anything cancels, and the `UiMessage`
/// channel it sends on is process-wide, not session-scoped: a `load()` still in
/// flight when the user switches accounts resolves anyway, and its outcome would
/// land in whatever `EventsState` `session::establish` has since installed for
/// the INCOMING actor. So the generation the restore was launched under travels
/// with the outcome and `apply_outcome` drops a stale one — the departing
/// actor's half-written event never reaches the next actor's compose buffers,
/// and so never rides their next keystroke into a save sealed under their own
/// `BackupKey`.
fn restore_on_launch(
    sync: Arc<EventDraftsSync>,
    session_generation: u64,
    tx: UnboundedSender<UiMessage>,
) {
    tokio::spawn(async move {
        let bytes = match sync.load().await {
            Ok(Some(bytes)) => bytes,
            Ok(None) => {
                tracing::debug!("event drafts: none persisted yet (first run)");
                return;
            }
            Err(e) => {
                tracing::warn!("event drafts: load failed: {e}");
                return;
            }
        };
        match EventDrafts::restore_from_bytes(&bytes) {
            Ok(draft) if draft.is_empty() => {
                tracing::debug!("event drafts: persisted blob is an empty compose, ignored")
            }
            Ok(draft) => {
                tracing::info!("event drafts: restored {} bytes", bytes.len());
                let _ = tx.send(UiMessage::Data(DataMessage::Page(
                    session_generation,
                    PageOutcome::Events(crate::events::Outcome::DraftsLoaded {
                        draft: Box::new(draft),
                        session_generation,
                    }),
                )));
            }
            // A corrupt or newer-shape blob must never break composing — the
            // form simply stays empty (the shared record's own contract).
            Err(e) => tracing::warn!("event drafts: restore failed: {e}"),
        }
    });
}

/// Attach the debounced autosave: compose edit → unbounded channel → tokio
/// debounce loop → `save_if_changed`.
///
/// `save_if_changed` gates the pre-load window and dedups an unchanged
/// snapshot, so a tick that changed nothing costs a cheap byte-compare — no
/// upload, and crucially no empty PUT before the launch load has run, which is
/// what keeps a slow load from wiping a real draft.
fn attach_autosave(sync: Arc<EventDraftsSync>) -> UnboundedSender<EventDrafts> {
    let (tx, mut rx) = unbounded_channel::<EventDrafts>();
    tokio::spawn(async move {
        // `None` = every sender dropped: this login is over, so is this task.
        while let Some(first) = rx.recv().await {
            let (latest, still_open) = quiesce(&mut rx, first).await;
            save(&sync, &latest).await;
            if !still_open {
                return;
            }
        }
    });
    tx
}

/// Wait for [`autosave_debounce`] of quiescence, re-arming on every further tick of the
/// same edit burst and keeping the newest draft seen. Returns that draft plus
/// whether the channel is still open — a closed channel still yields its last
/// value, so teardown flushes rather than discards (see the module docs).
async fn quiesce(
    rx: &mut UnboundedReceiver<EventDrafts>,
    first: EventDrafts,
) -> (EventDrafts, bool) {
    let mut latest = first;
    loop {
        tokio::select! {
            // `sleep` and `UnboundedReceiver::recv` are both cancel-safe, which
            // is what makes re-arming here lossless.
            _ = tokio::time::sleep(autosave_debounce()) => return (latest, true),
            tick = rx.recv() => match tick {
                Some(newer) => latest = newer,
                None => return (latest, false),
            },
        }
    }
}

/// One save attempt. A failure is logged, never surfaced: the user's form still
/// holds the text, and the next edit retries.
async fn save(sync: &EventDraftsSync, draft: &EventDrafts) {
    if let Err(e) = sync.save_if_changed(&draft.snapshot_bytes()).await {
        tracing::warn!("event drafts: autosave failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(summary: &str) -> EventDrafts {
        EventDrafts {
            summary: summary.into(),
            ..EventDrafts::default()
        }
    }

    /// The rail name is the wire's closed enumeration, not this app's choice —
    /// the property that makes a tui-written draft restore on the user's phone.
    #[test]
    fn the_rail_is_the_ratified_events_constant() {
        assert_eq!(RAIL, "events");
        assert!(
            fauna_protocol::drafts::is_ratified_rail(RAIL),
            "the rail must be one of the nest-validated DRAFT_RAILS",
        );
    }

    /// A burst of edits coalesces into ONE save carrying the LAST value — the
    /// whole point of the debounce, and the property that keeps typing from
    /// PUTting once per keystroke.
    #[tokio::test(start_paused = true)]
    async fn a_burst_coalesces_to_the_last_draft() {
        let (tx, mut rx) = unbounded_channel::<EventDrafts>();
        tx.send(draft("a")).unwrap();
        tx.send(draft("ab")).unwrap();
        tx.send(draft("abc")).unwrap();

        let first = rx.recv().await.unwrap();
        let (latest, still_open) = quiesce(&mut rx, first).await;

        assert_eq!(latest.summary, "abc", "the newest edit wins");
        assert!(still_open, "the sender is still alive");
    }

    /// Teardown flushes: the last edit inside the debounce window survives the
    /// session ending, rather than being dropped on the floor.
    #[tokio::test(start_paused = true)]
    async fn a_closed_channel_still_yields_its_last_draft() {
        let (tx, mut rx) = unbounded_channel::<EventDrafts>();
        tx.send(draft("half a thought")).unwrap();
        tx.send(draft("half a thought, revised")).unwrap();
        drop(tx);

        let first = rx.recv().await.unwrap();
        let (latest, still_open) = quiesce(&mut rx, first).await;

        assert_eq!(latest.summary, "half a thought, revised");
        assert!(!still_open, "the caller must retire after this flush");
    }
}
