<script lang="ts">
  // Settings → Folders (sync-file-set-ui-unification, 2026-06-28): the ONE
  // control plane for folders — list + create wizard + per-set config + conflict
  // resolution, rendered inside the Settings shell. Merges the former top-level
  // Devices/Peers page's folder surface with the former `Settings → Sync` page.
  // The device roster moved to Settings → Devices (DevicesSection.svelte). On web
  // there is NO local-folder binding (no user-bindable filesystem in a browser —
  // the folder-location-* family is desktop-only platform_elements). A dumb renderer of
  // the shared `DevicesMachine` (`libs/fauna-devices-machine`, WASM twin
  // `libs/fauna-wasm-folders`) + the embedded FolderWizardMachine: build the
  // machine → refresh() → render the folder / conflict / wizard slices → forward
  // every gesture back through the machine. No folder / conflict HTTP and no
  // wizard logic in the SPA (priority #2). Behaviour + IDs:
  // docs/goal/ui/folders.md, ui.yaml § folders.
  import { identity } from '$lib/store';
  import { nodeUrl } from '$lib/api';
  import {
    ensureWasm,
    shortId,
    accountDisplayLabel,
    parseCountI64,
    devicePlaceLabel,
  } from '$lib/wasm';
  import {
    conflictBadgeLabel,
    parsePathsField,
    joinPathsField,
    conflictPolicyOptions,
    memberAccessOptions,
    nestSnapshotsOptions,
    nestPlaceEditFromRow,
    versionRetentionEditFromBounds,
    audienceOptions,
    audienceHint,
    normalizeAudience,
    websiteServeHint,
    writerGrantReach,
    residencyOptions,
    residencyHint,
    normalizeResidency,
    placeRows,
    toggledPlaceRow,
    followPublicFolder,
    unfollowPublicFolder,
    type NestPlaceEdit,
    type VersionRetentionEdit,
  } from '$lib/wasm-folders';
  import { getConversationsManager } from '$lib/conversations';
  import { sessionDevicesMachine } from '$lib/devices-session';
  import { parseRecipient } from '$lib/resolve';
  import {
    defaultConflictPolicyGet,
    defaultConflictPolicySet,
    subscriptionsTiersList,
    foldersActorMembers,
    foldersMembers,
    foldersMemberSetAccess,
    foldersDevices,
    foldersDestinationsList,
    foldersDestinationAttach,
    foldersDestinationDetach,
    onPushEvent,
    staleSurfacesForPushKind,
    type SubscriptionTier,
  } from '$lib/rpc';
  import { resolveLocalized, resolveLocalizedNested } from '$lib/i18n/localized';
  import { onMount, onDestroy } from 'svelte';
  import { onStoreChange } from '$lib/store-change';
  import { t } from '$lib/i18n/strings';
  import { byteSize } from '$lib/value-format';
  import { getDeviceId } from '$lib/device-id';
  import {
    type ConflictSummary,
    type DevicesSnapshot,
    type FolderActorMember,
    type FolderDestinationPlace,
    type FolderDevice,
    type FolderSummary,
    type PlaceRow,
    type FollowedFolderSummary,
    type PendingShare,
    type WizardDevice,
    type WizardSnapshot,
  } from '$lib/devices-machine';
  import type { DevicesMachine } from '../../../static/fauna_wasm_folders.js';
  import { IDS } from '$lib/generated/uiIds';

  // The page-level error surface is the Settings shell's shared MessageBanner
  // (`error-message`); we write into its bound `error` so a read/write failure
  // renders there, matching every other settings sub-page.
  let { error = $bindable('') } = $props();

  let machine: DevicesMachine | null = null;
  let snap = $state<DevicesSnapshot | null>(null);
  let ready = $state(false);
  let timer: ReturnType<typeof setInterval> | null = null;
  let unsubPush: (() => void) | null = null;
  let unlisten: (() => void) | null = null;

  // UI-only view state (no domain state — that all lives in the machine).
  let expandedFs = $state<string | null>(null);
  let editIncludePaths = $state('');
  let editExcludePaths = $state('');
  let savingPaths = $state(false);
  // The expanded row's four `folder-nest-*` buffers, seeded on expand and
  // committed together. Staged rather than applied-on-change (unlike the
  // conflict-policy select) because the nest applies the policy WHOLE.
  let nestPlaceEdit = $state<NestPlaceEdit>({
    snapshots: '',
    quiet_secs: '',
    retention_snapshots: '',
    retention_days: '',
  });
  // The version-retention SIBLING pair — its own family, sent
  // whole on the SAME folder-nest-save-button click (file-versions.md §
  // Retention ruling 1), never folded into nestPlaceEdit's retention above.
  let versionRetentionEdit = $state<VersionRetentionEdit>({ count: '', days: '' });
  let savingNestPlace = $state(false);
  let pendingDeleteFs = $state<string | null>(null);

  // The armed declassify confirm (`folder-audience-public-confirm`). Picking
  // Public does NOT commit — it arms this, and the flip happens only when the
  // dialog is answered, because a public folder rests UNSEALED, names and paths
  // included (principles.md § The user always controls their data owns that one
  // exception). While it is armed the select keeps painting the folder's CURRENT
  // audience: showing "public" before the answer would report an audience the
  // folder does not have.
  let pendingPublicFs = $state<string | null>(null);

  // The armed content-residency confirm (`folder-residency-confirm`, folders
  // re-model phase 5 — file-sync.md § Content residency). The same shape as
  // `pendingPublicFs` for the same reason: the flip deletes the nest's copy of
  // the folder's content, and v1 has no custody-inferred softening — the
  // owner's explicit consent is the only gate. While armed the select keeps
  // painting the folder's CURRENT residency, never the pending one.
  let pendingResidencyFs = $state<string | null>(null);

  // Whether this actor can serve a set over WebDAV at all — it needs the MSEK
  // that serving seals the WebdavKeysBlob under, minted when mail is first
  // enabled. Read from the conversations rail (`foldersCanServeWebdav`) rather
  // than the devices snapshot: the DevicesMachine is deliberately keyless, so a
  // question about the actor's secrets belongs on the face that already holds
  // them. Gates `folder-webdav-toggle` so an MSEK-less actor sees it disabled
  // with a hint instead of clicking into a NoMsek failure that would have
  // already committed the nest flag (webdav-server.md § Independent enablement).
  let canServeWebdav = $state(false);

  // The user-global default conflict policy stamped onto NEW sets (the
  // page-level "Sync defaults" section, `sync-default-conflict-policy-select`)
  // — file-sync.md § Conflicts, policy. `null` (absent preference) renders as
  // the column default "auto". A failed read degrades to "auto" (the same
  // outcome as no preference) rather than blocking the section.
  let defaultConflictPolicy = $state('auto');

  async function loadDefaultConflictPolicy(secretHex: string): Promise<void> {
    try {
      defaultConflictPolicy = (await defaultConflictPolicyGet(secretHex)) ?? 'auto';
    } catch {
      defaultConflictPolicy = 'auto';
    }
  }

  // The creator's own subscription tiers — the `folder-paywall-tier-select`
  // option set (website-enabled rows), read per page-visit off the key-bearing
  // `SubscriptionsClient::tiers_list` face (NOT the keyless DevicesMachine
  // snapshot; folders.md § Web paywall). Empty ⇒ nothing to paywall to, so
  // the select stays disabled with a "create a tier first" hint (mirrors
  // `canServeWebdav`'s fail-safe-empty gate).
  let ownTiers = $state<SubscriptionTier[]>([]);

  async function loadOwnTiers(secretHex: string): Promise<void> {
    try {
      ownTiers = await subscriptionsTiersList(secretHex);
    } catch {
      ownTiers = [];
    }
  }

  // Model = wire values (tier names). The set's OWN current tier is always
  // present even if it was since removed from the creator's tier list, so the
  // row still shows its paywalled state (mirrors linux's `values` build).
  function paywallTierValues(fs: FolderSummary): string[] {
    const names = ownTiers.map((tier) => tier.name);
    const current = fs.web_paywall_tier;
    return current && !names.includes(current) ? [...names, current] : names;
  }

  // The `folder-writer-published-warning` copy for one `writer` grant on `fs`, or
  // null when the folder reaches nobody outside its members. The decision is the
  // shared `writerGrantReach` (public ⇒ "anyone", paywalled ⇒ "subscribers") —
  // never re-derived here — fed the NORMALIZED audience exactly as the audience
  // select above is. State-based (folders.md § Sharing): it paints on the share
  // form and the member row alike, whichever of the grant and the publish came
  // first, and it stacks with the uncapped-quota warning.
  function publishedWriterWarning(fs: FolderSummary, access: string): string | null {
    const reach = writerGrantReach(
      access,
      normalizeAudience(fs.audience ?? '', !!fs.mls_group_id),
      !!fs.web_paywall_tier,
    );
    return reach ? resolveLocalized(reach) : null;
  }

  // ── Cross-user sharing (owner side) — the "Shared with" section on an expanded
  // folder-row (folders.md § Sharing). The cross-user actor roster is DISTINCT
  // from the device roster (`members()` below); it is read on demand via the
  // WsRpcClient `foldersActorMembers` face (not carried on the keyless
  // DevicesMachine snapshot), keyed by set name. Eager-loaded for shared sets
  // (`mls_group_id != null`) so a collapsed row can paint its `folder-shared-badge`;
  // lazy-loaded on first expand otherwise. A read failure degrades to an empty
  // roster WITHOUT the page error (a missing badge is not a user-facing error — and
  // the affordances e2e asserts no error after expanding an unshared set).
  let actorMembers = $state<Record<string, FolderActorMember[]>>({});
  // Per-set device activity (fauna.folders.devices), keyed by set name — the
  // ordinary sync change signal, distinct from FolderSummary's
  // cached_snapshot_count/cached_total_bytes (snapshot-only). Lazy-loaded on
  // first expand, and re-fetched on every fauna.sync.changed push so the
  // remote-change nudge is actually visible (file-sync.md § Implementation
  // status today). A read failure degrades to an empty list, same as
  // actorMembers.
  let deviceActivity = $state<Record<string, FolderDevice[]>>({});
  // Per-set DEVICE roster (fauna.folders.members.list), projected into the rows
  // the device-place editor paints (`placeRows` — the one shared flags-first
  // rule, never re-derived here). Keyed by set name, lazy-loaded on first expand
  // like deviceActivity, and RE-READ after every place write: the row must show
  // the nest's answer, not an optimistic local flip
  // (ui/folders.md § Implementation status today). A read failure degrades to an
  // empty list — the section simply does not paint.
  let placeRowsBySet = $state<Record<string, PlaceRow[]>>({});
  // Per-set destination places (docs/goal/behavior/backup-destinations.md §
  // Ordinary-folder coverage), keyed by set name — which of the owner's
  // enrolled backup destinations are attached to this ordinary folder.
  // Lazy-loaded on first expand, same shape as deviceActivity; every
  // attach/detach re-fetches rather than flipping locally (non-optimistic,
  // matching linux's `populate_folder_destinations`). A read failure degrades
  // to an empty list — an absent section is not a user-facing error.
  let destinationPlaces = $state<Record<string, FolderDestinationPlace[]>>({});
  // The `folder-destination-attach-select` value per set, keyed by set name —
  // defaults to the first attachable destination so a single-destination
  // owner needs only the Attach click.
  let destinationAttachSelect = $state<Record<string, string>>({});
  // Which set's destination-place mutation is in flight — the Attach/Detach
  // buttons' disabled guard, mirroring `savingPaths`/`savingNestPlace`.
  let destinationBusy = $state<Record<string, boolean>>({});
  // Which set's inline Share form is open (its name), plus the recipient-picker
  // input + the chosen Reader/Writer access. Reader is the default; a writer grant
  // sends `access`, a reader grant omits it (matching the native share flow).
  let shareDialogFs = $state<string | null>(null);
  let shareInput = $state('');
  let shareAccess = $state('reader');
  let sharing = $state(false);
  // Recipient side (B5): the staged ("knocked") shares awaiting accept/decline.
  let pendingShares = $state<PendingShare[]>([]);
  let pendingBusy = $state(false);

  // ── Following a public folder (phase 4 slice 4f-iii) ──
  // Whether the follow flow is open. PAGE-level, not per-row: a follow names
  // somebody ELSE's folder, so it belongs to the page rather than to any row
  // (`ui/folders.md` § Following a public folder; tui's `follow_open`). The
  // owner half reuses `recipient-picker-input` verbatim, exactly as the share
  // flow does — no second picker (priority #2).
  let followOpen = $state(false);
  let followHandleInput = $state('');
  let followNameInput = $state('');
  let following = $state(false);
  let unfollowing = $state(false);

  // The cross-user members a set is shared WITH (role == "member"); the owner's own
  // entry (role == "owner") is filtered out. Its length is the "Shared · N" count —
  // one derivation site, never re-filter `role` elsewhere (folders.md § Sharing).
  function sharedMembers(fsName: string): FolderActorMember[] {
    return (actorMembers[fsName] ?? []).filter((m) => m.role === 'member');
  }

  async function loadActorMembers(fsName: string): Promise<void> {
    const id = $identity;
    if (!id) return;
    try {
      actorMembers[fsName] = await foldersActorMembers(id.secretHex, fsName);
    } catch {
      actorMembers[fsName] = [];
    }
  }

  async function loadDeviceActivity(fsName: string): Promise<void> {
    const id = $identity;
    if (!id) return;
    try {
      deviceActivity[fsName] = await foldersDevices(id.secretHex, fsName);
    } catch {
      deviceActivity[fsName] = [];
    }
  }

  async function loadPlaceRows(name: string): Promise<void> {
    const id = $identity;
    if (!id) return;
    try {
      placeRowsBySet[name] = placeRows(
        await foldersMembers(id.secretHex, name),
        snap?.devices ?? [],
      );
    } catch {
      placeRowsBySet[name] = [];
    }
  }

  async function loadDestinationPlaces(fs: FolderSummary): Promise<void> {
    const id = $identity;
    if (!id) return;
    try {
      destinationPlaces[fs.name] = await foldersDestinationsList(id.secretHex, fs.id);
    } catch {
      destinationPlaces[fs.name] = [];
    }
  }

  /** Attach `destinationId` (the select's current value — resolved at the call
   *  site so a single-destination row needs no onchange to have fired) — the
   *  section always repaints from the mutation's own re-read, never an
   *  optimistic flip. */
  async function attachFolderDestination(fs: FolderSummary, destinationId: string): Promise<void> {
    const id = $identity;
    if (!id || !destinationId) return;
    destinationBusy[fs.name] = true;
    try {
      destinationPlaces[fs.name] = await foldersDestinationAttach(
        id.secretHex,
        fs.id,
        destinationId,
      );
      // The attachable set just shrank — drop the stale selection so the next
      // render's default recomputes off the fresh list.
      delete destinationAttachSelect[fs.name];
    } catch (e: unknown) {
      error = t.devices.error_folder_destination({ message: detail(e) });
    } finally {
      destinationBusy[fs.name] = false;
    }
  }

  /** Detach one attached place — `place.folder_set` is the row's own
   *  `__folder/<hex>/<id>` name, never re-derived here. */
  async function detachFolderDestination(
    fs: FolderSummary,
    place: FolderDestinationPlace,
  ): Promise<void> {
    const id = $identity;
    if (!id) return;
    destinationBusy[fs.name] = true;
    try {
      destinationPlaces[fs.name] = await foldersDestinationDetach(
        id.secretHex,
        fs.id,
        place.destination_id,
        place.folder_set ?? '',
      );
    } catch (e: unknown) {
      error = t.devices.error_folder_destination({ message: detail(e) });
    } finally {
      destinationBusy[fs.name] = false;
    }
  }

  // Eager-load every shared set's roster once, so collapsed rows paint their badge.
  function loadSharedBadges(): void {
    for (const fs of snap?.folders ?? []) {
      if (fs.mls_group_id && actorMembers[fs.name] === undefined) {
        void loadActorMembers(fs.name);
      }
    }
  }

  function applySnapshot(): void {
    if (!machine) return;
    const raw = machine.snapshotJson();
    snap = raw ? (JSON.parse(raw) as DevicesSnapshot) : null;
    error = snap?.error ? resolveLocalized(snap.error) : '';
    loadSharedBadges();
  }

  // A failed capability read degrades to "cannot serve" (the toggle stays
  // disabled + hinted) rather than offering a control that cannot succeed.
  async function loadWebdavCapability(secretHex: string): Promise<void> {
    try {
      const mgr = await getConversationsManager();
      canServeWebdav = await mgr.foldersCanServeWebdav(secretHex);
    } catch {
      canServeWebdav = false;
    }
  }

  onMount(async () => {
    const id = $identity;
    if (!id) {
      ready = true;
      return;
    }
    try {
      await ensureWasm();
      // The session's ONE machine, shared with the Devices section and wired
      // once (`devices-session.ts` says why it must outlive this mount — the
      // followed rows' last-read memory and the refresh barrier).
      const session = await sessionDevicesMachine(id.secretHex, () => applySnapshot());
      machine = session.machine;
      unlisten = session.unlisten;
      // Paint the last-known rows now; the refresh below brings them current.
      applySnapshot();
      ready = true;
      await machine.refresh();
      applySnapshot();
      void loadWebdavCapability(id.secretHex);
      void loadDefaultConflictPolicy(id.secretHex);
      void loadOwnTiers(id.secretHex);
      void loadPendingShares();
      timer = setInterval(() => {
        machine?.refresh();
        void loadPendingShares();
      }, 15000);
      // Remote-change nudge (file-sync.md § Remote-change nudge): the nest
      // fires `fauna.sync.changed` at every same-nest participant of a set
      // (owner's other devices, or a shared-set member) after a durable sync
      // record — an off-cadence wake so a collaborator's change appears in
      // seconds, not on the next 15s poll. Web has no resident engine to wake
      // (unlike linux/tui's `PullFolderNow`), so the reaction is the same
      // blanket re-fetch android's `folderChangedTick` arm uses: no per-set
      // filtering, mirroring `CalendarChanged`'s unfiltered shape (both
      // android and the events page's `fauna.calendar.changed` arm re-fetch
      // everything rather than the one changed item). Checked through the
      // shared classifier (`transport.md` § Which surfaces a push invalidates)
      // rather than matching `kind` by hand — this also makes
      // `fauna.protocol.resync_required` refresh this section, which the old
      // exact-match check never did (the 15s poll above already backstops it,
      // so this closes the gap sooner rather than closing a break).
      unsubPush = onPushEvent((kind) => {
        if (!staleSurfacesForPushKind(kind).media) return;
        machine?.refresh();
        void loadPendingShares();
        // Same blanket-refresh shape as the reads above: no per-set filtering
        // in the push payload reaches this callback, so re-fetch whichever
        // row is currently expanded — the only one with device activity
        // visible on screen right now.
        if (expandedFs) void loadDeviceActivity(expandedFs);
      });
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : String(e);
      ready = true;
    }
  });

  // The store-change notice (`$lib/store-change`): the followed folders and
  // foreign sets rest in the account store, so the open page runs the periodic
  // re-read's own body at once. The default conflict policy is NOT joined: a
  // failed re-read resets that select to "auto".
  const unsubStoreChange = onStoreChange(() => {
    machine?.refresh();
    void loadPendingShares();
  });

  onDestroy(() => {
    if (timer) clearInterval(timer);
    unsubPush?.();
    unsubStoreChange();
    unlisten?.();
  });

  // ── Folder gestures (all through the machine) ──
  function requestDeleteFolder(name: string): void {
    pendingDeleteFs = name;
  }

  async function confirmDeleteFolder(): Promise<void> {
    const name = pendingDeleteFs;
    if (!machine || !name) return;
    await machine.deleteFolder(name);
    pendingDeleteFs = null;
    expandedFs = null;
    applySnapshot();
  }

  // ── Phase 4 slice 4d: audience + website serving (ui/folders.md § Audience
  // and website serving). Both are keyless `fauna.folders.update` writes through
  // the machine — no conversations manager, unlike `changeWebdav`/`changePaywallTier`
  // below, because neither touches a content key: the back-catalogue is moved by
  // each device's own engine at its next catch-up off the projected audience.

  // Picking Public ARMS the confirm; every other destination commits at once —
  // including `shared` on a bound row exiting its public window (the
  // flip-back, which each member's engine re-seals off the projected audience).
  // Anywhere else `audienceOptions` renders `shared` unselectable, because
  // bound-ness is entered through the share flow alone.
  async function changeAudience(name: string, value: string): Promise<void> {
    if (!machine) return;
    if (value === 'public') {
      pendingPublicFs = name;
      return;
    }
    pendingPublicFs = null;
    await machine.setFolderAudience(name, value);
    applySnapshot();
  }

  // ⚠ **The select must keep painting the folder's CURRENT audience while the
  // confirm is armed** (ui/folders.md § Audience and website serving) — showing
  // `public` before the answer reports an audience the folder does not have.
  //
  // A browser `<select>` moves its OWN value the instant an option is picked,
  // and Svelte schedules no update to move it back, because the bound
  // expression is unchanged (`private` → `private`). So the DOM has to be put
  // back by hand on the arming path. Caught by
  // `test_folder_audience_control.py::test_the_audience_control_arms_before_it_publishes[web]`,
  // which reads the select right after arming — the assertion tui passes for
  // free and the DOM does not.
  function onAudiencePicked(fs: FolderSummary, el: HTMLSelectElement): void {
    const picked = el.value;
    if (picked === 'public') {
      el.value = normalizeAudience(fs.audience ?? '', !!fs.mls_group_id);
    }
    void changeAudience(fs.name, picked);
  }

  async function confirmPublicAudience(): Promise<void> {
    const name = pendingPublicFs;
    if (!machine || !name) return;
    await machine.setFolderAudience(name, 'public');
    pendingPublicFs = null;
    applySnapshot();
  }

  // ── Content residency (folders re-model phase 5 — file-sync.md § Content
  // residency) — the audience pair's twin. Its own keyless `fauna.folders.update`
  // write, deliberately never folded into the batched setFolderNestPlace save.

  // Picking Metadata-only ARMS the confirm rather than committing — the nest
  // deletes its copy of the folder's content on that write. The flip back to
  // Full commits at once, like the audience select's non-destructive directions.
  async function changeResidency(name: string, value: string): Promise<void> {
    if (!machine) return;
    if (value === 'metadata_only') {
      pendingResidencyFs = name;
      return;
    }
    pendingResidencyFs = null;
    await machine.setFolderResidency(name, value);
    applySnapshot();
  }

  // ⚠ Same DOM-repaint trap `onAudiencePicked` documents: a browser `<select>`
  // moves its own value the instant an option is picked, and Svelte schedules no
  // update to move it back while the bound expression is unchanged. Put back by
  // hand on the arming path.
  function onResidencyPicked(fs: FolderSummary, el: HTMLSelectElement): void {
    const picked = el.value;
    if (picked === 'metadata_only') {
      el.value = normalizeResidency(fs.residency ?? '');
    }
    void changeResidency(fs.name, picked);
  }

  async function confirmResidency(): Promise<void> {
    const name = pendingResidencyFs;
    if (!machine || !name) return;
    await machine.setFolderResidency(name, 'metadata_only');
    pendingResidencyFs = null;
    applySnapshot();
  }

  async function changeWebsite(name: string, enabled: boolean): Promise<void> {
    if (!machine) return;
    await machine.setFolderWebsiteEnabled(name, enabled);
    applySnapshot();
  }

  async function useOtherVersion(id: number): Promise<void> {
    // Review list (file-sync.md § Conflicts, auto-resolve): one-tap re-point at
    // the latest retained non-winning candidate — an ordinary restore record
    // attributed to this browser's device id (the Media restore's idiom).
    const actorId = $identity?.actorId;
    if (!actorId) return;
    await machine?.useOtherVersion(BigInt(id), getDeviceId(actorId));
    applySnapshot();
  }

  function toggleExpand(fs: FolderSummary): void {
    if (expandedFs === fs.name) {
      expandedFs = null;
      return;
    }
    expandedFs = fs.name;
    editIncludePaths = joinPathsField(fs.include_paths);
    editExcludePaths = joinPathsField(fs.exclude_paths);
    // Seed the nest-place editor through the shared prefill, which owns the
    // rules this page must not re-derive: an unset knob shows BLANK, and so does
    // a zero retention bound (zero is the nest's own spelling of unset).
    nestPlaceEdit = nestPlaceEditFromRow(
      fs.nest_snapshots,
      fs.nest_snapshot_quiet_secs,
      fs.retention_policy,
    );
    versionRetentionEdit = versionRetentionEditFromBounds(
      fs.version_retention_max_versions,
      fs.version_retention_max_age_days,
    );
    // Close any Share form left open on another row, and lazy-load this set's
    // cross-user roster if it wasn't eager-loaded (an owner-only set with no badge).
    shareDialogFs = null;
    // Expanding a different row drops an armed declassify confirm, exactly as it
    // drops an armed delete confirm (ui/folders.md § Audience and website serving)
    // — an arm that outlived its row could publish a folder the user is no longer
    // looking at.
    pendingPublicFs = null;
    if (actorMembers[fs.name] === undefined) void loadActorMembers(fs.name);
    void loadDeviceActivity(fs.name);
    void loadPlaceRows(fs.name);
    void loadDestinationPlaces(fs);
  }

  // The caught value as the `{message}` every author-gesture failure string carries
  // (ui/README.md § Copy comprehensibility — the detail is the point of the banner).
  function detail(e: unknown): string {
    return e instanceof Error ? e.message : String(e);
  }

  // Per-set conflict policy (`folder-conflict-policy-select`, owner rows
  // only — conflicts arise from bidirectional sync) — file-sync.md § Conflicts,
  // policy. The resolving device reads the policy off this authoritative row.
  // A `folder-place-*` checkbox: flip ONE box on the seat and write the WHOLE
  // point, then RE-READ the roster so the boxes show what the nest holds
  // (ui/folders.md § Implementation status today — never the optimistic flip).
  //
  // ⚠ The point applies whole: `toggledPlaceRow` is what composes it, so the
  // two boxes the user did not touch cannot be dropped on the way to the wire.
  // A `null` back means the seat is unreadable — it paints no checkboxes, so
  // this is unreachable for it, and there is no partial edit to fall back to.
  async function togglePlaceFlag(name: string, row: PlaceRow, flag: string): Promise<void> {
    if (!machine) return;
    const next = toggledPlaceRow(row, flag);
    if (!next) return;
    await machine.setFolderPlace(
      name,
      next.device_id,
      next.originates,
      next.accepts,
      next.applies_deletes,
    );
    applySnapshot();
    await loadPlaceRows(name);
  }

  async function changeConflictPolicy(name: string, policy: string): Promise<void> {
    if (!machine) return;
    await machine.setFolderConflictPolicy(name, policy);
    applySnapshot();
  }

  // The page-level "Sync defaults" default-policy select
  // (`sync-default-conflict-policy-select`). Existing sets are untouched. NOT a
  // DevicesMachine gesture (seals into the owner's encrypted
  // `fauna.state.sync-prefs`), so a failure surfaces client-side on the page
  // `error-message` rather than through the snapshot's own `error` field.
  async function changeDefaultConflictPolicy(policy: string): Promise<void> {
    const id = $identity;
    if (!id) return;
    try {
      defaultConflictPolicy = (await defaultConflictPolicySet(id.secretHex, policy)) ?? 'auto';
    } catch (e: unknown) {
      error = t.devices.error_set_default_conflict_policy({ message: detail(e) });
    }
  }

  // Per-set "serve over WebDAV" opt-in (every owner row) — webdav-server.md §
  // Independent enablement point 2. NOT a DevicesMachine config gesture: it runs
  // the full serve orchestration (content-key genesis/rotation + the nest flag +
  // the MSEK-sealed WebdavKeysBlob re-provision) through the shared
  // `FoldersAuthor::serve_set` on the conversations rail's manager (the same
  // MlsEngine the share/remove gestures use — priority #2), then refreshes the
  // snapshot so the toggle reflects the persisted `webdav_enabled` state. A
  // failure (e.g. no MSEK — mail not set up yet) surfaces on the shared
  // error-message banner.
  async function changeWebdav(
    name: string,
    mlsGroupId: string | null | undefined,
    enable: boolean,
  ): Promise<void> {
    const id = $identity;
    if (!id || !machine) return;
    try {
      const mgr = await getConversationsManager();
      // This browser's device id: an enable also re-seals the folder's
      // pre-serve files onto the served key, recorded under it (the same id
      // the Media page records under — `webdav-server.md` § Key model (c)).
      await mgr.foldersServeSet(
        id.secretHex,
        name,
        mlsGroupId ?? undefined,
        enable,
        getDeviceId(id.actorId),
      );
      await machine.refresh();
      applySnapshot();
    } catch (e: unknown) {
      error = t.devices.error_serve_webdav({
        message: e instanceof Error ? e.message : String(e),
      });
    }
  }

  // Per-set "paywall to tier" control (website-enabled rows only) — the structural
  // sibling of `changeWebdav` above: NOT a DevicesMachine config write, it runs
  // the full paywall orchestration (content-key genesis/re-seal + the nest
  // `web_paywall_tier` flag + the web-serve-holder grant mint) through the
  // shared `FoldersAuthor::paywall_set` on the conversations rail's manager
  // (folders.md § Web paywall). v1 is SET-ONLY: the empty placeholder is not
  // a write — there is no "clear the paywall" path yet.
  async function changePaywallTier(
    name: string,
    mlsGroupId: string | null | undefined,
    tier: string,
  ): Promise<void> {
    const id = $identity;
    if (!id || !machine || !tier) return;
    try {
      const mgr = await getConversationsManager();
      await mgr.foldersPaywallSet(id.secretHex, name, tier, mlsGroupId ?? undefined);
      await machine.refresh();
      applySnapshot();
    } catch (e: unknown) {
      error = t.devices.error_paywall_set({ message: detail(e) });
    }
  }

  // ── Sharing gestures (owner side) ──
  function openShareDialog(fsName: string): void {
    // Closes the page-level follow form for the same reason `openFollowForm`
    // closes this one: both paint `recipient-picker-input`.
    followOpen = false;
    shareDialogFs = fsName;
    shareInput = '';
    shareAccess = 'reader';
  }
  function cancelShare(): void {
    shareDialogFs = null;
    shareInput = '';
    shareAccess = 'reader';
  }

  // Share a set with one picked recipient end-to-end (KeyPackage fetch → MLS group
  // → content-key genesis → Welcome), through the conversations rail's manager (the
  // same face `changeWebdav`/`changePaywallTier` use). The recipient is resolved by
  // the shared `parseRecipient` (a 64-hex actor id resolves directly; a handle
  // resolves over `nest.resolve` + `actor.by_handle`); a member on a different nest
  // passes their node URL, same-nest passes none. Reader is the default — only a
  // writer grant sends `access`.
  async function doShare(fs: FolderSummary): Promise<void> {
    const id = $identity;
    if (!id || !machine || sharing) return;
    const raw = shareInput.trim();
    if (!raw) return;
    sharing = true;
    try {
      const resolved = await parseRecipient(raw);
      const mgr = await getConversationsManager();
      const own = nodeUrl();
      const access = shareAccess === 'writer' ? 'writer' : undefined;
      await mgr.foldersShareSet(
        id.secretHex,
        fs.name,
        resolved.actorId,
        resolved.nodeUrl !== own ? resolved.nodeUrl : undefined,
        access,
      );
      shareDialogFs = null;
      shareInput = '';
      shareAccess = 'reader';
      await machine.refresh();
      applySnapshot();
      await loadActorMembers(fs.name);
    } catch (e: unknown) {
      error = t.devices.error_share_set({ message: detail(e) });
    } finally {
      sharing = false;
    }
  }

  // ── Cross-user sharing (recipient side, B5) — the "Shared with you" pending-knock
  // area (folders.md § Sharing — *Recipient side*). A CONTACT's share auto-joins
  // off the chat rail through the wasm contact gate and never lands here; only a
  // STRANGER's knock does. The un-acked Welcome in the durable inbox IS the list —
  // there is no second store, so a reload re-reads it (a peek never acks).
  async function loadPendingShares(): Promise<void> {
    if (!$identity) return;
    try {
      const mgr = await getConversationsManager();
      pendingShares = await mgr.foldersPendingShares();
    } catch {
      // A read failure degrades to an empty knock list WITHOUT the page error —
      // the same rule the owner-side roster read follows.
      pendingShares = [];
    }
  }

  // The row's sharer identity: the pre-computed `sharedByDisplay` verbatim (handle
  // else the sharer's canonical short id — value-formatting.md § Account display
  // label), never a local truncation. Empty only for a fully unstamped share.
  function pendingShareSharer(share: PendingShare): string {
    return share.sharedByDisplay || t.common.unknown;
  }

  async function acceptPendingShare(share: PendingShare): Promise<void> {
    if (pendingBusy) return;
    pendingBusy = true;
    try {
      const mgr = await getConversationsManager();
      await mgr.foldersAcceptShare(share.inboxId);
      await loadPendingShares();
      // The accept JOINED the group, so the set can now pass the machine's
      // join-filter and appear as a read-only shared-with-me row.
      await machine?.refresh();
      applySnapshot();
    } catch (e: unknown) {
      error = t.devices.error_accept_share({ message: detail(e) });
    } finally {
      pendingBusy = false;
    }
  }

  // A set shared WITH this actor (B3) — rendered read-only. `role` is always stamped by the nest;
  // an absent value reads as owner.
  function isMemberRow(fs: FolderSummary): boolean {
    return fs.role === 'member';
  }

  // Leave a set shared with you (`folder-leave-button`): the self-scoped nest
  // roster-drop + the local MLS forget. NOT a removal — it touches no other member
  // and does not rotate the owner's content key (mls-group-key-material.md § M2).
  // Off the roster the projection stops unioning the set in, so the row disappears.
  async function leaveSharedSet(fs: FolderSummary): Promise<void> {
    if (pendingBusy || !fs.mls_group_id) return;
    pendingBusy = true;
    try {
      const mgr = await getConversationsManager();
      await mgr.foldersLeave(fs.mls_group_id);
      await machine?.refresh();
      applySnapshot();
    } catch (e: unknown) {
      error = t.devices.error_leave_share({ message: detail(e) });
    } finally {
      pendingBusy = false;
    }
  }

  async function declinePendingShare(share: PendingShare): Promise<void> {
    if (pendingBusy) return;
    pendingBusy = true;
    try {
      const mgr = await getConversationsManager();
      await mgr.foldersDeclineShare(share.inboxId);
      await loadPendingShares();
    } catch (e: unknown) {
      error = t.devices.error_decline_share({ message: detail(e) });
    } finally {
      pendingBusy = false;
    }
  }

  // ── Following a public folder (`ui/folders.md` § Following a public folder) ──

  // Opening the follow form CLOSES any open per-row Share form, because both
  // render `recipient-picker-input` and two live copies would make an unscoped
  // read of it ambiguous (e2e convention 1: no duplicate IDs). They are
  // mutually exclusive gestures anyway — one names a recipient for a folder you
  // own, the other names an owner whose folder you don't.
  function openFollowForm(): void {
    shareDialogFs = null;
    followOpen = true;
    followHandleInput = '';
    followNameInput = '';
  }

  function cancelFollow(): void {
    followOpen = false;
    followHandleInput = '';
    followNameInput = '';
  }

  /**
   * Follow a public folder by (owner, folder name).
   *
   * The owner string goes to the shared Rust recipe UNTOUCHED
   * (`follow_ops::follow_public_folder`, behind the wasm `followPublicFolder`):
   * it classifies a bare 64-hex actor id or a handle, resolves a bare handle
   * on this nest, and discovers a `handle@domain`'s home nest itself — the
   * cross-nest follow, which the SPA's earlier own `parseRecipient` + empty
   * home url could never make (it always resolved on, and followed on, this
   * nest). The address a user types is a HANDLE, not a nest, which is why the
   * flow never asks for one; exactly what tui's `follow_public` passes.
   *
   * ⚠ Absent, private and misspelled fail IDENTICALLY — the home nest folds the
   * three so nothing can probe for a sealed folder's existence — so this
   * surfaces the recipe's own localized message rather than inventing a
   * friendlier per-case one, which would hand back the very distinction the
   * nest refused.
   */
  async function doFollow(): Promise<void> {
    const id = $identity;
    if (!id || !machine || following) return;
    const owner = followHandleInput.trim();
    const name = followNameInput.trim();
    // Refusing a blank box here rather than sending a blank address keeps the
    // nest's one-answer refusal meaningful: an empty query would come back
    // "not found" and read as "no such folder" rather than "you left a box
    // empty" (tui's `ConfirmFolderFollow` makes the same call; the recipe
    // refuses it again — this is the UX half).
    if (!owner || !name) return;
    following = true;
    try {
      await followPublicFolder(machine, id.secretHex, owner, name);
      followOpen = false;
      followHandleInput = '';
      followNameInput = '';
      await machine.refresh();
      applySnapshot();
    } catch (e: unknown) {
      // The recipe's rejection is already the catalog wording
      // (`follow_error_text`), so it lands on the page as-is.
      error = detail(e);
    } finally {
      following = false;
    }
  }

  // Unfollow — a purely LOCAL removal, idempotent: there is nothing to revoke
  // anywhere, because the home nest never knew this follower existed (the
  // public read plane keeps zero follower state by design).
  async function doUnfollow(f: FollowedFolderSummary): Promise<void> {
    const id = $identity;
    if (!id || !machine || unfollowing) return;
    unfollowing = true;
    try {
      await unfollowPublicFolder(machine, id.secretHex, f.home_nest_url, f.folder_id);
      await machine.refresh();
      applySnapshot();
    } catch (e: unknown) {
      error = t.devices.error_unfollow_failed({ message: detail(e) });
    } finally {
      unfollowing = false;
    }
  }

  // Remove a member — rotates the content key for forward secrecy (MLS Remove →
  // rotate → re-seal → evict), through the conversations manager. The remove face
  // takes the set's derived `ChannelId`, which for a set loaded from a snapshot must
  // be derived from `mls_group_id` via the wasm `foldersChannelIdFromGroupId` twin
  // (the derivation can't be done in TS).
  async function removeMember(fs: FolderSummary, member: FolderActorMember): Promise<void> {
    const id = $identity;
    if (!id || !machine || !fs.mls_group_id) return;
    try {
      const mgr = await getConversationsManager();
      const channelId = mgr.foldersChannelIdFromGroupId(fs.mls_group_id);
      await mgr.foldersRemoveMember(id.secretHex, fs.name, channelId, member.actor_id);
      await machine.refresh();
      applySnapshot();
      await loadActorMembers(fs.name);
    } catch (e: unknown) {
      error = t.devices.error_remove_member({ message: detail(e) });
    }
  }

  // A member's role-select change OR byte-cap commit writes the FULL (access, cap)
  // pair — `set_access` replaces both, so sending one half would clear the other
  // (multi-writer Phase 1). The caller passes the changed value plus the member's
  // current value for the other half.
  async function changeMemberAccess(
    fs: FolderSummary,
    member: FolderActorMember,
    access: string,
    byteCap: number | undefined,
  ): Promise<void> {
    const id = $identity;
    if (!id) return;
    try {
      await foldersMemberSetAccess(id.secretHex, fs.name, member.actor_id, access, byteCap);
      await loadActorMembers(fs.name);
    } catch (e: unknown) {
      error = t.devices.error_set_member_access({ message: detail(e) });
    }
  }

  async function saveSelectivePaths(name: string): Promise<void> {
    if (!machine) return;
    savingPaths = true;
    const include = parsePathsField(editIncludePaths);
    const exclude = parsePathsField(editExcludePaths);
    await machine.setFolderPaths(name, include, exclude);
    // Collapse so the next expand reads back the nest-authoritative, stored
    // paths from the refreshed snapshot (proves the round-trip).
    expandedFs = null;
    savingPaths = false;
    applySnapshot();
  }

  /** Commit the four `folder-nest-*` controls PLUS the version-retention
   *  SIBLING pair as ONE `fauna.folders.update`.
   *
   *  The raw control values go to the wasm face, which routes them through the
   *  shared `nest_place_write` / `version_retention_write` — so the SPA holds
   *  no copy of the rules that make this editor correct: every snapshot knob
   *  rides on every save (an emptied box must reach the nest as *unset*, since
   *  the policy applies whole), a cleared snapshot retention rides as the
   *  canonical binds-nothing policy rather than as `None` (which the wire
   *  reads as "leave unchanged"), and the version-retention pair always rides
   *  as `Some` since the boxes are on screen. */
  async function saveNestPlace(name: string): Promise<void> {
    if (!machine) return;
    savingNestPlace = true;
    await machine.setFolderNestPlace(
      name,
      nestPlaceEdit.snapshots,
      nestPlaceEdit.quiet_secs,
      nestPlaceEdit.retention_snapshots,
      nestPlaceEdit.retention_days,
      versionRetentionEdit.count,
      versionRetentionEdit.days,
    );
    // Collapse so the next expand re-seeds from the refreshed snapshot — the
    // round-trip proof, and the only way a cleared knob visibly comes back blank.
    expandedFs = null;
    savingNestPlace = false;
    applySnapshot();
  }

  // ── The three place-flag checkboxes ──────────────────────────────────
  // The flag MEANINGS live once, in `fauna_protocol::folders::PlaceFlags`; this
  // table only pairs each with its element id and copy, in canonical order.
  // Shared by the WIZARD's enrollment step (`wizard-device-*`) and the
  // post-create place EDITOR (`folder-place-*`) — the same boxes and the same
  // words on a live seat, which is why the labels are not duplicated.
  //
  // ⚠ `id` is the element-id suffix (hyphenated) and `wire` the shared flag key
  // `toggledPlaceRow` takes (underscored). They differ for `applies-deletes`
  // only, and passing one where the other belongs is a silent no-write.
  type PlaceFlagId = 'originates' | 'accepts' | 'applies-deletes';
  const PLACE_FLAGS: {
    id: PlaceFlagId;
    wire: string;
    read: (d: WizardDevice) => boolean;
    readRow: (r: PlaceRow) => boolean;
    label: () => string;
    desc: () => string;
  }[] = [
    {
      id: 'originates',
      wire: 'originates',
      read: (d) => d.originates,
      readRow: (r) => r.originates,
      label: () => t.devices.wizard.place_originates,
      desc: () => t.devices.wizard.place_originates_desc,
    },
    {
      id: 'accepts',
      wire: 'accepts',
      read: (d) => d.accepts,
      readRow: (r) => r.accepts,
      label: () => t.devices.wizard.place_accepts,
      desc: () => t.devices.wizard.place_accepts_desc,
    },
    {
      id: 'applies-deletes',
      wire: 'applies_deletes',
      read: (d) => d.applies_deletes,
      readRow: (r) => r.applies_deletes,
      label: () => t.devices.wizard.place_applies_deletes,
      desc: () => t.devices.wizard.place_applies_deletes_desc,
    },
  ];

  // ── Wizard gestures (forwarded to the embedded FolderWizardMachine) ──
  // Right after opening, inject the user's global default conflict policy so
  // `submit()` stamps it onto the create — the client-glue half of the
  // Sync-defaults contract (file-sync.md § Conflicts, policy; mirrors linux's
  // `open_wizard` → `set_default_conflict_policy` pairing).
  function openWizard(): void {
    machine?.openWizard();
    machine?.wizard()?.setDefaultConflictPolicy(defaultConflictPolicy);
  }
  function closeWizard(): void {
    machine?.closeWizard();
  }
  function wizardSetName(name: string): void {
    machine?.wizard()?.setName(name);
  }
  // No `wizardSetMode`: a folder has no type (slice e), so the wizard never sets
  // one. The machine still defaults new folders to `sync`, which is what the
  // `mode` column keeps until phase 3 contracts it.
  function wizardNext(): void {
    machine?.wizard()?.next();
  }
  function wizardBack(): void {
    machine?.wizard()?.back();
  }
  function wizardToggleDevice(index: number): void {
    machine?.wizard()?.toggleDeviceMember(index);
  }
  /** Set one of seat `index`'s three place flags. The machine takes the whole
   *  triple (it writes all three in one call, so they can never
   *  drift), so this reads the seat's other two off the snapshot rather than
   *  tracking them locally. */
  function wizardSetDeviceFlag(index: number, id: PlaceFlagId, on: boolean): void {
    const w = machine?.wizard();
    if (!w) return;
    const wd = snap?.wizard?.device_places.devices[index];
    if (!wd) return;
    w.setDeviceFlags(
      index,
      id === 'originates' ? on : wd.originates,
      id === 'accepts' ? on : wd.accepts,
      id === 'applies-deletes' ? on : wd.applies_deletes,
    );
    // The machine's observer re-applies the snapshot, like every other wizard
    // gesture here — no explicit refresh.
  }
  async function wizardCreate(): Promise<void> {
    const w = machine?.wizard();
    if (!w) return;
    const step = await w.submit();
    applySnapshot();
    if (step === 'Done') {
      machine?.closeWizard();
      await machine?.refresh();
      applySnapshot();
    }
    // Otherwise the wizard stays on Review with the failure in
    // `snap.wizard.review` (phase/created/failed_members/error).
  }

  // ── Derived helpers ──
  // The member roster + count derive from the device list (each device carries
  // its per-folder place flags) — no separate members call (DevicesMachine
  // surfaces the roster via `devices[].folders`). `place` is the shared composed
  // label (`fauna_core::format::device_place_label`), resolved NESTED because
  // its template arguments are themselves i18n keys.
  function members(fsName: string): { label: string; place: string }[] {
    return (snap?.devices ?? [])
      .map((d) => {
        const entry = d.folders.find((f) => f.name === fsName);
        return entry
          ? {
              label: d.label || shortId(d.device_id),
              place: resolveLocalizedNested(
                devicePlaceLabel(entry.originates, entry.accepts, entry.applies_deletes),
              ),
            }
          : null;
      })
      .filter((m): m is { label: string; place: string } => m !== null);
  }

  // Three steps since phase 5 retired the scan-frequency step (file-sync.md
  // § Config, the phase-5 block).
  const STEP_NUM: Record<string, number> = { Name: 1, Devices: 2, Review: 3 };
  function continueEnabled(w: WizardSnapshot): boolean {
    switch (w.step) {
      case 'Name': return w.name.continue_enabled;
      // The place-flag step's own gate, from the machine — never re-derived.
      case 'Devices': return w.device_places.continue_enabled;
      default: return false;
    }
  }

  function formatTime(epoch: number | null): string {
    if (epoch == null || epoch === 0) return t.common.never;
    return new Date(epoch * 1000).toLocaleString();
  }
</script>

{#if !ready}
  <p class="muted">{t.common.loading}</p>
{:else if !$identity}
  <p class="muted">{t.common.identity_required}</p>
{:else}
  <!-- Folder delete confirmation -->
  {#if pendingDeleteFs}
    <div class="overlay" role="dialog">
      <div class="dialog">
        <h3>{t.devices.delete_confirm_title}</h3>
        <p>{t.devices.delete_confirm_body({ name: pendingDeleteFs })}</p>
        <div class="dialog-actions">
          <button class="btn btn-danger" data-testid={IDS.FOLDER_DELETE_CONFIRM} onclick={confirmDeleteFolder}>{t.common.delete}</button>
          <button class="btn" onclick={() => (pendingDeleteFs = null)}>{t.common.cancel}</button>
        </div>
      </div>
    </div>
  {/if}

  <!-- The declassify confirm — picking Public arms, this commits. Both
       consequences are stated because each surprised readers on its own: names
       and paths go public too (they become the address of each file), and
       flipping back re-seals only FUTURE content. -->
  {#if pendingPublicFs}
    <div class="overlay" role="dialog">
      <div class="dialog">
        <h3>{t.devices.declassify_title}</h3>
        <p>{t.devices.declassify_body}</p>
        <p>{t.devices.declassify_irreversible}</p>
        <div class="dialog-actions">
          <button
            class="btn btn-danger"
            data-testid={IDS.FOLDER_AUDIENCE_PUBLIC_CONFIRM}
            onclick={confirmPublicAudience}>{t.devices.declassify_confirm}</button
          >
          <button class="btn" onclick={() => (pendingPublicFs = null)}>{t.common.cancel}</button>
        </div>
      </div>
    </div>
  {/if}

  <!-- The content-residency confirm (folders re-model phase 5 — file-sync.md §
       Content residency) — the declassify confirm's twin. Picking Metadata-only
       arms, this commits: the nest's copy of the folder's content is deleted
       now, and the owner's devices become the only holders. -->
  {#if pendingResidencyFs}
    <div class="overlay" role="dialog">
      <div class="dialog">
        <h3>{t.devices.residency_confirm_title}</h3>
        <p>{t.devices.residency_confirm_body}</p>
        <div class="dialog-actions">
          <button
            class="btn btn-danger"
            data-testid={IDS.FOLDER_RESIDENCY_CONFIRM}
            onclick={confirmResidency}>{t.devices.residency_confirm}</button
          >
          <button class="btn" onclick={() => (pendingResidencyFs = null)}>{t.common.cancel}</button>
        </div>
      </div>
    </div>
  {/if}

  <!-- ====== CONFLICTS (per-set; absorbs the retired standalone conflicts page) ====== -->
  {#if (snap?.conflicts.length ?? 0) > 0}
    <section class="conflicts-section">
      <div class="conflicts-banner">
        <span class="conflicts-icon">!</span>
        <span>{t.devices.conflicts.title}</span>
      </div>
      <table class="conflicts-table">
        <thead>
          <tr>
            <th>{t.devices.conflicts.col_file}</th>
            <th>{t.devices.conflicts.col_type}</th>
            <th>{t.devices.conflicts.device}</th>
            <th>{t.devices.conflicts.col_time}</th>
            <th></th>
          </tr>
        </thead>
        <tbody>
          {#each snap?.conflicts ?? [] as conflict}
            <tr>
              <td class="conflict-path" data-testid={IDS.CONFLICT_FILE_INFO} title={conflict.path}>{conflict.file_info}</td>
              <td>
                <span class="conflict-type" data-testid={IDS.CONFLICT_TYPE_BADGE} class:binary={conflict.conflict_type === 'binary_copy'} class:merge={conflict.conflict_type === 'merge_markers'}>
                  {resolveLocalized(conflictBadgeLabel(conflict.resolution, conflict.resolved_at, conflict.conflict_type))}
                </span>
              </td>
              <td>{shortId(conflict.device_id)}</td>
              <td class="nowrap">{formatTime(conflict.created_at)}</td>
              <td>
                {#if conflict.has_other_version}
                  <!-- Auto-resolved with a retained loser: the one-tap re-point
                       (the § File Versions restore — itself reversible). -->
                  <button
                    class="btn btn-small"
                    data-testid={IDS.CONFLICT_RESOLVE_BUTTON}
                    onclick={() => useOtherVersion(conflict.id)}
                  >{t.devices.conflicts.use_other_version}</button>
                {:else if !conflict.resolved_at}
                  <!-- Unresolved report (the engine's degraded path when a candidate upload fails): informational
                       only — resolution happens on the detecting device; the
                       blocking chooser is retired. -->
                  <span class="candidate-detail">{t.devices.conflicts.awaiting_device}</span>
                {/if}
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </section>
  {/if}

  <!-- ====== SHARED WITH YOU (recipient-side pending knocks) ======
       folders.md § Sharing — *Recipient side*. Page-level, above the list: a
       knocked share is NOT a folder of yours until you accept, so it must never
       render as a `folder-row` ("a stranger cannot force a set into your list").
       Absent entirely when there are no knocks. -->
  {#if pendingShares.length}
    <section class="section">
      <div class="section-header">
        <h2>{t.devices.shared_with_you}</h2>
      </div>
      <table>
        <tbody>
          {#each pendingShares as share}
            <tr data-testid={IDS.FOLDER_PENDING_SHARE}>
              <td>
                {share.setName ?? t.common.unknown}
                <span class="muted"
                  >{t.devices.shared_by({ who: pendingShareSharer(share) })}</span
                >
              </td>
              <td class="num">
                <button
                  class="btn btn-small btn-primary"
                  data-testid={IDS.FOLDER_SHARE_ACCEPT_BUTTON}
                  disabled={pendingBusy}
                  onclick={() => acceptPendingShare(share)}>{t.common.accept}</button
                >
                <button
                  class="btn btn-small"
                  data-testid={IDS.FOLDER_SHARE_DECLINE_BUTTON}
                  disabled={pendingBusy}
                  onclick={() => declinePendingShare(share)}>{t.common.decline}</button
                >
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </section>
  {/if}

  <!-- ====== FILE SETS ====== -->
  <section class="section">
    <div class="section-header">
      <h2>{t.devices.folders}</h2>
      <button class="btn btn-primary" data-testid={IDS.FOLDER_ADD_BUTTON} onclick={openWizard}>{t.devices.add_folder}</button>
    </div>

    {#if (snap?.folders.length ?? 0) === 0}
      <p class="muted">{t.common.no_folders_configured}</p>
    {:else}
      <table>
        <thead>
          <tr>
            <th>{t.devices.wizard.review_name}</th>
            <th class="num">{t.devices.wizard.review_devices}</th>
            <th class="num">{t.devices.col_snapshots}</th>
            <th class="num">{t.devices.col_size}</th>
          </tr>
        </thead>
        <tbody>
          {#each snap?.folders ?? [] as fs}
            <!-- A `role == "member"` row is a set shared WITH you (B3): READ-ONLY —
                 no expander (so none of the owner-only config controls can render),
                 the recipient "Shared by ‹…›" badge, and a leave button. It only
                 reaches here at all once the join-filter above confirmed this client
                 actually joined the MLS group. -->
            <tr
              class:clickable={!isMemberRow(fs)}
              data-testid={IDS.FOLDER_ROW}
              onclick={() => { if (!isMemberRow(fs)) toggleExpand(fs); }}
            >
              <td>
                {fs.name}
                {#if isMemberRow(fs)}
                  <span class="badge shared-badge" data-testid={IDS.FOLDER_SHARED_BADGE}
                    >{t.devices.shared_by({ who: fs.owner_display || t.common.unknown })}</span
                  >
                  <button
                    class="btn btn-small"
                    data-testid={IDS.FOLDER_LEAVE_BUTTON}
                    disabled={pendingBusy}
                    onclick={(e) => { e.stopPropagation(); void leaveSharedSet(fs); }}
                    >{t.common.leave}</button
                  >
                {:else if sharedMembers(fs.name).length}
                  <span class="badge shared-badge" data-testid={IDS.FOLDER_SHARED_BADGE}
                    >{t.devices.shared_badge({ count: String(sharedMembers(fs.name).length) })}</span
                  >
                {/if}
              </td>
              <td class="num">{members(fs.name).length}</td>
              <td class="num">{fs.cached_snapshot_count ?? 0}</td>
              <td class="num">{byteSize(fs.cached_total_bytes)}</td>
            </tr>
            {#if expandedFs === fs.name}
              <tr class="expanded-row">
                <td colspan="4">
                  <div class="fs-detail">
                    <div class="fs-info-grid">
                      <!-- Phase 4 slice 4d — audience + website serving. Both render on
                           ANY folder: a folder has no type since slice e, and the
                           website toggle is what restores website-folder creation. -->
                      <label class="fs-select" title={t.devices.folder_audience_hint}>
                        <strong>{t.devices.folder_audience}:</strong>
                        <select
                          data-testid={IDS.FOLDER_AUDIENCE_SELECT}
                          value={normalizeAudience(fs.audience ?? '', !!fs.mls_group_id)}
                          onchange={(e) => onAudiencePicked(fs, e.currentTarget)}
                        >
                          {#each audienceOptions(!!fs.mls_group_id, normalizeAudience(fs.audience ?? '', !!fs.mls_group_id)) as opt}
                            <option value={opt.value} disabled={!opt.selectable}>
                              {resolveLocalized(opt.label)}
                            </option>
                          {/each}
                        </select>
                        {#if fs.mls_group_id}
                          <!-- The shared (bound, current) hint: while the bound folder
                               is public it explains the one exit its picker offers —
                               picking Shared re-seals it for its members. -->
                          <span class="fs-webdav-needs-mail">
                            {resolveLocalized(
                              audienceHint(true, normalizeAudience(fs.audience ?? '', true)),
                            )}
                          </span>
                        {/if}
                      </label>
                      <label class="fs-webdav">
                        <input
                          type="checkbox"
                          data-testid={IDS.FOLDER_WEBSITE_TOGGLE}
                          checked={fs.website_enabled ?? false}
                          onchange={(e) => changeWebsite(fs.name, e.currentTarget.checked)}
                        />
                        <strong>{t.devices.serve_website}</strong>
                        <!-- The TRI-state, shared: no readable audience / address ON /
                             address OFF / address unknown. Never a hand-rolled two-state
                             if-else — the degrade direction is load-bearing, since
                             unknown must not claim the site is live. -->
                        <span class="fs-webdav-needs-mail">
                          {resolveLocalized(
                            websiteServeHint(
                              normalizeAudience(fs.audience ?? '', !!fs.mls_group_id),
                              !!fs.web_paywall_tier,
                              snap?.website_address_enabled,
                            ),
                          )}
                        </span>
                      </label>
                      <!-- The per-row scan-frequency select that used to sit
                           here retired with phase 5: the cadence is a constant,
                           not a choice (file-sync.md § Config, the phase-5
                           block). -->
                      <!-- Conflict policy + WebDAV: every owner row (a folder has no
                           type — `ui/folders.md` § Conflicts, `webdav-server.md` § What
                           the namespace is); an expanded row is always an owner's. -->
                      {#if !isMemberRow(fs)}
                        <label class="fs-select">
                          <strong>{t.devices.conflict_policy}:</strong>
                          <select
                            data-testid={IDS.FOLDER_CONFLICT_POLICY_SELECT}
                            value={fs.conflict_policy ?? 'auto'}
                            onchange={(e) => changeConflictPolicy(fs.name, e.currentTarget.value)}
                          >
                            {#each conflictPolicyOptions() as opt}
                              <option value={opt.value}>{resolveLocalized(opt.label)}</option>
                            {/each}
                          </select>
                        </label>
                        <label
                          class="fs-webdav"
                          title={canServeWebdav
                            ? t.devices.serve_webdav_hint
                            : t.devices.serve_webdav_needs_mail}
                        >
                          <input
                            type="checkbox"
                            data-testid={IDS.FOLDER_WEBDAV_TOGGLE}
                            checked={fs.webdav_enabled ?? false}
                            disabled={!canServeWebdav}
                            onchange={(e) => changeWebdav(fs.name, fs.mls_group_id, e.currentTarget.checked)}
                          />
                          <strong>{t.devices.serve_webdav}</strong>
                          {#if !canServeWebdav}
                            <span class="fs-webdav-needs-mail">{t.devices.serve_webdav_needs_mail}</span>
                          {/if}
                        </label>
                      {/if}
                      {#if fs.website_enabled}
                        <label
                          class="fs-select"
                          title={ownTiers.length ? t.devices.paywall_tier_hint : t.devices.paywall_tier_needs_tier}
                        >
                          <strong>{t.devices.paywall_tier}:</strong>
                          <select
                            data-testid={IDS.FOLDER_PAYWALL_TIER_SELECT}
                            value={fs.web_paywall_tier ?? ''}
                            disabled={!ownTiers.length}
                            onchange={(e) => changePaywallTier(fs.name, fs.mls_group_id, e.currentTarget.value)}
                          >
                            {#if !fs.web_paywall_tier}
                              <option value="">{t.devices.paywall_tier_none}</option>
                            {/if}
                            {#each paywallTierValues(fs) as tier}
                              <option value={tier}>{tier}</option>
                            {/each}
                          </select>
                          {#if !ownTiers.length}
                            <span class="fs-webdav-needs-mail">{t.devices.paywall_tier_needs_tier}</span>
                          {/if}
                        </label>
                      {/if}
                    </div>

                    <h3>{t.devices.enrolled_devices}</h3>
                    {#if members(fs.name).length}
                      <table class="inner-table">
                        <thead>
                          <tr><th>{t.devices.conflicts.device}</th><th>{t.devices.col_role}</th></tr>
                        </thead>
                        <tbody>
                          {#each members(fs.name) as member}
                            <tr>
                              <td>{member.label}</td>
                              <td><span class="badge">{member.place}</span></td>
                            </tr>
                          {/each}
                        </tbody>
                      </table>
                    {:else}
                      <p class="muted">{t.devices.no_devices_enrolled}</p>
                    {/if}

                    <!-- The post-create device-place editor (folder-place-row +
                         its three checkboxes) — each enrolled SEAT's place,
                         edited in place through fauna.folders.places.set, the
                         same boxes and words the wizard's enrollment step
                         paints (ui/folders.md § Implementation status today).
                         The rows come from the shared `placeRows` projection,
                         re-read after every write so the boxes show the nest's
                         answer rather than an optimistic flip. -->
                    {#if (placeRowsBySet[fs.name] ?? []).length}
                      <h3>{t.devices.folder_places_title}</h3>
                      {#each placeRowsBySet[fs.name] as place, j}
                        <!-- The row root is the scope its checkboxes hang under
                             (folder-row[i] -> folder-place-row[j], the shape
                             ui.yaml's registry names). -->
                        <div class="place-row" data-testid={IDS.FOLDER_PLACE_ROW}>
                          <span class="place-label">{place.label}</span>
                          {#each PLACE_FLAGS as flag}
                            <label class="place-flag">
                              <!-- `data-state` is not decoration: the cross-app
                                   `get_attr(id, "state")` contract every flag
                                   checkbox carries reads a DOM ATTRIBUTE (the
                                   web bridge tries `state` then `data-state`),
                                   and a checkbox's `checked` is a PROPERTY, so
                                   without this the read comes back None on web
                                   while the box paints correctly — which is how
                                   it failed on this leg's first e2e run. -->
                              <input
                                type="checkbox"
                                data-testid={`folder-place-${flag.id}`}
                                data-state={flag.readRow(place) ? 'on' : 'off'}
                                checked={flag.readRow(place)}
                                onchange={() => togglePlaceFlag(fs.name, place, flag.wire)}
                              />
                              {flag.label()}
                            </label>
                            <span class="mode-desc">{flag.desc()}</span>
                          {/each}
                        </div>
                      {/each}
                    {/if}

                    <!-- Per-set device activity — the ordinary sync change signal
                         (distinct from cached_snapshot_count/cached_total_bytes, which
                         are snapshot-only), read via fauna.folders.devices.
                         Lazy-loaded on expand, refreshed on fauna.sync.changed
                         (file-sync.md § Implementation status today). -->
                    <h3>{t.devices.device_activity}</h3>
                    {#if (deviceActivity[fs.name] ?? []).length}
                      <table class="inner-table">
                        <thead>
                          <tr><th>{t.devices.conflicts.device}</th><th>{t.devices.col_changes}</th></tr>
                        </thead>
                        <tbody>
                          {#each deviceActivity[fs.name] ?? [] as d (d.device_id)}
                            <tr data-testid={IDS.FOLDER_DEVICE_ACTIVITY_ITEM}>
                              <td data-testid={IDS.FOLDER_DEVICE_ACTIVITY_LABEL}>{d.label}</td>
                              <td data-testid={IDS.FOLDER_DEVICE_ACTIVITY_COUNT}>{d.change_count}</td>
                            </tr>
                          {/each}
                        </tbody>
                      </table>
                    {:else}
                      <p class="muted">{t.devices.no_device_activity}</p>
                    {/if}

                    <!-- Cross-user sharing (owner side) — the "Shared with" section.
                         Distinct from the device roster above: these are OTHER USERS
                         the set is shared with, read via foldersActorMembers.
                         folders.md § Sharing. -->
                    <h3>{t.devices.shared_with}</h3>
                    {#if sharedMembers(fs.name).length}
                      <table class="inner-table shared-with-table">
                        <tbody>
                          {#each sharedMembers(fs.name) as member (member.actor_id)}
                            {@const publishedWarning = publishedWriterWarning(fs, member.access ?? 'reader')}
                            <tr data-testid={IDS.FOLDER_MEMBER_ITEM}>
                              <td data-testid={IDS.FOLDER_MEMBER_HANDLE}
                                >{accountDisplayLabel(member.handle, member.actor_id)}</td
                              >
                              <td data-testid={IDS.FOLDER_MEMBER_STATUS}>{t.common.active}</td>
                              <td>
                                <select
                                  data-testid={IDS.FOLDER_MEMBER_ROLE_SELECT}
                                  value={member.access ?? 'reader'}
                                  onchange={(e) =>
                                    changeMemberAccess(
                                      fs,
                                      member,
                                      e.currentTarget.value,
                                      member.byte_cap ?? undefined,
                                    )}
                                >
                                  {#each memberAccessOptions() as opt}
                                    <option value={opt.value}>{resolveLocalized(opt.label)}</option>
                                  {/each}
                                </select>
                              </td>
                              <td>
                                <input
                                  type="text"
                                  class="cap-input"
                                  data-testid={IDS.FOLDER_MEMBER_CAP_INPUT}
                                  value={member.byte_cap != null ? String(member.byte_cap) : ''}
                                  placeholder={t.devices.member_byte_cap_placeholder}
                                  onchange={(e) =>
                                    changeMemberAccess(
                                      fs,
                                      member,
                                      member.access ?? 'reader',
                                      parseCountI64(e.currentTarget.value),
                                    )}
                                />
                                {#if member.access === 'writer' && member.byte_cap == null}
                                  <span class="fs-warning" data-testid={IDS.FOLDER_WRITER_UNCAPPED_WARNING}
                                    >{t.devices.writer_uncapped_warning}</span
                                  >
                                {/if}
                                {#if publishedWarning}
                                  <span class="fs-warning" data-testid={IDS.FOLDER_WRITER_PUBLISHED_WARNING}
                                    >{publishedWarning}</span
                                  >
                                {/if}
                              </td>
                              <td>
                                <button
                                  class="btn btn-small btn-danger"
                                  data-testid={IDS.FOLDER_MEMBER_REMOVE_BUTTON}
                                  onclick={() => removeMember(fs, member)}>{t.devices.remove_member}</button
                                >
                              </td>
                            </tr>
                          {/each}
                        </tbody>
                      </table>
                    {:else}
                      <p class="muted">{t.devices.not_shared_yet}</p>
                    {/if}
                    {#if shareDialogFs === fs.name}
                      {@const sharePublishedWarning = publishedWriterWarning(fs, shareAccess)}
                      <div class="share-form">
                        <input
                          type="text"
                          class="input"
                          data-testid={IDS.RECIPIENT_PICKER_INPUT}
                          bind:value={shareInput}
                          placeholder={t.conversations.unified.recipient_picker_placeholder}
                        />
                        <label class="fs-select">
                          <strong>{t.devices.member_access}:</strong>
                          <select data-testid={IDS.FOLDER_SHARE_ROLE_SELECT} bind:value={shareAccess}>
                            {#each memberAccessOptions() as opt}
                              <option value={opt.value}>{resolveLocalized(opt.label)}</option>
                            {/each}
                          </select>
                        </label>
                        {#if shareAccess === 'writer'}
                          <span class="fs-warning" data-testid={IDS.FOLDER_WRITER_UNCAPPED_WARNING}
                            >{t.devices.writer_uncapped_warning}</span
                          >
                        {/if}
                        {#if sharePublishedWarning}
                          <span class="fs-warning" data-testid={IDS.FOLDER_WRITER_PUBLISHED_WARNING}
                            >{sharePublishedWarning}</span
                          >
                        {/if}
                        <div class="share-form-actions">
                          <button
                            class="btn btn-small btn-primary"
                            data-testid={IDS.FOLDER_SHARE_CONFIRM}
                            disabled={sharing || !shareInput.trim()}
                            onclick={() => doShare(fs)}>{t.devices.share_button}</button
                          >
                          <button class="btn btn-small" onclick={cancelShare}>{t.common.cancel}</button>
                        </div>
                      </div>
                    {:else}
                      <div class="fs-actions">
                        <button
                          class="btn btn-small"
                          data-testid={IDS.FOLDER_SHARE_BUTTON}
                          onclick={() => openShareDialog(fs.name)}>{t.devices.share_button}</button
                        >
                      </div>
                    {/if}

                    <h3>{t.devices.selective_sync}</h3>
                    <div class="selective-sync">
                      <label class="field">
                        <span>{t.devices.include_paths}</span>
                        <input type="text" data-testid={IDS.FOLDER_INCLUDE_PATHS} bind:value={editIncludePaths} placeholder={t.devices.paths_placeholder} />
                      </label>
                      <label class="field">
                        <span>{t.devices.exclude_paths}</span>
                        <input type="text" data-testid={IDS.FOLDER_EXCLUDE_PATHS} bind:value={editExcludePaths} placeholder={t.devices.exclude_placeholder} />
                      </label>
                      <button
                        class="btn btn-small btn-primary"
                        data-testid={IDS.FOLDER_SAVE_PATHS}
                        disabled={savingPaths}
                        onclick={() => saveSelectivePaths(fs.name)}
                      >{savingPaths ? t.common.saving : t.devices.save_paths}</button>
                    </div>

                    <!--
                      Destination places (backup-destinations.md § Ordinary-folder
                      coverage) — attach/detach an enrolled backup destination to
                      THIS ordinary folder, from its own expanded owner row. The
                      whole section is hidden while `destinationPlaces` is empty —
                      no destination enrolled at all — since an affordance that
                      cannot work must not paint (mirrors linux's
                      `build_destination_places_section`). Always repaints from
                      the mutation's own re-read, never an optimistic flip.
                    -->
                    {#if (destinationPlaces[fs.name] ?? []).length}
                      {@const places = destinationPlaces[fs.name]}
                      {@const attached = places.filter((p) => p.attached)}
                      {@const attachable = places.filter((p) => !p.attached)}
                      <h3>{t.devices.folder_destinations_title}</h3>
                      {#if attached.length}
                        <table class="inner-table">
                          <tbody>
                            {#each attached as place (place.destination_id)}
                              <tr data-testid={IDS.FOLDER_DESTINATION_ROW}>
                                <td>{place.label}</td>
                                <td>
                                  <button
                                    class="btn btn-small btn-danger"
                                    data-testid={IDS.FOLDER_DESTINATION_DETACH_BUTTON}
                                    disabled={destinationBusy[fs.name]}
                                    onclick={() => detachFolderDestination(fs, place)}
                                    >{t.devices.folder_destination_detach}</button
                                  >
                                </td>
                              </tr>
                            {/each}
                          </tbody>
                        </table>
                      {/if}
                      {#if attachable.length}
                        {@const selected =
                          destinationAttachSelect[fs.name] ?? attachable[0].destination_id}
                        <div class="fs-actions">
                          <select
                            data-testid={IDS.FOLDER_DESTINATION_ATTACH_SELECT}
                            value={selected}
                            onchange={(e) =>
                              (destinationAttachSelect[fs.name] = e.currentTarget.value)}
                          >
                            {#each attachable as place}
                              <option value={place.destination_id}>{place.label}</option>
                            {/each}
                          </select>
                          <button
                            class="btn btn-small"
                            data-testid={IDS.FOLDER_DESTINATION_ATTACH_BUTTON}
                            disabled={destinationBusy[fs.name]}
                            onclick={() => attachFolderDestination(fs, selected)}
                            >{t.devices.folder_destination_attach}</button
                          >
                        </div>
                      {/if}
                    {/if}

                    <!--
                      The nest place's snapshot policy (backup-restore.md § 8b),
                      on EVERY folder rather than only a backup-type one — the
                      point of moving retention off the wizard. Three knobs, each
                      three-state: the select spells its third state out, and for
                      the other two a BLANK box IS that state. All four are
                      staged and saved together, because the nest applies the
                      policy WHOLE — an apply-on-change knob would have to send
                      its siblings with it, committing half-typed values.
                    -->
                    <h3>{t.devices.nest_place_section}</h3>
                    <div class="nest-place">
                      <label class="field">
                        <span>{t.devices.nest_snapshots}</span>
                        <select
                          data-testid={IDS.FOLDER_NEST_SNAPSHOTS_SELECT}
                          value={nestPlaceEdit.snapshots}
                          onchange={(e) => (nestPlaceEdit = { ...nestPlaceEdit, snapshots: e.currentTarget.value })}
                        >
                          {#each nestSnapshotsOptions() as opt}
                            <option value={opt.value}>{resolveLocalized(opt.label)}</option>
                          {/each}
                        </select>
                      </label>
                      <label class="field">
                        <span>{t.devices.nest_quiet}</span>
                        <input type="text" data-testid={IDS.FOLDER_NEST_QUIET_INPUT} bind:value={nestPlaceEdit.quiet_secs} />
                      </label>
                      <label class="field">
                        <span>{t.devices.nest_retention_snapshots}</span>
                        <input type="text" data-testid={IDS.FOLDER_NEST_RETENTION_SNAPSHOTS} bind:value={nestPlaceEdit.retention_snapshots} />
                      </label>
                      <label class="field">
                        <span>{t.devices.nest_retention_days}</span>
                        <input type="text" data-testid={IDS.FOLDER_NEST_RETENTION_DAYS} bind:value={nestPlaceEdit.retention_days} />
                      </label>
                      <!-- The version-retention SIBLING pair —
                           bounds file-version history, never snapshots
                           (file-versions.md § Retention ruling 1); its own
                           folders.version_retention wire field, riding the
                           SAME folder-nest-save-button click. -->
                      <label class="field">
                        <span>{t.devices.version_retention_count}</span>
                        <input type="text" data-testid={IDS.FOLDER_VERSION_RETENTION_COUNT} bind:value={versionRetentionEdit.count} />
                      </label>
                      <label class="field">
                        <span>{t.devices.version_retention_days}</span>
                        <input type="text" data-testid={IDS.FOLDER_VERSION_RETENTION_DAYS} bind:value={versionRetentionEdit.days} />
                      </label>
                      <!-- Not decoration: the only on-screen statement that
                           emptying a box is a real choice, not a no-op. -->
                      <p class="muted">{t.devices.nest_place_blank_hint}</p>
                      <button
                        class="btn btn-small btn-primary"
                        data-testid={IDS.FOLDER_NEST_SAVE_BUTTON}
                        disabled={savingNestPlace}
                        onclick={() => saveNestPlace(fs.name)}
                      >{savingNestPlace ? t.common.saving : t.devices.nest_save}</button>

                      <!-- The nest place's content residency (folders re-model
                           phase 5 — file-sync.md § Content residency). Applies
                           ON CHANGE like the audience select above, and like
                           it the destructive direction only ARMS the confirm
                           (`pendingResidencyFs`) — Metadata-only deletes the
                           nest's copy of the folder's content. Its own
                           fauna.folders.update field, deliberately OUTSIDE the
                           batched save above: an older writer's policy edit
                           must never silently clear it. -->
                      <label class="fs-select">
                        <strong>{t.devices.folder_residency}:</strong>
                        <select
                          data-testid={IDS.FOLDER_NEST_RESIDENCY_SELECT}
                          value={normalizeResidency(fs.residency ?? '')}
                          onchange={(e) => onResidencyPicked(fs, e.currentTarget)}
                        >
                          {#each residencyOptions() as opt}
                            <option value={opt.value}>{resolveLocalized(opt.label)}</option>
                          {/each}
                        </select>
                        <!-- Keyed on the same NORMALIZED value the select paints, so
                             copy and control cannot disagree: full states what the
                             nest's copy buys, metadata-only states the availability
                             cost accepted. -->
                        <span class="fs-webdav-needs-mail">
                          {resolveLocalized(residencyHint(normalizeResidency(fs.residency ?? '')))}
                        </span>
                      </label>
                    </div>

                    <div class="fs-actions">
                      <button class="btn btn-small btn-danger" data-testid={IDS.FOLDER_DELETE_BUTTON} onclick={() => requestDeleteFolder(fs.name)}>{t.devices.delete_folder}</button>
                    </div>
                  </div>
                </td>
              </tr>
            {/if}
          {/each}
        </tbody>
      </table>
    {/if}
  </section>

  <!-- ====== FOLDERS YOU FOLLOW (phase 4 slice 4f-iii) ======
       `ui/folders.md` § Following a public folder. Three shapes the trickle-down
       must keep, and this render keeps all three:
         (1) the SECTION IS ALWAYS OFFERED, even with no follows — the button is
             how a user gets their first one, so gating it on a non-empty list
             would make it unreachable;
         (2) a followed folder is a row in its OWN list, never a `folder-row` —
             it has no roster, no binding and no seat, so a `folder-row` would
             open an expander full of controls that cannot apply to it;
         (3) the handle half REUSES `recipient-picker-input` (no second picker).
  -->
  <section class="section">
    <div class="section-header">
      <h2>{t.devices.followed_folders_section}</h2>
      <button class="btn" data-testid={IDS.FOLDER_FOLLOW_BUTTON} onclick={openFollowForm}
        >{t.devices.follow_public_folder}</button
      >
    </div>
    <p class="muted">{t.devices.follow_public_folder_hint}</p>

    {#if followOpen}
      <div class="share-form">
        <input
          type="text"
          class="input"
          data-testid={IDS.RECIPIENT_PICKER_INPUT}
          bind:value={followHandleInput}
          placeholder={t.conversations.unified.recipient_picker_placeholder}
        />
        <input
          type="text"
          class="input"
          data-testid={IDS.FOLDER_FOLLOW_NAME_INPUT}
          bind:value={followNameInput}
          placeholder={t.devices.follow_folder_name}
        />
        <span class="muted">{t.devices.follow_folder_name_hint}</span>
        <div class="share-form-actions">
          <button
            class="btn btn-small btn-primary"
            data-testid={IDS.FOLDER_FOLLOW_CONFIRM}
            disabled={following || !followHandleInput.trim() || !followNameInput.trim()}
            onclick={doFollow}>{t.devices.follow_confirm}</button
          >
          <button class="btn btn-small" onclick={cancelFollow}>{t.common.cancel}</button>
        </div>
      </div>
    {/if}

    {#if (snap?.followed.length ?? 0) > 0}
      <ul class="followed-list">
        {#each snap?.followed ?? [] as f}
          <!-- The row is the scope its children resolve under, mirroring
               `folder-row`, so `folder-followed-item[k] / folder-unfollow-button`
               addresses one row. `available: false` is the REVOKE — the owner
               flipped the audience back or deleted the folder — and the row stays
               visible and loud until the user removes it, because a re-flip
               resumes it under the same `folder_id`. -->
          <li class="followed-row" data-testid={IDS.FOLDER_FOLLOWED_ITEM}>
            {f.display_name}
            <!-- WHOSE folder it is rides the row's own text (name + owner +
                 badge + status), so the one read every app offers carries it.
                 `owner_display` is the shared precomputed label — the handle
                 while it still names the owner, else the id's short form —
                 painted as given, never re-derived here. -->
            <span class="muted followed-owner">{t.devices.followed_owner({ owner: f.owner_display })}</span>
            <span class="badge shared-badge">{t.devices.followed_public_badge}</span>
            <span class="followed-status" data-testid={IDS.FOLDER_FOLLOWED_STATUS}
              >{f.available
                ? t.devices.followed_status_following
                : t.devices.followed_status_unavailable}</span
            >
            {#if !f.available}
              <!-- Names BOTH causes (unshared / removed) because the follower
                   genuinely cannot tell them apart — the home nest folds them —
                   and the difference does not change what the user can do. -->
              <span class="fs-warning">{t.devices.followed_unavailable_hint}</span>
            {/if}
            <button
              class="btn btn-small btn-danger"
              data-testid={IDS.FOLDER_UNFOLLOW_BUTTON}
              disabled={unfollowing}
              onclick={() => doUnfollow(f)}>{t.devices.unfollow_folder}</button
            >
          </li>
        {/each}
      </ul>
    {/if}
  </section>

  <!-- ====== SYNC DEFAULTS ====== -->
  <section class="section">
    <div class="section-header">
      <h2>{t.devices.sync_defaults}</h2>
    </div>
    <label class="fs-select">
      <strong>{t.devices.default_conflict_policy}:</strong>
      <select
        data-testid={IDS.SYNC_DEFAULT_CONFLICT_POLICY_SELECT}
        value={defaultConflictPolicy}
        onchange={(e) => changeDefaultConflictPolicy(e.currentTarget.value)}
      >
        {#each conflictPolicyOptions() as opt}
          <option value={opt.value}>{resolveLocalized(opt.label)}</option>
        {/each}
      </select>
    </label>
  </section>

  <!-- ====== WIZARD ====== -->
  {#if snap && snap.wizard}
    {@const wiz = snap.wizard}
    {@const wizErr = wiz.review.error ? resolveLocalized(wiz.review.error) : ''}
    <div class="overlay" role="dialog">
      <div class="wizard">
        <div class="wizard-header">
          <h2>{t.devices.wizard.new_folder}</h2>
          <button class="close-btn" onclick={closeWizard}>&times;</button>
        </div>

        <div class="wizard-steps">
          {#each [1, 2, 3] as s}
            <span class="step" class:active={STEP_NUM[wiz.step] === s} class:done={STEP_NUM[wiz.step] > s}>{s}</span>
          {/each}
        </div>

        {#if wizErr}
          <p class="error">{wizErr}</p>
        {/if}

        <!--
          Step 1: Name. A folder has NO TYPE (folders re-model phase 2 slice e),
          so `wizard-mode-*` and `wizard-mode-backup-warning` are retired and
          this step is the name and nothing else. New folders are created `sync`.
          ⚠ Retiring `wizard-mode-web` leaves website-folder creation unreachable
          until phase 4 mints the website toggle — surfaced to the user and
          accepted 2026-08-15. Do NOT re-add a mode control to close it.
        -->
        {#if wiz.step === 'Name'}
          <div class="wizard-body">
            <label class="field">
              <span>{t.devices.wizard.review_name}</span>
              <input type="text" data-testid={IDS.WIZARD_NAME_INPUT} value={wiz.name.name} oninput={(e) => wizardSetName(e.currentTarget.value)} placeholder={t.devices.wizard.name_placeholder} />
            </label>
            {#if !wiz.name.continue_enabled}
              <p class="muted">{t.devices.wizard.name_required}</p>
            {/if}
          </div>

        <!--
          Step 2 (`Devices`): enrollment + each seat's three place flags —
          checkboxes that each say what they do.
        -->
        {:else if wiz.step === 'Devices'}
          <div class="wizard-body">
            <p class="muted">{t.devices.wizard.select_devices_roles}</p>
            {#if wiz.device_places.devices.length === 0}
              <p class="muted">{t.devices.wizard.no_devices_available}</p>
            {:else}
              {#each wiz.device_places.devices as wd, i}
                <div class="device-check">
                  <label>
                    <input type="checkbox" data-testid={IDS.WIZARD_DEVICE_CHECK} checked={wd.selected} onchange={() => wizardToggleDevice(i)} />
                    {wd.label}
                  </label>
                </div>
                {#each PLACE_FLAGS as flag}
                  <label class="place-flag">
                    <!-- `data-state` for the same reason the editor's boxes
                         carry it: `wizard_set_device_flag` reads
                         get_attr(id, "state") and clicks only when it DIFFERS,
                         and web's bridge reads a DOM attribute, not the
                         `checked` property. Without it that read is None, never
                         matches, and the helper clicks every time — so driving
                         it twice undoes the caller's intent, which is exactly
                         what its docstring promises it will not do. -->
                    <input
                      type="checkbox"
                      data-testid={`wizard-device-${flag.id}`}
                      data-state={flag.read(wd) ? 'on' : 'off'}
                      checked={flag.read(wd)}
                      onchange={(e) => wizardSetDeviceFlag(i, flag.id, e.currentTarget.checked)}
                    />
                    {flag.label()}
                  </label>
                  <!-- Each box carries its own explainer: the flags are what is
                       being explained now, so it is per box, not per seat. -->
                  <span class="mode-desc">{flag.desc()}</span>
                {/each}
              {/each}
            {/if}
          </div>

        <!-- Step 3: Review. (The scan-frequency step that used to sit between
             devices and review retired with phase 5; retention left in slice e
             — it is the nest place's policy, editable on ANY folder's row.) -->
        {:else if wiz.step === 'Review'}
          <div class="wizard-body">
            <h3>{t.devices.wizard.review}</h3>
            <!-- No mode line, no retention line, no cadence line: a folder has
                 no type (slice e), retention is the nest place's policy edited
                 on the row, and the scan cadence is a constant (phase 5). The
                 enrolled list names devices, not roles — a seat's place is
                 three flags, which one review noun cannot summarize honestly. -->
            <div class="review-grid">
              <div><strong>{t.devices.wizard.review_name}:</strong> {wiz.review.name}</div>
              <div>
                <strong>{t.devices.wizard.review_devices}:</strong>
                {#each wiz.review.enrolled as d}
                  <span class="badge">{d.label}</span>
                {:else}
                  <span class="muted">{t.devices.wizard.review_no_devices}</span>
                {/each}
              </div>
            </div>
          </div>
        {/if}

        <div class="wizard-footer">
          {#if wiz.step !== 'Name'}
            <button class="btn" data-testid={IDS.WIZARD_BACK_BUTTON} onclick={wizardBack}>{t.devices.wizard.back}</button>
          {:else}
            <div></div>
          {/if}
          {#if wiz.step !== 'Review'}
            <button
              class="btn btn-primary"
              data-testid={IDS.WIZARD_NEXT_BUTTON}
              disabled={!continueEnabled(wiz)}
              onclick={wizardNext}
            >{t.devices.wizard.next}</button>
          {:else}
            <button
              class="btn btn-primary"
              data-testid={IDS.WIZARD_CREATE_BUTTON}
              disabled={!wiz.review.create_enabled || wiz.review.phase === 'Submitting'}
              onclick={wizardCreate}
            >{wiz.review.phase === 'Submitting' ? t.common.creating : t.devices.wizard.create}</button>
          {/if}
        </div>
      </div>
    </div>
  {/if}
{/if}

<style>
  h2 { font-size: 1.25rem; margin-bottom: 0.5rem; }
  h3 { font-size: 1rem; margin: 1rem 0 0.5rem; }
  .muted { color: var(--text-muted); }
  .error { color: var(--danger); margin: 0 1.25rem 0.75rem; }

  .section { margin-bottom: 2rem; }
  .section-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 0.75rem;
  }

  /* === Folder table === */
  table { width: 100%; border-collapse: collapse; }
  th, td { text-align: left; padding: 0.5rem 0.75rem; border-bottom: 1px solid var(--border); }
  .num { text-align: right; }
  .clickable { cursor: pointer; }
  .clickable:hover { background: var(--bg-hover); }
  .expanded-row td { padding: 0; }
  .inner-table { margin: 0; }
  .inner-table th,
  .inner-table td { border-bottom: 1px solid var(--border); }

  .fs-detail {
    padding: 1rem 1.5rem;
    background: var(--bg-surface);
    border-top: 1px solid var(--border);
  }
  .fs-info-grid {
    display: flex;
    flex-wrap: wrap;
    gap: 1rem 2rem;
    margin-bottom: 0.75rem;
    font-size: 0.875rem;
  }
  .fs-select {
    display: inline-flex;
    align-items: center;
    gap: 0.5rem;
  }
  .fs-select select {
    padding: 0.25rem 0.5rem;
    background: var(--bg);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 4px;
    font-size: 0.8125rem;
  }
  .fs-select select:focus { outline: none; border-color: var(--accent); }
  .selective-sync { margin-bottom: 1rem; }
  .selective-sync .field { margin-bottom: 0.5rem; }
  .fs-actions {
    margin-top: 1rem;
    display: flex;
    gap: 0.5rem;
  }

  /* === Badges === */
  .badge {
    display: inline-block;
    padding: 0.125rem 0.5rem;
    border-radius: 4px;
    font-size: 0.75rem;
    background: var(--bg-hover);
    color: var(--text);
    border: 1px solid var(--border);
  }

  /* === Buttons === */
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
  .btn-primary {
    background: var(--accent);
    color: var(--bg);
    border-color: var(--accent);
  }
  .btn-primary:hover { background: var(--accent-hover); }
  .btn-danger {
    color: var(--danger);
    border-color: var(--danger);
    background: transparent;
  }
  .btn-danger:hover { background: rgba(248, 81, 73, 0.1); }
  .btn-small { padding: 0.25rem 0.625rem; font-size: 0.8125rem; }

  /* === Overlay / Dialog === */
  .overlay {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.6);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 100;
  }
  .dialog {
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 1.5rem;
    max-width: 400px;
    width: 90%;
  }
  .dialog p { margin-bottom: 1rem; }
  .dialog-actions {
    display: flex;
    gap: 0.5rem;
    justify-content: flex-end;
  }

  /* === Wizard === */
  .wizard {
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: 8px;
    width: 90%;
    max-width: 520px;
    max-height: 90vh;
    overflow-y: auto;
  }
  .wizard-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 1rem 1.25rem;
    border-bottom: 1px solid var(--border);
  }
  .wizard-header h2 { margin: 0; }
  .close-btn {
    background: none;
    border: none;
    color: var(--text-muted);
    font-size: 1.5rem;
    cursor: pointer;
  }
  .close-btn:hover { color: var(--text); }

  .wizard-steps {
    display: flex;
    gap: 0.5rem;
    justify-content: center;
    padding: 1rem;
  }
  .step {
    width: 28px;
    height: 28px;
    border-radius: 50%;
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 0.8125rem;
    font-weight: 600;
    background: var(--bg-hover);
    color: var(--text-muted);
    border: 1px solid var(--border);
  }
  .step.active {
    background: var(--accent);
    color: var(--bg);
    border-color: var(--accent);
  }
  .step.done {
    background: var(--success);
    color: var(--bg);
    border-color: var(--success);
  }

  .wizard-body { padding: 1rem 1.25rem; }
  .wizard-footer {
    display: flex;
    justify-content: space-between;
    padding: 1rem 1.25rem;
    border-top: 1px solid var(--border);
  }

  .field {
    display: block;
    margin-bottom: 1rem;
  }
  .field > span {
    display: block;
    font-size: 0.8125rem;
    color: var(--text-muted);
    margin-bottom: 0.375rem;
  }
  /* No `input[type='number']` arm: the only number inputs here were the
     wizard's retired retention boxes. The nest place's replacements are text
     inputs on purpose — blank is a real value there, and a number input's
     spinner/validation invites the browser to treat empty as zero. */
  .field input[type='text'] {
    width: 100%;
    padding: 0.5rem 0.75rem;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg);
    color: var(--text);
    font-size: 0.875rem;
  }
  .field input[type='text']:focus {
    outline: none;
    border-color: var(--accent);
  }

  /* `.mode-picker` / `.mode-option*` deleted with the wizard's type step
     (folders re-model phase 2 slice e) — a folder has no type. */
  .mode-desc {
    display: block;
    font-size: 0.75rem;
    color: var(--text-muted);
    margin-top: 0.25rem;
  }

  .device-check {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0.5rem 0;
    border-bottom: 1px solid var(--border);
  }
  /* The three place-flag checkboxes, indented under the seat they belong to —
     each with its own explainer line (`.mode-desc`) below it. */
  .place-flag {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    cursor: pointer;
    margin-left: 1.5rem;
    margin-top: 0.5rem;
  }
  .place-flag + .mode-desc {
    margin-left: 3rem;
    margin-top: 0.125rem;
  }
  /* One seat in the post-create place editor: the label, then the same three
     `.place-flag` boxes the wizard's enrollment step paints. Bordered like the
     sibling `.inner-table` rows so a folder's seats read as a list. */
  .place-row {
    padding: 0.5rem 0;
    border-bottom: 1px solid var(--border);
  }
  .place-row:last-child { border-bottom: none; }
  .place-label { font-weight: 600; }
  .device-check label {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    cursor: pointer;
  }

  .review-grid {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    font-size: 0.875rem;
  }
  .review-grid .badge { margin-left: 0.375rem; }

  /* === Conflicts === */
  .conflicts-section { margin-bottom: 1.5rem; }
  .conflicts-banner {
    width: 100%;
    display: flex;
    align-items: center;
    gap: 0.625rem;
    padding: 0.75rem 1rem;
    background: rgba(248, 81, 73, 0.08);
    border: 1px solid var(--danger);
    border-radius: 8px;
    color: var(--danger);
    cursor: default;
    font-size: 0.9375rem;
    font-weight: 600;
    text-align: left;
  }
  .conflicts-icon {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 22px;
    height: 22px;
    border-radius: 50%;
    background: var(--danger);
    color: white;
    font-size: 0.75rem;
    font-weight: 700;
    flex-shrink: 0;
  }
  .conflicts-table {
    width: 100%;
    border-collapse: collapse;
    margin-top: 0.5rem;
    font-size: 0.875rem;
  }
  .conflicts-table th,
  .conflicts-table td {
    text-align: left;
    padding: 0.375rem 0.625rem;
    border-bottom: 1px solid var(--border);
  }
  .conflict-path {
    max-width: 200px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .conflict-type {
    display: inline-block;
    padding: 0.125rem 0.5rem;
    border-radius: 4px;
    font-size: 0.75rem;
    font-weight: 600;
  }
  .conflict-type.binary {
    background: rgba(248, 81, 73, 0.15);
    color: var(--danger);
  }
  .conflict-type.merge {
    background: rgba(227, 179, 65, 0.15);
    color: #e3b341;
  }
  .nowrap { white-space: nowrap; }
  .candidate-list {
    display: flex;
    flex-direction: column;
    gap: 0.375rem;
  }
  .candidate-row {
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }
  .candidate-detail {
    font-size: 0.8125rem;
    color: var(--text-muted);
  }
  .fs-webdav-needs-mail {
    font-size: 0.8125rem;
    color: var(--text-muted);
  }

  /* === Cross-user sharing ("Shared with" section) === */
  .shared-badge {
    margin-left: 0.5rem;
    background: var(--accent-subtle, var(--bg-hover));
    border-color: var(--accent);
    color: var(--accent);
    vertical-align: middle;
  }
  .shared-with-table td { vertical-align: top; }
  .shared-with-table .cap-input {
    width: 8rem;
    padding: 0.25rem 0.5rem;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg);
    color: var(--text);
  }
  .fs-warning {
    display: block;
    margin-top: 0.25rem;
    font-size: 0.8125rem;
    color: var(--danger, #c0392b);
  }
  .share-form {
    margin-top: 0.75rem;
    padding: 0.75rem;
    border: 1px solid var(--border);
    border-radius: 6px;
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    max-width: 32rem;
  }
  .share-form input[type='text'] {
    padding: 0.375rem 0.5rem;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg);
    color: var(--text);
  }
  .share-form-actions {
    display: flex;
    gap: 0.5rem;
  }

  /* === Folders you follow (phase 4 slice 4f-iii) === */
  .followed-list {
    list-style: none;
    margin: 0.75rem 0 0;
    padding: 0;
  }
  .followed-row {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 0.5rem;
    padding: 0.5rem 0;
    border-bottom: 1px solid var(--border);
  }
  .followed-status {
    color: var(--text-muted, var(--text));
    font-size: 0.875rem;
  }

  /* The one narrow-screen rule this page had stacked `.mode-picker`, so it left
     with the wizard's type step — and with it the only `@media` block here. */
</style>
