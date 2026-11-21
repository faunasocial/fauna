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
  import { identity } from '#lib/store';
  import { webdavPolicyMachine } from '#lib/rpc';
  import { ensureWasm, readPossibility } from '#lib/wasm';
  import { askPossibility, formGate, formRefusal } from '#lib/form-gate';
  import {
    dispatchPainted,
    machineAnswer,
    reasonAttr,
    type Possibility,
    type ReadWord,
  } from '#lib/admin-read-gate';
  import { readOnEntryAndReconnect } from '#lib/admin-reread';
  import MessageBanner from '#lib/components/MessageBanner.svelte';
  import { t } from '#lib/i18n/strings';
  import { IDS } from '#lib/generated/uiIds';

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


  /** The toggle's answer — `WebdavPolicyMachine::toggle_possibility` over wasm
   *  (`admin.md` § Errors & edge cases, *Controls wait for their read*): still
   *  loading before the `get_mail_config` read, failed to load after a failed
   *  one, still saving while a change is on its way. One answer paints the
   *  toggle and refuses its press. */
  let toggleAnswer = $state<Possibility | null>(null);
  const toggleGate = $derived(formGate(toggleAnswer));
  /** Whether a read ever resolved: until then the toggle's state is a guess
   *  and is not painted as `on`/`off`. */
  let readResolved = $state(false);

  /** The read that builds the machine: failed once its connect or first
   *  read threw, so the toggle says failed to load, never nothing. */
  let machineBuild: ReadWord = 'pending';

  function repaint(): void {
    toggleAnswer = machineAnswer(
      machine,
      (m) => m.togglePossibility() as Possibility,
      machineBuild,
      readPossibility,
    );
  }

  function applySnapshot(): void {
    if (!machine) return;
    repaint();
    snap = machine.snapshot() as unknown as WebdavPolicySnapshot;
    error = snap?.error ?? '';
    if (!snap) return;
    webdavEnabled = snap.webdav_enabled;
  }

  /** The read: build the machine (once) and hydrate it — painted from the
   *  moment the wasm core is up: still loading while the machine is built
   *  and hydrated, failed to load after, never disabled with nothing beside
   *  it. */
  async function readMachine(): Promise<void> {
    const id = $identity;
    if (!id?.secretHex) return;
    try {
      await ensureWasm();
      repaint();
      if (!machine) machine = await webdavPolicyMachine(id.secretHex);
      repaint();
      await machine.hydrate();
      readResolved = true;
      applySnapshot();
    } catch (e) {
      if (!machine) machineBuild = 'failed';
      repaint();
      error = e instanceof Error ? e.message : String(e);
    }
  }

  // Read on every entry (a `goto()` of this route does not remount it) and,
  // while the read is unanswered, again when the nest comes back.
  readOnEntryAndReconnect(readMachine, async () => {
    if (!readResolved) await readMachine();
  });

  async function handleToggleEnabled(e: Event): Promise<void> {
    const input = e.currentTarget as HTMLInputElement;
    const enabled = input.checked;
    // The press asks the painted answer again: before the read, after a
    // failed one or while a change is on its way it is refused with that
    // reason on `error-message`, the checkbox put back, nothing sent.
    const m = machine;
    const refusal = formRefusal(m ? askPossibility(() => m.togglePossibility() as Possibility) : null);
    if (refusal !== null || !m) {
      input.checked = !enabled;
      if (refusal) error = refusal;
      return;
    }
    try {
      await dispatchPainted(() => m.dispatch({ SetWebdavEnabled: { enabled } }), repaint);
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
      checked={readResolved && webdavEnabled}
      data-state={readResolved ? (webdavEnabled ? 'on' : 'off') : undefined}
      disabled={toggleGate.disabled}
      title={toggleGate.reason}
      data-reason={reasonAttr(toggleGate)}
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
