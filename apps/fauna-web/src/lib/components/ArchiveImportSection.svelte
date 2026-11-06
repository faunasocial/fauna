<script lang="ts">
  // Settings → Import from other services: the six-screen archive-import wizard
  // (Source → Archive → Scope → Confirm → Progress → Done), a dumb renderer of
  // the shared `ArchiveImportMachine` (`libs/fauna-archive-import-machine`, web
  // face in the core chunk, `libs/fauna-wasm/src/archive_import.rs`). Behavior + IDs:
  // docs/goal/behavior/archive-import.md § The wizard and its machine, ui.yaml
  // § archive-import / archive-import-category-progress-list. Lifts the tui lead
  // shape (apps/fauna-tui/src/settings/archive_import.rs).
  //
  // Every line comes from the machine's composed page text (`pageTextOf`,
  // resolved with `resolveComposed`) and every word from its label maps: no
  // template is filled here.
  //
  // The archive is a real `<input type=file>` (ui.yaml's web note): picking a
  // file reads it whole into the chunk (a synchronous range read of a `File` is
  // impossible on the browser's one thread — archive-import.md § Implementation
  // status today) and opens it; the open button clicks the input.
  import { onDestroy, onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import { identity } from '#lib/store';
  import { openUrl } from '#lib/open-url';
  import { archiveImportMachine } from '#lib/rpc';
  import { archiveLinksHandle } from '#lib/archive-links';
  import { ensureWasm } from '#lib/wasm';
  import { getDeviceId } from '#lib/device-id';
  import { accountRuntimeNextSettled } from '#lib/account-runtime';
  import { getConversationsManager } from '#lib/conversations';
  import { getFeedManager, refreshFeed } from '#lib/feed';
  import { onStoreChange } from '#lib/store-change';
  import { resolveComposed, resolveLocalized } from '#lib/i18n/localized';
  import { t } from '#lib/i18n/strings';
  import { IDS } from '#lib/generated/uiIds';
  import {
    AUDIENCE_MODES,
    SOURCE_KINDS,
    audienceModeLabel,
    pageTextOf,
    snapshotOf,
    sourceHelp,
    sourceKindLabel,
    type ArchiveImportMachine,
    type ArchiveImportPageText,
    type ArchiveImportSnapshot,
    type ArchiveSourceKind,
  } from '#lib/wasm-archive-import';

  // The settings page owns the single error surface.
  let { error = $bindable('') } = $props();

  let machine = $state<ArchiveImportMachine | null>(null);
  let unavailable = $state(false);
  let snap = $state<ArchiveImportSnapshot | null>(null);
  let text = $state<ArchiveImportPageText | null>(null);
  let busy = $state(false);
  let fileInput = $state<HTMLInputElement | null>(null);
  let dateFromInput = $state('');
  let dateToInput = $state('');
  // Cancel's two-click arm and the Done step's skip-log reveal: page state the
  // snapshot never touches.
  let cancelArmed = $state(false);
  let showSkipped = $state(false);
  let poll: ReturnType<typeof setInterval> | null = null;

  function render(): void {
    if (!machine) return;
    snap = snapshotOf(machine);
    text = pageTextOf(machine);
    error = snap.error ?? '';
    // The run mutates the snapshot between records; repaint while it runs.
    const running = snap.run_state === 'Running';
    if (running && !poll) poll = setInterval(render, 300);
    if (!running && poll) {
      clearInterval(poll);
      poll = null;
    }
  }

  /** Drive the run until it pauses, stops or completes — after a Start, a
   *  Resume, or a hydrate that finds it Running (the tui executor's rule:
   *  whenever the post-dispatch snapshot reads Running). Single-flight in the
   *  machine. The account's archive links ride every run (`attachLinks`): a
   *  run that completes puts the link counts on the Done step and runs the
   *  announce round itself (archive-import.md § Linking and merging, rulings
   *  4 and 11), whichever page the user is on by then. */
  function driveIfRunning(): void {
    if (!machine?.isRunning()) return;
    const links = archiveLinksHandle();
    if (links) machine.attachLinks(links);
    render();
    machine
      .runImport()
      .catch((e: unknown) => {
        error = e instanceof Error ? e.message : String(e);
      })
      .finally(render);
  }

  /** View imported — the Feed on the imported-posts selection for the
   *  archive's platform (`feed.md` § State & data shape → *The imported-posts
   *  selection*): the shared `select_imported`, whose token is the platform's
   *  lowercase protocol name (`facebook` | `instagram`). */
  async function viewImported(): Promise<void> {
    try {
      const manager = await getFeedManager();
      if (snap) await manager.selectImported(String(snap.source_kind).toLowerCase());
      refreshFeed();
    } catch (e) {
      console.warn('select imported failed:', e);
    }
    await goto('/app/feed');
  }

  async function dispatch(...actions: unknown[]): Promise<void> {
    if (!machine) return;
    busy = true;
    try {
      for (const action of actions) await machine.dispatch(action);
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
      render();
    }
    driveIfRunning();
  }

  onMount(async () => {
    await ensureWasm();
    identity.init();
    const id = $identity;
    if (!id) return;
    try {
      machine = await archiveImportMachine(id.secretHex, getDeviceId(id.actorId));
    } catch (e: unknown) {
      console.warn('[archive-import] build machine:', e);
      unavailable = true;
      // The notice says THAT the page cannot run; the page's one error
      // surface says why (convention 2).
      error = e instanceof Error ? e.message : String(e);
      return;
    }
    if (__FAUNA_E2E_AUTOMATION__) {
      const { setArchiveImportMachine } = await import('#lib/archive-import-e2e');
      setArchiveImportMachine(machine);
    }
    // Held busy until the first hydrate lands: a hydrate that finds nothing
    // to resume puts the wizard on its Source step, so a step the user took
    // while it was in flight would be thrown back (the machine's `refresh`).
    busy = true;
    render();
    // The folders and their keys rest in this tab's account runtime, which the
    // conversations build starts and nothing awaits: a page entered right after
    // sign-in — a second device's first visit — would hydrate over a custody
    // that answers "not running" and offer a fresh wizard where an unfinished
    // import waits. Hydrate at the runtime's settle (`#lib/launch-pass`'s rule).
    void getConversationsManager().catch(() => {});
    await accountRuntimeNextSettled();
    if (!machine) return;
    await machine.hydrate().catch(() => {});
    busy = false;
    render();
    driveIfRunning();
    // A store-backed page joins the store-change notice (`#lib/store-change`):
    // a second device's store receives the folder keys after its first visit,
    // and the unfinished import it then can read is offered without a reload.
    // Only an untouched Source step re-reads — never a wizard in progress or a
    // run.
    leaveStoreChange = onStoreChange(() => {
      if (!machine || busy || snap?.step !== 'Source' || snap.run_state === 'Running') return;
      busy = true;
      void machine.hydrate().catch(() => {}).then(() => {
        busy = false;
        render();
        driveIfRunning();
      });
    });
  });

  let leaveStoreChange: (() => void) | null = null;

  onDestroy(() => {
    leaveStoreChange?.();
    if (poll) clearInterval(poll);
    if (__FAUNA_E2E_AUTOMATION__) {
      void import('#lib/archive-import-e2e').then((m) => m.setArchiveImportMachine(null));
    }
  });

  async function onFilePicked(e: Event): Promise<void> {
    const file = (e.currentTarget as HTMLInputElement).files?.[0];
    if (!file || !machine) return;
    machine.setArchiveFile(file.name, new Uint8Array(await file.arrayBuffer()));
    await dispatch({ SetArchivePath: { value: file.name } }, 'OpenArchive');
  }

  function onCancel(): void {
    if (!cancelArmed) {
      cancelArmed = true;
      return;
    }
    cancelArmed = false;
    void dispatch('Cancel');
  }

  function openExportPage(): void {
    if (snap?.source_export_url) openUrl(snap.source_export_url);
  }

  let profileOffered = $derived(!!snap?.archive_profile && !snap?.profile_applied);
  let concluded = $derived(snap?.run_state === 'Cancelled' || snap?.run_state === 'Completed');
</script>

<section class="section">
  {#if unavailable}
    <p class="muted small">{t.archive_import.unavailable}</p>
  {:else if snap && text}
    <p class="muted small">{t.archive_import.description}</p>

    {#if snap.step === 'Source'}
      <div class="step">
        <h3 class="muted small">{t.archive_import.source_title}</h3>
        <select
          class="input"
          data-testid={IDS.ARCHIVE_IMPORT_SOURCE_PICKER}
          aria-label={t.archive_import.source_title}
          value={snap.source_kind}
          onchange={(e) =>
            dispatch({ SelectSource: { kind: (e.currentTarget as HTMLSelectElement).value as ArchiveSourceKind } })}
          disabled={busy}
        >
          {#each SOURCE_KINDS as kind (kind)}
            <option value={kind}>{sourceKindLabel(kind)}</option>
          {/each}
        </select>
        <p class="small" data-testid={IDS.ARCHIVE_IMPORT_SOURCE_HELP}>{resolveLocalized(sourceHelp(snap.source_kind))}</p>
        <button class="link" data-testid={IDS.ARCHIVE_IMPORT_SOURCE_EXPORT_LINK} onclick={openExportPage}>
          {resolveComposed(text.source_link)}
        </button>
        <div class="actions">
          <button class="btn" data-testid={IDS.WIZARD_NEXT_BUTTON} disabled={busy} onclick={() => dispatch('Next')}>
            {t.archive_import.next}
          </button>
        </div>
      </div>
    {:else if snap.step === 'Archive'}
      <div class="step">
        <h3 class="muted small">{t.archive_import.archive_title}</h3>
        <input
          class="input"
          type="file"
          accept=".zip,application/zip"
          data-testid={IDS.ARCHIVE_IMPORT_ARCHIVE_PATH}
          aria-label={t.archive_import.archive_path_placeholder}
          bind:this={fileInput}
          onchange={onFilePicked}
          disabled={busy}
        />
        <button class="btn" data-testid={IDS.ARCHIVE_IMPORT_ARCHIVE_OPEN_BUTTON} disabled={busy} onclick={() => fileInput?.click()}>
          {t.archive_import.archive_open_button}
        </button>
        {#if text.archive_summary}
          <p class="small" data-testid={IDS.ARCHIVE_IMPORT_ARCHIVE_SUMMARY}>{resolveComposed(text.archive_summary)}</p>
        {/if}
        <div class="actions">
          <button class="btn" data-testid={IDS.WIZARD_BACK_BUTTON} disabled={busy} onclick={() => dispatch('Back')}>
            {t.archive_import.back}
          </button>
          <button class="btn" data-testid={IDS.WIZARD_NEXT_BUTTON} disabled={busy} onclick={() => dispatch('Next')}>
            {t.archive_import.next}
          </button>
        </div>
      </div>
    {:else if snap.step === 'Scope'}
      <div class="step">
        <h3 class="muted small">{t.archive_import.scope_title}</h3>
        <div data-testid={IDS.ARCHIVE_IMPORT_SCOPE_CATEGORIES} aria-label={t.archive_import.scope_categories_label}>
          {#each snap.categories as row, i (row.token)}
            {#if row.importable}
              <label class="row" data-testid={IDS.ARCHIVE_IMPORT_SCOPE_CATEGORY_ITEM} data-state={row.selected ? 'on' : 'off'}>
                <input
                  type="checkbox"
                  checked={row.selected}
                  disabled={busy}
                  onchange={() => dispatch({ ToggleCategory: { token: row.token, selected: !row.selected } })}
                /><!-- No whitespace before the line: the row's text is the line alone,
                     as on every app. --><span class="row-text">{resolveComposed(text.scope_rows[i])}</span>
              </label>
            {:else}
              <!-- Model-only in phase one: kept in the folder, never selectable. -->
              <div class="row muted" data-testid={IDS.ARCHIVE_IMPORT_SCOPE_CATEGORY_ITEM} data-state="kept">
                {resolveComposed(text.scope_rows[i])}
              </div>
            {/if}
          {/each}
        </div>
        <select
          class="input"
          data-testid={IDS.ARCHIVE_IMPORT_SCOPE_AUDIENCE_MODE}
          aria-label={t.archive_import.scope_audience_mode_label}
          value={resolveLocalized(audienceModeLabel(snap.audience_mode))}
          onchange={(e) => {
            // The option's value is its visible label — the web convention
            // (`RuleEditor`), so a select names an option the way every app's
            // driver does; the mode is mapped back here.
            const picked = (e.currentTarget as HTMLSelectElement).value;
            const mode = AUDIENCE_MODES.find((m) => resolveLocalized(audienceModeLabel(m)) === picked);
            if (mode) dispatch({ SetAudienceMode: { mode } });
          }}
          disabled={busy}
        >
          {#each AUDIENCE_MODES as mode (mode)}
            <option value={resolveLocalized(audienceModeLabel(mode))}>{resolveLocalized(audienceModeLabel(mode))}</option>
          {/each}
        </select>
        <p class="small" data-testid={IDS.ARCHIVE_IMPORT_SCOPE_AUDIENCE_SUMMARY}>{resolveComposed(text.audience_summary)}</p>
        {#if snap.nest_supports_hidden_tiers === false}
          <p class="notice small">{t.archive_import.scope_hidden_tiers_unavailable}</p>
        {/if}
        <input
          class="input"
          data-testid={IDS.ARCHIVE_IMPORT_SCOPE_DATE_FROM}
          placeholder={t.archive_import.scope_date_from_placeholder}
          bind:value={dateFromInput}
          disabled={busy}
        />
        <input
          class="input"
          data-testid={IDS.ARCHIVE_IMPORT_SCOPE_DATE_TO}
          placeholder={t.archive_import.scope_date_to_placeholder}
          bind:value={dateToInput}
          disabled={busy}
        />
        <div class="actions">
          <button class="btn" data-testid={IDS.WIZARD_BACK_BUTTON} disabled={busy} onclick={() => dispatch('Back')}>
            {t.archive_import.back}
          </button>
          <button
            class="btn"
            data-testid={IDS.WIZARD_NEXT_BUTTON}
            disabled={busy}
            onclick={() =>
              dispatch(
                { SetDateFrom: { value: dateFromInput.trim() } },
                { SetDateTo: { value: dateToInput.trim() } },
                'Next',
              )}
          >
            {t.archive_import.next}
          </button>
        </div>
      </div>
    {:else if snap.step === 'Confirm'}
      <div class="step">
        <h3 class="muted small">{t.archive_import.confirm_title}</h3>
        <p class="small" data-testid={IDS.ARCHIVE_IMPORT_CONFIRM_SUMMARY}>{resolveComposed(text.confirm_summary)}</p>
        <div class="actions">
          <button class="btn" data-testid={IDS.WIZARD_BACK_BUTTON} disabled={busy} onclick={() => dispatch('Back')}>
            {t.archive_import.back}
          </button>
          <button class="btn primary" data-testid={IDS.ARCHIVE_IMPORT_START_BUTTON} disabled={busy} onclick={() => dispatch('Start')}>
            {t.archive_import.start_button}
          </button>
        </div>
      </div>
    {:else if snap.step === 'Progress'}
      <div class="step">
        <h3 class="muted small">{t.archive_import.progress_title}</h3>
        <p class="small" data-testid={IDS.ARCHIVE_IMPORT_PROGRESS_SUMMARY}>{resolveComposed(text.progress_summary)}</p>
        <progress
          data-testid={IDS.ARCHIVE_IMPORT_PROGRESS_BAR}
          data-fraction={text.progress_fraction.toFixed(4)}
          max="1"
          value={text.progress_fraction}
        >{Math.round(text.progress_fraction * 100)}%</progress>
        <div class="actions">
          <button class="btn" data-testid={IDS.ARCHIVE_IMPORT_PAUSE_BUTTON} disabled={busy} onclick={() => dispatch('Pause')}>
            {t.archive_import.pause_button}
          </button>
          <button class="btn" data-testid={IDS.ARCHIVE_IMPORT_RESUME_BUTTON} disabled={busy} onclick={() => dispatch('Resume')}>
            {t.archive_import.resume_button}
          </button>
          <button class="btn" data-testid={IDS.ARCHIVE_IMPORT_CANCEL_BUTTON} disabled={busy} onclick={onCancel}>
            {cancelArmed ? t.common.confirm_q : t.archive_import.cancel_button}
          </button>
        </div>
        <h4 class="muted small">{t.archive_import.error_log_title}</h4>
        <pre class="small" data-testid={IDS.ARCHIVE_IMPORT_ERROR_LOG}>{snap.skip_log.join('\n')}</pre>
        <ul data-testid={IDS.ARCHIVE_IMPORT_CATEGORY_PROGRESS_LIST} aria-label={t.archive_import.progress_title}>
          {#each text.progress_rows as row (row.token)}
            <li data-testid={IDS.ARCHIVE_IMPORT_CATEGORY_PROGRESS_LIST_ITEM}>
              <span data-testid={IDS.ARCHIVE_IMPORT_CATEGORY_PROGRESS_LIST_ITEM_NAME}>{resolveLocalized(row.label)}</span>
              <span data-testid={IDS.ARCHIVE_IMPORT_CATEGORY_PROGRESS_LIST_ITEM_PROGRESS}>{resolveComposed(row.progress)}</span>
            </li>
          {/each}
        </ul>
        {#if concluded}
          <!-- A concluded run may be left (Back starts a fresh wizard); a
               running, paused or errored one is resumable and has no Back. -->
          <div class="actions">
            <button class="btn" data-testid={IDS.WIZARD_BACK_BUTTON} disabled={busy} onclick={() => dispatch('Back')}>
              {t.archive_import.back}
            </button>
          </div>
        {/if}
      </div>
    {:else if snap.step === 'Done'}
      <div class="step">
        <h3 class="muted small">{t.archive_import.done_title}</h3>
        <p class="small" data-testid={IDS.ARCHIVE_IMPORT_DONE_SUMMARY}>{resolveComposed(text.done_summary)}</p>
        <div class="actions">
          <button class="btn" data-testid={IDS.ARCHIVE_IMPORT_VIEW_IMPORTED_BUTTON} onclick={viewImported}>
            {t.archive_import.view_imported_button}
          </button>
          <button class="btn" data-testid={IDS.ARCHIVE_IMPORT_REVIEW_SKIPPED_BUTTON} onclick={() => (showSkipped = !showSkipped)}>
            {t.archive_import.review_skipped_button}
          </button>
        </div>
        {#if showSkipped}
          <pre class="small" data-testid={IDS.ARCHIVE_IMPORT_ERROR_LOG}>{snap.skip_log.join('\n')}</pre>
        {/if}
        <!-- Offered, never applied silently: enabled while the run's end found
             a profile in the archive and the tap has not applied it yet. -->
        <button
          class="btn"
          data-testid={IDS.ARCHIVE_IMPORT_PROFILE_PREFILL_BUTTON}
          disabled={busy || !profileOffered}
          onclick={() => dispatch('ApplyArchiveProfile')}
        >
          {t.archive_import.profile_prefill_button}
        </button>
        <button class="link" data-testid={IDS.ARCHIVE_IMPORT_FOLDER_LINK} onclick={() => goto('/app/settings/folders')}>
          {resolveComposed(text.folder_link)}
        </button>
        <div class="actions">
          <button class="btn" data-testid={IDS.WIZARD_BACK_BUTTON} disabled={busy} onclick={() => dispatch('Back')}>
            {t.archive_import.back}
          </button>
        </div>
      </div>
    {/if}
  {/if}
</section>

<style>
  .step {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }
  .actions {
    display: flex;
    gap: 0.5rem;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }
  .link {
    background: none;
    border: none;
    padding: 0;
    color: var(--accent);
    text-align: left;
    cursor: pointer;
  }
  pre {
    white-space: pre-wrap;
  }
</style>
