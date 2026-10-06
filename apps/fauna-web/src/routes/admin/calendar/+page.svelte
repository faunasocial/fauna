<script lang="ts">
  // Admin Calendar page (`admin-calendar`) — the deployment-wide CalDAV-enable
  // toggle (admin.md § 8 Calendar / caldav-server.md § Independent enablement).
  // The single-toggle sibling of `admin-mail`'s mail-enable switch: email and
  // calendar are two independently enableable features of ONE MDA bridge (the MDA
  // runs iff `mail_enabled || caldav_enabled`), so each gets its own admin enable
  // toggle. This page is the calendar toggle's admin home.
  //
  // A dumb renderer of the shared `CaldavPolicyMachine`
  // (libs/fauna-client-mail-settings::caldav_policy, WASM twin WasmCaldavPolicyMachine):
  // build over the singleton WS-RPC client → hydrate() (via the Admin read twin
  // `fauna.bridges.get_mail_config`, whose `caldav_enabled` falls back to
  // `mail_enabled` until explicitly set) → render snapshot() → dispatch(action) →
  // re-render. No policy logic in the SPA (priority #2). Lifts the linux lead page
  // (apps/fauna-linux/src/settings/admin_calendar.rs). Read + write are both LIVE
  // (`set_caldav_enabled` + the MDA gate already existed — no nest work).
  //
  // The toggle dispatches on change and re-reads the persisted state from the
  // post-dispatch snapshot. Errors surface via the snapshot's `error` →
  // `error-message` (MessageBanner), never faked green.
  // UX/IDs: tests/e2e-unified/ui.yaml `admin-calendar` page.
  import { identity } from '$lib/store';
  import { caldavPolicyMachine } from '$lib/rpc';
  import { ensureWasm, parsePort } from '$lib/wasm';
  import { onMount } from 'svelte';
  import MessageBanner from '$lib/components/MessageBanner.svelte';
  import { t } from '$lib/i18n/strings';
  import { IDS } from '$lib/generated/uiIds';

  // Mirrors the shared serde JSON (snake_case across serde_wasm_bindgen; the status
  // enum serializes as its variant-name string).
  interface CaldavPolicySnapshot {
    caldav_enabled: boolean;
    caldav_port: number;
    status: string; // "Idle" | "Loading" | "Working"
    error: string | null;
  }

  let machine: Awaited<ReturnType<typeof caldavPolicyMachine>> | null = null;
  let snap = $state<CaldavPolicySnapshot | null>(null);
  let error = $state('');

  // The toggle dispatches on change, so it is rendered one-way (`checked={caldavEnabled}`)
  // — a programmatic re-seed updates the checkbox without firing its change handler.
  let caldavEnabled = $state(false);
  // The port is an explicit text_input + save button (the admin-mail pattern): bound
  // two-way to a string while the admin edits, committed via `set_caldav_port` on save.
  let caldavPort = $state('8443');

  const busy = $derived(snap?.status === 'Working' || snap?.status === 'Loading');

  function applySnapshot(): void {
    if (!machine) return;
    snap = machine.snapshot() as unknown as CaldavPolicySnapshot;
    error = snap?.error ?? '';
    if (!snap) return;
    caldavEnabled = snap.caldav_enabled;
    caldavPort = String(snap.caldav_port);
  }

  onMount(async () => {
    const id = $identity;
    if (!id?.secretHex) return;
    try {
      await ensureWasm();
      machine = await caldavPolicyMachine(id.secretHex);
      await machine.hydrate();
      applySnapshot();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  });

  async function handleToggleEnabled(e: Event): Promise<void> {
    if (!machine) return;
    const enabled = (e.currentTarget as HTMLInputElement).checked;
    try {
      await machine.dispatch({ SetCaldavEnabled: { enabled } });
    } catch {
      /* the snapshot carries the error */
    }
    applySnapshot();
  }

  async function handleSavePort(): Promise<void> {
    if (!machine) return;
    // Validate locally before dispatching — fauna_core::format::parse_port
    // (a u16 in [1, 65535]), the same validator android/apple/windows already
    // consume via UniFFI.
    const port = parsePort(caldavPort);
    if (port === undefined) {
      error = t.admin.calendar_page.caldav_port_invalid;
      return;
    }
    try {
      await machine.dispatch({ SetCaldavPort: { port } });
    } catch {
      /* the snapshot carries the error */
    }
    applySnapshot();
  }
</script>

<h1 data-testid={IDS.ADMIN_CALENDAR_HEADING}>{t.admin.calendar_page.title}</h1>
<p class="muted small">{t.admin.calendar_page.description}</p>

<MessageBanner bind:error />

<!-- ── CalDAV-enable master toggle (set_caldav_enabled) ── -->
<section class="section">
  <label class="switch-row">
    <span>
      <span class="label-text">{t.admin.calendar_page.enabled_label}</span>
      <span class="muted small">{t.admin.calendar_page.enabled_subtitle}</span>
    </span>
    <input
      type="checkbox"
      data-testid={IDS.ADMIN_CALENDAR_ENABLED_TOGGLE}
      checked={caldavEnabled}
      disabled={busy && !snap}
      onchange={handleToggleEnabled}
    />
  </label>
</section>

<!-- ── Admin-set CalDAV port (set_caldav_port) ── -->
<section class="section">
  <div class="field-row">
    <span>
      <span class="label-text">{t.admin.calendar_page.caldav_port_label}</span>
      <span class="muted small">{t.admin.calendar_page.caldav_port_desc}</span>
    </span>
    <span class="field-controls">
      <input
        type="text"
        inputmode="numeric"
        class="port-input"
        data-testid={IDS.ADMIN_CALENDAR_CALDAV_PORT_INPUT}
        bind:value={caldavPort}
        disabled={busy && !snap}
      />
      <button
        type="button"
        data-testid={IDS.ADMIN_CALENDAR_CALDAV_PORT_SAVE_BUTTON}
        disabled={busy}
        onclick={handleSavePort}
      >
        {t.admin.calendar_page.caldav_port_save}
      </button>
    </span>
  </div>
</section>

<style>
  h1 { margin-bottom: 0.25rem; font-size: 1.5rem; }
  .muted { color: var(--text-muted, #8b949e); }
  .small { font-size: 0.8rem; }
  .section {
    margin: 1.5rem 0;
    padding: 1rem;
    border: 1px solid var(--border, #30363d);
    border-radius: 8px;
    background: var(--bg-surface, #161b22);
  }
  .switch-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 1rem;
    margin: 0.75rem 0;
  }
  .switch-row span { display: flex; flex-direction: column; gap: 0.125rem; }
  .label-text {
    font-size: 0.8rem;
    color: var(--text, #e6edf3);
    font-weight: 500;
  }
  .field-row {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 1rem;
    margin: 0.75rem 0;
  }
  .field-row > span:first-child { display: flex; flex-direction: column; gap: 0.125rem; }
  .field-controls { display: flex; align-items: center; gap: 0.5rem; }
  .port-input {
    width: 6rem;
    padding: 0.35rem 0.5rem;
    font-size: 0.85rem;
    border: 1px solid var(--border, #30363d);
    border-radius: 6px;
    background: var(--bg, #0d1117);
    color: var(--text, #e6edf3);
  }
</style>
