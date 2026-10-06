// The web app's **session generation** — convention 14's negative-assert
// observable (`docs/goal/architecture/e2e-conventions.md` § convention 14,
// mechanism (b); the cross-app contract lives at
// `fauna_e2e_agent::SESSION_GENERATION_KEY`).
//
// A monotonic count of the authenticated-session teardowns this seat has
// *initiated*. A test proving "this gesture did NOT relaunch me" reads it,
// performs the gesture, positively awaits the handler's own completion
// observable, issues `barrier`, and asserts the value unchanged.
//
// ## Why this module persists, when tui's and linux's counters do not
//
// On tui and linux a teardown keeps the process alive, so an in-memory counter
// is enough. **Web's teardown is a full document navigation**
// (`performSwitch`'s `window.location.assign`), which destroys the JS heap —
// so an in-memory counter would read 0 both when nothing happened AND after
// exactly the relaunch this key exists to detect. The assert is `unchanged`, so
// that collapse is not a missing signal but a **false PASS**: the mutant that
// relaunches survives, and the test reads green while proving nothing. That is
// the same vacuity class as `BARRIER_ACK_PROBE_KEY`'s, arriving by a different
// door, and `sessionStorage` is what closes it — the value survives the
// navigation, so a relaunched seat reports N+1 and the assert fails as it must.
//
// `sessionStorage` and not `localStorage` on purpose: per-tab, so two seats
// driven from one browser profile cannot see each other's teardowns, and it
// dies with the tab rather than leaking into the next run.
//
// This module exists ONLY in builds made for testing (convention 15): its
// importers are `$lib/e2e-automation` (the state projection) and the settings
// account switcher, which calls the bump behind `__FAUNA_E2E_AUTOMATION__` so a
// production `vite build` constant-folds it away.

/** The `sessionStorage` slot. Namespaced like the SPA's other stored keys. */
const STORAGE_KEY = 'fauna_e2e_session_generation';

/**
 * The current generation — `state.session_generation`.
 *
 * Reads through to storage on every call rather than caching in a module
 * variable: a cache would be re-initialised to 0 by the very navigation this
 * counter exists to survive, which is the whole failure this module prevents.
 */
export function sessionGeneration(): number {
  try {
    const raw = sessionStorage.getItem(STORAGE_KEY);
    const n = raw === null ? 0 : Number.parseInt(raw, 10);
    return Number.isFinite(n) && n >= 0 ? n : 0;
  } catch {
    // A storage-denied context (some privacy modes) must not break the app.
    // Returning 0 makes the counter useless rather than wrong: the assert then
    // reads 0 -> 0 and passes vacuously, which is why the e2e run asserts the
    // counter MOVES on the approve arm — a leg whose storage is denied fails
    // that positive control instead of silently passing the negative one.
    return 0;
  }
}

/**
 * Count one initiated teardown.
 *
 * ⚠ **Call this synchronously at the initiation point — immediately before the
 * navigation, in the same handler** — never after an `await` that the
 * navigation might beat, and never from a `beforeunload` listener (which does
 * not fire reliably and would tie the count to the browser rather than to the
 * app's own decision to tear down).
 */
export function recordSessionTeardown(): void {
  try {
    sessionStorage.setItem(STORAGE_KEY, String(sessionGeneration() + 1));
  } catch {
    // See `sessionGeneration` — degrade to a useless counter, never a throw
    // inside a product path.
  }
}

/**
 * The one-test lifetime clear point, called from the agent's reset path.
 *
 * Unlike the barrier probes this is a *reset*, not a clear-to-null: the counter
 * has no "unset" state, and a test reads a delta across its own gesture anyway.
 */
export function clearSessionGeneration(): void {
  try {
    sessionStorage.removeItem(STORAGE_KEY);
  } catch {
    // Nothing to do — see above.
  }
}
