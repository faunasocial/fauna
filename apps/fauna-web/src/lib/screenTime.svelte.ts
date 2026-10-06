// The ward's screen-time lock state — the web twin of linux `screen_lock.rs`
// (`family-safety.md` § Screen time, Slice E). Holds the render STATE the lock
// decision consumes (the ward's own policy + guardian handle, and later the
// day's cross-device usage total) and exposes the one `currentLockMessage()`
// the layout calls, so the gate and its wording can never drift apart.
//
// Client-enforced by construction: the nest cannot see when a device is in use
// and deliberately does not gate on it (§ Don't do these — *"Don't put
// screen-time or content enforcement on the nest"*), so this surface IS the
// enforcement. No policy logic lives here: whether to lock and what the lock
// says are one call into shared Rust via the `screenLockMessage` wasm export.
//
// A `.svelte.ts` module so the cache is `$state`: the layout re-renders when a
// late-arriving `hydrateScreenTime` populates the policy, the reactive
// equivalent of linux repainting after its post-auth read lands.

import {
  newUsageHeartbeat,
  screenLockMessage,
  type ScreenTimePolicyValue,
  type UsageHeartbeatHandle,
} from '$lib/wasm';
import { familyUsageReport } from '$lib/rpc';
import { registerActorScopedReset } from '$lib/actorScope';
import { utcOffsetMinutes } from '$lib/utcOffset';
import type { LocalizedText } from '$lib/i18n/localized';

// The supervised viewer's OWN screen-time policy (`fauna.family.status`).
// `null` for an unsupervised account — nothing to enforce.
let wardScreenTime = $state<ScreenTimePolicyValue | null>(null);
// Who supervises this account. `null` = not supervised = never locked, whatever
// a stale policy says; it is also what the lock message names.
let guardianHandle = $state<string | null>(null);
// Bumped whenever a heartbeat reply changes the day's total, purely to
// invalidate the layout's `$derived` lock verdict — the heartbeat itself lives
// in wasm, which Svelte cannot observe.
//
// The monotonic source is a PLAIN counter and the `$state` is WRITE-ONLY here:
// every bump must publish a new value without ever *reading* the old one. The
// layout's screen-time `$effect` calls `setWardScreenTime` synchronously, and
// Svelte 5 tracks reads dynamically (through called functions, not just the
// effect body) — so a read-modify-write (`usageEpoch += 1`) registered
// `usageEpoch` as that effect's OWN dependency, and the write then re-dirtied
// the effect that had just run it. That self-invalidating loop is the
// `effect_update_depth_exceeded` pair that fired on every single boot until
// 2026-08-02 (twice: once per mount of the initial double-mount). Bump only via
// `bumpUsageEpoch()`; never read `usageEpoch` outside a derived/render context.
let usageEpochSeq = 0;
let usageEpoch = $state(0);

function bumpUsageEpoch(): void {
  usageEpoch = ++usageEpochSeq;
}

// The ward's usage heartbeat — the budget half of § Screen time. Lazily
// constructed so an unsupervised session never instantiates it.
let heartbeat: UsageHeartbeatHandle | null = null;
function usage(): UsageHeartbeatHandle {
  heartbeat ??= newUsageHeartbeat();
  return heartbeat;
}

// Test-only clock skew in seconds (testing.md § convention 14's fake clock),
// driven by the `screen_time_heartbeat` command in `$lib/screen-time-e2e` — the
// browser twin of linux's `advance_test_clock`. Screen time is the one pillar
// whose behavior really is a function of elapsed time, and a test that SLEPT for
// a heartbeat would be defunct under § point 14, not merely slow. It stays 0 in
// every production bundle, because the only module that writes it is
// tree-shaken out of builds without `__FAUNA_E2E_AUTOMATION__` (convention 15).
let testClockSkewSecs = 0;

/** Epoch seconds as this module reckons them — the real clock plus any test
 *  skew. Plain number, never `bigint` (the wasm `i64` trap). */
function nowSecs(): number {
  return Math.floor(Date.now() / 1000) + testClockSkewSecs;
}

/** Advance the heartbeat clock by `secs`, in the accrual steps a real caller
 *  ticks in — the engine caps a single gap, so one big jump would credit only
 *  one step. Test-only; see `testClockSkewSecs`. */
export function advanceTestClock(secs: number): void {
  const step = 120; // fauna_core::screen_time::MAX_ACCRUAL_STEP_SECS
  // Prime the engine's reference point first: it accrues from the GAP between
  // calls, so with no prior call the first step would credit nothing and the
  // poke would silently deliver less use than asked for.
  usage().setActive(true, nowSecs());
  let remaining = Math.max(0, Math.floor(secs));
  while (remaining > 0) {
    const bump = Math.min(remaining, step);
    testClockSkewSecs += bump;
    remaining -= bump;
    usage().setActive(true, nowSecs());
  }
}

/** Cache the supervised viewer's screen-time policy, guardian, and the day's
 *  cross-device usage total. An absent guardian clears the lock — a graduated
 *  ward, or an unsupervised account.
 *
 *  `usageTodayMinutes` comes off the same `fauna.family.status` read and seeds
 *  the heartbeat, so the very first paint can already evaluate the budget
 *  rather than leaving an over-budget ward unlocked until a round-trip lands. */
export function setWardScreenTime(
  policy: ScreenTimePolicyValue | null | undefined,
  guardian: string | null | undefined,
  usageTodayMinutes?: number | null,
): void {
  wardScreenTime = policy ?? null;
  guardianHandle = guardian ?? null;
  // Touch the heartbeat ONLY when there is something to store in it (or one
  // already exists to clear). This laziness is load-bearing, not an
  // optimization: the layout's $effect runs this on FIRST render — before
  // ensureWasm() has resolved — where famStatus is still undefined, and
  // newUsageHeartbeat() on the uninitialized core chunk throws, killing the
  // layout mid-boot and remounting the page in an infinite loop (~48/s) that
  // wedged every web e2e session (2026-08-02). famStatus non-null implies the
  // wasm is up (it came from an RPC), so the guarded path never fires early.
  if (policy != null || usageTodayMinutes != null || heartbeat !== null) {
    const h = usage();
    h.setPolicy(policy ?? null);
    h.seedTotal(usageTodayMinutes ?? undefined);
  }
  bumpUsageEpoch();
}

/** The current lock verdict as the `LocalizedText` to display, or `null` for
 *  "not locked". The single decision point: the overlay's visibility and its
 *  text both come from here. */
export function currentLockMessage(): LocalizedText | null {
  void usageEpoch;
  if (!guardianHandle) return null;
  return screenLockMessage(
    wardScreenTime,
    localMinutesFromMidnight(),
    usedTodayMinutes(),
    guardianHandle,
  );
}

/** The day's usage as the lock should see it: the nest's cross-device total
 *  PLUS what this tab has accrued since its last report, so the budget lock
 *  fires the minute it is reached instead of waiting out a heartbeat interval.
 *  `undefined` until a total has been heard — the ratified fail-open on the
 *  budget arm. */
export function usedTodayMinutes(): number | undefined {
  void usageEpoch;
  return usage().usedTodayMinutes(nowSecs());
}

/** Drive the heartbeat one step and, if the shared engine says a report is due,
 *  send it (`family-safety.md` § Screen time — daily-budget enforcement needs
 *  cross-device accounting).
 *
 *  Called from the layout's one-minute tick, which is what the engine's accrual
 *  step requires of a caller. `active` is *the tab is visible AND the lock is
 *  not showing*: time spent staring at the lock screen is not screen time, and
 *  crediting it would inflate the guardian's readout with minutes the child
 *  never spent. A zero-minute report is still sent on cadence — the goal doc
 *  defines it as a read, and it is what lifts the lock at local midnight or
 *  after a guardian raises the budget.
 *
 *  Best-effort: a failed report re-credits its minutes (the delta is defined
 *  against the last *successful* report) and retries on the next tick. */
export async function tickUsageHeartbeat(secretHex: string): Promise<void> {
  const h = usage();
  if (!h.isAccounting()) return;
  const active = typeof document !== 'undefined' && document.visibilityState === 'visible';
  h.setActive(active && currentLockMessage() === null, nowSecs());
  const minutes = h.takeDue(nowSecs());
  if (minutes === undefined) return;
  try {
    const reply = await familyUsageReport(secretHex, minutes, utcOffsetMinutes());
    h.reportSucceeded(reply.day, reply.day_total_minutes, nowSecs());
  } catch {
    // The nest never answered — those minutes are still owed.
    h.reportFailed();
  }
  bumpUsageEpoch();
}

/** The device's local clock as minutes from local midnight — the unit the
 *  policy stores its window bounds in. The window half is pure local clock, so
 *  this needs no wire traffic at all. */
export function localMinutesFromMidnight(): number {
  const now = new Date();
  return now.getHours() * 60 + now.getMinutes();
}

/** Drop the ward's screen-time state on an identity change — sign-out, account
 *  switch, reset. Without it a new account inherits the previous ward's lock,
 *  naming a guardian the user does not have. */
export function resetScreenTime(): void {
  wardScreenTime = null;
  guardianHandle = null;
  // `heartbeat?.reset()`, never `usage().reset()`: `usage()` CONSTRUCTS the
  // handle through wasm (`newUsageHeartbeat`), so calling it here made this drop
  // throw `WASM not initialized — call ensureWasm() first` on every switch that
  // lands before the module is up. The e2e agent's `set_state` patch is exactly
  // that door — it bypasses `identity.login()`, which is what would have run
  // `ensureWasm()` — so EVERY web test login logged
  // `[actor-scope] reset failed: WASM not initialized` and, worse, skipped the
  // two lines below it: `bumpUsageEpoch()` never ran, so the drop was PARTIAL
  // (`actorScope.ts` guards each reset, so the throw cost the rest of this
  // function, not the rest of the switch). A heartbeat that was never built
  // holds no state to reset, which is why the lazy read is also the correct one.
  heartbeat?.reset();
  bumpUsageEpoch();
}

registerActorScopedReset(resetScreenTime);
