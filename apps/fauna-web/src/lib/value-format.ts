/**
 * Human value formatting — relative time, byte sizes, durations — routed
 * through the shared `fauna_core::format` decision (wasm boundary) and resolved
 * to i18n strings. Clients MUST NOT hand-roll the bucketing thresholds or the
 * English strings; this is the single web surface for them.
 * See `docs/goal/behavior/value-formatting.md`.
 */

import {
  byteSizeRaw,
  relativeTimeRaw,
  durationSecsRaw,
  graceCountdownRaw,
  conversationTimestampRaw,
  backupLastUploadLabel,
  backupBacklogLabel,
  backupLastAuditLabel,
  backupSelfAuditLabel,
  backupAuditAlertLabel,
  backupDestinationKindLabel,
  backupUsageLabel,
  type BackupAuditAlertReason,
  type RelativeTimeDisplay,
} from '$lib/wasm';
import { resolveLocalized } from '$lib/i18n/localized';
import { utcOffsetSeconds } from '$lib/utcOffset';

/** Human byte size ("1.5 MB") for a raw byte count. */
export function byteSize(bytes: number): string {
  return resolveLocalized(byteSizeRaw(bytes));
}

/**
 * Resolve a shared `RelativeTimeDisplay` to text: `localized` for the recent
 * buckets, else a locale-aware absolute date (the `≥7d` arm). The shared side
 * picks which arm applies; this only renders it.
 */
function resolveRelativeTime(d: RelativeTimeDisplay): string {
  if (d.localized) return resolveLocalized(d.localized);
  if (d.absolute_epoch_ms != null) {
    return new Date(d.absolute_epoch_ms).toLocaleDateString();
  }
  return '';
}

/**
 * Relative time ("just now", "2h ago") for an epoch-**millisecond** timestamp,
 * falling back to an absolute, locale-aware date for timestamps `≥ 7 d` old.
 *
 * The unit is a property of each data source: callers whose wire field is in
 * **micros** (feed posts, notifications, search results) MUST divide by 1000
 * before calling; knocks are already millis (see the value-formatting.md
 * flow-trace).
 */
export function relativeTime(epochMs: number): string {
  return resolveRelativeTime(relativeTimeRaw(Date.now(), epochMs));
}

/**
 * `backup-destination-last-upload-time` row text. The never-vs-real key choice,
 * the epoch-`0` guard, and the seconds→ms conversion are the shared fn's
 * (`fauna_core::format::backup_last_upload_label`) — pass the status's raw
 * `last_upload_time` in unix **seconds** (`null` = no status read yet / nothing
 * mirrored ⇒ "never"). See value-formatting.md § Backup destination status labels.
 */
export function backupLastUploadText(lastUploadSecs: number | null): string {
  const d = backupLastUploadLabel(lastUploadSecs, Date.now());
  if (!d.when) return resolveLocalized(d.label);
  // The shared side hands back the label keyed but arg-less, plus the `when`
  // display to render into its `{when}` placeholder.
  return resolveLocalized({
    key: d.label.key,
    args: { ...(d.label.args ?? {}), when: resolveRelativeTime(d.when) },
  });
}

/**
 * `backup-destination-backlog-count` row text ("{count} queued"). The
 * absent-status 0 baseline is the shared fn's
 * (`fauna_core::format::backup_backlog_label`). See value-formatting.md
 * § Backup destination status labels.
 */
export function backupBacklogText(backlogCount: number | null): string {
  return resolveLocalized(backupBacklogLabel(backlogCount));
}

/**
 * `backup-destination-last-audit-time` row text: when this client's **own** audit
 * last *passed* against the destination. `lastPassedSecs` is the audit record's raw
 * `last_passed_at` (unix **seconds**; `null` = no pass yet ⇒ "never"), `nowMs` the
 * same clock the pass was judged against, so a stamp this client wrote can never
 * read as being in the future.
 *
 * Note what this is *not*: the row above it (last-upload) is the source nest
 * reporting on its own uploads; this is the only line on the page that neither the
 * source nor the destination gets to assert. The never-vs-real key choice and the
 * seconds→ms conversion are the shared fn's
 * (`fauna_core::format::backup_last_audit_label`). See `docs/goal/ui/backups.md`
 * § Audit-alert surface.
 */
export function backupLastAuditText(lastPassedSecs: number | null, nowMs: number): string {
  const d = backupLastAuditLabel(lastPassedSecs, nowMs);
  if (!d.when) return resolveLocalized(d.label);
  return resolveLocalized({
    key: d.label.key,
    args: { ...(d.label.args ?? {}), when: resolveRelativeTime(d.when) },
  });
}

/**
 * `backup-destination-last-audit-time` row text for a **client-device
 * custodian** row: when that device's own self-audit last *passed*.
 * `lastPassedSecs` is the status row's raw `last_audit_passed_at` (unix
 * **seconds**; `null` = not yet audited ⇒ "Self-checked: not yet", never a
 * verdict).
 *
 * A separate door from {@link backupLastAuditText} all the way down: the
 * owner-side loop and a custodian's self-report answer the same question
 * from opposite sides of the trust line, and collapsing them behind a flag
 * is exactly how a caller renders one while meaning the other
 * (`docs/goal/ui/backups.md` § Audit-alert surface → *The client-device
 * arm*).
 */
export function backupSelfAuditText(lastPassedSecs: number | null, nowMs: number): string {
  const d = backupSelfAuditLabel(lastPassedSecs, nowMs);
  if (!d.when) return resolveLocalized(d.label);
  return resolveLocalized({
    key: d.label.key,
    args: { ...(d.label.args ?? {}), when: resolveRelativeTime(d.when) },
  });
}

/**
 * `backup-audit-alert` banner text for one failing destination — a complete
 * sentence naming both the destination and the reason (the banner is indexed, so a
 * bare "backup problem" would not say *which* one).
 *
 * `reason` comes straight out of a `backupAuditRunPass` row and is opaque here on
 * purpose: which verdicts are loud, and what each one says, are shared Rust's
 * single answer (`DestinationAuditRecord::alert_reason` /
 * `fauna_core::format::backup_audit_alert_label`), so web cannot drift into
 * alerting on a transient `Unreachable` — the laptop-on-a-plane case the loop keeps
 * quiet by design.
 */
export function backupAuditAlertText(
  reason: BackupAuditAlertReason,
  destinationLabel: string,
): string {
  return resolveLocalized(backupAuditAlertLabel(reason, destinationLabel));
}

/**
 * `backup-destination-kind-badge` text for one destination row — the visible
 * half of "a client custodian never silently satisfies *you have an off-site
 * backup*" (`docs/goal/ui/backups.md` § Durability + labeling).
 *
 * Pass the row's own raw `kind`. An unrecognised kind (a newer client wrote the
 * row) renders **as itself** rather than collapsing into a generic word — that
 * is precisely when the user needs to see what this build cannot drive — and
 * that arm is the shared label's, not a check here.
 */
export function backupDestinationKindText(kind: string): string {
  return resolveLocalized(backupDestinationKindLabel(kind));
}

/**
 * `backup-destination-usage` row text (client-device rows only): held bytes
 * against the user-set cap.
 *
 * The two-level shape mirrors `backupLastUploadText` above: the shared side
 * hands back a label plus the already-computed `held`/`cap` byte-size displays,
 * which resolve into its `{held}`/`{cap}` placeholders. No second wasm round
 * trip, and no byte-size formatting decided here.
 *
 * ⚠ `capState` is **read, never inferred** — see `backupUsageLabel`'s own note.
 * Pass the status row's `cap_state` straight through; a caller tempted to
 * substitute `held >= cap` renders a stalled backup as healthy-with-room.
 */
export function backupUsageText(
  heldBytes: number | null,
  capacityCapBytes: number | null,
  capState: string | null,
): string {
  const d = backupUsageLabel(heldBytes, capacityCapBytes, capState);
  const args: Record<string, string> = { ...(d.label.args ?? {}) };
  if (d.held) args.held = resolveLocalized(d.held);
  if (d.cap) args.cap = resolveLocalized(d.cap);
  return resolveLocalized({ key: d.label.key, args });
}

/** Coarse d/h/m duration ("5d 2h 30m") for an elapsed-seconds count. */
export function durationSecs(secs: number): string {
  return resolveLocalized(durationSecsRaw(secs));
}

// The three tip formatters MOVED to `$lib/payments` (2026-08-16, the web
// `payments` excision leg). This module is unconditionally in the bundle, so a
// formatter defined here ships the wasm face's name into the store-safe
// artifact even with every caller folded away by `__FAUNA_PAYMENTS__`
// (`dynamic-features.md` § Platform-family surface excision — the
// isolated-module pattern). Same reason took `claimStatusLabel` and
// `providerStatusLabel` below.

/**
 * Coarse d/h countdown ("2d 3h" / "5h") to a future epoch-**ms** deadline, or
 * `null` once elapsed — callers render their own already-localized "elapsed"
 * label in that case (e.g. `admin.dns.rename.grace_elapsed`).
 */
export function graceCountdown(deadlineMs: number): string | null {
  const lt = graceCountdownRaw(deadlineMs, Date.now());
  return lt ? resolveLocalized(lt) : null;
}

/**
 * Contextual conversation/DM-row last-activity timestamp for an epoch-**ms**
 * timestamp, bucketed in the user's **local** timezone (unlike `relativeTime`'s
 * duration buckets): today → a local 24h clock ("14:30"), the previous local day
 * → "Yesterday", 2–6 days → a weekday ("Mon"), else a locale-aware absolute date.
 * The offset is passed because shared Rust is WASM-safe and can't read the zone
 * (seconds east of UTC, via the one `$lib/utcOffset` door). Clients MUST NOT
 * hand-roll the buckets — see value-formatting.md § Conversation timestamp.
 */
export function conversationTimestamp(epochMs: number): string {
  const offsetSeconds = utcOffsetSeconds();
  const d = conversationTimestampRaw(Date.now(), epochMs, offsetSeconds);
  if (d.clock != null) return d.clock;
  if (d.localized) return resolveLocalized(d.localized);
  if (d.absolute_epoch_ms != null) {
    return new Date(d.absolute_epoch_ms).toLocaleDateString();
  }
  return '';
}
