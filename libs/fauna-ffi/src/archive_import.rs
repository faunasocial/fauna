//! The `archive-import` page's machine on the native apps
//! (`docs/goal/behavior/archive-import.md` § The wizard and its machine): the
//! shared `fauna_archive_import_machine::ArchiveImportMachine`, re-exported so
//! its UniFFI face (snapshot / hydrate / dispatch / run_import, the snapshot
//! records, the action enum and the page's vocabulary fns) surfaces in the
//! generated Swift / Kotlin / C# bindings, plus the one free-fn constructor
//! that builds it over an [`FfiNestClient`]'s WS-RPC connection.
//!
//! A thin glue layer in the `src/mail_admin.rs` shape: the machine, the
//! parser, the folder writes and the authoring are shared Rust; an app builds
//! the machine here, renders its snapshot, dispatches its actions, and spawns
//! `run_import` after a `Start` or `Resume` that leaves the run `Running` —
//! the machine never spawns its own loop (`MailImportMachine`'s contract).
//! tui builds the same machine natively through the same
//! `rpc_glue::build_archive_import_machine` (`apps/fauna-tui/src/archive_glue.rs`).

use std::sync::Arc;

pub use fauna_archive_import_machine::{
    ArchiveImportAction, ArchiveImportDispatchError, ArchiveImportMachine, ArchiveImportSnapshot,
    ArchiveImportStatus, ArchiveImportStep, ArchiveSourceKind, ArchiveSummaryView, AudienceMode,
    CategoryRow, RunState,
};

use crate::{FfiError, FfiNestClient, general_err};

/// Build the [`ArchiveImportMachine`] for the user-facing `archive-import`
/// wizard over `nest`'s WS-RPC connection, for the signed-in actor whose sync
/// device id is `device_id_hex` — the archive folder's files are chunked
/// owner-sealed change records stamped with it, the same id the app's other
/// sync surfaces use. Every imported post is signed with the connection's own
/// identity key (§ Architectural rules — signed by its owner).
///
/// The machine's three custody doors are this seat's account runtime, each
/// resolved at every call (so a page built before the runtime lands reads
/// through once it does): the period keys a gated import mints into, the
/// folder keys the archive folder seals under, and the mail custody the
/// imported events' organizer address is read from.
///
/// `Err` on a bearer-only session (no identity key — the page then states it
/// cannot import rather than painting a wizard that could not commit) or a
/// device id that is not 64 hex characters.
#[uniffi::export]
pub fn build_archive_import_machine(
    nest: Arc<FfiNestClient>,
    device_id_hex: String,
) -> Result<Arc<ArchiveImportMachine>, FfiError> {
    let device_id = fauna_core::hex32::decode(&device_id_hex)
        .map_err(|e| general_err(format!("the archive import's device id: {e}")))?;
    fauna_archive_import_machine::rpc_glue::build_archive_import_machine(
        nest.nest_arc(),
        device_id,
        crate::account_runtime::period_key_store(),
        crate::account_runtime::folder_key_store(),
        crate::account_runtime::mail_store(),
    )
    .map(Arc::new)
    .map_err(|e| general_err(format!("build the archive-import machine: {e}")))
}

/// The late-populated slot [`wire_archive_seams`]'s links land in on
/// `FfiNestClient` (the `SchedulingSessionHolder` shape).
#[cfg(feature = "conversations-session")]
pub(crate) type ArchiveLinksHolder =
    Arc<std::sync::Mutex<Option<Arc<fauna_archive_import_machine::link::ArchiveLinks>>>>;

/// Register the conversations session's two archive seams over one folder
/// reader, built on the same connection and custody
/// [`build_archive_import_machine`] uses — what tui and linux wire in their
/// own `conv_backend`. Called once by the conversations factory
/// (`FfiNestClient::conversations_session`), so the four FFI legs — macOS,
/// iOS, windows, android — inherit both with no per-app glue:
///
/// - the `Archive` rail's read seam (`archive-import.md` § What each category
///   becomes, the message-threads row): the machine crate's `ArchiveThreads`.
///   Registering reads nothing: the app calls [`refresh_archive_threads`] at
///   login and on every visit to the conversations page.
/// - phase two's links (§ Linking and merging, ruling 2): an `ArchiveLinks`
///   registered as the session's archive-signal sink, its poster holding the
///   session weakly (`SessionPoster`). Returned for the factory to stash;
///   the app drives it (this is a synchronous export with no runtime of its
///   own, so nothing is driven here).
///
/// `recording_device` is the app's sync device id (hex) — the folder
/// reader's chunk store needs one. `None`, a malformed id or a bearer-only
/// session leaves both unset: the rail lists nothing and no row asks (tui's
/// posture when it has no device id).
#[cfg(feature = "conversations-session")]
pub(crate) fn wire_archive_seams(
    session: &Arc<fauna_conversations::ConversationsSession>,
    nest: Arc<fauna_client::NestClient>,
    recording_device: Option<&str>,
    keypair: &fauna_core::identity::ActorKeypair,
) -> Option<Arc<fauna_archive_import_machine::link::ArchiveLinks>> {
    use fauna_archive_import_machine::link::{ArchiveLinks, SessionPoster};
    let device_id = recording_device.and_then(|hex| fauna_core::hex32::decode(hex).ok())?;
    match fauna_archive_import_machine::rpc_glue::build_archive_nest(
        nest,
        device_id,
        crate::account_runtime::period_key_store(),
        crate::account_runtime::folder_key_store(),
        crate::account_runtime::mail_store(),
    ) {
        Ok(archive) => {
            let links = Arc::new(ArchiveLinks::new(
                Arc::clone(&archive),
                Arc::new(SessionPoster::new(session)),
                keypair,
            ));
            session.set_archive_signal_sink(Arc::clone(&links) as Arc<_>);
            fauna_archive_import_machine::threads::register_archive_threads(
                &session.manager(),
                archive,
            );
            Some(links)
        }
        Err(e) => {
            tracing::warn!("conversations: no archive source: {e}");
            None
        }
    }
}

/// Re-read the imported archives' message threads into the session's
/// read-only `Archive` projection (`ConversationsManager::
/// refresh_archive_threads`) — called at login and on every visit to the
/// conversations page, so an import finished since login lists without a
/// re-login (tui's `App::set_page`). A no-op when the factory registered no
/// source; a failed read keeps what the last read listed.
#[cfg(feature = "conversations-session")]
#[fauna_uniffi_async::export]
pub async fn refresh_archive_threads(
    session: Arc<fauna_conversations::ConversationsSession>,
) -> Result<(), FfiError> {
    session
        .manager()
        .refresh_archive_threads()
        .await
        .map_err(|e| general_err(format!("read the archive threads: {e}")))
}

/// Register the feed manager's folded-archive-comments read seam
/// (`archive-import.md` § What each category becomes → *Placement under the
/// owner's own post*): the machine crate's `ArchiveComments` over the archive
/// folders this seat reads, on the same connection and custody
/// [`wire_archive_seams`] uses. Called once by the feed factory
/// (`FfiNestClient::feed_manager`), so the four FFI legs fold an import's
/// comments under the owner's own posts with no per-app glue — tui wires the
/// same source in its `feed::wire_archive_comments`.
///
/// `recording_device` is the app's sync device id (hex), as for the rail.
/// `None`, a malformed id or a bearer-only session leaves the source unset,
/// and nothing folds. Registering reads nothing: the app calls
/// `FfiFeedManager::refresh_archive_comments` on every visit to the feed.
#[cfg(feature = "feed-manager")]
pub(crate) fn wire_archive_comments(
    manager: &fauna_feed::FeedManager<Arc<fauna_client::NestClient>>,
    nest: Arc<fauna_client::NestClient>,
    recording_device: Option<&str>,
) {
    let Some(device_id) = recording_device.and_then(|hex| fauna_core::hex32::decode(hex).ok())
    else {
        return;
    };
    match fauna_archive_import_machine::rpc_glue::build_archive_nest(
        nest,
        device_id,
        crate::account_runtime::period_key_store(),
        crate::account_runtime::folder_key_store(),
        crate::account_runtime::mail_store(),
    ) {
        Ok(archive) => {
            fauna_archive_import_machine::comments::register_archive_comments(manager, archive)
        }
        Err(e) => tracing::warn!("feed: no archive comment source: {e}"),
    }
}
