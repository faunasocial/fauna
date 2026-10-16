// The let-go of a dead generation — the Settings recovery-kit section's fifth
// act (`docs/goal/ui/settings.md` § Recovery kit; the act is
// `fleet-scope-reclamation.md`
// § *Fleet-scope reclamation*, clause (3)(j)). The read, the copy and the act are shared Rust
// (`fauna_account_plane::generation_let_go`), reached through the wasm door
// `fauna-wasm/src/generation_let_go.rs`; this module holds only the two gates
// and the answer's wording, so the page and its test read one copy — tui's
// `let_go_elements` and its `Outcome::RecoveryLetGoDone` fold.

import type { LocalizedText } from './i18n/localized.ts';

/** The account runtime's dead read, as the wasm door hands it. */
export interface DeadGenerations {
  /** The dead generations' ids, hex — handed back to the act. Never rendered. */
  generation_ids: string[];
  /** `recovery-kit-unreadable-status` — the shared projection's line; `null`
   *  when nothing is dead. **The trio's one render gate.** */
  unreadable_status: LocalizedText | null;
}

/** What one press of `recovery-kit-let-go-button` did. */
export interface LetGoOutcome {
  retired: number;
  /** A generation became readable again and was left untouched. */
  kept: boolean;
  /** The re-read after the act — it REPLACES the rendered one. */
  dead: DeadGenerations;
}

/** The line, its confirm field and its button render ONLY while the shared
 *  projection has a line — never re-derived from the id list. */
export function letGoRenders(dead: DeadGenerations | null): boolean {
  return dead?.unreadable_status != null;
}

/** The button arms on the confirm word alone (exact, case-sensitive — the word
 *  is shared Rust's constant, never localized), and holds while busy. The act
 *  re-checks the word when it fires. */
export function letGoArmed(confirm: string, word: string | null, busy: boolean): boolean {
  return !busy && word !== null && confirm === word;
}

/** The sentence a settled let-go leaves on `error-message`: the retired count,
 *  plus the kept clause when a generation became readable again — named, never
 *  silently skipped. */
export function letGoAnswer(
  outcome: LetGoOutcome,
  copy: { let_go_done: (v: { retired: string }) => string; let_go_kept: string },
): string {
  const done = copy.let_go_done({ retired: String(outcome.retired) });
  return outcome.kept ? `${done} ${copy.let_go_kept}` : done;
}
