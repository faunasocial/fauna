<script lang="ts">
  // The shared `feature-policy-editor` family's paint — ONE component for all
  // three hosts (`dynamic-features.md` § Authoring surfaces): the admin Nest page
  // opens it at the admin tier, a Settings feature-limits row at the self tier,
  // the Family page's feature-limits section at the guardian tier.
  // The twin of tui's and linux's `feature_editor.rs`.
  //
  // **This component paints; it decides nothing.** The draft, the cell list and
  // its order, the labels, the no-effect notes, whether *Remove limit* renders
  // and the parser behind *Save* are all `WasmPolicyEditor`'s (shared Rust), so
  // the editor cannot differ between hosts or across the 7 apps. The hosts
  // differ only in the tier they open it at, which decides the write kind the
  // offline gate reads.
  import { connectionStatus } from '#lib/store';
  import { featureEditorWrite } from '#lib/rpc';
  import {
    offlineAffordance,
    type FeatureAuthoringTier,
    type PolicyEditorHandle,
  } from '#lib/wasm';
  import { makeOfflineGate } from '#lib/offline-gate';
  import { resolveLocalized, resolveLocalizedNested, editorNoteText } from '#lib/i18n/localized';
  import { t } from '#lib/i18n/strings';
  import { IDS } from '#lib/generated/uiIds';

  let {
    secretHex,
    editor,
    tier,
    onwritten,
    oncancel,
    onerror,
  }: {
    secretHex: string;
    editor: PolicyEditorHandle;
    tier: FeatureAuthoringTier;
    /** A write landed (the editor has already re-seeded): the host re-reads. */
    onwritten: () => void;
    oncancel: () => void;
    /** The page's error banner: a nest refusal or an unparseable cell. */
    onerror: (message: string) => void;
  } = $props();

  // Save and Remove are `OnlineOnly` (the offline gate greys them from the
  // write kind declared here); Cancel is local and stays live. The kind is the
  // tier's own — `AuthoringTier::write_kind` — and `check-offline-gate-kinds.py`
  // grades both literals against the shared table.
  const offlineGate = makeOfflineGate(offlineAffordance, connectionStatus.subscribe);

  let view = $state(editor.view());
  let status = $state('');
  let working = $state(false);

  function chooseOn(on: boolean) {
    editor.setOn(on);
    view = editor.view();
  }

  function typeCell(index: number, text: string) {
    editor.setCell(index, text);
    view = editor.view();
  }

  async function write(remove: boolean) {
    working = true;
    onerror('');
    try {
      const verdict = await featureEditorWrite(secretHex, editor, remove);
      if (verdict.invalid) {
        // Nothing was sent: the shared parser refused a cell.
        onerror(resolveLocalizedNested(verdict.invalid));
        return;
      }
      view = editor.view();
      status = verdict.status ? resolveLocalized(verdict.status) : '';
      onwritten();
    } catch (e) {
      onerror(t.features.editor_save_failed({ error: e instanceof Error ? e.message : String(e) }));
    } finally {
      working = false;
    }
  }
</script>

<div class="policy-editor" data-testid={IDS.FEATURE_POLICY_EDITOR}>
  <h3 data-testid={IDS.FEATURE_POLICY_EDITOR_TITLE}>{resolveLocalizedNested(view.title)}</h3>

  <!-- A one-of-two radio pair — exactly two choices, never a third "Limit":
       whether On carries bounds is the cells' business. `data-state` is the
       uniform marked/unmarked read the other radio pairs expose. -->
  <div class="radios">
    <label>
      <input
        type="radio"
        name="feature-policy-editor-choice"
        checked={view.on}
        data-testid={IDS.FEATURE_POLICY_EDITOR_ON_RADIO}
        data-state={view.on ? 'on' : 'off'}
        onchange={() => chooseOn(true)}
      />
      {t.features.editor_on}
    </label>
    <label>
      <input
        type="radio"
        name="feature-policy-editor-choice"
        checked={!view.on}
        data-testid={IDS.FEATURE_POLICY_EDITOR_OFF_RADIO}
        data-state={view.on ? 'off' : 'on'}
        onchange={() => chooseOn(false)}
      />
      {t.features.editor_off}
    </label>
  </div>

  <!-- An outer tier already denies: whatever is chosen here changes nothing,
       and the editor says so rather than refusing anything. -->
  {#if view.off_note}
    <p class="muted">{resolveLocalized(view.off_note)}</p>
  {/if}
  {#if view.cells.length > 0}
    <p class="muted">{resolveLocalized(view.hint)}</p>
  {/if}

  {#each view.cells as cell, i (i)}
    {@const note = editorNoteText(cell)}
    <div class="field-group" data-testid={IDS.FEATURE_POLICY_EDITOR_CELL}>
      <label class="field-label" data-testid={IDS.FEATURE_POLICY_EDITOR_CELL_LABEL} for="feature-policy-editor-cell-{i}">
        {resolveLocalizedNested(cell.label)}
      </label>
      <input
        id="feature-policy-editor-cell-{i}"
        type="text"
        value={cell.text}
        data-testid={IDS.FEATURE_POLICY_EDITOR_CELL_INPUT}
        oninput={(e) => typeCell(i, e.currentTarget.value)}
      />
      {#if note}
        <p class="muted" data-testid={IDS.FEATURE_POLICY_EDITOR_CELL_NOTE}>{note}</p>
      {/if}
    </div>
  {/each}

  <div class="actions">
    <!-- The gate takes the kind as a literal at the call site (so the checker can
         grade it), hence one declaration per tier. -->
    {#if tier === 'admin'}
      <button
        type="button"
        class="btn"
        data-testid={IDS.FEATURE_POLICY_EDITOR_SAVE_BUTTON}
        use:offlineGate={{ kind: 'fauna.features.policy.update', disabled: working }}
        onclick={() => write(false)}
      >
        {t.features.editor_save}
      </button>
      {#if view.can_remove}
        <button
          type="button"
          class="btn ghost"
          data-testid={IDS.FEATURE_POLICY_EDITOR_REMOVE_BUTTON}
          use:offlineGate={{ kind: 'fauna.features.policy.update', disabled: working }}
          onclick={() => write(true)}
        >
          {t.features.editor_remove}
        </button>
      {/if}
    {:else if tier === 'guardian'}
      <button
        type="button"
        class="btn"
        data-testid={IDS.FEATURE_POLICY_EDITOR_SAVE_BUTTON}
        use:offlineGate={{ kind: 'fauna.family.policy.update', disabled: working }}
        onclick={() => write(false)}
      >
        {t.features.editor_save}
      </button>
      {#if view.can_remove}
        <button
          type="button"
          class="btn ghost"
          data-testid={IDS.FEATURE_POLICY_EDITOR_REMOVE_BUTTON}
          use:offlineGate={{ kind: 'fauna.family.policy.update', disabled: working }}
          onclick={() => write(true)}
        >
          {t.features.editor_remove}
        </button>
      {/if}
    {:else}
      <button
        type="button"
        class="btn"
        data-testid={IDS.FEATURE_POLICY_EDITOR_SAVE_BUTTON}
        use:offlineGate={{ kind: 'fauna.features.self_limits.update', disabled: working }}
        onclick={() => write(false)}
      >
        {t.features.editor_save}
      </button>
      {#if view.can_remove}
        <button
          type="button"
          class="btn ghost"
          data-testid={IDS.FEATURE_POLICY_EDITOR_REMOVE_BUTTON}
          use:offlineGate={{ kind: 'fauna.features.self_limits.update', disabled: working }}
          onclick={() => write(true)}
        >
          {t.features.editor_remove}
        </button>
      {/if}
    {/if}
    <button
      type="button"
      class="btn ghost"
      data-testid={IDS.FEATURE_POLICY_EDITOR_CANCEL_BUTTON}
      onclick={oncancel}
    >
      {t.features.editor_cancel}
    </button>
  </div>

  {#if status}
    <p data-testid={IDS.FEATURE_POLICY_EDITOR_STATUS}>{status}</p>
  {/if}
</div>

<style>
  .policy-editor {
    margin-top: 0.5rem;
    padding: 0.75rem;
    border: 1px solid var(--border, rgba(128, 128, 128, 0.3));
    border-radius: 8px;
  }
  .radios {
    display: flex;
    gap: 1rem;
    margin: 0.5rem 0;
  }
  .actions {
    display: flex;
    gap: 0.5rem;
    margin-top: 0.75rem;
  }
</style>
