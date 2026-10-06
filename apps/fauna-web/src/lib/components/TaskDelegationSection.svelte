<script lang="ts">
  // Settings → Task delegation (`docs/goal/behavior/participants.md` § Task
  // delegation; ui.yaml page `task-delegation`). A person sees each heavy
  // background task kind — its current runner and its assignment — and may pin a
  // kind to a capable participant instead of letting the automatic policy order
  // choose (always-on nest → plugged-in desktop → never a battery mobile: the work
  // queues rather than run on a phone).
  //
  // Everything rendered here comes from the one shared view-model
  // (`fauna_client_delegation::TaskDelegationView` via `WasmTaskDelegationView`) —
  // the same layer linux/windows render, so no client re-derives the surface (#2).
  // In particular the SPA renders `row.pin_options` VERBATIM: which options a
  // picker may offer is a correctness surface, not a cosmetic one, and shared Rust
  // owns it (participants.md § The assignment picker). Web is `ViewerOnly` — a
  // browser tab runs no lease driver — so "This device" is never among them; an
  // existing pin made from another device still renders, so the user can escape it.
  import { identity } from '$lib/store';
  import { taskDelegationView, syncDevicesList } from '$lib/rpc';
  import { getDeviceId } from '$lib/device-id';
  import { ensureWasm } from '$lib/wasm';
  import { resolveLocalized } from '$lib/i18n/localized';
  import { onDestroy, onMount } from 'svelte';
  import { onStoreChange } from '$lib/store-change';
  import { t } from '$lib/i18n/strings';
  import {
    pinOptionKey,
    pinOptionLabel,
    runnerText,
    type TaskDelegationRow,
  } from '$lib/task-delegation';
  import type { WasmTaskDelegationView } from '../../../static/fauna_wasm.js';
  import { IDS } from '$lib/generated/uiIds';

  // The parent settings page owns the single error surface (`error-message`).
  let { error = $bindable('') } = $props();

  let view: WasmTaskDelegationView | null = null;
  let rows = $state<TaskDelegationRow[]>([]);
  // hex `device_id` → display label, from the roster. Absent names fall back to
  // the shared `short_id` (see `$lib/task-delegation`), so a missing roster only
  // degrades the label, never the surface.
  let labels = $state<Map<string, string>>(new Map());
  let busy = $state(false);

  // The runner column is live lease state, so every mount re-reads it (a navigate
  // remounts this component — web is immune to the build-once shell trap, but the
  // page must still show the lease as it is *now*, not as it was at first paint).
  async function load(): Promise<void> {
    if (!view) return;
    rows = (await view.load()) as TaskDelegationRow[];
  }

  // The open page's re-read on a store-change notice (`$lib/store-change`): the
  // pins rest in the account store. A pick in flight re-loads by itself, and a
  // failed re-read leaves the rows on screen standing.
  onDestroy(
    onStoreChange(() => {
      if (!busy) void load().catch(() => {});
    }),
  );

  onMount(async () => {
    const id = $identity;
    if (!id?.secretHex) return;
    try {
      await ensureWasm();
      view = await taskDelegationView(id.secretHex, getDeviceId(id.actorId));
      await load();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
      return;
    }
    // Best-effort: the roster only *names* a foreign runner. A failure here must
    // not blank the page, so it degrades to short-hex rather than raising.
    try {
      const devices = await syncDevicesList(id.secretHex);
      labels = new Map(devices.map((d) => [d.device_id, d.label]));
    } catch {
      labels = new Map();
    }
  });

  // The picker's `onchange`. The selected `<option value>` is the stable
  // cross-app key, so we map it back onto the very `PinOption` the shared layer
  // offered — never a re-derived one. The write rides the config CAS inside
  // `setAssignment`; we then re-`load()` so the row shows what the nest actually
  // holds (on failure too — the picker must never lie about a write that lost).
  async function onPick(row: TaskDelegationRow, event: Event): Promise<void> {
    const key = (event.currentTarget as HTMLSelectElement).value;
    const option = row.pin_options.find((o) => pinOptionKey(o) === key);
    if (!option || !view) return;
    busy = true;
    try {
      await view.setAssignment(row.task_kind, option);
      error = '';
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
    try {
      await load();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
    busy = false;
  }
</script>

<section class="section" data-testid={IDS.TASK_DELEGATION}>
  <h2>{t.task_delegation.title}</h2>
  <p class="muted small">{t.task_delegation.description}</p>

  <div class="kind-list" data-testid={IDS.TASK_DELEGATION_LIST}>
    {#each rows as row (row.task_kind)}
      <div class="kind-item" data-testid={IDS.TASK_DELEGATION_KIND_ITEM}>
        <div class="kind-text">
          <span class="kind-name" data-testid={IDS.TASK_DELEGATION_KIND_NAME}>
            {resolveLocalized(row.name)}
          </span>
          <span class="muted small" data-testid={IDS.TASK_DELEGATION_KIND_RUNNER}>
            {runnerText(row.runner, labels)}
          </span>
        </div>
        <select
          class="input"
          data-testid={IDS.TASK_DELEGATION_ASSIGNMENT_PICKER}
          value={pinOptionKey(row.assignment)}
          onchange={(e) => onPick(row, e)}
          disabled={busy}
        >
          {#each row.pin_options as option (pinOptionKey(option))}
            <option value={pinOptionKey(option)}>{pinOptionLabel(option, labels)}</option>
          {/each}
        </select>
      </div>
    {/each}
  </div>
</section>

<style>
  .kind-list {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    margin-top: 0.75rem;
  }
  .kind-item {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 1rem;
  }
  .kind-text {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .kind-name {
    font-weight: 500;
  }
</style>
