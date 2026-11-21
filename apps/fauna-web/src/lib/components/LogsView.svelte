<script lang="ts">
  // Shared rendering for the two Fauna log surfaces (observability.md
  // § Surfaces) — the **Settings → Logs** sub-page (the client's own
  // process-global `fauna_log` ring) and the **admin Logs** page (the nest's
  // ring fetched over `fauna.admin.logs`). Both render the same `LogEntry`
  // list with the same severity filter and the same `log-entry` rows; the only
  // difference is the *source* and whether a Clear is offered (the nest ring
  // has no RPC to wipe it).
  //
  // The severity filter (level map), the `LEVEL · HH:MM:SS · target · message`
  // line, the two-line `LogRow` row form, and the newest-first copy payload all
  // come from the shared `fauna_log::format` logic over the `fauna-wasm` `log*`
  // exports — this component is no longer a web "twin of `logs_view.rs`"
  // (priority #2/#3, observability.md § Render logic lifted to shared Rust). The
  // filter narrows the held entries in memory (`logFilterEntries`), so one path
  // serves both surfaces — the admin view has no local ring to re-query, and the
  // client ring page is a synchronous snapshot. The browser supplies the only
  // environmental input the pure formatter can't read: its local UTC offset.
  //
  // Redaction rule (observability.md § Persistence & privacy): we render only
  // what `tracing` captured; call sites are forbidden from logging message
  // plaintext or secrets. This layer enforces nothing.
  import { t } from '#lib/i18n/strings';
  import {
    logFilterEntries,
    logRenderedText,
    logRingActionPossibility,
    logRows,
    type LogEntry,
  } from '#lib/wasm';
  import { IDS } from '#lib/generated/uiIds';
  import { utcOffsetSeconds } from '#lib/utcOffset';
  import { resolveLocalized } from '#lib/i18n/localized';
  import { askPossibility, formGate } from '#lib/form-gate';
  import { DEVICE_LOGS_EMPTY_KEY, clearRing, copyRing } from '#lib/ring-actions';
  import { copyAndReport } from '#lib/copy-confirm';

  let {
    entries = [],
    showClear = false,
    onClear,
    emptyKey = DEVICE_LOGS_EMPTY_KEY,
    error = $bindable(''),
  }: {
    entries: LogEntry[];
    showClear?: boolean;
    onClear?: () => void;
    /** The page's own empty line — also the reason copy/clear are blocked on
     *  an empty ring (`logs.empty` / `admin.logs_page.empty`). */
    emptyKey?: string;
    /** The host page's `error-message` text: a refused action or a failed
     *  clipboard write is said there. */
    error?: string;
  } = $props();

  // Filter options: `value` is the stable English token the e2e `select` passes
  // (the web bridge selects an `<option>` by its `value`, not its label), the
  // label is localized. Mirrors linux `logs_view::filter_labels` + the locked
  // `log-level-filter` severity set. "All" ⇒ no threshold (the shared
  // `parse_level` maps "all"/unknown → the least-severe threshold → everything).
  const filterOptions = [
    { value: 'All', label: t.logs.filter_all },
    { value: 'Error', label: t.logs.level_error },
    { value: 'Warn', label: t.logs.level_warn },
    { value: 'Info', label: t.logs.level_info },
    { value: 'Debug', label: t.logs.level_debug },
    { value: 'Trace', label: t.logs.level_trace },
  ];

  let level = $state('All');

  // The browser's current local UTC offset in seconds — the one environmental
  // input the shared (pure, wasm-safe) formatter can't read itself. A Logs ring
  // spans minutes, so a single "now" offset matches every entry's wall-clock
  // display (the across-a-DST-boundary case is ignored, by design).
  const tzOffsetSecs = utcOffsetSeconds();

  // Entries narrowed to the selected severity (oldest-first, order preserved) via
  // the shared `filter_entries`. Guarded on the *unfiltered* count: both surfaces
  // start `entries=[]` and only fill it once wasm is ready (settings via
  // `ensureLogging()`+`logSnapshot()`, admin via the wasm WS-RPC `adminLogs`), so
  // a non-empty `entries` ⇒ wasm is initialized — the guard avoids calling a
  // `wasm()` export on the first pre-init empty render.
  let filtered = $derived(
    entries.length === 0 ? [] : logFilterEntries(entries, level.toLowerCase()),
  );
  // Rendered `log-entry` rows, **newest-first** (the shared `rows`).
  let rows = $derived(filtered.length === 0 ? [] : logRows(filtered, tzOffsetSecs));

  // `log-copy-button` / `log-clear-button` paint from the shared
  // `ring_action_possibility` over the entries the filter shows: blocked by the
  // page's own empty line on an empty ring (that line is already on screen
  // below, so the reason rides only the buttons' `title`, web's
  // disabled-with-a-reason idiom); unreadable before wasm → disabled. Both
  // handlers ask the same answer and refuse on `error-message`.
  const ringAnswer = () => logRingActionPossibility(filtered.length, emptyKey);
  let ringGate = $derived.by(() => {
    void filtered;
    return formGate(askPossibility(ringAnswer));
  });

  // What the last confirmed copy put on the clipboard — "Copied!" and the
  // `data-copied` value (the `account-actor-id-copy-btn` contract). The error
  // this view last said, so a later success clears only its own line.
  let copied = $state<string | null>(null);
  let ownError = '';

  function say(text: string) {
    if (text) {
      error = text;
    } else if (ownError && error === ownError) {
      error = '';
    }
    ownError = text;
  }

  async function copy() {
    const result = await copyRing(
      ringAnswer,
      () => logRenderedText(filtered, tzOffsetSecs),
      copyAndReport,
    );
    copied = result.copied;
    say(result.error);
  }

  function clear() {
    const refusal = clearRing(ringAnswer, () => onClear?.());
    if (refusal === null) copied = null;
    say(refusal ?? '');
  }
</script>

<div class="logs-controls">
  <label class="filter">
    {t.logs.filter_label}
    <select data-testid={IDS.LOG_LEVEL_FILTER} bind:value={level}>
      {#each filterOptions as opt (opt.value)}
        <option value={opt.value}>{opt.label}</option>
      {/each}
    </select>
  </label>
  <button
    class="btn"
    data-testid={IDS.LOG_COPY_BUTTON}
    data-copied={copied ?? undefined}
    disabled={ringGate.disabled}
    title={ringGate.reason || undefined}
    onclick={copy}>{copied !== null ? t.common.copied : t.logs.copy_button}</button
  >
  {#if showClear}
    <button
      class="btn danger"
      data-testid={IDS.LOG_CLEAR_BUTTON}
      disabled={ringGate.disabled}
      title={ringGate.reason || undefined}
      onclick={clear}
    >
      {t.logs.clear_button}
    </button>
  {/if}
</div>

{#if rows.length === 0}
  <p class="muted empty">{resolveLocalized({ key: emptyKey })}</p>
{:else}
  <ul class="log-list">
    {#each rows as row, i (i + '-' + row.line)}
      <li class="log-entry" data-testid={IDS.LOG_ENTRY} data-index={i}>
        <span class="msg">{row.message}</span>
        <span class="subtitle">{row.subtitle}</span>
      </li>
    {/each}
  </ul>
{/if}

<style>
  .logs-controls {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    margin-bottom: 1rem;
    flex-wrap: wrap;
  }
  .filter {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    font-size: 0.875rem;
    color: var(--text-muted);
  }
  .filter select {
    padding: 0.25rem 0.5rem;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg);
    color: var(--text);
    font-size: 0.875rem;
  }
  .btn {
    padding: 0.375rem 0.75rem;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg-surface);
    color: var(--text);
    cursor: pointer;
    font-size: 0.8rem;
  }
  .btn:hover { background: var(--bg-hover); }
  .btn.danger { border-color: var(--danger); color: var(--danger); }
  .empty { color: var(--text-muted); }
  .log-list {
    list-style: none;
    padding: 0;
    margin: 0;
    font-size: 0.8rem;
  }
  /* Canonical two-line row (message title over `LEVEL · time · target`
     subtitle), matching the linux adw row + the WinUI/SwiftUI/Compose two-line
     row (the shared `LogRow` shape). */
  .log-entry {
    display: flex;
    flex-direction: column;
    gap: 0.125rem;
    padding: 0.375rem 0;
    border-bottom: 1px solid var(--border);
  }
  .log-entry .msg { color: var(--text); word-break: break-word; }
  .log-entry .subtitle { color: var(--text-muted); font-size: 0.75rem; }
</style>
