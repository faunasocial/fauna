<script lang="ts">
  // The every-page critical-alerts banner (`docs/goal/behavior/critical-alerts.md`
  // § Mechanism → *Rendering contract*; ui.yaml `global:` `critical-alerts` /
  // `critical-alert[N]`, user-approved 2026-07-23). The web twin of
  // `apps/fauna-linux/src/critical_alerts.rs` / `apps/fauna-tui/src/critical_alerts.rs`.
  //
  // Deliberately minimal (destructive-styled text, no interaction): mounted once
  // at the shell root (`+layout.svelte`) so it renders on every authenticated
  // page, including the admin/settings shells that bypass the normal sidebar
  // layout. Non-dismissable by design — an alert disappears only when the
  // condition that raised it re-checks clean.
  import { criticalAlerts } from '$lib/critical-alerts';
  import { resolveLocalized } from '$lib/i18n/localized';
  import { IDS } from '$lib/generated/uiIds';
</script>

{#if $criticalAlerts.length > 0}
  <div class="critical-alerts" data-testid={IDS.CRITICAL_ALERTS} role="alert">
    {#each $criticalAlerts as alert (alert.key)}
      <div class="critical-alert" data-testid={IDS.CRITICAL_ALERT}>
        {alert.lines.map(resolveLocalized).join(' ')}
      </div>
    {/each}
  </div>
{/if}

<style>
  .critical-alerts {
    display: flex;
    flex-direction: column;
    gap: 0.375rem;
    padding: 0.625rem 1rem;
    background: color-mix(in srgb, var(--danger, #ef4444) 18%, transparent);
    border-bottom: 2px solid var(--danger, #ef4444);
  }
  .critical-alert {
    color: var(--danger, #ef4444);
    font-weight: 600;
    font-size: 0.875rem;
  }
</style>
