<script lang="ts">
  // Admin Held-Custody page (`admin-custody-hosting`) — the nest-wide
  // custody-hosting registry (account-data-plane.md § Two-sided bounds). `fauna.custody.hosting.list`
  // is host-scoped, so without this page an admin can neither see nor drop a
  // row an account holder planted — the recovery would be sqlite3 on nest.db
  // plus rm -rf, which the *no client-causable unrecoverable nest state*
  // invariant forbids.
  //
  // A CONTEXTUAL detail page like admin-dns/admin-logs/admin-bridges-pending:
  // absent from ui.yaml's navigation.admin_pages, reached from the admin nav
  // rail (routes/admin/+layout.svelte) like its siblings. `admin-nav-back` is
  // provided by that shell.
  //
  // Read/write ride the shared `AdminHostingClient` + `admin_hosting_rows`
  // fold over the wasm twin (`libs/fauna-wasm/src/rpc.rs`'s `adminHostingList`/
  // `adminHostingRemove`, the FFI mirror of `libs/fauna-ffi/src/admin.rs`'s
  // `FfiAdminClient::custody_hosting_{list,remove}`) — no forking logic here
  // (priority #2). Reference: `apps/fauna-tui/src/admin/custody_hosting.rs`
  // (lead) and `apps/fauna-linux/src/client.rs`'s `fetch_custody_hosting`/
  // `remove_custody_hosting` (second app), both calling the shared client
  // directly since they are native Rust.
  import { onMount } from 'svelte';
  import { identity } from '$lib/store';
  import { adminHostingList, adminHostingRemove } from '$lib/rpc';
  import type { AdminHostingRow } from '$lib/rpc';
  import { toBytes } from '$lib/hex';
  import { byteSizeRaw } from '$lib/wasm';
  import { resolveLocalized } from '$lib/i18n/localized';
  import { t } from '$lib/i18n/strings';
  import { IDS } from '$lib/generated/uiIds';
  import MessageBanner from '$lib/components/MessageBanner.svelte';

  // `null` = not hydrated yet — distinct from an answered empty list. A page
  // that paints "0 held" or the empty state before the read has answered
  // would claim the reassuring fact for the unknown one (the same honesty
  // rule the tui/linux legs pin with tests).
  let rows = $state<AdminHostingRow[] | null>(null);
  let error = $state('');
  // The remove's own verdict (`removed: false` is an honest no-op, never an
  // error) — chrome text, no ui.yaml id of its own, mirroring tui's
  // `Element::chrome(status)`.
  let status = $state('');
  // The one armed row's key, or null. Mirrors tui/linux: the confirm names
  // ONE row, so opening a new one silently retargets it (no guard needed —
  // only the currently-armed row's own remove button disables itself below).
  let confirmKey = $state<string | null>(null);
  let busy = $state(false);

  function rowKey(row: AdminHostingRow): string {
    return `${row.host_actor_id}:${Array.from(toBytes(row.grant_id)).join(',')}`;
  }

  async function load(): Promise<void> {
    const s = $identity?.secretHex;
    if (!s) return;
    try {
      rows = await adminHostingList(s);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  onMount(load);

  // `0` is not "no bytes allowed" — the row carries no cap and the pump
  // substitutes the hard-coded default, so *Default* is the honest text; a
  // printed `0 B` would state the opposite of the truth.
  function budgetText(cap: number): string {
    return cap === 0
      ? t.admin.custody_hosting.budget_default
      : resolveLocalized(byteSizeRaw(cap));
  }

  const RECEIPT_TEXT: Record<AdminHostingRow['receipt_state'], string> = {
    fresh: t.admin.custody_hosting.receipt_fresh,
    stale: t.admin.custody_hosting.receipt_stale,
    no_receipt_yet: t.admin.custody_hosting.receipt_none,
  };

  function openConfirm(row: AdminHostingRow): void {
    status = '';
    confirmKey = rowKey(row);
  }

  function cancelConfirm(): void {
    confirmKey = null;
  }

  async function confirmRemove(row: AdminHostingRow): Promise<void> {
    const s = $identity?.secretHex;
    if (!s) return;
    confirmKey = null;
    busy = true;
    try {
      const reply = await adminHostingRemove(s, row.host_actor_id, toBytes(row.grant_id));
      if (!reply.removed) {
        status = t.admin.custody_hosting.remove_missing;
      } else if (reply.store_dropped) {
        status = t.admin.custody_hosting.removed_with_store;
      } else {
        status = t.admin.custody_hosting.removed;
      }
      await load();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }
</script>

<svelte:head><title>{t.common.admin} · {t.admin.custody_hosting.title}</title></svelte:head>

<h1 data-testid={IDS.PAGE_HEADING}>{t.admin.custody_hosting.title}</h1>

<MessageBanner bind:error />

<p class="muted">{t.admin.custody_hosting.description}</p>

{#if rows !== null}
  <p data-testid={IDS.ADMIN_CUSTODY_HOSTING_COUNT}>
    {t.admin.custody_hosting.count({ count: String(rows.length) })}
  </p>

  {#if rows.length === 0}
    <p data-testid={IDS.ADMIN_CUSTODY_HOSTING_EMPTY}>{t.admin.custody_hosting.empty}</p>
  {/if}

  {#if status}
    <p class="status">{status}</p>
  {/if}

  <div class="rows">
    {#each rows as row, i (rowKey(row))}
      <div class="row" data-testid={`${IDS.ADMIN_CUSTODY_HOSTING_ROW}-${i}`}>
        <div class="field">
          <span class="label">{t.admin.custody_hosting.host}</span>
          <span class="value mono" data-testid={`${IDS.ADMIN_CUSTODY_HOSTING_HOST}-${i}`}
            >{row.host_actor_id}</span
          >
        </div>
        <div class="field">
          <span class="label">{t.admin.custody_hosting.owner}</span>
          <span class="value mono" data-testid={`${IDS.ADMIN_CUSTODY_HOSTING_OWNER}-${i}`}
            >{row.owner_actor_id}</span
          >
        </div>
        <div class="field">
          <span class="label">{t.admin.custody_hosting.url}</span>
          <span class="value mono" data-testid={`${IDS.ADMIN_CUSTODY_HOSTING_URL}-${i}`}
            >{row.owner_nest_url}</span
          >
        </div>
        <div class="field">
          <span class="label">{t.admin.custody_hosting.budget}</span>
          <span class="value" data-testid={`${IDS.ADMIN_CUSTODY_HOSTING_BUDGET}-${i}`}
            >{budgetText(row.retained_bytes_cap)}</span
          >
        </div>
        <div class="field">
          <span class="label">{t.admin.custody_hosting.held}</span>
          <span class="value" data-testid={`${IDS.ADMIN_CUSTODY_HOSTING_HELD}-${i}`}
            >{resolveLocalized(byteSizeRaw(row.held_bytes))}</span
          >
        </div>
        <div class="field">
          <span class="value" data-testid={`${IDS.ADMIN_CUSTODY_HOSTING_STOPPED}-${i}`}>
            {row.stopped ? t.admin.custody_hosting.stopped : t.admin.custody_hosting.active}
          </span>
        </div>
        <div class="field">
          <span class="value" data-testid={`${IDS.ADMIN_CUSTODY_HOSTING_RECEIPT}-${i}`}>
            {RECEIPT_TEXT[row.receipt_state]}
          </span>
        </div>
        <div class="actions">
          <button
            class="btn small danger"
            data-testid={`${IDS.ADMIN_CUSTODY_HOSTING_REMOVE_BUTTON}-${i}`}
            disabled={busy || confirmKey === rowKey(row)}
            onclick={() => openConfirm(row)}
          >
            {t.admin.custody_hosting.remove}
          </button>
        </div>
        {#if confirmKey === rowKey(row)}
          <div class="confirm-box">
            <strong>{t.admin.custody_hosting.remove_confirm_title}</strong>
            <p class="muted small">{t.admin.custody_hosting.remove_confirm_body}</p>
            <div class="actions">
              <button
                class="btn small"
                data-testid={IDS.ADMIN_CUSTODY_HOSTING_REMOVE_CANCEL_BUTTON}
                disabled={busy}
                onclick={cancelConfirm}
              >
                {t.admin.custody_hosting.remove_cancel}
              </button>
              <button
                class="btn small danger"
                data-testid={IDS.ADMIN_CUSTODY_HOSTING_REMOVE_CONFIRM_BUTTON}
                disabled={busy}
                onclick={() => confirmRemove(row)}
              >
                {t.admin.custody_hosting.remove_confirm}
              </button>
            </div>
          </div>
        {/if}
      </div>
    {/each}
  </div>
{/if}

<style>
  h1 { margin-bottom: 0.5rem; font-size: 1.5rem; }
  .muted { color: var(--text-muted, #8b949e); }
  .small { font-size: 0.8rem; }
  .status { color: var(--text, #e6edf3); font-size: 0.875rem; margin: 0.5rem 0; }

  .rows { display: flex; flex-direction: column; gap: 0.75rem; margin-top: 1rem; }
  .row {
    border: 1px solid var(--border, #30363d);
    border-radius: 8px;
    padding: 0.75rem 1rem;
    background: var(--bg-surface, #161b22);
  }
  .field { display: flex; gap: 0.5rem; padding: 0.125rem 0; font-size: 0.875rem; }
  .label {
    min-width: 90px;
    color: var(--text-muted, #8b949e);
    text-transform: uppercase;
    font-size: 0.7rem;
    letter-spacing: 0.05em;
    align-self: center;
  }
  .value { word-break: break-all; }
  .mono { font-family: monospace; font-size: 0.8rem; }
  .actions { display: flex; gap: 0.5rem; justify-content: flex-end; margin-top: 0.5rem; }

  .confirm-box {
    margin-top: 0.75rem;
    padding: 0.75rem;
    border: 1px solid var(--danger, #f85149);
    border-radius: 6px;
    display: flex;
    flex-direction: column;
    gap: 0.375rem;
  }

  .btn {
    padding: 0.375rem 0.75rem;
    border: 1px solid var(--border, #30363d);
    border-radius: 6px;
    background: var(--bg-surface, #161b22);
    color: var(--text, #e6edf3);
    cursor: pointer;
    font-size: 0.8rem;
  }
  .btn:hover { background: var(--bg-hover, #1c2128); }
  .btn:disabled { opacity: 0.5; cursor: not-allowed; }
  .btn.danger { border-color: var(--danger, #f85149); color: var(--danger, #f85149); }
  .btn.small { font-size: 0.8rem; padding: 0.25rem 0.625rem; }
</style>
