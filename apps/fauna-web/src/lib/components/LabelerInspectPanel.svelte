<script lang="ts">
  // The labeler inspect panel — the decoded + client-side-RE-VERIFIED signed
  // metadata of one catalog entry, and (for a `list` / `text-model`) the whole
  // artifact, so a user reads the exact facts before granting the module their
  // content (content-moderation-and-ranking.md § Tier-3 community models). A
  // dumb renderer of the shared `LabelerCatalogMachine` snapshot's `inspecting`
  // value, hosted by Settings → Community labelers and, in place, by the
  // community room's policy editor (`ui/conversations.md` § Element IDs → *The
  // room's labeler set*) — one panel, so the two cannot drift.
  import { ngramDirectionLabel, ngramDocCountLabel } from '$lib/wasm';
  import { resolveLocalized } from '$lib/i18n/localized';
  import { t } from '$lib/i18n/strings';
  import type { LabelerCatalogSnapshot } from '$lib/labeler-catalog-machine';
  import { IDS } from '$lib/generated/uiIds';

  let {
    inspecting,
    onclose,
  }: {
    inspecting: NonNullable<LabelerCatalogSnapshot['inspecting']>;
    onclose: () => void;
  } = $props();
</script>

<div class="inspect-panel" data-testid={IDS.LABELER_INSPECT_PANEL}>
  <p data-testid={IDS.LABELER_INSPECT_METADATA}>
    labeler_id: {inspecting.labeler_id}
    version: {inspecting.version}
    artifact_kind: {inspecting.artifact_kind}
    wasm_hash: {inspecting.wasm_hash}
    wasm_size: {inspecting.wasm_size}
    needs_text: {inspecting.needs_text}
    needs_hashtags: {inspecting.needs_hashtags}
    needs_media_metadata: {inspecting.needs_media_metadata}
    needs_author: {inspecting.needs_author}
    needs_attachment_bytes: {inspecting.needs_attachment_bytes}
    label_abi: {inspecting.label_abi}
    verified: {inspecting.verified}{#if inspecting.newer_than_this_app}
    {t.labeler_catalog.inspect_needs_newer_app}{/if}
  </p>
  <!-- List-kind inspect section (content-moderation-and-ranking.md § Tier-3
       artifact kinds: "inspect returns the raw artifact, so the client renders
       the exact id→score map before subscribing" — the strongest
       inspect-before-subscribe of any kind). Hidden for a `wasm` labeler. The
       publisher-chosen name rides INSIDE the artifact (topic-factors.md §
       Publishing — zero new wire), so inspect is where it first becomes
       visible. -->
  {#if inspecting.artifact_kind === 'list'}
    <p data-testid={IDS.LABELER_INSPECT_LIST_NAME}>
      {inspecting.list_name
        ? t.labeler_catalog.list_name({ name: inspecting.list_name })
        : t.labeler_catalog.unnamed_list}
    </p>
    <p class="muted" data-testid={IDS.LABELER_INSPECT_LIST_ENTRY_COUNT}>
      {t.labeler_catalog.list_entry_count({ count: String(inspecting.list_entries.length) })}
    </p>
    <div class="labeler-list" data-testid={IDS.LABELER_INSPECT_LIST_ENTRIES}>
      {#each inspecting.list_entries as entry, i (entry.content_id + i)}
        <!-- ALL entries render, never a capped preview: a truncated map is not
             the exact map. The score is the publisher's per-mille verbatim —
             the same number the publish sheet showed, no rescale anywhere in
             the chain. -->
        <div class="labeler-item" data-testid={IDS.LABELER_INSPECT_LIST_ENTRY}>
          <span data-testid={IDS.LABELER_INSPECT_LIST_ENTRY_ID}>{entry.content_id}</span>
          <span class="muted" data-testid={IDS.LABELER_INSPECT_LIST_ENTRY_SCORE}>{entry.score}</span>
        </div>
      {/each}
    </div>
  {/if}
  <!-- text-model-kind inspect section, the list section's twin — keyed on the
       KIND rather than payload-emptiness (an empty vocabulary is a decode
       failure, not a reason to render the panel as if it were some other
       kind). -->
  {#if inspecting.artifact_kind === 'text-model'}
    <p data-testid={IDS.LABELER_INSPECT_MODEL_NAME}>
      {inspecting.model_name
        ? t.labeler_catalog.model_name({ name: inspecting.model_name })
        : t.labeler_catalog.unnamed_model}
    </p>
    <p class="muted" data-testid={IDS.LABELER_INSPECT_MODEL_NGRAM_COUNT}>
      {t.labeler_catalog.model_ngram_count({ count: String(inspecting.model_ngrams.length) })}
    </p>
    <div class="labeler-list" data-testid={IDS.LABELER_INSPECT_MODEL_ENTRIES}>
      {#each inspecting.model_ngrams as entry, i (entry.ngram + i)}
        <!-- EVERY entry — the vocabulary is the model's whole matching
             surface, never a capped preview. The SAME two shared faces the
             publish sheet's review rows painted, so what a publisher was shown
             before publishing is what a subscriber reads before subscribing. -->
        <div class="labeler-item" data-testid={IDS.LABELER_INSPECT_MODEL_ENTRY}>
          <span data-testid={IDS.LABELER_INSPECT_MODEL_ENTRY_TEXT}>{entry.ngram}</span>
          <span class="muted" data-testid={IDS.LABELER_INSPECT_MODEL_ENTRY_DIRECTION}>
            {resolveLocalized(ngramDirectionLabel(entry.more, entry.less))}
          </span>
          <span class="muted" data-testid={IDS.LABELER_INSPECT_MODEL_ENTRY_COUNT}>
            {resolveLocalized(ngramDocCountLabel(entry.more, entry.less))}
          </span>
        </div>
      {/each}
    </div>
  {/if}
  <button class="btn btn-small" data-testid={IDS.LABELER_INSPECT_CLOSE_BUTTON} onclick={onclose}>
    {t.labeler_catalog.close_inspect}
  </button>
</div>

<style>
  .muted { color: var(--text-muted); }
  .inspect-panel {
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 1rem;
    margin-bottom: 1rem;
    background: var(--bg-surface);
  }
  .inspect-panel p { white-space: pre-line; font-size: 0.8125rem; }
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
</style>
