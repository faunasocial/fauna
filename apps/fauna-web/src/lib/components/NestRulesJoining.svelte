<script lang="ts">
  // `onboarding-nest-rules` — the joining surface (`nest-rules.md` § Acceptance
  // at joining): joining is acceptance, so `invite_request`, and `handle_entry`
  // while its Continue signs in, show the nest's rules directly above the terms
  // notice. A paint of the shared machine's `joining_nest_rules` (which already
  // decided whether this page shows them): the heading naming the nest, the
  // not-applied sentence after it when none of the document applies here, then
  // one `onboarding-nest-rules-item` per rule. tui `nest_rules::joining_elements`.
  import { t } from '#lib/i18n/strings';
  import { resolveLocalized } from '#lib/i18n/localized';
  import { IDS } from '#lib/generated/uiIds';
  import type { NestRulesSummaryValue } from '#lib/wasm';
  import { nestRuleLine } from '#lib/nestRulesPaint';

  let { summary }: { summary: NestRulesSummaryValue } = $props();

  let heading = $derived(
    summary.not_applied_sentence
      ? `${t.nest_rules.joining_heading({ nest: summary.domain })} ${resolveLocalized(summary.not_applied_sentence)}`
      : t.nest_rules.joining_heading({ nest: summary.domain }),
  );
</script>

<div class="nest-rules-joining">
  <p data-testid={IDS.ONBOARDING_NEST_RULES}>{heading}</p>
  {#if summary.rules.length > 0}
    <ul>
      {#each summary.rules as rule, i (i)}
        <li data-testid={IDS.ONBOARDING_NEST_RULES_ITEM} data-index={i}>{nestRuleLine(rule)}</li>
      {/each}
    </ul>
  {/if}
</div>
