//! Native (apple/windows/android) assembly of the cross-device MLS state-sync
//! plane — the thin adapter the FFI `conversations_session` factory calls to
//! build the **shared tokio launcher**
//! ([`fauna_client_mls_sync::launcher::tokio_launcher`], run once before the
//! first poll by `start_receive_loop`; `docs/goal/behavior/devices.md`
//! § Cross-device MLS group-state sync, slice 5).
//!
//! The restore/save logic is `fauna_client_mls_sync::orchestration`; the
//! trigger (restore-with-retry → post-restore hook → debounced autosave) is the
//! shared launcher, consumed identically by tui's `conv_backend.rs`; and the
//! launch-time folder removal resume is the shared
//! [`FolderRemovalResume`] hook (one orchestration, one tokio trigger, one
//! hook body — priority #2). This module owns only what is genuinely this
//! build's: the concrete `NestMlsReplicaTransport` over the factory's
//! connection, and the feature split below.
//!
//! **Feature split:** the hook needs the `folders-author` graph
//! (`fauna-client-folders/mls`). The Go mail-bridge `--no-default-features`
//! build drops it and passes `None` — a server with no share UI stages no
//! removals (the launcher documents `None` as exactly this shape). Leg 3's
//! progress sink needs `recovery-aftermath` (`crate::succession_aftermath`'s
//! own feature) for the same reason and takes the same two-variant shape —
//! independent of `folders-author`, since a build could in principle carry
//! one without the other.

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_conversations::NestMlsReplicaTransport;
use fauna_client_mls_sync::launcher::{PostRestoreHook, tokio_launcher};
use fauna_conversations::ConversationsManager;
use fauna_conversations::backend::MlsSyncLauncher;
use fauna_conversations::backends::fauna_mls::FaunaMlsBackend;

/// Build the native cross-device MLS state-sync launcher over `nest` + the
/// session's `backend`/`manager`/`conv`, ready to hand to
/// [`fauna_conversations::ConversationsSession::set_mls_sync_launcher`]. `None`
/// when `self_secret` is not a 32-byte seed — the plane stays disabled and the
/// client runs single-device (the linux `keypair_from_hex(...).map(...)`
/// shape); the factory has already rejected a malformed secret, so this is a
/// defensive belt-and-suspenders.
///
/// `predecessors` carries the retired identities' `BackupKey`s the caller
/// resolved off its account registry (`FfiAccountRegistry::predecessor_backup_keys`,
/// converted from wire bytes by the caller) — empty for every identity that
/// never succeeded, which costs nothing (tui's `conv_backend.rs` is the
/// reference resolution).
///
/// `aftermath_sink` is `FfiNestClient`'s late-populated holder — the SAME one
/// [`crate::succession_aftermath::run_succession_aftermath`] registers onto
/// (`FfiNestClient::set_aftermath_sink`) — so leg 3's progress reaches the
/// app's `FfiAftermathSink` no matter which of the two builds first: the
/// closure below re-reads the holder at the moment the reseal itself reports,
/// not at this call's own time .
#[cfg(feature = "recovery-aftermath")]
pub(crate) fn mls_sync_launcher(
    nest: Arc<NestClient>,
    self_secret: &[u8],
    backend: Arc<FaunaMlsBackend>,
    manager: Arc<ConversationsManager>,
    predecessors: Vec<fauna_client_mls_sync::BackupKey>,
    aftermath_sink: Arc<
        std::sync::Mutex<Option<Arc<dyn crate::succession_aftermath::FfiAftermathSink>>>,
    >,
    recording_device: Option<String>,
) -> Option<Arc<dyn MlsSyncLauncher>> {
    tokio_launcher(
        Box::new(NestMlsReplicaTransport::new(Arc::clone(&nest))),
        self_secret,
        backend,
        manager,
        post_restore_hook(&nest, self_secret, recording_device),
        fauna_client_mls_sync::SuccessionReseal {
            predecessors,
            sink: Some(Box::new(move |progress| {
                // Cloned out of the lock before the call: the app's sink runs arbitrary
                // foreign code, and holding the mutex across it would invite
                // a deadlock against a concurrent `set_aftermath_sink`.
                let sink = aftermath_sink.lock().unwrap().clone();
                if let Some(sink) = sink {
                    sink.progress(
                        crate::succession_aftermath::FfiAftermathLeg::MlsReseal,
                        progress.status_line(),
                    );
                }
            })),
        },
    )
}

/// No-sink twin for a build with no `FfiAftermathSink` type at all
/// (`recovery-aftermath` off, e.g. the Go mail-bridge) — leg 3 still runs
/// (predecessors, if any, still re-seal the replica) but reports to nobody,
/// mirroring `sink: None`'s original shape before this row.
#[cfg(not(feature = "recovery-aftermath"))]
pub(crate) fn mls_sync_launcher(
    nest: Arc<NestClient>,
    self_secret: &[u8],
    backend: Arc<FaunaMlsBackend>,
    manager: Arc<ConversationsManager>,
    predecessors: Vec<fauna_client_mls_sync::BackupKey>,
    recording_device: Option<String>,
) -> Option<Arc<dyn MlsSyncLauncher>> {
    tokio_launcher(
        Box::new(NestMlsReplicaTransport::new(Arc::clone(&nest))),
        self_secret,
        backend,
        manager,
        post_restore_hook(&nest, self_secret, recording_device),
        fauna_client_mls_sync::SuccessionReseal {
            predecessors,
            sink: None,
        },
    )
}

/// The launch-time folder removal resume — the shared hook body
/// (`fauna_client_folders::FolderRemovalResume`; its doc carries the full
/// gated/ungated rationale). `recording_device` wires the served-set walk's
/// launch resume into the same pass (`webdav-server.md` § Key model (c)).
/// `None` on a malformed secret (the launcher rejects the same secret one
/// line later).
#[cfg(feature = "folders-author")]
fn post_restore_hook(
    nest: &Arc<NestClient>,
    self_secret: &[u8],
    recording_device: Option<String>,
) -> Option<Arc<dyn PostRestoreHook>> {
    let seed: [u8; 32] = self_secret.try_into().ok()?;
    Some(Arc::new(
        fauna_client_folders::FolderRemovalResume::new(
            Arc::clone(nest),
            seed,
            crate::account_runtime::folder_key_store(),
            crate::account_runtime::mail_store(),
            crate::ledger_seam(),
        )
        .with_recording_device(recording_device),
    ))
}

/// No-op twin for the `--no-default-features` (Go mail-bridge) build, which
/// drops the folders author entirely — a server with no share UI stages no
/// removals.
#[cfg(not(feature = "folders-author"))]
fn post_restore_hook(
    _nest: &Arc<NestClient>,
    _self_secret: &[u8],
    _recording_device: Option<String>,
) -> Option<Arc<dyn PostRestoreHook>> {
    None
}
