//! The re-seed gesture's native face (`docs/goal/ui/backups.md` § Restore after
//! losing the nest; the ceremony `docs/goal/behavior/backup-destinations.md`
//! § Re-seed).
//!
//! Two things every UniFFI shell needs whichever process runs the ceremony:
//!
//! * [`backup_reseed_rows`] — where `backup-destination-reseed-button` paints:
//!   the shared `fauna_client_backup::reseed::reseed_rows` over the legs the
//!   UniFFI shells host (the rule linux and tui paint by too);
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
//!
//! The second restore source, a surviving nest-kind destination, has one
//! entry point on every platform and lives here: the nest-held pull-back reads
//! no custodian store, so it always runs in the app
//! ([`backup_pull_back_from_destination`], over the shared host).

use std::sync::Arc;

use fauna_client_backup::reseed::{HostedLegs, ReseedOutcome, result_lines};
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
/// destination row — the shared `reseed_rows`, the rule linux and tui paint by,
/// over the legs the calling shell hosts.
///
/// `this_device_id` is the stable sync device id, hex — the one a client-device
/// row names its custodian by; blank claims no row. `orphaned_store` is the
/// page's own cached orphaned-store verdict. `hosts_pull_back` is the shell's
/// own statement that its confirm runs [`backup_pull_back_from_destination`]
/// on a nest-kind row; a shell that has not wired it passes `false` and paints
/// no button there, so a nest row never carries one with nothing behind it.
#[uniffi::export]
pub fn backup_reseed_rows(
    destinations: Vec<FfiBackupDestinationView>,
    this_device_id: String,
    orphaned_store: bool,
    hosts_pull_back: bool,
) -> Vec<Option<String>> {
    let rows: Vec<BackupDestination> = destinations.iter().map(source_row).collect();
    fauna_client_backup::reseed::reseed_rows(
        &rows,
        &this_device_id,
        orphaned_store,
        HostedLegs {
            nest_pull_back: hosts_pull_back,
        },
    )
}

/// Whether the confirm armed on `destination_id`'s row runs the nest-held
/// pull-back ([`backup_pull_back_from_destination`]) rather than the custodian
/// ceremony — the leg the shared `reseed_sources` types that row with, read at
/// press time so no shell re-derives it from the row's kind. `false` for an
/// unknown id and for every row that is not a pull-back source.
#[uniffi::export]
pub fn backup_reseed_runs_pull_back(
    destinations: Vec<FfiBackupDestinationView>,
    destination_id: String,
) -> bool {
    let rows: Vec<BackupDestination> = destinations.iter().map(source_row).collect();
    fauna_client_backup::reseed::reseed_runs_pull_back(&rows, &destination_id)
}

/// Restore the signed-in nest from a surviving **nest-kind** destination's
/// copy — the confirm of `backup-destination-reseed-button` on that
/// destination's `backup-destination-status-row` (`ui/backups.md` § Restore
/// after losing the nest; the leg `client-device-custodian.md` § *The
/// nest-held pull-back*).
///
/// A thin face over `fauna_sync_engine::reseed_pull::pull_back_from_destination`,
/// the one native composition tui and linux call directly: this resolves its
/// inputs and projects its result, and decides nothing — the order, the folder
/// names, the post-ceremony re-enrollment and the verdict are all the host's.
/// It runs in this process on every platform (no custodian store is involved).
///
/// * `nest` — the owner's keyed connection to the rebuilt nest, the target;
/// * `owner_secret` — the owner's 32-byte identity seed;
/// * `device_id` — this device's stable sync id (32 bytes), registered
///   write-capable on the target;
/// * `destination_id` — the pressed row, resolved against the client's **own
///   pinned** destination list (as `backup_recover_from_destination` resolves
///   it), never a URL the shell hands over.
///
/// A run that stopped before the driver's verdict comes back as
/// [`FfiReseedResult::stopped`], as the custodian ceremony's does: nothing was
/// made live, and a re-run resumes by content address. A whole outcome whose
/// re-enrollment failed carries it in `reenroll_error` — the shell renders
/// `backups.backup_reseed_pull_back_reenroll_failed` naming the destination.
///
/// # Errors
///
/// `FfiError::General` only for inputs that never reach the host: a secret or
/// device id of the wrong length.
#[cfg(feature = "sync-engine-host")]
#[fauna_uniffi_async::export]
pub async fn backup_pull_back_from_destination(
    nest: Arc<FfiNestClient>,
    owner_secret: Vec<u8>,
    device_id: Vec<u8>,
    destination_id: String,
) -> Result<FfiReseedResult, crate::FfiError> {
    let secret = crate::crypto::secret32(&owner_secret)?;
    let device_id = crate::crypto::device32(&device_id)?;
    let source_nest = match crate::deployment_seed::bound_nest_id(&nest).await {
        Ok(id) => id.0,
        Err(e) => return Ok(FfiReseedResult::stopped(e.to_string())),
    };
    let store = crate::backup_seam();
    let state = match load_backup_state(store.as_ref(), source_nest).await {
        Ok(state) => state,
        Err(e) => return Ok(FfiReseedResult::stopped(e.to_string())),
    };
    let Some(destination) = state
        .backup
        .destinations
        .iter()
        .find(|d| d.destination_id == destination_id)
    else {
        return Ok(FfiReseedResult::stopped(format!(
            "no backup destination {destination_id}"
        )));
    };
    let ran = fauna_sync_engine::reseed_pull::pull_back_from_destination(
        fauna_sync_engine::reseed_pull::NativePullBack {
            target: nest.nest_arc(),
            owner_secret: secret,
            device_id,
            destination,
            store: store.as_ref(),
            folder_keys: crate::account_runtime::folder_key_store(),
        },
    )
    .await;
    Ok(pull_back_result(ran))
}

/// Project the pull-back host's answer to the re-seed's FFI face. Split out so
/// the projection is unit-testable without a nest.
#[cfg(feature = "sync-engine-host")]
pub(crate) fn pull_back_result(
    ran: Result<fauna_sync_engine::reseed_pull::PullBackResult, String>,
) -> FfiReseedResult {
    match ran {
        Err(reason) => FfiReseedResult::stopped(reason),
        Ok(ran) => FfiReseedResult {
            stopped: None,
            result_lines: result_lines(&ran.outcome),
            is_whole: ran.outcome.is_whole(),
            reenroll_error: ran.reenroll_error,
        },
    }
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
    /// never another device's row (only the holder can push). A nest row
    /// carries the button only for a shell that hosts its pull-back.
    #[test]
    fn the_rows_are_the_orphaned_store_and_this_devices_own_row_only() {
        let rows = vec![
            view("mine", DESTINATION_KIND_CLIENT_DEVICE, Some(ME)),
            view("theirs", DESTINATION_KIND_CLIENT_DEVICE, Some("bb22")),
            view("off-site", DESTINATION_KIND_NEST, None),
        ];
        assert_eq!(
            backup_reseed_rows(rows.clone(), ME.into(), true, false),
            vec![None, Some("mine".to_string())]
        );
        assert_eq!(
            backup_reseed_rows(rows.clone(), ME.into(), false, false),
            vec![Some("mine".to_string())]
        );
        // A device that cannot name itself offers only an orphaned store.
        assert!(backup_reseed_rows(rows, String::new(), false, false).is_empty());
    }

    /// The shell's own `hosts_pull_back` is what puts the button on a nest
    /// row: stated, the nest row joins in row order; withheld, it never does.
    ///
    /// Mutation check: passing `HostedLegs::CUSTODIAN_ONLY` regardless reds the
    /// first assertion.
    #[test]
    fn a_nest_row_carries_the_button_only_for_a_shell_hosting_the_pull_back() {
        let rows = vec![
            view("mine", DESTINATION_KIND_CLIENT_DEVICE, Some(ME)),
            view("off-site", DESTINATION_KIND_NEST, None),
        ];
        assert_eq!(
            backup_reseed_rows(rows.clone(), ME.into(), false, true),
            vec![Some("mine".to_string()), Some("off-site".to_string())]
        );
        assert_eq!(
            backup_reseed_rows(rows, ME.into(), false, false),
            vec![Some("mine".to_string())]
        );
    }

    /// A nest row's confirm runs the pull-back; a custodian row's, an unknown
    /// id's and an inert row's never do.
    #[test]
    fn only_a_nest_rows_confirm_runs_the_pull_back() {
        let rows = vec![
            view("mine", DESTINATION_KIND_CLIENT_DEVICE, Some(ME)),
            view("off-site", DESTINATION_KIND_NEST, None),
            view("odd", "something-else", None),
        ];
        assert!(backup_reseed_runs_pull_back(
            rows.clone(),
            "off-site".into()
        ));
        assert!(!backup_reseed_runs_pull_back(rows.clone(), "mine".into()));
        assert!(!backup_reseed_runs_pull_back(rows.clone(), "odd".into()));
        assert!(!backup_reseed_runs_pull_back(rows, "gone".into()));
    }

    /// The pull-back's projection: a stop before the verdict is `stopped`
    /// with every other field empty; a verdict carries the shared lines, the
    /// outcome's own `is_whole` and the host's re-enrollment failure as given.
    #[cfg(feature = "sync-engine-host")]
    #[test]
    fn the_pull_back_projects_a_stop_and_a_verdict() {
        let stopped = pull_back_result(Err("unreachable".into()));
        assert_eq!(stopped.stopped.as_deref(), Some("unreachable"));
        assert!(stopped.result_lines.is_empty() && !stopped.is_whole);
        assert!(stopped.reenroll_error.is_none());

        let partial = outcome(SetOutcome::Refused {
            refusal: fauna_client_backup::reseed::SetRefusal::FolderUnnamed,
            detail: String::new(),
        });
        let expected = result_lines(&partial);
        let ran = pull_back_result(Ok(fauna_sync_engine::reseed_pull::PullBackResult {
            outcome: partial,
            reenroll_error: Some("seat".into()),
        }));
        assert!(ran.stopped.is_none());
        assert_eq!(ran.result_lines, expected);
        assert!(!ran.is_whole);
        assert_eq!(ran.reenroll_error.as_deref(), Some("seat"));
    }

    use fauna_client_backup::reseed::{
        DeliveredCorpus, DeliveredSet, SetAxis, SetOutcome, SetResult,
    };

    fn mail() -> DeliveredSet {
        DeliveredSet {
            set_name: "__mail".into(),
            axis: SetAxis::Segment,
            folder_display_name: None,
            folder_label: None,
        }
    }

    fn outcome(set_outcome: SetOutcome) -> ReseedOutcome {
        ReseedOutcome {
            delivered: DeliveredCorpus {
                sets: vec![mail()],
                ..Default::default()
            },
            sets: vec![SetResult {
                set: mail(),
                outcome: set_outcome,
            }],
        }
    }

    /// A connection that can never reach a nest, failing every call at once.
    async fn unreachable_nest() -> Arc<FfiNestClient> {
        let nest = FfiNestClient::new("ws://127.0.0.1:1".into(), vec![7u8; 32]).expect("client");
        nest.nest_arc().disconnect().await;
        nest
    }

    /// `finish` folds a partial outcome into the shared lines and the verdict,
    /// and does NOT re-enroll: the data is not all back, so this device must
    /// not become the seeded nest's custodian. The torn-down nest proves the
    /// re-enrollment never ran — had it, it would have failed and said so.
    ///
    /// Mutation check: dropping the `if is_whole` gate in `finish` reds the
    /// `reenroll_error` assertion.
    #[tokio::test]
    async fn a_partial_outcome_folds_its_lines_and_skips_the_reenrollment() {
        let nest = unreachable_nest().await;
        let partial = outcome(SetOutcome::Refused {
            refusal: fauna_client_backup::reseed::SetRefusal::FolderUnnamed,
            detail: String::new(),
        });
        let expected = result_lines(&partial);
        let result = finish(&nest, partial, "aa11").await;

        assert!(result.stopped.is_none(), "a verdict is not a stop");
        assert!(!result.is_whole, "a refused set is never a restore");
        assert_eq!(
            result.result_lines, expected,
            "the lines are the shared `result_lines`, never re-phrased here"
        );
        assert!(
            result.reenroll_error.is_none(),
            "a partial outcome must not re-enroll this device"
        );
    }

    /// A whole outcome re-enrolls — and a re-enrollment that cannot read the
    /// seeded box's identity reports that failure rather than guessing an
    /// empty destination list (which would mint a second row for a device
    /// that may already have one). The verdict stands beside it: the data IS
    /// back.
    ///
    /// Mutation check: replacing `reenroll`'s `bound_nest_id` arm with a
    /// silent `None` reds the `reenroll_error` assertion.
    #[tokio::test]
    async fn a_whole_outcome_reenrolls_and_reports_a_failed_reenrollment() {
        let nest = unreachable_nest().await;
        let whole = outcome(SetOutcome::Materialized {
            segments: vec![0],
            records: 3,
        });
        assert!(whole.is_whole(), "precondition: the fixture is whole");
        let expected = result_lines(&whole);
        let result = finish(&nest, whole, "aa11").await;

        assert!(result.stopped.is_none());
        assert!(result.is_whole);
        assert_eq!(result.result_lines, expected);
        assert!(
            result.reenroll_error.is_some(),
            "a whole outcome runs the re-enrollment, and its failure is reported"
        );
    }

    /// The stopped record carries the reason and nothing else, so a shell can
    /// never paint a verdict beside a stop.
    #[test]
    fn a_stop_carries_only_its_reason() {
        let stop = FfiReseedResult::stopped("why");
        assert_eq!(stop.stopped.as_deref(), Some("why"));
        assert!(stop.result_lines.is_empty() && !stop.is_whole && stop.reenroll_error.is_none());
    }
}
