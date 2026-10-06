<script lang="ts">
  import { identity } from '$lib/store';
  import { adminStats, setupStatus, type AdminStats, type SetupStatus } from '$lib/rpc';
  import { fetchNestInfo, type NestInfoResponse } from '$lib/api';
  import { onMount } from 'svelte';
  import MessageBanner from '$lib/components/MessageBanner.svelte';
  import { t } from '$lib/i18n/strings';
  import { byteSize } from '$lib/value-format';
  import { IDS } from '$lib/generated/uiIds';

  // The dashboard is a dumb renderer over three shared reads (no HTTP twins):
  //   • fauna.admin.stats   (adminStats)  — user count + total storage
  //   • fauna.setup.status  (setupStatus) — TLS-active status + registration posture
  //   • fauna.nest.info      (fetchNestInfo) — node domain, version
  // The HTTP `/admin/api/stats` twin (a richer ad-hoc shape) was deleted by the
  // WS-RPC-everywhere rip-out; its displayed fields are re-sourced from the
  // canonical kinds above.
  let stats = $state<AdminStats | null>(null);
  let setup = $state<SetupStatus | null>(null);
  let nestInfo = $state<NestInfoResponse | null>(null);
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

    const results = await Promise.allSettled([
      adminStats(id.secretHex),
      setupStatus(id.secretHex),
      fetchNestInfo(),
    ]);

    if (results[0].status === 'fulfilled') {
      stats = results[0].value;
    } else {
      // `allSettled` hands the rejection back on `.reason` — the stats read failed
      // for a reason the admin can act on (ui/README.md § Copy comprehensibility).
      const r: unknown = results[0].reason;
      error = t.admin.dashboard.load_error({
        message: r instanceof Error ? r.message : String(r),
      });
    }

    if (results[1].status === 'fulfilled') {
      setup = results[1].value;
    }

    if (results[2].status === 'fulfilled') {
      nestInfo = results[2].value;
    }

    loading = false;
  });

</script>

<h1 data-testid={IDS.ADMIN_DASHBOARD_HEADING}>{t.admin.dashboard.title}</h1>

<MessageBanner bind:error />

{#if loading}
  <p class="muted">{t.admin.dashboard.loading}</p>
{:else if error}
  <!-- error shown in banner above -->
{:else}
  <div class="grid">
    {#if nestInfo}
      <div class="card" data-testid={IDS.ADMIN_STAT_CARD}>
        <div class="card-label" data-testid={IDS.ADMIN_STAT_CARD_LABEL}>{t.admin.dashboard.nest_domain}</div>
        <div class="card-value" data-testid={IDS.ADMIN_STAT_CARD_VALUE}>{nestInfo.registration?.handle_domain ?? 'unknown'}</div>
      </div>

      <div class="card" data-testid={IDS.ADMIN_STAT_CARD}>
        <div class="card-label" data-testid={IDS.ADMIN_STAT_CARD_LABEL}>{t.admin.dashboard.version}</div>
        <div class="card-value mono" data-testid={IDS.ADMIN_STAT_CARD_VALUE}>{nestInfo.version ?? 'unknown'}</div>
      </div>
    {/if}

    {#if stats}
      <div class="card" data-testid={IDS.ADMIN_STAT_CARD}>
        <div class="card-label" data-testid={IDS.ADMIN_STAT_CARD_LABEL}>{t.common.users}</div>
        <div class="card-value" data-testid={IDS.ADMIN_STAT_CARD_VALUE}>{stats.total_users}</div>
      </div>

      <div class="card" data-testid={IDS.ADMIN_STAT_CARD}>
        <div class="card-label" data-testid={IDS.ADMIN_STAT_CARD_LABEL}>{t.admin.dashboard.total_storage}</div>
        <div class="card-value" data-testid={IDS.ADMIN_STAT_CARD_VALUE}>{byteSize(stats.total_storage_bytes)}</div>
      </div>
    {/if}

    {#if setup}
      <div class="card" data-testid={IDS.ADMIN_STAT_CARD}>
        <div class="card-label" data-testid={IDS.ADMIN_STAT_CARD_LABEL}>{t.admin.dashboard.email}</div>
        <div class="card-value" data-testid={IDS.ADMIN_STAT_CARD_VALUE}>
          <span class="badge" class:active={setup.email_enabled} class:inactive={!setup.email_enabled}>
            {setup.email_enabled ? t.common.enabled : t.common.disabled}
          </span>
        </div>
      </div>

      <div class="card" data-testid={IDS.ADMIN_STAT_CARD}>
        <div class="card-label" data-testid={IDS.ADMIN_STAT_CARD_LABEL}>{t.admin.dashboard.tls}</div>
        <div class="card-value" data-testid={IDS.ADMIN_STAT_CARD_VALUE}>
          <span class="badge" class:active={setup.tls_active} class:inactive={!setup.tls_active}>
            {setup.tls_active ? t.common.active : t.common.inactive}
          </span>
        </div>
      </div>
    {/if}

    {#if setup?.registration_mode}
      <!-- Open = anyone may request an account (the `open` and
           `invite_required` postures); closed = only the admin admits. -->
      {@const registrationOpen = setup.registration_mode !== 'closed'}
      <div class="card" data-testid={IDS.ADMIN_STAT_CARD}>
        <div class="card-label" data-testid={IDS.ADMIN_STAT_CARD_LABEL}>{t.admin.dashboard.registration}</div>
        <div class="card-value" data-testid={IDS.ADMIN_STAT_CARD_VALUE}>
          <span class="badge" class:active={registrationOpen} class:inactive={!registrationOpen}>
            {registrationOpen ? t.common.open : t.common.closed}
          </span>
        </div>
      </div>
    {/if}
  </div>

{/if}

<style>
  h1 { margin-bottom: 1.5rem; font-size: 1.5rem; }
  .muted { color: var(--text-muted, #8b949e); }
  .mono { font-family: monospace; font-size: 0.875rem; }

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

  .badge {
    display: inline-block;
    font-size: 0.75rem;
    font-weight: 600;
    padding: 0.125rem 0.5rem;
    border-radius: 4px;
  }
  .badge.active {
    background: rgba(63, 185, 80, 0.15);
    color: var(--success, #3fb950);
  }
  .badge.inactive {
    background: rgba(248, 81, 73, 0.15);
    color: var(--danger, #f85149);
  }

</style>
