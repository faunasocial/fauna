<script lang="ts">
  // Settings → Community labelers — browse + inspect-before-subscribe +
  // (un)subscribe (content-moderation-and-ranking.md § Tier-3 community
  // models & background re-processing). Every published labeler
  // (`fauna.labelers.list`), each with an inspect panel rendering the
  // decoded + client-side-RE-VERIFIED signed metadata (the trust gate: the
  // user reads the exact module's facts before granting it their content) and
  // subscribe/unsubscribe. A dumb renderer of the shared
  // `LabelerCatalogMachine` (`libs/fauna-labeler-catalog-machine`, WASM twin
  // `libs/fauna-wasm-labeler-catalog`) — the SAME machine + snapshot the
  // Personalization home reads (filtered there to `subscribed === true`).
  // Mirrors `apps/fauna-linux/src/views/personalization/mod.rs`'s
  // `CatalogShell`.
  //
  // v1 always subscribes with no capability grant (`grant_id: null`), even
  // for a restricted (`mail`) content_kind — an honest degrade: the
  // subscription registers the re-score obligation, but the MDA drain has
  // nothing to unseal until a matching capability is minted to the user's own
  // MDA holder.
  import { identity } from '#lib/store';
  import { sharedRpcPort } from '#lib/rpc';
  import { ensureWasm, textModelNeedsNewerApp } from '#lib/wasm';
  import LabelerInspectPanel from '#lib/components/LabelerInspectPanel.svelte';
  import { createLabelerCatalogMachine } from '#lib/wasm-labeler-catalog';
  import { resolveLocalized } from '#lib/i18n/localized';
  import { onMount, onDestroy } from 'svelte';
  import { t } from '#lib/i18n/strings';
  import type { LabelerCatalogSnapshot } from '#lib/labeler-catalog-machine';
  import type { LabelerCatalogMachine } from '../../../static/fauna_wasm_labeler_catalog.js';
  import { IDS } from '#lib/generated/uiIds';

  let { error = $bindable('') } = $props();

  let machine: LabelerCatalogMachine | null = null;
  let snap = $state<LabelerCatalogSnapshot | null>(null);
  /** Per-row subscribe-enable, read from the shared machine on every tick
   *  (`subscribeEnabled` — false while the inspected row fails the shared
   *  `subscribable` gate), never a `verified` check of the SPA's own. */
  let subscribeEnabled = $state<boolean[]>([]);
  let ready = $state(false);
  let timer: ReturnType<typeof setInterval> | null = null;

  function applySnapshot(): void {
    if (!machine) return;
    const raw = machine.snapshotJson();
    snap = raw ? (JSON.parse(raw) as LabelerCatalogSnapshot) : null;
    const m = machine;
    subscribeEnabled = (snap?.entries ?? []).map((_, i) => m.subscribeEnabled(i));
    error = snap?.error ? resolveLocalized(snap.error) : '';
  }

  /** `labeler-catalog-item-kind`'s ONE non-verbatim field: a subscribed
   *  `text-model` whose tokenizer contract this build does not implement
   *  says so here, because the compose seam leaves that factor inert and
   *  this row is where the user learns why. The predicate and wording are
   *  both shared (`textModelNeedsNewerApp`, over the same
   *  `scoring::text_model_version_supported` the seam's inert branch reads),
   *  so a badge that disagreed with the scorer is not expressible. `null` =
   *  paint the kind verbatim, which is every ordinary row. */
  function kindBadgeText(entry: { artifact_kind: string; artifact_version: number }): string {
    const note = textModelNeedsNewerApp(entry.artifact_kind, entry.artifact_version);
    return note ? resolveLocalized(note) : entry.artifact_kind;
  }

  onMount(async () => {
    const id = $identity;
    if (!id) {
      ready = true;
      return;
    }
    try {
      await ensureWasm();
      machine = await createLabelerCatalogMachine(
        { onChanged: () => applySnapshot() },
        await sharedRpcPort(id.secretHex),
        id.secretHex,
      );
      ready = true;
      await machine.refresh();
      applySnapshot();
      timer = setInterval(() => { machine?.refresh(); }, 15000);
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : String(e);
      ready = true;
    }
  });

  onDestroy(() => {
    if (timer) clearInterval(timer);
  });

  async function inspect(index: number): Promise<void> {
    await machine?.inspect(index);
    applySnapshot();
  }

  function closeInspect(): void {
    machine?.closeInspect();
    applySnapshot();
  }

  async function subscribe(index: number): Promise<void> {
    await machine?.subscribe(index);
    applySnapshot();
  }

  async function unsubscribe(index: number): Promise<void> {
    await machine?.unsubscribe(index);
    applySnapshot();
  }
</script>

{#if !ready}
  <p class="muted">{t.common.loading}</p>
{:else if !$identity}
  <p class="muted">{t.common.identity_required}</p>
{:else}
  <section class="section" data-testid={IDS.LABELER_CATALOG}>
    <h2>{t.labeler_catalog.title}</h2>

    {#if snap?.inspecting}
      <LabelerInspectPanel inspecting={snap.inspecting} onclose={closeInspect} />
    {/if}

    {#if snap?.loaded && snap.entries.length === 0}
      <p class="muted" data-testid={IDS.LABELER_CATALOG_EMPTY}>{t.labeler_catalog.empty}</p>
    {:else if (snap?.entries.length ?? 0) > 0}
      <div class="labeler-list" data-testid={IDS.LABELER_CATALOG_LIST}>
        {#each snap?.entries ?? [] as entry, i (entry.labeler_id)}
          <div class="labeler-item" data-testid={IDS.LABELER_CATALOG_ITEM}>
            <div class="labeler-info">
              <span data-testid={IDS.LABELER_CATALOG_ITEM_PUBLISHER}>{entry.publisher_actor}</span>
              <!-- The artifact kind (list | wasm, machine-normalized) — a
                   curated List is distinguishable from an executable module
                   BEFORE inspect (content-moderation-and-ranking.md § Tier-3
                   artifact kinds). -->
              <span data-testid={IDS.LABELER_CATALOG_ITEM_KIND}>{kindBadgeText(entry)}</span>
              <span data-testid={IDS.LABELER_CATALOG_ITEM_CONTENT_KIND}>{entry.content_kind}</span>
              <span data-testid={IDS.LABELER_CATALOG_ITEM_VERSION}>{entry.version}</span>
              <span data-testid={IDS.LABELER_CATALOG_ITEM_FACTOR}>{entry.factor}</span>
            </div>
            <div class="labeler-actions">
              <button class="btn btn-small" data-testid={IDS.LABELER_CATALOG_ITEM_INSPECT_BUTTON} onclick={() => inspect(i)}>
                {t.labeler_catalog.inspect}
              </button>
              {#if !entry.subscribed}
                <button class="btn btn-small btn-primary" data-testid={IDS.LABELER_CATALOG_ITEM_SUBSCRIBE_BUTTON} disabled={!subscribeEnabled[i]} onclick={() => subscribe(i)}>
                  {t.labeler_catalog.subscribe}
                </button>
              {:else}
                <button class="btn btn-small" data-testid={IDS.LABELER_CATALOG_ITEM_UNSUBSCRIBE_BUTTON} onclick={() => unsubscribe(i)}>
                  {t.labeler_catalog.unsubscribe}
                </button>
              {/if}
            </div>
          </div>
        {/each}
      </div>
    {/if}
  </section>
{/if}

<style>
  h2 { font-size: 1.25rem; margin-bottom: 0.5rem; }
  .muted { color: var(--text-muted); }
  .section { margin-bottom: 2rem; }
  .labeler-list { display: flex; flex-direction: column; gap: 0.5rem; }
  .labeler-item {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 1rem;
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 0.75rem 1rem;
    background: var(--bg-surface);
  }
  .labeler-info { display: flex; flex-direction: column; gap: 0.125rem; font-size: 0.8125rem; }
  .labeler-actions { display: flex; gap: 0.5rem; flex-shrink: 0; }
  .btn {
    display: inline-block;
    padding: 0.5rem 0.875rem;
    border-radius: 6px;
    background: var(--bg-hover);
    color: var(--text);
    border: 1px solid var(--border);
    cursor: pointer;
    text-decoration: none;
  }
  .btn-small { font-size: 0.8125rem; padding: 0.375rem 0.625rem; }
  .btn-primary { background: var(--accent); color: var(--bg); border-color: var(--accent); }
</style>
