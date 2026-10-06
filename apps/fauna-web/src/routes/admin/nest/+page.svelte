<script lang="ts">
  import { connectionStatus, identity } from '$lib/store';
  import {
    adminServicesList,
    adminServicesUpdate,
    setupStatus,
    adminSetServingPort,
    adminRequestHostRestart,
    factoryReset,
    adminSeedRotateRoster,
    rotateDeploymentSeed,
    adminRegionStatus,
    adminSetRegion,
    moderationLegalTakedown,
    adminIssuerKeyStatus,
    adminRotateIssuerKey,
    adminForceRotateIssuer,
    type SeedRotationConfirmView,
    type AdminRegionView,
  } from '$lib/rpc';
  import {
    armOauthForced,
    beginOauthRotate,
    cancelOauthForced,
    finishOauthCall,
    initialOauthSection,
    oauthKeys,
    oauthKeysLoaded,
    oauthLive,
    takeOauthConfirm,
    type IssuerForcedArm,
    type OauthKeysRead,
    type OauthSection,
  } from '$lib/admin-oauth-keys';
  import { mintAndPersistPendingFactoryReset } from '$lib/onboarding/launch-persistence';
  import { accountsClearNestBinding, accountsSessionMaterial } from '$lib/accounts';
  import { clearAllCriticalAlerts } from '$lib/critical-alerts';
  import {
    qualifyReclaimHandle,
    createAdminNatModeMachine,
    type AdminNatModeMachine,
  } from '$lib/wasm-onboarding';
  import { nodeUrl } from '$lib/api';
  import { goto } from '$app/navigation';
  import { onMount } from 'svelte';
  import MessageBanner from '$lib/components/MessageBanner.svelte';
  import { t } from '$lib/i18n/strings';
  import {
    offlineAffordance,
    osMaintenanceStatusLabel,
    parsePort,
    adminParseRegionCode,
    takedownFormView,
    takedownVerdict,
    issuerKeyRowLabel,
    issuerKeyRotateCost,
    issuerForcedConfirmView,
  } from '$lib/wasm';
  import { makeOfflineGate } from '$lib/offline-gate';
  import { resolveLocalized } from '$lib/i18n/localized';
  import { IDS } from '$lib/generated/uiIds';

  // W4 (account-data-plane.md § Workstreams) phase 4's UI desensitizing (`account-data-plane.md` § The
  // offline-mutation contract). "Admin/provisioning" is the charter's own
  // example of class 3, and the tui sweep bore it out: the deployment-mutating
  // half of this page is `OnlineOnly`. The verdict is the shared rule's, never a
  // class test written here; `$lib/offline-gate` owns how a reactive tree obeys
  // it. Note what is deliberately NOT gated below — arming a confirm is local,
  // so the openers stay live and only the ceremony's own confirm declares.
  // `admin-nest-seed-rotate-button` is the sharp case: it DOES issue a call,
  // but `fauna.admin.admins.list` is a `Read`, and ruling 1 never greys a read
  // (whether a read is answerable offline is a W3 projection question the table
  // declines to encode). Declaring it would gate nothing while reading as proof
  // the control is gated — the shape `check-offline-gate-kinds.py` rule 2
  // forbids — so the opener declares nothing and its confirm carries the kind.
  const offlineGate = makeOfflineGate(offlineAffordance, connectionStatus.subscribe);

  // admin-nest (admin.md § N Nest) — nest-wide settings that aren't a feature
  // page, introduced by the per-page-services redesign (admin.md § Admin IA
  // redesign, 2026-06-04) which removed the standalone admin-services page. It
  // carries the admin pairing toggle (the one surviving service flag, moved
  // off Services) + the Factory Reset danger zone (moved off Settings). Mirrors
  // linux `views/admin.rs::build_nest_page`. The read-only storage-mode
  // indicator was removed with the no-modes transition (ratified 2026-07-12 —
  // docs/goal/architecture/nest/storage-modes.md); every nest is sealed at
  // rest now, so there is no mode to display.

  let error = $state('');

  // ── Admin pairing policy (`fauna.admin.services.{list,update}` name "pairing") ──
  // Default-on nest master switch for user-initiated nest pairing (per-user
  // multi-homing); off → the nest rejects `fauna.pair.add`. The user-facing
  // link/unlink surface is /settings/nests (linked-nests.md); this is the
  // admin's only pairing control.
  let pairingEnabled = $state<boolean | null>(null);
  let pairingToggling = $state(false);

  // ── Admin-set client-facing API serving port (`fauna.admin.set_serving_port`) ──
  // The nest's own HTTPS listener port (the WS-RPC transport + the served SPA),
  // an admin choice (nest/common.md § Serving ports) — the symmetric twin of the
  // CalDAV port on admin-calendar. A text_input + save button (no shared policy
  // machine, unlike CalDAV): seeded from `setup.status.serving_port` (default
  // 443), written via the raw `fauna.admin.set_serving_port` kind, re-read after
  // a save. Governs only the router-less direct listener; inert behind the :443
  // SNI router. The new port binds on the next nest restart.
  let servingPort = $state('443');
  let servingPortSaving = $state(false);
  // `setup.status.fronted_by_router` — true behind the cloud :443 SNI router,
  // where the chosen port is inert (the nest rejects a write). The field renders
  // read-only with a hint then; editable only on a direct listener (default).
  let servingPortFronted = $state(false);

  // ── Host-OS maintenance (`nest-os-*`, installers/vps.md § Host OS Maintenance § 4) ──
  // Read-only patch/reboot status for the host Ubuntu box of an onboarded VPS,
  // from the `os_*` fields on `setup.status`. The status line + count badge are
  // passive; "restart now" expedites the nest-coordinated idle reboot. A nest with
  // no host channel (dev/desktop) reads the defaults → "OS up to date".
  let osSecurityUpdates = $state(0);
  let osRebootPending = $state(false);
  let restartingNow = $state(false);
  // Shared state→key decision in `fauna_core::format::os_maintenance_status_label`
  // (over wasm); the raw count renders separately in `nest-os-updates-count`.
  const osStatusText = $derived(
    resolveLocalized(osMaintenanceStatusLabel(osSecurityUpdates, osRebootPending)),
  );

  // ── NAT-mode control (`fauna.setup.nat_mode` via the shared AdminNatModeMachine) ──
  // The post-onboarding change surface for the axis the wizard's
  // nat_mode_choice confirmed once at claim (admin.md § Nest → NAT-mode
  // control). Radios + save + status driven by the shared machine (the wasm
  // twin of `fauna_onboarding_machine::admin_nat_mode` — same seam + commit
  // ceremony as the wizard page); radio labels reuse the onboarding.nat_mode
  // strings. No defer button — navigating away is the defer. Save stays
  // enabled after success (mutable upsert; an immediate re-flip is allowed).
  let natMachine: AdminNatModeMachine | null = null;
  let natMode = $state<'public' | 'private'>('public');
  let natStatusText = $state('');
  let natSaveEnabled = $state(false);
  let natInflight = $state(false);

  // The caught value as the `{message}` every admin page-error string carries
  // (ui/README.md § Copy comprehensibility — the banner must name the gesture AND
  // the reason; rendering the bare exception instead of the string dropped the first).
  function detail(e: unknown): string {
    return e instanceof Error ? e.message : String(e);
  }

  function natRender() {
    if (!natMachine) return;
    const snap = JSON.parse(natMachine.snapshotJson());
    natMode = snap.selected_mode;
    natSaveEnabled = !!snap.submit_enabled;
    natInflight = snap.state === 'Submitting';
    natStatusText = resolveLocalized(snap.message);
  }

  async function loadNatMode() {
    const id = $identity;
    if (!id?.secretHex) return;
    natMachine = await createAdminNatModeMachine(nodeUrl(), id.secretHex);
    natRender();
    await natMachine.hydrate();
    natRender();
  }

  function selectNatMode(mode: 'public' | 'private') {
    natMachine?.select(mode);
    natRender();
  }

  async function saveNatMode() {
    if (!natMachine) return;
    await natMachine.submit();
    natRender();
  }

  // ── Declared region (`fauna.admin.region.{get,set}`) ──
  // The deployment's legal situs — the region tier's one human choice
  // (region-blocking.md § Region determination; dynamic-features.md § The
  // region tier). Every rendering decision is the shared `AdminRegionView`
  // fold (tui `admin/nest.rs`, the reference leg) — this file paints exactly
  // what it hands back and wires two buttons; it decides nothing about the
  // plane. Mirrors linux `views/admin.rs`'s `build_nest_page` region block.
  //
  // ⚠ DECLARED, NEVER DETECTED (ratified 2026-08-11): no detect/prefill
  // affordance may be added here — `adminParseRegionCode` deliberately does
  // not even case-fold.
  let region = $state<AdminRegionView | null>(null);
  let regionInput = $state('');
  let regionWorking = $state(false);

  async function loadRegion() {
    const id = $identity;
    if (!id?.secretHex) return;
    try {
      region = await adminRegionStatus(id.secretHex);
      // The draft mirrors the declaration, so a withdrawal empties the field
      // rather than leaving the withdrawn code sitting in it looking declared.
      regionInput = region.declared ?? '';
    } catch (e) {
      // Non-fatal: the section stays at its fresh-install defaults if
      // fauna.admin.region.get is unreachable — mirrors loadServingPort.
      error = t.admin.nest_page.region_load_error({ message: detail(e) });
    }
  }

  // Declare (`region` already validated) or withdraw (`region = undefined`)
  // — both `fauna.admin.region.set`. Re-reads on success so the section
  // re-seeds from the persisted declaration (a re-declaration also retires
  // the previous region's feature-policy document nest-side).
  async function setRegion(newRegion: string | undefined) {
    const id = $identity;
    if (!id?.secretHex) return;
    error = '';
    regionWorking = true;
    try {
      await adminSetRegion(id.secretHex, newRegion);
      await loadRegion();
    } catch (e) {
      error = t.admin.nest_page.region_save_error({ message: detail(e) });
    } finally {
      regionWorking = false;
    }
  }

  // Validate client-side via the shared `adminParseRegionCode` (invalid →
  // `error-message`, no dispatch — the serving-port shape); the `Err` this
  // discards is the shared engine's own i18n KEY, not a display string, so
  // this always renders its own local `region_invalid` message.
  function saveRegion() {
    const code = adminParseRegionCode(regionInput);
    if (code === undefined) {
      error = t.admin.nest_page.region_invalid;
      return;
    }
    setRegion(code);
  }

  function withdrawRegion() {
    setRegion(undefined);
  }

  // ── Deployment-seed rotation (`admin-nest-seed-rotate-*`, box-recovery.md
  // § Deployment-seed rotation) — give this nest a brand-new identity; every
  // enrolled app re-trusts it automatically, and anyone still holding the old
  // identity (a removed admin, a lost device) stops being able to use it.
  // Mirrors linux `views/admin.rs`'s `SeedRotateConfirmState` (the reference
  // leg): `loading`/`failed` paint NO roster rows — an empty list beside a
  // live confirm would read as "nobody inherits", the one thing this surface
  // must never say (the doc's ordering rule: name the inheriting set before
  // dispatch). `null` = unarmed.
  type SeedRotateState =
    | { kind: 'loading' }
    | { kind: 'failed'; message: string }
    | { kind: 'ready'; view: SeedRotationConfirmView };
  let seedRotateState = $state<SeedRotateState | null>(null);
  let seedRotateStatus = $state('');
  const seedRotateCanConfirm = $derived(
    seedRotateState?.kind === 'ready' && seedRotateState.view.can_confirm,
  );
  const seedRotateReasonText = $derived.by(() => {
    if (!seedRotateState) return '';
    if (seedRotateState.kind === 'loading') return t.admin.nest_page.rotate_seed_roster_loading;
    if (seedRotateState.kind === 'failed') return seedRotateState.message;
    return seedRotateState.view.blocked_reason
      ? resolveLocalized(seedRotateState.view.blocked_reason)
      : '';
  });

  // Arm the confirm — into `Loading` FIRST, so the confirm exists (disabled)
  // from the same frame the button was pressed, then starts the roster read.
  async function armSeedRotate() {
    seedRotateState = { kind: 'loading' };
    seedRotateStatus = '';
    const id = $identity;
    if (!id?.secretHex) return;
    try {
      const view = await adminSeedRotateRoster(id.secretHex);
      // Guard: a cancel click before the read lands must not be silently
      // re-armed by the late reply (mirrors linux's `set_seed_rotate_roster`).
      if (seedRotateState !== null) {
        seedRotateState = { kind: 'ready', view };
      }
    } catch (e) {
      if (seedRotateState !== null) {
        seedRotateState = {
          kind: 'failed',
          message: t.admin.nest_page.rotate_seed_roster_error({ cause: detail(e) }),
        };
      }
    }
  }

  function cancelSeedRotate() {
    seedRotateState = null;
  }

  // Disarm-before-dispatch: clearing the state synchronously, before the
  // ceremony's own await, means a double click has nothing left to dispatch
  // a second rotation onto the first.
  async function confirmSeedRotate() {
    if (seedRotateState?.kind !== 'ready' || !seedRotateState.view.can_confirm) return;
    seedRotateState = null;
    seedRotateStatus = t.admin.nest_page.rotate_seed_working;
    const id = $identity;
    if (!id?.secretHex) return;
    try {
      const verdict = await rotateDeploymentSeed(id.secretHex);
      seedRotateStatus = resolveLocalized(verdict);
    } catch (e) {
      seedRotateStatus = t.admin.nest_page.rotate_seed_failed({ cause: detail(e) });
    }
  }

  // ── Outside-app sign-in keys (`admin-nest-oauth-*`, authorization-server.md
  // § The issuer → Two rotation arms; placed directly after the deployment
  // identity by admin.md § N Nest) — the nest-held OAuth issuer key set and its
  // second signer, the refresh-token secret: the served keys, the ordinary
  // rotation, and two forced arms sharing one inline confirm. Every sentence is
  // a shared `fauna_client_admin` fold (`$lib/wasm`) and every WHEN is
  // `$lib/admin-oauth-keys`' pure guard, so this block only paints and wires —
  // mirroring tui's `admin/nest.rs` `oauth_elements` (the reference leg).
  //
  // `$state.raw`: every transition hands back a whole new section, and the view
  // inside it goes back into the wasm folds as the plain JSON it arrived as.
  let oauth = $state.raw<OauthSection>(initialOauthSection());
  // Bumped on a timer so a retired key's countdown keeps falling while the page
  // stays open (see `oauthRows`).
  let oauthTick = $state(0);
  const oauthView = $derived(oauthKeys(oauth));
  const oauthControlsLive = $derived(oauthLive(oauth));
  // The countdown is counted at PAINT: the clock is re-read whenever the set
  // changes and on every tick — never cached beside the rows, or a re-read
  // landing just after a rotation would count from a stale instant and read a
  // minute longer than the nest will actually keep the key.
  const oauthRows = $derived.by(() => {
    void oauthTick;
    if (!oauthView) return [];
    const nowSecs = Math.floor(Date.now() / 1000);
    return oauthView.keys.map((row) => resolveLocalized(issuerKeyRowLabel(row, nowSecs)));
  });
  const oauthCostText = $derived(
    oauthView ? resolveLocalized(issuerKeyRotateCost(oauthView)) : '',
  );
  const oauthReasonText = $derived(
    oauth.keys.kind === 'failed' ? oauth.keys.reason : t.admin.nest_page.oauth_keys_loading,
  );

  // The session-secret verdict's instant, on the SPA's clock face (wasm has no
  // OS timezone database) — the `formatWhen` of the Devices custody card.
  function formatInstant(secs: number): string {
    return new Date(secs * 1000).toLocaleString();
  }

  // The key set's read, worded on failure for the section's own reason line —
  // never the page's `error-message`: the kind is newer than every other read
  // here, and any read error must still leave the rest painting.
  async function readOauthKeys(secretHex: string): Promise<OauthKeysRead> {
    try {
      return { kind: 'ready', view: await adminIssuerKeyStatus(secretHex) };
    } catch (e) {
      return { kind: 'failed', reason: t.admin.nest_page.oauth_keys_error({ cause: detail(e) }) };
    }
  }

  async function loadOauthKeys() {
    const id = $identity;
    if (!id?.secretHex) return;
    const keys = await readOauthKeys(id.secretHex);
    oauth = oauthKeysLoaded(oauth, keys);
  }

  // A call's end: the verdict and the re-read set land in ONE assignment, so
  // the verdict never names a key the rows beside it do not show yet, and the
  // controls come back live in the same frame.
  async function finishOauth(secretHex: string, status: string) {
    const keys = await readOauthKeys(secretHex);
    oauth = finishOauthCall(oauth, status, keys);
  }

  // The faces never reject — a failed call is the shared fold's own verdict. A
  // rejection here is the connection failing before the call was sent, worded
  // by that same failure sentence (the nest did not confirm), so this page
  // still decides no sentence of its own.
  function oauthCallFailed(e: unknown): string {
    return t.admin.nest_page.oauth_rotate_failed({ cause: detail(e) });
  }

  async function rotateOauthKey() {
    const id = $identity;
    if (!id?.secretHex) return;
    const began = beginOauthRotate(oauth, t.admin.nest_page.oauth_working);
    if (!began) return;
    oauth = began;
    let status: string;
    try {
      status = resolveLocalized(await adminRotateIssuerKey(id.secretHex));
    } catch (e) {
      status = oauthCallFailed(e);
    }
    await finishOauth(id.secretHex, status);
  }

  function armOauth(arm: IssuerForcedArm) {
    const next = armOauthForced(oauth, arm, issuerForcedConfirmView);
    if (next) oauth = next;
  }

  function cancelOauth() {
    oauth = cancelOauthForced(oauth);
  }

  // `arm` is the arm the pressed confirm was painted for. Disarm-before-
  // dispatch: the taken section is assigned before the first await, so the
  // confirm is gone when the click returns and a double press has nothing left
  // to drop the key the first press minted.
  async function confirmOauth(arm: IssuerForcedArm) {
    const id = $identity;
    if (!id?.secretHex) return;
    const taken = takeOauthConfirm(oauth, arm, t.admin.nest_page.oauth_working);
    if (!taken) return;
    oauth = taken.state;
    let status: string;
    try {
      status = resolveLocalized(
        await adminForceRotateIssuer(id.secretHex, taken.arm, formatInstant),
      );
    } catch (e) {
      status = oauthCallFailed(e);
    }
    await finishOauth(id.secretHex, status);
  }

  // ── Legal takedown (`admin-nest-takedown-*`; moderation.md § Legal
  // takedown → Invocation surface, ruled 2026-08-16).
  // Every gating/wording decision is the shared
  // `fauna_client_moderation::takedown` fold (`takedownFormView`/
  // `takedownVerdict`, `$lib/wasm`, pure — no nest hop) — this page paints
  // and wires; nothing here decides. Simpler than seed-rotation above: the
  // fold is pure, so the arm control's sensitivity/reason recompute reactively
  // off the form fields with no async intermediate. Mirrors tui's
  // `admin/nest.rs` (the reference leg).
  let takedownContentId = $state('');
  let takedownConversation = $state(false);
  let takedownReference = $state('');
  let takedownRestore = $state(false);
  const takedownView = $derived(
    takedownFormView(takedownContentId, takedownConversation, takedownReference, takedownRestore),
  );
  // The armed confirm's captured form — `null` while un-armed. Captured at
  // arm time and never re-derived while armed, so a field the admin keeps
  // typing after arming cannot silently change what the confirm named.
  // Cleared (disarm-before-dispatch) by the confirm click before dispatch, so
  // a double click cannot dispatch a second compulsory act.
  let takedownArmed = $state<{
    contentId: string;
    conversation: boolean;
    legalReference: string;
    restore: boolean;
    view: ReturnType<typeof takedownFormView>;
  } | null>(null);
  let takedownStatus = $state('');

  function armTakedown() {
    if (!takedownView.can_submit) return;
    takedownStatus = '';
    takedownArmed = {
      contentId: takedownContentId,
      conversation: takedownConversation,
      legalReference: takedownReference,
      restore: takedownRestore,
      view: takedownView,
    };
  }

  function cancelTakedown() {
    takedownArmed = null;
  }

  async function confirmTakedown() {
    const armed = takedownArmed;
    if (!armed) return;
    takedownArmed = null;
    takedownStatus = t.admin.nest_page.takedown_working;
    const id = $identity;
    if (!id?.secretHex) return;
    try {
      await moderationLegalTakedown(
        id.secretHex,
        armed.contentId,
        armed.conversation,
        armed.legalReference,
        armed.restore,
      );
      takedownStatus = resolveLocalized(takedownVerdict(armed.restore, null));
    } catch (e) {
      takedownStatus = resolveLocalized(takedownVerdict(armed.restore, detail(e)));
    }
  }

  // ── Factory-reset (danger zone) ──
  let showFactoryResetConfirm = $state(false);
  let factoryResetting = $state(false);
  let factoryResetError = $state('');

  onMount(() => {
    loadPairing();
    loadServingPort();
    loadRegion();
    loadOsMaintenance();
    loadNatMode();
    loadOauthKeys();
    // Re-count the retired keys' minutes while the page stays open; the rows
    // re-read the clock on each tick (`oauthRows`).
    const oauthTicker = setInterval(() => (oauthTick += 1), 15_000);
    return () => clearInterval(oauthTicker);
  });

  async function loadPairing() {
    const id = $identity;
    if (!id?.secretHex) return;
    try {
      pairingEnabled = (await adminServicesList(id.secretHex)).pairing;
    } catch (e) {
      error = t.admin.nest_page.load_settings_error({ message: detail(e) });
    }
  }

  // Flip the `pairing` flag, then refetch so the badge reflects the persisted
  // state (proving the write landed in the nest, not merely echoed) — mirroring
  // linux's reflective service-row pattern.
  async function togglePairing() {
    const id = $identity;
    if (!id?.secretHex || pairingEnabled === null) return;
    error = '';
    pairingToggling = true;
    try {
      await adminServicesUpdate(id.secretHex, 'pairing', !pairingEnabled);
      pairingEnabled = (await adminServicesList(id.secretHex)).pairing;
    } catch (e) {
      error = t.admin.nest_page.update_setting_error({ message: detail(e) });
    } finally {
      pairingToggling = false;
    }
  }

  async function loadServingPort() {
    const id = $identity;
    if (!id?.secretHex) return;
    try {
      const status = await setupStatus(id.secretHex);
      servingPort = String(status.serving_port);
      servingPortFronted = status.fronted_by_router === true;
    } catch (e) {
      // Non-fatal: the field stays at its default if setup-status is unreachable.
      error = t.admin.nest_page.load_serving_port_error({ message: detail(e) });
    }
  }

  // Validate a u16 in [1, 65535] client-side via fauna_core::format::parse_port
  // (invalid → `error-message`, no dispatch), else write via `set_serving_port`
  // and re-read the persisted port (proving the write landed, not merely
  // echoed). The new port binds on the next nest restart. Mirrors the
  // admin-calendar CalDAV-port save.
  async function saveServingPort() {
    const id = $identity;
    if (!id?.secretHex) return;
    const port = parsePort(servingPort);
    if (port === undefined) {
      error = t.admin.nest_page.serving_port_invalid;
      return;
    }
    error = '';
    servingPortSaving = true;
    try {
      await adminSetServingPort(id.secretHex, port);
      servingPort = String((await setupStatus(id.secretHex)).serving_port);
    } catch (e) {
      error = t.admin.nest_page.set_serving_port_error({ message: detail(e) });
    } finally {
      servingPortSaving = false;
    }
  }

  async function loadOsMaintenance() {
    const id = $identity;
    if (!id?.secretHex) return;
    try {
      const status = await setupStatus(id.secretHex);
      osSecurityUpdates = status.os_security_updates_pending;
      osRebootPending = status.os_reboot_pending;
    } catch (e) {
      // Non-fatal: the indicator stays "OS up to date" if setup-status is unreachable.
      error = t.admin.nest_page.load_settings_error({ message: detail(e) });
    }
  }

  // Trigger an immediate (still-graceful) host reboot via
  // `fauna.admin.request_host_restart` — the nest writes a flag the host
  // reboot-coordinator picks up on its next run. Rejected on a nest with no
  // maintenance channel (surfaced in `error-message`). We re-read status after so
  // the indicator reflects any change; the box actually reboots on the next
  // coordinator run.
  async function restartNow() {
    const id = $identity;
    if (!id?.secretHex) return;
    error = '';
    restartingNow = true;
    try {
      await adminRequestHostRestart(id.secretHex);
      await loadOsMaintenance();
    } catch (e) {
      error = t.admin.nest_page.os_restart_now_error({ message: detail(e) });
    } finally {
      restartingNow = false;
    }
  }

  // Factory reset: `fauna.admin.factory_reset` wipes the nest to fresh/
  // unclaimed and restarts it. The human NEVER sees the new claim code, so the
  // client re-seeds onboarding at claim-code with it pre-filled. The local
  // identity is KEPT (the box was wiped, not the client); nest-derived data is
  // cleared so the re-onboard is clean. Tolerates the ~1-2 s restart WS drop via
  // the claim-code page's transient retry.
  //
  // ORDER IS THE FIX (gap CR-1, common.md § Client-state recoverability). The
  // code is MINTED CLIENT-SIDE and DURABLY PERSISTED *before* the reset is
  // dispatched, then PINNED onto the request. It used to be read off the reply
  // and saved after — so a client killed between dispatch and reply-render lost
  // it forever: the nest landed at the recovery floor (fresh/unclaimed) with a
  // code no client could learn, i.e. un-claimable without shell access. A failed
  // persist therefore ABORTS the dispatch (the old "the reply gives us the code
  // anyway" escape hatch is gone), which is the whole reason the mint helper
  // throws instead of warning.
  //
  // Per mail-bridge-lifecycle.md § Factory reset + onboarding.md §3a. Mirrors
  // apps/fauna-linux/src/main.rs::register_factory_reset_handler.
  async function doFactoryReset() {
    const id = $identity;
    if (!id?.secretHex) return;
    factoryResetting = true;
    factoryResetError = '';

    // Resolve the slot's (nest_url, handle) BEFORE the dispatch — after it, the
    // nest is already wiping and the whole point is that the row is on disk first.
    // Read from the registry rows of the account this page acts as.
    const material = accountsSessionMaterial(id.actorId);
    const nestUrl = material?.nest_url ?? nodeUrl();
    // Re-qualify the bare cached localpart with its mail domain (shared Rust),
    // so the re-claim carries `localpart@domain` and the nest re-registers the
    // primary mail domain from the handle's `@domain` — matching the lead linux
    // flow (no inline re-derivation, which would re-introduce the per-app
    // divergence the shared fn exists to remove). Empty/locked cache → empty
    // handle returned unchanged (the recoverable claim-code re-prompt path).
    // mail-bridge-lifecycle.md § Factory reset (re-claim handle sourcing).
    let newClaimCode: string;
    try {
      const handle = await qualifyReclaimHandle(
        material?.handle ?? '',
        material?.domain ?? undefined,
        nestUrl,
      );
      // Mints, writes the row through the shared `LaunchPersistence` seam, and
      // reads it back — returns a code only once it is recoverable from disk.
      newClaimCode = await mintAndPersistPendingFactoryReset(nestUrl, handle);
    } catch (e) {
      // Nothing was dispatched: the nest is untouched and the admin can retry.
      // Dispatching here instead would recreate CR-1 exactly.
      console.warn('[admin-nest] pending-factory-reset persist failed:', e);
      factoryResetError = t.admin.settings_page.factory_reset_persist_failed;
      factoryResetting = false;
      showFactoryResetConfirm = false;
      return;
    }

    try {
      // Pin the code we just persisted; the nest honors it verbatim, so the wiped
      // box boots with exactly the code sitting in the slot.
      await factoryReset(id.secretHex, newClaimCode);
      // Factory reset tears down authenticated state without changing
      // `identity` (creds are kept for re-claim), so the actor-scoped reset
      // registry's identity-change subscription does NOT fire here — clear
      // explicitly (critical-alerts.md § Mechanism → Lifetime; mirrors
      // apps/fauna-linux/src/main.rs's factory-reset handler).
      clearAllCriticalAlerts();
      // Drop the authed session but KEEP the local identity: clear this
      // actor's per-actor (nest_url, device_id) binding through the registry
      // (`account-scoping.md` § Concurrent instances, the delete corollary),
      // so the next launch routes on the pending-factory-reset slot.
      await accountsClearNestBinding(id.actorId);
      goto('/app/onboarding');
    } catch (e) {
      // The slot deliberately SURVIVES a failed dispatch. A thrown call cannot
      // distinguish "the nest never reset" from "the nest reset and the reply
      // was lost" — and clearing the slot in the second case is CR-1 all over
      // again (an un-claimable box). So we keep it: against a wiped nest the
      // pre-filled claim just works; against an untouched one it reports
      // "already claimed", which is visible and retryable in-client. The slot is
      // cleared at the claim terminal either way (onboarding `LoggedIn` exit).
      factoryResetError = e instanceof Error
        ? e.message
        : t.admin.settings_page.factory_reset_failed;
      factoryResetting = false;
      showFactoryResetConfirm = false;
    }
  }
</script>

<h1 data-testid={IDS.ADMIN_NEST_HEADING}>{t.admin.nest_page.title}</h1>
<p class="subtitle">{t.admin.nest_page.description}</p>

<MessageBanner bind:error />

<!-- Admin pairing policy (moved off the removed Services page) -->
<section class="section">
  <div class="service-row">
    <div class="service-info">
      <div class="service-name">{t.admin.services_page.pairing}</div>
      <div class="service-desc">{t.admin.services_page.pairing_desc}</div>
    </div>
    <div class="service-control">
      <span
        class="badge"
        class:active={pairingEnabled === true}
        class:inactive={pairingEnabled === false}
        data-testid={IDS.ADMIN_SERVICE_PAIRING_STATUS}
      >{pairingEnabled === null
          ? ''
          : pairingEnabled
            ? t.admin.services_page.enabled
            : t.admin.services_page.disabled}</span>
      <button
        class="btn toggle"
        class:on={pairingEnabled === true}
        data-testid={IDS.ADMIN_SERVICE_PAIRING_TOGGLE}
        onclick={togglePairing}
        use:offlineGate={{
          kind: 'fauna.admin.services.update',
          disabled: pairingToggling || pairingEnabled === null,
        }}
      >{pairingEnabled ? t.common.disable : t.common.enable}</button>
    </div>
  </div>
</section>

<!-- Admin-set client-facing API serving port (set_serving_port) -->
<section class="section">
  <div class="field-row">
    <span>
      <span class="service-name">{t.admin.nest_page.serving_port_label}</span>
      <span class="service-desc">{t.admin.nest_page.serving_port_desc}</span>
    </span>
    <span class="field-controls">
      <input
        type="text"
        inputmode="numeric"
        class="port-input"
        data-testid={IDS.ADMIN_NEST_SERVING_PORT_INPUT}
        bind:value={servingPort}
        disabled={servingPortSaving || servingPortFronted}
      />
      <button
        type="button"
        class="btn"
        data-testid={IDS.ADMIN_NEST_SERVING_PORT_SAVE_BUTTON}
        use:offlineGate={{
          kind: 'fauna.admin.set_serving_port',
          disabled: servingPortSaving || servingPortFronted,
        }}
        onclick={saveServingPort}
      >
        {t.admin.nest_page.serving_port_save}
      </button>
    </span>
  </div>
  {#if servingPortFronted}
    <p class="service-desc">{t.admin.nest_page.serving_port_fronted_hint}</p>
  {/if}
</section>

<!-- NAT-mode control (fauna.setup.nat_mode via the shared AdminNatModeMachine) -->
<section class="section">
  <div class="nat-mode-card">
    <div class="service-name">{t.admin.nest_page.nat_mode_label}</div>
    <label class="nat-radio">
      <input
        type="radio"
        name="admin-nat-mode"
        value="public"
        data-testid={IDS.ADMIN_NEST_NAT_MODE_PUBLIC_RADIO}
        checked={natMode === 'public'}
        disabled={natInflight}
        onchange={() => selectNatMode('public')}
      />
      {t.onboarding.nat_mode.public_label}
    </label>
    <div class="service-desc nat-desc">{t.onboarding.nat_mode.public_desc}</div>
    <label class="nat-radio">
      <input
        type="radio"
        name="admin-nat-mode"
        value="private"
        data-testid={IDS.ADMIN_NEST_NAT_MODE_PRIVATE_RADIO}
        checked={natMode === 'private'}
        disabled={natInflight}
        onchange={() => selectNatMode('private')}
      />
      {t.onboarding.nat_mode.private_label}
    </label>
    <div class="service-desc nat-desc">{t.onboarding.nat_mode.private_desc}</div>
    <div class="nat-actions">
      <span class="service-desc" data-testid={IDS.ADMIN_NEST_NAT_MODE_STATUS}>{natStatusText}</span>
      <button
        type="button"
        class="btn"
        data-testid={IDS.ADMIN_NEST_NAT_MODE_SAVE_BUTTON}
        use:offlineGate={{ kind: 'fauna.setup.nat_mode', disabled: !natSaveEnabled }}
        onclick={saveNatMode}
      >
        {t.admin.nest_page.nat_mode_save}
      </button>
    </div>
  </div>
</section>

<!-- Declared region (fauna.admin.region.{get,set}) — region-blocking.md § Region
     determination. DECLARED, NEVER DETECTED: no detect/prefill affordance here. -->
<section class="section" data-testid={IDS.ADMIN_NEST_REGION_SECTION}>
  <div class="nat-mode-card">
    <div class="service-name">{t.admin.nest_page.region_label}</div>
    <div class="service-desc">{t.admin.nest_page.region_desc}</div>
    <!-- admin-nest-region-status — the declared region, or that none is
         declared. A NORMAL state, never an error: a deployment that has
         never declared is conforming. -->
    <p data-testid={IDS.ADMIN_NEST_REGION_STATUS}>
      {region ? resolveLocalized(region.status) : t.admin.nest_page.region_none}
    </p>
    <!-- admin-nest-region-authority — present only while a region is
         declared: before that there is no authority channel to describe,
         and inventing a line about one would be a claim. -->
    {#if region?.authority}
      <p class="service-desc" data-testid={IDS.ADMIN_NEST_REGION_AUTHORITY}>
        {resolveLocalized(region.authority)}
      </p>
    {/if}
    <!-- admin-nest-region-staleness — the nest-reported "act when you can"
         warning, only when the channel is unreached. The rules already
         received stay in force, so this is a caveat, never an outage. -->
    {#if region?.staleness}
      <p class="service-desc" data-testid={IDS.ADMIN_NEST_REGION_STALENESS}>
        {resolveLocalized(region.staleness)}
      </p>
    {/if}
    <div class="nat-actions">
      <input
        type="text"
        class="port-input"
        data-testid={IDS.ADMIN_NEST_REGION_INPUT}
        placeholder={t.admin.nest_page.region_placeholder}
        bind:value={regionInput}
        disabled={regionWorking}
      />
      <button
        type="button"
        class="btn"
        data-testid={IDS.ADMIN_NEST_REGION_SAVE_BUTTON}
        use:offlineGate={{ kind: 'fauna.admin.region.set', disabled: regionWorking }}
        onclick={saveRegion}
      >
        {t.admin.nest_page.region_save}
      </button>
      <!-- admin-nest-region-withdraw-button — shown only while a region is
           declared. Withdrawing also retires the previous region's
           feature-policy document nest-side. -->
      {#if region?.can_withdraw}
        <button
          type="button"
          class="btn ghost"
          data-testid={IDS.ADMIN_NEST_REGION_WITHDRAW_BUTTON}
          use:offlineGate={{ kind: 'fauna.admin.region.set', disabled: regionWorking }}
          onclick={withdrawRegion}
        >
          {t.admin.nest_page.region_withdraw}
        </button>
      {/if}
    </div>
  </div>
</section>

<!-- Host-OS maintenance status (installers/vps.md § Host OS Maintenance § 4) -->
<section class="section">
  <div class="field-row">
    <span>
      <span class="service-name" data-testid={IDS.NEST_OS_MAINTENANCE_STATUS}>{osStatusText}</span>
      {#if osSecurityUpdates > 0}
        <span class="service-desc" data-testid={IDS.NEST_OS_UPDATES_COUNT}>{osSecurityUpdates}</span>
      {/if}
    </span>
    {#if osRebootPending}
      <button
        type="button"
        class="btn"
        data-testid={IDS.NEST_OS_RESTART_NOW_BUTTON}
        use:offlineGate={{
          kind: 'fauna.admin.request_host_restart',
          disabled: restartingNow,
        }}
        onclick={restartNow}
      >
        {t.admin.nest_page.os_restart_now}
      </button>
    {/if}
  </div>
</section>

<!-- Deployment-seed rotation (box-recovery.md § Deployment-seed rotation) -->
<section class="section" data-testid={IDS.ADMIN_NEST_SEED_ROTATE_SECTION}>
  <h2>{t.admin.nest_page.rotate_seed_label}</h2>
  <p class="muted">{t.admin.nest_page.rotate_seed_desc}</p>

  <button
    type="button"
    class="btn"
    data-testid={IDS.ADMIN_NEST_SEED_ROTATE_BUTTON}
    onclick={armSeedRotate}
  >
    {t.admin.nest_page.rotate_seed_button}
  </button>

  {#if seedRotateState}
    <div class="confirm-box">
      <p>{t.admin.nest_page.rotate_seed_confirm_body}</p>
      {#if seedRotateState.kind === 'ready'}
        {#each seedRotateState.view.inheritors as inheritor, i (i)}
          <p data-testid={`admin-nest-seed-rotate-roster-item-${i}`}>{inheritor.label}</p>
        {/each}
      {/if}
      {#if seedRotateReasonText}
        <p class="muted" data-testid={IDS.ADMIN_NEST_SEED_ROTATE_ROSTER_REASON}>{seedRotateReasonText}</p>
      {/if}
      <button
        class="btn danger"
        data-testid={IDS.ADMIN_NEST_SEED_ROTATE_CONFIRM_BUTTON}
        use:offlineGate={{
          kind: 'fauna.admin.deployment_seed.rotate',
          disabled: !seedRotateCanConfirm,
        }}
        onclick={confirmSeedRotate}
      >
        {t.admin.nest_page.rotate_seed_confirm_button}
      </button>
      <button
        class="btn ghost"
        data-testid={IDS.ADMIN_NEST_SEED_ROTATE_CANCEL_BUTTON}
        onclick={cancelSeedRotate}
      >
        {t.admin.nest_page.rotate_seed_cancel_button}
      </button>
    </div>
  {/if}

  {#if seedRotateStatus}
    <p data-testid={IDS.ADMIN_NEST_SEED_ROTATE_STATUS}>{seedRotateStatus}</p>
  {/if}
</section>

<!-- Outside-app sign-in keys (authorization-server.md § The issuer → Two
     rotation arms) — directly after the deployment identity (admin.md § N
     Nest). Paint only: every sentence below that depends on the key set or
     a call's outcome is a shared fold's. -->
<section class="section" data-testid={IDS.ADMIN_NEST_OAUTH_SECTION}>
  <h2>{t.admin.nest_page.oauth_label}</h2>
  <p class="muted">{t.admin.nest_page.oauth_desc}</p>

  <!-- The key rows, painted ONLY from an answered read (the seed-rotate
       roster's rule) and in the nest's own order, signer first. "Not asked
       yet" and "couldn't find out" get the reason line instead — never an
       empty list, which would read as "this nest has no keys". -->
  {#if oauthView}
    {#each oauthRows as row, i (i)}
      <p class="key-row" data-testid={`${IDS.ADMIN_NEST_OAUTH_KEY_ITEM}-${i}`}>{row}</p>
    {/each}
  {:else}
    <p class="muted" data-testid={IDS.ADMIN_NEST_OAUTH_KEY_REASON}>{oauthReasonText}</p>
  {/if}

  <!-- The ordinary arm states its cost beside itself: it has no confirm.
       All three controls are live exactly when the set has answered and no
       call is in flight — disabled, never hidden, otherwise. Only the calls
       declare an offline kind: the two forced openers arm a confirm locally
       and dispatch nothing, so their confirm carries the kind instead. -->
  {#if oauthCostText}
    <p class="muted">{oauthCostText}</p>
  {/if}
  <div class="oauth-actions">
    <button
      type="button"
      class="btn"
      data-testid={IDS.ADMIN_NEST_OAUTH_ROTATE_BUTTON}
      use:offlineGate={{ kind: 'fauna.oauth.rotate_issuer_key', disabled: !oauthControlsLive }}
      onclick={rotateOauthKey}
    >
      {t.admin.nest_page.oauth_rotate_button}
    </button>
    <button
      type="button"
      class="btn danger"
      data-testid={IDS.ADMIN_NEST_OAUTH_FORCE_ROTATE_BUTTON}
      disabled={!oauthControlsLive}
      onclick={() => armOauth('IssuerKey')}
    >
      {t.admin.nest_page.oauth_force_rotate_button}
    </button>
    <button
      type="button"
      class="btn danger"
      data-testid={IDS.ADMIN_NEST_OAUTH_SECRET_FORCE_ROTATE_BUTTON}
      disabled={!oauthControlsLive}
      onclick={() => armOauth('SessionSecret')}
    >
      {t.admin.nest_page.oauth_secret_force_rotate_button}
    </button>
  </div>

  <!-- The one forced confirm, painting what was CAPTURED at arm time. Its
       confirm is spelled once per arm so each declares its own wire kind as a
       literal and carries the arm it was painted for (a press for the other
       arm dispatches nothing). -->
  {#if oauth.armed}
    <div class="confirm-box">
      <p data-testid={IDS.ADMIN_NEST_OAUTH_CONFIRM_SUMMARY}>{resolveLocalized(oauth.armed.confirm.summary)}</p>
      {#if oauth.armed.arm === 'IssuerKey'}
        <button
          type="button"
          class="btn danger"
          data-testid={IDS.ADMIN_NEST_OAUTH_CONFIRM_BUTTON}
          use:offlineGate={{ kind: 'fauna.oauth.force_rotate_issuer_key', disabled: false }}
          onclick={() => confirmOauth('IssuerKey')}
        >
          {resolveLocalized(oauth.armed.confirm.confirm_label)}
        </button>
      {:else}
        <button
          type="button"
          class="btn danger"
          data-testid={IDS.ADMIN_NEST_OAUTH_CONFIRM_BUTTON}
          use:offlineGate={{ kind: 'fauna.oauth.force_rotate_session_secret', disabled: false }}
          onclick={() => confirmOauth('SessionSecret')}
        >
          {resolveLocalized(oauth.armed.confirm.confirm_label)}
        </button>
      {/if}
      <button
        type="button"
        class="btn ghost"
        data-testid={IDS.ADMIN_NEST_OAUTH_CANCEL_BUTTON}
        onclick={cancelOauth}
      >
        {t.admin.nest_page.oauth_cancel_button}
      </button>
    </div>
  {/if}

  <!-- The verdict — deliberately NOT error-message: every success here has
       consequences worth words, and a failure says the nest did not confirm
       rather than that nothing changed. -->
  {#if oauth.status !== null}
    <p data-testid={IDS.ADMIN_NEST_OAUTH_STATUS}>{oauth.status}</p>
  {/if}
</section>

<!-- Legal takedown (moderation.md § Legal takedown → Invocation surface) -->
<section class="section" data-testid={IDS.ADMIN_NEST_TAKEDOWN_SECTION}>
  <h2>{t.admin.nest_page.takedown_label}</h2>
  <p class="muted">{t.admin.nest_page.takedown_desc}</p>

  <label>
    {t.admin.nest_page.takedown_content_id_label}
    <input
      type="text"
      data-testid={IDS.ADMIN_NEST_TAKEDOWN_CONTENT_ID_INPUT}
      bind:value={takedownContentId}
    />
  </label>

  <label class="nat-radio">
    <input
      type="radio"
      name="admin-nest-takedown-type"
      data-testid={IDS.ADMIN_NEST_TAKEDOWN_TYPE_POST_RADIO}
      checked={!takedownConversation}
      onchange={() => (takedownConversation = false)}
    />
    {t.admin.nest_page.takedown_type_post}
  </label>
  <label class="nat-radio">
    <input
      type="radio"
      name="admin-nest-takedown-type"
      data-testid={IDS.ADMIN_NEST_TAKEDOWN_TYPE_CONVERSATION_RADIO}
      checked={takedownConversation}
      onchange={() => (takedownConversation = true)}
    />
    {t.admin.nest_page.takedown_type_conversation}
  </label>

  <label>
    {t.admin.nest_page.takedown_reference_label}
    <input
      type="text"
      data-testid={IDS.ADMIN_NEST_TAKEDOWN_REFERENCE_INPUT}
      bind:value={takedownReference}
    />
  </label>

  <label>
    <input
      type="checkbox"
      data-testid={IDS.ADMIN_NEST_TAKEDOWN_RESTORE_CHECKBOX}
      bind:checked={takedownRestore}
    />
    {t.admin.nest_page.takedown_restore_label}
  </label>

  <button
    type="button"
    class="btn danger"
    data-testid={IDS.ADMIN_NEST_TAKEDOWN_BUTTON}
    use:offlineGate={{ kind: 'fauna.moderation.legal_takedown', disabled: !takedownView.can_submit }}
    onclick={armTakedown}
  >
    {resolveLocalized(takedownView.arm_label)}
  </button>
  {#if takedownView.blocked_reason}
    <p class="muted">{resolveLocalized(takedownView.blocked_reason)}</p>
  {/if}

  {#if takedownArmed}
    <div class="confirm-box">
      <p data-testid={IDS.ADMIN_NEST_TAKEDOWN_CONFIRM_SUMMARY}>
        {resolveLocalized(takedownArmed.view.confirm_summary)}
      </p>
      <button
        class="btn danger"
        data-testid={IDS.ADMIN_NEST_TAKEDOWN_CONFIRM_BUTTON}
        use:offlineGate={{ kind: 'fauna.moderation.legal_takedown', disabled: false }}
        onclick={confirmTakedown}
      >
        {resolveLocalized(takedownArmed.view.confirm_label)}
      </button>
      <button
        class="btn ghost"
        data-testid={IDS.ADMIN_NEST_TAKEDOWN_CANCEL_BUTTON}
        onclick={cancelTakedown}
      >
        {t.admin.nest_page.takedown_cancel_button}
      </button>
    </div>
  {/if}

  {#if takedownStatus}
    <p data-testid={IDS.ADMIN_NEST_TAKEDOWN_STATUS}>{takedownStatus}</p>
  {/if}
</section>

<!-- Factory Reset Danger Zone (moved off Settings) -->
<section class="section danger-zone" data-testid={IDS.ADMIN_FACTORY_RESET_SECTION}>
  <h2>{t.admin.settings_page.factory_reset_section}</h2>
  <p class="muted">{t.admin.settings_page.factory_reset_desc}</p>

  {#if factoryResetError}
    <p class="error" data-testid={IDS.ERROR_MESSAGE}>{factoryResetError}</p>
  {/if}

  {#if !showFactoryResetConfirm}
    <button class="btn danger" data-testid={IDS.ADMIN_FACTORY_RESET_BUTTON} onclick={() => showFactoryResetConfirm = true}>
      {t.admin.settings_page.factory_reset_button}
    </button>
  {:else}
    <div class="confirm-box">
      <strong>{t.admin.settings_page.factory_reset_confirm_title}</strong>
      <p>{t.admin.settings_page.factory_reset_confirm_body}</p>
      <!-- The reset itself is the gated call, and it lives on THIS confirm — the
           opener above only reveals this box, so greying it would strand the
           user in a ceremony they cannot leave (the over-claim rulings 1-3
           forbid). linux declares on its opener instead, because there the
           confirm lives in a transient dialog it cannot register. -->
      <button
        class="btn danger"
        data-testid={IDS.ADMIN_FACTORY_RESET_CONFIRM_BUTTON}
        onclick={doFactoryReset}
        use:offlineGate={{ kind: 'fauna.admin.factory_reset', disabled: factoryResetting }}
      >
        {factoryResetting ? t.common.loading : t.admin.settings_page.factory_reset_confirm_button}
      </button>
      <button class="btn ghost" data-testid={IDS.ADMIN_FACTORY_RESET_CANCEL_BUTTON} onclick={() => showFactoryResetConfirm = false}>{t.admin.settings_page.factory_reset_cancel}</button>
    </div>
  {/if}
</section>

<style>
  h1 { margin-bottom: 0.25rem; font-size: 1.5rem; }
  .subtitle { color: var(--text-muted, #8b949e); margin-bottom: 1.5rem; font-size: 0.875rem; }
  .muted { color: var(--text-muted, #8b949e); }

  .section { margin-bottom: 2rem; }
  .section:last-child { margin-bottom: 0; }

  .service-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 1rem;
    border: 1px solid var(--border, #30363d);
    border-radius: 8px;
    background: var(--bg-surface, #161b22);
  }
  .service-info { flex: 1; }
  .service-name { font-weight: 600; margin-bottom: 0.25rem; }
  .service-desc { font-size: 0.8rem; color: var(--text-muted, #8b949e); }
  .service-control { display: flex; align-items: center; gap: 0.75rem; }

  .badge {
    display: inline-block;
    font-size: 0.75rem;
    font-weight: 600;
    padding: 0.125rem 0.5rem;
    border-radius: 4px;
  }
  .badge.active {
    background: rgba(63, 185, 80, 0.15);
    color: var(--success, #3fb950);
  }
  .badge.inactive {
    background: rgba(248, 81, 73, 0.15);
    color: var(--danger, #f85149);
  }

  .btn.toggle {
    font-size: 0.8rem;
    padding: 0.375rem 0.75rem;
    border-radius: 6px;
    border: 1px solid var(--border, #30363d);
    cursor: pointer;
  }
  .btn.toggle:disabled { opacity: 0.5; cursor: not-allowed; }

  .danger-zone {
    border: 1px solid var(--danger, #f85149);
    border-radius: 8px;
    padding: 1rem;
    margin-top: 2rem;
  }

  /* Serving-port field row (mirrors the pairing service-row card + the
     admin-calendar port-input control). */
  .field-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 1rem;
    padding: 1rem;
    border: 1px solid var(--border, #30363d);
    border-radius: 8px;
    background: var(--bg-surface, #161b22);
  }
  .field-row > span:first-child {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    flex: 1;
  }
  .field-controls { display: flex; align-items: center; gap: 0.5rem; }
  .nat-mode-card {
    display: flex;
    flex-direction: column;
    gap: 0.375rem;
    padding: 1rem;
    border: 1px solid var(--border, #30363d);
    border-radius: 8px;
    background: var(--bg-surface, #161b22);
  }
  .nat-radio { display: flex; align-items: center; gap: 0.5rem; cursor: pointer; }
  .nat-desc { margin-left: 1.5rem; }
  .nat-actions {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 1rem;
    margin-top: 0.5rem;
  }
  .port-input {
    width: 6rem;
    padding: 0.35rem 0.5rem;
    font-size: 0.85rem;
    border: 1px solid var(--border, #30363d);
    border-radius: 6px;
    background: var(--bg, #0d1117);
    color: var(--text, #e6edf3);
  }
  .btn {
    font-size: 0.8rem;
    padding: 0.375rem 0.75rem;
    border-radius: 6px;
    border: 1px solid var(--border, #30363d);
    cursor: pointer;
  }
  .btn:disabled { opacity: 0.5; cursor: not-allowed; }

  /* Outside-app sign-in keys: a kid is a base64url thumbprint — monospace, and
     free to wrap rather than widen the page. */
  .key-row {
    margin: 0.25rem 0;
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 0.85rem;
    overflow-wrap: anywhere;
  }
  .oauth-actions {
    display: flex;
    flex-wrap: wrap;
    gap: 0.5rem;
    margin-top: 0.5rem;
  }
</style>
