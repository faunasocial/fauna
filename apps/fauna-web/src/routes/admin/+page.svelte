<script lang="ts">
  import { identity } from '#lib/store';
  import { adminDashboardSnapshot, type AdminStatCard } from '#lib/rpc';
  import { onMount } from 'svelte';
  import MessageBanner from '#lib/components/MessageBanner.svelte';
  import { t } from '#lib/i18n/strings';
  import { resolveLocalized } from '#lib/i18n/localized';
  import { IDS } from '#lib/generated/uiIds';

  // The dashboard is a dumb renderer over the shared snapshot (`admin.md` § 1.
  // Dashboard → *The card set*): the fold, its order and its labels/values live
  // in shared Rust; this page resolves each `LocalizedText` and paints it.
  let cards = $state<AdminStatCard[]>([]);
  let error = $state('');
  let loading = $state(true);

  // The former read-only paired-nests list (admin-dashboard-pairing-item, fed by
  // the retired GET /admin/api/pairings) was removed with the per-user pairing
  // redesign: pairing is the user's own concern, surfaced on the user-settings
  // /settings/nests page (fauna.pair.list), not an admin-dashboard view.
  // The admin's only pairing control is the admin knob on the Services page
  // (admin-service-pairing-toggle). See linked-nests.md § The surface / § 53.

  onMount(async () => {
    const id = $identity;
    if (!id?.secretHex) return;

    try {
      const snapshot = await adminDashboardSnapshot(id.secretHex);
      cards = snapshot.stats;
      // A failed read still leaves the cards the other reads produced.
      if (snapshot.error) {
        error = t.admin.dashboard.load_error({ message: snapshot.error });
      }
    } catch (r: unknown) {
      error = t.admin.dashboard.load_error({
        message: r instanceof Error ? r.message : String(r),
      });
    }

    loading = false;
  });

</script>

<h1 data-testid={IDS.ADMIN_DASHBOARD_HEADING}>{t.admin.dashboard.title}</h1>

<MessageBanner bind:error />

{#if loading}
  <p class="muted">{t.admin.dashboard.loading}</p>
{:else}
  <div class="grid">
    {#each cards as card}
      <div class="card" data-testid={IDS.ADMIN_STAT_CARD}>
        <div class="card-label" data-testid={IDS.ADMIN_STAT_CARD_LABEL}>{resolveLocalized(card.label)}</div>
        <div class="card-value" data-testid={IDS.ADMIN_STAT_CARD_VALUE}>{resolveLocalized(card.value)}</div>
      </div>
    {/each}
  </div>
{/if}

<style>
  h1 { margin-bottom: 1.5rem; font-size: 1.5rem; }
  .muted { color: var(--text-muted, #8b949e); }

  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(200px, 1fr));
    gap: 1rem;
  }
  .card {
    padding: 1rem;
    border: 1px solid var(--border, #30363d);
    border-radius: 8px;
    background: var(--bg-surface, #161b22);
  }
  .card-label {
    font-size: 0.75rem;
    color: var(--text-muted, #8b949e);
    text-transform: uppercase;
    letter-spacing: 0.05em;
    margin-bottom: 0.375rem;
  }
  .card-value {
    font-size: 1.25rem;
    font-weight: 600;
  }

</style>
