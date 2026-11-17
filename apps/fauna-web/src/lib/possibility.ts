/**
 * `fauna_core::possibility::Possibility` on web — the one answer every gated
 * affordance's shared-Rust predicate gives (`docs/goal/ui/README.md` § Offer
 * only what is possible, rule 6). The SPA never decides an action's enabled,
 * disabled-with-reason or hidden state itself: it paints from this value.
 */
import type { LocalizedText } from './i18n/localized.ts';

/** `fauna_core::possibility::Possibility`'s serde form. */
export type Possibility =
  | { state: 'possible' }
  | { state: 'blocked'; reason: LocalizedText }
  | { state: 'absent' };

/** The paint an answer asks for: absent → not shown; blocked → shown disabled
 *  with its reason; possible → shown live. */
export function paintPossibility(
  answer: Possibility,
): { shown: boolean; disabled: boolean; reason: LocalizedText | null } {
  switch (answer.state) {
    case 'possible':
      return { shown: true, disabled: false, reason: null };
    case 'blocked':
      return { shown: true, disabled: true, reason: answer.reason };
    case 'absent':
      return { shown: false, disabled: true, reason: null };
  }
}
