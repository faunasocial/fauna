//! The re-seed gesture's native face (`docs/goal/ui/backups.md` § Restore after
//! losing the nest; the ceremony `docs/goal/behavior/backup-destinations.md`
//! § Re-seed).
//!
//! Two things every UniFFI shell needs whichever process runs the ceremony:
//!
//! * [`backup_reseed_rows`] — where `backup-destination-reseed-button` paints,
//!   over the shared `fauna_client_backup::reseed::reseed_sources` filtered to
//!   the legs this build can run (the rule linux's and tui's `reseed_rows`
//!   apply);
//! * [`FfiReseedResult`] — what a finished (or stopped) ceremony hands back:
//!   the shared `result_lines`, the `is_whole` verdict, and the post-ceremony
//!   re-enrollment's failure.
//!
//! The two ceremony entry points live beside the store they read, because
//! *where the ceremony runs* is the one thing the platforms differ on
//! (`backup-destinations.md` § Re-seed → *Where the ceremony runs*): a desktop
//! asks its sync agent (`FfiSyncAgentProvisioner::reseed_custodian_store`), a
//! phone runs it in-process (`crate::reseed_custodian_store`). Both end in
//! [`finish`], so the order after the verdict cannot drift per shell.

use std::sync::Arc;

use fauna_client_backup::reseed::{ReseedOutcome, reseed_sources, result_lines};
use fauna_client_config::{load_backup_state, reenroll_custodian_after_reseed};
use fauna_core::data::BackupDestination;
use fauna_core::localized::LocalizedText;

use crate::FfiBackupDestinationView;
use crate::nest_client::FfiNestClient;

/// How a re-seed ended.
///
/// A record rather than an error for the stopped case, because the page renders
/// the two differently and neither is this boundary's to phrase: a stop lands on
/// `error-message` inside the `backups.backup_reseed_failed` sentence, while a
/// verdict lands on `backup-destination-reseed-result`.
#[derive(uniffi::Record)]
pub struct FfiReseedResult {
    /// Set when the ceremony stopped before its verdict — nothing was made live,
    /// and re-running resumes it. The detail the shell puts in the `{reason}`
    /// slot. When set, every other field is empty.
    pub stopped: Option<String>,
    /// `backup-destination-reseed-result`'s lines, verdict first
    /// (`fauna_client_backup::reseed::result_lines`). A line's set name is
    /// itself a key, so resolve each one **nested**.
    pub result_lines: Vec<LocalizedText>,
    /// The one bit a shell may render as "restored"
    /// (`ReseedOutcome::is_whole`), never re-derived from the lines.
    pub is_whole: bool,
    /// The post-ceremony re-enrollment failed: the data IS back, but this device
    /// is not yet the seeded nest's custodian. `None` when it ran and landed, and
    /// when the outcome was not whole (it does not run then).
    pub reenroll_error: Option<String>,
}

impl FfiReseedResult {
    /// The ceremony stopped before its verdict.
    pub(crate) fn stopped(reason: impl Into<String>) -> Self {
        Self {
            stopped: Some(reason.into()),
            result_lines: Vec::new(),
            is_whole: false,
            reenroll_error: None,
        }
    }
}

/// The ceremony's shared tail: re-enroll this device as the seeded nest's
/// custodian when the outcome is whole
/// (`fauna_client_config::reenroll_custodian_after_reseed`), then fold the
/// verdict into the record. Both entry points call it, which is what keeps the
/// post-ceremony duty from being a per-shell choice.
///
/// The destination rows the re-enrollment matches this device against are read
/// here, from the seeded box's own `fauna.state.backup` list (keyed by the box
/// this connection is bound to), rather than taken from the page: the page's
/// copy may be as old as its last refresh, and the read is the same one that
/// painted it. A failed read — the bound id or the list — is reported as the
/// re-enrollment's failure, never guessed into an empty list, which would mint
/// a second row for a device that may already have one.
pub(crate) async fn finish(
    nest: &Arc<FfiNestClient>,
    outcome: ReseedOutcome,
    device_hex: &str,
) -> FfiReseedResult {
    let lines = result_lines(&outcome);
    let is_whole = outcome.is_whole();
    let reenroll_error = if is_whole {
        reenroll(nest, &outcome, device_hex).await
    } else {
        None
    };
    FfiReseedResult {
        stopped: None,
        result_lines: lines,
        is_whole,
        reenroll_error,
    }
}

/// [`finish`]'s whole-outcome duty: the re-enrollment's failure, or `None`
/// when it ran and landed.
async fn reenroll(
    nest: &Arc<FfiNestClient>,
    outcome: &ReseedOutcome,
    device_hex: &str,
) -> Option<String> {
    let source_nest = match crate::deployment_seed::bound_nest_id(nest).await {
        Ok(id) => id.0,
        Err(e) => return Some(e.to_string()),
    };
    let store = crate::backup_seam();
    let state = match load_backup_state(store.as_ref(), source_nest).await {
        Ok(state) => state,
        Err(e) => return Some(e.to_string()),
    };
    reenroll_custodian_after_reseed(
        nest.nest_arc(),
        store.as_ref(),
        source_nest,
        outcome,
        device_hex,
        &state.backup.destinations,
        || uuid::Uuid::new_v4().to_string(),
    )
    .await
    .and_then(Result::err)
    .map(|e| e.to_string())
}

/// Where `backup-destination-reseed-button` paints: `None` for the orphaned
/// store's row (`backup-orphaned-store-row`), `Some(destination_id)` for a
/// destination row. The shared `reseed_sources` decides which rows are sources
/// at all; this keeps the ones whose delivery leg this build can run, so a
/// nest-kind row joins by its leg becoming built, never by a change in a shell.
/// linux's and tui's `reseed_rows` apply the same rule.
///
/// `this_device_id` is the stable sync device id, hex — the one a client-device
/// row names its custodian by; blank claims no row. `orphaned_store` is the
/// page's own cached orphaned-store verdict.
#[uniffi::export]
pub fn backup_reseed_rows(
    destinations: Vec<FfiBackupDestinationView>,
    this_device_id: String,
    orphaned_store: bool,
) -> Vec<Option<String>> {
    let rows: Vec<BackupDestination> = destinations.iter().map(source_row).collect();
    reseed_sources(&rows, &this_device_id, orphaned_store)
        .into_iter()
        .filter(|s| s.leg.is_built())
        .map(|s| s.destination_id)
        .collect()
}

/// The fields `reseed_sources` reads, rebuilt from the view record: the id and
/// what `BackupDestination::kind_view` classifies on. The rest stay at their
/// defaults — the view deliberately does not carry them, and no source rule
/// reads them.
fn source_row(view: &FfiBackupDestinationView) -> BackupDestination {
    BackupDestination {
        destination_id: view.destination_id.clone(),
        destination_nest_url: view.destination_nest_url.clone(),
        kind: view.kind.clone(),
        custodian_device_id: view.custodian_device_id.clone(),
        capacity_cap_bytes: view.capacity_cap_bytes,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::data::{DESTINATION_KIND_CLIENT_DEVICE, DESTINATION_KIND_NEST};

    const ME: &str = "aa11";

    fn view(id: &str, kind: &str, device: Option<&str>) -> FfiBackupDestinationView {
        FfiBackupDestinationView {
            destination_id: id.into(),
            destination_nest_url: String::new(),
            display_name: None,
            kind: kind.into(),
            custodian_device_id: device.map(str::to_string),
            capacity_cap_bytes: None,
            unattested: false,
        }
    }

    /// The orphaned store first, then this device's own custodian row — and
    /// neither another device's row (only the holder can push) nor a nest row
    /// (its pull-back leg is not built).
    #[test]
    fn the_rows_are_the_orphaned_store_and_this_devices_own_row_only() {
        let rows = vec![
            view("mine", DESTINATION_KIND_CLIENT_DEVICE, Some(ME)),
            view("theirs", DESTINATION_KIND_CLIENT_DEVICE, Some("bb22")),
            view("off-site", DESTINATION_KIND_NEST, None),
        ];
        assert_eq!(
            backup_reseed_rows(rows.clone(), ME.into(), true),
            vec![None, Some("mine".to_string())]
        );
        assert_eq!(
            backup_reseed_rows(rows.clone(), ME.into(), false),
            vec![Some("mine".to_string())]
        );
        // A device that cannot name itself offers only an orphaned store.
        assert!(backup_reseed_rows(rows, String::new(), false).is_empty());
    }
}
