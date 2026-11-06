// The paint both rules surfaces share — the joining surface
// (`NestRulesJoining.svelte`) and the settings one
// (`NestRulesSettingsSection.svelte`): one rule reads the same wherever it is
// shown (`nest-rules.md` § Acceptance at joining). tui `nest_rules::rule_line`.
// Free of the wasm core, so the pre-login onboarding chunk can take it.

import { t } from '#lib/i18n/strings';
import { resolveLocalized } from '#lib/i18n/localized';
import type { NestRulesSummaryValue } from '#lib/wasm';

/** The language the admin's reason is picked in — the app's UI language (tui
 *  `UI_LANG`). */
export const NEST_RULES_LANG = 'en';

/** One rule: verdict, level where the rule has one, and the nest's own reason
 *  verbatim. */
export function nestRuleLine(rule: NestRulesSummaryValue['rules'][number]): string {
  const verdict = resolveLocalized(rule.verdict);
  return rule.level
    ? t.nest_rules.rule_line({ verdict, level: resolveLocalized(rule.level), reason: rule.reason })
    : t.nest_rules.rule_line_no_level({ verdict, reason: rule.reason });
}
