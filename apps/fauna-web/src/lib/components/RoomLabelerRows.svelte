<script lang="ts">
  // The community room's labeler set, inside the room policy editor
  // (`ui/conversations.md` § Element IDs → *The room's labeler set*;
  // `community-rooms.md`). One `room-labeler-toggle[i]` +
  // `room-labeler-inspect-button[i]` row per catalog labeler a room may name,
  // read off the SAME shared `LabelerCatalogMachine` Settings → Community
  // labelers drives. Every decision — which kinds a room may name, whether a
  // row is staged, whether it may act — is shared Rust's
  // (`roomMayNameLabelerKind`, `roomSettingsLabelerStaged`,
  // `roomSettingsLabelerToggleLive` over the editor's opaque draft); this
  // component paints. Inspect opens the catalog's inspect panel IN PLACE so the
  // editor's draft survives; its Close returns to the rows.
  import { onMount } from 'svelte';
  import { identity } from '$lib/store';
  import { sharedRpcPort } from '$lib/rpc';
  import { ensureWasm, roomMayNameLabelerKind } from '$lib/wasm';
  import { createLabelerCatalogMachine } from '$lib/wasm-labeler-catalog';
  import { t } from '$lib/i18n/strings';
  import LabelerInspectPanel from '$lib/components/LabelerInspectPanel.svelte';
  import type { LabelerCatalogSnapshot } from '$lib/labeler-catalog-machine';
  import type { LabelerCatalogMachine } from '../../../static/fauna_wasm_labeler_catalog.js';
  import type { WasmConversationsManager } from '../../../static/fauna_wasm.js';
  import { IDS } from '$lib/generated/uiIds';

  let {
    manager,
    draft,
    canSetPolicy,
    ontoggle,
  }: {
    manager: WasmConversationsManager;
    /** The editor's staged draft (opaque; shared Rust owns it). */
    draft: unknown;
    canSetPolicy: boolean;
    /** Stage the toggle for this labeler id — the page owns the draft. */
    ontoggle: (labelerId: string) => void;
  } = $props();

  let machine: LabelerCatalogMachine | null = null;
  let snap = $state<LabelerCatalogSnapshot | null>(null);

  function applySnapshot(): void {
    if (!machine) return;
    const raw = machine.snapshotJson();
    snap = raw ? (JSON.parse(raw) as LabelerCatalogSnapshot) : null;
  }

  // Opening the editor re-reads the catalog, best-effort: a failed read leaves
  // whatever the machine already holds (or no rows) and never blocks Save.
  onMount(async () => {
    const id = $identity;
    if (!id) return;
    try {
      await ensureWasm();
      machine = await createLabelerCatalogMachine(
        { onChanged: () => applySnapshot() },
        await sharedRpcPort(id.secretHex),
        id.secretHex,
      );
      applySnapshot();
      await machine.refresh();
      applySnapshot();
    } catch {
      /* best-effort */
    }
  });

  /** Catalog rows a room may name, keeping each row's catalog SNAPSHOT index
   *  (what inspect addresses), in catalog order. */
  let rows = $derived(
    (snap?.entries ?? [])
      .map((entry, index) => ({ entry, index }))
      .filter(({ entry }) => roomMayNameLabelerKind(entry.artifact_kind)),
  );

  async function inspect(index: number): Promise<void> {
    await machine?.inspect(index);
    applySnapshot();
  }

  function closeInspect(): void {
    machine?.closeInspect();
    applySnapshot();
  }
</script>

{#if snap?.inspecting}
  <LabelerInspectPanel inspecting={snap.inspecting} onclose={closeInspect} />
{:else if rows.length > 0}
  <div class="room-labelers">
    <span>{t.conversations.unified.room_labelers_label}</span>
    {#each rows as { entry, index } (entry.labeler_id)}
      {@const staged = manager.roomSettingsLabelerStaged(draft, entry.labeler_id)}
      <div class="room-row">
        <span>{entry.publisher_actor} · {entry.factor}</span>
        <button
          data-testid={IDS.ROOM_LABELER_TOGGLE}
          class="action-btn"
          data-checked={staged ? 'true' : 'false'}
          disabled={!canSetPolicy ||
            !manager.roomSettingsLabelerToggleLive(draft, entry.labeler_id)}
          onclick={() => ontoggle(entry.labeler_id)}
        >
          {staged
            ? t.conversations.unified.room_labeler_on
            : t.conversations.unified.room_labeler_off}
        </button>
        <button
          data-testid={IDS.ROOM_LABELER_INSPECT_BUTTON}
          class="action-btn"
          onclick={() => inspect(index)}
        >
          {t.labeler_catalog.inspect}
        </button>
      </div>
    {/each}
  </div>
{/if}

<style>
  .room-labelers { display: flex; flex-direction: column; gap: 0.5rem; margin-bottom: 0.5rem; }
  .room-row { display: flex; align-items: center; gap: 0.5rem; justify-content: space-between; }
  .action-btn {
    padding: 0.375rem 0.625rem;
    border-radius: 6px;
    background: var(--bg-hover);
    color: var(--text);
    border: 1px solid var(--border);
    cursor: pointer;
  }
  .action-btn:disabled { opacity: 0.5; cursor: default; }
</style>
