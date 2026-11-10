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
  when: (kind: string, createdAtSecs: number) => string | null = () => null,
): string {
  const who = raw(
    a.kind,
    a.peer_address,
    a.peer_handle ?? '',
    a.summary,
    a.bridge_id ?? '',
    a.operation ?? '',
    a.target ?? '',
  ) ?? t.family.approval_no_sender;
  // A held mail also reads when it arrived ("sender · 14:03") — the shared
  // rule (`approvalTimestamp`) decides which kinds carry a time.
  const time = when(a.kind, a.created_at);
  return time == null ? who : `${who} · ${time}`;
}

/** What the page's decide calls — rpc.ts / wasm.ts in production, fakes in
 *  `family-approvals.test.ts` (this module stays wasm-free). */
export interface DecideVerbs {
  /** The composer's send-or-queue; `true` = queued. */
  decideOrQueue: (entry: FamilyApprovalEntry, approve: boolean) => Promise<boolean>;
  /** The shared fold of a queued decision. */
  afterQueued: (approvals: FamilyApprovalEntry[], decided: FamilyApprovalEntry) => FamilyApprovalEntry[];
  /** The page's full re-read, after a sent decision. */
  reread: () => Promise<void>;
}

/**
 * A guardian's approve/deny of one approvals-queue item
 * (`account-offline-mutation.md` § Implementation status today, the family
 * composer). Sent → the page re-reads (`null`: the re-read set the list).
 * Queued with no nest → the item leaves the local list through the shared
 * fold and nothing is re-read; the read after the drain is the nest's word.
 */
export async function decideApproval(
  verbs: DecideVerbs,
  approvals: FamilyApprovalEntry[],
  entry: FamilyApprovalEntry,
  approve: boolean,
): Promise<FamilyApprovalEntry[] | null> {
  if (await verbs.decideOrQueue(entry, approve)) {
    return verbs.afterQueued(approvals, entry);
  }
  await verbs.reread();
  return null;
}
