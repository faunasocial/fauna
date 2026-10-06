//! The T16 custody-ceremony glue's **tui-specific** half — the observer that
//! bridges a ceremony-moved edge onto this app's message loop.
//!
//! Everything else moved to shared Rust 2026-08-17:
//! [`spawn_drive`] and the pre-store `CustodyRegistryWriter` stand-in now live
//! in `fauna-client-custody`, the crate above both `fauna-sync-engine` and
//! `fauna-client-conversations`, so the other six app legs assemble the same
//! doors instead of each re-deriving them. Re-exported here under the original
//! path so this app's call sites (`settings::mod`, `automation`) are unchanged.
//!
//! What stays is genuinely per-app: an observer is a binding onto *this* app's
//! event loop, and each shell has its own.

use fauna_client_conversations::CustodyCeremonyObserver;
use tokio::sync::mpsc::UnboundedSender;

use crate::app::{DataMessage, UiMessage};

pub use fauna_client_custody::spawn_drive;

/// The ceremony-moved edge → one drive pass. Fired by the production sink
/// whenever an ingest actually advanced ceremony state; the app's message loop
/// answers with [`spawn_drive`]. Notifies only — never works inline (the
/// observer runs in the poll's async context).
pub struct TuiCeremonyObserver {
    pub tx: UnboundedSender<UiMessage>,
}

impl CustodyCeremonyObserver for TuiCeremonyObserver {
    fn ceremony_moved(&self, _grant_id: &[u8]) {
        // A closed channel means the app is shutting down — the next session
        // start re-drives from durable state.
        let _ = self
            .tx
            .send(UiMessage::Data(DataMessage::CustodyCeremonyMoved));
    }
}
