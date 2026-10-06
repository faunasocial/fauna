<script lang="ts">
  // User-settings "Import mailbox" section: a person pulls their existing mail
  // off a foreign IMAP server (Gmail / Outlook / iCloud / generic) into their
  // Fauna mailbox over a five-screen wizard — source → scope → confirm →
  // progress → done. A dumb renderer of the shared `MailImportMachine`
  // (`libs/fauna-client-mail-settings`, WASM twin `WasmMailImportMachine`):
  // build over the singleton WS-RPC client → hydrate() → render snapshot() →
  // dispatch(action) → re-render. No import logic in the SPA (priority #2).
  // Lifts the tui lead shape (apps/fauna-tui/src/settings/mail_import.rs) and
  // its linux twin (apps/fauna-linux/src/settings/mail_import.rs). Reuses the
  // settings page's single `error-message` via the bindable `error`. Behavior +
  // IDs: docs/goal/behavior/mailbox-migration.md, ui.yaml § mail-import /
  // mail-import-mailbox-progress-list.
  //
  // ── The one thing web does differently, and it is NOT a shortcut ───────────
  //
  // This page ships a REDUCED slice, and the reduction is in the wizard's first
  // step. `MailImportNest` is real on web (`MailImportClient<WsRpcClient>`,
  // wasm-clean), so hydrate, the session read, and Pause/Resume/Cancel on an
  // already-open session all work against the nest for real. `ImportSourceNest`
  // is the stub: the web IMAP transport (sans-io `rustls` over the relay) is
  // unbuilt and the relay host is undeployed, so `Connect` — which dials the
  // FOREIGN server — returns the honest `Rejected` rejection
  // (mailbox-migration.md § Implementation status today names this as the last
  // unbuilt leg of the whole feature).
  //
  // So the Source step is painted, actuable, and tells the truth when clicked:
  // the ui.yaml element scope is the contract and a control that silently
  // vanished would be a worse lie than one that explains itself. `notice`
  // carries that explanation ABOVE the step rather than only on the rejection,
  // so nobody types a real mailbox password into a form that cannot use it.
  // When the transport lands, delete `SOURCE_UNAVAILABLE` and this comment —
  // nothing else about this page has to change.
  //
  // The fetch-drive loop (`run_import`) is not spawned here for the same
  // reason: it reads the source. tui and linux spawn it after a successful
  // Start; web cannot, and pretending otherwise would fake progress.
  import { identity } from '$lib/store';
  import { mailImportMachine } from '$lib/rpc';
  import { ensureWasm, importSourceKindLabel, importTlsModeLabel, mailImportConnectActions } from '$lib/wasm';
  import { resolveLocalized } from '$lib/i18n/localized';
  import { onDestroy, onMount } from 'svelte';
  import { t } from '$lib/i18n/strings';
  import type { WasmMailImportMachine } from '../../../static/fauna_wasm.js';
  import { IDS } from '$lib/generated/uiIds';

  // The parent settings page owns the single error surface; import dispatch
  // errors flow into it (the same surface the other mail-settings sections use).
  let { error = $bindable('') } = $props();

  // Shapes mirror the shared `MailImportSnapshot` serde JSON (snake_case across
  // the serde_wasm_bindgen boundary; the step/kind enums serialize as their
  // PascalCase variant-name string).
  interface SourceMailboxOption { name: string; selected: boolean; message_count: number; }
  interface ImportSnapshot {
    step: string; // "Source" | "Scope" | "Confirm" | "Progress" | "Done"
    source_kind: string; // "Gmail" | "Outlook" | "ICloud" | "Generic"
    host: string;
    port: number;
    tls_mode: string; // "Implicit" | "StartTls"
    username: string;
    mailboxes: SourceMailboxOption[];
    date_from: string;
    max_size_bytes: number;
    session_state: string | null; // "Running" | "Paused" | "Errored" | "Completed" | "Cancelled"
    imported_count: number;
    skipped_count: number;
    errored_count: number;
    total_count: number;
    error_log: string[];
    status: string; // "Idle" | "Loading" | "Working"
    error: string | null;
  }

  // The machine's own default (§ Wizard steps step 3), in MB — the field's unit;
  // the action takes bytes.
  const DEFAULT_MAX_SIZE_MB = '50';
  const MB = 1024 * 1024;

  let machine = $state<WasmMailImportMachine | null>(null);
  // Cancel's two-click arm: page state the snapshot re-read never touches, so a
  // repaint cannot disarm a Cancel just armed (MailExportSection's shape).
  const CANCEL_ARM_MS = 4000;
  let cancelArmed = $state(false);
  let cancelArmTimer: ReturnType<typeof setTimeout> | null = null;
  onDestroy(() => {
    if (cancelArmTimer) clearTimeout(cancelArmTimer);
  });

  // Cancel aborts the import, so it asks first: the first click arms, the
  // second confirms (mailbox-migration.md § Wizard steps).
  function onCancel(): void {
    if (cancelArmTimer) clearTimeout(cancelArmTimer);
    cancelArmTimer = null;
    if (cancelArmed) {
      cancelArmed = false;
      dispatch('Cancel');
      return;
    }
    cancelArmed = true;
    cancelArmTimer = setTimeout(() => { cancelArmed = false; }, CANCEL_ARM_MS);
  }
  let snap = $state<ImportSnapshot | null>(null);

  // Page-local draft buffers, exactly as tui and linux hold them: every Source
  // and Scope text field is a client-only value the machine would otherwise
  // take one dispatch per keystroke for, and the whole form commits as ONE
  // ordered multi-action dispatch at Connect / Next. (`password` is deliberately
  // never read back out of the snapshot — § Credential handling keeps it in
  // client memory only, and the machine clears it the moment Connect succeeds.)
  let hostInput = $state('');
  let portInput = $state('');
  let usernameInput = $state('');
  let passwordInput = $state('');
  let dateFromInput = $state('');
  let maxSizeInput = $state(DEFAULT_MAX_SIZE_MB);

  const busy = $derived(snap?.status === 'Working');
  // Which Source fields this provider asks for — the per-provider table in
  // mailbox-migration.md § Wizard steps step 1, as transcribed by the tui lead
  // app. Gmail/iCloud take an app password; Outlook takes OAuth *or* the IMAP
  // fallback; Generic takes the IMAP fields alone.
  const appPasswordKind = $derived(snap?.source_kind === 'Gmail' || snap?.source_kind === 'ICloud');
  const imapFieldsKind = $derived(snap?.source_kind === 'Outlook' || snap?.source_kind === 'Generic');

  function applySnapshot(): void {
    if (!machine) return;
    snap = machine.snapshot() as unknown as ImportSnapshot;
    if (snap?.error) error = snap.error;
  }

  onMount(async () => {
    const id = $identity;
    if (!id?.secretHex) return;
    try {
      await ensureWasm();
      machine = await mailImportMachine(id.secretHex);
      await machine.hydrate();
      applySnapshot();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  });

  // Dispatch `actions` IN ORDER, awaiting each before the single re-read, so a
  // Scope→Confirm advance can never observe a half-committed scope (the tui
  // `MailImportDispatch` rule).
  async function dispatch(...actions: unknown[]): Promise<void> {
    if (!machine) return;
    try {
      for (const action of actions) await machine.dispatch(action);
      applySnapshot();
    } catch (e) {
      applySnapshot();
      if (!error) error = e instanceof Error ? e.message : String(e);
    }
  }

  // Step 1: re-seed the Source-step host/port drafts from the snapshot the
  // kind-change dispatch just wrote — tui/linux/apple's `syncDraftsFromSnapshot`
  // shape. Nothing did this here before, so picking Outlook left the host
  // field empty and `connect()` sent it verbatim. Unconditional,
  // matching the other apps: a Generic pick after Gmail paints
  // `imap.gmail.com` (no `Generic` preset to overwrite it with) rather than
  // blanking the field.
  async function selectSourceKind(kind: string): Promise<void> {
    await dispatch({ SelectSourceKind: { kind } });
    if (snap) {
      hostInput = snap.host;
      portInput = String(snap.port);
    }
  }

  // Step 1→2: commit whichever Source fields this provider actually shows, then
  // Connect — one op, now built by `fauna_client_mail_settings::connect_actions`
  // (tui's and linux's shared builder) instead of a hand-rolled mirror: the
  // kind-gate on host/port and the port-parse both move into the one shared fn.
  function connect(): void {
    if (!snap) return;
    void dispatch(
      ...mailImportConnectActions(snap.source_kind, hostInput, portInput, usernameInput, passwordInput),
    );
  }

  // Step 2→3: commit both Scope drafts, then advance. An unparseable max-size
  // buffer falls back to the machine's own default rather than sending a
  // stale/zero value (tui's `scope_next_actions`).
  function scopeNext(): void {
    const mb = Number.parseInt(maxSizeInput.trim(), 10);
    const bytes = Number.isInteger(mb) && mb > 0 ? mb * MB : 50 * MB;
    void dispatch(
      { SetDateFrom: { value: dateFromInput.trim() } },
      { SetMaxSizeBytes: { value: bytes } },
      'Next',
    );
  }

  // The two picker vocabularies come from the shared Rust maps over wasm — the
  // same source every other app consumes, single-sourced (priority #2). `k`/`m`
  // are the serde variant-name strings the snapshot carries.
  function sourceLabel(k: string): string {
    return resolveLocalized(importSourceKindLabel(k));
  }
  function tlsLabel(m: string): string {
    return resolveLocalized(importTlsModeLabel(m));
  }

  function confirmSummary(s: ImportSnapshot): string {
    const selected = s.mailboxes.filter((m) => m.selected);
    return t.mail_import.confirm_summary_fmt({
      source: sourceLabel(s.source_kind),
      mailboxes: String(selected.length),
      messages: String(selected.reduce((n, m) => n + m.message_count, 0)),
    });
  }
</script>

<section class="section">
  <h2>{t.mail_import.title}</h2>
  <p class="muted small">{t.mail_import.description}</p>

  {#if snap}
    <!-- Step 1 — Source (provider + credentials). -->
    {#if snap.step === 'Source'}
      <div class="step">
        <h3 class="muted small">{t.mail_import.source_title}</h3>
        <!-- Said BEFORE the fields, not after the click: nobody should type a
             real mailbox password into a form that cannot use it yet. -->
        <p class="notice small">{t.mail_import.source_unavailable}</p>
        <select
          class="input"
          data-testid={IDS.MAIL_IMPORT_SOURCE_PICKER}
          value={snap.source_kind}
          onchange={(e) => selectSourceKind((e.currentTarget as HTMLSelectElement).value)}
          disabled={busy}
        >
          <option value="Gmail">{t.mail_import.source_gmail}</option>
          <option value="Outlook">{t.mail_import.source_outlook}</option>
          <option value="ICloud">{t.mail_import.source_icloud}</option>
          <option value="Generic">{t.mail_import.source_generic}</option>
        </select>

        <!-- Painted for every provider kind — nothing in ui.yaml scopes
             `source-username` to a subset the way the other four are annotated. -->
        <input
          class="input"
          data-testid={IDS.MAIL_IMPORT_SOURCE_USERNAME}
          placeholder={t.mail_import.source_username_placeholder}
          bind:value={usernameInput}
          disabled={busy}
        />

        {#if appPasswordKind}
          <input
            class="input"
            type="password"
            data-testid={IDS.MAIL_IMPORT_SOURCE_APP_PASSWORD}
            placeholder={t.mail_import.source_app_password_label}
            bind:value={passwordInput}
            disabled={busy}
          />
          <p class="muted small">
            {snap.source_kind === 'Gmail'
              ? t.mail_import.source_app_password_help_gmail
              : t.mail_import.source_app_password_help_icloud}
          </p>
        {/if}

        {#if snap.source_kind === 'Outlook'}
          <!-- Painted because ui.yaml requires the element exist, but never
               actuable: no app wires the Microsoft Graph dance yet
               (mailbox-migration.md's own "Not in scope" list). The IMAP
               fallback below is the real path for an Outlook account. -->
          <button class="btn" data-testid={IDS.MAIL_IMPORT_SOURCE_OAUTH_BUTTON} disabled>
            {t.mail_import.source_oauth_button}
          </button>
        {/if}

        {#if imapFieldsKind}
          <input
            class="input"
            data-testid={IDS.MAIL_IMPORT_SOURCE_HOST}
            placeholder={t.mail_import.source_host_placeholder}
            bind:value={hostInput}
            disabled={busy}
          />
          <input
            class="input"
            data-testid={IDS.MAIL_IMPORT_SOURCE_PORT}
            placeholder={t.mail_import.source_port_placeholder}
            bind:value={portInput}
            disabled={busy}
          />
          <select
            class="input"
            data-testid={IDS.MAIL_IMPORT_SOURCE_TLS_MODE}
            value={snap.tls_mode}
            onchange={(e) => dispatch({ SetTlsMode: { mode: (e.currentTarget as HTMLSelectElement).value } })}
            disabled={busy}
          >
            <option value="Implicit">{tlsLabel('Implicit')}</option>
            <option value="StartTls">{tlsLabel('StartTls')}</option>
          </select>
          <input
            class="input"
            type="password"
            data-testid={IDS.MAIL_IMPORT_SOURCE_PASSWORD}
            placeholder={t.mail_import.source_password_placeholder}
            bind:value={passwordInput}
            disabled={busy}
          />
        {/if}

        <div class="nav">
          <button class="btn primary" data-testid={IDS.MAIL_IMPORT_CONNECT_BUTTON} onclick={connect} disabled={busy}>
            {t.mail_import.connect_button}
          </button>
        </div>
      </div>
    {/if}

    <!-- Step 2 — Scope (which mailboxes, since when, how big). -->
    {#if snap.step === 'Scope'}
      <div class="step">
        <h3 class="muted small">{t.mail_import.scope_title}</h3>
        <span class="muted small">{t.mail_import.scope_mailboxes_label}</span>
        <div class="mailboxes" data-testid={IDS.MAIL_IMPORT_SCOPE_MAILBOXES}>
          {#if snap.mailboxes.length === 0}
            <p class="muted small">{t.mail_import.scope_mailboxes_empty}</p>
          {/if}
          {#each snap.mailboxes as mb (mb.name)}
            <label class="mailbox">
              <input
                type="checkbox"
                data-testid={IDS.MAIL_IMPORT_SCOPE_MAILBOX_ITEM}
                data-state={mb.selected ? 'on' : 'off'}
                checked={mb.selected}
                disabled={busy}
                onchange={() => dispatch({ ToggleMailbox: { mailbox: mb.name } })}
              />
              <span>{mb.name}</span>
            </label>
          {/each}
        </div>
        <input
          class="input"
          data-testid={IDS.MAIL_IMPORT_SCOPE_DATE_FROM}
          placeholder={t.mail_import.scope_date_from_placeholder}
          bind:value={dateFromInput}
          disabled={busy}
        />
        <input
          class="input"
          data-testid={IDS.MAIL_IMPORT_SCOPE_MAX_SIZE}
          placeholder={t.mail_import.scope_max_size_label}
          bind:value={maxSizeInput}
          disabled={busy}
        />
        <!-- Informational only: no MailImportAction changes the mapping, and the
             goal doc names no control for it either (§ Wizard steps step 3). -->
        <p class="muted small" data-testid={IDS.MAIL_IMPORT_SCOPE_MAILBOX_MAPPING}>
          {t.mail_import.scope_mailbox_mapping_label}
        </p>
        <div class="nav">
          <button class="btn" data-testid={IDS.WIZARD_BACK_BUTTON} onclick={() => dispatch('Back')} disabled={busy}>
            {t.mail_import.back}
          </button>
          <button class="btn primary" data-testid={IDS.WIZARD_NEXT_BUTTON} onclick={scopeNext} disabled={busy}>
            {t.mail_import.next}
          </button>
        </div>
      </div>
    {/if}

    <!-- Step 3 — Confirm + the durable commit. -->
    {#if snap.step === 'Confirm'}
      <div class="step">
        <h3 class="muted small">{t.mail_import.confirm_title}</h3>
        <p class="summary" data-testid={IDS.MAIL_IMPORT_CONFIRM_SUMMARY}>{confirmSummary(snap)}</p>
        <div class="nav">
          <button class="btn" data-testid={IDS.WIZARD_BACK_BUTTON} onclick={() => dispatch('Back')} disabled={busy}>
            {t.mail_import.back}
          </button>
          <button class="btn primary" data-testid={IDS.MAIL_IMPORT_START_BUTTON} onclick={() => dispatch('Start')} disabled={busy}>
            {t.mail_import.start_button}
          </button>
        </div>
      </div>
    {/if}

    <!-- Step 4 — Progress (running / paused / errored / cancelled). -->
    {#if snap.step === 'Progress'}
      <div class="step">
        <h3 class="muted small">{t.mail_import.progress_title}</h3>
        <p class="summary" data-testid={IDS.MAIL_IMPORT_PROGRESS_SUMMARY}>
          {t.mail_import.progress_summary_fmt({
            imported: String(snap.imported_count),
            total: String(snap.total_count),
            skipped: String(snap.skipped_count),
            errored: String(snap.errored_count),
          })}
        </p>
        <progress data-testid={IDS.MAIL_IMPORT_PROGRESS_BAR} value={snap.imported_count} max={snap.total_count || 1}></progress>
        <!-- The machine tracks only GLOBAL counts, not a per-mailbox breakdown
             (MailImportSnapshot has no mailbox_progress field the way export's
             does), so each row shows its PLANNED message count rather than a
             live per-mailbox fraction — the tui lead app's own
             accurate-to-what-exists simplification, inherited verbatim. -->
        <div class="progress-list" data-testid={IDS.MAIL_IMPORT_MAILBOX_PROGRESS_LIST}>
          {#each snap.mailboxes.filter((m) => m.selected) as mb (mb.name)}
            <div class="progress-item" data-testid={IDS.MAIL_IMPORT_MAILBOX_PROGRESS_LIST_ITEM}>
              <span data-testid={IDS.MAIL_IMPORT_MAILBOX_PROGRESS_LIST_ITEM_NAME}>{mb.name}</span>
              <span class="muted small" data-testid={IDS.MAIL_IMPORT_MAILBOX_PROGRESS_LIST_ITEM_PROGRESS}>
                {t.mail_import.progress_row_fmt({ count: String(mb.message_count) })}
              </span>
            </div>
          {/each}
        </div>
        <span class="muted small">{t.mail_import.error_log_title}</span>
        <div class="error-log" data-testid={IDS.MAIL_IMPORT_ERROR_LOG}>
          {#each snap.error_log as line}<p class="muted small">{line}</p>{/each}
        </div>
        <div class="nav">
          <button class="btn" data-testid={IDS.MAIL_IMPORT_PAUSE_BUTTON} onclick={() => dispatch('Pause')} disabled={busy}>
            {t.mail_import.pause_button}
          </button>
          <button class="btn" data-testid={IDS.MAIL_IMPORT_RESUME_BUTTON} onclick={() => dispatch('Resume')} disabled={busy}>
            {t.mail_import.resume_button}
          </button>
          <button class="btn danger" class:armed={cancelArmed} data-testid={IDS.MAIL_IMPORT_CANCEL_BUTTON} onclick={onCancel} disabled={busy}>
            {cancelArmed ? t.common.confirm_q : t.mail_import.cancel_button}
          </button>
        </div>
      </div>
    {/if}

    <!-- Step 5 — Done. -->
    {#if snap.step === 'Done'}
      <div class="step">
        <h3 class="muted small">{t.mail_import.done_title}</h3>
        <p class="summary" data-testid={IDS.MAIL_IMPORT_DONE_SUMMARY}>
          {t.mail_import.done_summary_fmt({
            imported: String(snap.imported_count),
            skipped: String(snap.skipped_count),
            errored: String(snap.errored_count),
          })}
        </p>
        <!-- Neither button is backed by a MailImportAction — the shared machine
             never wired a "view inbox" / "skip log" RPC. Both go to the mail
             surface, tui's own resolution; "Review skipped" cannot deep-link to
             a skip-log page that exists nowhere yet (the log is inline on
             Progress instead). -->
        <div class="nav">
          <a class="btn primary" data-testid={IDS.MAIL_IMPORT_VIEW_IMPORTED_BUTTON} href="/conversations">
            {t.mail_import.view_imported_button}
          </a>
          <a class="btn" data-testid={IDS.MAIL_IMPORT_REVIEW_SKIPPED_BUTTON} href="/conversations">
            {t.mail_import.review_skipped_button}
          </a>
        </div>
      </div>
    {/if}
  {/if}
</section>

<style>
  .section { margin-bottom: 2rem; }
  .section h2 { margin-bottom: 0.5rem; font-size: 1.125rem; }
  .section h3 { margin: 0.5rem 0 0.25rem; font-size: 0.9rem; }
  .muted { color: var(--text-muted); }
  .small { font-size: 0.8rem; }
  .step { display: flex; flex-direction: column; gap: 0.5rem; max-width: 32rem; margin-top: 0.5rem; }
  .nav { display: flex; gap: 0.5rem; margin-top: 0.5rem; }
  .btn.danger.armed { background: var(--danger); color: #fff; }
  .mailboxes { display: flex; flex-direction: column; gap: 0.25rem; }
  .mailbox { display: flex; align-items: flex-start; gap: 0.5rem; }
  .summary { font-size: 0.9rem; }
  .notice {
    padding: 0.5rem 0.75rem; border: 1px solid var(--border); border-radius: 6px;
    background: var(--bg-surface); color: var(--text-muted);
  }
  .progress-list { display: flex; flex-direction: column; gap: 0.25rem; margin-top: 0.25rem; }
  .progress-item { display: flex; justify-content: space-between; gap: 0.5rem; }
  .error-log { display: flex; flex-direction: column; gap: 0.125rem; }
  .input {
    padding: 0.5rem; border: 1px solid var(--border); border-radius: 6px;
    background: var(--bg); color: var(--text); font-size: 0.875rem;
  }
  .btn {
    padding: 0.5rem 1rem; border: 1px solid var(--border); border-radius: 6px;
    background: var(--bg-surface); color: var(--text); cursor: pointer;
    font-size: 0.875rem; text-decoration: none; display: inline-block;
  }
  .btn:hover { background: var(--bg-hover); }
  .btn:disabled { opacity: 0.5; cursor: not-allowed; pointer-events: none; }
  .btn.primary { background: var(--accent); color: #fff; border-color: var(--accent); }
  .btn.danger { border-color: var(--danger); color: var(--danger); }
  progress { width: 100%; }
</style>
