<script lang="ts">
  // The one-line notice when the session nest changed its rules (`nest-rules.md`
  // § Acceptance at joining, and a change of rules): names the nest, with the way
  // to the rules surface — Settings → Status, where opening the surface reads the
  // change and clears it. The text is the shared frame (`nest_rules.changed_notice`);
  // the notice reuses the app's shell notice surface and adds no element id.
  import { t } from '#lib/i18n/strings';
  import { nestRulesChangedNotice } from '#lib/nestRules.svelte';

  let domain = $derived(nestRulesChangedNotice());
</script>

{#if domain !== null}
  <a href="/app/settings" class="nest-rules-notice">{t.nest_rules.changed_notice({ nest: domain })}</a>
{/if}

<style>
  .nest-rules-notice {
    display: block;
    padding: 0.5rem 0.75rem;
    margin-bottom: 0.5rem;
    border: 1px solid var(--border, currentColor);
    border-radius: 0.375rem;
    text-decoration: none;
    color: inherit;
  }
</style>
