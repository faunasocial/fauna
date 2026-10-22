<script lang="ts">
  // `settings-nest-rules-section` — the rules surface (`nest-rules.md`
  // § Acceptance at joining, and a change of rules → *The rules surface*): the
  // rules in force for the nest this session reads through — per rule the verdict,
  // the level and the nest's own reason — its version and issue time, and any rule
  // not applied on this device. A paint of the shared book's `summary` (every label
  // already chosen in `fauna_client_policy`), never an app-side fold — the region
  // section's twin, tui `nest_rules::settings_elements`.
  //
  // Opening this surface is reading the change: it clears the one-line notice.
  import { onMount } from 'svelte';
  import { t } from '#lib/i18n/strings';
  import { resolveLocalized } from '#lib/i18n/localized';
  import { IDS } from '#lib/generated/uiIds';
  import { nestRulesSummary, dismissNestRulesNotice } from '#lib/nestRules.svelte';

  let summary = $derived(nestRulesSummary());

  onMount(() => dismissNestRulesNotice());

  // Web formats times in JS: `format_unix_local` needs the OS timezone database,
  // which wasm lacks (`#lib/custody.ts`).
  function when(secs: number): string {
    return new Date(secs * 1000).toLocaleString();
  }

  function line(rule: NonNullable<typeof summary>['rules'][number]): string {
    const verdict = resolveLocalized(rule.verdict);
    return rule.level
      ? t.nest_rules.rule_line({ verdict, level: resolveLocalized(rule.level), reason: rule.reason })
      : t.nest_rules.rule_line_no_level({ verdict, reason: rule.reason });
  }
</script>

{#if summary && (summary.rules.length > 0 || summary.not_applied)}
  <section class="section" data-testid={IDS.SETTINGS_NEST_RULES_SECTION}>
    <h2>{t.nest_rules.settings_title({ nest: summary.domain })}</h2>
    <p class="muted" data-testid={IDS.SETTINGS_NEST_RULES_VERSION}>{t.nest_rules.version({ sequence: String(summary.sequence), issued: when(summary.issued_at) })}</p>
    {#each summary.rules as rule, i (i)}
      <div class="field-group">
        <span data-testid={IDS.SETTINGS_NEST_RULES_ITEM} data-index={i}>{line(rule)}</span>
        {#if rule.inert}
          <p class="muted" data-testid={IDS.SETTINGS_NEST_RULES_INERT_NOTICE} data-index={i}>{resolveLocalized(rule.inert)}</p>
        {/if}
      </div>
    {/each}
  </section>
{/if}
