<script lang="ts">
  // The Search page — a paint shell over the shared (wasm) `SearchManager`
  // (`docs/goal/ui/search.md` § State & data shape, ratified 2026-08-02;
  // ui.yaml `search`). Every decision this page used to make itself — which
  // backends to ask, how to map the type filter onto them, how to order/dedup
  // rows, when to offer "load more" — now belongs to the manager; web is the
  // last of the seven apps to adopt it (the wasm face gates were the final
  // blocker). What is left here is genuinely per-app: the query-field buffer,
  // the collapsible bar, the type-filter option labels, and painting a
  // `SearchSnapshot` — the same split tui's `src/search.rs` documents.
  //
  // Backend 2 (the sealed local index) never registers on web — tantivy can't
  // run in a browser (content-index.md § Where queries run), so `has_more`,
  // `results`, etc. are always nest-only here; that is a normal state, not a
  // gap, and there is nothing to "fix".
  import { get } from 'svelte/store';
  import { goto } from '$app/navigation';
  import { identity } from '$lib/store';
  import { getSearchManager, searchSnapshot, refreshSearch, setPendingSearchNav, type PendingSearchNav } from '$lib/search';
  import { searchTypeFilterOptions, searchTypeFilterLabel } from '$lib/wasm';
  import MessageBanner from '$lib/components/MessageBanner.svelte';
  import { t } from '$lib/i18n/strings';
  import { relativeTime } from '$lib/value-format';
  import { resolveLocalized } from '$lib/i18n/localized';
  import { IDS } from '$lib/generated/uiIds';

  const TYPE_FILTER_ALL = 'all';
  const typeFilterTokens = searchTypeFilterOptions();

  let query = $state('');
  let barVisible = $state(true);
  let error = $state('');

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  let snap = $derived<any>($searchSnapshot);
  let searched = $derived(!!snap?.query);
  let typeFilter = $derived(snap?.type_filter ?? TYPE_FILTER_ALL);

  /** Mirror the manager's current error onto this page's `error-message`
   *  surface — called after every action settles, tui's `sync_error` shape.
   *  Reads the store directly (not the `snap` derived) so the read is
   *  synchronous with the just-applied `refreshSearch()`. */
  function syncError(): void {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const s = get(searchSnapshot) as any;
    error = s?.error ? resolveLocalized(s.error) : '';
  }

  async function handleSearch(): Promise<void> {
    const id = $identity;
    if (!id?.secretHex || !query.trim()) return;
    try {
      const m = await getSearchManager();
      await m.runQuery(query.trim(), typeFilter);
      refreshSearch();
      syncError();
    } catch (e) {
      error = e instanceof Error ? e.message : t.search_page.search_failed;
    }
  }

  /** `search-type-filter` change — re-fires the LIVE query buffer under the
   *  new token immediately (tui's `Action::SetTypeFilter`), not just the last
   *  committed query. A no-op on an empty buffer, the same guard `handleSearch`
   *  applies. */
  async function handleTypeFilterChange(token: string): Promise<void> {
    const id = $identity;
    if (!id?.secretHex || !query.trim()) return;
    try {
      const m = await getSearchManager();
      await m.runQuery(query.trim(), token);
      refreshSearch();
      syncError();
    } catch (e) {
      error = e instanceof Error ? e.message : t.search_page.search_failed;
    }
  }

  async function handleLoadMore(): Promise<void> {
    try {
      const m = await getSearchManager();
      await m.loadMore();
      refreshSearch();
      syncError();
    } catch (e) {
      error = e instanceof Error ? e.message : t.search_page.load_more_failed;
    }
  }

  /** `search-clear-button` — clears the QUERY BUFFER only. Nothing else
   *  touched; the loaded results are left as-is until the next fire (linux's
   *  `clear_btn`, tui's `Action::Clear`). */
  function handleClear(): void {
    query = '';
  }

  /** `search-cancel-button` — resets the page to pre-search and force-shows
   *  the bar. The manager owns the reset (it also drops any reply still in
   *  flight via its query generation); only the buffer and the bar are ours. */
  function handleCancel(): void {
    query = '';
    barVisible = true;
    getSearchManager()
      .then((m) => {
        m.cancel();
        refreshSearch();
        syncError();
      })
      .catch(() => {
        /* no manager yet — nothing to cancel */
      });
  }

  /** `search-result-item[i]` activation (`search.md` § User actions — "Open
   *  destination"). Maps a row's raw wasm-serialized `SearchNav` (externally
   *  tagged JSON: `{"Post": {"post_id": "..."}}` etc. — `serde`'s default enum
   *  representation, `libs/fauna-wasm/src/search.rs`'s `json_compatible`
   *  serializer) onto the target shape web can act on (`$lib/search`'s
   *  `PendingSearchNav`). `Contact`/`File` carry the row's RAW id — the
   *  id-space resolve (`uid_hash` → `card_id`; `(folder_id, path_hash)` →
   *  the Media row) happens on the destination page, not here (same split
   *  `post` already used before `resolvePost` existed: this function never
   *  does the lookup itself). Returns `null` only for a row with no target at
   *  all (`result.navigation` is `null`). Mirrors tui's `navigable()` —
   *  exhaustive-by-hand rather than a catch-all, so a target this function
   *  doesn't recognize renders inert instead of a dead click. */
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  function navigableTarget(nav: any): PendingSearchNav | null {
    if (!nav) return null;
    if (nav.Post) return { kind: 'post', postId: nav.Post.post_id };
    if (nav.Draft) {
      return nav.Draft.thread_id
        ? { kind: 'thread', threadId: nav.Draft.thread_id }
        : { kind: 'compose' };
    }
    if (nav.Mail) {
      // Lands the thread jump only — web has no wasm export for the message-
      // select half of `Mail`'s contract yet (no `selectThreadAndMessage`
      // face; tui's `focus_selected_message` has no web twin). Strictly
      // better than the inert row it replaces (search.md § State & data
      // shape — the same partial state tui itself shipped before its own
      // message-select landed).
      return { kind: 'thread', threadId: nav.Mail.thread_id };
    }
    if (nav.Contact) return { kind: 'contact', uidHash: nav.Contact.uid_hash };
    if (nav.File) {
      return { kind: 'file', folderId: nav.File.folder_id, pathHash: nav.File.path_hash };
    }
    return null;
  }

  /** Stash the target and navigate — the destination page's own `onMount`
   *  consumes it (`$lib/search`'s `consumePendingSearchNav`). */
  function openResult(target: PendingSearchNav): void {
    setPendingSearchNav(target);
    switch (target.kind) {
      case 'post':
        void goto('/app/feed');
        break;
      case 'thread':
      case 'compose':
        void goto('/app/conversations');
        break;
      case 'contact':
        void goto('/app/contacts');
        break;
      case 'file':
        void goto('/app/media');
        break;
    }
  }
</script>

<h1 data-testid={IDS.PAGE_HEADING}>{t.common.search}</h1>

<MessageBanner bind:error />

{#if !$identity}
  <p class="muted">{t.search_page.sign_in_prompt}</p>
{:else}
  <div class="search-controls">
    <button data-testid={IDS.SEARCH_TOGGLE_BUTTON} class="btn toggle-btn" onclick={() => { barVisible = !barVisible; }} title={barVisible ? t.search_page.hide_search_bar : t.search_page.show_search_bar}>{barVisible ? '▲' : '▼'} {t.common.search}</button>
    {#if searched}
      <button data-testid={IDS.SEARCH_CANCEL_BUTTON} class="btn" onclick={handleCancel}>{t.common.cancel}</button>
    {/if}
  </div>
  {#if barVisible}
  <div class="search-bar">
    <input
      data-testid={IDS.SEARCH_QUERY_FIELD}
      type="search"
      class="input"
      placeholder={t.search_page.placeholder}
      bind:value={query}
      onkeydown={(e) => { if (e.key === 'Enter') handleSearch(); }}
    />
    {#if query}
      <button data-testid={IDS.SEARCH_CLEAR_BUTTON} class="btn clear-btn" onclick={handleClear} title={t.search_page.clear}>&times;</button>
    {/if}
    <select
      data-testid={IDS.SEARCH_TYPE_FILTER}
      class="type-filter"
      value={typeFilter}
      onchange={(e) => handleTypeFilterChange((e.currentTarget as HTMLSelectElement).value)}
    >
      {#each typeFilterTokens as token (token)}
        <option value={token}>{resolveLocalized(searchTypeFilterLabel(token))}</option>
      {/each}
    </select>
    <button
      data-testid={IDS.SEARCH_SUBMIT_BUTTON}
      class="btn primary"
      onclick={handleSearch}
      disabled={snap?.in_flight || !query.trim()}
    >
      {snap?.in_flight ? t.common.searching : t.common.search}
    </button>
  </div>
  {/if}

  {#if searched}
    {#if snap.no_results}
      <p data-testid={IDS.SEARCH_NO_RESULTS} class="muted">{t.search_page.no_results} <strong>{snap.query}</strong>.</p>
    {:else if snap.results.length > 0}
      <p class="result-count muted">{snap.results.length} result{snap.results.length === 1 ? '' : 's'}</p>
      <div data-testid={IDS.SEARCH_RESULTS_VIEW} class="result-list">
        {#each snap.results as result (result.content_id)}
          {@const nav = navigableTarget(result.navigation)}
          {#if nav}
            <!-- Navigable — a real interactive control (static role/tabindex,
                 not conditional attributes, so the a11y linter can verify it). -->
            <div
              data-testid={IDS.SEARCH_RESULT_ITEM}
              class="result-card clickable"
              role="button"
              tabindex="0"
              onclick={() => openResult(nav)}
              onkeydown={(e) => { if (e.key === 'Enter') openResult(nav); }}
            >
              {@render resultCardBody(result)}
            </div>
          {:else}
            <!-- Inert — the row's target is `None`, or one this app can't act
                 on yet (ui.yaml search-result-item: "a row whose target has no
                 destination yet renders inert"). -->
            <div data-testid={IDS.SEARCH_RESULT_ITEM} class="result-card">
              {@render resultCardBody(result)}
            </div>
          {/if}
        {/each}
      </div>
      {#if snap.has_more}
        <button
          data-testid={IDS.SEARCH_LOAD_MORE_BUTTON}
          class="btn load-more-btn"
          onclick={handleLoadMore}
          disabled={snap.in_flight}
        >{snap.in_flight ? t.common.loading : t.common.load_more}</button>
      {/if}
    {/if}
  {/if}
{/if}

<!-- eslint-disable-next-line @typescript-eslint/no-explicit-any -->
{#snippet resultCardBody(result: any)}
  <div class="result-meta">
    <span class="badge">{resolveLocalized(result.badge)}</span>
    <span class="muted small">{relativeTime(result.timestamp)}</span>
  </div>
  <p class="snippet">{result.snippet}</p>
  <span class="content-id muted small mono">{result.content_id.slice(0, 16)}…</span>
{/snippet}

<style>
  h1 { margin-bottom: 1.25rem; }
  .muted { color: var(--text-muted); }
  .small { font-size: 0.8rem; }
  .mono { font-family: monospace; }

  .search-controls {
    display: flex;
    gap: 0.5rem;
    align-items: center;
    margin-bottom: 0.75rem;
  }
  .toggle-btn { font-size: 0.8rem; }

  .search-bar {
    display: flex;
    gap: 0.5rem;
    align-items: center;
    margin-bottom: 1.25rem;
  }

  .input {
    flex: 1;
    max-width: 560px;
    padding: 0.5rem 0.75rem;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg);
    color: var(--text);
    font-size: 0.9rem;
  }

  .btn {
    padding: 0.5rem 1rem;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg-surface);
    color: var(--text);
    cursor: pointer;
    font-size: 0.875rem;
  }
  .btn:hover { background: var(--bg-hover); }
  .btn:disabled { opacity: 0.5; cursor: not-allowed; }
  .btn.primary { background: var(--accent); color: #fff; border-color: var(--accent); }
  .btn.primary:hover { filter: brightness(1.1); }
  .type-filter { padding: 0.5rem; border: 1px solid var(--border); border-radius: 6px; background: var(--bg); color: var(--text); font-size: 0.875rem; }
  .clear-btn { padding: 0.3rem 0.5rem; font-size: 1rem; line-height: 1; }

  .result-count { margin-bottom: 0.75rem; }

  .result-list { display: flex; flex-direction: column; gap: 0.75rem; }

  .result-card {
    padding: 0.875rem 1rem;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--bg-surface);
  }
  .result-card.clickable { cursor: pointer; }
  .result-card.clickable:hover { background: var(--bg-hover); }

  .result-meta {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    margin-bottom: 0.4rem;
  }

  .badge {
    font-size: 0.7rem;
    padding: 0.125rem 0.5rem;
    border-radius: 999px;
    font-weight: 600;
    text-transform: uppercase;
    background: color-mix(in srgb, var(--accent) 20%, transparent);
    color: var(--accent);
  }

  .snippet {
    font-size: 0.875rem;
    margin: 0 0 0.375rem;
    line-height: 1.5;
  }

  .load-more-btn {
    display: block;
    margin: 1rem auto 0;
  }
</style>
