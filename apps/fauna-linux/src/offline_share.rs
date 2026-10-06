//! linux's leg of the **offline share-initiation ceremony** — the co-present,
//! nest-free hand-off of a recipient set (`docs/goal/behavior/p2p.md`
//! § Offline share initiation).
//!
//! # What is left here, and why only this
//!
//! The ceremony itself — every ordering rule, every write-through, every
//! record and the folders page's own read — lives once, shared, in
//! [`fauna_sync_engine::offline_share`], and is re-exported below so this
//! app's call sites read unchanged. Two things genuinely cannot follow it
//! there, and they are the whole of this file:
//!
//! 1. **This app's device label.** The bind doors themselves are shared now
//!    (2026-08-23) — what a leg supplies is the string it calls itself by,
//!    and the actor-keyed `fauna_iroh` transport factory (`fauna_iroh::
//!    ceremony_transport`, shared with `apps/fauna-tui` since round 33 —
//!    the shared crate names no concrete transport substrate itself, the
//!    iroh-cleanliness bargain `fauna_sync_engine::peer_leg` documents, but
//!    both native apps assemble it identically). The wrapper below is that
//!    label applied to the shared doors, and nothing else.
//! 2. **The i18n *resolution*.** Only the resolution: which key each state
//!    and each refusal carries is a fact, and it lives once beside the types
//!    in [`fauna_client_capabilities::group_ceremony_view`]
//!    (`status_label` / `code_error_label`) — the arrangement `p2p.md`
//!    § Cross-user shared-set transfer already ratifies for this page's other
//!    readings, and that `share_glue.rs`'s `serve_status_text` next door has
//!    followed since the plane driver's own lift. The two doors below turn
//!    that key into a sentence through this app's own lookup, and do nothing
//!    else.
//!
//! Why the transport is actor-keyed and relay-less is the shared factory
//! type's own documentation ([`CeremonyTransportFactory`]) — this app only
//! satisfies it.

use std::cell::RefCell;
use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_capabilities::group_ceremony_view::{
    CeremonyStatus, PeerCodeError, code_error_label, status_label,
};

pub use fauna_sync_engine::offline_share::*;

/// This app's name for itself in a ceremony — the string the person across
/// the table reads on their own screen while comparing codes.
const DEVICE_LABEL: &str = "fauna-linux";

/// This session's ceremony seat for the Folders page panel — bound, or handed
/// back when the share plane already bound it. This app's label and transport
/// over the shared door. The bind never waits for the account runtime: the
/// runtime lends the ceremony record to the seat at its store-ready edge
/// (`SessionSeat::lend_record`).
pub async fn bind_seat(
    session_seat: &SessionSeat,
    nest: Arc<NestClient>,
    secret_hex: String,
) -> Result<Arc<CeremonySeat>, String> {
    fauna_sync_engine::offline_share::bind_seat(
        session_seat,
        nest,
        &secret_hex,
        DEVICE_LABEL,
        &fauna_iroh::ceremony_transport(),
    )
    .await
}

/// The share plane's bind door (`crate::share_glue`, row 338) — the SAME
/// session seat as [`bind_seat`], lent the transfer plane's roster. This
/// app's label and transport over the shared door.
pub async fn bind_share_plane_seat(
    session_seat: &SessionSeat,
    secret_hex: &str,
    evidence: Option<Vec<String>>,
    membership: &Arc<dyn fauna_peer_share::admission::SetMembership + Send + Sync>,
    group_roster: &Arc<dyn fauna_peer_share::admission::GroupRosterState + Send + Sync>,
) -> Result<(Arc<CeremonySeat>, Vec<std::net::SocketAddr>), String> {
    fauna_sync_engine::offline_share::bind_share_plane_seat(
        session_seat,
        secret_hex,
        DEVICE_LABEL,
        evidence,
        membership,
        group_roster,
        &fauna_iroh::ceremony_transport(),
    )
    .await
}

/// The refusal reading for a typed compare code, resolved through this app's
/// own i18n lookup. The key — and the judgement that an empty box says
/// nothing — are the shared crate's.
pub fn code_error_text(e: PeerCodeError) -> Option<String> {
    code_error_label(e).map(|t| t.resolve(crate::i18n::strings::lookup))
}

/// The `offline-share-status` reading for a state.
pub fn status_text(status: CeremonyStatus) -> String {
    status_label(status).resolve(crate::i18n::strings::lookup)
}

thread_local! {
    /// The live window's panel-reset callback. Registered once, at window
    /// build (`views::devices_folders::build_devices_and_folders_pages`),
    /// closing over that window's own `Rc<RefCell<OfflineShareState>>` and
    /// `OfflineShareHandles` — [`reset_for_actor_change`] has neither, since
    /// `actor_scope::reset_actor_scoped_state` calls it with no window in
    /// scope. Only one window is ever live (linux never exits on an actor
    /// change, `actor_scope` module docs), so the next sign-in's window
    /// simply overwrites this one; never appended to. `None` before any
    /// window has built — a factory-reset landing mid-onboarding, or this
    /// module's own unit tests.
    static RESET_HOOK: RefCell<Option<Box<dyn Fn()>>> = const { RefCell::new(None) };
}

/// Register this window's offline-share panel reset — see [`RESET_HOOK`].
pub fn register_reset_hook(hook: impl Fn() + 'static) {
    RESET_HOOK.with(|cell| *cell.borrow_mut() = Some(Box::new(hook)));
}

thread_local! {
    /// The live window's read of its own [`SessionSeat`], registered at window
    /// build beside [`RESET_HOOK`] and for the same reason: the panel state
    /// lives in that window's `Rc<RefCell<OfflineShareState>>`, and
    /// `crate::client::FaunaClient` — which drives the Folders page's reads —
    /// and the automation arms that act on the live listener have no way to
    /// reach it. `None` before any window has built.
    static SEAT_SOURCE: RefCell<Option<Box<dyn Fn() -> SessionSeat>>> =
        const { RefCell::new(None) };
}

/// Register this window's read of its own ceremony seat slot — see
/// [`SEAT_SOURCE`].
pub fn register_seat_source(source: impl Fn() -> SessionSeat + 'static) {
    SEAT_SOURCE.with(|cell| *cell.borrow_mut() = Some(Box::new(source)));
}

/// This sign-in's ceremony seat slot, for the Folders page's own read (its
/// bound seat's replica answers until the account runtime lends the record)
/// and the automation arms that act on the live listener
/// (`offline_share_drop_connections`, `offline_share_probe_set`).
///
/// GTK thread only (it reads the window's `RefCell`), so a caller clones it
/// out HERE and moves the clone into whatever task needs it — the shape
/// `account_runtime::install` already uses for the share plane's seams.
/// `None` before any window has built — the same nothing
/// [`fauna_sync_engine::offline_share::load_group_shares`] reads without a
/// seat, and what those arms report as "no ceremony seat is bound".
pub fn session_seat() -> Option<SessionSeat> {
    SEAT_SOURCE.with(|cell| cell.borrow().as_ref().map(|read| read()))
}

/// `actor_scope::reset_actor_scoped_state`'s call in for this app's Folders
/// page panel — drops the session's `OfflineShareState` (its `SessionSeat`
/// included) back to fresh/unbound and repaints, mirroring tui's `sign_out`
/// (`apps/fauna-tui/src/session.rs`). Called synchronously, on the GTK thread
/// doing the teardown, since sign-out stops the message pump
/// (`settings::trigger_pump_shutdown`) around the same point — a reset
/// queued as a `DataMessage` could be left unhandled. A no-op before any
/// window has registered.
pub fn reset_for_actor_change() {
    RESET_HOOK.with(|cell| {
        if let Some(hook) = cell.borrow().as_ref() {
            hook();
        }
    });
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use super::*;

    /// Every ceremony state resolves to its own English reading through
    /// linux's own lookup — the shared label's keys are in the table this app
    /// links, not just tui's. (The key-level uniqueness is pinned once, on
    /// the shared side.)
    #[test]
    fn every_ceremony_status_has_its_own_reading() {
        let all = [
            CeremonyStatus::Idle,
            CeremonyStatus::Expecting,
            CeremonyStatus::OfferSent,
            CeremonyStatus::AwaitingConsent,
            CeremonyStatus::Delivering,
            CeremonyStatus::Delivered,
            CeremonyStatus::Admitted,
            CeremonyStatus::Failed,
        ];
        let texts: Vec<String> = all.iter().map(|s| status_text(*s)).collect();
        for t in &texts {
            assert!(!t.is_empty(), "every status has a reading");
            assert!(
                !t.contains("folders.offline_share_status"),
                "a raw i18n key reached the surface: {t}"
            );
        }
        let unique: std::collections::BTreeSet<&String> = texts.iter().collect();
        assert_eq!(
            unique.len(),
            texts.len(),
            "two states sharing a reading would make the status element lie"
        );
    }

    #[test]
    fn an_empty_code_is_silent_but_a_wrong_one_speaks() {
        assert!(code_error_text(PeerCodeError::Empty).is_none());
        for e in [PeerCodeError::Malformed, PeerCodeError::OwnCode] {
            let t = code_error_text(e).expect("a wrong code speaks");
            assert!(
                !t.contains("folders.offline_share_code"),
                "a raw i18n key reached the surface: {t}"
            );
        }
    }

    /// Teardown leaves a fresh, unbound seat behind — the in-memory half of
    /// the switch isolation contract `actor_scope` owns, and the property
    /// `p2p.md`'s wormability rule 5 rests on ("no listener when off").
    /// Deliberately no real `CeremonySeat` here: binding one opens a real
    /// iroh listener, and [`reset_for_actor_change`] resets the whole struct
    /// regardless of whether its `SessionSeat` was ever bound — so a plain
    /// non-default state is enough to pin that the registered hook actually
    /// runs and leaves the default, unbound seat behind.
    #[test]
    fn reset_for_actor_change_leaves_a_fresh_unbound_seat() {
        use fauna_client_capabilities::group_ceremony_view::OfflineSharePanel;

        let state = Rc::new(RefCell::new(OfflineShareState {
            panel: OfflineSharePanel::Initiate,
            peer_code_input: "some-code".to_string(),
            ..OfflineShareState::default()
        }));
        {
            let state = Rc::clone(&state);
            register_reset_hook(move || {
                *state.borrow_mut() = OfflineShareState::default();
            });
        }

        reset_for_actor_change();

        assert!(
            state.borrow().session_seat.seat().is_none(),
            "teardown must leave a fresh, unbound seat behind"
        );
        assert_eq!(
            state.borrow().panel,
            OfflineSharePanel::Closed,
            "the registered hook must actually have run"
        );
    }
}
