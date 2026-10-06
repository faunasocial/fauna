//! Resolved Backups-page destination-row text, shared between tui and linux
//! (the two native shells that render a `BackupDestinationStatusItem`/
//! [`crate::audit::DestinationAuditRecord`] row directly in Rust — web
//! composes the same two-level split client-side in `$lib/value-format.ts`).
//!
//! Each function here used to be one function hand-duplicated in both apps:
//! extract the row's field, get "now", delegate to
//! `fauna_core::format::backup_*_text`. The delegation target already took an
//! app-supplied `lookup` closure, and both apps passed the exact same one
//! (`fauna_i18n::strings::lookup` is a single shared function, not
//! per-app-generated), so nothing about the wrapper was actually app-specific
//! — moving it here lifts both the extraction and the resolve, leaving each
//! app supplying only "now" (kept a parameter, not computed here, so each
//! app's own mockable clock seam for audit-driven tests stays intact —
//! `e2e-conventions.md` convention 14).

use crate::audit::DestinationAuditRecord;
use fauna_protocol::backup::BackupDestinationStatusItem;

/// `backup-destination-last-upload-time` row text, via the shared
/// [`fauna_core::format::backup_last_upload_text`]. `status` absent or
/// carrying no `last_upload_time` reads as "never".
pub fn destination_last_upload_text(
    status: Option<&BackupDestinationStatusItem>,
    now_ms: i64,
) -> String {
    fauna_core::format::backup_last_upload_text(
        status.and_then(|s| s.last_upload_time),
        now_ms,
        fauna_i18n::strings::lookup,
    )
}

/// `backup-destination-backlog-count` row text, via the shared
/// [`fauna_core::format::backup_backlog_label`]. An absent status is the
/// not-yet-read baseline of `0`, applied inside the shared fn.
pub fn destination_backlog_text(status: Option<&BackupDestinationStatusItem>) -> String {
    fauna_core::format::backup_backlog_label(status.map(|s| s.backlog_count))
        .resolve(fauna_i18n::strings::lookup)
}

/// `backup-destination-last-audit-time` row text: when **this client's own**
/// audit last passed against the destination, via the shared
/// [`fauna_core::format::backup_last_audit_text`]. `None` (no pass yet) ⇒
/// "never".
///
/// Note what this is *not*: [`destination_last_upload_text`] is the source
/// nest reporting on its own uploads; this is the only line on the page that
/// neither the source nor the destination gets to assert.
pub fn destination_last_audit_text(record: Option<&DestinationAuditRecord>, now_ms: i64) -> String {
    let last_passed = record
        .and_then(|r| r.state.last_passed_at)
        .and_then(|s| u64::try_from(s).ok());
    fauna_core::format::backup_last_audit_text(last_passed, now_ms, fauna_i18n::strings::lookup)
}

/// `backup-destination-last-audit-time` row text for a **client-device
/// custodian** row: when that device's own self-audit last **passed**, via
/// the shared [`fauna_core::format::backup_self_audit_text`].
///
/// The verdict itself is not rendered here — it is what raises (or does not
/// raise) the banner, through the same shared predicate
/// ([`fauna_core::format::backup_self_audit_is_alerting`]). `None` reads as
/// *not yet*, never as a pass: a custodian shipped before the carrier landed
/// reports nothing at all.
pub fn destination_self_audit_text(
    status: Option<&BackupDestinationStatusItem>,
    now_ms: i64,
) -> String {
    fauna_core::format::backup_self_audit_text(
        status.and_then(|s| s.last_audit_passed_at),
        now_ms,
        fauna_i18n::strings::lookup,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::DestinationAuditState;

    /// The never-arm resolves to a complete sentence with no `{when}` left in
    /// it — the shared fn hands back a one-level label there, and a client
    /// that blindly substituted would leave the placeholder visible.
    #[test]
    fn last_upload_never_leaves_no_placeholder() {
        for status in [
            None,
            Some(BackupDestinationStatusItem::default()),
            Some(BackupDestinationStatusItem {
                last_upload_time: Some(0),
                ..Default::default()
            }),
        ] {
            let got = destination_last_upload_text(status.as_ref(), 1_700_000_000_000);
            assert!(!got.contains("{when}"), "got {got:?}");
            assert!(!got.is_empty());
        }
    }

    /// A real timestamp fills `{when}` with the resolved relative time.
    #[test]
    fn last_upload_substitutes_the_resolved_when() {
        let now_ms = 1_700_000_000_000_i64;
        let two_hours_ago_secs = (now_ms / 1000) as u64 - 2 * 3600;
        let status = BackupDestinationStatusItem {
            last_upload_time: Some(two_hours_ago_secs),
            ..Default::default()
        };
        let got = destination_last_upload_text(Some(&status), now_ms);
        assert!(!got.contains("{when}"), "placeholder survived: {got:?}");
        assert!(got.contains('2'), "expected the 2h bucket in {got:?}");
    }

    /// An absent status reads as the shared zero baseline, not as blank.
    #[test]
    fn backlog_absent_reads_as_zero() {
        let zero = BackupDestinationStatusItem::default();
        let seven = BackupDestinationStatusItem {
            backlog_count: 7,
            ..Default::default()
        };
        assert_eq!(
            destination_backlog_text(None),
            destination_backlog_text(Some(&zero))
        );
        assert!(destination_backlog_text(Some(&seven)).contains('7'));
    }

    /// A `None` record — never audited — reads as "never", not blank, and a
    /// signed `last_passed_at` that somehow went negative (never a real
    /// state, but the `u64::try_from` guard exists for it) degrades to the
    /// same "never" rather than panicking.
    #[test]
    fn last_audit_never_when_absent_or_negative() {
        let never = destination_last_audit_text(None, 1_700_000_000_000);
        assert!(!never.contains("{when}"));
        assert!(!never.is_empty());

        let negative = DestinationAuditRecord {
            state: DestinationAuditState {
                destination_id: "d1".into(),
                last_passed_at: Some(-1),
                last_attempt_at: None,
                verified_ledger_generations: Default::default(),
                accepted_regressions: Default::default(),
                seat_settled_under: None,
            },
            verdict: None,
        };
        assert_eq!(
            destination_last_audit_text(Some(&negative), 1_700_000_000_000),
            never
        );
    }

    /// A real pass timestamp resolves through, mirroring the upload row's
    /// contract.
    #[test]
    fn last_audit_substitutes_the_resolved_when() {
        let now_ms = 1_700_000_000_000_i64;
        let two_hours_ago_secs = (now_ms / 1000) - 2 * 3600;
        let record = DestinationAuditRecord {
            state: DestinationAuditState {
                destination_id: "d1".into(),
                last_passed_at: Some(two_hours_ago_secs),
                last_attempt_at: None,
                verified_ledger_generations: Default::default(),
                accepted_regressions: Default::default(),
                seat_settled_under: None,
            },
            verdict: None,
        };
        let got = destination_last_audit_text(Some(&record), now_ms);
        assert!(!got.contains("{when}"), "placeholder survived: {got:?}");
        assert!(got.contains('2'), "expected the 2h bucket in {got:?}");
    }

    /// A custodian that has never self-audited reads as "not yet" — never a
    /// pass — exactly like the owner-side audit's "never" arm.
    #[test]
    fn self_audit_never_when_absent() {
        let got = destination_self_audit_text(None, 1_700_000_000_000);
        assert!(!got.contains("{when}"));
        assert!(!got.is_empty());
    }

    /// A custodian's own passed timestamp resolves through the self-audit
    /// door, independently of the owner-side audit door above — the two must
    /// not collapse into one text.
    #[test]
    fn self_audit_substitutes_the_resolved_when() {
        let now_ms = 1_700_000_000_000_i64;
        let two_hours_ago_secs = (now_ms / 1000) as u64 - 2 * 3600;
        let status = BackupDestinationStatusItem {
            last_audit_passed_at: Some(two_hours_ago_secs),
            ..Default::default()
        };
        let got = destination_self_audit_text(Some(&status), now_ms);
        assert!(!got.contains("{when}"), "placeholder survived: {got:?}");
        assert!(got.contains('2'), "expected the 2h bucket in {got:?}");
    }
}
