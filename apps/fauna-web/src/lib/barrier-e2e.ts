// The web app's `barrier` test-agent command — convention 14's causal anchor
// for negative asserts (`docs/goal/architecture/e2e-conventions.md`), plus the
// self-test probe that proves it.
//
// This module exists ONLY in builds made for testing (convention 15): its sole
// importer is `#lib/e2e-automation`, itself reached only through
// `+layout.svelte`'s `if (__FAUNA_E2E_AUTOMATION__)` branch, which a production
// `vite build` constant-folds away.
//
// ## Why a macrotask turn, and why two
//
// The driver's `call_command` (`tests/e2e-unified/drivers/web.py`) awaits the
// promise `__fauna_callCommand` returns, so a handler that merely `await`s
// resolves after the current **microtask** drain. That is not the contract: the
// barrier must also order against work already queued as a **macrotask** — a
// `setTimeout(fn, 0)`, a message-channel task, the rendering turn a Svelte flush
// rides. A single `setTimeout(…, 0)` turn is dispatched after macrotasks queued
// before it (the HTML spec's task queues are FIFO per source), and the second
// turn covers the common case of a task that itself queues one more — a
// promise-then chain hopping to a new task, or a component that schedules its
// effect from inside a first-turn callback.
//
// Two is a deliberate, stated bound rather than "enough in practice": a barrier
// cannot promise to outrun *unbounded* re-queueing, and pretending otherwise
// would be the settle-sleep this convention replaces, only spelled differently.
// Work that re-queues without bound needs a completion observable of its own
// (convention 14's corollary: an effect must be initiated synchronously inside a
// handler whose completion is observable), not a longer barrier.
//
// ## The two completion observables: a navigation, an RPC
//
// Two kinds of gesture outlive any fixed number of turns, and the web
// actuation sweep met both (2026-11-19): a nav tab — a SvelteKit link, whose
// client navigation fetches the route's lazily loaded chunk before it renders —
// and a button whose handler awaits an RPC (`nostr-link-button`). On a loaded
// box either takes longer than two turns, so a barrier of turns alone let the
// sweep read the old page. So before its turns the barrier waits, polling once
// per macrotask turn, for:
//
//   1. **no navigation in flight** — `#lib/navigation-in-flight`'s count, kept
//      from SvelteKit's `beforeNavigate` to the navigation's own `complete`
//      promise, reading zero. A link click is counted from the click itself:
//      the router claims it inside the dispatch but reaches `beforeNavigate`
//      a task or more later (about 10 ms idle, measured 2026-11-20 — the gap
//      in which the actuation sweep's barrier acked on the old page), so the
//      root layout raises an owed navigation for every click the router
//      claimed, which the next `beforeNavigate` turns into the counted one
//      (that module's *Why a claimed click is owed a navigation*). (The `$app/state` `navigating` object read from
//      this module did not see a probe's `goto` in flight — measured
//      2026-11-19 — so the leg counts the navigation's own promise instead.)
//      A navigation still in flight at
//      `NAV_SETTLE_BOUND_MS` is a hung navigation, and the barrier THROWS
//      (convention 11: never ack early in silence);
//   2. **no WS-RPC call in flight** — `#lib/rpc-in-flight`'s count, kept from
//      each call's entry, reading zero. It counts the conversations manager's
//      gesture-awaited work too — its build and every promise a method of the
//      manager `getConversationsManager` hands a page returns — because that
//      work reaches the nest through wasm, never through `call`
//      (`nostr-link-button`'s settle race, 2026-11-20). A session-long loop seam (the
//      critical-alert sweep; the conversations receive poll) is not counted
//      (`#lib/rpc`'s `callLoop`, `#lib/conversations`'s
//      `buildConversationsManager`): it never settles. At `RPC_SETTLE_BOUND_MS` the barrier
//      goes on regardless: the one call that legitimately lives longer is the
//      connect wait of a socket that is down (`#lib/rpc`'s `ensureConnected`,
//      15 s), which tests that stop a nest meet with timers still firing, and
//      a throw there would red every such test for nothing it asserts.
//
// Both are read first, then the turns, so the paint that follows a settled
// navigation or call is ordered by the turns exactly as before. On an idle tab
// both read idle at once and the barrier costs what it always did.

import { goto } from '$app/navigation';
import { getConversationsManager } from '#lib/conversations';
import { registerE2eCommands } from '#lib/e2e-commands';
import { navigationOwed, navigationsInFlight, nextNavigationSettled } from '#lib/navigation-in-flight';
import { oldestRpcInFlight, rpcInFlight, traceRpcInFlight, trackRpc } from '#lib/rpc-in-flight';

// The cross-app action names. Kept in sync with `fauna_e2e_agent::BARRIER` /
// `::BARRIER_PROBE`, which is where the contract is documented for all 7 apps —
// web reaches the registry as plain strings, exactly as the other command
// domains here do (`backup_audit_run_now`, `family_notify_check_now`).
const BARRIER = 'barrier';
const BARRIER_PROBE = 'barrier_probe';
/** Mirrors `fauna_e2e_agent::BARRIER_PROBE_FUSE_FIELD` — the fused-probe flag. */
const BARRIER_PROBE_FUSE_FIELD = 'barrier';
/** Mirrors `fauna_e2e_agent::BARRIER_PROBE_DEFAULT_COUNT`. */
const BARRIER_PROBE_DEFAULT_COUNT = 64;
/** Mirrors `fauna_e2e_agent::barrier_probe_value`. */
function barrierProbeValue(token: string, i: number): string {
  return `${token}#${i}`;
}

/** Web's own probe fields (not part of the cross-app probe contract — the
 *  other apps' barriers have no navigation or RPC leg to grade):
 *  `rpc_hold_ms` counts a fake call in flight for that long, which publishes
 *  `"<token>#rpc"` as it settles; `navigate` starts a client navigation to that
 *  path, which publishes `"<token>@<path>"` once it has completed. Fused, only
 *  a barrier that waited for the leg can freeze either value.
 *  `manager_hold_ms` holds a call of that length on the conversations manager
 *  `getConversationsManager` hands a page — through the very face a handler
 *  calls (`#lib/rpc-in-flight`'s `countedFace`) — and publishes
 *  `"<token>#manager"` as it settles. */
const BARRIER_PROBE_RPC_HOLD_FIELD = 'rpc_hold_ms';
const BARRIER_PROBE_MANAGER_HOLD_FIELD = 'manager_hold_ms';
/** The probe's own method, set on the manager for one call and removed. */
const MANAGER_HOLD_METHOD = '__faunaBarrierHoldForTest';
const BARRIER_PROBE_NAVIGATE_FIELD = 'navigate';
/** `click` names a link by its `data-testid` (a nav tab) and clicks it — the
 *  same dispatch a person's click makes, which the router claims and begins a
 *  task later — publishing `"<token>@<path>"` once the navigation it started
 *  has settled. Fused, only a barrier that counts a claimed click as owed a
 *  navigation can freeze that value (`#lib/navigation-in-flight`, *Why a
 *  claimed click is owed a navigation*). */
const BARRIER_PROBE_CLICK_FIELD = 'click';

/** See this module's header, *The two completion observables*. */
const NAV_SETTLE_BOUND_MS = 15_000;
const RPC_SETTLE_BOUND_MS = 5_000;

/** One macrotask turn. */
function macrotask(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

function navigationInFlight(): boolean {
  return navigationsInFlight() > 0;
}

/** Turn by turn until no navigation and no RPC is in flight — see the header. */
async function settleNavigationAndRpc(): Promise<void> {
  const navDeadline = Date.now() + NAV_SETTLE_BOUND_MS;
  while (navigationInFlight()) {
    if (Date.now() >= navDeadline) {
      const msg = navigationOwed()
        ? `${BARRIER}: a link click the router claimed began no navigation within ` +
          `${NAV_SETTLE_BOUND_MS} ms (now at ${location.pathname}) — a claimed click ` +
          'that never navigates, not a settled frame'
        : `${BARRIER}: ${navigationsInFlight()} navigation(s) still in flight after ` +
          `${NAV_SETTLE_BOUND_MS} ms (now at ${location.pathname}) — a hung navigation, ` +
          'not a settled frame';
      console.warn(`[barrier] ${msg}`);
      throw new Error(msg);
    }
    await macrotask();
  }
  const rpcDeadline = Date.now() + RPC_SETTLE_BOUND_MS;
  while (rpcInFlight() > 0 || navigationInFlight()) {
    if (Date.now() >= rpcDeadline) {
      // Going on regardless (the header says why) — but never in silence.
      console.warn(
        `[barrier] ${rpcInFlight()} RPC call(s) still in flight after ${RPC_SETTLE_BOUND_MS} ms; ` +
          `going on. Oldest: ${oldestRpcInFlight().join(' || ') || '(untraced)'}`,
      );
      return;
    }
    await macrotask();
  }
}

/** The last applied probe item's value — live, and therefore NOT assertable. */
let probeToken: string | null = null;

/**
 * What `probeToken` held when the last `barrier` resolved, frozen.
 *
 * ⚠ This is the only value the self-test may assert. The driver reads state over
 * a *separate* round trip after the command resolves, and the browser runs every
 * queued macrotask in that window — so asserting the live token above passes
 * against a `barrier` that awaits nothing at all. See
 * `fauna_e2e_agent::BARRIER_ACK_PROBE_KEY` for the measured proof of that.
 */
let ackProbe: string | null = null;

/** `state.barrier_probe`, read by the state projection. */
export function barrierProbeToken(): string | null {
  return probeToken;
}

/** `state.barrier_ack_probe` — the frozen ack-time observable. */
export function barrierAckProbe(): string | null {
  return ackProbe;
}

/** The one-test lifetime clear point, called from the agent's reset path. */
export function clearBarrierProbe(): void {
  probeToken = null;
  ackProbe = null;
}

/**
 * The barrier's whole mechanism — the two macrotask turns plus the ack-time
 * freeze — in ONE place, so the bare `barrier` command and the fused
 * `barrier_probe` cannot drift apart. That sharing is what makes the mutant
 * below meaningful: a mutant applied here is applied to both.
 *
 * ⚠ **Mutation-grading status: PINNED.** Deleting both `await macrotask()`
 * calls reds `test_agent_barrier.py::test_fused_barrier_probe_orders_the_batch
 * --app web` — measured 2026-08-13, the M5 mutant that previously survived.
 * What changed is the *test*, not this mechanism: fused, the probe's batch and
 * this barrier run inside one CDP round trip, so the gap in which the browser
 * used to run every queued macrotask regardless — making a do-nothing barrier
 * indistinguishable from this one — no longer exists. The older two-command
 * test remains and still cannot discriminate these turns; it is kept as the
 * usage-shape smoke, and says so.
 */
async function runBarrier(): Promise<void> {
  // A first turn lets a handler that `await`s before it calls `goto` or an RPC
  // reach that call, so the settle below sees it begun.
  await macrotask();
  await settleNavigationAndRpc();
  await macrotask();
  await macrotask();
  // Freeze what the barrier saw, synchronously in the same job that resolves
  // it — before the driver's next round trip can let further macrotasks run.
  ackProbe = probeToken;
}

export function registerBarrierCommands(): void {
  traceRpcInFlight();
  registerE2eCommands([BARRIER, BARRIER_PROBE], async (action, payload) => {
    if (action === BARRIER) {
      await runBarrier();
      return null;
    }
    // BARRIER_PROBE: queue the work as a macrotask and resolve WITHOUT waiting
    // for it. The early ack is the whole self-test — only a correct barrier can
    // make the token observable afterwards.
    const token = typeof payload?.token === 'string' ? payload.token : '';
    if (!token) {
      // Convention 11: a token-less probe would ack green and prove nothing.
      throw new Error(`${BARRIER_PROBE}: payload needs a non-empty \`token\``);
    }
    // A batch, not one item — see `fauna_e2e_agent::BARRIER_PROBE` for why the
    // one-item probe is nearly vacuous on an app whose queue drains by itself.
    const count =
      typeof payload?.count === 'number' ? payload.count : BARRIER_PROBE_DEFAULT_COUNT;
    for (let i = 0; i < count; i++) {
      const value = barrierProbeValue(token, i);
      setTimeout(() => {
        probeToken = value;
      }, 0);
    }
    // Web's navigation and RPC legs (`BARRIER_PROBE_RPC_HOLD_FIELD` says what
    // each publishes). Started, never awaited: the ack stays early.
    const holdMs = payload?.[BARRIER_PROBE_RPC_HOLD_FIELD];
    if (typeof holdMs === 'number' && holdMs > 0) {
      void trackRpc(
        () =>
          new Promise<void>((resolve) =>
            setTimeout(() => {
              probeToken = `${token}#rpc`;
              resolve();
            }, holdMs),
          ),
      );
    }
    const managerHoldMs = payload?.[BARRIER_PROBE_MANAGER_HOLD_FIELD];
    if (typeof managerHoldMs === 'number' && managerHoldMs > 0) {
      // A gesture's shape: await the manager, then await one of its methods.
      // The method is the probe's own, put on the manager object for this call
      // and called through the face the page gets, so only a face that counts
      // its methods' promises makes the barrier wait for `#manager`.
      void getConversationsManager().then(
        (m) => {
          const face = m as unknown as Record<string, (ms: number) => Promise<void>>;
          face[MANAGER_HOLD_METHOD] = (ms: number) =>
            new Promise<void>((resolve) => setTimeout(resolve, ms));
          const held = face[MANAGER_HOLD_METHOD](managerHoldMs);
          delete face[MANAGER_HOLD_METHOD];
          return held.then(() => {
            probeToken = `${token}#manager`;
          });
        },
        (e: unknown) => {
          probeToken = `${token}!manager failed: ${e instanceof Error ? e.message : String(e)}`;
        },
      );
    }
    const navTo = payload?.[BARRIER_PROBE_NAVIGATE_FIELD];
    if (typeof navTo === 'string' && navTo) {
      // A refused navigation publishes why, so the test's message says it
      // rather than reading as a barrier that acked early.
      void goto(navTo).then(
        () => {
          probeToken = `${token}@${location.pathname}`;
        },
        (e: unknown) => {
          probeToken = `${token}!goto failed: ${e instanceof Error ? e.message : String(e)}`;
        },
      );
    }
    const clickId = payload?.[BARRIER_PROBE_CLICK_FIELD];
    if (typeof clickId === 'string' && clickId) {
      const link = document.querySelector<HTMLElement>(`[data-testid="${CSS.escape(clickId)}"]`);
      if (!link) {
        throw new Error(`${BARRIER_PROBE}: no element with data-testid ${clickId} to click`);
      }
      void nextNavigationSettled().then(() => {
        probeToken = `${token}@${location.pathname}`;
      });
      link.click();
    }
    // Fused form (`fauna_e2e_agent::BARRIER_PROBE_FUSE_FIELD`): barrier before
    // acking, inside this same command. The batch above was queued on the timer
    // task source, and `macrotask()` queues on it too — the HTML spec dispatches
    // one task source FIFO, so the first turn alone already lands after all
    // `count` items; the second is the stated bound for a task that queues one
    // more (see this module's header).
    if (payload?.[BARRIER_PROBE_FUSE_FIELD] === true) {
      await runBarrier();
    }
    return null;
  });
}
