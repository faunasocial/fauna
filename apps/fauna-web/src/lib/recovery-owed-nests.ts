// The successor's owed nests — the Settings recovery-kit section's list of the
// nests its recovery has not reached yet, with the two-step dismiss
// (`docs/goal/ui/settings.md` § Recovery kit → *The successor's owed nests*;
// the rule is `succession-linked-nests.md`). The read, the copy and the
// dismiss are shared Rust (`fauna_client_recovery::owed_nests`), reached
// through the wasm door `recoveryOwedNests` / `recoveryDismissOwedNest`; this
// module holds only the entry's key and the list's local fold, so the page and
// its test read one copy — tui's `owed_nests_elements` and its
// `ConfirmDismissOwedNest` fold.

import type { LocalizedText } from './i18n/localized.ts';

/** One owed entry as the wasm door hands it, and as the dismiss takes it back. */
export interface OwedNestEntry {
  /** The nest that keeps the entry — `null` the bound nest, else the linked
   *  keeper's id, hex. Where the dismiss settles. */
  keeper: string | null;
  /** The retired identity whose pairing named the owed nest, hex. */
  old_actor_id: string;
  /** The owed nest's id, hex. */
  nest_id: string;
  /** `recovery-kit-owed-nest-address`'s line. */
  address_copy: LocalizedText;
  /** `recovery-kit-owed-nest-outcome`'s line. */
  outcome_copy: LocalizedText;
  /** The outcome's arm — for a test, never to select copy by. */
  outcome_kind: string;
}

/** `null` from the door when nothing is owed: the section paints none of the
 *  seven ids. */
export interface OwedNests {
  /** `recovery-kit-owed-nests-status`'s line. */
  status: LocalizedText;
  entries: OwedNestEntry[];
}

/** The entry's stable key — keeper and settle key, never a painted index (a
 *  re-read may re-order the list). */
export function owedKey(entry: Pick<OwedNestEntry, 'keeper' | 'old_actor_id' | 'nest_id'>): string {
  return `${entry.keeper ?? ''}/${entry.old_actor_id}/${entry.nest_id}`;
}

/** A settled dismiss drops that entry; the last one dropped takes the whole
 *  section off the page. The status line is the shared projection's and is
 *  re-read on the next hydrate. */
export function withoutOwed(view: OwedNests | null, key: string): OwedNests | null {
  if (!view) return null;
  const entries = view.entries.filter((e) => owedKey(e) !== key);
  return entries.length ? { ...view, entries } : null;
}
