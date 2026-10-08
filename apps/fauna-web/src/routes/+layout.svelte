<script lang="ts">
  import '../app.css';
  import { page } from '$app/stores';
  import { goto } from '$app/navigation';
  import { onMount } from 'svelte';
  import { identity, contacts, connectionStatus, onActorChange } from '$lib/store';
  import { t } from '$lib/i18n/strings';
  import {
    selfHealDeploymentSeedCustody,
    type DeploymentSeedCustodyWarning,
    reportHostAddress,
    refreshMailEpochSchedule,
    runCriticalAlertSweep,
    runSealBackfillSweep,
    familyStatus,
    type FamilyStatus,
  } from '$lib/rpc';
  import MessageBanner from '$lib/components/MessageBanner.svelte';
  import CriticalAlertsBanner from '$lib/components/CriticalAlertsBanner.svelte';
  import AppShellFrame from '$lib/components/AppShellFrame.svelte';
  import { checkIsAdmin } from '$lib/api';
  import { accountsAutoEnableRequireConfirm, accountsTabSessionMaterial } from '$lib/accounts';
  import { tabPin } from '$lib/tabPin';
  import { loadPendingFactoryReset } from '$lib/onboarding/pending-factory-reset-store';
  import { ensureLogging, ensureWasm, connectionStateLabel } from '$lib/wasm';
  import { resolveLocalized } from '$lib/i18n/localized';
  import { startReceivePoll, flushDraftsNow as flushConversationsDraftsNow } from '$lib/conversations';
  import { flushDraftsNow as flushFeedDraftsNow } from '$lib/feed';
  import { flushCuesNow } from '$lib/feed-cues';
  import { flushEventDraftNow } from '$lib/event-drafts';
  import { startSubscriptionsAuthorPump } from '$lib/subscriptionsAuthor';
  import { rearmPush } from '$lib/push';
  // The post-succession aftermath pass — see its registration below.
  import { runSuccessionAftermath } from '$lib/rpc';
  import { successionKitOwed } from '$lib/wasm';
  import { configStageSettled, recordAftermathLeg } from '$lib/succession-aftermath';
  import { refreshMemberReviewRoster } from '$lib/member-reviews';
  import {
    restoreSupervisionSnapshot,
    restoredGuardian,
    clearRestoredSupervision,
  } from '$lib/supervision.svelte';
  import { setGuardianHalf } from '$lib/contentPolicy.svelte';
  import { openRegion, refreshRegion } from '$lib/region.svelte';
  import { setWardAsksFromStatus } from '$lib/wardAsks.svelte';
  import {
    currentLockMessage,
    setWardScreenTime,
    tickUsageHeartbeat,
  } from '$lib/screenTime.svelte';
  import { IDS } from '$lib/generated/uiIds';

  let { children } = $props();

  /** Set once the wasm module is up — see `connectionLabel`. */
  let wasmReady = $state(false);

  if (typeof window !== 'undefined') {
    if (__FAUNA_E2E_AUTOMATION__) {
      // Test builds only (testing.md § Test-agent build exclusion): install the
      // whole `window.__fauna_*` automation surface. A production `vite build`
      // constant-folds this flag to false and emits neither this branch nor the
      // dynamic chunk, so the hooks are absent from the shipped bundle — not
      // merely inert.
      void import('$lib/e2e-automation').then((m) => m.installAutomationSurface());
    }
    // Install the ring-only logging subscriber at SPA boot (observability.md
    // § Surfaces / § Persistence & privacy — Web): the shared `fauna_log::RingLayer`
    // + browser console, no on-disk file. Seeds a startup line so the Settings →
    // Logs page is never empty on first open. Memoized + awaitable (the Logs
    // page awaits the same promise before reading the ring, so the read never
    // races this async install). Fire-and-forget here.
    // Also gates `connectionLabel` below: the shared label mapping lives in wasm,
    // and `wasm()` throws until the module is up, so the indicator falls back to
    // the plain "Disconnected" reading for the pre-init frame (which is exactly
    // what `connectionStatus` holds until a client is built anyway).
    void ensureLogging().then(() => {
      wasmReady = true;
    });
    // Start the app-wide conversations receive poll (both rails: the FaunaMls MLS
    // DM backstop — drainInbox + pollConversations — and the SMTP receive rail).
    // Started here (not gated on the current route / a present session) because
    // the layout mounts once: if the app first loaded before login, an
    // onMount-gated start would never fire on a later login. The loop self-gates
    // — it no-ops each tick until an identity appears; the FaunaMls drain runs
    // whenever the real backend is wired, the SMTP poll waits for mail-enable.
    // Fire-and-forget; idempotent.
    void startReceivePoll();
    // Start the app-wide author auto-approve pump (the browser twin of linux's
    // AuthSuccess `subscriptions_author::start` + windows `SubscriptionsAuthorPump`):
    // on connect it runs one shared `subscriptionsReconcileOnce` tick (resume then
    // drain), then re-ticks on the same receive cadence. This is what grants a
    // queued encrypted-mode FOLLOW with no manual approve — the nest can't mint the
    // KeyBlob, so the author's own client must (monetization.md § grant path 2).
    // Self-gating (no-ops until an identity appears); fire-and-forget; idempotent.
    void startSubscriptionsAuthorPump();
    // Keep this browser's push subscription pointed at whoever is signed in
    // now (succession-aftermath.md § Implementation status today — "a
    // successor's own device does not re-subscribe to push"). `onActorChange`
    // fires on every identity settle, first load included, so a reload after
    // a succession lands here with no Settings visit required; the handler
    // itself no-ops unless this browser already holds a push subscription.
    // Run the post-succession aftermath — the ordered pass
    // a successor's first authenticated session owes (`succession-aftermath.md`
    // § Re-key scope → the `BackupKey` corpus row; `succession-aftermath.md`
    // owns the wider pass). The browser twin of tui's `session.rs` hook, over
    // the same shared driver.
    //
    // The legs restore backups, trusts and drafts, and burn the mail
    // credentials the retired seed's holder would otherwise keep reading with.
    // (Before the `__config` rail retired at closure step (6), a leg 1 also
    // re-sealed the blob; see `docs/goal/architecture/config-dissolution.md`.)
    //
    // Registered FIRST among the actor-change handlers on purpose — they fire
    // in insertion order, and the account-state readers elsewhere in the app
    // benefit from the pass starting ahead of them. ⚠ That is a HEAD START, not a
    // barrier: this is fire-and-forget (as is every other handler here), so a
    // reader can still lose the race by a round trip. What makes that
    // acceptable rather than hidden is that those readers self-heal — they
    // re-read on the nav edge or the next poll tick. tui's hook declares the
    // same residual for the same reason. (The barriers *inside* the pass are
    // real ones; that is what the shared driver exists to state once.)
    //
    // Free for an identity that never succeeded (no predecessors ⇒ it returns
    // before any round trip), which is why this is unconditional rather than
    // gated on detecting a succession. Idempotent, so a reload after a
    // completed pass costs one `get` per plane. Log-only: a failure here must
    // not break launch, and the next actor settle retries.
    onActorChange(async (id) => {
      try {
        // Logged UNCONDITIONALLY, including the boring `not-a-successor`. The
        // browser console ring is bridge-captured and readable from a test
        // (`drivers/web.py::console_log`), and every failure mode of this pass
        // is silent by construction — "not-a-successor" is what a MISSING
        // predecessor link looks like, and it is indistinguishable from a
        // healthy non-successor unless the line is here. That exact ambiguity
        // cost a 30-minute e2e cycle on 2026-08-20.
        console.info(
          '[succession] aftermath:',
          await runSuccessionAftermath(id.secretHex, recordAftermathLeg),
        );
      } catch (e) {
        console.warn('[succession] aftermath failed; will retry on next sign-in:', e);
      }
    });
    // The unattested-member roster, read BEHIND the raise — the first of the
    // two refresh points `succession-aftermath.md` § Propagation → *MLS
    // groups* fixes, and what makes a successor's FIRST session show the
    // marks the ceremony it just ran produced. The raise is the
    // post-store-ready pass's (the roster rests on the succession ledger in
    // this tab's account store), which reports `configStageSettled` when its
    // raises have had their turn — unconditionally, because items raised by
    // *earlier* ceremonies are already at rest and a refused raise must not
    // also leave a flagged person unmarked.
    configStageSettled.subscribe((settled) => {
      const id = $identity;
      if (settled > 0 && id) void refreshMemberReviewRoster();
    });
    onActorChange((id) => void rearmPush(id));

    // Leave-flush (`reserved-folders.md` § The leave-flush promise, row 481):
    // web's leave door is the tab-leave events the browser actually
    // delivers — `visibilitychange` to `hidden` fires reliably (backgrounding
    // a tab, not necessarily closing it — the browser twin of a phone app
    // moving to the background) and `pagehide` is the last signal before an
    // actual unload. The goal doc states plainly that a browser grants no
    // reliable async work after either event, so this is deliberately
    // best-effort — same posture as every other handler in this block
    // (fire-and-forget), just with no guarantee the write lands before the
    // tab is gone. Each rail's own flush is a no-op when nothing is open on
    // that rail (no manager / no held draft).
    const flushAllDraftsNow = () => {
      flushConversationsDraftsNow();
      flushFeedDraftsNow();
      flushEventDraftNow();
    };
    // The engagement-cue rollup rides the same last signal
    // (engagement-cues.md § At rest: put on background/close): end the live
    // capture's exposures, then put the sealed rollup.
    const flushAllNow = () => {
      flushAllDraftsNow();
      flushCuesNow();
    };
    document.addEventListener('visibilitychange', () => {
      if (document.visibilityState === 'hidden') flushAllNow();
    });
    window.addEventListener('pagehide', flushAllNow);
    // The succession's CLOSING ACT — land this launch where the kit renders.
    //
    // Only the NAVIGATION is here; the mint is the Recovery Kit section's own,
    // from its hydrate, so the secret lands in the state that renders it. That
    // split is apple's (each target's launch sets `selectedSettingsPage =
    // .account`; `RecoveryKitVM.dischargeOwedSuccessionKit` mints) and it is
    // forced by the order the goal doc makes load-bearing: entering Account
    // CLEARS any kit on screen (the shown-once custody rule), so a mint that
    // landed before the navigation would be wiped by the navigation itself.
    //
    // A peek, never a claim — one reader decides where to land, exactly one
    // claimant performs. Fires at most once per succession: `successionKitOwed`
    // is false on every ordinary sign-in, so this costs one sessionStorage read
    // at boot and navigates nobody who is not a fresh successor.
    onActorChange(async (id) => {
      try {
        if (await successionKitOwed(id.actorId)) {
          console.info('[succession] the successor owes itself a kit — landing on Account');
          await goto('/app/settings/account', { replaceState: true });
          console.info('[succession] landed on', window.location.pathname);
        }
      } catch (e) {
        // Log-only, like the aftermath above: a failed read must not break
        // launch, and the obligation is still recorded — the section discharges
        // it whenever the user next opens Account.
        console.warn('[succession] could not check for an owed kit:', e);
      }
    });
  }

  let isOnboarding = $derived($page.url.pathname.startsWith('/app/onboarding'));
  // The admin shell is a vertical sidebar-swap (admin.md § Navigation model):
  // while in `/app/admin` the admin layout renders its OWN shell whose rail
  // takes over the sidebar slot, so the root layout yields the full viewport
  // (same bare-canvas bypass as onboarding) — the normal nav is replaced in
  // place, not shown beside a horizontal admin nav-bar.
  let isAdmin = $derived($page.url.pathname.startsWith('/app/admin'));
  // The Settings shell is the same vertical sidebar-swap (settings.md
  // § Navigation model, ratified 2026-06-03 — parallel to admin): while in
  // `/app/settings` the settings layout (routes/settings/+layout.svelte)
  // renders its OWN rail (settings-nav-back atop + the flat sub-page list)
  // that replaces the app sidebar in place, so the root layout yields the
  // full viewport here too. `settings-nav-back` swaps it back to the app.
  let isSettings = $derived($page.url.pathname.startsWith('/app/settings'));

  // Whether the current identity is a nest admin — gates the `admin-tab` nav
  // entry (admin.md § Navigation model: a single gated admin entry, shown only
  // when `am-i-admin` passes, opens the admin shell). This is the canonical
  // nav-row entry the linux/windows reference clients render and that ui.yaml's
  // `navigation.gated_tabs` now declares + lint-enforces; web previously only
  // inlined an untagged link into Settings (the drift this closes). The check
  // is fail-closed (checkIsAdmin treats any transport/auth failure as not-admin).
  let userIsAdmin = $state(false);
  $effect(() => {
    const id = $identity;
    const secret = id?.secretHex;
    if (!secret) {
      userIsAdmin = false;
      return;
    }
    checkIsAdmin(secret)
      .then((ok) => {
        userIsAdmin = ok;
        // The Stage-2 admin auto-default (long-term-store.md § Multi-account
        // evolution → Per-account re-auth): every am-i-admin=true observation
        // auto-enables the observed account's require-confirm flag — unless the
        // user ever touched its toggle (require_confirm_user_set pins their
        // choice; the shared registry enforces both halves, so this is
        // idempotent and never flips OFF). This nav-gate probe is web's
        // canonical observation point: it re-fires on every page load and every
        // identity change, including the post-switch reload. Keyed to the
        // OBSERVED identity's own actor id — never the registry's active
        // pointer — so a mid-append transient identity can't flag the wrong
        // account. Fire-and-forget: a UX default is never worth failing the
        // nav gate over (e.g. the observed identity isn't registry-held yet).
        if (ok && id.actorId) {
          accountsAutoEnableRequireConfirm(id.actorId).catch(() => {});
        }
      })
      .catch(() => { userIsAdmin = false; });
  });

  // The caller's family relationships (family-safety.md § App surface) — ONE
  // `fauna.family.status` read gates BOTH the `family-tab` nav entry (shown when
  // the status returns any relationship, guardian or supervised — ui.yaml
  // `navigation.gated_tabs`, exactly like `admin-tab`'s `am-i-admin` gate) and the
  // permanent global `supervised-indicator` (rendered on every page when this
  // account is supervised). Re-read on each connection-state change so the gate
  // resolves as soon as the socket comes up (and re-resolves after a reconnect /
  // a graduation).
  //
  // A FAILED read keeps the last-known state (family-safety.md § Content
  // policy, the unfetched-policy ruling clause 1): "read failed" and "read
  // says unsupervised" are different facts, and this read re-fires exactly
  // when it is least likely to succeed (boot, WS reconnect) — the old
  // `catch → famStatus = null` cleared a supervised ward's screen-time lock
  // inputs on any reconnect blip, the same latent defect linux's status
  // producer had. State moves only on a successful read or on the actor
  // change below (the one other transition clause 1 permits).
  let famStatus = $state<FamilyStatus | null>(null);
  // Restore the last-known supervision snapshot when the identity becomes
  // known — at launch, ahead of the first read landing (clause 2), and again
  // on every actor change. The same guard is what drops the OUTGOING actor's
  // famStatus on a switch, so a failed read for the incoming actor can never
  // leave the previous actor's supervision rendering.
  let restoredForActor: string | null = null;
  $effect(() => {
    const actorId = $identity?.actorId ?? null;
    if (actorId === restoredForActor) return;
    restoredForActor = actorId;
    famStatus = null;
    if (actorId) void restoreSupervisionSnapshot(actorId);
  });
  $effect(() => {
    const secret = $identity?.secretHex;
    void $connectionStatus;
    if (!secret) {
      famStatus = null;
      return;
    }
    familyStatus(secret)
      .then((s) => {
        famStatus = s;
        // The content floor + Guardian Notify move with this read too — off the
        // reply's gated `supervision` fold, never raw `policy`. This effect is
        // the ONE status read that re-fires on a WS reconnect (clause 1's second
        // moment): the social surfaces hydrate only on mount and a reconnect
        // remounts nothing, so without this a guardian's edit bound only at the
        // ward's next login.
        setGuardianHalf(s.supervision.content_policy, s.supervision.content_notify);
        // …and the ward's own outstanding asks (the durable half of the
        // contacts / profile / bridges ask surfaces), gated on supervised_by.
        setWardAsksFromStatus(s);
        // The live reply supersedes any restored fallback (the wasm choke
        // point has already re-persisted the snapshot from it).
        clearRestoredSupervision();
      })
      .catch(() => {
        // Clause 1: no information — keep whatever the last successful read
        // (or the launch restore) established.
      });
  });
  // Widened for the transfer handshake (family-safety.md § Graduation &
  // transfer → Visibility): a PROPOSED guardian may have no other family
  // relationship, and without the incoming_transfers arm they could never
  // reach the prompt. (A pending outgoing proposal implies wards > 0, so that
  // arm is already covered; stated in ui.yaml's gated_tabs note.) The
  // restored-supervision fallback keeps the tab reachable for a supervised
  // ward before/without a successful read — the ward must always be able to
  // see who supervises them (the same gate tui's snapshot restore drives).
  let hasFamilyRelationship = $derived(
    (!!famStatus &&
      ((famStatus.wards?.length ?? 0) > 0 ||
        !!famStatus.supervised_by ||
        (famStatus.incoming_transfers?.length ?? 0) > 0)) ||
      restoredGuardian() !== null,
  );
  // The supervised indicator's guardian: the live read when one has landed,
  // else the restored last-known supervision (transparency invariant 4 —
  // supervision is never silent, a cold offline launch included).
  let supervisedHandle = $derived(famStatus?.supervised_by?.handle ?? restoredGuardian());

  // Screen time (family-safety.md § Screen time): the same status read that
  // gates the indicator also feeds the lock's inputs, so no second RPC exists
  // and the two supervised surfaces can never disagree about who the guardian
  // is. Both come off the reply's `supervision` fold, whose graduation gate keeps
  // a guardian-less reply's leftover policy from locking anyone. The family
  // page re-runs `setWardScreenTime` from its own read, which is what makes a
  // guardian's edit bind without a reconnect.
  $effect(() => {
    setWardScreenTime(
      famStatus?.supervision.screen_time,
      famStatus?.supervision.supervised_by?.handle ?? null,
      famStatus?.usage_today_minutes,
    );
  });
  // The lock verdict is time-dependent, so it is re-asked on every navigation
  // and on a one-minute tick — the resolution of the policy itself (both window
  // bounds and the budget are whole minutes), so a finer tick cannot change an
  // answer. `lockTick` exists purely to invalidate the derived below.
  // The same tick drives the screen-time usage heartbeat (§ Screen time — the
  // budget half). One minute is also what the shared engine's accrual step
  // requires of a caller: it credits at most one step per call, so a slower
  // tick would silently under-count real use. Whether a report actually goes
  // out is the engine's own cadence decision, not this timer's.
  let lockTick = $state(0);
  // The blessed-grant auto-renew loop (`$lib/nests-auto-renew`): a pass as the
  // signed-in app comes up, then every `AUTO_RENEW_CHECK_SECS`; restarted when
  // the identity changes.
  $effect(() => {
    const secret = $identity?.secretHex;
    if (!secret) return;
    let stop: (() => void) | null = null;
    let cancelled = false;
    void import('$lib/nests-auto-renew').then((m) => {
      if (!cancelled) stop = m.startNestsAutoRenew(secret);
    });
    return () => {
      cancelled = true;
      stop?.();
    };
  });
  $effect(() => {
    const timer = setInterval(() => {
      lockTick += 1;
      const secret = $identity?.secretHex;
      if (secret) void tickUsageHeartbeat(secret);
      if (secret) void refreshRegion(secret, false);
    }, 60_000);
    return () => clearInterval(timer);
  });
  let screenLock = $derived.by(() => {
    void lockTick;
    void famStatus;
    return currentLockMessage();
  });
  // The Family page stays reachable read-only while locked — the ward must
  // always be able to see who supervises them and what the policy is.
  let onFamilyPage = $derived($page.url.pathname.startsWith('/app/family'));

  // The deployment-seed custody leg's post-auth edge (box-recovery.md § The
  // plane-era recovery floor, (c) The writes) — the ONLY capture of this box's
  // deployment seed onto the account plane: no claim-time capture, no fan-out.
  // Fired on EVERY connect (each transition into `connected`, and each identity
  // change): the leg runs at whichever of this edge and the account runtime's
  // store-ready edge lands second, and at every later post-auth edge, so a
  // transient failure is retried at the next one. The steady state (the plane
  // already holds this box) makes no round trip. A run that ends with custody
  // unconfirmed for an admin of this nest warns on the shell's message banner —
  // from either edge, through the sink handed over here.
  function surfaceRecoveryCustodyWarning(warning: DeploymentSeedCustodyWarning): void {
    const message =
      warning === 'mismatch'
        ? t.launch.recovery_custody_mismatch
        : t.launch.recovery_custody_failed;
    window.dispatchEvent(new CustomEvent('fauna-message-update', { detail: { warning: message } }));
  }
  let seedCustodyEdgeFor: string | null = null;
  $effect(() => {
    const secret = $identity?.secretHex;
    if (!secret || $connectionStatus !== 'connected') {
      // Leaving `connected` arms the next post-auth edge.
      seedCustodyEdgeFor = null;
      return;
    }
    if (seedCustodyEdgeFor === secret) return;
    seedCustodyEdgeFor = secret;
    void selfHealDeploymentSeedCustody(secret, surfaceRecoveryCustodyWarning).catch((e) => {
      // Rejects only on a malformed secret or a dead socket — the next
      // connect re-runs it.
      console.warn('deployment-seed custody leg failed (best-effort):', e);
    });
  });

  // Opportunistically refresh the published mail content-sealing epoch
  // schedule once per session at the same universal post-auth point
  // (`encryption-at-rest.md` § Capability tiering → Content-sealing epochs),
  // mirroring linux's `refresh_mail_epoch_schedule` posture exactly. A no-op
  // when mail isn't enabled; without it a schedule published at enable-mail
  // time slides stale past the publish horizon for a user who never re-runs
  // enable/rotate — the design's degradation (seal under the newest
  // published epoch) covers that gap safely in the meantime, so a failure
  // here is never surfaced to the user.
  let didRefreshEpochSchedule = false;
  $effect(() => {
    const secret = $identity?.secretHex;
    if (didRefreshEpochSchedule || !secret || $connectionStatus !== 'connected') return;
    didRefreshEpochSchedule = true;
    void refreshMailEpochSchedule(secret).catch((e) => {
      // Best-effort — re-converges on the next connect.
      console.warn('mail epoch schedule refresh failed (best-effort):', e);
    });
  });

  // The region content plane (`region-blocking.md` § How an app obtains its
  // region's policy): the device's plane opens at boot, its persisted record
  // loaded AHEAD of the first fetch, and asks the session's nest (the relay) at
  // every connect; the minute tick below re-asks on the shared cadence. The
  // plane is the device's — an identity change only resets its refresh clock
  // (`$lib/region.svelte`'s actor-scoped reset).
  onMount(() => {
    void openRegion().catch((e) => console.warn('region plane failed to open:', e));
  });
  $effect(() => {
    const secret = $identity?.secretHex;
    if (!secret || $connectionStatus !== 'connected') return;
    void refreshRegion(secret, true);
  });

  // Report the nest's PUBLIC IP once per session at the same universal post-auth
  // point (the web twin of linux's post-auth `report_host_address` + the native
  // FFI): the admin client tells the nest its public host-address so ACME HTTP-01
  // gates on the strong resolve-check (`domains-and-tls-bootstrap.md`
  // § Host-address acquisition). Admin-gated (`set_host_address` is Admin-only, so
  // gating avoids a pointless failing RPC on every non-admin connect) and fired
  // once; idempotent last-writer-wins nest-side. Fire-and-forget — a
  // `skipped_no_public_ip` on a LAN box is normal, never publishes a private address.
  let didReportHost = false;
  $effect(() => {
    const secret = $identity?.secretHex;
    if (didReportHost || !secret || !userIsAdmin || $connectionStatus !== 'connected') return;
    didReportHost = true;
    void reportHostAddress(secret).catch((e) => {
      // Non-fatal — the nest keeps its self-signed floor + weak gate; the next
      // connect retries.
      console.warn('host-address report failed (best-effort):', e);
    });
  });

  // Run the feeders that have no page of their own, at the same universal
  // post-auth point (`critical-alerts.md` § Mechanism → Who runs the
  // detector) — the web twin of tui's `session::establish` call. Fire-and-forget; sweeps
  // immediately and then every `RE_SWEEP_INTERVAL_SECS` for as long as the
  // identity lives — a standing alert (if any) is left alone on a failed
  // sweep and the next one retries.
  //
  // Guarded on the `identity` store's OBJECT, not a plain "ever ran" boolean:
  // a plain boolean latches true forever after the first authenticated
  // actor, so a later actor switch (or a same-actor re-establish, both
  // reachable without a page reload) would never sweep again — the SPA's
  // twin of linux's `apply_session_patch` gap, found the same way: `test_alert_sweep_directory_feeders_e2e.py`'s
  // sweep-driven path re-sets the SAME actor's identity to simulate a fresh
  // session establishment. `identity.set()` is a plain Svelte `writable` —
  // every call hands subscribers a fresh object, even with identical field
  // values — so comparing the object reference (not `secretHex`) is what
  // lets a same-actor re-establish re-trigger the sweep.
  let sweptForIdentity: unknown = undefined;
  $effect(() => {
    const id = $identity;
    const secret = id?.secretHex;
    if (!secret || $connectionStatus !== 'connected' || sweptForIdentity === id) return;
    sweptForIdentity = id;
    void runCriticalAlertSweep(secret).catch((e) => {
      console.warn('session-start critical-alert sweep failed (best-effort):', e);
    });
    // The S8 seal-backfill sweep rides the SAME re-establish gate (one pass per
    // identity, not per navigation) — web's D1/D3 leg, the last client to get
    // one. Fire-and-forget beside the alert sweep: it stamps sealed siblings a
    // prior unwired session left plaintext-only, and every failure mode inside
    // it is already best-effort (`path-sealing.md` § Implementation status
    // today).
    void runSealBackfillSweep(secret).catch((e) => {
      console.warn('session-start seal-backfill sweep failed (best-effort):', e);
    });
  });

  // Global connection-status indicator label (top of the sidebar) — the live
  // nest WS-RPC state, the web twin of linux's top-of-sidebar indicator. The
  // state → label decision is the shared `fauna_core::format::connection_state_label`
  // over wasm (priority #2), not a web-local ternary: a *transient* swap still
  // shows as "Connecting…" and never as an error, but a connection that has
  // failed to establish repeatedly reports `unreachable` → "Cannot connect", so
  // a browser that can never open the socket (e.g. a nest on its self-signed
  // floor, which no browser will carry a WSS handshake over) says so instead of
  // showing "Connecting…" forever. transport.md § Connection-status indicator.
  let connectionLabel = $derived(
    wasmReady ? resolveLocalized(connectionStateLabel($connectionStatus)) : t.common.disconnected,
  );

  // SINGLE OWNER of the `/app` root-index redirect (one owner per claim). This
  // guard alone decides where an app-root entry lands; the root
  // `+page.svelte` deliberately no longer redirects. It used to `goto`
  // `/app/conversations` from its own onMount, racing this guard — the winner
  // decided only by Svelte's child-before-parent onMount order (the page fires
  // first, this layout's `goto` fires second and supersedes it). That "two
  // owners" shape is correct today by ordering luck alone; a later timing change
  // (or a third redirect) could silently invert it and strand an unauthenticated
  // user on the feed. Folding both decisions here removes the race.
  //
  // Two decisions, in order:
  //  1. No credentials — OR a pending factory-reset slot present — ⇒ onboarding.
  //     The launch machine (which boot-reconciles the slot: pre-fill the claim
  //     page on a wiped box, or clear a stale slot on a still-claimed one) only
  //     runs on the onboarding route, so a client that crashed mid-factory-reset
  //     lands here with credentials still present and, without this, is stranded
  //     on the feed unable to re-claim its wiped box — a client-unrecoverable
  //     state the invariant forbids (nest/common.md § Client-state
  //     recoverability, CR-1/CR-2). The slot is the active account's per-actor
  //     row, read through the SAME registry accessor the launch machine routes
  //     on (`loadPendingFactoryReset`), so the gate and the machine can never
  //     disagree.
  //  2. Authenticated and sitting on the bare index ⇒ the default landing page
  //     (Conversations) — the role the removed `+page.svelte` redirect filled.
  //
  // Matches the root both with and without a trailing slash: the nest mounts the
  // SPA at `/app` (nest_service("/app", …)) and its info page links to bare
  // `/app`, so an entry can arrive as either `/app` or `/app/` (SvelteKit
  // normalises the former, but the guard does not depend on that).
  // `wasmSettled` is the second pass of the same guard, after `ensureWasm()` —
  // see the deferral below for why it exists rather than a bare re-read.
  function routeAppRoot(wasmSettled = false) {
    if (typeof window === 'undefined') return;
    // `: string` widens SvelteKit's generated `Pathname` union, which (per the
    // route tree) has no literal member equal to bare `/app` — every member
    // carries a `/`-prefixed suffix. That's a real type-vs-runtime gap here:
    // the raw initial-load `pathname` genuinely can be exactly `/app` before
    // any client-side routing has normalised it (see the comment above), so
    // the equality checks below are correct at runtime even though the
    // generated type disagrees.
    const path: string = $page.url.pathname;
    const inApp = path === '/app' || path.startsWith('/app/');
    if (!inApp) return;
    // The identity is read through the registry: a PINNED tab by ITS OWN
    // account's rows, an unpinned one by the active account's
    // (`account-scoping.md` § Concurrent instances → *Session identity resolves
    // through the session's account*). Judging a pinned secondary by the
    // *active* account would divert it to onboarding whenever the two differ —
    // and a secondary must never enter the wizard anyway, since the onboarding
    // scratchpad belongs to the primary.
    const pinnedAccount = tabPin();
    const material = accountsTabSessionMaterial();
    // ⚠ Unreadable is NOT unauthenticated. This guard runs in `onMount` and the
    // registry read needs wasm, whose init is async — so a tab can reach here
    // before the module is up, and answering "no credentials" then would bounce
    // a perfectly signed-in tab into the wizard. So a first pass that reads
    // nothing waits for wasm and re-runs; only the second pass's answer is
    // evidence.
    //
    // ⚠ And a bare `return` would PARK the tab, a client-unrecoverable state
    // (nest/common.md § Client-state recoverability): the bare index
    // deliberately runs no `identity.init()` of its own (`routes/+page.svelte`
    // is empty precisely because this guard owns the redirect), so a guard that
    // defers must own the resumption.
    if (material === undefined && !wasmSettled) {
      void ensureWasm()
        .then(() => routeAppRoot(true))
        .catch(() => { /* wasm itself failed; the page's own error path owns it */ });
      return;
    }
    // A pin that still resolves to nothing once wasm is up: the account is
    // genuinely gone, which only `init()`'s boot can clear (it drops the dead
    // pin and reloads). Call it exactly where nothing else will, the bare index.
    if (pinnedAccount && material === undefined) {
      if (path === '/app' || path === '/app/') identity.init();
      return;
    }
    const toOnboarding = () => {
      // Idempotence is load-bearing: a goto() fired during initial mount
      // recreates the tree and re-runs this guard, so an UNCONDITIONAL
      // same-URL goto here self-sustains — observed as ~47 navigations/s to
      // /app/onboarding from /app/onboarding, remounting the page forever
      // (effect_update_depth_exceeded, boot never settling, every web e2e
      // wedged; 2026-08-02). Already on onboarding ⇒ nothing to do.
      if (!$page.url.pathname.startsWith('/app/onboarding')) {
        goto('/app/onboarding', { replaceState: true });
      }
    };
    if (!material?.secret_hex || !material.nest_url) {
      toOnboarding();
      return;
    }
    // Credentials present: the pending-factory-reset slot decides between
    // onboarding and the default landing. An unreadable slot reads as absent —
    // the launch machine is the slot's authoritative reconciler either way.
    void loadPendingFactoryReset()
      .catch(() => null)
      .then((pendingFactoryReset) => {
        if (pendingFactoryReset) {
          toOnboarding();
          return;
        }
        const now: string = $page.url.pathname;
        if (now === '/app' || now === '/app/') {
          goto('/app/conversations', { replaceState: true });
        }
      });
  }

  onMount(() => {
    routeAppRoot();
  });

  // Nav labels come from the shared i18n strings (priority #1/#3 — "all apps
  // share the same strings"), mirroring linux's sidebar.rs::label() map. `t` is a
  // compile-time const object, so referencing it in this module-level array is fine.
  const tabs = [
    { path: '/app/conversations', label: t.conversations.list.title, icon: '💬', testid: 'conversations-tab' },
    { path: '/app/search', label: t.common.search, icon: '🔍', testid: 'search-tab' },
    { path: '/app/events', label: t.events.title, icon: '📅', testid: 'events-tab' },
    { path: '/app/contacts', label: t.common.contacts, icon: '👤', testid: 'contacts-tab' },
    { path: '/app/profile', label: t.profile.title, icon: '🪪', testid: 'profile-tab' },
    { path: '/app/feed', label: t.common.feed, icon: '📡', testid: 'feed-tab' },
    { path: '/app/media', label: t.media.title, icon: '📁', testid: 'media-tab' },
    { path: '/app/backups', label: t.backups.title, icon: '💾', testid: 'backups-tab' },
    // Devices is no longer a top-level nav item (sync-file-set-ui-unification,
    // 2026-06-28): the device roster + folder control plane moved into the
    // Settings shell (Settings → Devices / Settings → Folders). ui.yaml drops
    // `devices` from top-level `navigation.pages`. /app/devices redirects there.
    { path: '/app/nostr', label: t.nostr.title, icon: '\u{1F5DD}', testid: 'nostr-tab' },
    { path: '/app/bridges', label: t.common.bridges, icon: '\u{1F517}', testid: 'bridges-tab' },
    { path: '/app/notifications', label: t.common.notifications, icon: '\u{1F514}', testid: 'notifications-tab' },
    { path: '/app/settings', label: t.common.settings, icon: '⚙️', testid: 'settings-tab' },
  ];
</script>

<!-- The ward's `screen-time-lock` (family-safety.md § Screen time) — a
     conditionally-present global rendered in PLACE of the page while the ward is
     outside their usage window or over their daily budget. One snippet, rendered
     from both shell branches, because the contract is "every authenticated page"
     — the same reason the critical-alerts banner sits above the bypass.
     Whether to lock and what it says are ONE shared-Rust call, so no
     screen-time logic lives in JS. The hint is a real link to the Family page,
     which stays reachable read-only from EVERY locked surface (the goal-doc
     invariant) — including the admin/settings shells, which render no sidebar
     and no supervised-indicator. /app/family itself is exempt outright. -->
{#snippet screenTimeLock()}
  <div class="screen-time-lock" data-testid={IDS.SCREEN_TIME_LOCK}>
    <h2>{t.family.screen_lock_title}</h2>
    <p data-testid={IDS.SCREEN_TIME_LOCK_MESSAGE}>{resolveLocalized(screenLock)}</p>
    <p class="hint"><a href="/app/family">{t.family.screen_lock_family_hint}</a></p>
  </div>
{/snippet}

{#if isOnboarding}
  {@render children?.()}
{:else}
  <!-- Every authenticated page, including the admin/settings shells below that
       bypass the normal sidebar layout — the critical-alerts contract is
       "every authenticated page", not "every page with a sidebar"
       (critical-alerts.md § Mechanism → Rendering contract). -->
  <CriticalAlertsBanner />
  {#if isAdmin || isSettings}
    {#if screenLock && !onFamilyPage}
      {@render screenTimeLock()}
    {:else}
      {@render children?.()}
    {/if}
  {:else}
    {#snippet sidebarContent()}
      <div class="logo">Fauna</div>
      <div
        class="connection-status"
        class:connected={$connectionStatus === 'connected'}
        data-testid={IDS.CONNECTION_STATUS}
      >
        {connectionLabel}
      </div>
      {#each tabs as tab}
        <a
          href={tab.path}
          class="tab"
          class:active={$page.url.pathname.startsWith(tab.path)}
          data-testid={tab.testid}
        >
          <span class="icon">{tab.icon}</span>
          <span class="label">{tab.label}</span>
        </a>
      {/each}
      {#if hasFamilyRelationship}
        <a
          href="/app/family"
          class="tab"
          class:active={$page.url.pathname.startsWith('/app/family')}
          data-testid={IDS.FAMILY_TAB}
        >
          <span class="icon">👪</span>
          <span class="label">{t.family.title}</span>
        </a>
      {/if}
      {#if userIsAdmin}
        <a
          href="/app/admin"
          class="tab"
          class:active={$page.url.pathname.startsWith('/app/admin')}
          data-testid={IDS.ADMIN_TAB}
        >
          <span class="icon">🛡️</span>
          <span class="label">{t.common.admin}</span>
        </a>
      {/if}
    {/snippet}
    {#snippet mainContent()}
      <!-- Shell-level app-message surface (the web twin of native's content-top
           MessageBanner): shows transient cross-page warnings dispatched via the
           shared `fauna-message-update` window event — e.g. the post-onboarding
           "off-box recovery custody not saved" warning the admin-claim launch glue
           raises after navigating here (box-recovery.md § Mechanism). Renders no DOM until a message is set. -->
      <MessageBanner />
      <!-- The permanent, non-dismissable supervised indicator (family-safety.md
           § App surface — "on every page"): it lives in the shell, not per
           page, and navigates to the Family surface. Rendered only when this
           account is supervised; the text names the guardian (transparency
           invariant 4 — supervision is never silent). -->
      {#if supervisedHandle}
        <a href="/app/family" class="supervised-indicator" data-testid={IDS.SUPERVISED_INDICATOR}
          >{t.family.supervised_indicator({ guardian: supervisedHandle })}</a>
      {/if}
      {#if screenLock && !onFamilyPage}
        {@render screenTimeLock()}
      {:else}
        {@render children?.()}
      {/if}
    {/snippet}
    <AppShellFrame sidebar={sidebarContent} main={mainContent} />
  {/if}
{/if}

<style>
  .logo {
    font-size: 1.25rem;
    font-weight: 700;
    padding: 0 1rem 1rem;
    color: var(--accent);
  }
  .connection-status {
    font-size: 0.75rem;
    padding: 0 1rem 0.75rem;
    color: var(--text-muted);
  }
  .connection-status.connected {
    color: var(--text);
  }
  .tab {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    padding: 0.625rem 1rem;
    color: var(--text-muted);
    transition: background 0.15s;
  }
  .tab:hover {
    background: var(--bg-hover);
    color: var(--text);
  }
  .tab.active {
    color: var(--accent);
    background: var(--bg-hover);
  }
  .icon { font-size: 1.125rem; }
  /* The lock REPLACES the page content (see the {#if} in the shell), so it
     needs no overlay positioning — and deliberately must not cover the sidebar
     or the supervised-indicator, which are the ward's two routes to the Family
     page that § Screen time requires stay reachable while locked. */
  .screen-time-lock {
    display: flex;
    flex: 1;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 0.75rem;
    padding: 2rem;
    text-align: center;
  }
  .screen-time-lock .hint {
    color: var(--text-muted);
  }
  .supervised-indicator {
    display: block;
    padding: 0.5rem 0.75rem;
    margin-bottom: 0.75rem;
    border: 1px solid var(--accent, #3b82f6);
    border-radius: 6px;
    background: color-mix(in srgb, var(--accent, #3b82f6) 12%, transparent);
    color: var(--accent, #3b82f6);
    font-size: 0.875rem;
  }

  @media (max-width: 768px) {
    .logo { display: none; }
    .tab { flex-direction: column; gap: 0.25rem; padding: 0.5rem; font-size: 0.75rem; }
  }
</style>
