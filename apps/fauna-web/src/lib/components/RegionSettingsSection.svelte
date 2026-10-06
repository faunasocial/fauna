<script lang="ts">
  // `settings-region-section` — the region content plane's transparency surface
  // (`region-blocking.md` § The blocked render and the transparency surface):
  // the declared region and its source (read-only, the change path named — no
  // in-app override), each policy on the chain, when it was last checked, and
  // the staleness warning. A paint of the shared `RegionPlane::view`, never an
  // app-side fold — tui `region::settings_elements`, linux `region::paint_settings`.
  import { t } from '$lib/i18n/strings';
  import { IDS } from '$lib/generated/uiIds';
  import { regionView } from '$lib/region.svelte';

  let view = $derived(regionView());

  // Web formats times in JS: `format_unix_local` needs the OS timezone
  // database, which wasm lacks (`$lib/custody.ts`).
  function when(secs: number): string {
    return new Date(secs * 1000).toLocaleString();
  }

  function sourceText(key: string): string {
    switch (key) {
      case 'region.source_storefront':
        return t.region.source_storefront;
      case 'region.source_system_region':
        return t.region.source_system_region;
      case 'region.source_system_locale':
        return t.region.source_system_locale;
      default:
        return t.region.source_browser_locale;
    }
  }
</script>

{#if view}
  <section class="section" data-testid={IDS.SETTINGS_REGION_SECTION}>
    <h2>{t.region.section_title}</h2>
    {#if !view.declared}
      <p class="muted" data-testid={IDS.SETTINGS_REGION_DECLARED}>{t.region.none_declared}</p>
    {:else}
      <div class="field">
        <span data-testid={IDS.SETTINGS_REGION_DECLARED}>{t.region.declared({ region: view.declared.code })}</span>
      </div>
      <p class="muted" data-testid={IDS.SETTINGS_REGION_SOURCE}>{sourceText(view.declared.sourceLabelKey)}</p>
      {#if view.policies.length === 0}
        <p class="muted">{t.region.no_policy}</p>
      {/if}
      {#each view.policies as policy}
        <div class="field-group" data-testid={IDS.SETTINGS_REGION_POLICY_ITEM}>
          <span class="field-label" data-testid={IDS.SETTINGS_REGION_POLICY_AUTHORITY}>{t.region.policy_authority({ region: policy.region, authority: policy.authorityName })}</span>
          <span class="muted" data-testid={IDS.SETTINGS_REGION_POLICY_VERSION}>{t.region.policy_version({ sequence: String(policy.sequence), issued: when(policy.issuedAt) })}</span>
          {#if policy.state === 'inert'}
            <p class="muted" data-testid={IDS.SETTINGS_REGION_INERT_NOTICE}>{t.region.inert_notice({ version: String(policy.inertVersion ?? '') })}</p>
          {:else if policy.state === 'malformed'}
            <p class="muted" data-testid={IDS.SETTINGS_REGION_INERT_NOTICE}>{t.region.malformed_notice}</p>
          {/if}
        </div>
      {/each}
      {#if view.lastCheckedAt !== null}
        <p class="muted small" data-testid={IDS.SETTINGS_REGION_LAST_CHECKED}>{t.region.last_checked({ time: when(view.lastCheckedAt) })}</p>
      {/if}
      {#if view.stale}
        <p class="muted" data-testid={IDS.SETTINGS_REGION_STALE_WARNING}>{t.region.stale_warning}</p>
      {/if}
    {/if}
  </section>
{/if}
