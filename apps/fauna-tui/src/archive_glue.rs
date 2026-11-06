//! Shared archive-import machine construction — the tui twin of what
//! `mail_glue.rs` does for the mail machines: a thin adapter of the session's
//! handles onto the shared `libs/fauna-archive-import-machine` builder
//! (priority #2; tui *is* Rust, so it calls the builder directly rather than
//! through `fauna-ffi`).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use fauna_archive_import_machine::ArchiveImportMachine;
use fauna_archive_import_machine::link::{ArchiveLinks, SessionPoster};
use fauna_client::NestClient;
use tokio::sync::mpsc::UnboundedSender;

use crate::app::{DataMessage, UiMessage};

/// Build the `ArchiveImportMachine` for the authenticated actor `actor_id_hex`
/// (the user-facing `archive-import` wizard). Needs that account's sync device
/// id — the archive folder's files are chunked owner-sealed change records
/// stamped with it, the same id the app's other sync surfaces use
/// (`crate::media::device_id_hex`).
/// `Err` on a session that holds no identity key (bearer-only), or no device id
/// — or one this app cannot read back; the page then paints
/// `archive_import::UNAVAILABLE` instead of a wizard that could not commit.
///
/// The hex → `[u8; 32]` step is `fauna_core::hex32::decode`, the crate's one
/// canonical 32-byte decoder (`hex32.rs`'s own module doc: consumers used to
/// hand-roll `hex::decode(s).try_into::<[u8; 32]>()` with per-call-site error
/// handling, which is exactly how the parse drifts). It is also the exact
/// inverse of `hex32::encode`, which is what `device_id_hex` minted this string
/// with — and how `backups.rs` decodes the very same value.
pub fn build_archive_import_machine(
    nest: Arc<NestClient>,
    actor_id_hex: &str,
    period_keys: fauna_client_subscriptions::SharedPeriodKeyStore,
    folder_keys: std::sync::Arc<dyn fauna_client_folders::FolderKeyStore>,
    mail: Arc<dyn fauna_client_config::MailStore>,
) -> Result<ArchiveImportMachine, String> {
    let hex_id =
        crate::media::device_id_hex(actor_id_hex).ok_or_else(|| "no sync device id".to_string())?;
    let device_id = fauna_core::hex32::decode(&hex_id).map_err(|e| e.to_string())?;
    fauna_archive_import_machine::rpc_glue::build_archive_import_machine(
        nest,
        device_id,
        period_keys,
        folder_keys,
        mail,
    )
    .map_err(|e| e.to_string())
}

// ── Phase two: the archive links (`archive-import.md` § Linking and merging) ──
//
// Everything below is wiring: the protocol, the state and every line the
// Contacts row paints are the machine crate's `link` module and `text`
// composers.

/// Build the account's [`ArchiveLinks`] over the archive seam the login wired
/// and register it as the session's archive-signal sink (ruling 2, beside
/// the custody sink). The poster holds the session weakly — `SessionPoster`
/// says why.
pub fn wire_archive_links(
    session: &Arc<fauna_conversations::ConversationsSession>,
    archive: Arc<dyn fauna_archive_import_machine::nest::ArchiveNest>,
    keypair: &fauna_core::identity::ActorKeypair,
) -> Arc<ArchiveLinks> {
    let links = Arc::new(ArchiveLinks::new(
        archive,
        Arc::new(SessionPoster::new(session)),
        keypair,
    ));
    session.set_archive_signal_sink(Arc::clone(&links) as Arc<_>);
    links
}

/// One drive pass in the background (ruling 4: pending sends re-driven, due
/// indices and claim runs done) — at session start and whenever the sink
/// left work owed. A pass already in flight absorbs the request: the next
/// tick re-checks `work_due`.
pub fn spawn_links_drive(
    links: Arc<ArchiveLinks>,
    in_flight: Arc<AtomicBool>,
    tx: UnboundedSender<UiMessage>,
) {
    if in_flight.swap(true, Ordering::AcqRel) {
        return;
    }
    tokio::spawn(async move {
        if let Err(e) = links.drive().await {
            tracing::warn!("archive links: drive: {e}");
        }
        in_flight.store(false, Ordering::Release);
        let _ = tx.send(UiMessage::Data(DataMessage::ArchiveLinksMoved));
    });
}

/// What a Start or Resume carries into its spawned run: the links the
/// machine announces with when the run completes (rulings 4 and 11 ride the
/// run itself — `ArchiveImportMachine::attach_links`), and the repaint
/// afterwards, whichever page the user is on by then.
pub struct LinkRound {
    links: Arc<ArchiveLinks>,
    tx: UnboundedSender<UiMessage>,
    session_generation: u64,
}

impl std::fmt::Debug for LinkRound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LinkRound")
    }
}

impl LinkRound {
    /// `None` when this login wired no links (no conversations session, or
    /// no archive seam): the import then finishes with no round.
    pub fn of(app: &crate::app::App) -> Option<Self> {
        Some(Self {
            links: app.conversations.archive_links.clone()?,
            tx: app.tx.clone(),
            session_generation: app.session_generation,
        })
    }

    /// Hand the links to the machine before its run is spawned, so the run
    /// that completes counts them onto the Done step and announces its
    /// platform itself.
    pub fn attach(&self, machine: &ArchiveImportMachine) {
        machine.attach_links(Arc::clone(&self.links));
    }

    /// After a completed run (its round already done inside the run): a
    /// repaint of the Done step and the Contacts rows.
    pub fn after_run(self, machine: Arc<ArchiveImportMachine>) {
        crate::settings::spawn_archive_import_progress_refresh(
            machine,
            &self.tx,
            self.session_generation,
        );
        let _ = self
            .tx
            .send(UiMessage::Data(DataMessage::ArchiveLinksMoved));
    }
}
