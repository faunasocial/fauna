<script lang="ts">
  import { onMount } from 'svelte';
  import { identity } from '#lib/store';
  import { adminLogs, adminAuditLog } from '#lib/rpc';
  import type { LogEntry, AuditLogHandle, AuditLogView } from '#lib/wasm';
  import { t } from '#lib/i18n/strings';
  import { resolveLocalized } from '#lib/i18n/localized';
  import { localTimestamp } from '#lib/value-format';
  import LogsView from '#lib/components/LogsView.svelte';
  import { IDS } from '#lib/generated/uiIds';

  // Admin view of the NEST's `fauna-log` ring (observability.md § Surfaces),
  // fetched once over the admin-scoped `fauna.admin.logs` WS-RPC and rendered
  // with the SAME `LogsView` widget as the client's own Settings → Logs page.
  // No Clear — there is no admin RPC to wipe the nest ring; the filter narrows
  // the held fetched Vec in memory (no refetch), mirroring linux
  // `views/admin.rs build_admin_logs_page`. `admin-nav-back` is provided by the
  // admin shell (routes/admin/+layout.svelte). Redaction binds the nest call sites.
  //
  // Below the log lines sits the admin audit log section (`admin.md` § The admin
  // audit log on `admin-logs`): its rows, chain-check line and more cursor are
  // all the shared `WasmAuditLog` view; this page adds only the local time ahead
  // of each row. A failed read lands on `error-message` and keeps the rows shown.

  let entries = $state<LogEntry[]>([]);
  let error = $state('');
  let audit = $state<AuditLogView | null>(null);
  let auditLog: AuditLogHandle | null = null;
  let auditBusy = $state(false);

  const message = (e: unknown) => (e instanceof Error ? e.message : String(e));

  async function auditRead(read: () => Promise<AuditLogView>) {
    auditBusy = true;
    try {
      audit = await read();
    } catch (e) {
      error = message(e);
    } finally {
      auditBusy = false;
    }
  }

  onMount(() => {
    const s = $identity?.secretHex;
    if (!s) return;
    adminLogs(s).then(
      (r) => (entries = r),
      (e) => (error = message(e)),
    );
    adminAuditLog(s).then(
      (h) => {
        auditLog = h;
        return auditRead(() => h.load());
      },
      (e) => (error = message(e)),
    );
    return () => auditLog?.free();
  });
</script>

<svelte:head><title>{t.common.admin} · {t.admin.logs_page.title}</title></svelte:head>

<h1 data-testid={IDS.ADMIN_LOGS_HEADING}>{t.admin.logs_page.title}</h1>

{#if error}
  <p class="error" data-testid={IDS.ERROR_MESSAGE}>{error}</p>
{/if}

<p class="muted">{t.admin.logs_page.description}</p>

<LogsView {entries} />

<section data-testid={IDS.ADMIN_AUDIT_SECTION}>
  <h2>{t.admin.logs_page.audit_title}</h2>
  {#if audit}
    <ul class="audit">
      {#each audit.rows as row (row.id)}
        <li data-testid={IDS.ADMIN_AUDIT_ENTRY}>{localTimestamp(row.ts_ms)}  {resolveLocalized(row.text)}</li>
      {/each}
    </ul>
    <p class="muted" data-testid={IDS.ADMIN_AUDIT_INTEGRITY_STATUS}>{resolveLocalized(audit.integrity_text)}</p>
  {/if}
  <button
    data-testid={IDS.ADMIN_AUDIT_MORE_BUTTON}
    disabled={auditBusy || !auditLog || audit?.more_before_id == null}
    onclick={() => auditLog && auditRead(() => auditLog!.more())}
  >{t.admin.logs_page.audit_more}</button>
</section>

<style>
  h1 { font-size: 1.25rem; margin-bottom: 0.5rem; }
  h2 { font-size: 1.05rem; margin: 1.5rem 0 0.5rem; }
  .audit { list-style: none; padding: 0; margin: 0 0 0.5rem; font-size: 0.875rem; }
  .audit li { white-space: pre-wrap; padding: 0.15rem 0; }
  .error { color: var(--color-error, #f85149); }
  .muted { color: var(--text-muted, #8b949e); margin-bottom: 1rem; font-size: 0.875rem; }
</style>
