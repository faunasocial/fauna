<script lang="ts">
  /** The STANDING unsent-changes section on Settings → Account
   *  (`account-offline-mutation.md` § The offline-mutation contract →
   *  *The unsent-changes surface*, point 1), the pending-actions section's
   *  sibling and shape — twin of tui's `unsent_changes_elements`. Always
   *  present; the h2 carries the testid and the three-state honest text (bare
   *  title before the first read / empty line once read and empty / counted
   *  title). One row per outbox intent in the shared read's order, its
   *  description and status from the shared describer (never composed here).
   *  Retry and discard on a not-sent row only; discard arms a confirm on that
   *  row first. Every read and door is this device's outbox, so the section
   *  works with the nest down.
   *
   *  The generic drain tells the app nothing when it parks or sends an
   *  intent, so the list is re-read on the shared unsent tick while mounted
   *  (tui's `spawn_unsent_refresh`). */
  import { onDestroy, onMount } from 'svelte';
  import { t } from '#lib/i18n/strings';
  import { IDS } from '#lib/generated/uiIds';
  import { resolveLocalized } from '#lib/i18n/localized';
  import {
    unsentChanges,
    unsentChangeStateText,
    unsentChangeReasonText,
    retryUnsentChange,
    discardUnsentChange,
    type UnsentChange,
  } from '#lib/wasm';
  import { startUnsentTick } from '#lib/unsent-changes';

  // Raw, not a deep proxy: each row is handed back whole to the two wasm text
  // functions, which deserialize the plain object the shared read produced.
  let rows = $state.raw<UnsentChange[] | null>(null);
  let armed = $state<string | null>(null);
  let error = $state('');
  let stopTick: (() => void) | null = null;
  let destroyed = false;

  async function load() {
    try {
      const read = await unsentChanges();
      if (destroyed) return;
      rows = read;
      error = '';
      // A discard armed on a row that has since left the list disarms.
      if (armed && !read.some((r) => r.intent_id === armed)) armed = null;
    } catch (e) {
      // The section keeps its last honest text on a failed read.
      error = e instanceof Error ? e.message : String(e);
    }
  }

  /** The row's state line: the shared state text, and on a not-sent row why. */
  function status(row: UnsentChange): string {
    const state = resolveLocalized(unsentChangeStateText(row));
    const reason = unsentChangeReasonText(row);
    return reason
      ? t.settings.unsent_changes.status_with_reason({ state, reason: resolveLocalized(reason) })
      : state;
  }

  async function retry(id: string) {
    armed = null;
    try {
      await retryUnsentChange(id);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
    await load();
  }

  async function confirmDiscard(id: string) {
    armed = null;
    try {
      await discardUnsentChange(id);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
    await load();
  }

  onMount(() => {
    void load();
    stopTick = startUnsentTick(load);
  });
  onDestroy(() => {
    destroyed = true;
    stopTick?.();
  });
</script>

<section class="section">
  <h2 data-testid={IDS.UNSENT_CHANGES_SECTION}>
    {rows === null
      ? t.settings.unsent_changes.title
      : rows.length === 0
        ? t.settings.unsent_changes.none
        : t.settings.unsent_changes.title_count({ count: String(rows.length) })}
  </h2>
  {#each rows ?? [] as row (row.intent_id)}
    <div data-testid={IDS.UNSENT_CHANGE_ITEM} class="unsent-change-item">
      <span data-testid={IDS.UNSENT_CHANGE_DESCRIPTION}>{resolveLocalized(row.description.label)}</span>
      <span data-testid={IDS.UNSENT_CHANGE_STATUS}>{status(row)}</span>
      {#if row.state.state === 'unsent'}
        <button
          data-testid={IDS.UNSENT_CHANGE_RETRY_BUTTON}
          class="btn small"
          onclick={() => retry(row.intent_id)}
        >
          {t.settings.unsent_changes.retry}
        </button>
        <button
          data-testid={IDS.UNSENT_CHANGE_DISCARD_BUTTON}
          class="btn danger small"
          onclick={() => (armed = row.intent_id)}
        >
          {t.settings.unsent_changes.discard}
        </button>
        {#if armed === row.intent_id}
          <button
            data-testid={IDS.UNSENT_CHANGE_DISCARD_CONFIRM_BUTTON}
            class="btn danger small"
            onclick={() => confirmDiscard(row.intent_id)}
          >
            {t.settings.unsent_changes.discard_confirm}
          </button>
        {/if}
      {/if}
    </div>
  {/each}
  {#if error}<p class="error" data-testid={IDS.ERROR_MESSAGE}>{error}</p>{/if}
</section>

<style>
  .unsent-change-item {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.5rem;
    padding: 0.25rem 0;
  }
</style>
