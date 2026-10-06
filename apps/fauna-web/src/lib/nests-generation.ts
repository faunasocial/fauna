// Pure logic for the Nests page's retained-generation rows (`nests.md` §
// Trust facet — generation recovery, ratified 2026-07-29). Extracted from
// `NestsSection.svelte` so the two ratified honesty invariants are
// unit-testable without a live Svelte/DOM context — mirrors linux's
// `generation_path_text`/`generation_shows_restore`/`generation_notice_text`
// extraction (`apps/fauna-linux/src/settings/linked_nests.rs`), the same
// idiom this file's own component uses for `backupScopeText`.
// Relative, not `$lib/...` — this module's own `.test.ts` runs under plain
// `deno test` (no Vite/SvelteKit alias resolution; see `justfile`'s
// `web-unit-test`), and a relative import resolves under both.
import { t } from './i18n/strings.ts';

export type TrustGenerationStatus = 'Listed' | 'Unreachable';

export interface TrustGenerationRow {
  status: TrustGenerationStatus;
  destination_id: string;
  destination_label: string;
  folder_name: string;
  // Genuinely optional: `path_hash` is one-way, so a sealed-path row
  // (or a rogue source's) has none and the leaf renders the hash instead —
  // never hidden or skipped (nests.md:123).
  path: string | null;
  path_hash: string;
  manifest_hash: string;
  size_bytes: number;
  superseded_at: number;
  expires_at: number;
}

// What a `RestoreGeneration` dispatch did — a PRODUCT state
// (`PastRecoveryWindow`), not folded onto `error` (nests.md:124).
export type TrustRestoreOutcome = 'Restored' | 'PastRecoveryWindow';

/** The `nest-trust-generation-path` identity leaf (nests.md:122,123). On an
 *  `Unreachable` row this names the DESTINATION that went dark (there is no
 *  generation to identify); on a `Listed` row with no plaintext `path` (a
 *  sealed custody row with its path scrubbed), this renders the hash
 *  instead — the rows a rogue source produced are exactly the ones a user
 *  needs to see. NEVER hidden or skipped. */
export function generationPathText(g: TrustGenerationRow): string {
  if (g.status === 'Unreachable') return g.destination_label;
  return g.path ? t.nests.generation_path({ path: g.path }) : t.nests.generation_path_unknown({ hash: g.path_hash });
}

/** Whether `nest-trust-generation-restore` renders (nests.md:122). An
 *  `Unreachable` row carries no restore address — offering the affordance
 *  would imply we knew something we do not. */
export function generationShowsRestore(g: TrustGenerationRow): boolean {
  return g.status !== 'Unreachable';
}

/** The `nest-trust-generation-notice` text for the page's last restore
 *  action (nests.md § Trust facet — generation recovery). Never an error —
 *  `PastRecoveryWindow` is a product state, not a failure. */
export function generationNoticeText(outcome: TrustRestoreOutcome | null): string {
  if (outcome === 'Restored') return t.nests.generation_restored;
  if (outcome === 'PastRecoveryWindow') return t.nests.generation_past_window;
  return '';
}
