// T16 custody facet, owner side — the two rendered lines of a
// `custody-holder-card` (docs/goal/ui/devices.md § Custody facet, piece 2).
//
// Both are pure composers over the SHARED label decisions the boundary already
// folded into the row (`custody_receipt_status_display` /
// `custody_held_bytes_display` in `fauna_client_capabilities`): which receipt
// state maps to which key, the em-dash placeholders for "no receipt yet", and
// that `degraded` rides independently of freshness. This module resolves and
// substitutes; it decides nothing, exactly as the linux/tui/android twins
// (`i18n::custody_receipt_status`, `custodyReceiptStatusText`) do.
//
// Both impure edges are PARAMETERS rather than imports — the timestamp
// formatter and the degraded badge's key — which keeps this module free of
// `$lib/wasm-folders` (and so of `$app/paths`), hence loadable under plain
// `deno test`. The imports below are relative for the same reason: the `$lib`
// alias is a SvelteKit/vite one `deno test` cannot resolve, as the file header
// of `./i18n/localized.ts` records for itself.
import { resolveLocalized } from './i18n/localized.ts';
import type { CustodyReceiptRowView } from './devices-machine.ts';

/**
 * The `custody-holder-receipt-status` line — the A7 three-state honesty rule,
 * where fresh / stale / no-receipt-yet are three different strings that never
 * collapse and never go empty.
 *
 * `formatWhen` renders the epoch seconds the shared side hands over instead of a
 * finished string: `format_unix_local` needs the OS timezone database, which
 * wasm lacks, so web is the one app that formats in JS. A receipt-less row has
 * no timestamp and its key carries no `{when}`, so the formatter is not called.
 */
export function custodyReceiptStatusText(
  receipt: CustodyReceiptRowView,
  formatWhen: (secs: number) => string,
): string {
  const secs = receipt.attested_at_secs;
  return resolveLocalized({
    key: receipt.status_label.key,
    args: {
      ...receipt.status_label.args,
      ...(secs != null ? { when: formatWhen(secs) } : {}),
    },
  });
}

/**
 * The `custody-holder-held-bytes` line — held bytes against the budget in
 * force, with the degraded marker appended when the receipt honestly reports
 * evicted or capped-short coverage.
 *
 * The two inner byte texts are themselves `LocalizedText` and are resolved
 * FIRST: a `LocalizedText` argument is a flat string — the same composition
 * `cellValueText` performs for a volume quota cell. `degraded` is **orthogonal
 * to freshness** (a fresh receipt can truthfully say it dropped payload), so the
 * marker appends here rather than replacing the status line above.
 */
export function custodyHeldBytesText(
  receipt: CustodyReceiptRowView,
  degradedBadgeKey: string,
): string {
  const line = resolveLocalized({
    key: receipt.held_bytes_label.key,
    args: {
      ...receipt.held_bytes_label.args,
      held: resolveLocalized(receipt.held),
      cap: resolveLocalized(receipt.cap),
    },
  });
  if (!receipt.degraded) return line;
  return `${line} — ${resolveLocalized({ key: degradedBadgeKey, args: {} })}`;
}
