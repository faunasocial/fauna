<script lang="ts">
  // Admin Files page (`admin-files`) — the deployment-wide WebDAV-enable
  // toggle (admin.md § Files / webdav-server.md § Independent enablement).
  // The files sibling of `admin-contacts`'s CardDAV-enable switch: email,
  // calendar, contacts, and files are four independently enableable features of
  // ONE MDA bridge (the MDA runs iff
  // `mail_enabled || caldav_enabled || carddav_enabled || webdav_enabled`),
  // so each gets its own admin enable toggle. No port field — WebDAV rides the
  // shared DAV listener admin-calendar's port input governs.
  //
  // A dumb renderer of the shared `WebdavPolicyMachine`
  // (libs/fauna-client-mail-settings::webdav_policy, WASM twin
  // WasmWebdavPolicyMachine): build over the singleton WS-RPC client →
  // hydrate() (via the Admin read twin `fauna.bridges.get_mail_config`, whose
  // `webdav_enabled` falls back to `mail_enabled` until explicitly set) →
  // render snapshot() → dispatch(action) → re-render. No policy logic in the SPA
  // (priority #2). Mirrors admin/contacts/+page.svelte.
  //
  // The toggle dispatches on change and re-reads the persisted state from the
  // post-dispatch snapshot. Errors surface via the snapshot's `error` →
  // `error-message` (MessageBanner), never faked green.
  // UX/IDs: tests/e2e-unified/ui.yaml `admin-files` page.
  import { identity } from '$lib/store';
  import { webdavPolicyMachine } from '$lib/rpc';
  import { ensureWasm } from '$lib/wasm';
  import { onMount } from 'svelte';
  import MessageBanner from '$lib/components/MessageBanner.svelte';
  import { t } from '$lib/i18n/strings';
  import { IDS } from '$lib/generated/uiIds';

  // Mirrors the shared serde JSON (snake_case across serde_wasm_bindgen; the status
  // enum serializes as its variant-name string).
  interface WebdavPolicySnapshot {
    webdav_enabled: boolean;
    status: string; // "Idle" | "Loading" | "Working"
    error: string | null;
  }

  let machine: Awaited<ReturnType<typeof webdavPolicyMachine>> | null = null;
  let snap = $state<WebdavPolicySnapshot | null>(null);
  let error = $state('');

  // The toggle dispatches on change, so it is rendered one-way (`checked={webdavEnabled}`)
  // — a programmatic re-seed updates the checkbox without firing its change handler.
  let webdavEnabled = $state(false);

  const busy = $derived(snap?.status === 'Working' || snap?.status === 'Loading');

  function applySnapshot(): void {
    if (!machine) return;
    snap = machine.snapshot() as unknown as WebdavPolicySnapshot;
    error = snap?.error ?? '';
    if (!snap) return;
    webdavEnabled = snap.webdav_enabled;
  }

  onMount(async () => {
    const id = $identity;
    if (!id?.secretHex) return;
    try {
      await ensureWasm();
      machine = await webdavPolicyMachine(id.secretHex);
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
      await machine.dispatch({ SetWebdavEnabled: { enabled } });
    } catch {
      /* the snapshot carries the error */
    }
    applySnapshot();
  }
</script>

<h1 data-testid={IDS.ADMIN_FILES_HEADING}>{t.admin.files_page.title}</h1>
<p class="muted small">{t.admin.files_page.description}</p>

<MessageBanner bind:error />

<!-- ── WebDAV-enable master toggle (set_webdav_enabled) ── -->
<section class="section">
  <label class="switch-row">
    <span>
      <span class="label-text">{t.admin.files_page.enabled_label}</span>
      <span class="muted small">{t.admin.files_page.enabled_subtitle}</span>
    </span>
    <input
      type="checkbox"
      data-testid={IDS.ADMIN_FILES_WEBDAV_ENABLED_TOGGLE}
      checked={webdavEnabled}
      disabled={busy && !snap}
      onchange={handleToggleEnabled}
    />
  </label>
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
</style>
