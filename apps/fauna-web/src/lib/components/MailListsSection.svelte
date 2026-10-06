<script lang="ts">
  // User-settings "Lists" section: a person runs **their own** mailing lists (a
  // list is a sixth alias kind), so it sits beside Aliases under mail settings.
  // Create / edit / delete a list, each with a friendly name, a send-from address
  // on one of the user's domains, optional List-Help / List-Archive URLs, and a
  // per-send recipient cap; and drill into a list's members (add / batch-import /
  // unsubscribe / resubscribe). A dumb renderer of the shared `MailListsMachine` +
  // `MailListMembersMachine` (`libs/fauna-client-mail-settings`, WASM twins
  // `WasmMailListsMachine` / `WasmMailListMembersMachine`): build over the
  // singleton WS-RPC client → hydrate() → render snapshot() → dispatch(action) →
  // re-render. No list logic in the SPA (priority #2). Lifts the linux lead shape
  // (apps/fauna-linux/src/settings/mail_lists.rs + mail_list_members.rs). Reuses
  // the settings page's single `error-message` via the bindable `error`. Behavior:
  // docs/goal/behavior/mail-mass-mailing.md § mail-lists page UX / § mail-list-
  // members page; UX/IDs: ui.yaml `mail-lists` + `mail-list-members` pages + their
  // `*-list` components.
  //
  // The per-list members view is an **in-section reveal** keyed on the selected
  // list id (NOT a new SvelteKit route — keeps the embedded pattern aliases/spam/
  // export use); the members machine is built lazily on selection. This is richer
  // than the linux lead, which left the members button inert (its embedded seed
  // has no inter-page routing); the shared machine is identical, only the shell
  // presentation differs (a sanctioned platform-shell divergence, priority #2).
  //
  // Honest backend gap (never faked, mirroring linux): the list RPCs
  // (`fauna.bridges.{list,create,update,delete}_account_list` + `*_list_member`)
  // have no nest handler yet (mail-mass-mailing.md § Implementation status today).
  // The machines return the honest "unimplemented" rejection; this section renders
  // it via `error` and stays at empty lists/members until the nest track lands.
  import { identity } from '$lib/store';
  import { mailListsMachine, mailListMembersMachine } from '$lib/rpc';
  import { ensureWasm, memberStatusLabel } from '$lib/wasm';
  import { resolveLocalized } from '$lib/i18n/localized';
  import { afterNavigate } from '$app/navigation';
  import { onMount } from 'svelte';
  import { t } from '$lib/i18n/strings';
  import type {
    WasmMailListsMachine,
    WasmMailListMembersMachine,
  } from '../../../static/fauna_wasm.js';
  import { IDS } from '$lib/generated/uiIds';

  // The parent settings page owns the single error surface; list dispatch errors
  // flow into it (the same surface the other mail-settings sections bind to).
  // `subpage` is the rail sub-id ('mail-lists' | 'mail-list-members') so this ONE
  // instance can tell which rail slot the user is on: `mail-list-members` has its
  // own reachable rail slot (settings.md § Navigation model). A direct visit with
  // nothing selected falls back to the caller's first owned list — never silently
  // to the Lists view — matching tui/windows/apple (mail-mass-mailing.md § Per-app
  // render status); the honest empty state is reserved for the genuinely-zero-
  // lists case. Unlike windows/apple (each panel builds its own throwaway lists
  // machine to resolve this), this component already has an already-hydrated
  // `machine`/`snap` for the Lists view — the tui shape, where any settings page
  // can just peek at it (`resolveMembersFallback` below).
  let { error = $bindable(''), subpage = '' } = $props();

  // Shapes mirror the shared serde JSON (snake_case across the
  // serde_wasm_bindgen boundary; the status enum serializes as its variant-name
  // string).
  interface ListView {
    list_id_hex: string;
    friendly_name: string;
    local_part: string;
    local_domain: string;
    address: string;
    description: string;
    member_count: number;
    last_send_at_ms: number | null;
    sends_today: number;
    recipients_today: number;
    list_help_url: string;
    list_archive_url: string;
    recipients_per_send: number | null;
  }
  interface ListsSnapshot {
    lists: ListView[];
    local_domains: string[];
    status: string; // "Idle" | "Loading" | "Working"
    error: string | null;
  }
  interface MemberView {
    address: string;
    subscribed_at_ms: number | null;
    status: string; // "Subscribed" | "Unsubscribed"
  }
  interface MembersSnapshot {
    list_id_hex: string;
    list_name: string;
    members: MemberView[];
    subscribed_count: number;
    unsubscribed_count: number;
    status: string;
    error: string | null;
    last_import: { added: number; skipped_invalid: number; skipped_duplicate: number } | null;
  }

  let machine: WasmMailListsMachine | null = null;
  let snap = $state<ListsSnapshot | null>(null);

  // Add/edit inline sheet (reveal, not modal — matches aliases + mail-settings).
  let sheetOpen = $state(false);
  let editingId = $state<string | null>(null); // non-null ⇒ Edit (local-part/domain read-only)
  let nameInput = $state('');
  let localPartInput = $state('');
  let domainInput = $state('');
  let descriptionInput = $state('');
  let listHelpInput = $state('');
  let listArchiveInput = $state('');
  let perSendInput = $state('');

  // Per-row two-click arm state for the destructive delete gesture (auto-disarms
  // after 4s — matches the aliases revoke/delete idiom).
  let armedDelete = $state<string | null>(null);

  const hasDomain = $derived((snap?.local_domains.length ?? 0) > 0);
  const busy = $derived(snap?.status === 'Working');

  // ── members drill-down (in-section reveal) ──────────────────────────
  let membersMachine = $state<WasmMailListMembersMachine | null>(null);
  let membersSnap = $state<MembersSnapshot | null>(null);
  let selectedListId = $state<string | null>(null); // non-null ⇒ members view shown
  // Reachable directly via the `mail-list-members` rail slot even with nothing
  // selected (a user with no lists still lands here on an honest empty page).
  const showMembersView = $derived(selectedListId != null || subpage === 'mail-list-members');
  let memberSheetOpen = $state(false);
  let importSheetOpen = $state(false);
  let addMemberInput = $state('');
  let importInput = $state('');

  function applySnapshot(): void {
    if (!machine) return;
    snap = machine.snapshot() as unknown as ListsSnapshot;
    if (snap?.error) error = snap.error;
    resolveMembersFallback();
  }

  function applyMembersSnapshot(): void {
    if (!membersMachine) return;
    membersSnap = membersMachine.snapshot() as unknown as MembersSnapshot;
    if (membersSnap?.error) error = membersSnap.error;
  }

  // The `mail-list-members` fallback (mail-mass-mailing.md § Per-app render
  // status — tui's shape, adopted by windows/apple): a direct rail visit with
  // nothing selected opens the caller's first owned list instead of the honest
  // empty state, when there is one to open. Called on every navigation event
  // (`afterNavigate` below — the reliable per-nav signal `WebSettingsSection`
  // established, since `$page.params` does not change on a same-route
  // re-navigation) and again once `snap` actually loads (a nav can land before
  // `machine.hydrate()` resolves). Deliberately NOT re-run by `closeMembers` —
  // that only clears local selection, no navigation happened, so the "← Lists"
  // button must not immediately re-select the same first list out from under
  // the user who just backed out of it.
  function resolveMembersFallback(): void {
    if (subpage !== 'mail-list-members' || selectedListId != null) return;
    const first = snap?.lists[0];
    if (first) void openMembers(first);
  }

  onMount(async () => {
    const id = $identity;
    if (!id?.secretHex) return;
    try {
      await ensureWasm();
      machine = await mailListsMachine(id.secretHex);
      await machine.hydrate();
      applySnapshot();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  });

  afterNavigate(() => resolveMembersFallback());

  // Parse an optional non-negative integer (empty/invalid → null; the nest
  // re-validates against the admin ceiling).
  function parseOptInt(text: string): number | null {
    const trimmed = text.trim();
    if (!trimmed) return null;
    const n = Number(trimmed);
    return Number.isFinite(n) && n >= 0 ? Math.floor(n) : null;
  }

  function lastSendText(v: ListView): string {
    return v.last_send_at_ms != null ? new Date(v.last_send_at_ms).toLocaleDateString() : '—';
  }

  function openAdd(): void {
    editingId = null;
    nameInput = '';
    localPartInput = '';
    domainInput = snap?.local_domains[0] ?? '';
    descriptionInput = '';
    listHelpInput = '';
    listArchiveInput = '';
    perSendInput = '';
    error = '';
    sheetOpen = true;
  }

  function openEdit(v: ListView): void {
    editingId = v.list_id_hex;
    nameInput = v.friendly_name;
    localPartInput = v.local_part;
    domainInput = v.local_domain;
    descriptionInput = v.description;
    listHelpInput = v.list_help_url;
    listArchiveInput = v.list_archive_url;
    perSendInput = v.recipients_per_send != null ? String(v.recipients_per_send) : '';
    error = '';
    sheetOpen = true;
  }

  function buildDraft() {
    return {
      friendly_name: nameInput.trim(),
      local_part: localPartInput.trim(),
      local_domain: domainInput,
      description: descriptionInput.trim(),
      list_help_url: listHelpInput.trim(),
      list_archive_url: listArchiveInput.trim(),
      recipients_per_send: parseOptInt(perSendInput),
    };
  }

  async function submitSheet(): Promise<void> {
    if (!machine) return;
    error = '';
    const draft = buildDraft();
    const action = editingId
      ? { Update: { list_id_hex: editingId, draft } }
      : { Create: { draft } };
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
  // destructive delete (cascades members per mail-mass-mailing.md).
  function onDelete(v: ListView): void {
    const id = v.list_id_hex;
    if (armedDelete === id) {
      armedDelete = null;
      dispatch({ Delete: { list_id_hex: id } });
    } else {
      armedDelete = id;
      setTimeout(() => { if (armedDelete === id) armedDelete = null; }, 4000);
    }
  }

  // Open the per-list members view: build the scoped members machine lazily, then
  // hydrate. On a real nest hydrate rejects (backend unbuilt) → the error flows to
  // the page surface; the members controls still render (empty list).
  async function openMembers(v: ListView): Promise<void> {
    const id = $identity;
    if (!id?.secretHex) return;
    selectedListId = v.list_id_hex;
    memberSheetOpen = false;
    importSheetOpen = false;
    addMemberInput = '';
    importInput = '';
    membersSnap = null;
    error = '';
    try {
      membersMachine = await mailListMembersMachine(id.secretHex, v.list_id_hex, v.friendly_name);
      await membersMachine.hydrate();
      applyMembersSnapshot();
    } catch (e) {
      if (!error) error = e instanceof Error ? e.message : String(e);
    }
  }

  function closeMembers(): void {
    selectedListId = null;
    membersMachine = null;
    membersSnap = null;
    memberSheetOpen = false;
    importSheetOpen = false;
  }

  async function membersDispatch(action: unknown): Promise<void> {
    if (!membersMachine) return;
    try {
      await membersMachine.dispatch(action);
      applyMembersSnapshot();
    } catch (e) {
      applyMembersSnapshot();
      if (!error) error = e instanceof Error ? e.message : String(e);
    }
  }

  async function submitAddMember(): Promise<void> {
    const address = addMemberInput.trim();
    if (!address) return;
    await membersDispatch({ AddMember: { address } });
    if (!membersSnap?.error) {
      addMemberInput = '';
      memberSheetOpen = false;
    }
  }

  async function submitImport(): Promise<void> {
    const addresses = importInput;
    if (!addresses.trim()) return;
    await membersDispatch({ BatchImport: { addresses } });
    if (!membersSnap?.error) {
      importInput = '';
      importSheetOpen = false;
    }
  }

  // The Subscribed/Unsubscribed label comes from the shared Rust map
  // (`fauna_client_mail_settings::member_status_label`, WASM twin `memberStatusLabel`)
  // — the same source the native apps consume, single-sourced (priority #2).
  function memberStatusBadge(status: string): string {
    return resolveLocalized(memberStatusLabel(status));
  }
</script>

<section class="section">
  {#if !showMembersView}
    <!-- ── Lists view ── -->
    <h2>{t.mail_lists.title}</h2>
    <p class="muted small">{t.mail_lists.description}</p>

    <div class="actions">
      <button class="btn" data-testid={IDS.MAIL_LISTS_ADD_BUTTON} disabled={!hasDomain} onclick={openAdd}>
        {t.mail_lists.add_button}
      </button>
    </div>

    {#if !hasDomain}
      <p class="muted small">{t.mail_lists.no_domain}</p>
    {/if}

    <!-- Add/edit inline sheet. -->
    {#if sheetOpen}
      <div class="form">
        <input
          class="input"
          data-testid={IDS.MAIL_LISTS_ADD_SHEET_NAME_INPUT}
          placeholder={t.mail_lists.name_placeholder}
          bind:value={nameInput}
        />
        <input
          class="input"
          data-testid={IDS.MAIL_LISTS_ADD_SHEET_LOCAL_PART_INPUT}
          placeholder={t.mail_lists.local_part_placeholder}
          disabled={editingId != null}
          bind:value={localPartInput}
        />
        <select
          class="input"
          data-testid={IDS.MAIL_LISTS_ADD_SHEET_DOMAIN_PICKER}
          disabled={editingId != null}
          bind:value={domainInput}
        >
          {#each snap?.local_domains ?? [] as d (d)}
            <option value={d}>{d}</option>
          {/each}
        </select>
        <input
          class="input"
          data-testid={IDS.MAIL_LISTS_ADD_SHEET_DESCRIPTION_INPUT}
          placeholder={t.mail_lists.description_placeholder}
          bind:value={descriptionInput}
        />
        <input
          class="input"
          data-testid={IDS.MAIL_LISTS_ADD_SHEET_LIST_HELP_URL_INPUT}
          placeholder={t.mail_lists.list_help_placeholder}
          bind:value={listHelpInput}
        />
        <input
          class="input"
          data-testid={IDS.MAIL_LISTS_ADD_SHEET_LIST_ARCHIVE_URL_INPUT}
          placeholder={t.mail_lists.list_archive_placeholder}
          bind:value={listArchiveInput}
        />
        <input
          class="input"
          data-testid={IDS.MAIL_LISTS_ADD_SHEET_PER_SEND_CAP_INPUT}
          placeholder={t.mail_lists.per_send_placeholder}
          bind:value={perSendInput}
        />
        <div class="form-actions">
          <button class="btn primary" data-testid={IDS.MAIL_LISTS_ADD_SHEET_SUBMIT_BUTTON} disabled={busy} onclick={submitSheet}>
            {t.mail_lists.submit}
          </button>
          <button class="btn" data-testid={IDS.MAIL_LISTS_ADD_SHEET_CANCEL_BUTTON} onclick={cancelSheet}>
            {t.mail_lists.cancel}
          </button>
        </div>
      </div>
    {/if}

    <!-- Per-list list (mail-lists-list component). -->
    <div class="lists" data-testid={IDS.MAIL_LISTS_LIST}>
      {#if snap === null}
        <p class="muted small">{t.mail_lists.loading}</p>
      {:else if snap.lists.length === 0}
        <p class="muted small">{t.mail_lists.empty}</p>
      {/if}
      {#each snap?.lists ?? [] as v (v.list_id_hex)}
        <div class="list-item" data-testid={IDS.MAIL_LISTS_LIST_ITEM}>
          <span class="list-name" data-testid={IDS.MAIL_LISTS_LIST_ITEM_NAME}>{v.friendly_name} — {v.address}</span>
          <span class="muted small" data-testid={IDS.MAIL_LISTS_LIST_ITEM_MEMBER_COUNT}>{v.member_count}</span>
          <span class="muted small" data-testid={IDS.MAIL_LISTS_LIST_ITEM_LAST_SEND}>{lastSendText(v)}</span>
          <span class="muted small" data-testid={IDS.MAIL_LISTS_LIST_ITEM_QUOTA}>{v.sends_today} · {v.recipients_today}</span>
          <button class="btn small" data-testid={IDS.MAIL_LISTS_LIST_ITEM_EDIT_BUTTON} onclick={() => openEdit(v)}>
            {t.mail_lists.edit}
          </button>
          <button class="btn small" data-testid={IDS.MAIL_LISTS_LIST_ITEM_MEMBERS_BUTTON} onclick={() => openMembers(v)}>
            {t.mail_lists.members}
          </button>
          <button
            class="btn small danger"
            class:armed={armedDelete === v.list_id_hex}
            data-testid={IDS.MAIL_LISTS_LIST_ITEM_DELETE_BUTTON}
            onclick={() => onDelete(v)}
          >{armedDelete === v.list_id_hex ? t.mail_lists.delete_confirm : t.mail_lists.delete}</button>
        </div>
      {/each}
    </div>
  {:else}
    <!-- ── Members view (in-section reveal, scoped to one list) ── -->
    <div class="members-head">
      <button class="btn small" onclick={closeMembers}>← {t.mail_lists.title}</button>
      <h2>{membersSnap?.list_name ?? ''} · {t.mail_lists.members_title}</h2>
    </div>
    <p class="muted small" data-testid={IDS.MAIL_LIST_MEMBERS_SUMMARY}>
      {t.mail_lists.summary_fmt({
        subscribed: String(membersSnap?.subscribed_count ?? 0),
        unsubscribed: String(membersSnap?.unsubscribed_count ?? 0),
      })}
    </p>

    <div class="actions">
      <button class="btn" data-testid={IDS.MAIL_LIST_MEMBERS_ADD_BUTTON} onclick={() => { memberSheetOpen = true; importSheetOpen = false; error = ''; }}>
        {t.mail_lists.add_member_button}
      </button>
      <button class="btn" data-testid={IDS.MAIL_LIST_MEMBERS_IMPORT_BUTTON} onclick={() => { importSheetOpen = true; memberSheetOpen = false; error = ''; }}>
        {t.mail_lists.import_button}
      </button>
    </div>

    {#if memberSheetOpen}
      <div class="form">
        <input
          class="input"
          data-testid={IDS.MAIL_LIST_MEMBERS_ADD_SHEET_ADDRESS_INPUT}
          placeholder={t.mail_lists.add_member_placeholder}
          bind:value={addMemberInput}
        />
        <div class="form-actions">
          <button class="btn primary" data-testid={IDS.MAIL_LIST_MEMBERS_ADD_SHEET_SUBMIT_BUTTON} onclick={submitAddMember}>
            {t.mail_lists.add_member_submit}
          </button>
          <button class="btn" data-testid={IDS.MAIL_LIST_MEMBERS_ADD_SHEET_CANCEL_BUTTON} onclick={() => { memberSheetOpen = false; }}>
            {t.mail_lists.add_member_cancel}
          </button>
        </div>
      </div>
    {/if}

    {#if importSheetOpen}
      <div class="form">
        <textarea
          class="input"
          rows="6"
          data-testid={IDS.MAIL_LIST_MEMBERS_IMPORT_SHEET_INPUT}
          placeholder={t.mail_lists.import_placeholder}
          bind:value={importInput}
        ></textarea>
        <div class="form-actions">
          <button class="btn primary" data-testid={IDS.MAIL_LIST_MEMBERS_IMPORT_SHEET_SUBMIT_BUTTON} onclick={submitImport}>
            {t.mail_lists.import_submit}
          </button>
          <button class="btn" data-testid={IDS.MAIL_LIST_MEMBERS_IMPORT_SHEET_CANCEL_BUTTON} onclick={() => { importSheetOpen = false; }}>
            {t.mail_lists.import_cancel}
          </button>
        </div>
      </div>
    {/if}

    <!-- Per-member list (mail-list-members-list component). -->
    <div class="members" data-testid={IDS.MAIL_LIST_MEMBERS_LIST}>
      {#if selectedListId === null}
        <p class="muted small">{t.mail_lists.members_no_list}</p>
      {:else if membersSnap === null}
        <p class="muted small">{t.mail_lists.members_loading}</p>
      {/if}
      {#each membersSnap?.members ?? [] as m (m.address)}
        <div class="member-item" data-testid={IDS.MAIL_LIST_MEMBERS_LIST_ITEM}>
          <span class="member-addr" data-testid={IDS.MAIL_LIST_MEMBERS_LIST_ITEM_ADDRESS}>{m.address}</span>
          <span class="muted small" data-testid={IDS.MAIL_LIST_MEMBERS_LIST_ITEM_SUBSCRIBED_AT}>
            {m.subscribed_at_ms != null ? new Date(m.subscribed_at_ms).toLocaleDateString() : '—'}
          </span>
          <span class="badge" data-testid={IDS.MAIL_LIST_MEMBERS_LIST_ITEM_STATUS}>{memberStatusBadge(m.status)}</span>
          <button
            class="btn small"
            data-testid={IDS.MAIL_LIST_MEMBERS_LIST_ITEM_UNSUBSCRIBE_BUTTON}
            disabled={m.status !== 'Subscribed'}
            onclick={() => membersDispatch({ Unsubscribe: { address: m.address } })}
          >{t.mail_lists.unsubscribe}</button>
          <button
            class="btn small"
            data-testid={IDS.MAIL_LIST_MEMBERS_LIST_ITEM_RESUBSCRIBE_BUTTON}
            disabled={m.status !== 'Unsubscribed'}
            onclick={() => membersDispatch({ Resubscribe: { address: m.address } })}
          >{t.mail_lists.resubscribe}</button>
        </div>
      {/each}
    </div>
  {/if}
</section>

<style>
  .section { margin-bottom: 2rem; }
  .section h2 { margin-bottom: 0.5rem; font-size: 1.125rem; }
  .muted { color: var(--text-muted); }
  .small { font-size: 0.8rem; }
  .actions { display: flex; gap: 0.5rem; margin: 0.5rem 0; flex-wrap: wrap; }
  .members-head { display: flex; align-items: center; gap: 0.75rem; flex-wrap: wrap; }
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
  .lists, .members { display: flex; flex-direction: column; gap: 0.5rem; margin-top: 0.75rem; }
  .list-item, .member-item {
    display: flex; align-items: center; gap: 0.5rem; padding: 0.5rem;
    border: 1px solid var(--border); border-radius: 6px; flex-wrap: wrap;
  }
  .list-name, .member-addr { font-family: monospace; font-size: 0.85rem; flex: 1; word-break: break-all; }
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
  .btn.primary { background: var(--accent); color: #fff; border-color: var(--accent); }
  .btn.small { padding: 0.25rem 0.625rem; font-size: 0.8rem; }
  .btn.danger { border-color: var(--danger); color: var(--danger); }
  .btn.danger.armed { background: var(--danger); color: #fff; }
</style>
