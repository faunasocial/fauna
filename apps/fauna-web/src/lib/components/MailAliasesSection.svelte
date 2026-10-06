<script lang="ts">
  // User-settings "Aliases" section: a person manages **their own** per-account
  // mail addresses — the canonical `<handle>@<domain>` exact alias, extra exact
  // aliases, wildcard prefixes, and one-click disposable mints — each with an
  // optional label, a per-alias spam-threshold / rate-limit override, and disable
  // / revoke / delete controls. A dumb renderer of the shared `MailAliasesMachine`
  // (`libs/fauna-client-mail-settings`, WASM twin `WasmMailAliasesMachine`): build
  // over the singleton WS-RPC client → hydrate() → render snapshot() →
  // dispatch(action) → re-render. No alias logic in the SPA (priority #2). Lifts
  // the linux lead shape (apps/fauna-linux/src/settings/mail_aliases.rs). Reuses
  // the settings page's single `error-message` (no duplicate IDs) via the bindable
  // `error`. Behavior + IDs: docs/goal/behavior/mail-aliases.md § Aliases UX,
  // ui.yaml § mail-aliases / mail-aliases-list.
  //
  // Two honest gaps surfaced (never faked), mirroring linux: the per-row
  // disabled-toggle is one-way (no un-revoke RPC — revoke/update can't clear
  // `disabled`), and `mail-aliases-list-item-show-audit` is inert (the per-alias
  // hit list `list_account_alias_hits` is a follow-on slice the machine doesn't
  // expose yet).
  import { identity } from '$lib/store';
  import { mailAliasesMachine } from '$lib/rpc';
  import { ensureWasm, aliasKindBadge, aliasHitsLabel, parseCount, parseCountI64 } from '$lib/wasm';
  import { resolveLocalized } from '$lib/i18n/localized';
  import { onMount } from 'svelte';
  import { t } from '$lib/i18n/strings';
  import type { WasmMailAliasesMachine } from '../../../static/fauna_wasm.js';
  import { IDS } from '$lib/generated/uiIds';

  // The parent settings page owns the single error surface; alias dispatch errors
  // flow into it (the same surface MailSettingsSection binds to).
  let { error = $bindable('') } = $props();

  // Shapes mirror the shared `MailAliasesSnapshot` / `AliasView` serde JSON
  // (snake_case across the serde_wasm_bindgen boundary; the kind enum serializes
  // as its variant name string).
  interface AliasView {
    alias_id_hex: string;
    local_domain: string;
    kind: string; // "Exact" | "Subaddress" | "Wildcard" | "Disposable" | "Catchall" | "Forwarder" | "Other"
    pattern: string;
    address: string;
    label: string;
    disabled: boolean;
    // The `<handle>@<domain>` primary mailbox + AUTH-login identity (wire
    // `AliasRow.is_canonical`, computed from the runtime mail domain). The nest
    // rejects disabling / renaming / deleting it; the row renders read-only
    // (mail-aliases.md § Disable / § Aliases UX).
    is_canonical: boolean;
    hit_count: number;
    last_hit_at_ms: number | null;
    spam_threshold_override: number | null;
    rate_limit_per_hour: number | null;
    rate_limit_per_day: number | null;
    uses_remaining: number | null;
    expires_at_ms: number | null;
  }
  // One imported line's outcome (mail-aliases.md § Bulk import).
  interface ImportAliasOutcomeView {
    line_index: number;
    address: string;
    status: string; // "Created" | "SkippedDuplicate" | "Invalid"
    reason: string | null;
  }
  interface ImportResultView {
    created: number;
    skipped_duplicate: number;
    invalid: number;
    outcomes: ImportAliasOutcomeView[];
  }
  interface AliasesSnapshot {
    aliases: AliasView[];
    default_domain: string | null;
    last_minted_address: string | null;
    last_import_result: ImportResultView | null;
    status: string; // "Idle" | "Loading" | "Working"
    error: string | null;
  }

  let machine: WasmMailAliasesMachine | null = null;
  let snap = $state<AliasesSnapshot | null>(null);

  // Add/edit inline sheet (reveal, not modal — matches linux + mail-settings).
  let sheetOpen = $state(false);
  let editingId = $state<string | null>(null); // non-null ⇒ Edit mode (kind read-only)
  let isWildcard = $state(false); // kind-picker: false = Exact (default), true = Wildcard
  let patternInput = $state('');
  let labelInput = $state('');
  let spamThresholdInput = $state('');
  let ratePerHourInput = $state('');
  // ttl/uses are disposable-only and minted via the dedicated generate button,
  // not this sheet — present (hidden) for ui.yaml conformance, same as linux and
  // the mail-add-credential conditional-field pattern.
  let ttlInput = $state('');
  let usesInput = $state('');

  // Bulk paste-import inline sheet (reveal, not modal — mirrors the add sheet
  // and the linux `mail-aliases-import-sheet` shape).
  let importOpen = $state(false);
  let importText = $state('');
  // Locally hides a stale `last_import_result` on reopen (mirrors linux's
  // `open_import_sheet` widget-clear) so a leftover success banner from a
  // prior import can't be mistaken for the outcome of a fresh paste; any
  // dispatch also clears the underlying snapshot field server-side.
  let importResultDismissed = $state(false);

  // Transient "copied <address>" confirmation after a disposable mint.
  let mintedToast = $state('');

  // Per-row two-click arm state for the destructive revoke/delete gestures (no
  // modal, no separate confirm id — matches linux; auto-disarms after 4s).
  let armedRevoke = $state<string | null>(null);
  let armedDelete = $state<string | null>(null);

  const hasDomain = $derived(snap?.default_domain != null);

  function applySnapshot(): void {
    if (!machine) return;
    snap = machine.snapshot() as unknown as AliasesSnapshot;
    if (snap?.error) error = snap.error;
    // Disposable-mint success: copy the full address + show a transient toast
    // (the machine clears last_minted_address at the start of the next dispatch).
    if (snap?.last_minted_address) {
      copyText(snap.last_minted_address);
      mintedToast = `${t.mail_aliases.copied} ${snap.last_minted_address}`;
      const addr = snap.last_minted_address;
      setTimeout(() => { if (mintedToast.endsWith(addr)) mintedToast = ''; }, 4000);
    }
  }

  onMount(async () => {
    const id = $identity;
    if (!id?.secretHex) return;
    try {
      await ensureWasm();
      machine = await mailAliasesMachine(id.secretHex);
      await machine.hydrate();
      applySnapshot();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  });

  // The kind→badge label comes from the shared Rust map
  // (`fauna_client_mail_settings::alias_kind_badge`, WASM twin `aliasKindBadge`)
  // — the same source the native apps consume, single-sourced (priority #2).
  // `kind` is the serde `AliasKind` variant-name string the snapshot carries.
  function kindBadge(kind: string): string {
    return resolveLocalized(aliasKindBadge(kind));
  }

  // `mail-aliases-list-item-hits` text: hit count + last-hit date if any. Routes
  // through the shared Rust `alias_hits_label` (WASM twin `aliasHitsLabel`), so
  // web stops hand-rolling the `"{n} hits · last {date}"` literal (priority #2).
  // The last-hit date stays a JS-local format (a local-tz concern — the shared fn
  // owns only the surrounding template; mail-aliases.md § Aliases UX).
  function formatHits(v: AliasView): string {
    const d = v.last_hit_at_ms != null ? new Date(v.last_hit_at_ms).toLocaleDateString() : null;
    return resolveLocalized(aliasHitsLabel(v.hit_count, d));
  }

  function openAdd(): void {
    editingId = null;
    isWildcard = false;
    patternInput = '';
    labelInput = '';
    spamThresholdInput = '';
    ratePerHourInput = '';
    ttlInput = '';
    usesInput = '';
    error = '';
    sheetOpen = true;
  }

  function openEdit(v: AliasView): void {
    editingId = v.alias_id_hex;
    isWildcard = v.kind === 'Wildcard';
    patternInput = v.pattern;
    labelInput = v.label;
    spamThresholdInput = v.spam_threshold_override != null ? String(v.spam_threshold_override) : '';
    ratePerHourInput = v.rate_limit_per_hour != null ? String(v.rate_limit_per_hour) : '';
    error = '';
    sheetOpen = true;
  }

  // Kind picker: a single value-via-state toggle (unchecked = Exact, checked =
  // Wildcard), the same idiom as mail-add-credential-type-selector. Disposable
  // mints via the generate button, not this sheet. Read-only on edit (kind is
  // immutable — can't change a wildcard into a disposable mid-life).
  function toggleKind(): void {
    if (editingId) return;
    isWildcard = !isWildcard;
  }

  async function submitSheet(): Promise<void> {
    if (!machine) return;
    const pattern = patternInput.trim();
    const label = labelInput.trim();
    // Shared validators (value-formatting.md § Mail-knob validation): spam
    // threshold is the `u32` knob (parseCount), rate-per-hour the signed-`i64`
    // per-alias knob (parseCountI64). Empty/invalid → null = "no override" (no
    // fall-back-to-prev, unlike admin-mail). Both reject fractional input — the
    // convergence off the old parseOptInt `Math.floor` drift.
    const spam_threshold_override = parseCount(spamThresholdInput) ?? null;
    const rate_limit_per_hour = parseCountI64(ratePerHourInput) ?? null;
    error = '';
    const action = editingId
      ? { Update: { alias_id_hex: editingId, pattern, label, spam_threshold_override, rate_limit_per_hour } }
      : { Create: { kind: isWildcard ? 'Wildcard' : 'Exact', pattern, label, spam_threshold_override, rate_limit_per_hour } };
    try {
      await machine.dispatch(action);
      applySnapshot();
      if (snap?.error) return; // leave the sheet open for retry
      sheetOpen = false;
    } catch (e) {
      applySnapshot();
      if (!error) error = e instanceof Error ? e.message : String(e);
    }
  }

  function cancelSheet(): void {
    sheetOpen = false;
  }

  function openImport(): void {
    importText = '';
    error = '';
    importResultDismissed = true;
    importOpen = true;
  }

  function cancelImport(): void {
    importOpen = false;
  }

  async function submitImport(): Promise<void> {
    if (!machine) return;
    const lines = importText.split('\n');
    error = '';
    try {
      await machine.dispatch({ Import: { lines } });
      applySnapshot();
      importResultDismissed = false;
    } catch (e) {
      applySnapshot();
      if (!error) error = e instanceof Error ? e.message : String(e);
    }
  }

  // `mail-aliases-import-result`: the shared counts template
  // (`mail_aliases.import_result`) plus one `mail_aliases.import_invalid_line`
  // row per invalid outcome — mail-aliases.md:199-201 requires the reason be
  // rendered, not just tallied.
  function importResultText(r: ImportResultView): string {
    const summary = t.mail_aliases.import_result({
      created: String(r.created),
      existed: String(r.skipped_duplicate),
      invalid: String(r.invalid),
    });
    const invalidLines = r.outcomes
      .filter((o) => o.status === 'Invalid')
      .map((o) => t.mail_aliases.import_invalid_line({ address: o.address, reason: o.reason ?? '' }));
    return invalidLines.length > 0 ? [summary, ...invalidLines].join('\n') : summary;
  }

  // Dispatch a fire-and-render action (generate / revoke / delete), then
  // re-render from the resulting snapshot.
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

  // One-click disposable mint with the per-user defaults; the nest derives
  // <handle>+<domain> from the actor's canonical exact alias server-side.
  async function generateDisposable(): Promise<void> {
    await dispatch({ GenerateDisposable: { ttl_days: null, uses: null, label: '' } });
  }

  // Two-click confirm: first click arms (auto-disarms after 4s), second fires.
  function onRevoke(v: AliasView): void {
    const aliasId = v.alias_id_hex;
    if (armedRevoke === aliasId) {
      armedRevoke = null;
      dispatch({ Revoke: { alias_id_hex: aliasId } });
    } else {
      armedRevoke = aliasId;
      setTimeout(() => { if (armedRevoke === aliasId) armedRevoke = null; }, 4000);
    }
  }

  function onDelete(v: AliasView): void {
    const aliasId = v.alias_id_hex;
    if (armedDelete === aliasId) {
      armedDelete = null;
      dispatch({ Delete: { alias_id_hex: aliasId } });
    } else {
      armedDelete = aliasId;
      setTimeout(() => { if (armedDelete === aliasId) armedDelete = null; }, 4000);
    }
  }

  // "Active" toggle: two-way (matches linux). The switch reflects "enabled" —
  // turning it OFF revokes (`disabled=true`), turning it back ON re-enables via
  // `fauna.bridges.enable_account_alias` (mail-aliases.md § Disable: "the user
  // can re-enable later"; disable is no longer a one-way trap).
  function onActiveToggle(v: AliasView): void {
    if (v.disabled) dispatch({ Enable: { alias_id_hex: v.alias_id_hex } });
    else dispatch({ Revoke: { alias_id_hex: v.alias_id_hex } });
  }

  async function copyText(value: string): Promise<void> {
    try { await navigator.clipboard.writeText(value); } catch { /* headless / no perm */ }
  }
</script>

<section class="section">
  <h2>{t.mail_aliases.title}</h2>
  <p class="muted small">{t.mail_aliases.description}</p>

  <div class="actions">
    <button class="btn" data-testid={IDS.MAIL_ALIASES_ADD_BUTTON} disabled={!hasDomain} onclick={openAdd}>
      {t.mail_aliases.add_button}
    </button>
    <button class="btn" data-testid={IDS.MAIL_ALIASES_GENERATE_DISPOSABLE_BUTTON} disabled={!hasDomain} onclick={generateDisposable}>
      {t.mail_aliases.generate_button}
    </button>
    <button class="btn" data-testid={IDS.MAIL_ALIASES_IMPORT_BUTTON} disabled={!hasDomain} onclick={openImport}>
      {t.mail_aliases.import_button}
    </button>
  </div>

  {#if !hasDomain}
    <p class="muted small">{t.mail_aliases.no_default_domain}</p>
  {/if}
  {#if mintedToast}
    <p class="muted small minted">{mintedToast}</p>
  {/if}

  <!-- Add/edit inline sheet (Create / Update). -->
  {#if sheetOpen}
    <div class="form">
      <button
        class="btn toggle"
        class:on={isWildcard}
        data-testid={IDS.MAIL_ALIASES_ADD_SHEET_KIND_PICKER}
        disabled={editingId != null}
        onclick={toggleKind}
      >{isWildcard ? t.mail_aliases.kind_wildcard_label : t.mail_aliases.kind_exact}</button>

      <input
        class="input"
        data-testid={IDS.MAIL_ALIASES_ADD_SHEET_PATTERN_INPUT}
        placeholder={t.mail_aliases.pattern_placeholder}
        bind:value={patternInput}
      />
      <input
        class="input"
        data-testid={IDS.MAIL_ALIASES_ADD_SHEET_LABEL_INPUT}
        placeholder={t.mail_aliases.label_placeholder}
        bind:value={labelInput}
      />
      <input
        class="input"
        data-testid={IDS.MAIL_ALIASES_ADD_SHEET_SPAM_THRESHOLD_INPUT}
        placeholder={t.mail_aliases.spam_threshold_placeholder}
        bind:value={spamThresholdInput}
      />
      <input
        class="input"
        data-testid={IDS.MAIL_ALIASES_ADD_SHEET_RATE_PER_HOUR_INPUT}
        placeholder={t.mail_aliases.rate_per_hour_placeholder}
        bind:value={ratePerHourInput}
      />
      <!-- Disposable-only; minted via the generate button (not this sheet) —
           present but hidden for ui.yaml conformance, same as linux. -->
      <input
        class="input"
        data-testid={IDS.MAIL_ALIASES_ADD_SHEET_TTL_INPUT}
        hidden
        placeholder={t.mail_aliases.ttl_placeholder}
        bind:value={ttlInput}
      />
      <input
        class="input"
        data-testid={IDS.MAIL_ALIASES_ADD_SHEET_USES_INPUT}
        hidden
        placeholder={t.mail_aliases.uses_placeholder}
        bind:value={usesInput}
      />

      <div class="form-actions">
        <button class="btn primary" data-testid={IDS.MAIL_ALIASES_ADD_SHEET_SUBMIT_BUTTON} onclick={submitSheet}>
          {t.mail_aliases.submit}
        </button>
        <button class="btn" data-testid={IDS.MAIL_ALIASES_ADD_SHEET_CANCEL_BUTTON} onclick={cancelSheet}>
          {t.mail_aliases.cancel}
        </button>
      </div>
    </div>
  {/if}

  <!-- Bulk paste-import inline sheet (recipient-whitelist migration path). -->
  {#if importOpen}
    <div class="form" data-testid={IDS.MAIL_ALIASES_IMPORT_SHEET}>
      <p class="muted small">{t.mail_aliases.import_subtitle}</p>
      <textarea
        class="input"
        rows="6"
        data-testid={IDS.MAIL_ALIASES_IMPORT_TEXTAREA}
        placeholder={t.mail_aliases.import_placeholder}
        bind:value={importText}
      ></textarea>

      <div class="form-actions">
        <button class="btn primary" data-testid={IDS.MAIL_ALIASES_IMPORT_SUBMIT_BUTTON} onclick={submitImport}>
          {t.mail_aliases.import_submit}
        </button>
        <button class="btn" data-testid={IDS.MAIL_ALIASES_IMPORT_CANCEL_BUTTON} onclick={cancelImport}>
          {t.mail_aliases.import_cancel}
        </button>
      </div>

      {#if !importResultDismissed && snap?.last_import_result}
        <p class="muted small result-block" data-testid={IDS.MAIL_ALIASES_IMPORT_RESULT}>
          {importResultText(snap.last_import_result)}
        </p>
      {/if}
    </div>
  {/if}

  <!-- Per-alias list (mail-aliases-list component). -->
  <div class="aliases" data-testid={IDS.MAIL_ALIASES_LIST}>
    {#if snap === null}
      <p class="muted small">{t.mail_aliases.loading}</p>
    {:else if snap.aliases.length === 0}
      <p class="muted small">{t.mail_aliases.empty}</p>
    {/if}
    {#each snap?.aliases ?? [] as v (v.alias_id_hex)}
      <div class="alias-item" data-testid={IDS.MAIL_ALIASES_LIST_ITEM}>
        <span class="alias-addr" data-testid={IDS.MAIL_ALIASES_LIST_ITEM_PATTERN}>{v.address}</span>
        <span class="badge" data-testid={IDS.MAIL_ALIASES_LIST_ITEM_KIND}>{kindBadge(v.kind)}</span>
        <span class="muted small" data-testid={IDS.MAIL_ALIASES_LIST_ITEM_LABEL}>{v.label}</span>
        <span class="muted small" data-testid={IDS.MAIL_ALIASES_LIST_ITEM_HITS}>{formatHits(v)}</span>
        {#if v.is_canonical}
          <!-- The canonical <handle>@<domain> primary is read-only: the nest
               rejects disabling / editing / revoking / deleting it
               (canonical_alias_protected), so the mutating controls are omitted
               and a "primary address" marker shown instead (mail-aliases.md
               § Aliases UX — no new ui.yaml id; the per-row mutators simply don't
               render on this row). The audit disclosure below stays (it's the
               busiest address). -->
          <span class="badge primary" title={t.mail_aliases.primary_address_tooltip}>
            {t.mail_aliases.primary_address_badge}
          </span>
        {:else}
          <label class="toggle-cell" title={v.disabled ? t.mail_aliases.disabled_badge : ''}>
            <input
              type="checkbox"
              data-testid={IDS.MAIL_ALIASES_LIST_ITEM_DISABLED_TOGGLE}
              checked={!v.disabled}
              onchange={() => onActiveToggle(v)}
            />
          </label>
          <button class="btn small" data-testid={IDS.MAIL_ALIASES_LIST_ITEM_EDIT_BUTTON} onclick={() => openEdit(v)}>
            {t.mail_aliases.edit}
          </button>
          <button
            class="btn small"
            class:danger={armedRevoke === v.alias_id_hex}
            data-testid={IDS.MAIL_ALIASES_LIST_ITEM_REVOKE_BUTTON}
            onclick={() => onRevoke(v)}
          >{armedRevoke === v.alias_id_hex ? t.common.confirm_q : t.mail_aliases.revoke}</button>
          <button
            class="btn small danger"
            class:armed={armedDelete === v.alias_id_hex}
            data-testid={IDS.MAIL_ALIASES_LIST_ITEM_OVERFLOW_MENU}
            onclick={() => onDelete(v)}
          >{armedDelete === v.alias_id_hex ? t.common.confirm_q : t.mail_aliases.delete}</button>
        {/if}
        <!-- Inert: per-alias hit audit (list_account_alias_hits) is a follow-on
             slice the shared machine doesn't expose yet (same gap as linux). -->
        <button class="btn small" data-testid={IDS.MAIL_ALIASES_LIST_ITEM_SHOW_AUDIT} disabled>
          {t.mail_aliases.show_audit}
        </button>
      </div>
    {/each}
  </div>
</section>

<style>
  .section { margin-bottom: 2rem; }
  .section h2 { margin-bottom: 0.5rem; font-size: 1.125rem; }
  .muted { color: var(--text-muted); }
  .small { font-size: 0.8rem; }
  .minted { color: var(--success, #3fb950); }
  /* import_invalid_line rows join the summary with real newlines. */
  .result-block { white-space: pre-line; }
  .actions { display: flex; gap: 0.5rem; margin: 0.5rem 0; flex-wrap: wrap; }
  .form {
    display: flex; flex-direction: column; gap: 0.5rem; max-width: 420px;
    margin-top: 0.5rem; padding: 0.75rem; border: 1px solid var(--border);
    border-radius: 6px; background: var(--bg-surface);
  }
  .form-actions { display: flex; gap: 0.5rem; }
  .input {
    padding: 0.5rem; border: 1px solid var(--border); border-radius: 6px;
    background: var(--bg); color: var(--text); font-size: 0.875rem;
  }
  .aliases { display: flex; flex-direction: column; gap: 0.5rem; margin-top: 0.75rem; }
  .alias-item {
    display: flex; align-items: center; gap: 0.5rem; padding: 0.5rem;
    border: 1px solid var(--border); border-radius: 6px; flex-wrap: wrap;
  }
  .alias-addr { font-family: monospace; font-size: 0.85rem; flex: 1; word-break: break-all; }
  .toggle-cell { display: flex; align-items: center; }
  .badge {
    font-size: 0.75rem; padding: 0.125rem 0.5rem; border-radius: 4px;
    background: var(--bg-hover); color: var(--text-muted);
  }
  /* The read-only canonical-row marker — accent-tinted so the primary address
     reads as protected rather than just another kind badge. */
  .badge.primary {
    background: color-mix(in srgb, var(--accent) 18%, transparent);
    color: var(--accent);
  }
  .btn {
    padding: 0.5rem 1rem; border: 1px solid var(--border); border-radius: 6px;
    background: var(--bg-surface); color: var(--text); cursor: pointer;
    font-size: 0.875rem;
  }
  .btn:hover { background: var(--bg-hover); }
  .btn:disabled { opacity: 0.5; cursor: not-allowed; }
  .btn.primary { background: var(--accent); color: #fff; border-color: var(--accent); }
  .btn.small { padding: 0.25rem 0.625rem; font-size: 0.8rem; }
  .btn.danger { border-color: var(--danger); color: var(--danger); }
  .btn.toggle.on { background: rgba(63, 185, 80, 0.15); border-color: var(--success, #3fb950); color: var(--success, #3fb950); }
</style>
