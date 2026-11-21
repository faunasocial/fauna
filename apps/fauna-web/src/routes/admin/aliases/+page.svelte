<script lang="ts">
  // Admin Aliases page (`admin-aliases`) — admin **external forwarders**
  // (admin.md § 4 / mail-aliases.md § Kind 7, ratified 2026-06-01): the admin
  // maps an address on a hosted local domain (`info@<domain>`) to an EXTERNAL
  // destination with no local mailbox. User-tier alias kinds (exact/+suffix/
  // wildcard/disposable) live on the user `mail-aliases` page, NOT here; the
  // per-domain catch-all lives on the `admin-dns` per-domain row.
  //
  // A dumb renderer of the shared `ForwarderMachine`
  // (libs/fauna-client-mail-settings::forwarders, WASM twin WasmForwarderMachine):
  // build over the singleton WS-RPC client → hydrate() → render snapshot() →
  // dispatch(action) → re-render. No forwarder logic in the SPA (priority #2).
  // Lifts the linux lead + windows shape (apps/fauna-linux/src/views/admin.rs
  // forwarders section). Unlike the deferred user mail-spam/export/lists surfaces,
  // the forwarder nest handlers (`fauna.bridges.{create,list,delete}_forwarder`)
  // exist, so this page is genuinely green.
  //
  // The machine auto-refreshes after Create/Delete (forwarders.rs) and clears its
  // error before each action, so the page just re-reads snapshot() after dispatch.
  // UX/IDs: tests/e2e-unified/ui.yaml `admin-aliases` page +
  // `admin-aliases-forwarder-list` component.
  import { identity } from '#lib/store';
  import { forwarderMachine } from '#lib/rpc';
  import { ensureWasm, forwarderCreateDraftPossibility, readPossibility } from '#lib/wasm';
  import { readOnEntryAndReconnect } from '#lib/admin-reread';
  import MessageBanner from '#lib/components/MessageBanner.svelte';
  import { t } from '#lib/i18n/strings';
  import { IDS } from '#lib/generated/uiIds';
  import { goto } from '$app/navigation';
  import type { Possibility } from '#lib/possibility';
  import { afterGate, formGate, formRefusal } from '#lib/form-gate';
  import {
    dispatchPainted,
    listReadReason,
    machineAnswer,
    reasonAttr,
    type ReadWord,
  } from '#lib/admin-read-gate';

  // Shapes mirror the shared serde JSON (snake_case across the serde_wasm_bindgen
  // boundary; the status enum serializes as its variant-name string).
  interface ForwarderView {
    alias_id_hex: string;
    local_domain: string;
    pattern: string;
    address: string;
    forward_target: string;
  }
  interface ForwardersSnapshot {
    forwarders: ForwarderView[];
    local_domains: string[];
    status: string; // "Idle" | "Loading" | "Working"
    error: string | null;
  }

  let machine: Awaited<ReturnType<typeof forwarderMachine>> | null = null;
  let snap = $state<ForwardersSnapshot | null>(null);
  // The app-wide banner carries load errors; `admin-aliases-action-error` carries
  // create/delete validation failures (validate_forward_target / collisions /
  // reserved local-part), per admin.md § 4 + mail-aliases.md:114.
  let error = $state('');
  let actionError = $state('');

  // Always-visible inline add form (no reveal — the e2e types directly into it).
  let domainInput = $state('');
  let patternInput = $state('');
  let targetInput = $state('');

  const busy = $derived(snap?.status === 'Working');

  // The add form's gate (admin.md § 4, *A forwarder needs a hosted domain*):
  // the machine's shared `formPossibility` paints the picker and the two
  // fields, its `createPossibility` over the picked domain the submit — with no
  // hosted domain both are blocked naming DNS, and the step that opens it
  // (`admin-aliases-forwarder-add-domains-step`) is offered. Each reads `snap`
  // so a new snapshot repaints it; unreadable before the machine exists.
  //
  // Both fold AFTER the machine's read gate, `ForwarderMachine::add_possibility`
  // (admin.md § Errors & edge cases, *Controls wait for their read*): still
  // loading before the forwarder and local-domain read, failed to load after
  // a failed one, still saving while a change is on its way.
  // `readTick` moves on every repaint, so a failed read (no new snapshot)
  // still repaints.
  let readTick = $state(0);
  let listRead = $state<ReadWord>('pending');
  function repaint(): void {
    readTick += 1;
    listRead = machine ? (machine.readState() as ReadWord) : machineBuild;
  }
  function addAnswer(): Possibility | null {
    void readTick;
    void snap;
    return machineAnswer(
      machine,
      (m) => m.addPossibility() as Possibility,
      machineBuild,
      readPossibility,
    );
  }
  /** The read that builds the machine: failed once its connect or first
   *  read threw, so the form says failed to load, never nothing. */
  let machineBuild = $state<ReadWord>('pending');
  function formAnswer(): Possibility | null {
    const m = machine;
    return afterGate(addAnswer(), () => m!.formPossibility() as Possibility);
  }
  function createAnswer(): Possibility | null {
    const m = machine;
    const domain = domainInput;
    return afterGate(addAnswer(), () => m!.createPossibility(domain) as Possibility);
  }
  /** The forwarder list's read line: loading or failed until the list was
   *  read, and only then the empty text. */
  const listReason = $derived(listReadReason(readPossibility, listRead));
  /** The submit's answer: the machine's gate first, then the draft's own
   *  validity — the shared `ForwarderMachine::create_draft_possibility` (a
   *  domain picked, a non-blank local part and destination; admin.md § Errors
   *  & edge cases). A reserved local part, a collision or a hosted target stays
   *  the nest's refusal on `admin-aliases-action-error`. */
  const submitAnswer = $derived(
    afterGate(createAnswer(), () =>
      forwarderCreateDraftPossibility(domainInput, patternInput, targetInput),
    ),
  );

  /** The Domains step: offered exactly when the form is blocked for want of a
   *  hosted domain (a pending read gives another reason). */
  function offerDomainsStep(): boolean {
    const answer = formAnswer();
    return answer?.state === 'blocked' && answer.reason.key === 'admin.aliases_page.no_hosted_domain';
  }

  function applySnapshot(): void {
    if (!machine) return;
    repaint();
    snap = machine.snapshot() as unknown as ForwardersSnapshot;
    // The snapshot error is the last action's error → the action-error surface.
    if (snap?.error) actionError = snap.error;
    // Default the picker to the first hosted domain once it loads.
    if (!domainInput && (snap?.local_domains.length ?? 0) > 0) {
      domainInput = snap!.local_domains[0];
    }
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
      if (!machine) machine = await forwarderMachine(id.secretHex);
      repaint();
      await machine.hydrate();
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
    if (!machine || (machine.readState() as ReadWord) !== 'resolved') await readMachine();
  });

  async function handleCreate(): Promise<void> {
    repaint();
    // The painted answer, asked again: never a create on an empty or unhosted
    // `local_domain` or a blank draft — refused on the page's `error-message`
    // with the painted reason (admin.md § Errors & edge cases, tui's
    // `Errors::refuse`), the typed draft kept.
    const refusal = formRefusal(submitAnswer);
    if (refusal !== null) {
      if (refusal) error = refusal;
      return;
    }
    const m = machine;
    if (!m) return;
    const local_domain = domainInput;
    const pattern = patternInput.trim();
    const forward_target = targetInput.trim();
    actionError = '';
    try {
      await dispatchPainted(() => m.dispatch({ Create: { local_domain, pattern, forward_target } }), repaint);
      applySnapshot();
      if (snap?.error) return; // rejected — keep the form populated for a retry
      patternInput = '';
      targetInput = '';
    } catch (e) {
      applySnapshot();
      if (!actionError) actionError = e instanceof Error ? e.message : String(e);
    }
  }

  async function handleDelete(v: ForwarderView): Promise<void> {
    if (!machine) return;
    actionError = '';
    try {
      await machine.dispatch({ Delete: { alias_id_hex: v.alias_id_hex } });
      applySnapshot();
    } catch (e) {
      applySnapshot();
      if (!actionError) actionError = e instanceof Error ? e.message : String(e);
    }
  }
</script>

<h1 data-testid={IDS.ADMIN_ALIASES_HEADING}>{t.admin.aliases_page.title}</h1>

<MessageBanner bind:error />

{#if actionError}
  <p class="error" data-testid={IDS.ADMIN_ALIASES_ACTION_ERROR}>{actionError}</p>
{/if}

<section class="section" data-testid={IDS.ADMIN_ALIASES_FORWARDERS_SECTION}>
  <h2>{t.admin.aliases_page.forwarders_title}</h2>
  <p class="muted small">{t.admin.aliases_page.forwarders_desc}</p>

  <!-- Always-visible add form. -->
  <div class="form">
    <label>
      <span class="label-text">{t.admin.aliases_page.forwarder_domain}</span>
      <select
        class="input"
        data-testid={IDS.ADMIN_ALIASES_FORWARDER_ADD_DOMAIN_SELECT}
        disabled={formGate(formAnswer()).disabled}
        title={formGate(formAnswer()).reason}
        data-reason={reasonAttr(formGate(formAnswer()))}
        bind:value={domainInput}
      >
        {#each snap?.local_domains ?? [] as d (d)}
          <option value={d}>{d}</option>
        {/each}
      </select>
    </label>
    <label>
      <span class="label-text">{t.admin.aliases_page.forwarder_local_part}</span>
      <input
        class="input"
        type="text"
        data-testid={IDS.ADMIN_ALIASES_FORWARDER_ADD_PATTERN_INPUT}
        disabled={formGate(formAnswer()).disabled}
        title={formGate(formAnswer()).reason}
        data-reason={reasonAttr(formGate(formAnswer()))}
        placeholder={t.admin.aliases_page.forwarder_local_part_placeholder}
        bind:value={patternInput}
      />
    </label>
    <label>
      <span class="label-text">{t.admin.aliases_page.forwarder_target}</span>
      <input
        class="input"
        type="email"
        data-testid={IDS.ADMIN_ALIASES_FORWARDER_ADD_TARGET_INPUT}
        disabled={formGate(formAnswer()).disabled}
        title={formGate(formAnswer()).reason}
        data-reason={reasonAttr(formGate(formAnswer()))}
        placeholder={t.admin.aliases_page.forwarder_target_placeholder}
        bind:value={targetInput}
      />
    </label>
    <button
      class="btn primary"
      data-testid={IDS.ADMIN_ALIASES_FORWARDER_ADD_SUBMIT_BUTTON}
      disabled={formGate(submitAnswer).disabled}
      title={formGate(submitAnswer).reason}
      data-reason={reasonAttr(formGate(submitAnswer))}
      onclick={handleCreate}
    >
      {t.admin.aliases_page.create_forwarder}
    </button>
  </div>
  {#if offerDomainsStep()}
    <p class="muted small">{formGate(formAnswer()).reason}</p>
    <button
      class="btn"
      data-testid={IDS.ADMIN_ALIASES_FORWARDER_ADD_DOMAINS_STEP}
      onclick={() => void goto('/app/admin/dns')}
    >
      {t.admin.aliases_page.open_domains}
    </button>
  {/if}

  <!-- Forwarder rows (admin-aliases-forwarder-list component). -->
  <div class="rows" data-testid={IDS.ADMIN_ALIASES_FORWARDER_LIST}>
    {#if listReason !== null}
      <p class="muted small">{listReason}</p>
    {:else if (snap?.forwarders.length ?? 0) === 0}
      <p class="muted small">{t.admin.aliases_page.no_forwarders}</p>
    {/if}
    {#each snap?.forwarders ?? [] as v (v.alias_id_hex)}
      <div class="row">
        <span class="addr mono" data-testid={IDS.ADMIN_ALIASES_FORWARDER_ROW_ADDRESS}>{v.address}</span>
        <span class="muted">→</span>
        <span class="target mono" data-testid={IDS.ADMIN_ALIASES_FORWARDER_ROW_TARGET}>{v.forward_target}</span>
        <button
          class="btn small danger"
          data-testid={IDS.ADMIN_ALIASES_FORWARDER_ROW_DELETE_BUTTON}
          disabled={busy}
          onclick={() => handleDelete(v)}
        >
          {t.admin.aliases_page.delete_forwarder}
        </button>
      </div>
    {/each}
  </div>
</section>

<style>
  h1 { margin-bottom: 1.5rem; font-size: 1.5rem; }
  h2 { font-size: 1.125rem; margin-bottom: 0.25rem; }
  .muted { color: var(--text-muted, #8b949e); }
  .small { font-size: 0.8rem; }
  .error { color: var(--danger, #f85149); font-size: 0.875rem; margin-bottom: 0.75rem; }
  .section { margin-bottom: 2rem; }

  .form {
    display: flex;
    align-items: flex-end;
    gap: 0.75rem;
    flex-wrap: wrap;
    margin: 0.75rem 0 1rem;
    padding: 0.75rem;
    border: 1px solid var(--border, #30363d);
    border-radius: 8px;
    background: var(--bg-surface, #161b22);
  }
  .form label {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    flex: 1;
    min-width: 150px;
  }
  .label-text {
    font-size: 0.75rem;
    color: var(--text-muted, #8b949e);
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }
  .input {
    padding: 0.5rem;
    border: 1px solid var(--border, #30363d);
    border-radius: 6px;
    background: var(--bg, #0d1117);
    color: var(--text, #e6edf3);
    font-size: 0.875rem;
  }

  .rows { display: flex; flex-direction: column; gap: 0.5rem; }
  .row {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.5rem 0.75rem;
    border: 1px solid var(--border, #30363d);
    border-radius: 6px;
    flex-wrap: wrap;
  }
  .mono { font-family: monospace; font-size: 0.85rem; }
  .addr { flex: 1; word-break: break-all; }
  .target { flex: 1; word-break: break-all; }

  .btn {
    padding: 0.5rem 1rem;
    border: 1px solid var(--border, #30363d);
    border-radius: 6px;
    background: var(--bg-surface, #161b22);
    color: var(--text, #e6edf3);
    cursor: pointer;
    font-size: 0.875rem;
  }
  .btn:hover { background: var(--bg-hover, #1c2128); }
  .btn:disabled { opacity: 0.5; cursor: not-allowed; }
  .btn.primary { border-color: var(--accent, #58a6ff); color: var(--accent, #58a6ff); }
  .btn.danger { border-color: var(--danger, #f85149); color: var(--danger, #f85149); }
  .btn.small { font-size: 0.8rem; padding: 0.25rem 0.625rem; }
</style>
