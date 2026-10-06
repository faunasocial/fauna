<script lang="ts">
  import { onMount } from 'svelte';
  import { identity } from '$lib/store';
  import { adminLogs } from '$lib/rpc';
  import type { LogEntry } from '$lib/wasm';
  import { t } from '$lib/i18n/strings';
  import LogsView from '$lib/components/LogsView.svelte';
  import { IDS } from '$lib/generated/uiIds';

  // Admin view of the NEST's `fauna-log` ring (observability.md § Surfaces),
  // fetched once over the admin-scoped `fauna.admin.logs` WS-RPC and rendered
  // with the SAME `LogsView` widget as the client's own Settings → Logs page.
  // No Clear — there is no admin RPC to wipe the nest ring; the filter narrows
  // the held fetched Vec in memory (no refetch), mirroring linux
  // `views/admin.rs build_admin_logs_page`. `admin-nav-back` is provided by the
  // admin shell (routes/admin/+layout.svelte). Redaction binds the nest call sites.

  let entries = $state<LogEntry[]>([]);
  let error = $state('');

  onMount(async () => {
    const s = $identity?.secretHex;
    if (!s) return;
    try {
      entries = await adminLogs(s);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  });
</script>

<svelte:head><title>{t.common.admin} · {t.admin.logs_page.title}</title></svelte:head>

<h1 data-testid={IDS.ADMIN_LOGS_HEADING}>{t.admin.logs_page.title}</h1>

{#if error}
  <p class="error" data-testid={IDS.ERROR_MESSAGE}>{error}</p>
{/if}

<p class="muted">{t.admin.logs_page.description}</p>

<LogsView {entries} />

<style>
  h1 { font-size: 1.25rem; margin-bottom: 0.5rem; }
  .error { color: var(--color-error, #f85149); }
  .muted { color: var(--text-muted, #8b949e); margin-bottom: 1rem; font-size: 0.875rem; }
</style>
