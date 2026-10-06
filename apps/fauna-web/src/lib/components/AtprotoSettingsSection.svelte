<script lang="ts">
  // Settings → AT Protocol — the ATProto integration-depth page
  // (docs/goal/ui/atproto.md). One page answers one question: *how deep is
  // this user's Bluesky integration?* The spine is the four-rung depth
  // selector (`atproto-depth-*`); every other control reveals below it as a
  // sub-setting of the level that makes it meaningful — the transition card,
  // the Linked-account panel (the shared `BridgeCard`, embedded verbatim —
  // zero new element IDs, ui/atproto.md § Element IDs), the hosted-identity
  // panel, and the full-PDS login-plane surface (app credentials +
  // connected-app sessions + the external-apps kill-switch), gated on
  // level = `hosted_full`. Mirrors `apps/fauna-linux/src/settings/atproto.rs`
  // (the reference implementation) and `apps/fauna-tui/src/settings/
  // bluesky.rs` (the closest architectural analog — a direct render over the
  // shared `AtprotoSettingsMachine`, re-snapshotting after every gesture).
  //
  // F1 collects no label/dm_allowed input at mint time — no such ui.yaml IDs
  // were approved, only `atproto-app-credential-mint` (a single button). This
  // page auto-labels ("App credential N") and defaults `dm_allowed` to
  // `false` (least privilege, matching the doc's default-closed posture).
  import { identity } from '$lib/store';
  import { sharedRpcPort } from '$lib/rpc';
  import { ensureWasm, atprotoDepthLevelOptions, type DepthLevelOption } from '$lib/wasm';
  import {
    getAtprotoSettingsMachine,
    ensureAtprotoSettingsWasm,
    atprotoSettingsPrefetchSnapshot,
    delegationCapabilityLabels,
    delegationStatusLabel,
    identityStatusLabel,
  } from '$lib/wasm-atproto-settings';
  import { resolveLocalized } from '$lib/i18n/localized';
  import { isSafeNavUrl } from '$lib/safe-url';
  import { onMount, onDestroy } from 'svelte';
  import { t } from '$lib/i18n/strings';
  import type { AtprotoSettingsSnapshot } from '$lib/atproto-settings-machine';
  import type { AtprotoSettingsMachine } from '../../../static/fauna_wasm_atproto_settings.js';
  import BridgeCard from './BridgeCard.svelte';
  import {
    listBridges,
    linkBridge,
    unlinkBridge,
    updateBridgeSettings,
    listBridgeFollows,
    addBridgeFollow,
    removeBridgeFollow,
    type BridgeInfo,
    type BridgeFollow,
  } from '$lib/bridges';
  import { IDS } from '$lib/generated/uiIds';

  let { error = $bindable('') } = $props();

  // The hosted rungs (`atproto-depth-hosted-visible`/`-hosted-full`) render
  // disabled from the moment `ready` flips true — before `snap` has ever been
  // populated by the first `machine.refresh()` (onMount sets `ready` right
  // after construction, then awaits `refresh()`). During that window the
  // disabled rungs must still carry an on-screen reason (`ui/README.md` § Copy
  // comprehensibility rule 5), so `onMount` seeds `snap` from the SHARED Rust
  // default via `atprotoSettingsPrefetchSnapshot()`.
  //
  // This used to be a local `PENDING_GATE_REASON` literal. It was deleted with
  // the other three apps' stand-ins: a hand-rolled pre-fetch state can express
  // a combination the record's own invariants forbid, and did — twice on
  // android, and windows shipped a pre-fetch gate that rendered OPEN. Reaching
  // the one ratified default across the seam is what removes that class; do not
  // reintroduce a literal here.

  let machine: AtprotoSettingsMachine | null = null;
  let snap = $state<AtprotoSettingsSnapshot | null>(null);
  let ready = $state(false);
  let timer: ReturnType<typeof setInterval> | null = null;
  // Secrets revealed (by mint or explicit reveal) this session, keyed by
  // credential_id. Never persisted, never part of the snapshot (D3) — the
  // reveal button's OWN text becomes the secret once revealed (no separate
  // secret-display id in F1's approved ui.yaml surface).
  let revealedSecrets = $state<Record<string, string>>({});

  // ── Linked-account panel: the shared "bluesky" BridgeCard ───────────────
  // A synthetic, unlinked placeholder — mirrors tui's `embed_bridge_card`
  // synthetic-status case (`apps/fauna-tui/src/bridges.rs`): before the
  // bridges list has resolved, or on a nest built without the `bluesky`
  // provider feature, the panel still renders an honest unlinked surface
  // rather than vanishing.
  const SYNTHETIC_BLUESKY_BRIDGE: BridgeInfo = {
    id: 'bluesky',
    name: '',
    available: true,
    linked: false,
    identity: null,
    mode: null,
    settings: [],
    supports_follows: false,
    link_modes: null,
    error: null,
  };
  let blueskyBridge = $state<BridgeInfo | null>(null);
  let blueskyFollows = $state<BridgeFollow[]>([]);

  async function refreshLinkedPanel(): Promise<void> {
    const secret = $identity?.secretHex;
    if (!secret) return;
    try {
      const bridges = await listBridges(secret);
      const b = bridges.find((x) => x.id === 'bluesky') ?? null;
      blueskyBridge = b;
      blueskyFollows = b && b.linked && b.supports_follows ? await listBridgeFollows(secret, 'bluesky') : [];
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  async function handleBridgeLink(mode: string, fields: Record<string, string>): Promise<void> {
    const secret = $identity?.secretHex;
    if (!secret) return;
    try {
      const resp = await linkBridge(secret, 'bluesky', mode, fields);
      if (resp.redirect_url) {
        // The redirect target is nest-supplied; refuse a non-https scheme
        // (security.md § Transport trust).
        if (!isSafeNavUrl(resp.redirect_url)) {
          error = t.bridges.unsafe_redirect;
          return;
        }
        window.location.href = resp.redirect_url;
        return;
      }
      await refreshLinkedPanel();
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  async function handleBridgeUnlink(): Promise<void> {
    const secret = $identity?.secretHex;
    if (!secret) return;
    try {
      await unlinkBridge(secret, 'bluesky');
      await refreshLinkedPanel();
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  async function handleBridgeSettingChange(key: string, value: unknown): Promise<void> {
    const secret = $identity?.secretHex;
    if (!secret) return;
    try {
      await updateBridgeSettings(secret, 'bluesky', { [key]: value });
      await refreshLinkedPanel();
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  async function handleBridgeAddFollow(id: string, petname?: string): Promise<void> {
    const secret = $identity?.secretHex;
    if (!secret) return;
    try {
      await addBridgeFollow(secret, 'bluesky', id, petname);
      await refreshLinkedPanel();
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  async function handleBridgeRemoveFollow(fId: string): Promise<void> {
    const secret = $identity?.secretHex;
    if (!secret) return;
    try {
      await removeBridgeFollow(secret, 'bluesky', fId);
      await refreshLinkedPanel();
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  // Lazily populate the Linked panel the moment the level first reads
  // `linked`; the callbacks above keep it in sync thereafter (mirrors
  // routes/bridges/+page.svelte's own refresh-after-mutation shape).
  $effect(() => {
    if (snap?.level === 'linked') {
      refreshLinkedPanel();
    } else {
      blueskyBridge = null;
      blueskyFollows = [];
    }
  });

  function applySnapshot(): void {
    if (!machine) return;
    const raw = machine.snapshotJson();
    snap = raw ? (JSON.parse(raw) as AtprotoSettingsSnapshot) : null;
    error = snap?.error ? resolveLocalized(snap.error) : '';
  }

  onMount(async () => {
    // Seed the pre-fetch state from the SHARED Rust default FIRST, before the
    // identity check below can return early — so there is exactly one source of
    // pre-fetch truth on this page and the signed-out path paints off it too.
    try {
      await ensureAtprotoSettingsWasm();
      snap = atprotoSettingsPrefetchSnapshot();
      depthLevels = atprotoDepthLevelOptions();
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : String(e);
    }

    const id = $identity;
    if (!id) {
      ready = true;
      return;
    }
    try {
      await ensureWasm();
      machine = await getAtprotoSettingsMachine(
        { onChanged: () => applySnapshot() },
        await sharedRpcPort(id.secretHex),
        id.secretHex,
      );
      ready = true;
      // The D10 lapse journey's rehydrate hook — the browser shape of linux's
      // `notify_atproto_rehydrate` nudge. Installed the moment the machine
      // exists, BEFORE the first `refresh()`: "the page is mounted" (all a
      // driver's `navigate()` can observe) must imply "the hook is there", and
      // installing it after the load round-trip would make the command's
      // availability depend on wall-clock timing, which convention 14 forbids.
      // Test builds only (convention 15) — a production `vite build` folds the
      // constant to false and strips the import.
      if (__FAUNA_E2E_AUTOMATION__) {
        const { setAtprotoDelegationRehydrateHook } = await import('$lib/atproto-delegation-e2e');
        setAtprotoDelegationRehydrateHook(async () => {
          await machine?.refresh();
          applySnapshot();
        });
      }
      await machine.refresh();
      applySnapshot();
      // Explicit pull, not a bare `refresh()`: on a REUSED machine (see
      // `getAtprotoSettingsMachine`), the observer this mount registered was
      // never wired in, so nothing repaints `snap` unless this timer pulls
      // it itself.
      timer = setInterval(async () => { await machine?.refresh(); applySnapshot(); }, 15000);
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : String(e);
      ready = true;
    }
  });

  onDestroy(() => {
    if (timer) clearInterval(timer);
    // Drop the rehydrate hook with the page, so the command fails loudly ("the
    // AT Protocol settings page is not mounted") instead of poking a torn-down
    // closure whose `machine` reference is stale.
    if (__FAUNA_E2E_AUTOMATION__) {
      void import('$lib/atproto-delegation-e2e').then((m) =>
        m.setAtprotoDelegationRehydrateHook(null),
      );
    }
  });

  // ── The 72 h recovery-fork contest (`atproto-contest-*`) ──────────────
  //
  // Wholly client-side (`atproto-identity-custody.md` § The 72 h
  // recovery-fork contest, decision 9): every gesture is the browser's own
  // connection, never a nest round trip — `openContestConfirm`/
  // `cancelContest` are synchronous (the machine notifies the registered
  // observer itself, mirroring `cancelTransition` above), `requestContest`
  // is the one network round trip (to the public PLC directory).

  function openContest(): void {
    machine?.openContestConfirm(); // fires the registered observer → applySnapshot
    applySnapshot();
  }

  function cancelContest(): void {
    machine?.cancelContest(); // fires the registered observer → applySnapshot
    applySnapshot();
  }

  async function confirmContest(): Promise<void> {
    if (!machine) return;
    try {
      await machine.requestContest();
      applySnapshot();
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  // ── "Delete my Bluesky presence" (`atproto-delete-*`) ──────────────────
  //
  // Row 7's six-app trickle-down: open/cancel are pure-local machine
  // mutations (the machine notifies the registered observer itself, mirroring
  // `cancelTransition` above); confirm is the one network round trip (the
  // nest's `fauna.bridges.atproto.delete_presence`).

  function openDelete(): void {
    machine?.openDeleteConfirm(); // fires the registered observer → applySnapshot
    applySnapshot();
  }

  function cancelDelete(): void {
    machine?.cancelDelete(); // fires the registered observer → applySnapshot
    applySnapshot();
  }

  async function confirmDelete(): Promise<void> {
    if (!machine) return;
    try {
      await machine.confirmDelete();
      applySnapshot();
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  // ── The depth selector ───────────────────────────────────────────────
  // The rungs — order, ids, copy, and whether each is gate-subject — come from
  // the shared catalog (`atproto.md` § Where logic lives, which already named
  // the machine the owner of "level logic … all of it" while every app still
  // carried its own copy of this table). Filled in `onMount` once the wasm
  // module is loaded; empty before that, like the rest of the page.
  let depthLevels: DepthLevelOption[] = $state([]);

  // The raw gate (ignores whether the rung is currently active) — this is
  // what the `reason` attr always carries, mirroring linux/tui's
  // `set_test_attr(rung, "reason", if gated {...})`. `hosted` is the catalog's
  // fact now, not a `startsWith('hosted')` sniff this file re-runs.
  function isHostedRungGated(rung: DepthLevelOption): boolean {
    return rung.hosted && !(snap?.hosted_allowed ?? false);
  }

  // A hosted rung the user is ALREADY at stays selectable even when gated,
  // so a step-down is reachable if the domain later stops being public — the
  // gate only ever blocks ENTERING a hosted level from a lower one (mirrors
  // linux/tui's `!gated || active`).
  function isHostedRungEnabled(rung: DepthLevelOption): boolean {
    return !isHostedRungGated(rung) || snap?.level === rung.level;
  }

  async function selectDepth(level: string): Promise<void> {
    if (!machine) return;
    try {
      await machine.selectLevel(level);
      applySnapshot();
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  async function confirmTransition(): Promise<void> {
    if (!machine) return;
    try {
      await machine.confirmTransition();
      applySnapshot();
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  function cancelTransition(): void {
    machine?.cancelTransition(); // fires the registered observer → applySnapshot
    applySnapshot();
  }

  function setDidMethod(method: string): void {
    machine?.setDidMethod(method);
    applySnapshot();
  }

  function toggleHistoryBackfill(): void {
    machine?.setHistoryBackfill(!(snap?.history_backfill ?? false));
    applySnapshot();
  }

  async function mintCredential(): Promise<void> {
    if (!machine) return;
    const before = new Set((snap?.credentials ?? []).map((c) => c.credential_id));
    const label = t.atproto_settings.default_credential_label({
      count: String((snap?.credentials.length ?? 0) + 1),
    });
    try {
      const secret = await machine.mint(label, false);
      applySnapshot();
      const newRow = (snap?.credentials ?? []).find((c) => !before.has(c.credential_id));
      if (newRow) revealedSecrets = { ...revealedSecrets, [newRow.credential_id]: secret };
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  async function revealCredential(credentialId: string): Promise<void> {
    if (!machine || credentialId in revealedSecrets) return;
    try {
      const secret = await machine.revealSecret(credentialId);
      revealedSecrets = { ...revealedSecrets, [credentialId]: secret };
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  async function revokeCredential(credentialId: string): Promise<void> {
    await machine?.revoke(credentialId);
    applySnapshot();
  }

  async function toggleExternalApps(): Promise<void> {
    if (!machine) return;
    await machine.setExternalAppsEnabled(!(snap?.external_apps_enabled ?? true));
    applySnapshot();
  }

  // ── The D10 authoring-delegation row (atproto-pds-full.md § App surface) ──
  //
  // What authorizes an external ATProto app to *post* as this account, as
  // opposed to merely signing in (the kill-switch + credential + connected-app
  // groups above govern that). Six IDs user-approved 2026-07-29, `-last-used`
  // 2026-07-31; tui leads, linux and apple are the built references.
  //
  // Two states, one always-present control:
  //
  //  * `delegation == null` — none provisioned, OR a stored cert that failed the
  //    client-side verify under this account's own identity key. The row and its
  //    leaves are WITHHELD, never rendered as a grant the user cannot be shown to
  //    have made (the mismatch surfaces on `error-message`, which the machine has
  //    already set). Only `-authorize` renders.
  //  * `delegation != null` — the four leaves render. `-authorize` STAYS, because
  //    re-authorizing IS the renewal gesture: provisioning overwrites the cert,
  //    so a lapsed grant recovers in one gesture with no revoke first.
  async function authorizeExternalApps(): Promise<void> {
    if (!machine) return;
    await machine.authorizeExternalApps();
    applySnapshot();
  }

  async function deauthorizeExternalApps(): Promise<void> {
    if (!machine) return;
    await machine.deauthorizeExternalApps();
    applySnapshot();
  }

  // Both leaves go through the SHARED wire→i18n maps rather than a local
  // object literal: tui and linux call the same Rust
  // (`DelegationRow::{capability_labels,status_label}`), and a TS copy here
  // would be the fourth (priority #4). `$derived` so a snapshot fold repaints
  // them; the wasm calls are pure string maps, no round trip.
  let delegationScope = $derived(
    snap?.delegation
      ? delegationCapabilityLabels(snap.delegation.capabilities)
          .map((l) => resolveLocalized(l))
          .join(', ')
      : '',
  );
  let delegationStatus = $derived(
    snap?.delegation ? resolveLocalized(delegationStatusLabel(snap.delegation.liveness)) : '',
  );

  /** MICROseconds on this row — these come from the signed cert, not the wire,
   *  unlike the credential/session rows above, which carry milliseconds. */
  function fromCertMicros(micros: number): string {
    return new Date(micros / 1000).toLocaleString();
  }
</script>

{#if !ready}
  <p class="muted">{t.common.loading}</p>
{:else if !$identity}
  <p class="muted">{t.common.identity_required}</p>
{:else}
  <section class="section" data-testid={IDS.ATPROTO_PAGE}>
    <h2>{t.atproto_settings.title}</h2>

    <!-- ── The 72 h recovery-fork contest — LEADS the page, above the
         selector (behavior/atproto-identity-custody.md § The 72 h
         recovery-fork contest). Every string below is machine-composed and
         rendered verbatim — mirrors apps/fauna-linux/src/settings/
         bluesky.rs and apps/fauna-tui/src/settings/atproto.rs::
         contest_elements, the reference shape every shell copies. ── -->
    {#if snap?.contest}
      {@const contestCard = snap.contest}
      <div class="card" data-testid={IDS.ATPROTO_CONTEST_CARD} data-state={contestCard.state}>
        <h3>{t.atproto_settings.contest_card_heading}</h3>
        <p data-testid={IDS.ATPROTO_CONTEST_DETAIL}>{resolveLocalized(contestCard.detail)}</p>
        {#if contestCard.deadline}
          <p class="muted small" data-testid={IDS.ATPROTO_CONTEST_DEADLINE}>{resolveLocalized(contestCard.deadline)}</p>
        {/if}
        {#if contestCard.show_contest}
          <button class="btn danger" data-testid={IDS.ATPROTO_CONTEST} onclick={openContest}>
            {t.atproto_settings.contest_button}
          </button>
        {/if}
      </div>
      {#if snap.contest_confirm}
        {@const confirm = snap.contest_confirm}
        <div class="card" data-testid={IDS.ATPROTO_CONTEST_CONFIRM_CARD}>
          {#each confirm.lines as line}
            <p>{resolveLocalized(line)}</p>
          {/each}
          <div class="card-actions">
            <button class="btn" data-testid={IDS.ATPROTO_CONTEST_CANCEL} disabled={confirm.in_progress} onclick={cancelContest}>
              {t.atproto_settings.contest_cancel_button}
            </button>
            <button class="btn danger" data-testid={IDS.ATPROTO_CONTEST_CONFIRM} disabled={confirm.in_progress} onclick={confirmContest}>
              {t.atproto_settings.contest_confirm_button}
            </button>
          </div>
        </div>
      {/if}
    {/if}

    <!-- ── Depth selector ─────────────────────────────────────────────── -->
    <div class="depth-selector" data-testid={IDS.ATPROTO_DEPTH_SELECTOR} data-state={snap?.level ?? 'off'}>
      <h3>{t.atproto_settings.depth_heading}</h3>
      {#each depthLevels as rung (rung.level)}
        {@const active = snap?.level === rung.level}
        {@const gated = isHostedRungGated(rung)}
        <button
          class="depth-rung"
          class:active
          data-testid={rung.ui_id}
          data-state={active ? 'active' : 'inactive'}
          data-reason={gated ? 'gated' : 'ok'}
          disabled={!isHostedRungEnabled(rung)}
          onclick={() => selectDepth(rung.level)}
        >
          <span class="depth-title">{resolveLocalized(rung.title)}</span>
          <span class="depth-desc muted small">{resolveLocalized(rung.description)}</span>
        </button>
      {/each}
      {#if !(snap?.hosted_allowed ?? false) && snap?.hosted_gate_reason}
        <p class="muted small">{resolveLocalized(snap.hosted_gate_reason)}</p>
      {/if}
    </div>

    <!-- ── Transition card ────────────────────────────────────────────── -->
    {#if snap?.pending_transition}
      {@const card = snap.pending_transition}
      <div class="card" data-testid={IDS.ATPROTO_DEPTH_CONFIRM_CARD}>
        {#each card.lines as line}
          <p>{resolveLocalized(line)}</p>
        {/each}
        {#if card.show_history_backfill}
          <label class="toggle">
            <input
              type="checkbox"
              data-testid={IDS.ATPROTO_HISTORY_BACKFILL}
              checked={snap.history_backfill}
              onchange={toggleHistoryBackfill}
            />
            {t.atproto_settings.history_backfill_label}
          </label>
        {/if}
        <div class="card-actions">
          <button class="btn" data-testid={IDS.ATPROTO_DEPTH_CANCEL} disabled={card.in_progress} onclick={cancelTransition}>
            {t.atproto_settings.depth_cancel_button}
          </button>
          <button class="btn btn-primary" data-testid={IDS.ATPROTO_DEPTH_CONFIRM} disabled={card.in_progress} onclick={confirmTransition}>
            {t.atproto_settings.depth_confirm_button}
          </button>
        </div>
      </div>
    {/if}

    <!-- ── Linked-account panel: the shared bridge surface at level = linked ── -->
    {#if snap?.level === 'linked'}
      <BridgeCard
        bridge={blueskyBridge ?? SYNTHETIC_BLUESKY_BRIDGE}
        follows={blueskyFollows}
        onLink={handleBridgeLink}
        onUnlink={handleBridgeUnlink}
        onSettingChange={handleBridgeSettingChange}
        onAddFollow={handleBridgeAddFollow}
        onRemoveFollow={handleBridgeRemoveFollow}
      />
    {/if}

    <!-- ── Hosted panel: at (or entering) a hosted level ──────────────────── -->
    {#if snap && (snap.level.startsWith('hosted') || (snap.pending_transition?.target_level ?? '').startsWith('hosted'))}
      <div class="hosted-panel">
        {#if snap.show_did_method_radio}
          <div data-testid={IDS.ATPROTO_DID_METHOD}>
            <h4>{t.atproto_settings.did_method_heading}</h4>
            <button
              class="did-method-rung"
              class:active={snap.did_method === 'plc'}
              data-testid={IDS.ATPROTO_DID_METHOD_PLC}
              data-state={snap.did_method === 'plc' ? 'active' : 'inactive'}
              onclick={() => setDidMethod('plc')}
            >
              <span class="depth-title">{t.atproto_settings.did_method_plc_title}</span>
              <span class="depth-desc muted small">{t.atproto_settings.did_method_plc_desc}</span>
            </button>
            <button
              class="did-method-rung"
              class:active={snap.did_method === 'web'}
              data-testid={IDS.ATPROTO_DID_METHOD_WEB}
              data-state={snap.did_method === 'web' ? 'active' : 'inactive'}
              onclick={() => setDidMethod('web')}
            >
              <span class="depth-title">{t.atproto_settings.did_method_web_title}</span>
              <span class="depth-desc muted small">{t.atproto_settings.did_method_web_desc}</span>
            </button>
            {#if snap.handle_preview}
              <p class="muted small">{t.atproto_settings.handle_either_way({ handle: snap.handle_preview })}</p>
            {/if}
          </div>
        {/if}
      </div>
    {/if}

    <!-- ── The identity summary: gated on the IDENTITY, not the level ──────
         `ui/atproto.md` § Errors & edge cases: "A deactivated identity at
         level Off/Linked: the identity summary renders (marked deactivated)
         so the user can see what re-enabling restores." Used to sit inside
         the hosted panel above, the one place the rule can never hold — the
         states it names are exactly the two that gate closes on.
         `apps/fauna-tui/src/settings/atproto.rs` leads the fix; this is the
         trickle-down leg. ── -->
    {#if snap?.identity}
      {@const status = resolveLocalized(identityStatusLabel(snap.identity.status))}
      <p data-testid={IDS.ATPROTO_HOSTED_HANDLE}>
        {t.atproto_settings.hosted_handle_prefix({ handle: snap.identity.handle })}
        {' · '}
        {t.atproto_settings.hosted_method_prefix({ method: snap.identity.method })}
        {' · '}{status}
      </p>
    {/if}

    <!-- ── Delete presence: whenever a hosted identity exists. Its own
         confirm card — never the depth selector's. The copy is the
         machine's, rendered verbatim. ─────────────────────────────────── -->
    {#if snap?.show_delete_presence}
      <button class="btn danger" data-testid={IDS.ATPROTO_DELETE_PRESENCE} onclick={openDelete}>
        {t.atproto_settings.delete_presence_button}
      </button>
    {/if}
    {#if snap?.delete_confirm}
      {@const deleteConfirm = snap.delete_confirm}
      <div class="card" data-testid={IDS.ATPROTO_DELETE_CONFIRM_CARD}>
        {#each deleteConfirm.lines as line}
          <p>{resolveLocalized(line)}</p>
        {/each}
        <div class="card-actions">
          <button class="btn" data-testid={IDS.ATPROTO_DELETE_CANCEL} disabled={deleteConfirm.in_progress} onclick={cancelDelete}>
            {t.atproto_settings.delete_cancel_button}
          </button>
          <button class="btn danger" data-testid={IDS.ATPROTO_DELETE_CONFIRM} disabled={deleteConfirm.in_progress} onclick={confirmDelete}>
            {t.atproto_settings.delete_confirm_button}
          </button>
        </div>
      </div>
    {/if}

    <!-- ── Full-PDS panel: gated on level = hosted_full ───────────────────── -->
    {#if snap?.level === 'hosted_full'}
      <!-- The consent cards and the connected-app rows moved to Settings →
           Connected apps (connected-apps.md — a lift, never a duplication). -->

      <button
        class="btn toggle"
        class:on={snap?.external_apps_enabled}
        data-testid={IDS.ATPROTO_EXTERNAL_APPS_ENABLE}
        data-state={snap?.external_apps_enabled ? 'on' : 'off'}
        onclick={toggleExternalApps}
      >{t.atproto_settings.external_apps_toggle}</button>

      <div class="creds">
        <h3>{t.atproto_settings.app_credentials_heading}</h3>
        <button class="btn btn-primary" data-testid={IDS.ATPROTO_APP_CREDENTIAL_MINT} onclick={mintCredential}>
          {t.atproto_settings.mint_button}
        </button>
        {#if (snap?.credentials ?? []).length === 0}
          <p class="muted small">{t.atproto_settings.app_credentials_empty}</p>
        {:else}
          {#each snap?.credentials ?? [] as c (c.credential_id)}
            <div class="cred-item" data-testid={IDS.ATPROTO_APP_CREDENTIAL_ITEM}>
              <span class="cred-name">{c.label}</span>
              <span class="muted small">
                {t.atproto_settings.credential_created_prefix({ date: new Date(c.created_at_millis).toLocaleString() })}
                {' · '}
                {c.last_used_at_millis == null
                  ? t.atproto_settings.credential_never_used
                  : t.atproto_settings.credential_last_used_prefix({ date: new Date(c.last_used_at_millis).toLocaleString() })}
              </span>
              {#if c.revealable || c.credential_id in revealedSecrets}
                <button
                  class="btn small"
                  data-testid={IDS.ATPROTO_APP_CREDENTIAL_REVEAL}
                  disabled={c.credential_id in revealedSecrets}
                  onclick={() => revealCredential(c.credential_id)}
                >{revealedSecrets[c.credential_id] ?? t.atproto_settings.reveal_button}</button>
              {/if}
              <button class="btn danger small" data-testid={IDS.ATPROTO_APP_CREDENTIAL_REVOKE} onclick={() => revokeCredential(c.credential_id)}>
                {t.atproto_settings.revoke_button}
              </button>
            </div>
          {/each}
        {/if}
      </div>

      <!-- ── The D10 authoring-delegation row ─────────────────────────────
           What authorizes an external app to POST as this account, not merely
           to sign in. `-authorize` renders in BOTH states because re-minting
           IS renewal; `-revoke` only alongside a live row. -->
      <div class="creds">
        <h3>{t.atproto_settings.delegation_heading}</h3>
        {#if !snap?.delegation}
          <p class="muted small">{t.atproto_settings.delegation_empty}</p>
          <button class="btn btn-primary" data-testid={IDS.ATPROTO_DELEGATION_AUTHORIZE} onclick={authorizeExternalApps}>
            {t.atproto_settings.delegation_authorize_button}
          </button>
        {:else}
          {@const d = snap.delegation}
          <div class="card" data-testid={IDS.ATPROTO_DELEGATION_ROW}>
            <p data-testid={IDS.ATPROTO_DELEGATION_SCOPE}>
              {t.atproto_settings.delegation_scope_prefix({ capabilities: delegationScope })}
            </p>
            <p data-testid={IDS.ATPROTO_DELEGATION_LASTS_UNTIL}>
              {d.expires_at_micros == null
                ? t.atproto_settings.delegation_lasts_until_no_expiry({
                    authorized: fromCertMicros(d.authorized_at_micros),
                  })
                : t.atproto_settings.delegation_lasts_until({
                    authorized: fromCertMicros(d.authorized_at_micros),
                    expires: fromCertMicros(d.expires_at_micros),
                  })}
            </p>
            <!-- The liveness WIRE spelling rides `data-state` so the e2e asserts
                 the state, not its prose — an unrecognized spelling from a newer
                 nest still renders (degrade, never fail to decode). -->
            <p data-testid={IDS.ATPROTO_DELEGATION_STATUS} data-state={d.liveness}>{delegationStatus}</p>
            <!-- ADVISORY (D10 § Audit). Every leaf above derives from the SIGNED
                 cert, re-verified client-side under this account's own identity
                 key; this one does not — the nest simply asserts it, with nothing
                 signing it. So the wording hedges and the leaf carries
                 `data-advisory`: an absent stamp means nothing was REPORTED,
                 never that nothing happened, because a nest that under-reports is
                 exactly what this value cannot detect. The hint line points at
                 the feed badge, which IS read from signed bytes. -->
            <p data-testid={IDS.ATPROTO_DELEGATION_LAST_USED} data-advisory="true">
              {d.last_used_at_millis == null
                ? t.atproto_settings.delegation_last_used_never
                : t.atproto_settings.delegation_last_used({
                    when: new Date(d.last_used_at_millis).toLocaleString(),
                  })}
            </p>
            <p class="muted small">{t.atproto_settings.delegation_last_used_hint}</p>
          </div>
          <div class="card-actions">
            <button class="btn" data-testid={IDS.ATPROTO_DELEGATION_AUTHORIZE} onclick={authorizeExternalApps}>
              {t.atproto_settings.delegation_reauthorize_button}
            </button>
            <button class="btn danger" data-testid={IDS.ATPROTO_DELEGATION_REVOKE} onclick={deauthorizeExternalApps}>
              {t.atproto_settings.delegation_revoke_button}
            </button>
          </div>
        {/if}
      </div>
    {/if}
  </section>
{/if}

<style>
  h2 { font-size: 1.25rem; margin-bottom: 0.5rem; }
  h3 { font-size: 1rem; margin: 1rem 0 0.5rem; }
  h4 { font-size: 0.9rem; margin: 0.75rem 0 0.25rem; }
  .muted { color: var(--text-muted); }
  .section { margin-bottom: 2rem; }
  .creds { margin-top: 1rem; }
  .cred-item {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    padding: 0.5rem 0;
    border-bottom: 1px solid var(--border, #e0e0e0);
  }
  .cred-name { font-weight: 500; }
  .small { font-size: 0.85rem; }

  .depth-selector { margin-bottom: 1rem; }
  .depth-rung, .did-method-rung {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    width: 100%;
    text-align: left;
    gap: 0.15rem;
    padding: 0.5rem 0.75rem;
    margin: 0.35rem 0;
    border: 1px solid var(--border, #333);
    border-radius: 6px;
    background: transparent;
    color: inherit;
    cursor: pointer;
  }
  .depth-rung.active, .did-method-rung.active {
    border-color: var(--accent, #4a6cf7);
    background: color-mix(in srgb, var(--accent, #4a6cf7) 12%, transparent);
  }
  .depth-rung:disabled, .did-method-rung:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
  .depth-title { font-weight: 500; }
  .depth-desc { display: block; }

  .card {
    border: 1px solid var(--border, #333);
    border-radius: 6px;
    padding: 0.75rem;
    margin: 1rem 0;
  }
  .card-actions {
    display: flex;
    justify-content: flex-end;
    gap: 0.5rem;
    margin-top: 0.5rem;
  }
  .toggle {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    margin: 0.4rem 0;
  }
  .hosted-panel { margin: 1rem 0; }
</style>
