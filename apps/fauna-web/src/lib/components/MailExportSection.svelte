<script lang="ts">
  // User-settings "Export mailbox" section: a person exports **their own** whole
  // mailbox in a standard format (mbox / Maildir++ / EML-zip) over a resumable
  // wizard — format → scope → confirm → progress → done. A dumb renderer of the
  // shared `MailExportMachine` (`libs/fauna-client-mail-settings`, WASM twin
  // `WasmMailExportMachine`): build over the singleton WS-RPC client → hydrate()
  // → render snapshot() → dispatch(action) → re-render. No export logic in the
  // SPA (priority #2). Lifts the linux lead shape
  // (apps/fauna-linux/src/settings/mail_export.rs). Reuses the settings page's
  // single `error-message` via the bindable `error`. Behavior + IDs:
  // docs/goal/behavior/mail-export.md, ui.yaml § mail-export /
  // mail-export-mailbox-progress-list.
  //
  // The machine has key custody, so this page owes it three things, and they
  // land together (custody without the spawn would open a session nothing
  // drives):
  //  1. It spawns `runExport()` after a `Start` or `Resume` that lands a
  //     `Running` session — fire-and-forget; the loop mutates the machine's own
  //     snapshot and a failure lands on its `error`.
  //  2. A progress tick repaints the snapshot while the loop runs. It only
  //     re-reads the snapshot: Cancel's two-click arm is its own state the tick
  //     never touches, so a repaint cannot disarm a Cancel just armed.
  //  3. Download dispatches `MailExportAction::Download` — never a link to the
  //     nest's URL, which would hand the user the still-sealed blob. The save
  //     itself is `$lib/mail-export-save` (§ Download flow step 5), and the
  //     Done summary then names the saved file, so the press is visible.
  import { identity } from '$lib/store';
  import { mailExportMachine } from '$lib/rpc';
  import { sweepStaleExportArchives } from '$lib/mail-export-save';
  import { ensureWasm, exportFormatLabel } from '$lib/wasm';
  import { resolveLocalized } from '$lib/i18n/localized';
  import { onDestroy, onMount } from 'svelte';
  import { t } from '$lib/i18n/strings';
  import type { WasmMailExportMachine } from '../../../static/fauna_wasm.js';
  import { IDS } from '$lib/generated/uiIds';

  // The parent settings page owns the single error surface; export dispatch
  // errors flow into it (the same surface the other mail-settings sections use).
  let { error = $bindable('') } = $props();

  // Shapes mirror the shared `MailExportSnapshot` serde JSON (snake_case across
  // the serde_wasm_bindgen boundary; the step/format enums serialize as their
  // PascalCase variant-name string).
  interface MailboxOption { name: string; selected: boolean; }
  interface MailboxProgressView { name: string; exported: number; total: number; }
  interface ExportSnapshot {
    step: string; // "Format" | "Scope" | "Confirm" | "Progress" | "Done"
    format: string; // "Mbox" | "MaildirPlus" | "EmlZip"
    mailboxes: MailboxOption[];
    date_from: string;
    date_to: string;
    strip_headers: boolean;
    session_state: string | null; // "Running" | "Paused" | "Errored" | "Completed" | "Cancelled"
    exported_count: number;
    skipped_count: number;
    errored_count: number;
    total_count: number;
    mailbox_progress: MailboxProgressView[];
    error_log: string[];
    blob_bytes: number | null;
    download_url: string;
    saved_archive_path: string; // the saved file's name once Download finished
    status: string; // "Idle" | "Loading" | "Working"
    error: string | null;
  }

  // The Progress repaint cadence — the same 400 ms the linux lead ticks at.
  const PROGRESS_TICK_MS = 400;
  // How long an armed Cancel waits for its confirming click.
  const CANCEL_ARM_MS = 4000;

  let machine: WasmMailExportMachine | null = null;
  let snap = $state<ExportSnapshot | null>(null);
  let cancelArmed = $state(false);
  let tick: ReturnType<typeof setInterval> | null = null;
  let cancelArmTimer: ReturnType<typeof setTimeout> | null = null;

  const busy = $derived(snap?.status === 'Working');

  function applySnapshot(): void {
    if (!machine) return;
    snap = machine.snapshot() as unknown as ExportSnapshot;
    if (snap?.error) error = snap.error;
  }

  onMount(async () => {
    const id = $identity;
    if (!id?.secretHex) return;
    try {
      await ensureWasm();
      machine = await mailExportMachine(id.secretHex, id.handle ?? '');
      await machine.hydrate();
      applySnapshot();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
    // Temporary archives an earlier download left behind (best-effort).
    void sweepStaleExportArchives();
  });

  onDestroy(() => {
    stopProgressTick();
    if (cancelArmTimer) clearTimeout(cancelArmTimer);
  });

  async function dispatch(action: unknown): Promise<void> {
    if (!machine) return;
    const shouldRun = action === 'Start' || action === 'Resume';
    if (shouldRun || action === 'Download') {
      // The handle names the archive's root directory and the saved file, and
      // can arrive or change after the machine was built — so it is read here,
      // at the gesture.
      const handle = $identity?.handle;
      if (handle) machine.setActorHandle(handle);
    }
    // The machine clears its own error when a new action starts; the shared
    // surface follows, so a refusal never outlives the retry that succeeded.
    error = '';
    try {
      await machine.dispatch(action);
      applySnapshot();
    } catch (e) {
      applySnapshot();
      if (!error) error = e instanceof Error ? e.message : String(e);
      return;
    }
    if (shouldRun && snap?.session_state === 'Running') {
      // Fire-and-forget: a failure is already on the snapshot's `error`, which
      // the tick surfaces.
      machine.runExport().catch(() => {});
      startProgressTick();
    }
  }

  // Repaint the Progress step from the machine while `runExport` runs — a
  // synchronous snapshot read, no RPC. Stops once the wizard leaves Progress
  // (Done, or a Cancel that unwinds it).
  function startProgressTick(): void {
    if (tick) return;
    tick = setInterval(() => {
      applySnapshot();
      if (snap?.step !== 'Progress') stopProgressTick();
    }, PROGRESS_TICK_MS);
  }

  function stopProgressTick(): void {
    if (tick) clearInterval(tick);
    tick = null;
  }

  // Cancel is destructive (the partial blob is unlinked), so it asks first: the
  // first click arms, the second confirms (mail-export.md § Wizard steps).
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

  // The format→label comes from the shared Rust map
  // (`fauna_client_mail_settings::export_format_label`, WASM twin `exportFormatLabel`)
  // — the same source the native apps consume, single-sourced (priority #2).
  // `f` is the serde `ExportFormat` variant-name string the snapshot carries.
  function formatLabel(f: string): string {
    return resolveLocalized(exportFormatLabel(f));
  }

  // Format picker (step 1): a select bound to the snapshot's current format.
  function onSelectFormat(e: Event): void {
    const format = (e.currentTarget as HTMLSelectElement).value;
    dispatch({ SelectFormat: { format } });
  }
</script>

<section class="section">
  <h2>{t.mail_export.title}</h2>
  <p class="muted small">{t.mail_export.description}</p>

  {#if snap}
    <!-- Step 1 — Format. -->
    {#if snap.step === 'Format'}
      <div class="step">
        <h3 class="muted small">{t.mail_export.format_title}</h3>
        <select class="input" data-testid={IDS.MAIL_EXPORT_FORMAT_PICKER} value={snap.format} onchange={onSelectFormat} disabled={busy}>
          <option value="Mbox">{t.mail_export.format_mbox}</option>
          <option value="MaildirPlus">{t.mail_export.format_maildir}</option>
          <option value="EmlZip">{t.mail_export.format_eml}</option>
        </select>
        <div class="nav">
          <button class="btn primary" data-testid={IDS.WIZARD_NEXT_BUTTON} onclick={() => dispatch('Next')} disabled={busy}>{t.mail_export.next}</button>
        </div>
      </div>
    {/if}

    <!-- Step 2 — Scope (which mailboxes + date range + options). -->
    {#if snap.step === 'Scope'}
      <div class="step">
        <h3 class="muted small">{t.mail_export.scope_title}</h3>
        <span class="muted small">{t.mail_export.scope_mailboxes_label}</span>
        <div class="mailboxes" data-testid={IDS.MAIL_EXPORT_SCOPE_MAILBOXES}>
          {#if snap.mailboxes.length === 0}
            <p class="muted small">{t.mail_export.scope_mailboxes_empty}</p>
          {/if}
          {#each snap.mailboxes as mb (mb.name)}
            <label
              class="mailbox"
              data-testid={IDS.MAIL_EXPORT_SCOPE_MAILBOX_ITEM}
              data-state={mb.selected ? 'on' : 'off'}
            >
              <input
                type="checkbox"
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
          data-testid={IDS.MAIL_EXPORT_SCOPE_DATE_FROM}
          placeholder={t.mail_export.scope_date_from_placeholder}
          value={snap.date_from}
          oninput={(e) => dispatch({ SetDateFrom: { value: (e.currentTarget as HTMLInputElement).value } })}
        />
        <input
          class="input"
          data-testid={IDS.MAIL_EXPORT_SCOPE_DATE_TO}
          placeholder={t.mail_export.scope_date_to_placeholder}
          value={snap.date_to}
          oninput={(e) => dispatch({ SetDateTo: { value: (e.currentTarget as HTMLInputElement).value } })}
        />
        <label class="opt">
          <input
            type="checkbox"
            data-testid={IDS.MAIL_EXPORT_SCOPE_STRIP_HEADERS_TOGGLE}
            checked={snap.strip_headers}
            disabled={busy}
            onchange={(e) => dispatch({ SetStripHeaders: { on: (e.currentTarget as HTMLInputElement).checked } })}
          />
          <span class="opt-text">
            <span>{t.mail_export.scope_strip_headers_label}</span>
            <span class="muted small">{t.mail_export.scope_strip_headers_subtitle}</span>
          </span>
        </label>
        <div class="nav">
          <button class="btn" data-testid={IDS.WIZARD_BACK_BUTTON} onclick={() => dispatch('Back')} disabled={busy}>{t.mail_export.back}</button>
          <button class="btn primary" data-testid={IDS.WIZARD_NEXT_BUTTON} onclick={() => dispatch('Next')} disabled={busy}>{t.mail_export.next}</button>
        </div>
      </div>
    {/if}

    <!-- Step 3 — Confirm + Start. -->
    {#if snap.step === 'Confirm'}
      <div class="step">
        <h3 class="muted small">{t.mail_export.confirm_title}</h3>
        <p class="summary" data-testid={IDS.MAIL_EXPORT_CONFIRM_SUMMARY}>
          {t.mail_export.confirm_summary_fmt({
            format: formatLabel(snap.format),
            mailboxes: String(snap.mailboxes.filter((m) => m.selected).length),
          })}
        </p>
        <div class="nav">
          <button class="btn" data-testid={IDS.WIZARD_BACK_BUTTON} onclick={() => dispatch('Back')} disabled={busy}>{t.mail_export.back}</button>
          <button class="btn primary" data-testid={IDS.MAIL_EXPORT_START_BUTTON} onclick={() => dispatch('Start')} disabled={busy}>
            {t.mail_export.start_button}
          </button>
        </div>
      </div>
    {/if}

    <!-- Step 4 — Progress (running / paused / errored). -->
    {#if snap.step === 'Progress'}
      <div class="step">
        <h3 class="muted small">{t.mail_export.progress_title}</h3>
        <p class="summary" data-testid={IDS.MAIL_EXPORT_PROGRESS_SUMMARY}>
          {t.mail_export.progress_summary_fmt({
            exported: String(snap.exported_count),
            total: String(snap.total_count),
            skipped: String(snap.skipped_count),
            errored: String(snap.errored_count),
          })}
        </p>
        <progress data-testid={IDS.MAIL_EXPORT_PROGRESS_BAR} value={snap.exported_count} max={snap.total_count || 1}></progress>
        <div class="progress-list" data-testid={IDS.MAIL_EXPORT_MAILBOX_PROGRESS_LIST}>
          {#each snap.mailbox_progress as mp (mp.name)}
            <div class="progress-item" data-testid={IDS.MAIL_EXPORT_MAILBOX_PROGRESS_LIST_ITEM}>
              <span data-testid={IDS.MAIL_EXPORT_MAILBOX_PROGRESS_LIST_ITEM_NAME}>{mp.name}</span>
              <span class="muted small" data-testid={IDS.MAIL_EXPORT_MAILBOX_PROGRESS_LIST_ITEM_PROGRESS}>{mp.exported} / {mp.total}</span>
            </div>
          {/each}
        </div>
        {#if snap.error_log.length > 0}
          <div class="error-log" data-testid={IDS.MAIL_EXPORT_ERROR_LOG}>
            {#each snap.error_log as line}<p class="muted small">{line}</p>{/each}
          </div>
        {/if}
        <div class="nav">
          <button class="btn" data-testid={IDS.MAIL_EXPORT_PAUSE_BUTTON} onclick={() => dispatch('Pause')} disabled={busy}>{t.mail_export.pause_button}</button>
          <button class="btn" data-testid={IDS.MAIL_EXPORT_RESUME_BUTTON} onclick={() => dispatch('Resume')} disabled={busy}>{t.mail_export.resume_button}</button>
          <button class="btn danger" class:armed={cancelArmed} data-testid={IDS.MAIL_EXPORT_CANCEL_BUTTON} onclick={onCancel} disabled={busy}>
            {cancelArmed ? t.common.confirm_q : t.mail_export.cancel_button}
          </button>
        </div>
      </div>
    {/if}

    <!-- Step 5 — Done (download + discard). -->
    {#if snap.step === 'Done'}
      <div class="step">
        <h3 class="muted small">{t.mail_export.done_title}</h3>
        <p class="summary" data-testid={IDS.MAIL_EXPORT_DONE_SUMMARY}>
          {#if snap.blob_bytes != null && snap.saved_archive_path}
            {t.mail_export.saved_summary_fmt({
              format: formatLabel(snap.format),
              bytes: String(snap.blob_bytes),
              path: snap.saved_archive_path,
            })}
          {:else if snap.blob_bytes != null}
            {t.mail_export.done_summary_fmt({ format: formatLabel(snap.format), bytes: String(snap.blob_bytes) })}
          {:else}
            {formatLabel(snap.format)}
          {/if}
        </p>
        <button
          class="btn primary"
          data-testid={IDS.MAIL_EXPORT_DOWNLOAD_BUTTON}
          onclick={() => dispatch('Download')}
          disabled={busy || !snap.download_url}
        >{t.mail_export.download_button}</button>
        <span class="muted small mono" data-testid={IDS.MAIL_EXPORT_DOWNLOAD_URL}>{snap.download_url}</span>
        <div class="nav">
          <button class="btn danger" data-testid={IDS.MAIL_EXPORT_DISCARD_BUTTON} onclick={() => dispatch('Discard')} disabled={busy}>{t.mail_export.discard_button}</button>
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
  .mono { font-family: monospace; word-break: break-all; }
  .step { display: flex; flex-direction: column; gap: 0.5rem; max-width: 32rem; margin-top: 0.5rem; }
  .nav { display: flex; gap: 0.5rem; margin-top: 0.5rem; }
  .mailboxes { display: flex; flex-direction: column; gap: 0.25rem; }
  .mailbox, .opt { display: flex; align-items: flex-start; gap: 0.5rem; }
  .opt-text { display: flex; flex-direction: column; gap: 0.125rem; }
  .summary { font-size: 0.9rem; }
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
  .btn.danger.armed { background: var(--danger); color: #fff; }
  progress { width: 100%; }
</style>
