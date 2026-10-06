//! Shared archive-import machine construction — the tui twin of what
//! `mail_glue.rs` does for the mail machines: a thin adapter of the session's
//! handles onto the shared `libs/fauna-archive-import-machine` builder
//! (priority #2; tui *is* Rust, so it calls the builder directly rather than
//! through `fauna-ffi`).

use std::sync::Arc;

use fauna_archive_import_machine::ArchiveImportMachine;
use fauna_client::NestClient;

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
