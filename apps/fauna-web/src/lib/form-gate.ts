// How a form's submit paints and obeys its shared draft-validity answer
// (`docs/goal/ui/README.md` § Offer only what is possible, rule 4's form case
// and rule 7; the page rulings in `docs/goal/ui/feed.md`, `search.md` and
// `nostr.md` § Errors & edge cases).
//
// The answer is shared Rust — one predicate per form (`fauna_feed::forms`,
// `fauna_client_search::query_possibility`, `fauna_client_bridges`'s
// `nostr_forms`, `fauna_protocol::nostr_relay::add_relay_possibility`), read
// over wasm as a `Possibility` (`#lib/wasm`'s `feed*Possibility`,
// `searchQueryPossibility`, `nostr*Possibility`). This module only maps that
// answer onto paint and onto the submit's refusal: the SPA never tests a draft
// itself. The tui twin is `Errors::refuse` (`apps/fauna-tui/src/app.rs`).
//
// Kept free of `#lib/wasm` (which pulls the SvelteKit `$app/paths` alias plain
// `deno test` cannot resolve) — the caller hands over the predicate call, the
// `offline-gate.ts` seam for the same reason; `form-gate.test.ts` pins it.

import { resolveLocalized, type LocalizedText } from './i18n/localized.ts';
import { t } from './i18n/strings.ts';
import type { Possibility } from './mail-gate.ts';
import { andPossibility } from './read-gate.ts';

export type { Possibility };

/** A submit's paint. `reason` is the blocked reason resolved through the
 *  catalog — web's disabled-with-a-reason idiom puts it on the control's
 *  `title` (the analogue of linux's tooltip and apple's `.help`). */
export interface FormGate {
  /** `false` only for `absent`: the surface is not rendered at all (rule 2). */
  shown: boolean;
  disabled: boolean;
  /** `''` when there is nothing to say (possible, absent, not readable yet). */
  reason: string;
}

/** Ask a predicate, or `null` while it cannot be read — the wasm core not yet
 *  initialized (a wasm face throws until `ensureWasm()` resolves). */
export function askPossibility(ask: () => Possibility): Possibility | null {
  try {
    return ask();
  } catch {
    return null;
  }
}

/** A draft's own answer folded AFTER the page's gate (`readAnswer.and(draftAnswer)`
 *  — `docs/goal/behavior/admin.md` § Errors & edge cases, a form's submit is
 *  live only for a draft the nest would accept): `null` while the gate is
 *  unreadable; the gate's own answer while it is not possible (a page still
 *  loading says so first, and the draft is not asked); else the draft's
 *  answer (`null` while the wasm core is not up). The tui twin is
 *  `action_possibility`'s `read_possibility(..).and(draft_possibility(..))`. */
export function afterGate(
  gate: Possibility | null,
  draft: () => Possibility,
): Possibility | null {
  if (gate === null) return null;
  if (gate.state !== 'possible') return gate;
  const answer = askPossibility(draft);
  return answer === null ? null : andPossibility(gate, answer);
}

/** How a blocked reason becomes text: `resolveLocalized` by default;
 *  `resolveLocalizedNested` for an answer whose reason carries a nested key
 *  (the features gate's `{window}`). */
export type ReasonResolver = (reason: LocalizedText) => string;

/** The submit's paint from the predicate's answer. `null` (unreadable yet) is
 *  disabled with no reason — unknown is not possible (rule 5). */
export function formGate(
  answer: Possibility | null,
  resolve: ReasonResolver = resolveLocalized,
): FormGate {
  if (answer === null) return { shown: true, disabled: true, reason: '' };
  switch (answer.state) {
    case 'possible':
      return { shown: true, disabled: false, reason: '' };
    case 'blocked':
      return { shown: true, disabled: true, reason: resolve(answer.reason) };
    case 'absent':
      return { shown: false, disabled: true, reason: '' };
  }
}

/** The submit's refusal, asked again at the click before anything runs: the
 *  text to put on the page's `error-message`, or `null` when the submit may go
 *  on. A blocked answer refuses with its own painted reason; an unreadable one
 *  with the shared "still loading"; an absent one (the surface should not have
 *  been on screen) refuses with nothing to add. */
export function formRefusal(
  answer: Possibility | null,
  resolve: ReasonResolver = resolveLocalized,
): string | null {
  if (answer === null) return t.common.still_loading;
  switch (answer.state) {
    case 'possible':
      return null;
    case 'blocked':
      return resolve(answer.reason);
    case 'absent':
      return '';
  }
}
