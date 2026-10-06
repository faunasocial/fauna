//! Linux leg of draft-persistence v2 for the **events rail**
//! (`docs/goal/behavior/reserved-folders.md` § Drafts Sync;
//! `docs/goal/ui/events.md` § Persistence).
//!
//! Pure trigger glue over the shared core: [`fauna_client_drafts::DraftsSync`]
//! owns the seal, the `fauna.drafts.{get,put}` calls, the launch gate and the
//! last-saved baseline, and the at-rest shape is
//! [`fauna_client_caldav::drafts::EventDrafts`] — the five user-authored
//! `event-form` inputs, nothing else.
//!
//! **Why this is not a copy of [`crate::feed::drafts`] / [`crate::conversations::drafts`].**
//! Those two hang off a long-lived *manager* that owns the compose state and
//! notifies observers, so their glue is "tick → re-read a fresh snapshot from
//! the manager". The Events page has no manager: the compose lives in a
//! transient `adw::Window` (`super::event_form`) that is built fresh on every
//! open and dropped on close, so there is nothing to hold a `Weak` to and
//! nothing to re-read at save time. This leg therefore **holds the draft
//! itself** — the same call tui's leg made for the same reason
//! (`apps/fauna-tui/src/events/drafts.rs`, and the ruling in
//! `reserved-folders.md` § Drafts Sync that the three trigger shapes stay three
//! files). Where tui carries the value on a channel, the GTK shell keeps it in
//! one main-thread cell, because the same cell is what the **New Event opener
//! reads to resume** — a dialog that does not yet exist cannot be handed a
//! restored draft, so the rail's live value has to outlive every compose window.
//!
//! * **Load on launch** — [`start`] spawns `DraftsSync::load()` on the tokio
//!   runtime and hops the result back to the GTK main loop with
//!   `glib::idle_add_once`; the restore is dropped if the user has already begun
//!   composing, so it can never clobber live input.
//! * **Debounced autosave** — every `event-form` input edit calls [`note_edit`],
//!   which stores the draft and re-arms a one-shot glib timer; only the timer
//!   whose generation is still current when it fires saves, so a burst coalesces
//!   into one `save_if_changed` after [`autosave_debounce`] of quiescence.
//! * **Cleared on create / on a day-cell compose** — [`clear`] empties the rail
//!   *and ticks it*, so a created or abandoned event stops following the user to
//!   their other devices (`events.md` § Persistence).
//! * **Leave-flush** (`reserved-folders.md` § The leave-flush promise, row 481)
//!   — [`flush_now_blocking`] forces an immediate save of the currently-held
//!   draft, bypassing the debounce, wired into `main.rs`'s
//!   `connect_close_request` alongside the other two rails' own flush.
//!
//! Wired from the AuthSuccess handler beside the other two rails (`app.rs`) and,
//! like them, outside every engine/MLS gate: drafts seal under the owner's
//! `BackupKey`, so nothing else failing may cost the user their unsent event.

use adw::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_caldav::drafts::EventDrafts;
use fauna_client_drafts::{DraftsSync, autosave_debounce};
use fauna_core::identity::ActorKeypair;

/// The event composer's `__drafts` path — one of the three frozen constants on
/// the wire (`fauna_protocol::drafts::DRAFT_RAILS`), never this app's choice: a
/// leg that minted its own rail name would round-trip only with itself and
/// silently lose every draft the user's other devices wrote
/// (`reserved-folders.md` § Drafts Sync step 1).
const RAIL: &str = fauna_protocol::drafts::RAIL_EVENTS;

type EventDraftsSync = DraftsSync<Arc<NestClient>>;

/// The five `event-form` inputs of a compose dialog that is open right now.
///
/// Held so a launch restore that lands **after** the user already opened the
/// form still reaches the widgets they are looking at. Without it the restore
/// would only ever be visible to the *next* open: the dialog is built once from
/// [`resume_draft`] and never re-reads the rail, so a slow nest would silently
/// turn "your draft came back" into "your draft is gone" — the exact latency
/// dependence e2e convention 14 forbids a test from papering over with a sleep.
pub struct OpenForm {
    pub summary: gtk::Entry,
    pub dtstart: gtk::Entry,
    pub dtend: gtk::Entry,
    pub location: gtk::Entry,
    pub description: gtk::TextView,
}

impl OpenForm {
    /// Whether the user has authored anything into this form yet. Deliberately
    /// the three free-text fields and not the datetimes: the opener *prefills*
    /// dtstart/dtend with today's working hours, so a form nobody has touched is
    /// never datetime-empty and testing those would make every restore look
    /// unsafe.
    fn is_untouched(&self) -> bool {
        let buf = self.description.buffer();
        self.summary.text().is_empty()
            && self.location.text().is_empty()
            && buf
                .text(&buf.start_iter(), &buf.end_iter(), false)
                .is_empty()
    }

    /// Paint a restored draft onto the live widgets, keeping the opener's date
    /// prefill wherever the draft has none (the same rule
    /// `PrefilledEventData::with_draft` applies at build time).
    fn apply(&self, draft: &EventDrafts) {
        self.summary.set_text(&draft.summary);
        self.location.set_text(&draft.location);
        self.description.buffer().set_text(&draft.description);
        if !draft.dtstart.is_empty() {
            self.dtstart.set_text(&draft.dtstart);
        }
        if !draft.dtend.is_empty() {
            self.dtend.set_text(&draft.dtend);
        }
    }
}

/// The rail's live state, owned by the GTK main thread for the session.
struct Rail {
    sync: Arc<EventDraftsSync>,
    runtime: tokio::runtime::Handle,
    /// The compose the user last left — what the New Event opener resumes and
    /// what the debounce timer saves.
    current: RefCell<EventDrafts>,
    /// Debounce generation: each edit bumps it, and only the timer still
    /// carrying the current value performs its save.
    generation: Cell<u64>,
    /// The compose dialog currently on screen, if any (see [`OpenForm`]).
    form: RefCell<Option<OpenForm>>,
}

thread_local! {
    /// `None` until a login installs the rail (and on a malformed secret, which
    /// disables persistence for the session rather than failing login — the
    /// same non-fatal arm both older legs take).
    static RAIL_STATE: RefCell<Option<Rc<Rail>>> = const { RefCell::new(None) };
}

/// Wire event-draft persistence for the just-authenticated actor: build the
/// shared [`DraftsSync`] and kick the launch restore.
///
/// Called from the AuthSuccess handler on the GTK main thread. A second login
/// (account switch / re-auth) replaces the rail wholesale, so the previous
/// actor's draft is never saved under the new actor's key: the old `Rc` is
/// dropped here and any timer still holding one saves into a `DraftsSync` that
/// no opener can reach and no further edit can feed.
///
/// `succession_predecessors` is the retired identities' `BackupKey`s
/// (`FaunaClient::predecessor_backup_keys()`, resolved ONCE by the caller and
/// shared with the `__mls` plane and the other two drafts rails — never a
/// second registry walk), offered to [`DraftsSync::with_predecessors`] so a
/// successor's launch load opens this rail instead of hard-erroring on it.
pub fn start(
    nest: Arc<NestClient>,
    secret_hex: &str,
    runtime: &tokio::runtime::Handle,
    succession_predecessors: &[fauna_client_drafts::BackupKey],
) {
    let keypair = match ActorKeypair::from_secret_hex(secret_hex) {
        Ok(k) => k,
        Err(e) => {
            tracing::warn!("event drafts: malformed identity secret, persistence disabled: {e}");
            RAIL_STATE.with(|s| *s.borrow_mut() = None);
            return;
        }
    };
    let rail = Rc::new(Rail {
        sync: Arc::new(build_sync(nest, &keypair, succession_predecessors)),
        runtime: runtime.clone(),
        current: RefCell::new(EventDrafts::default()),
        generation: Cell::new(0),
        form: RefCell::new(None),
    });
    RAIL_STATE.with(|s| *s.borrow_mut() = Some(Rc::clone(&rail)));
    restore_on_launch(Arc::clone(&rail.sync), runtime);
}

/// Assemble this rail's [`DraftsSync`] — factored out of [`start`] so the
/// call-site pin test below can call it without building a whole rail
/// (mirrors `conversations::drafts::build_sync` / `feed::drafts::build_sync`).
///
/// ⚠ The retired keys are a READ fallback, never a seal root
/// ([`DraftsSync::with_predecessors`]). A successor's launch load beats the
/// post-auth re-seal pass more often than not, and without them it
/// hard-errors, the load gate never lifts, and the user's half-written event
/// draft stays invisible for the whole session.
fn build_sync(
    nest: Arc<NestClient>,
    keypair: &ActorKeypair,
    succession_predecessors: &[fauna_client_drafts::BackupKey],
) -> EventDraftsSync {
    DraftsSync::new(nest, keypair, RAIL).with_predecessors(succession_predecessors.to_vec())
}

/// Spawn the launch load and hand the result back to the GTK main loop.
///
/// `Ok(None)` is first run (keep the empty form); a transport/seal error is
/// logged and left non-fatal — an unreachable nest must not blank the composer.
/// A blob that decodes to an all-empty record is dropped rather than installed:
/// it is indistinguishable from no draft, and the form's default already is it.
fn restore_on_launch(sync: Arc<EventDraftsSync>, runtime: &tokio::runtime::Handle) {
    runtime.spawn(async move {
        // The identity seam: the handle this load was launched for. Nothing
        // cancels the load, and [`install_restored`] re-resolves `RAIL_STATE`
        // at DELIVERY time, so without it a load outliving an account switch
        // installs the departing actor's draft into the incoming actor's rail
        // (`account-scoping.md` § The scoping taxonomy → the in-memory
        // corollary; ).
        let launched_for = Arc::clone(&sync);
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
                // Cross-thread hop onto the GTK main loop, where the rail cell
                // lives (the same shape `sync_agent.rs` uses for its observers).
                glib::idle_add_once(move || install_restored(draft, &launched_for));
            }
            // A corrupt or newer-shape blob must never break composing — the
            // form simply stays empty (the shared record's own contract).
            Err(e) => tracing::warn!("event drafts: restore failed: {e}"),
        }
    });
}

/// Install a restored draft, unless the user has already started composing —
/// or unless the actor it belongs to has since gone away.
///
/// Two guards, for two different races:
///
/// * `current.is_empty()` is what makes an async restore safe *within* one
///   session: the load normally lands long before the first keystroke, but a
///   slow nest could otherwise drop a stale draft on top of text the user is
///   typing right now.
/// * `launched_for` is the **identity** seam, and the empty-compose guard is no
///   substitute for it — a rail freshly installed for the incoming actor has an
///   empty compose by construction, so it *passes*. `start` replaces the rail
///   wholesale on a second login, and this function reads `RAIL_STATE` at
///   delivery time rather than holding the rail it was launched for, so the
///   departing actor's half-written event would land in the next actor's
///   composer — and their first edit would autosave it under their own
///   `BackupKey`. Comparing the `DraftsSync` handles answers "is this still the
///   rail that asked?" without a counter to keep in step.
fn install_restored(draft: EventDrafts, launched_for: &Arc<EventDraftsSync>) {
    RAIL_STATE.with(|s| {
        let borrowed = s.borrow();
        let Some(rail) = borrowed.as_ref() else {
            return;
        };
        if !Arc::ptr_eq(&rail.sync, launched_for) {
            tracing::debug!("event drafts: restore outlived its session, dropped");
            return;
        }
        {
            let mut current = rail.current.borrow_mut();
            if !current.is_empty() {
                tracing::debug!("event drafts: compose already in progress, restore skipped");
                return;
            }
            *current = draft.clone();
        }
        // The form may already be on screen — the user can open New Event
        // before a slow load returns. Painting it here is what makes the
        // restore observable without the opener having to wait for the nest.
        if let Some(form) = rail.form.borrow().as_ref()
            && form.is_untouched()
        {
            form.apply(&draft);
        }
    });
}

/// Register the compose dialog now on screen, so a late launch restore can still
/// reach it (see [`OpenForm`]). Replacing an existing registration is correct:
/// the previous dialog is gone by the time a new one is built.
pub fn attach_form(form: OpenForm) {
    RAIL_STATE.with(|s| {
        if let Some(rail) = s.borrow().as_ref() {
            *rail.form.borrow_mut() = Some(form);
        }
    });
}

/// Forget the compose dialog — it has closed. Keeps the rail from holding
/// widgets of a destroyed window alive for the rest of the session.
pub fn detach_form() {
    RAIL_STATE.with(|s| {
        if let Some(rail) = s.borrow().as_ref() {
            *rail.form.borrow_mut() = None;
        }
    });
}

/// The draft the **New Event opener** should resume, or `None` when persistence
/// is off / nothing is pending (`events.md` § Persistence — that opener
/// restores; clearing there is what would make the rail inert).
///
/// The day-cell gesture deliberately does NOT call this: it means *start a new
/// event here*, so it calls [`clear`] instead.
pub fn resume_draft() -> Option<EventDrafts> {
    RAIL_STATE.with(|s| {
        let borrowed = s.borrow();
        let rail = borrowed.as_ref()?;
        let draft = rail.current.borrow().clone();
        (!draft.is_empty()).then_some(draft)
    })
}

/// Record a compose edit and re-arm the debounced save. A no-op when
/// persistence is off for this session.
pub fn note_edit(draft: EventDrafts) {
    RAIL_STATE.with(|s| {
        if let Some(rail) = s.borrow().as_ref() {
            *rail.current.borrow_mut() = draft;
            arm_debounce(rail);
        }
    });
}

/// Empty the rail **and tick it** — a created event, an explicit discard, or a
/// day-cell "start a new event here" (`events.md` § Persistence). Distinct from
/// simply clearing the form's widgets: forgetting the tick would leave a stale
/// draft that reappears on the next launch for an event already on the calendar.
pub fn clear() {
    note_edit(EventDrafts::default());
}

/// Force an immediate, bounded-blocking save of the current held draft — the
/// leave-door flush (`reserved-folders.md` § The leave-flush promise, row
/// 481), bypassing the debounce timer entirely. A no-op before the first
/// [`start`] (no rail installed yet). Reads `RAIL_STATE` directly rather than
/// through a separate slot: this module already owns the thread-local, and
/// both it and `main.rs`'s close handler run on the GTK main thread.
pub fn flush_now_blocking(bounded: bool) {
    let Some((sync, snapshot)) = RAIL_STATE.with(|s| {
        let borrowed = s.borrow();
        let rail = borrowed.as_ref()?;
        Some((
            Arc::clone(&rail.sync),
            rail.current.borrow().snapshot_bytes(),
        ))
    }) else {
        return;
    };
    crate::blocking_flush::run_bounded(
        async move {
            if let Err(e) = sync.save_if_changed(&snapshot).await {
                tracing::warn!("event drafts: leave-flush failed: {e}");
            }
        },
        std::time::Duration::from_millis(2_000),
        std::time::Duration::from_millis(2_500),
        bounded,
    );
}

/// (Re)arm the one-shot debounce timer for the current edit burst. Each edit
/// bumps the generation and schedules an [`autosave_debounce`] timer; only the
/// timer whose generation is still current when it fires performs the save, so a
/// later edit supersedes an earlier pending one — the same shape both older
/// linux rails use, over the one shared window.
fn arm_debounce(rail: &Rc<Rail>) {
    let g = rail.generation.get().wrapping_add(1);
    rail.generation.set(g);
    let rail = Rc::clone(rail);
    glib::timeout_add_local_once(autosave_debounce(), move || {
        if rail.generation.get() != g {
            return; // a newer edit arrived; its timer owns the save
        }
        let bytes = rail.current.borrow().snapshot_bytes();
        let sync = Arc::clone(&rail.sync);
        // `save_if_changed` gates the pre-load window and dedups an unchanged
        // snapshot, so a tick that changed nothing costs a cheap byte-compare —
        // and crucially never an empty PUT before the launch load has run, which
        // is what keeps a slow load from wiping a real draft.
        rail.runtime.spawn(async move {
            if let Err(e) = sync.save_if_changed(&bytes).await {
                tracing::warn!("event drafts: autosave failed: {e}");
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rail name is the wire's closed enumeration, not this app's choice —
    /// the property that makes a linux-written draft restore on the user's
    /// phone.
    #[test]
    fn the_rail_is_the_ratified_events_constant() {
        assert_eq!(RAIL, "events");
        assert!(
            fauna_protocol::drafts::is_ratified_rail(RAIL),
            "the rail must be one of the nest-validated DRAFT_RAILS",
        );
    }

    /// Build a rail for one actor and install it, exactly as [`start`] does.
    fn install_test_rail(rt: &tokio::runtime::Runtime) -> Rc<Rail> {
        let nest = fauna_client::NestClient::new(
            "http://127.0.0.1:1".to_string(),
            ActorKeypair::generate(),
        );
        let keypair = ActorKeypair::generate();
        let rail = Rc::new(Rail {
            sync: Arc::new(DraftsSync::new(nest, &keypair, RAIL)),
            runtime: rt.handle().clone(),
            current: RefCell::new(EventDrafts::default()),
            generation: Cell::new(0),
            form: RefCell::new(None),
        });
        RAIL_STATE.with(|s| *s.borrow_mut() = Some(Rc::clone(&rail)));
        rail
    }

    /// **A restore that outlived the account switch reaches nothing.**
    ///
    /// `account-scoping.md` § The scoping taxonomy → the in-memory corollary:
    /// the loops that WRITE actor-scoped state must be retired by the same
    /// drop, and one holding no cancellation handle needs a seam. The launch
    /// load is exactly that loop, and [`install_restored`] re-resolves
    /// `RAIL_STATE` at delivery time rather than holding the rail it was
    /// launched for — so without the handle check the departing actor's
    /// half-written event lands in the incoming actor's composer, and their
    /// first edit autosaves it under their own `BackupKey`.
    ///
    /// ⚠ The `current.is_empty()` guard above it is NOT a substitute and must
    /// not be mistaken for one: a rail freshly installed for the incoming actor
    /// has an empty compose by construction, so it passes.
    ///
    /// Red-verify by deleting the `Arc::ptr_eq` guard: the summary below
    /// becomes the incoming actor's resumable draft.
    #[test]
    fn a_restore_from_a_departed_session_never_reaches_the_next_actor() {
        let rt = tokio::runtime::Runtime::new().expect("test runtime");
        let departing = install_test_rail(&rt);
        let launched_for = Arc::clone(&departing.sync);

        // The switch: a second login replaces the rail wholesale.
        let _incoming = install_test_rail(&rt);

        install_restored(
            EventDrafts {
                summary: "the departing actor's offsite".into(),
                description: "and its private notes".into(),
                ..EventDrafts::default()
            },
            &launched_for,
        );

        assert_eq!(
            resume_draft(),
            None,
            "the departing actor's draft was installed into the next actor's rail",
        );
    }

    /// The positive control, and it is load-bearing: the seam must reject a
    /// *foreign* session's restore, not the restore. A guard that also dropped
    /// the in-session case would silently retire draft persistence on linux.
    #[test]
    fn a_restore_landing_inside_its_own_session_still_installs() {
        let rt = tokio::runtime::Runtime::new().expect("test runtime");
        let rail = install_test_rail(&rt);
        let launched_for = Arc::clone(&rail.sync);

        install_restored(
            EventDrafts {
                summary: "lunch with the auditors".into(),
                ..EventDrafts::default()
            },
            &launched_for,
        );

        assert_eq!(
            resume_draft().map(|d| d.summary),
            Some("lunch with the auditors".to_string()),
        );
    }

    /// With no login installed, every door is a safe no-op rather than a panic:
    /// the compose is reachable before AuthSuccess in tests and on the
    /// malformed-secret arm, and neither may take the app down.
    #[test]
    fn the_doors_are_inert_without_a_rail() {
        RAIL_STATE.with(|s| *s.borrow_mut() = None);
        assert_eq!(resume_draft(), None);
        note_edit(EventDrafts {
            summary: "no rail installed".into(),
            ..EventDrafts::default()
        });
        clear();
        assert_eq!(resume_draft(), None, "still nothing to resume");
    }

    /// An all-empty record is not a draft — the opener must not "resume" one,
    /// or every fresh compose would look like a restore.
    #[test]
    fn an_empty_record_is_not_a_resumable_draft() {
        assert!(EventDrafts::default().is_empty());
        assert!(
            !EventDrafts {
                location: "Room 2".into(),
                ..EventDrafts::default()
            }
            .is_empty(),
            "any user-authored field makes it a draft — not just the summary",
        );
    }
}

#[cfg(test)]
mod succession_fallback_tests {
    use super::*;

    /// **The call-site pin for the `__drafts` read fallback on the events
    /// rail.** `fauna-client-drafts`' own tests prove the fallback *works*;
    /// none of them can see THIS app stop passing the walk.
    ///
    /// Mutation: drop the `.with_predecessors(..)` in [`build_sync`] and this reds.
    #[test]
    fn the_events_rail_offers_the_accounts_retired_roots() {
        let retired = vec![fauna_core::crypto::BackupKey::from_bytes([0x33u8; 32])];
        let sync = build_sync(
            NestClient::new("http://127.0.0.1:1".to_string(), ActorKeypair::generate()),
            &ActorKeypair::generate(),
            &retired,
        );
        assert_eq!(sync.predecessor_count(), 1);
    }

    #[test]
    fn an_identity_that_never_succeeded_offers_nothing() {
        let sync = build_sync(
            NestClient::new("http://127.0.0.1:1".to_string(), ActorKeypair::generate()),
            &ActorKeypair::generate(),
            &[],
        );
        assert_eq!(sync.predecessor_count(), 0);
    }
}
