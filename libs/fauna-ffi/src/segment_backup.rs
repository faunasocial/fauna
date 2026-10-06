//! UniFFI **status records** for the cross-location segment backup.
//!
//! This module used to be the UniFFI surface for the *client* upload driver:
//! per-platform triggers (iOS `BGProcessingTask`, Android `WorkManager`, desktop
//! always-on) drove `BackupCoordinator::run_all_tuples` through an
//! `FfiBackupCoordinator` handle running on a dedicated worker thread.
//!
//! **That driver is deleted (2026-08-16).** Since the slice-5 flip completed
//! across all 7 apps, backup upload for a `nest`-kind destination is run by the
//! source nest's own in-process coordinator with no client alive
//! (`docs/goal/architecture/message-segment-store.md` § Cross-location backup
//! protocol — *Source-side coordinator*), so every one of those triggers had
//! already been removed from the apps and the handle was exported to four
//! platforms with zero callers.
//!
//! What remains here is the record the **status read** returns
//! ([`FfiBackupDestinationStatus`], produced by
//! `crate::backup_destinations::backup_destination_status`, which reads the
//! nest's own `fauna.backup.status` projection).
//!
//! ⚠ **Do not confuse the client upload driver with the client-device
//! CUSTODIAN**, which is alive and still client-driven: `FfiCustodianHost`
//! exports a near-identical method set (`start_push_debounce` included) and
//! android genuinely drives it. The custodian *pull* is a distinct mechanism the
//! flip deliberately did not touch (`docs/goal/behavior/backup-destinations.md`
//! § Third destination kind).

/// One Backups-page status row. Returned by
/// [`crate::backup_destinations::backup_destination_status`] for native apps to
/// render `backup-destination-last-upload-time` /
/// `backup-destination-backlog-count`.
#[derive(uniffi::Record, Clone)]
pub struct FfiBackupDestinationStatus {
    /// The configured destination this row reports on.
    pub destination_id: String,
    /// Unix seconds of the last successful manifest-mirror upload to this
    /// destination; `None` until the first manifest is mirrored.
    pub last_upload_time: Option<u64>,
    /// Source segments still queued for upload to this destination.
    pub backlog_count: u32,
    /// Client-device rows only: bytes the custodian reported holding at its last
    /// check-in, for the `backup-destination-usage` render. `None` on a nest row
    /// **and** on a custodian that has never checked in — which reads as
    /// *nothing held yet*, not *0 bytes held*.
    pub held_bytes: Option<u64>,
    /// Client-device rows only: `CAP_STATE_OK` / `CAP_STATE_REACHED`, passed
    /// straight to the shared `backup_usage_label`.
    ///
    /// ⚠ This field is why the usage label takes three arguments instead of
    /// two. Cap-reached is **read, never inferred**: a pull pass that stopped at
    /// its cap ends *below* the cap (a segment larger than the remaining
    /// headroom stops the pass without filling it), so a shell re-deriving the
    /// verdict from `held >= cap` renders a silently-stopped backup as
    /// healthy-with-room. Dropping it from this projection would leave every
    /// native shell no choice but that inference.
    pub cap_state: Option<String>,
    /// Client-device rows only: the custodian's own last self-audit verdict
    /// (`AUDIT_STATE_OK` / `AUDIT_STATE_FAILED`), read through the shared
    /// `fauna_core::format::backup_self_audit_is_alerting`.
    ///
    /// ⚠ **`None` is *not yet audited*, never *passing*.** Every custodian
    /// shipped before the carrier landed reports nothing at all, so a shell
    /// reading absence as a pass would render an unverified copy as verified —
    /// and one reading it as a failure would raise a fleet-wide false data-loss
    /// alarm. Both misreadings are what the shared predicate exists to prevent.
    pub audit_state: Option<String>,
    /// Client-device rows only: unix seconds of the custodian's last **passed**
    /// self-audit, for the `backup-destination-last-audit-time` render through
    /// `fauna_core::format::backup_self_audit_label`.
    ///
    /// ⚠ Read **with** [`Self::audit_state`], never instead of it: a failure
    /// deliberately leaves this stamp at the previous pass (`SelfAudit::failed`
    /// is never handed `now`), so a shell rendering the stamp alone would show
    /// a freshly-rotted custodian as just-verified.
    pub last_audit_passed_at: Option<u64>,
}

/// The wire row the **nest's** `fauna.backup.status` projection returns — the
/// source the Backups page reads since the slice-4 leg (d) repoint. This is the
/// only path by which a native shell learns a custodian's held bytes, cap state
/// and **self-audit verdict**, so all four custodian columns cross here; the
/// source-side impl above cannot supply them.
///
/// ⚠ The held-bytes/cap-state pair crossed 2026-08-03 and the audit pair did
/// **not**, for a reason worth remembering rather than repeating: a sibling
/// commit had widened the wire row one layer up hours earlier, and this
/// conversion's own doc comment then asserted completeness over the two columns
/// its author was carrying. A projection that silently drops fields it receives
/// leaves every non-linking shell unable to render them at all — which is what
/// kept the custodian's audit answer dark on five of seven apps until
/// 2026-08-20. Widen this together with the wire type, or the field is dark.
impl From<fauna_protocol::backup::BackupDestinationStatusItem> for FfiBackupDestinationStatus {
    fn from(s: fauna_protocol::backup::BackupDestinationStatusItem) -> Self {
        Self {
            destination_id: s.destination_id,
            last_upload_time: s.last_upload_time,
            backlog_count: s.backlog_count,
            held_bytes: s.held_bytes,
            cap_state: s.cap_state,
            audit_state: s.audit_state,
            last_audit_passed_at: s.last_audit_passed_at,
        }
    }
}

// NOTE (2026-07-15 dark-rail audit): the standalone `resolve_backup_destination`
// free fn (and its `FfiResolvedDestination` record) were deleted — its doc
// claimed "shared by every native app's add-destination dialog", but no
// client ever called it: the native dialogs use the atomic
// `backup_destination_add`/`backup_destination_edit` wrappers (which resolve
// internally via `segment_backup::resolve_destination`), and desktop calls
// the resolvers directly.

// The `participant_class_for_ac` AC-line mapping and its test went with the
// upload driver above: the `backup-lease` gate existed only to keep exactly one
// plugged-in desktop uploading, and there is no client upload to coordinate any
// more. The `backup-lease` FEATURE itself was removed 2026-08-17 (it gated no
// code by then; `fauna-client-delegation` stays a dep via `task-delegation`,
// which the Settings page's shared view genuinely uses).
