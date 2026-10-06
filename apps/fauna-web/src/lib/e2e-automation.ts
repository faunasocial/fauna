// The web app's e2e automation surface — every `window.__fauna_*` hook the
// Playwright bridge drives (`tests/e2e-unified/drivers/web.py`).
//
// This module exists ONLY in builds made for testing (testing.md § Test-agent
// build exclusion): the sole importer is `+layout.svelte`'s
// `if (__FAUNA_E2E_AUTOMATION__) import('$lib/e2e-automation')` branch, which a
// production `vite build` constant-folds away — so neither this chunk nor the
// hook installers it pulls in (`installRpcTestHooks`,
// `installConversationsCommandHook`) reach a production bundle. Never import it
// from production code paths, and add new `window.__fauna_*` hooks here (or
// behind the same flag), never bare in a component.

import { goto } from '$app/navigation';
import { identity, inbox, sent, knocks, contacts, connectionStatus } from '$lib/store';
import { feedSnapshot, feedReloads, type FeedReloads } from '$lib/feed';
import { devicesRefreshes } from '$lib/devices-session';
import { messageBanners, type FiredBanner } from '$lib/message-banner';
import { resetActorScopedState } from '$lib/actorScope';
import { installRpcTestHooks } from '$lib/rpc';
import { regionBlockRenderForTest } from '$lib/region.svelte';
import { silentSignIn, homeBearerScheduleForTest } from '$lib/api';
import { accountsClearNestBinding, accountsTabSessionMaterial } from '$lib/accounts';
import { clearTabPin } from '$lib/tabPin';
import {
  ensureWasm,
  enableDnsFakeProviderForTest,
  enableFakePlcDirectoryForTestCore,
  servingEnablement,
  type ServingEnablement,
} from '$lib/wasm';
import { enableFakePlcDirectoryForTest } from '$lib/wasm-atproto-settings';
import { launchClockForTest } from '$lib/wasm-launch';
import {
  conversationThreads,
  successionWitness,
  convRealBackendActive,
  registerConversationsCommands,
} from '$lib/conversations';
import { installCommandHook } from '$lib/e2e-commands';
import { registerBackupAuditCommands } from '$lib/backup-audit-e2e';
import { registerBackupCustodianCommands } from '$lib/backup-custodian-e2e';
import { registerAtprotoDelegationCommands } from '$lib/atproto-delegation-e2e';
import { registerTrustClockCommands } from '$lib/trust-clock-e2e';
import { registerFamilyNotifyCommands } from '$lib/family-notify-e2e';
import { registerAccountRuntimeCommands } from '$lib/account-runtime-e2e';
import { registerScreenTimeCommands } from '$lib/screen-time-e2e';
import { registerIdentityCommands } from '$lib/identity-e2e';
import { registerMailCaldavCommands } from '$lib/mail-caldav-e2e';
import { registerFocusWalkCommands } from '$lib/focus-walk-e2e';
import { registerLoudSurfaceCommands } from '$lib/loud-surface-e2e';
import { resetPostAuthEscalation } from '$lib/post-auth-escalation';
import {
  registerBarrierCommands,
  barrierProbeToken,
  barrierAckProbe,
  clearBarrierProbe,
} from '$lib/barrier-e2e';
import {
  sessionGeneration,
  recordSessionTeardown,
  clearSessionGeneration,
} from '$lib/generation-e2e';
import { criticalAlertSweepPasses, connectionIsOnline, renderDocumentLinkPreviews, type LinkPreviewState } from '$lib/wasm';
import { get } from 'svelte/store';
import {
  convReceiveCycles,
  convReceiveExit,
  convReceiveNow,
  mlsFoldedCommits,
  setConvPollSecs,
  setConvPushSuppressed,
} from '$lib/conversations';

/** The cross-app **connection barrier** observable
 *  (`fauna_e2e_agent::CONNECTION_KEY`) — `{state, online}`, published so a test
 *  can wait out the WS handshake instead of racing it.
 *
 *  Why the harness needs it: every app greys an `OnlineOnly` affordance while
 *  its transport word is offline (`fauna_protocol::offline_class::affordance`,
 *  the rule `$lib/offline-gate` binds), `'connecting'` is one of the offline
 *  words, and 276 of the 628 registered kinds are `OnlineOnly` — so a test
 *  driving an online-only control on a freshly loaded SPA loses exactly when
 *  the box is loaded, and the failure looks like the assertion rather than the
 *  race.
 *
 *  ⚠ **`online` is Rust's verdict, never `state === 'connected'`.** The gate is
 *  asymmetric on purpose (online unless the word is a *known* offline word), so
 *  an equality test would hang a barrier to its ceiling on the first future
 *  word — the one case the rule exists to tolerate. `connectionIsOnline` is the
 *  wasm face of that rule; nothing here re-derives the word list, exactly as
 *  `$lib/offline-gate` takes `{available}` rather than re-reading the class
 *  table.
 *
 *  `null` before wasm is initialized — the honest "cannot answer yet", which
 *  the shared waiter keeps polling through and only fails on if it never
 *  becomes an answer. It is NOT the same as `{online: false}` (a real transport
 *  still shaking hands), and collapsing the two would turn a missing leg into a
 *  silent wait. */
function connectionObservable(): { state: string; online: boolean } | null {
  const state = get(connectionStatus);
  try {
    return { state, online: connectionIsOnline(state) };
  } catch {
    return null;
  }
}

/** Install the whole `window.__fauna_*` automation surface. Called from the
 *  root layout at SPA boot, test builds only. Guarded once per page load:
 *  the layout can legitimately remount within one load (the initial-entry
 *  redirect recreates the tree), while this module — and the e2e command
 *  table — are module-scoped and survive it, so an unguarded re-install
 *  re-claims every command with fresh closures and the registry's duplicate
 *  rejection throws ("claimed by two handlers") on a perfectly healthy boot
 *  (observed 2026-08-02). */
let installed = false;
export function installAutomationSurface(): void {
  if (typeof window === 'undefined' || installed) return;
  installed = true;
  (window as any).__fauna_stores = {
    identity, inbox, sent, knocks, contacts, feedSnapshot, goto, conversationThreads,
    convRealBackendActive, successionWitness,
  };
  installRpcTestHooks();
  // Convention 17's `region_block_render` (`region-block-never-silent`,
  // `helpers/frame_invariants.py`): each mounted surface's verdict-side walk vs.
  // the block placeholders painted — tui's `region::block_render_json`.
  (window as unknown as { __fauna_regionBlockRender?: () => { blocked: number; placeholders: number } })
    .__fauna_regionBlockRender = regionBlockRenderForTest;
  // Flip the wasm fake DNS provider on so the onboarding-launch-glue /
  // managed-publish e2e can verify+publish a `fake-dns-ok:<zone>` credential
  // offline (no real registrar reachable from the sandbox). Mirrors native's
  // `FAUNA_DNS_PROVIDER_FAKE`. See `dns-management.md` § Where the credential lives.
  (window as unknown as { __fauna_enableDnsFakeProviderForTest?: () => Promise<void> })
    .__fauna_enableDnsFakeProviderForTest = async () => {
      await ensureWasm();
      enableDnsFakeProviderForTest();
    };
  // Point the ATProto custody check (critical-alerts.md feeder #1) at a fake
  // PLC directory so test_atproto_custody_alarm.py can drive the mint →
  // tamper → alarm → un-tamper → clear round trip with no real
  // plc.directory reachable — the wasm twin of native's
  // FAUNA_ATPROTO_PLC_DIRECTORY_URL. `enableFakePlcDirectoryForTest` loads
  // the atproto-settings chunk itself if it isn't up yet.
  //
  // Both chunks, not just this one: the session-start SWEEP (feeder #1's
  // sweep-driven path + feeder #3, both in the CORE chunk's
  // `runCriticalAlertSweep`) reads its own separately-compiled copy of the
  // directory override — setting only the atproto-settings chunk's copy left
  // the sweep 404ing against the real plc.directory. `enableFakePlcDirectoryForTestCore`
  // loads the core chunk itself if it isn't up yet (mirrors the atproto-settings call).
  (window as unknown as { __fauna_enableFakePlcDirectoryForTest?: (url: string) => Promise<void> })
    .__fauna_enableFakePlcDirectoryForTest = async (url: string) => {
      await Promise.all([
        enableFakePlcDirectoryForTest(url),
        enableFakePlcDirectoryForTestCore(url),
      ]);
    };
  // Drive the registry-routed nest-binding walk-away directly, so a test can
  // prove the reload-resurrection fix (`account-scoping.md` § Concurrent
  // instances, the delete corollary) without a live `fauna.admin.factory_reset`
  // round trip + nest restart — the admin-nest page's own trigger for this call.
  (window as unknown as { __fauna_clearNestBindingForTest?: (actorId: string) => Promise<void> })
    .__fauna_clearNestBindingForTest = (actorId: string) => accountsClearNestBinding(actorId);
  // Drop every piece of in-memory actor-scoped state — the plain-JS half of what
  // an identity switch does (all registered drops are pure module/component state
  // resets, no WASM needed — a standing requirement of a drop, not a happy
  // accident, and one this call site depends on: `resetScreenTime` broke it by
  // reading a LAZY CONSTRUCTOR (`usage()`, which builds through wasm), so it
  // threw `WASM not initialized` on every test login here and left its own state
  // half-dropped. `actorScope.ts` guards each drop, so the breakage was invisible
  // except in the console ring; `test_web_boot_effect_loop.py` now fails on it).
  // The test agent's `applyPatch({session})` deliberately
  // bypasses `identity.login()` (it would need WASM's `actorIdFromSecret`, and the
  // patch already carries `actor_id`), so without this a test that logs in as a
  // second actor via `set_state` — same nest or a different one — keeps rendering
  // the FIRST actor's state to the second: same nest, the subscriber sees the
  // author's still-live custody view (full body instead of a teaser); different
  // nest (e.g. a fresh per-test fixture), the manager's live connection points at
  // a torn-down nest and every read comes back empty. `agent.js` calls this before
  // applying a session patch whose `secret_hex` differs from the stored one.
  //
  // Delegating to `resetActorScopedState()` rather than naming the drops here is
  // the point: this hook used to hand-list the two managers, so every piece of
  // actor-scoped state added after it was written stayed live across a test's
  // switch — which is how the feed page's caches and this page's
  // twin both reached e2e as "empty, no error".
  //
  // ⚠ The window symbol keeps its historical `...ManagersForTest` name: it is a
  // cross-file contract with `web-bridge/agent.js`, which calls it optionally
  // (`?.()`), so a rename that missed one side would silently no-op rather than
  // fail — the exact shape convention 11 forbids. The name is narrower than what
  // it now does.
  (window as unknown as { __fauna_resetIdentityScopedManagersForTest?: () => void })
    .__fauna_resetIdentityScopedManagersForTest = () => {
      resetActorScopedState();
      // The escalation latch is module state, so a soft reset between tests must
      // clear it or the second test on a seat could never escalate again.
      resetPostAuthEscalation();
    };
  // Drop THIS TAB's pin (+ its nest-url sibling, `tabPin.ts`) — the
  // synchronous leaf of the erase `identity.logout()` reaches through
  // `accountsClearAll()` (`$lib/accounts.ts:297`). A `set_state` account
  // switch cannot call `identity.logout()` itself (WASM, and the patch is
  // applied with no reload — see the reset above), so `agent.js` calls this
  // hook instead of hand-listing the pin's storage keys itself: the owning
  // module keeps owning them (account-scoping.md § The scoping taxonomy —
  // one canonical drop, no list per teardown site). Without it a stale pin
  // survives a switch for the tab's whole life: `accountsSessionMaterial()`
  // fails closed for the incoming actor on the next read.
  (window as unknown as { __fauna_clearTabPinForTest?: () => void })
    .__fauna_clearTabPinForTest = clearTabPin;
  // ── The machine-free half of the E2E machine-method bridge ────────────────
  //
  // `driver.call_machine_method(name, json)` normally reaches
  // `$lib/onboarding/machine.svelte`'s hook — but that module's ONLY importer is
  // the onboarding page, so on a live authenticated session (which never mounts
  // it) the hook is simply absent. That is the same no-machine-post-auth wall
  // linux hit, and the shared answer is the same: the pin seed and its reader
  // touch only process-global state, so they are dispatched by
  // `fauna_onboarding_machine::call_machine_free_method` with no machine at all
  // (`security.md` § Post-auth surfacing, seam 1).
  //
  // Installed here — from the module every route loads — so it is up whenever the
  // agent is. It **defers** to the onboarding hook for anything the free
  // dispatcher does not claim, and installs itself only if that hook is not
  // already up, so the two can never fight over the single global slot (the
  // clobber hazard `$lib/e2e-commands` documents). The onboarding page's hook
  // routes the same names through the same shared dispatcher on its own, so
  // whichever ends up installed, one name table answers.
  (window as unknown as {
    __fauna_callMachineFreeMethod?: (n: string, a: string) => Promise<unknown>;
  }).__fauna_callMachineFreeMethod = async (name: string, jsonArg: string) => {
    const { callMachineFreeMethodForTest } = await import('$lib/wasm-onboarding');
    const handled = await callMachineFreeMethodForTest(name, jsonArg);
    if (handled === undefined || handled === null) {
      throw new Error(
        `machine-free bridge cannot serve '${name}' — it needs a live ` +
          'OnboardingMachine, which this route has not mounted',
      );
    }
    return JSON.parse(handled);
  };
  // The e2e command table behind `window.__fauna_callCommand`. Each domain claims
  // its own action names (`$lib/e2e-commands` rejects a duplicate claim), then the
  // hook goes up once — an unrecognised action throws rather than being dropped
  // (testing.md § convention 11).
  //   * conversations: `conversations_*` (+ `feed_inject_posts`) → the shared wasm
  //     managers, the browser twin of linux's bridge commands.
  //   * backup audit: `backup_audit_run_now` → the Backups page's own production
  //     audit path, with only the clock shifted.
  //   * family notify: `family_notify_check_now` → `$lib/familyNotify`'s own
  //     flush check, run_now-poked instead of waiting out its 5s interval
  //     (which this same agent doesn't even arm — see its own comment).
  //   * D10 delegation: `atproto_delegation_advance_clock` → the AT Protocol page's
  //     own machine refresh, with only the row's RENDER clock shifted (the
  //     ~90-day authorization window is unreachable by waiting — convention 14's
  //     fake clock, never the mint clock).
  registerConversationsCommands();
  registerBackupAuditCommands();
  //   * backup custodian: `backup_enroll_custodian_for_test` → the shared
  //     custodian enrol run as ANOTHER device of this owner, since enrolment is
  //     that device's act and declared absent on web; what the Backups page then
  //     shows for the row is the product's own read.
  registerBackupCustodianCommands();
  registerAtprotoDelegationCommands();
  //   * trust facet: `trust_facet_advance_clock` → the Nests page's grant
  //     liveness render clock (a ~90-day window — convention 14's fake clock).
  registerTrustClockCommands();
  //   * screen time: `screen_time_heartbeat` → the usage heartbeat's own
  //     production step, with only the clock advanced (convention 14's fake
  //     clock — sleeping for a real heartbeat would be a defunct test).
  registerFamilyNotifyCommands();
  registerAccountRuntimeCommands();
  registerScreenTimeCommands();
  //   * barrier: `barrier` (+ its `barrier_probe` self-test) → convention 14's
  //     causal anchor for negative asserts. See `$lib/barrier-e2e` for why web's
  //     shape is a double macrotask turn rather than an awaited promise.
  registerBarrierCommands();
  //   * identity: `silent_sign_in` → the production background silent challenge,
  //     re-run on demand. The trigger for web's post-auth nest-identity re-check
  //     (security.md § Post-auth surfacing); the handler it exercises is the
  //     product's own, not the command's.
  registerIdentityCommands();
  //   * mail: `enable_caldav_mailbox` → the shared mail-settings machine's
  //     production CalDAV-mailbox mint (the fixture step the dedicated-nest
  //     helper runs), its outcome published as `caldav_mailbox_reply`.
  registerMailCaldavCommands();
  //   * focus walk: `focus_move` / `switch_pane` → named refusals pointing at
  //     the DRIVER-level door (`tests/e2e-unified/drivers/web.py`), since
  //     in-page JS has no real key door to move focus through. See
  //     `$lib/focus-walk-e2e` and `e2e-systematic-ui-walks.md` § web leg.
  registerFocusWalkCommands();
  //   * loud surfaces: `alert_sweep_wake` / `reconnect_backoff` → the critical-
  //     alert loop's wait and the reconnect loop's pace, plus the
  //     `connection_reports` / `painted_errors` counters the connection-gap
  //     journeys read. See `$lib/loud-surface-e2e`.
  registerLoudSurfaceCommands();
  installCommandHook();
  // The barrier probe's observable + its one-test clear point. Read and cleared
  // by the harness-side `web-bridge/agent.js`, which is where web assembles the
  // state object the driver polls.
  (
    window as unknown as {
      __fauna_barrierProbeToken?: () => string | null;
      __fauna_clearBarrierProbe?: () => void;
    }
  ).__fauna_barrierProbeToken = barrierProbeToken;
  (window as unknown as { __fauna_barrierAckProbe?: () => string | null }).__fauna_barrierAckProbe =
    barrierAckProbe;
  (window as unknown as { __fauna_clearBarrierProbe?: () => void }).__fauna_clearBarrierProbe =
    clearBarrierProbe;
  // The session generation + its clear point — convention 14's negative-assert
  // observable (`fauna_e2e_agent::SESSION_GENERATION_KEY`). Read by the same
  // harness-side `web-bridge/agent.js` state assembly as the probes above.
  (window as unknown as { __fauna_sessionGeneration?: () => number }).__fauna_sessionGeneration =
    sessionGeneration;
  (
    window as unknown as { __fauna_clearSessionGeneration?: () => void }
  ).__fauna_clearSessionGeneration = clearSessionGeneration;
  // The critical-alert sweep's pass counters — the sweep's own negative-assert
  // barrier (`fauna_e2e_agent::ALERT_SWEEP_PASSES_KEY`). Same harness-side
  // assembly as the keys above; the accessor reads the core wasm chunk's
  // registry, which is the one the sweep posts to.
  (
    window as unknown as { __fauna_alertSweepPasses?: () => [number, number] }
  ).__fauna_alertSweepPasses = criticalAlertSweepPasses;
  // Per-channel folded-in inbound MLS commit counts — the twin-device barrier
  // (`fauna_e2e_agent::MLS_FOLDED_COMMITS_KEY`). Same harness-side assembly as
  // the keys above; the accessor reads the live conversations manager's shared
  // backend, so the counting is the identical Rust the natives publish.
  (
    window as unknown as { __fauna_mlsFoldedCommits?: () => Record<string, number> }
  ).__fauna_mlsFoldedCommits = mlsFoldedCommits;
  // The receive loop's cycle counters + its run-one-now poke
  // (`fauna_e2e_agent::{CONV_RECEIVE_CYCLES_KEY,CONV_RECEIVE_NOW}`). Web mirrors
  // the arms of the native loop on its own rail, so it mirrors these too: the
  // counters are bumped inside the one pump, and the poke drives that same pump
  // rather than a per-rail shortcut. The poke is a hook rather than a command in
  // `$lib/e2e-commands` because it is a one-liner over module state, the same
  // shape as the accessors above.
  (
    window as unknown as { __fauna_convReceiveCycles?: () => [number, number] }
  ).__fauna_convReceiveCycles = convReceiveCycles;
  // The loop's exit word beside the counters — `'stalled'` while a pass has
  // outrun the pump's ceiling (`$lib/receive-pump`), else `null`; the bridge
  // folds it into `conv_receive_cycles.exit`.
  (
    window as unknown as { __fauna_convReceiveExit?: () => 'stalled' | null }
  ).__fauna_convReceiveExit = convReceiveExit;
  // The shared feed manager's reload counters — the `conv_receive_cycles` twin
  // for the feed's re-query funnel (`fauna_e2e_agent::FEED_RELOADS_KEY`). Same
  // harness-side assembly as the keys above; the accessor reads the wasm
  // manager singleton's two atomics, so the counting is the identical Rust the
  // natives publish. Zeros before the singleton is built (the legitimate
  // "no reloads yet"); the no-leg answer is this hook being absent.
  (
    window as unknown as { __fauna_feedReloads?: () => FeedReloads }
  ).__fauna_feedReloads = feedReloads;
  // The Devices/Folders machine's refresh triple — the `feed_reloads` twin for
  // `DevicesMachine::refresh` (`fauna_e2e_agent::DEVICES_REFRESHES_KEY`). A
  // nav ack here says nothing about the section's mount refresh, so this is the
  // anchor; zeros before the session machine exists.
  (
    window as unknown as { __fauna_devicesRefreshes?: typeof devicesRefreshes }
  ).__fauna_devicesRefreshes = devicesRefreshes;
  // One post's link previews with their states (`{url, state}`, in body order) —
  // `data.feed.posts[].link_previews`, the shared `RenderDocument::link_previews`
  // projection (render-model.md § D4). A card is absent while a preview is still
  // resolving too, so this is what lets a test wait until a preview has FAILED
  // before it reads "no card". The bridge maps it over each post's `document`;
  // the no-leg answer is this hook being absent (the key is then omitted).
  (
    window as unknown as {
      __fauna_feedLinkPreviews?: (doc: unknown) => LinkPreviewState[];
    }
  ).__fauna_feedLinkPreviews = (doc) =>
    doc ? renderDocumentLinkPreviews(doc as Parameters<typeof renderDocumentLinkPreviews>[0]) : [];
  // The post-claim serving-enablement step's completion anchor
  // (`fauna_e2e_agent::SERVING_ENABLEMENT_KEY`). The accessor re-parses the
  // shared Rust derivation off the core wasm chunk the step runs in; an empty
  // run list before it runs, and the no-leg answer is this hook being absent.
  (
    window as unknown as { __fauna_servingEnablement?: () => ServingEnablement }
  ).__fauna_servingEnablement = servingEnablement;
  // The new-message OS banners this tab actually fired, plus the diff-tick
  // counters that make a NEGATIVE read of them sound
  // (`fauna_e2e_agent::MESSAGE_BANNERS_KEY`, which owns the contract). Same
  // harness-side assembly as the keys above; the accessor reads
  // `$lib/message-banner`'s log, whose entries are appended at the firing site,
  // so it says what the user was shown rather than what the shared tracker
  // returned. Absent hook ⇒ the bridge sends `null`, which the consumer refuses
  // loudly — an empty list from an app with no leg would satisfy every negative
  // assertion the key exists to make.
  (
    window as unknown as {
      __fauna_messageBanners?: () => { started: number; completed: number; fired: FiredBanner[] };
    }
  ).__fauna_messageBanners = messageBanners;
  // The transport's own state + the offline gate's verdict on it — the
  // connection barrier's observable (`fauna_e2e_agent::CONNECTION_KEY`), read
  // once per login by `helpers/connection.py::wait_until_online`. Absent hook
  // (or wasm not up yet) ⇒ null, which the waiter polls through rather than
  // reading as "online"; see `connectionObservable` for why the boolean crosses
  // the boundary already decided.
  (
    window as unknown as { __fauna_connection?: () => { state: string; online: boolean } | null }
  ).__fauna_connection = connectionObservable;
  // The launch clock this tab signs in on — the wrong-clock launch witness's
  // in-app control (`fauna_e2e_agent::CLOCK_KEY`), read off the launch chunk's
  // test-flavor getters. The offset itself is seeded by the chunk's own
  // `LaunchMachine` constructor from localStorage, synchronously before
  // `start()` — never from here: this surface installs through an async import
  // (`+layout.svelte`) and could land after the launch has already signed in.
  // `null` until the launch chunk is up, which the bridge omits.
  (
    window as unknown as {
      __fauna_launchClock?: () => { offset_secs: number; now_secs: number } | null;
    }
  ).__fauna_launchClock = launchClockForTest;
  // The held home-nest bearer's schedule on this tab's own clock — the web leg
  // of `fauna_e2e_agent::LAUNCH_TOKEN_KEY` (the wrong-clock refresh witness,
  // case M). A plain read of `$lib/api`'s cache, never a mint; `null` with no
  // identity on the seat, which the bridge omits.
  (
    window as unknown as {
      __fauna_launchToken?: () => { expires_in_secs: number | null; own_session_ids: string[] } | null;
    }
  ).__fauna_launchToken = () => {
    const secret = get(identity)?.secretHex ?? accountsTabSessionMaterial()?.secret_hex;
    return secret ? homeBearerScheduleForTest(secret) : null;
  };
  (window as unknown as { __fauna_convReceiveNow?: () => void }).__fauna_convReceiveNow =
    convReceiveNow;
  // The receive rail's backstop cadence — the web twin of native's
  // `FAUNA_CONV_POLL_SECS` launch env var, which web cannot take (the SPA has no
  // process environment). Runtime rather than launch-time, which is strictly more
  // capable: a test can mute the ticker mid-session on the SHARED web driver and
  // restore it afterwards, instead of paying for a second Chromium per cadence.
  // Muting is what makes a push-arm test on this rail able to FAIL — see
  // `setConvPollSecs`' own contract for why the re-arm is load-bearing.
  (
    window as unknown as { __fauna_setConvPollSecs?: (secs: number | null) => void }
  ).__fauna_setConvPollSecs = setConvPollSecs;
  // The other half of the same pair — web's twin of native's
  // `FAUNA_E2E_SUPPRESS_CONV_PUSH`, switching off the push + reconnect arms so the
  // durable inbox-apply drain is the rail's only path. Between the two knobs each
  // arm of the web rail can be proven ALONE, which is what the natives get from
  // their env var plus a muted `FAUNA_CONV_POLL_SECS`.
  (
    window as unknown as { __fauna_setConvPushSuppressed?: (on: boolean) => void }
  ).__fauna_setConvPushSuppressed = setConvPushSuppressed;
  // The bump itself is a hook rather than a `$lib/generation-e2e` import in the
  // account-switcher route: that keeps the module out of a production bundle
  // entirely (convention 15's strong form), instead of shipping it as dead code
  // behind a folded-false flag.
  (
    window as unknown as { __fauna_recordSessionTeardown?: () => void }
  ).__fauna_recordSessionTeardown = recordSessionTeardown;
  // Run the silent sign-in challenge/verify ceremony for the stored identity
  // and return the verified actor (or null when unregistered). Lets
  // `test_web_silent_sign_in_ws_rpc` assert the ceremony rides anonymous WS-RPC
  // (`fauna.auth.{challenge,verify}`), not the deprecated HTTP twins.
  (window as unknown as {
    __fauna_silentSignIn?: (nestUrl?: string) => Promise<unknown>;
  }).__fauna_silentSignIn = async (nestUrl?: string) => {
    const secret = accountsTabSessionMaterial()?.secret_hex;
    if (!secret) return null;
    return silentSignIn(secret, nestUrl);
  };
}
