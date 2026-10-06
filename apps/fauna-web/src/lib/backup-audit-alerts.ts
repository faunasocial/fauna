// The owner-side `backup-audit-alert` reasons for one destination's audit row
// (`docs/goal/ui/backups.md` § Audit-alert surface). The row's `alert_reasons`
// is the shared door's single answer (`DestinationAuditRecord::alert_reasons`):
// the standing verdict's reason, then at most one `SourceRegressed`. The page
// paints every entry, flat, each **opaque** — straight into
// `backupAuditAlertText` — so which reasons are loud never gets re-derived here.
import type { DestinationAuditRow } from '$lib/rpc';
import type { BackupAuditAlertReason } from '$lib/wasm';

export function auditAlertReasons(row: DestinationAuditRow | undefined): BackupAuditAlertReason[] {
  return row?.alert_reasons ?? [];
}
