// Which launch is the current one — the web twin of tui's `launch::deliver` /
// `launch::retire_in_flight` and linux's `launch_currency.rs`
// (`onboarding.md` § App-launch routing → *A verdict renders only while its own
// launch is still the current one*; multiple-accounts outcome 17).
//
// A launch verdict is decided from a store read taken when the launch started
// and rendered a round trip later. Every asynchronous launch drive on the
// onboarding page (the cold start, Retry, Trust) `begin`s its machine here
// before it awaits, and asks `deliver` before rendering anything. A newer
// launch (a remount of the page) replaces the held machine; the e2e session
// patch, which builds a session without a launch and without a reload, clears
// it (`retireInFlight`). A verdict from a machine that is no longer held is
// dropped whole.
//
// It is an OWNERSHIP test, never an "is a session present?" one: the held
// launch's verdict still routes in full over a live session — the
// `IdentityChanged` teardown depends on it.
//
// The held machine is the IN-FLIGHT one only: `deliver` takes it when the
// verdict routes, so a settled launch is never retired.
//
// The counters are `fauna_e2e_agent::LAUNCH_VERDICTS_KEY`'s value. In memory is
// right: a reload is a fresh app process under the key's contract, and a fresh
// process starts both at 0. Generic over the machine (`object`) so this module
// imports no wasm and runs under `just web-unit-test`.

let inFlight: object | null = null;
let routed = 0;
let superseded = 0;

/** `machine` is now the launch in flight: any earlier one still waiting is
 *  superseded, and its verdict will be dropped when it lands. */
export function begin(machine: object): void {
  inFlight = machine;
}

/** Is `machine` still the launch in flight? A read, no count — for the
 *  launch watchdog, which must not paint over a newer session either. */
export function holds(machine: object): boolean {
  return inFlight === machine;
}

/** May `machine`'s delivered verdict be rendered? `true` only while it is
 *  still the launch in flight — which it then stops being (the verdict settles
 *  it). Counts the verdict either way, before anything is rendered or dropped. */
export function deliver(machine: object): boolean {
  const current = inFlight === machine;
  if (current) {
    inFlight = null;
    routed += 1;
  } else {
    superseded += 1;
    console.info('[launch] verdict superseded before it rendered — dropping it');
  }
  return current;
}

/** Retire a launch still in flight because a newer session has just been built
 *  without it (the e2e session patch). A settled launch holds nothing here, so
 *  this is a no-op for it. */
export function retireInFlight(): void {
  if (inFlight !== null) {
    inFlight = null;
    console.info('[launch] a newer session retired the launch still in flight');
  }
}

/** `fauna_e2e_agent::LAUNCH_VERDICTS_KEY`'s value. */
export function launchVerdicts(): { routed: number; superseded: number } {
  return { routed, superseded };
}
