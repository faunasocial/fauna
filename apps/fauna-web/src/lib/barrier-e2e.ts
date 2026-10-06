// The web app's `barrier` test-agent command — convention 14's causal anchor
// for negative asserts (`docs/goal/architecture/e2e-conventions.md`), plus the
// self-test probe that proves it.
//
// This module exists ONLY in builds made for testing (convention 15): its sole
// importer is `$lib/e2e-automation`, itself reached only through
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

import { registerE2eCommands } from '$lib/e2e-commands';

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

/** One macrotask turn. */
function macrotask(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 0));
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
  await macrotask();
  await macrotask();
  // Freeze what the barrier saw, synchronously in the same job that resolves
  // it — before the driver's next round trip can let further macrotasks run.
  ackProbe = probeToken;
}

export function registerBarrierCommands(): void {
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
