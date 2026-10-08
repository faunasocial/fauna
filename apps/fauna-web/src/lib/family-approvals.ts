// The `family-approval-item` row's display text — the one rule every app
// shares (family-safety.md § Reach approvals + § Where logic lives).
import { t } from './i18n/strings.ts';
import type { FamilyApprovalEntry } from './rpc.ts';

/**
 * What a `family-approval-item` row renders — the localized no-sender
 * fallback (whenever the kind's own field is empty) over whatever `raw` picks
 * between `peer_address`, `peer_handle` and `summary` — or, for a
 * `feed_source`, composes from the grant's `(bridge_id, operation, target)`
 * key with the ward's label quoted after it.
 *
 * `raw` is NOT defaulted here on purpose — this module deliberately never
 * imports `./wasm` (which pulls in the SvelteKit `$app/paths` alias), so its
 * own fallback-decision logic stays plain-Deno-testable
 * (`family-approvals.test.ts`) without a wasm/SvelteKit runtime. The one
 * production call site (`routes/family/+page.svelte`) passes
 * `approvalDisplayTextRaw` (`fauna_core::format::approval_display_text` over
 * wasm) explicitly.
 */
export function approvalText(
  a: FamilyApprovalEntry,
  raw: (
    kind: string,
    peerAddress: string,
    peerHandle: string,
    summary: string,
    bridgeId: string,
    operation: string,
    target: string,
  ) => string | null,
): string {
  return raw(
    a.kind,
    a.peer_address,
    a.peer_handle ?? '',
    a.summary,
    a.bridge_id ?? '',
    a.operation ?? '',
    a.target ?? '',
  ) ?? t.family.approval_no_sender;
}
