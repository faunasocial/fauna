<script lang="ts">
  // User-settings "Spam" section: a person manages **their own** per-account
  // spam classifier — reset the training model (destructive, two-click confirm),
  // opt in/out of the deployment baseline, and undo individual training events
  // from the history. A dumb renderer of the shared `MailSpamMachine`
  // (`libs/fauna-client-mail-settings`, WASM twin `WasmMailSpamMachine`): build
  // over the singleton WS-RPC client → hydrate() → render snapshot() →
  // dispatch(action) → re-render. No spam logic in the SPA (priority #2). Lifts
  // the linux lead shape (apps/fauna-linux/src/settings/mail_spam.rs). Reuses the
  // settings page's single `error-message` via the bindable `error`. Behavior +
  // IDs: docs/goal/behavior/mail-spam.md § (UI ratified ahead of the backend),
  // ui.yaml § mail-spam / mail-spam-training-history-list.
  //
  // The per-user spam RPCs (`fauna.bridges.{reset_spam_model,
  // list_spam_training_history,put_spam_model}` + the baseline-contribution
  // setter) are driven by the shared machine; a row's undo runs client-side
  // (its delta is sealed to the actor) and rides `put_spam_model`. A refusal
  // renders via `error`, never faked green.
  import { identity } from '$lib/store';
  import { mailSpamMachine, spamThresholdOverrideGet, spamThresholdOverrideSet } from '$lib/rpc';
  import { ensureWasm, trainingLabelBadge, trainingSourceBadge, parseCount } from '$lib/wasm';
  import { resolveLocalized } from '$lib/i18n/localized';
  import { onMount } from 'svelte';
  import { t } from '$lib/i18n/strings';
  import type { WasmMailSpamMachine } from '../../../static/fauna_wasm.js';
  import { IDS } from '$lib/generated/uiIds';

  // The parent settings page owns the single error surface; spam dispatch errors
  // flow into it (the same surface the other mail-settings sections bind to).
  let { error = $bindable('') } = $props();

  // Shapes mirror the shared `MailSpamSnapshot` / `SpamTrainingView` serde JSON
  // (snake_case across the serde_wasm_bindgen boundary; the label/source enums
  // serialize as their PascalCase variant-name string).
  interface SpamTrainingView {
    history_id_hex: string;
    message: string;
    label: string; // "Spam" | "Ham" | "Unknown" (a label a newer nest wrote: neutral badge, undo refused)
    source: string; // "ExplicitButton" | "ImapJunkFlag" | "ImapJunkMove"
    created_at_ms: number;
    // The row's stored `model_delta_applied` verbatim (leg 1c) — not rendered;
    // the shared machine routes a sealed row's undo client-side on it.
    model_delta_applied?: number[];
  }
  interface SpamSnapshot {
    events: SpamTrainingView[];
    contribute_baseline: boolean;
    status: string; // "Idle" | "Loading" | "Working"
    error: string | null;
  }

  let machine: WasmMailSpamMachine | null = null;
  let snap = $state<SpamSnapshot | null>(null);

  // Per-action two-click arm state for the destructive model-reset gesture (no
  // modal, no separate confirm id — matches the aliases revoke/delete idiom;
  // auto-disarms after 4s).
  let armedReset = $state(false);

  const busy = $derived(snap?.status === 'Working');

  // Per-account spam-threshold override (mail-policy-config.md § Tier 3) — a plain fauna.bridges.{get,set}_spam_threshold_override
  // RPC pair over the WsRpcClient directly, NOT the MailSpamMachine (mirrors
  // linux's separate hydrate_threshold_override/commit_threshold_override).
  // Empty follows the admin default; "0" is a real setting, never collapsed
  // into empty.
  let thresholdInput = $state('');

  function applySnapshot(): void {
    if (!machine) return;
    snap = machine.snapshot() as unknown as SpamSnapshot;
    if (snap?.error) error = snap.error;
  }

  onMount(async () => {
    const id = $identity;
    if (!id?.secretHex) return;
    try {
      await ensureWasm();
      machine = await mailSpamMachine(id.secretHex);
      await machine.hydrate();
      applySnapshot();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
    try {
      const value = await spamThresholdOverrideGet(id.secretHex);
      thresholdInput = value != null ? String(value) : '';
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  });

  /** Commit the threshold input on Enter — no separate save button (tui's
   *  `Element::input_commit` shape, mirrored by linux/android). Parses
   *  through the shared `parseCount` (the alias add-sheet's own
   *  `spam_threshold_override` field uses the same validator), then
   *  re-reads so the field reflects the persisted value, never the local
   *  keystroke. */
  async function commitThresholdOverride(): Promise<void> {
    const id = $identity;
    if (!id?.secretHex) return;
    try {
      const value = parseCount(thresholdInput) ?? null;
      const confirmed = await spamThresholdOverrideSet(id.secretHex, value);
      thresholdInput = confirmed != null ? String(confirmed) : '';
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  // The label/source badges come from the shared Rust maps
  // (`fauna_client_mail_settings::{training_label_badge,training_source_badge}`,
  // WASM twins `trainingLabelBadge`/`trainingSourceBadge`) — the same source the
  // native apps consume, single-sourced (priority #2). `label`/`source` are the
  // serde `TrainingLabel`/`TrainingSource` variant-name strings the snapshot carries.
  function labelBadge(label: string): string {
    return resolveLocalized(trainingLabelBadge(label));
  }

  function sourceBadge(source: string): string {
    return resolveLocalized(trainingSourceBadge(source));
  }

  // Dispatch a fire-and-render action, then re-render from the resulting snapshot.
  async function dispatch(action: unknown): Promise<void> {
    if (!machine) return;
    try {
      await machine.dispatch(action);
      applySnapshot();
    } catch (e) {
      applySnapshot();
      if (!error) error = e instanceof Error ? e.message : String(e);
    }
  }

  // Two-click confirm: first click arms (auto-disarms after 4s), second fires the
  // destructive reset (deletes the model + history; "cannot be undone").
  function onReset(): void {
    if (armedReset) {
      armedReset = false;
      dispatch('ResetModel');
    } else {
      armedReset = true;
      setTimeout(() => { armedReset = false; }, 4000);
    }
  }

  function onContributeToggle(): void {
    dispatch({ SetContributeBaseline: { contribute: !(snap?.contribute_baseline ?? false) } });
  }

  function onUndo(v: SpamTrainingView): void {
    dispatch({ UndoTraining: { history_id_hex: v.history_id_hex } });
  }
</script>

<section class="section">
  <h2>{t.mail_spam.title}</h2>
  <p class="muted small">{t.mail_spam.description}</p>

  <!-- Reset model (destructive, two-click inline confirm — no modal). -->
  <div class="row">
    <div class="row-text">
      <span class="row-label">{t.mail_spam.reset_button}</span>
      <span class="muted small">{t.mail_spam.reset_subtitle}</span>
    </div>
    <button
      class="btn"
      class:danger={armedReset}
      data-testid={IDS.MAIL_SPAM_RESET_MODEL_BUTTON}
      disabled={busy}
      onclick={onReset}
    >{armedReset ? t.mail_spam.reset_confirm : t.mail_spam.reset_button}</button>
  </div>

  <!-- Contribute-to-baseline toggle (default off). -->
  <div class="row">
    <div class="row-text">
      <span class="row-label">{t.mail_spam.contribute_baseline_label}</span>
      <span class="muted small">{t.mail_spam.contribute_baseline_subtitle}</span>
    </div>
    <label class="toggle-cell">
      <input
        type="checkbox"
        data-testid={IDS.MAIL_SPAM_CONTRIBUTE_BASELINE_TOGGLE}
        checked={snap?.contribute_baseline ?? false}
        disabled={busy}
        onchange={onContributeToggle}
      />
    </label>
  </div>

  <!-- Per-account spam-folder threshold override (mail-policy-config.md
       § Tier 3). Commits on Enter — no separate save
       button. -->
  <div class="row">
    <div class="row-text">
      <span class="row-label">{t.mail_spam.threshold_override_label}</span>
      <span class="muted small">{t.mail_spam.threshold_override_subtitle}</span>
    </div>
    <input
      type="text"
      class="threshold-input"
      data-testid={IDS.MAIL_SPAM_THRESHOLD_OVERRIDE_INPUT}
      bind:value={thresholdInput}
      onkeydown={(e) => { if (e.key === 'Enter') commitThresholdOverride(); }}
    />
  </div>

  <!-- Training-history list (mail-spam-training-history-list component). -->
  <h3 class="muted small">{t.mail_spam.history_title}</h3>
  <div class="history" data-testid={IDS.MAIL_SPAM_TRAINING_HISTORY_LIST}>
    {#if (snap?.events.length ?? 0) === 0}
      <p class="muted small">{t.mail_spam.empty}</p>
    {/if}
    {#each snap?.events ?? [] as v (v.history_id_hex)}
      <div class="history-item" data-testid={IDS.MAIL_SPAM_TRAINING_HISTORY_LIST_ITEM}>
        <span class="msg" data-testid={IDS.MAIL_SPAM_TRAINING_HISTORY_LIST_ITEM_MESSAGE}>{v.message}</span>
        <span class="badge" data-testid={IDS.MAIL_SPAM_TRAINING_HISTORY_LIST_ITEM_LABEL}>{labelBadge(v.label)}</span>
        <span class="badge" data-testid={IDS.MAIL_SPAM_TRAINING_HISTORY_LIST_ITEM_SOURCE}>{sourceBadge(v.source)}</span>
        <span class="muted small" data-testid={IDS.MAIL_SPAM_TRAINING_HISTORY_LIST_ITEM_CREATED_AT}>
          {new Date(v.created_at_ms).toLocaleString()}
        </span>
        <button
          class="btn small"
          data-testid={IDS.MAIL_SPAM_TRAINING_HISTORY_LIST_ITEM_UNDO_BUTTON}
          disabled={busy}
          onclick={() => onUndo(v)}
        >{t.mail_spam.undo}</button>
      </div>
    {/each}
  </div>
</section>

<style>
  .section { margin-bottom: 2rem; }
  .section h2 { margin-bottom: 0.5rem; font-size: 1.125rem; }
  .section h3 { margin: 0.75rem 0 0.25rem; font-size: 0.9rem; }
  .muted { color: var(--text-muted); }
  .small { font-size: 0.8rem; }
  .row {
    display: flex; align-items: center; justify-content: space-between; gap: 0.75rem;
    padding: 0.5rem 0; border-bottom: 1px solid var(--border);
  }
  .row-text { display: flex; flex-direction: column; gap: 0.125rem; max-width: 32rem; }
  .row-label { font-size: 0.9rem; }
  .toggle-cell { display: flex; align-items: center; }
  .threshold-input {
    width: 5rem; padding: 0.375rem 0.5rem; border: 1px solid var(--border);
    border-radius: 6px; background: var(--bg); color: var(--text);
    font-size: 0.875rem;
  }
  .history { display: flex; flex-direction: column; gap: 0.5rem; margin-top: 0.5rem; }
  .history-item {
    display: flex; align-items: center; gap: 0.5rem; padding: 0.5rem;
    border: 1px solid var(--border); border-radius: 6px; flex-wrap: wrap;
  }
  .msg { flex: 1; font-size: 0.85rem; word-break: break-word; }
  .badge {
    font-size: 0.75rem; padding: 0.125rem 0.5rem; border-radius: 4px;
    background: var(--bg-hover); color: var(--text-muted);
  }
  .btn {
    padding: 0.5rem 1rem; border: 1px solid var(--border); border-radius: 6px;
    background: var(--bg-surface); color: var(--text); cursor: pointer;
    font-size: 0.875rem;
  }
  .btn:hover { background: var(--bg-hover); }
  .btn:disabled { opacity: 0.5; cursor: not-allowed; }
  .btn.small { padding: 0.25rem 0.625rem; font-size: 0.8rem; }
  .btn.danger { border-color: var(--danger); color: var(--danger); }
</style>
