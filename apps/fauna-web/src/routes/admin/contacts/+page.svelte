<script lang="ts">
  // Admin Contacts page (`admin-contacts`) — the deployment-wide CardDAV-enable
  // toggle (admin.md § Contacts / carddav-server.md § Independent enablement).
  // The contacts sibling of `admin-calendar`'s CalDAV-enable switch: email,
  // calendar, and contacts are three independently enableable features of ONE MDA
  // bridge (the MDA runs iff `mail_enabled || caldav_enabled || carddav_enabled`),
  // so each gets its own admin enable toggle. No port field — CardDAV rides the
  // shared DAV listener admin-calendar's port input governs.
  //
  // A dumb renderer of the shared `CarddavPolicyMachine`
  // (libs/fauna-client-mail-settings::carddav_policy, WASM twin
  // WasmCarddavPolicyMachine): build over the singleton WS-RPC client →
  // hydrate() (via the Admin read twin `fauna.bridges.get_mail_config`, whose
  // `carddav_enabled` falls back to `mail_enabled` until explicitly set) →
  // render snapshot() → dispatch(action) → re-render. No policy logic in the SPA
  // (priority #2). Mirrors admin/calendar/+page.svelte.
  //
  // The toggle dispatches on change and re-reads the persisted state from the
  // post-dispatch snapshot. Errors surface via the snapshot's `error` →
  // `error-message` (MessageBanner), never faked green.
  // UX/IDs: tests/e2e-unified/ui.yaml `admin-contacts` page.
  import { identity } from '#lib/store';
  import { carddavPolicyMachine } from '#lib/rpc';
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
  interface CarddavPolicySnapshot {
    carddav_enabled: boolean;
    status: string; // "Idle" | "Loading" | "Working"
    error: string | null;
  }

  let machine: Awaited<ReturnType<typeof carddavPolicyMachine>> | null = null;
  let snap = $state<CarddavPolicySnapshot | null>(null);
  let error = $state('');

  // The toggle dispatches on change, so it is rendered one-way (`checked={carddavEnabled}`)
  // — a programmatic re-seed updates the checkbox without firing its change handler.
  let carddavEnabled = $state(false);


  /** The toggle's answer — `CarddavPolicyMachine::toggle_possibility` over wasm
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
    snap = machine.snapshot() as unknown as CarddavPolicySnapshot;
    error = snap?.error ?? '';
    if (!snap) return;
    carddavEnabled = snap.carddav_enabled;
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
      if (!machine) machine = await carddavPolicyMachine(id.secretHex);
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
      await dispatchPainted(() => m.dispatch({ SetCarddavEnabled: { enabled } }), repaint);
    } catch {
      /* the snapshot carries the error */
    }
    applySnapshot();
  }
</script>

<h1 data-testid={IDS.ADMIN_CONTACTS_HEADING}>{t.admin.contacts_page.title}</h1>
<p class="muted small">{t.admin.contacts_page.description}</p>

<MessageBanner bind:error />

<!-- ── CardDAV-enable master toggle (set_carddav_enabled) ── -->
<section class="section">
  <label class="switch-row">
    <span>
      <span class="label-text">{t.admin.contacts_page.enabled_label}</span>
      <span class="muted small">{t.admin.contacts_page.enabled_subtitle}</span>
    </span>
    <input
      type="checkbox"
      data-testid={IDS.ADMIN_CONTACTS_CARDDAV_ENABLED_TOGGLE}
      checked={readResolved && carddavEnabled}
      data-state={readResolved ? (carddavEnabled ? 'on' : 'off') : undefined}
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
