<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { connectionStatus, identity, onActorChange } from "$lib/store";
  import { registerActorScopedReset } from "$lib/actorScope";
  import { sharedRpcPort } from "$lib/rpc";
  import { ensureWasm, mediaSortLabel, offlineAffordance } from "$lib/wasm";
  import {
    createMediaMachine,
    shareLinkExpiryLabel,
    shareLinkStateLabel,
    syncedStateBadgeLabel,
    type MediaMachine,
  } from "$lib/wasm-media";
  import { makeOfflineGate } from "$lib/offline-gate";
  import type { MediaPageSnapshot, MediaItemSummary, FileVersionSummary } from "$lib/media-machine";
  import { resolveLocalized } from "$lib/i18n/localized";
  import MessageBanner from "$lib/components/MessageBanner.svelte";
  import { getDeviceId } from "$lib/device-id";
  import { t } from "$lib/i18n/strings";
  import { byteSize, relativeTime } from "$lib/value-format";
  import { toArrayBufferView } from "$lib/bytes";
  import { consumePendingSearchNav } from "$lib/search";
  import { IDS } from '$lib/generated/uiIds';

  // The all-media sentinel — the `media-folder-filter` value for "every readable
  // set" (the default view). Mirrors linux's `FILTER_ALL_VALUE` (mod.rs).
  const FILTER_ALL = "__all__";
  const SORT_KEYS = ["name", "size", "date"] as const;

  let machine: MediaMachine | null = null;
  let snap = $state<MediaPageSnapshot | null>(null);
  let ready = $state(false);
  // Page-level error-message, bound to MessageBanner (publishes to
  // window.__fauna_messages for the e2e; carries the shared error-message id).
  let mediaError = $state("");
  let timer: ReturnType<typeof setInterval> | undefined;
  // Unregisters the actor-scope reset + change handler installed in `onMount`.
  let cleanupActorScope: (() => void) | null = null;
  // `fauna.share.create` / `fauna.share.revoke` are OnlineOnly
  // (`fauna_protocol::offline_class`): the shared verdict greys their controls
  // while the nest is unreachable.
  const offlineGate = makeOfflineGate(offlineAffordance, connectionStatus.subscribe);

  // Upload glue (the only app-specific bit — the browser file picker).
  let uploadFiles: FileList | null = $state(null);
  let uploading = $state(false);

  // Thumbnail paint: resolved object-URLs keyed by item key (folder + path).
  // Fetched lazily on-appear only for items carrying a thumbnail_hash; the whole
  // fetch → content-address verify → owner-key decrypt is shared Rust
  // (`MediaMachine::fetch_thumbnail`), so the page only wraps the decoded JPEG
  // bytes in a blob URL. A null hash or a per-item failure keeps the placeholder —
  // one unreadable thumbnail never blanks the page or touches the error banner.
  let thumbUrls = $state<Record<string, string>>({});
  // Item keys already fetched (or in flight), so a re-render / poll never re-fetches.
  const thumbAttempted = new Set<string>();

  // The item key — the value the {#each} keys on, so it stays stable across a
  // re-sort / re-filter / refresh (the DOM node + its thumbnail persist).
  // NUL joins the two components because it is the one byte neither a folder
  // name nor a path can contain: with a plain space, {"a b", "c"} and
  // {"a", "b c"} share a key and cross-paint each other's thumbnail.
  function itemKey(item: MediaItemSummary): string {
    return item.folder + "\u0000" + item.path;
  }

  // Fetch + paint one item's thumbnail (once). Swallows per-item failures so the
  // placeholder stands; the owner BackupKey is derived from the seed inside the
  // wasm binding (mirrors uploadSelected — the raw key never enters JS).
  async function loadThumbnail(item: MediaItemSummary): Promise<void> {
    const secretHex = $identity?.secretHex;
    if (!machine || !secretHex || !item.thumbnail_hash) return;
    const key = itemKey(item);
    if (thumbAttempted.has(key)) return;
    thumbAttempted.add(key);
    try {
      const bytes: Uint8Array = await machine.fetchThumbnail(item.thumbnail_hash, secretHex);
      thumbUrls[key] = URL.createObjectURL(
        new Blob([toArrayBufferView(bytes)], { type: "image/jpeg" }),
      );
    } catch {
      // Per-item failure — keep the placeholder; never surface on the page banner.
    }
  }

  // Svelte action: lazily load the thumbnail when the tile scrolls into view
  // (on-appear), matching the other apps' realized-row fetch. Re-attempts on an
  // `update` only after the tile has been seen — so a thumbnail_hash that appears
  // mid-session (a peer's producer backfill picked up by the 15s poll) still paints.
  function thumbnailLoader(node: HTMLElement, item: MediaItemSummary) {
    let current = item;
    let seen = false;
    const observer = new IntersectionObserver((entries) => {
      for (const entry of entries) {
        if (entry.isIntersecting) {
          seen = true;
          loadThumbnail(current);
          observer.unobserve(node);
        }
      }
    });
    observer.observe(node);
    return {
      update(next: MediaItemSummary) {
        current = next;
        if (seen) loadThumbnail(current);
      },
      destroy() {
        observer.disconnect();
      },
    };
  }

  // Re-read the whole renderable page off the machine on every tick.
  function applySnapshot(): void {
    if (!machine) return;
    const raw = machine.snapshotJson();
    snap = raw ? (JSON.parse(raw) as MediaPageSnapshot) : null;
    mediaError = snap?.error ? resolveLocalized(snap.error) : "";
  }

  /** Drop this page's actor-scoped state (`$lib/actorScope`).
   *
   *  `thumbUrls` holds blob URLs over thumbnail bytes decrypted under THIS actor's
   *  owner key, so under the switch/sign-out isolation contract
   *  (account-scoping.md) the next account may not reach them at all — revoking is
   *  what actually releases the decrypted bytes, since an un-revoked URL stays
   *  fetchable from the document. `thumbAttempted` is the fetch-once guard, so a
   *  stale entry would suppress the incoming actor's fetch for the same item key
   *  permanently (the feed page's bug).
   *
   *  The poll timer is cleared here rather than in the rebuild: it captured the
   *  outgoing actor's secret, so every tick after a switch would refresh as the
   *  wrong actor. */
  function resetActorScopedMedia(): void {
    if (timer) { clearInterval(timer); timer = undefined; }
    for (const url of Object.values(thumbUrls)) URL.revokeObjectURL(url);
    thumbUrls = {};
    thumbAttempted.clear();
    machine = null;
    snap = null;
  }

  onMount(() => {
    identity.init();

    const offReset = registerActorScopedReset(resetActorScopedMedia);
    // Rebuild for whoever is signed in, now and on every later actor change —
    // this page's `machine` is built around one actor's id + auth-token getter,
    // and an in-app switch is a same-route `goto` that does NOT remount the
    // component, so a one-shot `onMount` build would keep serving (and polling
    // as) the outgoing actor. See `$lib/actorScope`.
    const offActor = onActorChange(async (id) => {
      try {
        await ensureWasm();
        machine = await createMediaMachine(
          { onChanged: () => applySnapshot() },
          // The SPA singleton's socket, lent to the media chunk — one
          // WebSocket per actor; no chunk-private connection to fall asleep
          // in its own backoff after a nest restart.
          await sharedRpcPort(id.secretHex),
          // Write-side label custody for the delete/restore gestures (S8 D2):
          // the same per-actor owner key `refresh()`/`uploadSelected` derive.
          // `createMediaMachine` injects it the moment the machine exists, so
          // this page cannot hold a keyless one — mirrors linux/tui/android
          // (apps/fauna-linux/src/views/media/mod.rs), which inject at their own
          // build sites.
          id.secretHex,
        );
        ready = true;
        await machine.refresh(id.secretHex);
        applySnapshot();
        // A `SearchNav::File` deep-link left by the Search page (`$lib/search`'s
        // `consumePendingSearchNav`) — after the refresh above so the raw
        // aggregate `locateFileJson` reads over is already loaded.
        const pendingNav = consumePendingSearchNav();
        if (pendingNav?.kind === 'file') {
          openFileByLocation(pendingNav.folderId, pendingNav.pathHash);
        }
        // Cross-set liveness (source-online) drifts; a light poll keeps it fresh,
        // matching the Devices page's 15s cadence.
        timer = setInterval(() => { machine?.refresh(id.secretHex); }, 15000);
      } catch (e: unknown) {
        mediaError = e instanceof Error ? e.message : String(e);
        ready = true;
      }
    });

    // Signed out: nothing will build, so release the page rather than leaving the
    // skeleton up forever (`onActorChange` does not fire without an identity).
    if (!$identity) ready = true;

    cleanupActorScope = () => { offReset(); offActor(); };
  });

  onDestroy(() => {
    cleanupActorScope?.();
    if (timer) clearInterval(timer);
    // Release the painted thumbnails' object-URLs so they don't leak.
    for (const url of Object.values(thumbUrls)) URL.revokeObjectURL(url);
  });

  // ── View controls (shared-Rust setters; each also fires onChanged) ──────────
  function toggleView(): void {
    if (!machine || !snap) return;
    machine.setViewGrid(!snap.view_grid);
    applySnapshot();
  }
  function changeSort(value: string): void {
    machine?.setSort(value);
    applySnapshot();
  }
  // A followed scope's minted value routes to the async on-demand listing fetch
  // (`ui/media.md` § Followed public folders); a set name routes to the ordinary
  // filter. The MACHINE says which values are followed — the SPA asks its own
  // snapshot rather than parsing the value, because the value is opaque by
  // contract and its shape is the machine's business (the same lookup tui's
  // media page makes before minting `Op::SelectFollowedScope`).
  //
  // Selecting is what FETCHES, once, on demand: a followed folder's rows live on
  // its home nest and the follower's nest keeps no copy, so eager aggregation
  // would put one relayed cross-nest fetch per follow on every Media refresh.
  function changeFilter(value: string): void {
    if (!machine) return;
    if ((snap?.followed ?? []).some((f) => f.value === value)) {
      // The machine notifies on settle; `applySnapshot` runs from the observer.
      void machine.selectFollowedScope(value);
      return;
    }
    machine.setFilter(value === FILTER_ALL ? undefined : value);
    applySnapshot();
  }

  // ── Upload into the selected set (shared "which set" + no-set policy) ────────
  async function handleUpload(): Promise<void> {
    const id = $identity;
    if (!machine || !id) return;
    if (!uploadFiles || uploadFiles.length === 0) {
      // Never a silent no-op, which reads as a dead button (media.md § User
      // actions). Surfaced bare, not as an upload failure — no upload began —
      // the same guard linux, windows and tui carry.
      mediaError = t.media.file_required;
      return;
    }
    uploading = true;
    try {
      const deviceId = getDeviceId(id.actorId);
      for (let i = 0; i < uploadFiles.length; i++) {
        const file = uploadFiles[i];
        const bytes = new Uint8Array(await file.arrayBuffer());
        // The shared machine resolves the target set + applies the no-set policy
        // (media.error_no_set) and refuses a metadata-only folder; the owner
        // BackupKey is derived from the seed in the wasm binding.
        await machine.uploadSelected(deviceId, file.name, bytes, id.secretHex);
      }
      uploadFiles = null;
      const input = document.querySelector(`[data-testid="${IDS.FILE_UPLOAD}"]`) as HTMLInputElement | null;
      if (input) input.value = "";
      applySnapshot();
    } finally {
      uploading = false;
    }
  }

  // ── Per-item detail (`media-item-detail`) + version history ────────────────
  // `media-item` tap/open → the detail surface (media.md § Element IDs), listing
  // `MediaMachine::file_versions` oldest→newest and driving restore through the
  // shared `restore_version` gesture behind the lightweight confirm modal
  // (restore is reversible — it appends a new version — file-sync.md § Restore).
  // Mirrors the linux reference (`views/media/detail.rs`): all semantics live in
  // shared Rust, this is pure renderer + the device-id glue.
  let detailItem = $state<MediaItemSummary | null>(null); // null = modal closed
  let detailVersions = $state<FileVersionSummary[]>([]);
  let detailLoading = $state(false);
  // Per-item load error — the wasm `fileVersions` rejection is a plain string,
  // not a LocalizedText, and (like fetchThumbnail) never touches the page banner.
  let detailError = $state("");
  let restoreTarget = $state<FileVersionSummary | null>(null); // null = confirm closed
  let restoring = $state(false);
  let deleteArmed = $state(false); // true = confirm open
  let deleting = $state(false);
  // The recovery browse (`file-versions.md` § Retention (3), apps row 323) —
  // ON re-lists with `include_pruned`, so soft-pruned rows appear with their
  // badge + undelete button. Reset on every open (mirrors linux's DetailCtx).
  let showPruned = $state(false);

  // A followed item has NO version history to load — the public plane is
  // head-only by the follow's v1 non-goals — so the detail opens straight to
  // its metadata rather than firing a `fileVersions` query that would ride the
  // name-keyed custody path a follower structurally cannot use
  // (`ui/media.md` § Followed public folders + architectural rule 6). tui makes
  // the same call at its own detail-open (`followed_scope_value.is_some()`).
  function openDetail(item: MediaItemSummary): void {
    detailItem = item;
    detailVersions = [];
    detailError = "";
    showPruned = false;
    if (snap?.followed_scope) return;
    loadVersions(item);
  }

  /** `SearchNav::File` deep-link (`../search/+page.svelte`'s `openResult`, via
   *  `$lib/search`'s pending-nav handoff) — resolves the row's durable
   *  `(folder_id, path_hash)` pair to the item this page's rows are keyed on
   *  (`MediaMachine::locateFileJson`), since a rename moves the rendered
   *  `name`/`path` but not the pair (`ui/search.md` § Where logic lives →
   *  *Result navigation (deep link)*). Opens the same `media-item-detail` a
   *  normal `media-item` click does. `undefined` = deleted, renamed, or in a
   *  set this actor can't see — surfaces the shared "not found" banner rather
   *  than a blank page. Runs over the raw aggregate `refresh()` already
   *  loaded, so no second fetch. */
  function openFileByLocation(folderId: number, pathHash: string): void {
    if (!machine) return;
    const raw = machine.locateFileJson(folderId, pathHash);
    if (!raw) {
      mediaError = t.common.not_found;
      return;
    }
    openDetail(JSON.parse(raw) as MediaItemSummary);
  }

  function closeDetail(): void {
    detailItem = null;
    restoreTarget = null;
    deleteArmed = false;
    // The create surface lives inside the detail, so it closes with it.
    if (snap?.share_create) machine?.closeShareCreate();
  }

  // `forItem` defaults to the currently-open item (the confirmRestore reload);
  // openDetail passes it explicitly. Guarded by reference against `detailItem`
  // after the await: a quick close-then-reopen (or reopen-a-different-item)
  // while this fetch is in flight must not let its stale response overwrite
  // the NEW item's rows (or clear its loading state out from under it).
  async function loadVersions(forItem?: MediaItemSummary): Promise<void> {
    const item = forItem ?? detailItem;
    if (!machine || !item) return;
    detailLoading = true;
    detailError = "";
    try {
      const raw: string = await machine.fileVersions(item.folder, item.path, showPruned);
      if (detailItem !== item) return;
      detailVersions = JSON.parse(raw) as FileVersionSummary[];
    } catch (e: unknown) {
      if (detailItem !== item) return;
      const message = e instanceof Error ? e.message : String(e);
      detailError = t.media.versions_error({ message });
    } finally {
      if (detailItem === item) detailLoading = false;
    }
  }

  function toggleShowPruned(): void {
    showPruned = !showPruned;
    loadVersions();
  }

  let undeleting = $state(false);

  /** `file-version-undelete-button` — restore a soft-pruned version to the
   *  listable population, then re-list (keeping the browse's toggle). A
   *  failure surfaces on this surface's own `detailError`, mirroring linux's
   *  status line rather than the page banner (per-item query). */
  async function undeleteVersion(version: FileVersionSummary): Promise<void> {
    if (!machine || !detailItem) return;
    undeleting = true;
    try {
      await machine.undeleteVersion(detailItem.path, version.version_num);
      await loadVersions();
    } catch (e: unknown) {
      const message = e instanceof Error ? e.message : String(e);
      detailError = t.media.error_undelete({ message });
    } finally {
      undeleting = false;
    }
  }

  function openRestoreConfirm(version: FileVersionSummary): void {
    restoreTarget = version;
  }

  function cancelRestore(): void {
    restoreTarget = null;
  }

  // Restore always resolves — a failure sets the shared machine's page-level
  // error (surfaced via the existing MessageBanner through applySnapshot, same
  // as upload/delete) rather than rejecting; a success appends a new head
  // version, so the open detail reloads its rows either way (matches linux).
  async function confirmRestore(): Promise<void> {
    const id = $identity;
    if (!machine || !detailItem || !restoreTarget || !id) return;
    restoring = true;
    try {
      const deviceId = getDeviceId(id.actorId);
      await machine.restoreVersion(
        detailItem.folder,
        deviceId,
        detailItem.path,
        JSON.stringify(restoreTarget),
      );
      restoreTarget = null;
      applySnapshot();
      await loadVersions();
    } finally {
      restoring = false;
    }
  }

  // ── Delete the opened file (`media-delete-button` → `media-delete-confirm-modal`,
  //    media.md § Element IDs, user-approved 2026-07-16) ──────────────────────
  // Own-item detail only (gated the same way as the version history above): a
  // followed item's public plane is head-only, so there is nothing to delete.

  function openDeleteConfirm(): void {
    deleteArmed = true;
  }

  function cancelDelete(): void {
    deleteArmed = false;
  }

  /** Single confirm (no typed id): the shared `MediaMachine::delete` records a
   *  tombstone (`fauna.sync.delete_member`), which leaves the historical
   *  version rows and is not forwarded to backup destinations — the
   *  heavier typed-id immediate-delete ceremony would be miscalibrated to the
   *  risk here (`file-sync.md` § File Versions). The shared observer's
   *  `onChanged` already re-ran `applySnapshot()` by the time this awaits, so
   *  `snap?.error` reflects the outcome: close the detail surface (its subject
   *  is gone) on success, keep it open on failure — a deleted file has no
   *  reachable restore path, unlike restore's reload-in-place. */
  async function confirmDelete(): Promise<void> {
    const id = $identity;
    if (!machine || !detailItem || !deleteArmed || !id) return;
    deleteArmed = false;
    deleting = true;
    try {
      const deviceId = getDeviceId(id.actorId);
      await machine.delete(detailItem.folder, deviceId, detailItem.path);
      applySnapshot();
      if (!snap?.error) {
        detailItem = null;
      }
    } finally {
      deleting = false;
    }
  }

  // ── Download the opened file (`media-item-detail-download-button`, user-approved
  //    2026-09-25 — media.md § Element IDs) ────────────────────────────────────
  // Rows are oldest→newest, so the last row is the current file — the same pick
  // tui's `external_open_op` makes. `null` until the version rows have loaded,
  // and the button waits for it (a painted trigger is always actionable; a
  // followed item needs no row — the machine resolves its head by path).
  let downloading = $state(false);
  const downloadableVersion = $derived<FileVersionSummary | null>(
    detailVersions.length ? detailVersions[detailVersions.length - 1] : null,
  );

  /** One shared query, two arms: a followed item is the keyless scope download
   *  (`downloadFollowed`, never the custody path — `ui/media.md` § Followed
   *  public folders + architectural rule 6); any other item is the shared
   *  `MediaMachine::download_file` walk keyed by the latest version row, a
   *  shared set opening under the content keys the machine's resolver reads
   *  from this actor's custody, an owner-only set under the owner key derived
   *  in wasm. Only the save below is web-specific (the backups single-file
   *  download's idiom): a browser download named after the file. The user
   *  asked for this action, so a failure lands on the page banner — unlike
   *  the per-item thumbnail/version loads. */
  async function downloadDetailItem(): Promise<void> {
    const id = $identity;
    const item = detailItem;
    if (!machine || !item || !id || downloading) return;
    downloading = true;
    try {
      let bytes: Uint8Array;
      if (snap?.followed_scope) {
        bytes = (await machine.downloadFollowed(snap.followed_scope.value, item.path)) as Uint8Array;
      } else {
        const latest = downloadableVersion;
        if (!latest) return;
        bytes = (await machine.download(
          latest.manifest_hash,
          latest.content_key_version,
          item.folder,
          item.path,
          id.secretHex,
        )) as Uint8Array;
      }
      const url = URL.createObjectURL(new Blob([toArrayBufferView(bytes)]));
      try {
        const a = document.createElement("a");
        a.href = url;
        a.download = item.name;
        a.click();
      } finally {
        URL.revokeObjectURL(url);
      }
    } catch (e: unknown) {
      const message = e instanceof Error ? e.message : String(e);
      mediaError = t.media.error_download({ message });
    } finally {
      downloading = false;
    }
  }

  // ── Share links (`share-links.md` § Flows; the `share-link-*` family) ───────
  // Pure renderer: eligibility, the step state, the reveal-after-registration
  // rule, row states and errors all live in the shared Media machine
  // (`MediaPageSnapshot.share_create` / `.share_links`); each gesture fires
  // `onChanged`, which re-reads the snapshot. Failures land in the page's
  // `error-message` and the surface stays open. Mirrors tui's
  // `share_create_elements` / `share_list_elements`.
  function openShareCreate(item: MediaItemSummary): void {
    machine?.openShareCreate(item.folder, item.path);
  }

  async function copyShareUrl(url: string): Promise<void> {
    try {
      await navigator.clipboard.writeText(url);
    } catch {
      // Clipboard refused (no focus / permission) — the URL stays on screen.
    }
  }

  function expiryOptionLabel(value: string): string {
    const label = shareLinkExpiryLabel(value);
    return label ? resolveLocalized(label) : value;
  }

  function shareStateLabel(state: string): string {
    const label = shareLinkStateLabel(state);
    return label ? resolveLocalized(label) : state;
  }

  function shareExpiresText(expiresAt: number): string {
    return t.share_link.expires({ date: new Date(expiresAt * 1000).toLocaleDateString() });
  }

  // The row whose revoke confirm is armed — its name goes in the confirm body.
  const revokeRow = $derived(
    snap?.share_links.revoke_confirm
      ? snap.share_links.rows.find((r) => r.token_id === snap?.share_links.revoke_confirm) ?? null
      : null,
  );

  // ── Labels (localized display; the option/value contract stays the wire form) ─
  // Via the shared `fauna_core::format::media_sort_label` map (priority #2) —
  // the same one linux/tui already consume.
  function sortLabel(key: string): string {
    return resolveLocalized(mediaSortLabel(key));
  }
</script>

<h1 data-testid={IDS.PAGE_HEADING}>{t.media.title}</h1>

<MessageBanner bind:error={mediaError} />

{#if !ready}
  <p class="muted">{t.common.loading}</p>
{:else if !$identity}
  <p class="muted">{t.common.identity_required}</p>
{:else}
  <section class="toolbar" aria-label={t.media.title}>
    <button
      data-testid={IDS.MEDIA_VIEW_TOGGLE}
      class="btn"
      onclick={toggleView}
      title={t.media.view_grid + " / " + t.media.view_list}
    >
      {snap?.view_grid ? t.media.view_grid : t.media.view_list}
    </button>

    <label class="ctrl">
      <select
        data-testid={IDS.MEDIA_SORT_SELECT}
        value={snap?.sort ?? "name"}
        onchange={(e) => changeSort(e.currentTarget.value)}
      >
        {#each SORT_KEYS as key}
          <option value={key}>{sortLabel(key)}</option>
        {/each}
      </select>
    </label>

    <label class="ctrl">
      <select
        data-testid={IDS.MEDIA_FOLDER_FILTER}
        value={snap?.filter ?? FILTER_ALL}
        onchange={(e) => changeFilter(e.currentTarget.value)}
      >
        <option value={FILTER_ALL}>{t.media.filter_all}</option>
        {#each snap?.folders ?? [] as name}
          <option value={name}>{name}</option>
        {/each}
        <!-- The followed browse scopes, AFTER the own-set options
             (`ui/media.md` § Followed public folders). Both halves are
             shared-Rust-minted: `value` is an opaque stable string guaranteed
             disjoint from every set name, `label` already carries the owner
             disambiguator that tells a follow apart from a same-named set of
             the user's own. Render them; never compose or parse either. -->
        {#each snap?.followed ?? [] as scope}
          <option value={scope.value}>{scope.label}</option>
        {/each}
      </select>
    </label>

    <!-- `share-link-list-button` — the caller's share links, page-level
         (`share-links.md` § Flows → List). -->
    <button
      data-testid={IDS.SHARE_LINK_LIST_BUTTON}
      class="btn"
      onclick={() => machine?.openShareLinks()}
    >{t.share_link.list_button}</button>

    <!-- A followed browse scope is READ-ONLY, structurally: the upload
         affordance is ABSENT entirely rather than painted-but-inert, because a
         follow never enters `known_folders` and so can never be an upload
         target (`ui/media.md` § Followed public folders; tui gates its own
         upload gesture on the same `followed_scope.is_none()`). -->
    {#if !snap?.followed_scope}
      <div class="upload-form">
        <input
          data-testid={IDS.FILE_UPLOAD}
          type="file"
          multiple
          onchange={(e) => { uploadFiles = (e.target as HTMLInputElement).files; }}
        />
        <button
          data-testid={IDS.UPLOAD_BUTTON}
          class="btn primary"
          onclick={handleUpload}
          disabled={uploading}
        >
          {uploading ? t.media.uploading({ progress: "0" }) : t.media.upload}
        </button>
      </div>
    {/if}
  </section>

  {#if snap?.loaded && snap.items.length === 0}
    <p class="placeholder" data-testid={IDS.MEDIA_EMPTY_STATE}>{t.media.no_media_yet}</p>
  {:else if snap && snap.items.length > 0}
    <div class="media-list" class:grid={snap.view_grid}>
      {#each snap.items as item (itemKey(item))}
        <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
        <div
          class="media-item"
          data-testid={IDS.MEDIA_ITEM}
          role="button"
          tabindex="0"
          onclick={() => openDetail(item)}
          onkeydown={(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); openDetail(item); } }}
        >
          <div class="thumb" data-testid={IDS.MEDIA_THUMBNAIL} aria-hidden="true" use:thumbnailLoader={item}>
            {#if thumbUrls[itemKey(item)]}
              <img class="thumb-img" src={thumbUrls[itemKey(item)]} alt="" />
            {/if}
          </div>
          <span class="name" data-testid={IDS.MEDIA_ITEM_NAME}>{item.name}</span>
          <span class="size" data-testid={IDS.MEDIA_ITEM_SIZE}>{byteSize(item.size_bytes)}</span>
          <span class="date" data-testid={IDS.MEDIA_ITEM_DATE}>{relativeTime(item.updated_at * 1000)}</span>
          <span
            class="source-status"
            class:online={item.source_online}
            class:offline={!item.source_online}
            data-testid={IDS.MEDIA_SOURCE_STATUS}
          >
            {item.source_online ? t.media.source_online : t.media.source_offline}
          </span>
          <!-- This file's own presence (file-sync.md § Per-file sync-status
               display), distinct from the source folder's online/offline
               liveness above. Web is a control-plane client — `fauna.sync.files`
               carries no per-file status and there is no local sync engine
               behind this page — so it renders only the `Synced` state, the
               class split the goal doc sanctions. The label comes from the
               shared `sync_display_state_label`, never a hand-written string. -->
          <span class="sync-state" data-testid={IDS.SYNC_STATE_BADGE}>
            {resolveLocalized(syncedStateBadgeLabel())}
          </span>
        </div>
      {/each}
    </div>
  {/if}
{/if}

{#if detailItem}
  <!-- Per-item detail (`media-item-detail`, media.md § Element IDs) — the
       `file-version-history` component listing MediaMachine::file_versions
       oldest→newest, each row's restore gated by the lightweight confirm
       modal below (restore is reversible; file-sync.md § Restore). -->
  <div class="modal-backdrop" role="presentation" onclick={closeDetail}>
    <div
      data-testid={IDS.MEDIA_ITEM_DETAIL}
      class="modal"
      role="dialog"
      aria-modal="true"
      onclick={(e) => e.stopPropagation()}
    >
      <div class="detail-header">
        <h2 data-testid={IDS.MEDIA_ITEM_DETAIL_NAME} class="detail-name">{detailItem.name}</h2>
      </div>

      <!-- Everything below is withheld for a followed item: no version list, no
           show-pruned toggle, no restore and no undelete. The public plane is
           head-only, so there are no version rows to offer and no write to
           offer them for (`ui/media.md` § Followed public folders — "the item
           detail for a followed item offers download alone"); the download it
           does offer is `media-item-detail-download-button` in the actions
           row below, routed to the keyless scope download. -->
      {#if !snap?.followed_scope}
        <h3 class="versions-heading">{t.media.versions_title}</h3>

        <label class="ctrl">
          <input
            data-testid={IDS.FILE_VERSION_SHOW_PRUNED_TOGGLE}
            type="checkbox"
            checked={showPruned}
            onchange={toggleShowPruned}
          />
          {t.media.versions_show_pruned}
        </label>

        {#if detailLoading}
          <p class="muted">{t.media.versions_loading}</p>
        {:else if detailError}
          <p class="warn">{detailError}</p>
        {:else}
          <div data-testid={IDS.FILE_VERSION_LIST} class="version-list">
            {#each detailVersions as version (version.version_num)}
              <div data-testid={IDS.FILE_VERSION_ITEM} class="version-item">
                <span data-testid={IDS.FILE_VERSION_TIMESTAMP} class="version-time"
                  >{relativeTime(version.created_at)}</span
                >
                <span data-testid={IDS.FILE_VERSION_SIZE} class="version-size muted"
                  >{byteSize(version.size_bytes)}</span
                >
                <span data-testid={IDS.FILE_VERSION_AUTHOR} class="version-author muted"
                  >{t.media.version_author({ author: version.author_display })}</span
                >
                {#if version.pruned}
                  <span data-testid={IDS.FILE_VERSION_PRUNED_BADGE} class="version-pruned muted"
                    >{t.media.version_pruned_badge}</span
                  >
                  <button
                    data-testid={IDS.FILE_VERSION_UNDELETE_BUTTON}
                    class="btn"
                    disabled={undeleting}
                    onclick={() => undeleteVersion(version)}
                  >{t.media.version_undelete}</button>
                {/if}
                <button
                  data-testid={IDS.FILE_VERSION_RESTORE_BUTTON}
                  class="btn"
                  onclick={() => openRestoreConfirm(version)}
                >{t.media.version_restore}</button>
              </div>
            {/each}
          </div>
        {/if}
      {/if}

      <!-- The share-link create surface (`share-link-create-modal`), inside the
           detail it was opened from. The URL and its Copy appear ONLY once the
           machine reports the registration succeeded. -->
      {#if snap?.share_create}
        <section data-testid={IDS.SHARE_LINK_CREATE_MODAL} class="share-create" aria-label={t.share_link.button}>
          <h3>{t.share_link.create_title({ name: snap.share_create.name })}</h3>
          <p class="muted">{t.share_link.create_body}</p>
          {#if snap.share_create.url}
            <div class="share-url-row">
              <code data-testid={IDS.SHARE_LINK_URL} class="share-url">{snap.share_create.url}</code>
              <button
                data-testid={IDS.SHARE_LINK_COPY_BUTTON}
                class="btn"
                onclick={() => snap?.share_create?.url && copyShareUrl(snap.share_create.url)}
              >{t.share_link.copy}</button>
            </div>
          {:else}
            <label class="ctrl">
              {t.share_link.expiry_label}
              <select
                data-testid={IDS.SHARE_LINK_EXPIRY_SELECT}
                value={snap.share_create.expiry}
                disabled={snap.share_create.busy}
                onchange={(e) => machine?.setShareExpiry(e.currentTarget.value)}
              >
                {#each snap.share_expiry_options as value}
                  <option {value}>{expiryOptionLabel(value)}</option>
                {/each}
              </select>
            </label>
          {/if}
          <div class="modal-actions">
            <button
              data-testid={IDS.SHARE_LINK_CANCEL_BUTTON}
              class="btn"
              onclick={() => machine?.closeShareCreate()}
            >{snap.share_create.url ? t.share_link.close : t.share_link.cancel}</button>
            {#if !snap.share_create.url}
              <button
                data-testid={IDS.SHARE_LINK_CREATE_BUTTON}
                class="btn primary"
                use:offlineGate={{ kind: 'fauna.share.create', disabled: snap.share_create.busy }}
                onclick={() => machine?.createShareLink()}
              >{snap.share_create.busy ? t.share_link.creating : t.share_link.create}</button>
            {/if}
          </div>
        </section>
      {/if}

      <div class="modal-actions">
        <!-- Painted only once it is actionable (a manifest known, or a followed
             scope), mirroring tui's `has_manifest` gate — never painted-but-inert. -->
        {#if snap?.followed_scope || downloadableVersion}
          <button
            data-testid={IDS.MEDIA_ITEM_DETAIL_DOWNLOAD_BUTTON}
            class="btn"
            disabled={downloading}
            onclick={downloadDetailItem}
          >{t.media.download}</button>
        {/if}
        <button data-testid={IDS.MEDIA_ITEM_DETAIL_CLOSE_BUTTON} class="btn" onclick={closeDetail}
          >{t.media.detail_close}</button
        >
        <!-- Present ONLY on an eligible file (a public-audience folder) —
             absent otherwise, never inert (`share-links.md` § Which files can
             be linked); the verdict is the machine's. -->
        {#if !snap?.followed_scope && detailItem.share_link_eligible && !snap?.share_create}
          <button
            data-testid={IDS.SHARE_LINK_BUTTON}
            class="btn"
            onclick={() => detailItem && openShareCreate(detailItem)}
          >{t.share_link.button}</button>
        {/if}
        {#if !snap?.followed_scope}
          <button
            data-testid={IDS.MEDIA_DELETE_BUTTON}
            class="btn"
            onclick={openDeleteConfirm}
          >{t.media.file_detail.delete_file}</button>
        {/if}
      </div>
    </div>
  </div>
{/if}

{#if deleteArmed && detailItem}
  <!-- Lightweight delete confirm (`media-delete-confirm-modal`) — a SINGLE
       confirm (no typed id): the body names the file and says it cannot be
       undone, which is honest — a deleted file has no reachable restore path
       (media.md § Element IDs). Mirrors the restore confirm below. -->
  <div class="modal-backdrop" role="presentation" onclick={cancelDelete}>
    <div
      data-testid={IDS.MEDIA_DELETE_CONFIRM_MODAL}
      class="modal"
      role="dialog"
      aria-modal="true"
      onclick={(e) => e.stopPropagation()}
    >
      <h2>{t.media.file_detail.delete_confirm_title}</h2>
      <p>{t.media.file_detail.delete_confirm({ name: detailItem.name })}</p>
      <div class="modal-actions">
        <button
          data-testid={IDS.MEDIA_DELETE_CANCEL_BUTTON}
          class="btn"
          onclick={cancelDelete}
          disabled={deleting}
        >{t.common.cancel}</button>
        <button
          data-testid={IDS.MEDIA_DELETE_CONFIRM_BUTTON}
          class="btn primary"
          onclick={confirmDelete}
          disabled={deleting}
        >{t.media.file_detail.delete_confirm_button}</button>
      </div>
    </div>
  </div>
{/if}

{#if restoreTarget}
  <!-- Lightweight restore confirm (`file-version-restore-confirm-modal`) — a
       single confirm, not the backups immediate-delete ceremony, since restore
       is reversible (media.md § Element IDs). -->
  <div class="modal-backdrop" role="presentation" onclick={cancelRestore}>
    <div
      data-testid={IDS.FILE_VERSION_RESTORE_CONFIRM_MODAL}
      class="modal"
      role="dialog"
      aria-modal="true"
      onclick={(e) => e.stopPropagation()}
    >
      <h2>{t.media.restore_confirm_title}</h2>
      <p>{t.media.restore_confirm_body}</p>
      <div class="modal-actions">
        <button
          data-testid={IDS.FILE_VERSION_RESTORE_CANCEL_BUTTON}
          class="btn"
          onclick={cancelRestore}
          disabled={restoring}
        >{t.media.restore_cancel}</button>
        <button
          data-testid={IDS.FILE_VERSION_RESTORE_CONFIRM_BUTTON}
          class="btn primary"
          onclick={confirmRestore}
          disabled={restoring}
        >{t.media.restore_confirm}</button>
      </div>
    </div>
  </div>
{/if}

{#if snap?.share_links.open}
  <!-- The share-link list (`share-link-list`, `share-links.md` § Flows →
       List / Revoke). Three states off one loaded bit: rows / empty state /
       neither = loading. -->
  <div class="modal-backdrop" role="presentation" onclick={() => machine?.closeShareLinks()}>
    <div
      data-testid={IDS.SHARE_LINK_LIST}
      class="modal"
      role="dialog"
      aria-modal="true"
      onclick={(e) => e.stopPropagation()}
    >
      <h2>{t.share_link.list_title}</h2>
      {#if !snap.share_links.loaded}
        <p class="muted">{t.share_link.list_loading}</p>
      {:else if snap.share_links.rows.length === 0}
        <p class="placeholder" data-testid={IDS.SHARE_LINK_EMPTY_STATE}>{t.share_link.empty}</p>
      {:else}
        <div class="share-list">
          {#each snap.share_links.rows as row (row.token_id)}
            <div data-testid={IDS.SHARE_LINK_ITEM} class="share-item">
              <span data-testid={IDS.SHARE_LINK_ITEM_NAME} class="name">{row.name}</span>
              <span data-testid={IDS.SHARE_LINK_ITEM_EXPIRES} class="muted">{shareExpiresText(row.expires_at)}</span>
              <span data-testid={IDS.SHARE_LINK_ITEM_STATE} data-state={row.state} class="share-state"
                >{shareStateLabel(row.state)}</span
              >
              {#if row.url}
                <button
                  data-testid={IDS.SHARE_LINK_ITEM_COPY_BUTTON}
                  class="btn"
                  onclick={() => row.url && copyShareUrl(row.url)}
                >{t.share_link.copy}</button>
              {/if}
              {#if row.state === "active"}
                <button
                  data-testid={IDS.SHARE_LINK_REVOKE_BUTTON}
                  class="btn"
                  onclick={() => machine?.armShareRevoke(row.token_id)}
                >{t.share_link.revoke}</button>
              {/if}
            </div>
          {/each}
        </div>
      {/if}
      <div class="modal-actions">
        <button
          data-testid={IDS.SHARE_LINK_LIST_CLOSE_BUTTON}
          class="btn"
          onclick={() => machine?.closeShareLinks()}
        >{t.share_link.close}</button>
      </div>
    </div>
  </div>
{/if}

{#if revokeRow}
  <!-- Single revoke confirm (`share-link-revoke-confirm-modal`) — the
       media-delete confirm on this page is the precedent. No un-revoke. -->
  <div class="modal-backdrop" role="presentation" onclick={() => machine?.cancelShareRevoke()}>
    <div
      data-testid={IDS.SHARE_LINK_REVOKE_CONFIRM_MODAL}
      class="modal"
      role="dialog"
      aria-modal="true"
      onclick={(e) => e.stopPropagation()}
    >
      <h2>{t.share_link.revoke_confirm_title}</h2>
      <p>{t.share_link.revoke_confirm_body({ name: revokeRow.name })}</p>
      <div class="modal-actions">
        <button
          data-testid={IDS.SHARE_LINK_REVOKE_CANCEL_BUTTON}
          class="btn"
          onclick={() => machine?.cancelShareRevoke()}
        >{t.share_link.cancel}</button>
        <button
          data-testid={IDS.SHARE_LINK_REVOKE_CONFIRM_BUTTON}
          class="btn primary"
          use:offlineGate={{ kind: 'fauna.share.revoke' }}
          onclick={() => machine?.confirmShareRevoke()}
        >{t.share_link.revoke_confirm}</button>
      </div>
    </div>
  </div>
{/if}

<style>
  .muted { color: var(--text-muted); }
  .placeholder { color: var(--text-muted); margin-top: 1rem; }
  .toolbar {
    display: flex;
    gap: 0.75rem;
    align-items: center;
    flex-wrap: wrap;
    margin: 1rem 0;
  }
  .ctrl select {
    padding: 0.4rem 0.5rem;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg-surface);
    color: var(--text);
  }
  .upload-form { display: flex; gap: 0.5rem; align-items: center; margin-left: auto; flex-wrap: wrap; }
  .btn {
    padding: 0.5rem 1rem; border: 1px solid var(--border); border-radius: 6px;
    background: var(--bg-surface); color: var(--text); cursor: pointer; font-size: 0.875rem;
  }
  .btn:hover { background: var(--bg-hover); }
  .btn:disabled { opacity: 0.5; cursor: not-allowed; }
  .btn.primary { background: var(--accent); color: #fff; border-color: var(--accent); }
  .btn.primary:hover { background: var(--accent-hover); }

  /* List view (default): rows. Grid view: thumbnail tiles. */
  .media-list { display: flex; flex-direction: column; gap: 0.5rem; margin-top: 1rem; }
  .media-list.grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(160px, 1fr));
    gap: 1rem;
  }
  .media-item {
    display: flex; gap: 1rem; align-items: center; cursor: pointer;
    border: 1px solid var(--border, #ccc); border-radius: 0.5rem; padding: 0.6rem 0.75rem;
  }
  .media-list.grid .media-item { flex-direction: column; align-items: stretch; text-align: center; }
  .thumb {
    width: 2.5rem; height: 2.5rem; flex: 0 0 auto;
    border-radius: 6px; background: var(--bg-hover, #eee); overflow: hidden;
  }
  .media-list.grid .thumb { width: 100%; height: 96px; }
  .thumb-img { width: 100%; height: 100%; object-fit: cover; display: block; }
  .name { font-weight: 500; word-break: break-all; }
  .size, .date { color: var(--text-muted, #888); font-size: 0.85rem; }
  .source-status { margin-left: auto; font-size: 0.8rem; display: inline-flex; align-items: center; gap: 0.35rem; }
  .media-list.grid .source-status { margin-left: 0; justify-content: center; }
  .source-status::before {
    content: ""; display: inline-block; width: 0.55rem; height: 0.55rem; border-radius: 50%;
  }
  .source-status.online::before { background: #22c55e; }
  .source-status.offline::before { background: #ef4444; }
  .sync-state {
    font-size: 0.8rem; color: #22c55e; display: inline-flex; align-items: center; gap: 0.35rem;
  }
  .sync-state::before {
    content: ""; display: inline-block; width: 0.55rem; height: 0.55rem; border-radius: 50%; background: #22c55e;
  }

  /* `media-item-detail` + `file-version-restore-confirm-modal` (same overlay
     idiom as backups.svelte's immediate-delete / divergence-details modals). */
  .modal-backdrop {
    position: fixed; inset: 0; background: rgba(0, 0, 0, 0.4);
    display: flex; align-items: center; justify-content: center; z-index: 100;
  }
  .modal {
    background: var(--bg, #fff); padding: 1.5rem; border-radius: 8px;
    max-width: 560px; max-height: 80vh; overflow-y: auto;
  }
  .modal-actions { display: flex; gap: 0.5rem; justify-content: flex-end; margin-top: 1rem; }
  .warn { color: var(--danger, #c0392b); }
  .detail-header { display: flex; align-items: center; justify-content: space-between; gap: 1rem; }
  .detail-name { margin: 0; font-size: 1.125rem; word-break: break-all; }
  .versions-heading { margin: 1rem 0 0.25rem; font-size: 0.95rem; }
  .version-list { display: flex; flex-direction: column; gap: 0.4rem; margin-bottom: 0.5rem; }
  .version-item {
    display: flex; align-items: center; gap: 0.75rem;
    padding: 0.4rem 0; border-bottom: 1px solid var(--border, #eee);
  }
  .version-time { flex: 1 1 auto; }
  .version-size { flex: 0 0 auto; }
  .share-create { margin-top: 1rem; padding-top: 0.75rem; border-top: 1px solid var(--border, #eee); }
  .share-create h3 { margin: 0 0 0.25rem; font-size: 0.95rem; }
  .share-url-row { display: flex; gap: 0.5rem; align-items: center; }
  .share-url { flex: 1 1 auto; word-break: break-all; font-size: 0.85rem; }
  .share-list { display: flex; flex-direction: column; gap: 0.4rem; }
  .share-item {
    display: flex; align-items: center; gap: 0.75rem;
    padding: 0.4rem 0; border-bottom: 1px solid var(--border, #eee);
  }
  .share-item .name { flex: 1 1 auto; }
  .share-state { font-size: 0.85rem; }
</style>
