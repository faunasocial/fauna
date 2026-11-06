//! Shared archive-import construction — linux's twin of tui's `archive_glue.rs`
//! (what `mail_glue.rs` is for the mail machines): a thin adapter of this
//! seat's handles onto the shared `libs/fauna-archive-import-machine` builders
//! (priority #2; linux is Rust, so it calls them directly rather than through
//! `fauna-ffi`).
//!
//! Both builders need the account's sync device id — the archive folder's files
//! are chunked owner-sealed change records stamped with it — and the account
//! runtime's period-key, folder-key and mail custody, each resolved here so no
//! call site re-spells the five inputs.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use fauna_archive_import_machine::ArchiveImportMachine;
use fauna_archive_import_machine::ArchiveNest;
use fauna_archive_import_machine::link::{ArchiveLinks, SessionPoster};
use fauna_client::NestClient;

/// This device's sync id for `actor_id_hex`, decoded — the same get-or-create
/// the rest of the app's sync surfaces read (`crate::sync::device_id_hex_for`),
/// through the workspace's one canonical 32-byte decoder.
fn device_id(actor_id_hex: &str) -> Result<[u8; 32], String> {
    let hex = crate::sync::device_id_hex_for(actor_id_hex)
        .ok_or_else(|| "no sync device id".to_string())?;
    fauna_core::hex32::decode(&hex).map_err(|e| e.to_string())
}

/// Build the `ArchiveImportMachine` for the authenticated actor `actor_id_hex`
/// (the user-facing `archive-import` wizard). `Err` on a session that holds no
/// identity key (bearer-only) or no readable device id; the page then paints
/// its unavailable sentence instead of a wizard that could not commit.
pub fn build_archive_import_machine(
    nest: Arc<NestClient>,
    actor_id_hex: &str,
) -> Result<ArchiveImportMachine, String> {
    fauna_archive_import_machine::rpc_glue::build_archive_import_machine(
        nest,
        device_id(actor_id_hex)?,
        crate::account_runtime::period_key_store(),
        crate::account_runtime::folder_key_store(),
        crate::account_runtime::mail_store(),
    )
    .map_err(|e| e.to_string())
}

/// The wizard's own archive seam, alone — what the `Archive` rail's thread
/// source reads the imported threads through (`conv_backend`).
pub fn build_archive_nest(
    nest: Arc<NestClient>,
    actor_id_hex: &str,
) -> Result<Arc<dyn ArchiveNest>, String> {
    fauna_archive_import_machine::rpc_glue::build_archive_nest(
        nest,
        device_id(actor_id_hex)?,
        crate::account_runtime::period_key_store(),
        crate::account_runtime::folder_key_store(),
        crate::account_runtime::mail_store(),
    )
    .map_err(|e| e.to_string())
}

// ── Phase two: the archive links (`archive-import.md` § Linking and merging) ──
//
// Everything below is wiring — tui's `archive_glue.rs` twin: the protocol, the
// state and every line the Contacts row paints are the machine crate's `link`
// module and `text` composers.

/// Set when something outside the tick moved the links' state (a completed
/// import's announce round): the tick re-reads the views on its next second.
static VIEWS_DIRTY: AtomicBool = AtomicBool::new(false);

/// How often the links tick looks at `work_due` / `captures` — two atomic
/// loads, cheap (tui's `archive_links_tick`).
const LINKS_TICK: std::time::Duration = std::time::Duration::from_secs(1);

/// Build the account's [`ArchiveLinks`] over the archive seam the login wired
/// and register it as the session's archive-signal sink (ruling 2, beside the
/// custody sink). The poster holds the session weakly — `SessionPoster` says
/// why.
pub fn wire_archive_links(
    session: &Arc<fauna_conversations::ConversationsSession>,
    archive: Arc<dyn ArchiveNest>,
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

/// The links' one background task for this login: a drive pass now (ruling 4:
/// pending sends re-driven at session start), then every second a drive pass
/// whenever the sink left work owed, and a re-read of the views — sent to the
/// window as [`crate::app::DataMessage::ArchiveLinkViews`] — whenever a drive
/// ran, a question was captured, or [`mark_views_dirty`] was called. Holds the
/// links weakly, so the next login (which replaces them) ends it. One task, so
/// a drive pass never runs twice at once.
pub fn spawn_links_tick(
    links: &Arc<ArchiveLinks>,
    runtime: &tokio::runtime::Handle,
    tx: crate::client::UiSender,
) {
    let weak: Weak<ArchiveLinks> = Arc::downgrade(links);
    runtime.spawn(async move {
        let mut tick = tokio::time::interval(LINKS_TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut seen: Option<u64> = None;
        let mut drive = true;
        loop {
            let Some(links) = weak.upgrade() else {
                return;
            };
            let mut repaint = VIEWS_DIRTY.swap(false, Ordering::AcqRel);
            if drive || links.work_due() {
                if let Err(e) = links.drive().await {
                    tracing::warn!("archive links: drive: {e}");
                }
                repaint = true;
            }
            drive = false;
            let captures = links.captures();
            if seen != Some(captures) {
                seen = Some(captures);
                repaint = true;
            }
            if repaint {
                send_views(&links, &tx).await;
            }
            drop(links);
            tick.tick().await;
        }
    });
}

/// Ask the links tick to re-read the views on its next second — after a run
/// whose completion announced its platform (the run's own round).
pub fn mark_views_dirty() {
    VIEWS_DIRTY.store(true, Ordering::Release);
}

async fn send_views(links: &ArchiveLinks, tx: &crate::client::UiSender) {
    match links.views().await {
        Ok(views) => tx.send(crate::app::UiMessage::Data(
            crate::app::DataMessage::ArchiveLinkViews { views },
        )),
        Err(e) => tracing::warn!("archive links: views: {e}"),
    }
}

/// A gesture on a Contacts row's archive-link affordances
/// (`contact-archive-link-yes` / `-no`, `contact-archive-unlink`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkGesture {
    /// Answer the row's same-person question — once, either way.
    Answer(bool),
    /// Unlink a linked contact: future merges stop, claimed content stays.
    Unlink,
}

/// Run `gesture` for the row's `(contact, platform)` through the shared calls,
/// then repaint the rows from the views it leaves. A no-op on a login that
/// wired no links; a refusal reaches the window as a failed action.
pub fn run_link_gesture(
    contact: String,
    platform: String,
    gesture: LinkGesture,
    tx: crate::client::UiSender,
) {
    let Some((links, runtime)) = crate::conversations::conv_backend::archive_links() else {
        return;
    };
    runtime.spawn(async move {
        let done = match gesture {
            LinkGesture::Answer(yes) => links.answer_link_token(contact, platform, yes).await,
            LinkGesture::Unlink => links.unlink_token(contact, platform).await,
        };
        if let Err(e) = done {
            tx.send(crate::app::UiMessage::Action(
                crate::app::ActionResult::Failed {
                    context: "archive-link".into(),
                    error: e.to_string(),
                },
            ));
        }
        send_views(&links, &tx).await;
    });
}
