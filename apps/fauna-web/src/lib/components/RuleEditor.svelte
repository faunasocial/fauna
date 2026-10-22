<script lang="ts">
  // The shared `rule-editor` component's paint — ONE component for every
  // mounting (`nest-rules.md` § The shared rule editor → *Three mountings, one
  // shape*): the admin Nest page hosts it in `admin-nest-rules-section`, the
  // Family page in `family-policy-rules-section` for the selected ward. The twin
  // of tui `rule_editor.rs`.
  //
  // **This component paints; it decides nothing.** The rule list, the kinds, the
  // categories, the verdicts, the level scale, every label and defect sentence
  // and whether the document may be saved are `WasmRuleEditor`'s (shared Rust,
  // `fauna_client_policy`), so the editor cannot differ between hosts or across
  // the 7 apps. A host owns the editor handle, the save (where the write kind the
  // offline gate reads is decided) and the status line; this component forwards
  // the edits and calls `onsave`.
  import { connectionStatus } from '#lib/store';
  import { ruleLabelers, ruleModule } from '#lib/rpc';
  import {
    offlineAffordance,
    ruleKeywordsInline,
    type LabelerChoice,
    type RuleEditorHandle,
    type RuleKind,
    type RuleVerdict,
  } from '#lib/wasm';
  import { makeOfflineGate } from '#lib/offline-gate';
  import { resolveLocalized } from '#lib/i18n/localized';
  import { t } from '#lib/i18n/strings';
  import { IDS } from '#lib/generated/uiIds';

  let {
    secretHex,
    editor,
    writeKind,
    status = '',
    saving = false,
    onsave,
    onchange,
  }: {
    secretHex: string;
    editor: RuleEditorHandle;
    /** The write the host's save sends — the offline gate greys Save from it
     *  (`fauna.admin.content_policy.set` | `fauna.family.policy.update`). */
    writeKind: string;
    /** `rule-editor-status`'s text — the host's last verdict; empty shows the
     *  editor's own defects. */
    status?: string;
    saving?: boolean;
    onsave: () => void;
    /** Any edit: the host clears its stale "saved" status. */
    onchange?: () => void;
  } = $props();

  const offlineGate = makeOfflineGate(offlineAffordance, connectionStatus.subscribe);

  let view = $state(editor.view());
  // The keywords inputs as typed. A single-line input re-painted from the stored
  // terms would eat a trailing comma mid-typing, so the typed text is kept per
  // row while the editor holds the parsed terms.
  let typedKeywords = $state<Record<number, string>>({});
  // The publisher's subscribed labelers a rule may name, read beside the document.
  let subscribed = $state<string[]>([]);
  let labelerChoices = $derived.by<LabelerChoice[]>(() => {
    void view;
    return editor.labelerChoices(subscribed);
  });

  $effect(() => {
    let cancelled = false;
    ruleLabelers(secretHex)
      .then((ids) => {
        if (!cancelled) subscribed = ids;
      })
      .catch((e) => console.warn('rule editor: subscribed labelers unavailable:', e));
    return () => {
      cancelled = true;
    };
  });

  function refresh() {
    view = editor.view();
    onchange?.();
  }

  function label(lt: Parameters<typeof resolveLocalized>[0]): string {
    return resolveLocalized(lt);
  }

  function add() {
    editor.addRule('category');
    refresh();
  }

  function remove(row: number) {
    if (editor.removeRule(row)) {
      // Rows after `row` shift up; their typed text is re-read from the stored terms.
      typedKeywords = {};
      refresh();
    }
  }

  // Every picker's `<option value>` IS the label the crate painted (the cross-app
  // select contract), so the value maps back through the view's own lists.
  function pickKind(row: number, value: string) {
    const kind = view.kinds.find((k) => label(k.label) === value)?.kind;
    if (kind && editor.setKind(row, kind as RuleKind)) {
      typedKeywords = {};
      refresh();
    }
  }
  function pickCategory(row: number, value: string) {
    const category = view.categories.find((c) => label(c.label) === value)?.category;
    if (category !== undefined && editor.setCategory(row, category)) refresh();
  }
  function pickVerdict(row: number, value: string) {
    const verdict = view.verdicts.find((v) => label(v.label) === value)?.verdict;
    if (verdict && editor.setVerdict(row, verdict as RuleVerdict)) refresh();
  }
  function pickLevel(row: number, value: string) {
    const level = view.levels.findIndex((l) => label(l.label) === value);
    if (level >= 0 && editor.setLevel(row, level)) refresh();
  }
  async function pickLabeler(row: number, name: string) {
    const choice = labelerChoices.find((c) => c.name === name);
    if (!choice) return;
    if (choice.labeler_id) {
      // A subscribed labeler the document does not bundle yet: fetch its module
      // and hand it to the editor under the choice's name.
      try {
        const module = await ruleModule(secretHex, choice.labeler_id);
        if (!module || !editor.setLabeler(row, choice.name, module)) return;
      } catch (e) {
        console.warn('rule editor: labeler module unavailable:', e);
        return;
      }
    } else if (!editor.chooseBundledLabeler(row, choice.name)) {
      return;
    }
    refresh();
  }
  function typeKeywords(row: number, text: string) {
    typedKeywords[row] = text;
    editor.setKeywordsInline(row, text);
    refresh();
  }
  function typePrompt(row: number, text: string) {
    editor.setPrompt(row, text);
    refresh();
  }
  function typeReason(row: number, text: string) {
    editor.setReason(row, view.reason_default_key, text);
    refresh();
  }

  function reasonOf(reasons: { lang: string; text: string }[]): string {
    return reasons.find((r) => r.lang === view.reason_default_key)?.text ?? '';
  }

  let statusText = $derived(status || view.defects.map((d) => label(d)).join(' '));
</script>

<div class="rule-editor">
  {#if !view.editable}
    <p class="muted small">{t.rule_editor.newer_version}</p>
  {/if}

  {#each view.rows as row, i (i)}
    <div class="field-group" data-testid={IDS.RULE_EDITOR_ITEM} data-index={i}>
      <p class="muted small">{t.rule_editor.item_label({ rule: String(row.number) })}</p>

      {#if row.read_only}
        <p>{label(row.kind_label)}</p>
      {:else}
        <label class="field">
          <span>{t.rule_editor.kind_label}</span>
          <select
            data-testid={IDS.RULE_EDITOR_ITEM_KIND_SELECT}
            data-index={i}
            disabled={!view.editable}
            value={label(row.kind_label)}
            onchange={(e) => pickKind(i, e.currentTarget.value)}
          >
            {#each view.kinds as k (k.kind)}
              <option value={label(k.label)}>{label(k.label)}</option>
            {/each}
          </select>
        </label>

        {#if row.category !== null}
          <label class="field">
            <span>{t.rule_editor.category_label}</span>
            <select
              data-testid={IDS.RULE_EDITOR_ITEM_CATEGORY_SELECT}
              data-index={i}
              disabled={!view.editable}
              value={label(view.categories.find((c) => c.category === row.category)?.label ?? null)}
              onchange={(e) => pickCategory(i, e.currentTarget.value)}
            >
              {#each view.categories as c (c.category)}
                <option value={label(c.label)}>{label(c.label)}</option>
              {/each}
            </select>
          </label>
        {:else if row.keywords !== null}
          <label class="field">
            <span>{t.rule_editor.keywords_label}</span>
            <input
              type="text"
              data-testid={IDS.RULE_EDITOR_ITEM_KEYWORDS_INPUT}
              data-index={i}
              disabled={!view.editable}
              value={typedKeywords[i] ?? ruleKeywordsInline(row.keywords)}
              oninput={(e) => typeKeywords(i, e.currentTarget.value)}
            />
          </label>
        {:else if row.labeler !== null}
          <label class="field">
            <span>{t.rule_editor.labeler_label}</span>
            <select
              data-testid={IDS.RULE_EDITOR_ITEM_LABELER_SELECT}
              data-index={i}
              disabled={!view.editable}
              value={row.labeler}
              onchange={(e) => void pickLabeler(i, e.currentTarget.value)}
            >
              {#if row.labeler === ''}
                <option value=""></option>
              {/if}
              {#each labelerChoices as c (c.name)}
                <option value={c.name}>{c.name}</option>
              {/each}
            </select>
          </label>
        {:else if row.prompt !== null}
          <label class="field">
            <span>{t.rule_editor.prompt_label}</span>
            <input
              type="text"
              data-testid={IDS.RULE_EDITOR_ITEM_PROMPT_INPUT}
              data-index={i}
              disabled={!view.editable}
              value={row.prompt}
              oninput={(e) => typePrompt(i, e.currentTarget.value)}
            />
          </label>
        {/if}

        <label class="field">
          <span>{t.rule_editor.verdict_label}</span>
          <select
            data-testid={IDS.RULE_EDITOR_ITEM_VERDICT_SELECT}
            data-index={i}
            disabled={!view.editable}
            value={label(view.verdicts.find((v) => v.verdict === row.verdict)?.label ?? null)}
            onchange={(e) => pickVerdict(i, e.currentTarget.value)}
          >
            {#each view.verdicts as v (v.verdict)}
              <option value={label(v.label)}>{label(v.label)}</option>
            {/each}
          </select>
        </label>

        {#if row.level_index !== null}
          <label class="field">
            <span>{t.rule_editor.level_label}</span>
            <select
              data-testid={IDS.RULE_EDITOR_ITEM_LEVEL_SELECT}
              data-index={i}
              disabled={!view.editable}
              value={label(view.levels[row.level_index]?.label ?? null)}
              onchange={(e) => pickLevel(i, e.currentTarget.value)}
            >
              {#each view.levels as l (l.permille)}
                <option value={label(l.label)}>{label(l.label)}</option>
              {/each}
            </select>
          </label>
        {/if}

        <label class="field">
          <span>{t.rule_editor.reason_label}</span>
          <input
            type="text"
            data-testid={IDS.RULE_EDITOR_ITEM_REASON_INPUT}
            data-index={i}
            disabled={!view.editable}
            value={reasonOf(row.reasons)}
            oninput={(e) => typeReason(i, e.currentTarget.value)}
          />
        </label>
      {/if}

      {#if row.inert}
        <p class="muted small">{label(row.inert)}</p>
      {/if}

      <button
        type="button"
        class="btn"
        data-testid={IDS.RULE_EDITOR_ITEM_REMOVE_BUTTON}
        data-index={i}
        onclick={() => remove(i)}
      >{t.rule_editor.remove_button}</button>
    </div>
  {:else}
    <p class="muted">{t.rule_editor.no_rules}</p>
  {/each}

  <div class="actions">
    <button
      type="button"
      class="btn"
      data-testid={IDS.RULE_EDITOR_ADD_BUTTON}
      disabled={!view.editable}
      onclick={add}
    >{t.rule_editor.add_button}</button>
    <!-- The gate's kind is spelled out per host: `check-offline-gate-kinds.py`
         grades each literal against the shared table. -->
    {#if writeKind === 'fauna.admin.content_policy.set'}
      <button
        type="button"
        class="btn primary"
        data-testid={IDS.RULE_EDITOR_SAVE_BUTTON}
        use:offlineGate={{ kind: 'fauna.admin.content_policy.set', disabled: saving || !view.can_save }}
        onclick={onsave}
      >{saving ? t.rule_editor.saving : t.rule_editor.save_button}</button>
    {:else}
      <button
        type="button"
        class="btn primary"
        data-testid={IDS.RULE_EDITOR_SAVE_BUTTON}
        use:offlineGate={{ kind: 'fauna.family.policy.update', disabled: saving || !view.can_save }}
        onclick={onsave}
      >{saving ? t.rule_editor.saving : t.rule_editor.save_button}</button>
    {/if}
  </div>
  <p class="muted small" data-testid={IDS.RULE_EDITOR_STATUS}>{statusText}</p>
</div>

<style>
  .actions {
    display: flex;
    gap: 0.5rem;
    align-items: center;
  }
</style>
