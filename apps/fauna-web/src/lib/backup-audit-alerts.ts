// The owner-side `backup-audit-alert` reasons for one destination's audit row
// (`docs/goal/ui/backups.md` § Audit-alert surface). The row's `alert_reasons`
// is the shared door's single answer (`DestinationAuditRecord::alert_reasons`):
// the standing verdict's reason, then at most one `SourceRegressed`. The page
// paints every entry, flat, each **opaque** — straight into
// `backupAuditAlertText` — so which reasons are loud never gets re-derived here.
import type { DestinationAuditRow } from '#lib/rpc';
import type { BackupAuditAlertReason } from '#lib/wasm';

export function auditAlertReasons(row: DestinationAuditRow | undefined): BackupAuditAlertReason[] {
  return row?.alert_reasons ?? [];
}

/** One `backup-audit-alert` banner: its opaque reason, and whether it carries
 *  `backup-audit-recover-button`. */
export interface AuditAlertBanner {
  reason: BackupAuditAlertReason;
  recover: boolean;
}

// The row's banners, each marked with whether it carries the recovery gesture
// (the fifth reason, `ui/backups.md` § Audit-alert surface). Whether a row
// offers the gesture at all is the shared `recover_offered` answer, read off
// the row — never re-derived here; it is true only while a `SourceRegressed`
// stands, which the door orders **last**, so the gesture sits on the row's
// last banner, the fifth reason's own (tui's placement).
export function auditAlertBanners(row: DestinationAuditRow | undefined): AuditAlertBanner[] {
  const reasons = auditAlertReasons(row);
  return reasons.map((reason, i) => ({
    reason,
    recover: !!row?.recover_offered && i === reasons.length - 1,
  }));
}
