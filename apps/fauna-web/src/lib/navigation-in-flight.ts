// How many SvelteKit client navigations this tab has in flight — counted from
// the link click the router claimed (`oweNavigation`) or, for a `goto`, from
// `beforeNavigate`, to the navigation's own `complete` promise settling (either
// way: arrived, cancelled or failed).
//
// Its one reader is the e2e `barrier` (`#lib/barrier-e2e`, *The two completion
// observables*); its writers are the root layout's `beforeNavigate` and its
// window click listener, both registered only in test builds
// (`+layout.svelte`'s `__FAUNA_E2E_AUTOMATION__` branch, which a production
// build constant-folds away). Kept outside the test-only barrier chunk because
// `beforeNavigate` must be called while the layout initialises, before that
// chunk has loaded.
//
// ## Why a claimed click is owed a navigation
//
// The router does NOT begin a link's navigation inside the click's dispatch:
// it claims the click there (`preventDefault`, so the browser follows nothing)
// and reaches `beforeNavigate` a task or more later — about 10 ms on an idle
// tab, measured 2026-11-20 with a capture-phase probe on the web actuation
// sweep, which caught a barrier that read the count at zero in that gap and
// acked on the page the click was leaving. So a claimed click raises an owed
// navigation at once, and the next `beforeNavigate` turns it into the counted
// one.

let inFlight = 0;
let owed = 0;
let waiters: Array<() => void> = [];

/** A link click the router claimed: a navigation is owed from now until the
 *  next `beforeNavigate` begins it. */
export function oweNavigation(): void {
  owed = 1;
}

/** Count a navigation from now until `complete` settles; it begins whatever
 *  navigation a claimed click owed. */
export function trackNavigation(complete: Promise<unknown>): void {
  owed = 0;
  inFlight += 1;
  const settle = () => {
    inFlight -= 1;
    const ready = waiters;
    waiters = [];
    for (const w of ready) w();
  };
  complete.then(settle, settle);
}

/** The navigations in flight right now, an owed one included. */
export function navigationsInFlight(): number {
  return inFlight + owed;
}

/** Whether the count is up only for a claimed click whose navigation has not
 *  begun — for the barrier's message when it never does. */
export function navigationOwed(): boolean {
  return owed > 0;
}

/** Settles when the next counted navigation settles — the barrier probe's
 *  `click` leg publishes from it. */
export function nextNavigationSettled(): Promise<void> {
  return new Promise((resolve) => waiters.push(resolve));
}
