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
  import { nestRuleLine } from '#lib/nestRulesPaint';

  let summary = $derived(nestRulesSummary());

  onMount(() => dismissNestRulesNotice());

  // Web formats times in JS: `format_unix_local` needs the OS timezone database,
  // which wasm lacks (`#lib/custody.ts`).
  function when(secs: number): string {
    return new Date(secs * 1000).toLocaleString();
  }
</script>

<!-- Shown when a rule is listed, or when none of the document applies here
     (`NestRulesSummary::is_shown`); only an applied empty document paints
     nothing. -->
{#if summary && (summary.rules.length > 0 || summary.not_applied)}
  <section class="section" data-testid={IDS.SETTINGS_NEST_RULES_SECTION}>
    <h2>{t.nest_rules.settings_title({ nest: summary.domain })}</h2>
    <p class="muted" data-testid={IDS.SETTINGS_NEST_RULES_VERSION}>{t.nest_rules.version({ sequence: String(summary.sequence), issued: when(summary.issued_at) })}</p>
    {#if summary.not_applied_sentence}
      <!-- The whole document's note, unscoped — a rule's own sits in its item. -->
      <p class="muted" data-testid={IDS.SETTINGS_NEST_RULES_INERT_NOTICE}>{resolveLocalized(summary.not_applied_sentence)}</p>
    {/if}
    {#each summary.rules as rule, i (i)}
      <!-- The item is the rule's container, so its own not-applied note is a
           descendant a scoped query finds (convention 1). -->
      <div class="field-group" data-testid={IDS.SETTINGS_NEST_RULES_ITEM} data-index={i}>
        <span>{nestRuleLine(rule)}</span>
        {#if rule.inert}
          <p class="muted" data-testid={IDS.SETTINGS_NEST_RULES_INERT_NOTICE}>{resolveLocalized(rule.inert)}</p>
        {/if}
      </div>
    {/each}
  </section>
{/if}
