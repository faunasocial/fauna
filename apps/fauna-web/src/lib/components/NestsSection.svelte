<script lang="ts">
  // User-settings "Nests" page: the nest-side sibling of the device roster. It
  // lists every nest the user's content lives on — their home nest and any
  // linked nests — with, on each row, a **trust facet**: what content-processing
  // the nest has been *trusted to read* (the grants the client minted to it),
  // with a per-row Now/History lens, renew/revoke controls, and the honest bound
  // of revocation. A dumb renderer of the shared trust-enabled
  // `LinkedNestsMachine` (`libs/fauna-client-pair`, WASM twin
  // `WasmLinkedNestsMachine`): build over the singleton WS-RPC client with the
  // trust seams → `hydrate()` (= fauna.pair.list + the home nest's holder
  // discovery + grant-log folds) → render `snapshot()` → `dispatch(action)` →
  // re-render. No trust/pairing logic in the SPA (priority #1/#2) — the shell
  // only maps scope tuples to localized labels and dispatches actions.
  //
  // v1 renders the trust facet for the home nest (its holder roster is directly
  // enumerable); linked nests show the `nest-trust-empty` state until
  // per-linked-nest holder enumeration lands. The **mint flow** (scope-first
  // picker, nests.md § Mint, ratified 2026-07-13, linux-lead) is a straight lift
  // of the linux `build_mint_flow`: it renders `row.mint_options` (the shared
  // `view_model::mint_options` catalog the WASM row already carries), maps each
  // to its localized use-case label, derives the holder from the scope choice,
  // and dispatches `Mint{nest_id, holder_bridge_id, scope}`. No mint/holder
  // logic in the SPA (priority #1/#2) — only label mapping + the dispatch.
  //
  // Behavior + IDs: docs/goal/ui/nests.md; linking half owned by
  // docs/goal/behavior/linked-nests.md; ui.yaml § nests.
  import { connectionStatus, identity } from '$lib/store';
  import { linkedNestsMachineWithTrust } from '$lib/rpc';
  import {
    ensureWasm,
    classifyLinkInput,
    shortNestId,
    isBoundedMailGrant,
    offlineAffordance,
    grantScopeLabels,
    type TrustFolder,
    statusLabel as wasmStatusLabel,
    backupStatusLabel as wasmBackupStatusLabel,
    mintOptionLabel as wasmMintOptionLabel,
    mintDurationOptions,
    durationLabel as wasmDurationLabel,
    type TrustGrantDuration,
    hexFull,
    shortId,
  } from '$lib/wasm';
  import { sessionDevicesMachine } from '$lib/devices-session';
  import { custodyDegradedBadgeKey, custodyFacetLoad, custodyRevoke } from '$lib/wasm-folders';
  import { custodyHeldBytesText, custodyReceiptStatusText } from '$lib/custody';
  import type { CustodyHolderRowView } from '$lib/devices-machine';
  import { makeOfflineGate } from '$lib/offline-gate';
  import { resolveLocalized, type LocalizedText } from '$lib/i18n/localized';
  import { byteSize } from '$lib/value-format';
  import {
    generationPathText,
    generationShowsRestore,
    generationNoticeText,
    type TrustGenerationRow,
    type TrustRestoreOutcome,
  } from '$lib/nests-generation';
  import { onDestroy, onMount } from 'svelte';
  import { afterNavigate } from '$app/navigation';
  import { onStoreChange } from '$lib/store-change';
  import { t } from '$lib/i18n/strings';
  import { IDS } from '$lib/generated/uiIds';
  import { forwardQueueView, type ForwardQueueStatus, type ForwardQueueView } from '$lib/nests-forward-queue';

  // W4 (account-data-plane.md § Workstreams) phase 4's UI desensitizing (`account-data-plane.md` § The
  // offline-mutation contract). The verdict is the shared rule's, never a class
  // test written here; `$lib/offline-gate` owns how a reactive tree obeys it.
  const offlineGate = makeOfflineGate(offlineAffordance, connectionStatus.subscribe);

  // The parent settings page owns the single error surface (its MessageBanner /
  // `error-message`); Nests-page dispatch errors flow into it.
  let { error = $bindable('') } = $props();

  // Shapes mirror the shared `LinkedNestsSnapshot` serde JSON (snake_case fields
  // across the serde_wasm_bindgen boundary). `Vec<u8>` handles (grant_id /
  // holder) cross as `number[]` and round-trip unchanged into dispatch. Typed
  // locally over the `any` snapshot.
  type TrustScope = { class: string; kind: string | null; tier: string | null };
  type TrustLiveness = 'Active' | 'ExpiringSoon' | 'Expired' | 'AutoRenewing';
  type TrustLens = 'Now' | 'History';
  type TrustEventKind = 'Mint' | 'Renew' | 'Revoke';
  interface TrustGrantRow {
    grant_id: number[];
    holder: number[];
    scope: TrustScope[];
    lasts_until: number;
    liveness: TrustLiveness;
    // The post-succession review mark, joined onto the row in shared Rust
    // (`fauna_client_pair::TrustGrantRow::unattested`, set by `project_grant`
    // off `GrantUnattestedMark::any_open`). It crosses the boundary already —
    // `snapshot()` serde-serializes the shared struct whole — so this line is
    // the whole of what web owed on the read side.
    //
    // ⚠ Deliberately NOT joined here from the two planes. The grant comes off
    // the live nest roster and the mark out of `fauna.state.blessed-nests`; the shared
    // projection joins them once, where both are in hand, rather than giving
    // each of the seven shells its own chance to get the join wrong.
    unattested: boolean;
    // The folder a folder read grant covers (the web-serve paywall grant),
    // resolved in shared Rust (`fauna_client_pair::TrustGrantRow::folder`);
    // absent/null for every other grant.
    folder?: TrustFolder | null;
  }
  interface TrustHistoryRow {
    grant_id: number[];
    holder: number[];
    kind: TrustEventKind;
    scope: TrustScope[];
    window_start: number;
    window_end: number;
    at: number;
    // `TrustGrantRow.folder` for the event's grant — a scopeless `Revoke` of
    // a named folder's grant is named too.
    folder?: TrustFolder | null;
  }
  // One mint-picker option (`nest-trust-mint-scope-select`) — the FFI/serde
  // projection of `view_model::MintOptionModel`, pre-resolved to the scope it
  // mints and the holder(s) that can take it (`nests.md` § Mint). Exactly one
  // `holder_candidates` entry ⇒ the shell mints to it directly; more ⇒ the
  // conditional `nest-trust-mint-holder-select` renders (unreachable today).
  // One backup trust row (`nest-trust-backup-item`, nests.md § Trust facet —
  // backup rows). Two kinds share the component: the `NestBackupKey` seal grant
  // (revoked at the home nest) and one writer-grant row per configured backup
  // destination (read AND revoked over that destination's own connection, which
  // is what keeps the affordance working with the source nest hostile).
  // Deliberately no lasts_until/liveness/History twin — both grants are standing
  // live nest reads, not folds of the signed grant-event log (nests.md:99).
  type TrustBackupKind = 'Seal' | 'Writer';
  // `Unreachable` ("we could not ask the destination") stays distinct from
  // `Missing` ("it answered and holds no such trust") — collapsing them would
  // let a flaky network read as a revoked backup.
  type TrustBackupStatus = 'Active' | 'Unreachable' | 'Missing';
  interface TrustBackupRow {
    kind: TrustBackupKind;
    status: TrustBackupStatus;
    // Empty on the seal row; names the destination a Writer row revokes at.
    destination_id: string;
    destination_label: string;
    // The writer grant's granted_at (epoch secs); null on the seal row, which
    // carries no timestamp on the wire — that leaf renders empty.
    since: number | null;
  }
  type TrustMintUseCase = 'Mail' | 'Calendar' | 'PaywalledPosts';
  interface TrustMintOption {
    use_case: TrustMintUseCase;
    tier: string | null;
    scope: TrustScope[];
    holder_candidates: string[];
  }
  // TrustGenerationRow / TrustRestoreOutcome (nests.md § Trust facet —
  // generation recovery) live in `$lib/nests-generation` — the pure render
  // logic there needs them unit-testable without a live Svelte context.
  interface NestRow {
    nest_id: string;
    capabilities: string[];
    /** What the capabilities line shows, one per capability (shared Rust's
     *  `capability_label`); the wire names above are never rendered. */
    capability_labels: LocalizedText[];
    expires_at: number | null;
    created_at: number;
    label: string | null;
    nest_url: string | null;
    is_home: boolean;
    trust_grants: TrustGrantRow[];
    trust_history: TrustHistoryRow[];
    // Populated on the HOME row only — both backup grants empower the source
    // nest, so no pairing row ever carries them (shared machine decides).
    trust_backups: TrustBackupRow[];
    // Populated on the HOME row only — recovery is addressed to the owner's
    // own destinations (nests.md § Trust facet — generation recovery).
    trust_generations: TrustGenerationRow[];
    lens: TrustLens;
    // The per-nest blessing and the mint picker's default duration it implies
    // (`nests.md` § Expiry / renewal → Duration and blessing).
    blessed: boolean;
    mint_default_duration: TrustGrantDuration;
    // The mint-picker catalog (`view_model::mint_options`, home row only in v1):
    // one entry per use case whose scope actually derives AND has a discoverable
    // holder. Empty ⇒ no mint affordance renders (never a picker that can only
    // error). Populated shared-side in `build_home_row`; carried across the WASM
    // serde boundary unchanged.
    mint_options: TrustMintOption[];
  }
  interface NestsSnapshot {
    home: NestRow | null;
    pairings: NestRow[];
    status: string;
    error: string | null;
    // What the last `RestoreGeneration` dispatch resolved to, or `null` when
    // the last action was not a restore. Cleared at the start of every
    // dispatch, so a stale "restored" can never describe a replaced action.
    restore_outcome: TrustRestoreOutcome | null;
    // The user's own post-forward queue (`nests.md` § Forward queue); `null`
    // when none is reported (reads as empty).
    forward_queue: ForwardQueueStatus | null;
  }

  let rows = $state<NestRow[]>([]);
  // Custodian nests (`nests.md` § Trust facet — custody rows): one `nests-item`
  // per NEST-anchored custody (the nest-custodian identity fact) — a friend's
  // nest holding sealed copies for this account. The rows come from the SAME
  // custody fold the Devices page paints (`custodyFacetLoad` over the session's
  // one `DevicesMachine`), which keeps exactly the complement: a custody never
  // renders on both pages. Web has no account store, so the escrow-holder
  // badge (`participant-escrow-holder-badge`) is a declared absence here
  // (`nests.md` § Implementation status today).
  let custodyNestRows = $state<CustodyHolderRowView[]>([]);
  let restoreOutcome = $state<TrustRestoreOutcome | null>(null);
  let forwardView = $state<ForwardQueueView | null>(null);
  let loading = $state(true);
  let addFormOpen = $state(false);
  let addInput = $state('');

  // Per-row mint-form state, keyed by nest_id (the mint flow can render on any
  // row whose `mint_options` is non-empty — home only in v1). `mintScopeLabel`
  // holds the selected `<option value>` (the localized label, `''` = the
  // placeholder); `mintHolder` the picked candidate on the conditional-holder
  // arm. Svelte 5 deep-proxied `$state`, so keyed writes are reactive.
  let mintOpen = $state<Record<string, boolean>>({});
  let mintScopeLabel = $state<Record<string, string>>({});
  let mintHolder = $state<Record<string, string>>({});
  // The duration select's value — the localized label (the cross-app select
  // contract), pre-set to the row's `mint_default_duration` on open.
  let mintDuration = $state<Record<string, string>>({});
  const durationText = (d: TrustGrantDuration): string => resolveLocalized(wasmDurationLabel(d));

  let machine: Awaited<ReturnType<typeof linkedNestsMachineWithTrust>> | null = null;

  function applySnapshot(): void {
    const snap = (machine?.snapshot() ?? null) as NestsSnapshot | null;
    // The home nest sits first in the list, then the pairings (nests.md § Layout).
    rows = [...(snap?.home ? [snap.home] : []), ...(snap?.pairings ?? [])];
    restoreOutcome = snap?.restore_outcome ?? null;
    forwardView = forwardQueueView(snap?.forward_queue);
    if (snap?.error) error = snap.error;
  }

  // A grant's scope line, via the shared `fauna_client_pair::grant_scope_labels`
  // (priority #2 — one shared mapping, not a per-app copy) that tui and linux
  // (native) and android (UniFFI) consume: it names a folder grant's folder
  // (`folder`, resolved in shared Rust) in place of the bare folder read.
  function scopeLine(scope: TrustScope[], folder: TrustFolder | null | undefined): string {
    return grantScopeLabels(scope, folder).map(resolveLocalized).join(', ');
  }

  // REQUIRED honest-bound copy (nests.md § Honest bound) — never over-promise.
  // A bounded (content-sealing-epochs) mail grant gets the stronger,
  // crypto-bounded wording WITH the honest INFO-A caveat; every other
  // kind/regime keeps the standing trust-until-revoke wording (flip-checklist
  // line 6). Never re-derive the (class, kind, tier) check here — the shared
  // `isBoundedMailGrant` predicate is the single source of truth (priority #2).
  function boundNote(scope: TrustScope[]): string {
    return isBoundedMailGrant(scope) ? t.nests.bound_note_bounded_mail : t.nests.bound_note_standing;
  }

  // Map a shared mint-picker option to its localized use-case label, via the
  // shared `fauna_client_pair::mint_option_label` map (priority #2) — the
  // same one linux/tui/android already consume.
  function mintOptionLabel(o: TrustMintOption): string {
    return resolveLocalized(wasmMintOptionLabel(o));
  }
  // Resolve the row's picked option from the selected label (`''`/placeholder ⇒
  // none, keeping the confirm button disabled). Labels are deterministic and
  // tier names unique, so label→option is unambiguous.
  function chosenMintOption(row: NestRow): TrustMintOption | null {
    const label = mintScopeLabel[row.nest_id];
    if (!label) return null;
    return row.mint_options.find((o) => mintOptionLabel(o) === label) ?? null;
  }

  // Trust timestamps (`lasts_until`, history `at`) are epoch **seconds** (the
  // shared row's wire unit); JS `Date` takes millis.
  function fmtEpochSecs(secs: number): string {
    return new Date(secs * 1000).toLocaleString();
  }

  // Via the shared `fauna_client_pair::status_label` map (priority #2) — the
  // same one linux/tui/android already consume.
  function statusLabel(l: TrustLiveness): string {
    return resolveLocalized(wasmStatusLabel(l));
  }

  function historyLine(h: TrustHistoryRow): string {
    // The history_* strings carry {scope}/{when} placeholders, so the i18n
    // generator emits them as functions taking a named-arg object.
    const args = { scope: scopeLine(h.scope, h.folder), when: fmtEpochSecs(h.at) };
    switch (h.kind) {
      case 'Mint':
        return t.nests.history_minted(args);
      case 'Renew':
        return t.nests.history_renewed(args);
      case 'Revoke':
        return t.nests.history_revoked(args);
    }
  }

  async function loadCustodyNests(): Promise<void> {
    const id = $identity;
    if (!id?.secretHex) return;
    const session = await sessionDevicesMachine(id.secretHex, () => {});
    session.unlisten();
    // A null fold is a transient (unreadable config this pass) — keep the rows
    // already painted rather than telling the owner their custodians are gone.
    const facet = await custodyFacetLoad(session.machine, id.secretHex);
    if (facet) custodyNestRows = facet.rows.filter((r) => r.custodian_nest_url !== null);
  }

  async function revokeCustodyNest(row: CustodyHolderRowView): Promise<void> {
    const id = $identity;
    if (!id?.secretHex) return;
    const session = await sessionDevicesMachine(id.secretHex, () => {});
    session.unlisten();
    // The row's own grant id + accept-bound custodian key, never an index; the
    // nest-before-record ordering lives in shared Rust. Never a silent drop
    // (convention 11): the act's error lands on the page's `error-message`,
    // and either way we refold so a revoked row drops at once.
    const err = await custodyRevoke(session.machine, id.secretHex, row.grant_id, row.custodian_key);
    if (err) error = err;
    await loadCustodyNests();
  }

  // Receipt timestamps cross as epoch seconds (wasm has no tz database), the
  // same formatting the Devices page's custody card uses.
  function formatWhen(secs: number): string {
    return new Date(secs * 1000).toLocaleString();
  }

  onMount(async () => {
    const id = $identity;
    if (!id?.secretHex) { loading = false; return; }
    try {
      await ensureWasm();
      machine = await linkedNestsMachineWithTrust(id.secretHex);
      // The `trust_facet_advance_clock` command's repaint (`$lib/trust-clock-e2e`):
      // installed before the first hydrate, so "the page is mounted" implies
      // "the hook is there". Test builds only (convention 15).
      if (__FAUNA_E2E_AUTOMATION__) {
        const { setTrustClockRehydrateHook } = await import('$lib/trust-clock-e2e');
        setTrustClockRehydrateHook(async () => {
          await machine?.hydrate();
          applySnapshot();
          await loadCustodyNests();
        });
      }
      await rehydrate();
      // The custody rows ride their own load on the same page edge, off the
      // linked rows' critical path: a slow config read must not hold the
      // page back, and a failure degrades to "no custodian nests shown".
      void loadCustodyNests().catch(() => {});
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  });

  // One read of the linked rows (the machine's `hydrate()` — `fauna.pair.list`,
  // the trust facet, and each backup destination's own writer list over its own
  // connection). Runs at mount AND on every later re-entry to this sub-page
  // (`afterNavigate` below): the backup rows are a per-VISIT read of each
  // destination (nests.md § Trust facet — backup rows), so a destination that
  // went down while the owner sat here must read `unreachable` on return, never
  // the previous visit's `Active`.
  //
  // ⚠ A SvelteKit `goto()` to the route this component is ALREADY mounted on
  // does not remount it (the parent's `{:else if current === 'nests'}` branch
  // stays satisfied), so `onMount` alone fires once per settings visit —
  // `afterNavigate` is the per-navigation-EVENT signal, the same fix as
  // `WebSettingsSection.svelte` / `ConnectedAppsSection.svelte`. A re-entry
  // while a read is still in flight joins it instead of starting another:
  // against a dead destination the read is a connect attempt, and restarting
  // it on every re-entry would never let it finish.
  let hydrating: Promise<void> | null = null;
  function rehydrate(): Promise<void> {
    if (!machine) return Promise.resolve();
    const m = machine;
    hydrating ??= (async () => {
      try {
        await m.hydrate();
        applySnapshot();
      } finally {
        hydrating = null;
      }
    })();
    return hydrating;
  }

  afterNavigate(() => {
    rehydrate().catch((e) => {
      error = e instanceof Error ? e.message : String(e);
    });
  });

  // The store-change notice (`$lib/store-change`): the custody facet folds
  // from the account store's records, so the open page re-folds it — a fold
  // only (a null fold keeps the rows). The linked-nest rows are NOT joined:
  // their hydrate is the machine's own nest round trip.
  onDestroy(onStoreChange(() => void loadCustodyNests().catch(() => {})));

  onDestroy(() => {
    // Drop the hook with the page, so the command never pokes a torn-down
    // closure whose `machine` is stale.
    if (__FAUNA_E2E_AUTOMATION__) {
      void import('$lib/trust-clock-e2e').then((m) => m.setTrustClockRehydrateHook(null));
    }
  });

  function openAddForm(): void {
    addFormOpen = true;
    addInput = '';
  }
  function cancelAdd(): void {
    addFormOpen = false;
    addInput = '';
  }
  async function submitAdd(): Promise<void> {
    const raw = addInput.trim();
    if (!raw || !machine) return;
    addFormOpen = false;
    addInput = '';
    try {
      // Shell-side classify (shared `classify_link_input`, identical on every
      // app): a 64-hex value is a nest **identity** → single-end `Link`; any
      // other value is a nest **address** → `LinkBoth`, which seeds the
      // authorization row on *both* nests in one action. Empty capabilities →
      // the nest fills in `default_self_sync()`.
      const input = classifyLinkInput(raw);
      if ('NestUrl' in input) {
        await machine.dispatch({
          LinkBoth: { other_nest_url: input.NestUrl.nest_url, capabilities: [], expires_at: null, label: null },
        });
      } else {
        await machine.dispatch({
          Link: { nest_id: input.NestId.nest_id, capabilities: [], expires_at: null, label: null, nest_url: null },
        });
      }
      applySnapshot();
    } catch (e) {
      applySnapshot();
      if (!error) error = e instanceof Error ? e.message : String(e);
    }
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
  const unlink = (nestId: string) => dispatch({ Unlink: { nest_id: nestId } });
  const retryForwards = () => dispatch('RetryForwards');
  const discardForwards = () => dispatch('DiscardForwards');
  const setLens = (nestId: string, lens: TrustLens) => dispatch({ SetLens: { nest_id: nestId, lens } });
  const renew = (grantId: number[]) => dispatch({ Renew: { grant_id: grantId } });
  const revoke = (grantId: number[]) => dispatch({ Revoke: { grant_id: grantId } });
  // `nest-trust-grant-keep-button`. Clears this row's review mark
  // (`LinkedNestsAction::KeepGrant` → the shared `keep_grant_action`), which is
  // a verdict AT REST, never a deletion: Keep closes the raising EVENT, so a
  // later succession legitimately re-raises the same row. There is no Keep-side
  // confirm — Keep is non-destructive and the row stays revocable forever after.
  const keepGrant = (grantId: number[]) => dispatch({ KeepGrant: { grant_id: grantId } });
  // `nest-trust-backup-revoke`. The seal press freezes the home nest's ability to
  // seal NEW backups; a writer press withdraws its authorization at ONE
  // destination, spoken over that destination's own connection. The shared
  // machine routes each to the right nest — this layer only names which row was
  // pressed (a unit serde variant crosses as the bare string).
  const revokeBackup = (b: TrustBackupRow) =>
    b.kind === 'Seal'
      ? dispatch('RevokeBackupSeal')
      : dispatch({ RevokeBackupWriter: { destination_id: b.destination_id } });
  // `nest-trust-generation-restore`. The address triple rides straight off the
  // row unchanged — never a row index, which would promote the wrong
  // generation the moment this flattened list is filtered or re-ordered.
  const restoreGeneration = (g: TrustGenerationRow) =>
    dispatch({
      RestoreGeneration: {
        destination_id: g.destination_id,
        folder_name: g.folder_name,
        path_hash: g.path_hash,
        manifest_hash: g.manifest_hash,
      },
    });

  // Localized `nest-trust-backup-status` label, via the shared
  // `fauna_client_pair::backup_status_label` map (priority #2) — the same one
  // linux/tui/android already consume.
  function backupStatusLabel(s: TrustBackupStatus): string {
    return resolveLocalized(wasmBackupStatusLabel(s));
  }

  function backupScopeText(b: TrustBackupRow): string {
    return b.kind === 'Seal'
      ? t.nests.backup_scope_seal
      : t.nests.backup_scope_writer({ destination: b.destination_label });
  }

  // The seal grant carries no granted_at, so its `since` leaf renders EMPTY —
  // the element is still present so a row's leaf set does not vary by kind.
  const backupSinceText = (b: TrustBackupRow): string =>
    b.since === null ? '' : `${t.nests.backup_since} ${fmtEpochSecs(b.since)}`;

  // Reveal the mint form; `''` selects the placeholder option deterministically.
  function openMint(nestId: string): void {
    mintOpen[nestId] = true;
    mintScopeLabel[nestId] = '';
    mintHolder[nestId] = '';
    const row = rows.find((r) => r.nest_id === nestId);
    mintDuration[nestId] = row ? durationText(row.mint_default_duration) : '';
  }
  // Confirm the mint: derive the holder from the scope choice (single candidate
  // ⇒ direct; >1 ⇒ the holder select's pick), dispatch `Mint`, collapse the
  // form. Scope crosses back as the same `{class,kind,tier}` tuples the row
  // carried — a straight round-trip into `Vec<TrustScope>`.
  async function confirmMint(row: NestRow): Promise<void> {
    const option = chosenMintOption(row);
    if (!option) return;
    let holderBridgeId: string;
    if (option.holder_candidates.length > 1) {
      const picked = mintHolder[row.nest_id];
      if (!picked) return;
      holderBridgeId = picked;
    } else {
      holderBridgeId = option.holder_candidates[0];
    }
    // The owner's pick, mapped back from its label; the row's default until
    // they choose.
    const duration =
      mintDurationOptions().find((d) => durationText(d) === mintDuration[row.nest_id]) ??
      row.mint_default_duration;
    mintOpen[row.nest_id] = false;
    mintScopeLabel[row.nest_id] = '';
    mintHolder[row.nest_id] = '';
    await dispatch({
      Mint: {
        nest_id: row.nest_id,
        holder_bridge_id: holderBridgeId,
        scope: option.scope,
        duration,
      },
    });
  }

  // The per-nest blessing (`nest-trust-blessed-toggle`): a `fauna.state.blessed-nests` write
  // through the shared machine; the re-render carries the new state.
  const setBlessed = (row: NestRow) =>
    dispatch({ SetBlessed: { nest_id: row.nest_id, blessed: !row.blessed } });
</script>

<section class="section" data-testid={IDS.NESTS_SECTION}>
  <h2>{t.nests.title}</h2>
  <p class="muted desc">{t.nests.description}</p>

  <!-- Add-a-nest form: the button reveals an inline input + submit/cancel.
       Submit dispatches Link / LinkBoth on the shared machine. -->
  <div class="add-nest">
    <!-- The opener STAYS RENDERED while the form is open — the shape tui, linux
         and apple all share ("`nests-add-button` reveals this form and stays
         live"). web used to swap it out via `{:else}`, which made the opener
         *absent* rather than live and so unassertable: `is_enabled` reads a
         missing element as false, which is indistinguishable from a gate
         greying it. Revealing the form is pure local state, so it needs no nest
         and declares no kind. -->
    <button class="btn primary" data-testid={IDS.NESTS_ADD_BUTTON} onclick={openAddForm}>
      {t.nests.add_button}
    </button>
    {#if addFormOpen}
      <div class="add-form">
        <input
          class="add-input"
          data-testid={IDS.NESTS_ADD_INPUT}
          placeholder={t.nests.add_input_placeholder}
          bind:value={addInput}
          onkeydown={(e) => { if (e.key === 'Enter') submitAdd(); }}
        />
        <button
          class="btn primary"
          data-testid={IDS.NESTS_ADD_SUBMIT_BUTTON}
          onclick={submitAdd}
          use:offlineGate={{ kind: 'fauna.pair.add', disabled: addInput.trim() === '' }}
        >
          {t.nests.add_submit}
        </button>
        <button class="btn" data-testid={IDS.NESTS_ADD_CANCEL_BUTTON} onclick={cancelAdd}>
          {t.nests.add_cancel}
        </button>
      </div>
    {/if}
  </div>

  <!-- Forward queue (page-level, conditional — nests.md § Forward queue): after
       the add form, before the nest rows, only while the connected nest reports
       posts of the user's still waiting to reach its relay. The reason is
       relay-chosen text: plain interpolation only (the shared projection has
       already control-stripped it). -->
  {#if forwardView}
    <div class="forward-queue">
      <p data-testid={IDS.NESTS_FORWARD_QUEUE}>{forwardView.summary}</p>
      {#if forwardView.reason}
        <p class="muted" data-testid={IDS.NESTS_FORWARD_QUEUE_REASON}>{forwardView.reason}</p>
      {/if}
      <div class="forward-actions">
        <button
          class="btn"
          data-testid={IDS.NESTS_FORWARD_RETRY_BUTTON}
          onclick={retryForwards}
          use:offlineGate={{ kind: 'fauna.pair.forward_retry' }}
        >
          {t.nests.forward_retry}
        </button>
        <button
          class="btn danger"
          data-testid={IDS.NESTS_FORWARD_DISCARD_BUTTON}
          onclick={discardForwards}
          use:offlineGate={{ kind: 'fauna.pair.forward_discard' }}
        >
          {t.nests.forward_discard}
        </button>
      </div>
    </div>
  {/if}

  {#if loading}
    <p class="muted">{t.nests.empty}</p>
  {:else if rows.length === 0 && custodyNestRows.length === 0}
    <p class="muted">{t.nests.empty}</p>
  {:else}
    <div class="nest-list">
      {#each rows as p (p.nest_id)}
        <div class="nest-item" data-testid={IDS.NESTS_ITEM}>
          <div class="nest-main">
            <span class="nest-label" data-testid={IDS.NESTS_ITEM_LABEL}>{p.label ?? shortNestId(p.nest_id)}</span>
            {#if !p.is_home}
              <button class="btn" data-testid={IDS.NESTS_ITEM_UNLINK_BUTTON} onclick={() => unlink(p.nest_id)}>
                {t.nests.unlink}
              </button>
            {/if}
          </div>
          <div class="nest-meta">
            <span class="mono" data-testid={IDS.NESTS_ITEM_NEST_ID}>{shortNestId(p.nest_id)}</span>
          </div>
          {#if !p.is_home}
            <div class="nest-meta">
              <span class="caption">{t.nests.capabilities_label}</span>
              <span data-testid={IDS.NESTS_ITEM_CAPABILITIES}>{p.capability_labels.map((l) => resolveLocalized(l)).join(', ')}</span>
            </div>
            <div class="nest-meta">
              <span class="caption">{t.nests.expiry_label}</span>
              <span data-testid={IDS.NESTS_ITEM_EXPIRY}>{p.expires_at ? new Date(p.expires_at).toLocaleString() : t.nests.expiry_never}</span>
            </div>
          {/if}

          <!-- Trust facet: per-row Now/History lens over the client-authoritative
               grant-event log (nests.md § Trust facet). -->
          <div class="trust-facet">
            <div class="lens-toggle" role="tablist">
              <button
                class="lens-btn"
                class:active={p.lens === 'Now'}
                data-testid={IDS.NEST_TRUST_VIEW_NOW}
                onclick={() => setLens(p.nest_id, 'Now')}
              >{t.nests.view_now}</button>
              <button
                class="lens-btn"
                class:active={p.lens === 'History'}
                data-testid={IDS.NEST_TRUST_VIEW_HISTORY}
                onclick={() => setLens(p.nest_id, 'History')}
              >{t.nests.view_history}</button>
            </div>

            {#if p.lens === 'Now'}
              <!-- `nest-trust-empty` claims the nest is trusted with NOTHING, so
                   a backup row suppresses it even with zero content grants
                   (nests.md:99) — a nest that seals and uploads your messages is
                   plainly trusted, and "not trusted to read anything" directly
                   above "Backs up your messages for you" contradicts the row
                   beneath it. -->
              {#if p.trust_grants.length === 0 && p.trust_backups.length === 0}
                <p class="muted trust-empty" data-testid={IDS.NEST_TRUST_EMPTY}>{t.nests.not_trusted}</p>
              {:else if p.trust_grants.length === 0}
                <!-- Backup rows only: the grant list is genuinely empty, and the
                     backup block below carries the facet. -->
              {:else}
                <div class="grant-list" data-testid={IDS.NEST_TRUST_GRANT_LIST}>
                  {#each p.trust_grants as g (g.grant_id.join(','))}
                    <div class="grant-item" data-testid={IDS.NEST_TRUST_GRANT_ITEM}>
                      <div class="grant-row">
                        <span data-testid={IDS.NEST_TRUST_GRANT_SCOPE}>{t.nests.trusted_to_read} {scopeLine(g.scope, g.folder)}</span>
                        <span class="badge status-{g.liveness.toLowerCase()}" data-testid={IDS.NEST_TRUST_GRANT_STATUS}>{statusLabel(g.liveness)}</span>
                      </div>
                      <div class="nest-meta">
                        <span class="caption">{t.nests.lasts_until}</span>
                        <span data-testid={IDS.NEST_TRUST_GRANT_LASTS_UNTIL}>{fmtEpochSecs(g.lasts_until)}</span>
                      </div>
                      <p class="bound-note" data-testid={IDS.NEST_TRUST_GRANT_BOUND_NOTE}>{boundNote(g.scope)}</p>
                      <!-- The post-succession review mark and its Keep half — rendered
                           only while this row is actually raised (succession-aftermath.md
                           § Adjudicating what the aftermath carries across). ABSENT rather
                           than empty otherwise: in a healthy account every grant is the
                           owner's own, so a permanently-present mark would train the user
                           straight past the one succession that matters.

                           This plane is the STRICTER of the two that carry such a pair. A
                           thief-added backup destination still only receives segments
                           sealed under a key it lacks; a thief-added grantee is handed live
                           READ capability by the successor's own client, so the mark is
                           mandatory here rather than defence-in-depth.

                           ⚠ Both leaves sit INSIDE grant-item, which is the scoping a
                           driver reads as a descendant of nest-trust-grant-item[i]. Pushed
                           flat they would index over raised rows while the item indexes
                           over all rows, and a scoped read would silently name a different
                           grant. tui expresses the same nesting through its element tree.

                           ⚠ Remove is deliberately NOT re-rendered — nest-trust-grant-revoke
                           below already is it, exactly as the backups plane's pair reuses
                           its own remove. Minting a second revocation path here is what the
                           § forbids. -->
                      {#if g.unattested}
                        <p class="bound-note" data-testid={IDS.NEST_TRUST_GRANT_UNATTESTED_MARK}>{t.nests.grant_unattested_mark}</p>
                      {/if}
                      <div class="grant-actions">
                        {#if g.unattested}
                          <button class="btn" data-testid={IDS.NEST_TRUST_GRANT_KEEP_BUTTON} onclick={() => keepGrant(g.grant_id)}>{t.nests.grant_keep_button}</button>
                        {/if}
                        <button class="btn" data-testid={IDS.NEST_TRUST_GRANT_RENEW} onclick={() => renew(g.grant_id)}>{t.nests.renew}</button>
                        <button class="btn danger" data-testid={IDS.NEST_TRUST_GRANT_REVOKE} onclick={() => revoke(g.grant_id)}>{t.nests.revoke}</button>
                      </div>
                    </div>
                  {/each}
                </div>
              {/if}

              <!-- Backup trust rows (nests.md § Trust facet — backup rows,
                   ratified 2026-07-24) — AFTER the content-processing grant
                   rows, home row only (the shared machine populates them
                   nowhere else, since both grants empower the source nest).
                   Outside the branch above because they render alongside
                   *either* arm: beside the grant list when content grants
                   exist, and on their own when none do. -->
              {#each p.trust_backups as b (b.kind + ':' + b.destination_id)}
                <div class="grant-item" data-testid={IDS.NEST_TRUST_BACKUP_ITEM}>
                  <div class="grant-row">
                    <span data-testid={IDS.NEST_TRUST_BACKUP_SCOPE}>{backupScopeText(b)}</span>
                    <span
                      class="badge status-{b.status.toLowerCase()}"
                      data-testid={IDS.NEST_TRUST_BACKUP_STATUS}
                    >{backupStatusLabel(b.status)}</span>
                  </div>
                  <div class="nest-meta">
                    <span data-testid={IDS.NEST_TRUST_BACKUP_SINCE}>{backupSinceText(b)}</span>
                  </div>
                  <!-- REQUIRED honest-bound copy — revoking freezes only NEW
                       writes; custody already held remains until the holder
                       reclaims it. Never over-promise. -->
                  <p class="bound-note" data-testid={IDS.NEST_TRUST_BACKUP_BOUND_NOTE}>
                    {b.kind === 'Seal'
                      ? t.nests.backup_bound_note_seal
                      : t.nests.backup_bound_note_writer}
                  </p>
                  <div class="grant-actions">
                    <button
                      class="btn danger"
                      data-testid={IDS.NEST_TRUST_BACKUP_REVOKE}
                      onclick={() => revokeBackup(b)}
                    >{t.nests.backup_revoke}</button>
                  </div>
                </div>
              {/each}

              <!-- Retained generations (nests.md § Trust facet — generation
                   recovery, ratified 2026-07-29) — AFTER the backup trust
                   rows, home row only (the shared machine populates them
                   nowhere else — recovery is addressed to the owner's own
                   destinations). Same both-arms placement as the backup rows
                   above: renders alongside either the grant list or the
                   empty state's suppression. -->
              {#each p.trust_generations as g (g.destination_id + ':' + g.path_hash)}
                <div class="grant-item" data-testid={IDS.NEST_TRUST_GENERATION_ITEM}>
                  <div class="grant-row">
                    <span class="heading" data-testid={IDS.NEST_TRUST_GENERATION_PATH}>{generationPathText(g)}</span>
                  </div>
                  <div class="nest-meta">
                    <span class="caption" data-testid={IDS.NEST_TRUST_GENERATION_SUPERSEDED}>
                      {g.status === 'Unreachable' ? '' : `${t.nests.generation_superseded} ${fmtEpochSecs(g.superseded_at)}`}
                    </span>
                  </div>
                  <div class="nest-meta">
                    <span class="caption" data-testid={IDS.NEST_TRUST_GENERATION_EXPIRES}>
                      {g.status === 'Unreachable' ? '' : t.nests.generation_expires({ when: fmtEpochSecs(g.expires_at) })}
                    </span>
                  </div>
                  <div class="nest-meta">
                    <span class="caption" data-testid={IDS.NEST_TRUST_GENERATION_SIZE}>
                      {g.status === 'Unreachable' ? '' : byteSize(g.size_bytes)}
                    </span>
                  </div>
                  <div class="nest-meta">
                    <span class="caption" data-testid={IDS.NEST_TRUST_GENERATION_STATUS}>
                      {g.status === 'Unreachable' ? t.nests.generation_status_unreachable : t.nests.generation_status_listed}
                    </span>
                  </div>
                  {#if generationShowsRestore(g)}
                    <div class="grant-actions">
                      <button
                        class="btn suggested-action"
                        data-testid={IDS.NEST_TRUST_GENERATION_RESTORE}
                        onclick={() => restoreGeneration(g)}
                      >{t.nests.generation_restore}</button>
                    </div>
                  {/if}
                </div>
              {/each}
              <!-- The restore-outcome notice (`nest-trust-generation-notice`,
                   ratified 2026-07-29) — home-row-scoped, NOT per-row: it
                   describes the page's LAST restore action, not any one
                   generation row. Registered whenever the home row's Now
                   lens renders, EMPTY until a restore resolves. Distinct from
                   `error-message`: only a genuinely failed call reaches that;
                   `PastRecoveryWindow` is a product state, never an error. -->
              {#if p.is_home}
                <p class="muted" data-testid={IDS.NEST_TRUST_GENERATION_NOTICE}>{generationNoticeText(restoreOutcome)}</p>
              {/if}

              <!-- Mint flow (scope-first picker, nests.md § Mint, ratified
                   2026-07-13) — rendered after the grant list / empty state, and
                   only when the shared option catalog is non-empty (empty ⇒
                   nothing derivable or no discoverable holder — never a picker
                   that can only error). The `nest-trust-mint-scope-select`
                   `<option value>` IS the localized label (the cross-app
                   select contract); the holder is derived from the scope choice,
                   with the conditional `nest-trust-mint-holder-select` rendered
                   only on the >1-candidate ambiguity arm (unreachable today). -->
              <!-- The per-nest blessing (nests.md § Expiry / renewal → Duration
                   and blessing) — home row only, like the rest of the facet;
                   `data-state` mirrors it for a driver (the toggle convention). -->
              {#if p.is_home}
                <label class="blessed-toggle">
                  <input
                    type="checkbox"
                    data-testid={IDS.NEST_TRUST_BLESSED_TOGGLE}
                    data-state={p.blessed ? 'on' : 'off'}
                    checked={p.blessed}
                    onclick={(e) => { e.preventDefault(); void setBlessed(p); }}
                  />
                  {t.nests.blessed_toggle}
                </label>
              {/if}
              {#if p.mint_options.length > 0}
                <div class="mint-flow">
                  <button
                    class="btn"
                    data-testid={IDS.NEST_TRUST_GRANT_MINT_BUTTON}
                    onclick={() => openMint(p.nest_id)}
                  >{t.nests.mint_button}</button>
                  {#if mintOpen[p.nest_id]}
                    {@const opt = chosenMintOption(p)}
                    <div class="mint-form">
                      <select
                        class="mint-select"
                        data-testid={IDS.NEST_TRUST_MINT_SCOPE_SELECT}
                        bind:value={mintScopeLabel[p.nest_id]}
                      >
                        <option value="">{t.nests.mint_scope_placeholder}</option>
                        {#each p.mint_options as o (mintOptionLabel(o))}
                          <option value={mintOptionLabel(o)}>{mintOptionLabel(o)}</option>
                        {/each}
                      </select>
                      <select
                        class="mint-select"
                        data-testid={IDS.NEST_TRUST_MINT_DURATION_SELECT}
                        bind:value={mintDuration[p.nest_id]}
                      >
                        {#each mintDurationOptions() as d (d)}
                          <option value={durationText(d)}>{durationText(d)}</option>
                        {/each}
                      </select>
                      {#if opt && opt.holder_candidates.length > 1}
                        <select
                          class="mint-select"
                          data-testid={IDS.NEST_TRUST_MINT_HOLDER_SELECT}
                          bind:value={mintHolder[p.nest_id]}
                        >
                          {#each opt.holder_candidates as h (h)}
                            <option value={h}>{h}</option>
                          {/each}
                        </select>
                      {/if}
                      <button
                        class="btn primary"
                        data-testid={IDS.NEST_TRUST_MINT_CONFIRM_BUTTON}
                        disabled={!opt}
                        onclick={() => confirmMint(p)}
                      >{t.nests.mint_confirm}</button>
                    </div>
                  {/if}
                </div>
              {/if}
            {:else}
              <div class="history-list" data-testid={IDS.NEST_TRUST_HISTORY_LIST}>
                {#each p.trust_history as h (h.at + ':' + h.kind + ':' + h.grant_id.join(','))}
                  <div class="history-item" data-testid={IDS.NEST_TRUST_HISTORY_ITEM}>{historyLine(h)}</div>
                {/each}
              </div>
            {/if}
          </div>
        </div>
      {/each}
      <!-- Custodian nests, after the linked rows (the tui order). The item
           anchor carries the honest-bound revoke note, stated beside the
           control it bounds (REQUIRED — nests.md § Trust facet — custody
           rows); the copy is the shared `devices.custody_*` strings. -->
      {#each custodyNestRows as c (c.grant_id.join(','))}
        <div class="nest-item" data-testid={IDS.NESTS_ITEM}>
          <div class="nest-main">
            <span class="nest-label" data-testid={IDS.NESTS_ITEM_LABEL}>{t.nests.custody_nest_label({ host: shortId(hexFull(Uint8Array.from(c.host))) })}</span>
          </div>
          <div class="trust-facet" data-testid={IDS.NEST_TRUST_CUSTODY_ITEM}>
            <span data-testid={IDS.NEST_TRUST_CUSTODY_SCOPE}>{t.devices.custody_holder_scope}</span>
            <!-- Three states, three strings — never collapsed, never empty. -->
            <span class:custody-stale={c.receipt_state === 'Stale'} data-testid={IDS.NEST_TRUST_CUSTODY_RECEIPT_STATUS}>
              {custodyReceiptStatusText(c.receipt, formatWhen)}
            </span>
            <span data-testid={IDS.NEST_TRUST_CUSTODY_HELD_BYTES}>
              {custodyHeldBytesText(c.receipt, custodyDegradedBadgeKey())}
            </span>
            <p class="caption">{t.devices.custody_revoke_bound_note}</p>
            <div>
              <button
                class="btn danger"
                data-testid={IDS.NEST_TRUST_CUSTODY_REVOKE_BUTTON}
                disabled={c.pending}
                onclick={() => revokeCustodyNest(c)}
              >{t.devices.custody_revoke}</button>
            </div>
          </div>
        </div>
      {/each}
    </div>
  {/if}
</section>

<style>
  .section { margin-bottom: 2rem; }
  h2 { margin-bottom: 0.25rem; font-size: 1.125rem; }
  .muted { color: var(--text-muted, #8b949e); }
  .desc { margin-bottom: 1rem; font-size: 0.875rem; }

  .add-nest { margin-bottom: 1rem; }
  .forward-queue { margin-bottom: 1rem; }
  .forward-actions { display: flex; gap: 0.5rem; }
  .add-form { display: flex; gap: 0.5rem; align-items: center; }
  .add-input {
    flex: 1 1 auto;
    padding: 0.375rem 0.625rem;
    border-radius: 6px;
    border: 1px solid var(--border, #30363d);
    background: var(--bg-surface, #161b22);
    color: var(--text, #e6edf3);
    font-size: 0.875rem;
  }

  .nest-list { display: flex; flex-direction: column; gap: 0.75rem; }
  .custody-stale { color: var(--danger); }
  .nest-item {
    border: 1px solid var(--border, #30363d);
    border-radius: 8px;
    background: var(--bg-surface, #161b22);
    padding: 1rem;
    display: flex;
    flex-direction: column;
    gap: 0.375rem;
  }
  .nest-main { display: flex; align-items: center; justify-content: space-between; }
  .nest-label { font-weight: 600; font-size: 1.05rem; }
  .nest-meta { display: flex; align-items: baseline; gap: 0.5rem; font-size: 0.8rem; }
  .caption { color: var(--text-muted, #8b949e); }
  .mono { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; word-break: break-all; color: var(--text-muted, #8b949e); }

  .trust-facet {
    margin-top: 0.5rem;
    padding-top: 0.5rem;
    border-top: 1px solid var(--border, #30363d);
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }
  .lens-toggle { display: flex; gap: 0.25rem; }
  .lens-btn {
    font-size: 0.75rem;
    padding: 0.2rem 0.6rem;
    border-radius: 999px;
    border: 1px solid var(--border, #30363d);
    background: transparent;
    color: var(--text-muted, #8b949e);
    cursor: pointer;
  }
  .lens-btn.active { background: var(--accent, #1f6feb); border-color: var(--accent, #1f6feb); color: #fff; }
  .trust-empty { font-size: 0.8rem; margin: 0; }

  .grant-list, .history-list { display: flex; flex-direction: column; gap: 0.5rem; }
  .grant-item {
    border: 1px solid var(--border, #30363d);
    border-radius: 6px;
    padding: 0.625rem;
    display: flex;
    flex-direction: column;
    gap: 0.375rem;
  }
  .grant-row { display: flex; align-items: center; justify-content: space-between; gap: 0.5rem; font-size: 0.85rem; }
  .badge {
    font-size: 0.7rem;
    padding: 0.1rem 0.5rem;
    border-radius: 999px;
    border: 1px solid var(--border, #30363d);
    color: var(--text-muted, #8b949e);
    white-space: nowrap;
  }
  .badge.status-active { color: #3fb950; border-color: #238636; }
  .badge.status-expiringsoon { color: #d29922; border-color: #9e6a03; }
  .badge.status-expired { color: #f85149; border-color: #da3633; }
  .badge.status-autorenewing { color: #58a6ff; border-color: #1f6feb; }
  .bound-note { font-size: 0.72rem; color: var(--text-muted, #8b949e); margin: 0; line-height: 1.35; }
  .grant-actions { display: flex; gap: 0.375rem; }
  .history-item { font-size: 0.8rem; color: var(--text, #e6edf3); }

  .mint-flow { display: flex; flex-direction: column; gap: 0.375rem; margin-top: 0.25rem; }
  .mint-form { display: flex; flex-wrap: wrap; align-items: center; gap: 0.375rem; }
  .mint-select {
    padding: 0.3rem 0.5rem;
    border-radius: 6px;
    border: 1px solid var(--border, #30363d);
    background: var(--bg-surface, #161b22);
    color: var(--text, #e6edf3);
    font-size: 0.8rem;
  }
  .btn:disabled { opacity: 0.5; cursor: not-allowed; }

  .btn {
    font-size: 0.8rem;
    padding: 0.25rem 0.625rem;
    border-radius: 6px;
    border: 1px solid var(--border, #30363d);
    background: transparent;
    color: var(--text, #e6edf3);
    cursor: pointer;
  }
  .btn.primary { background: var(--accent, #1f6feb); border-color: var(--accent, #1f6feb); color: #fff; }
  .btn.danger { color: #f85149; border-color: #da3633; }
</style>
