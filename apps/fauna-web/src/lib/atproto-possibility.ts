/**
 * The AT Protocol settings page's gated controls, painted from the shared
 * machine's `Possibility` answers (`docs/goal/ui/atproto.md` § Errors & edge
 * cases; `docs/goal/ui/README.md` § Offer only what is possible, rule 6). The
 * page derives none of it: each rung, `atproto-delegation-authorize`,
 * `atproto-contest` and the retire-identity tick paint from
 * `AtprotoSettingsMachine::*_possibility`, and the machine's gestures refuse
 * on the same readings.
 */
import { paintPossibility, type Possibility } from './possibility.ts';
import type { LocalizedText } from './i18n/localized.ts';

/** The four answers the wasm `AtprotoSettingsMachine` gives, as serde JSON. */
export interface AtprotoPossibilitySource {
  rungPossibilityJson(level: string): string;
  authorizePossibilityJson(): string;
  contestPossibilityJson(): string;
  retireIdentityPossibilityJson(): string;
}

export type Paint = ReturnType<typeof paintPossibility>;

const STILL_LOADING: Possibility = {
  state: 'blocked',
  reason: { key: 'common.still_loading' } as LocalizedText,
};

/** Before the machine exists a control is shown but dead, saying it is still
 *  loading. */
function before(): Paint {
  return paintPossibility(STILL_LOADING);
}

export function rungPaint(machine: AtprotoPossibilitySource | null, level: string): Paint {
  if (!machine) return before();
  return paintPossibility(JSON.parse(machine.rungPossibilityJson(level)) as Possibility);
}

export function authorizePaint(machine: AtprotoPossibilitySource | null): Paint {
  if (!machine) return before();
  return paintPossibility(JSON.parse(machine.authorizePossibilityJson()) as Possibility);
}

export function contestPaint(machine: AtprotoPossibilitySource | null): Paint {
  if (!machine) return paintPossibility({ state: 'absent' });
  return paintPossibility(JSON.parse(machine.contestPossibilityJson()) as Possibility);
}

export function retireIdentityPaint(machine: AtprotoPossibilitySource | null): Paint {
  if (!machine) return before();
  return paintPossibility(JSON.parse(machine.retireIdentityPossibilityJson()) as Possibility);
}

/** The distinct reasons of a set of paints, in first-seen order, so a reason
 *  shared by several rungs is painted once. */
export function distinctReasons(paints: Paint[]): LocalizedText[] {
  const seen = new Set<string>();
  const out: LocalizedText[] = [];
  for (const p of paints) {
    if (!p.reason) continue;
    const k = JSON.stringify(p.reason);
    if (seen.has(k)) continue;
    seen.add(k);
    out.push(p.reason);
  }
  return out;
}

/** What the page paints: every control, or — when the machine could not be
 *  built — the one unavailable line in place of all of them (rule 7). */
export function pageMode(buildFailed: boolean): 'controls' | 'unavailable' {
  return buildFailed ? 'unavailable' : 'controls';
}
