// The onboarding `LoggedIn` terminal's tail, as one sequence the page hands its
// steps to (`routes/onboarding/+page.svelte::handleWizardExit`).

export type LoggedInTerminalStep = 'register' | 'restored predecessors' | 'begin session' | 'activate';

export interface LoggedInTerminalSteps {
  /** The terminal's registry write — `persist_logged_in`, or append mode's
   *  `accountsAdd`. Awaited: the feed's `identity.init()` reads the registry
   *  back. */
  register: () => Promise<void>;
  /** The predecessor seeds a phrase-only restore recovered, linked to the
   *  restored identity by name; resolves at once when there are none. */
  persistRestoredPredecessors: () => Promise<void>;
  /** Start the signed-in session over the account that now exists: retire
   *  the WS-RPC client built before it (a pre-account dial can have stopped it
   *  for good on `not_registered`, and the claim changes neither half of its
   *  key) and deliver the signed-in actor-change handlers the missing home nest
   *  held back. After the registry write, which records that nest; before the
   *  switch, whose own actor change must not find its fresh client retired
   *  under it; before the hand-offs, which must dial through a live client. */
  beginSession: () => void;
  /** Append mode's switch to the added identity; absent on a first sign-in,
   *  whose identity is already active. Skipped when `register` failed — there
   *  is no added identity to switch to. */
  activate?: () => Promise<void>;
  /** The fire-and-forget post-`LoggedIn` hand-offs (kit, DNS credential,
   *  deployment seed, trust set, serving enablement) — the work the wizard
   *  deferred to the authenticated session. Never run by the terminal itself:
   *  handed to `launch`, which runs it once, at `Online`. */
  handoffs: () => void;
  /** The step from `LoggedIn` to the app, which IS a launch (`onboarding.md`
   *  § Wizard exit handling → *The finished wizard holds until the session is
   *  signed in*): the same `LaunchMachine::start` and verdict routing as a cold
   *  load, `launch_progress` while it runs, `launch_retry` when it cannot
   *  connect, and the app entered only on its `Online` verdict. `atOnline` is
   *  the deferred work: the launch keeps it through every Retry and calls it
   *  at `Online` — the terminal guards it to run once. */
  launch: (atOnline: () => void) => void;
  /** Log a failed step. A failed step stops nothing that follows it: the
   *  launch still runs, and it alone decides whether the app is entered. */
  onFailure: (step: LoggedInTerminalStep, e: unknown) => void;
}

export async function runLoggedInTerminal(steps: LoggedInTerminalSteps): Promise<void> {
  let registered = true;
  try {
    await steps.register();
  } catch (e) {
    registered = false;
    steps.onFailure('register', e);
  }
  // AWAITED, after the add and before both the switch and the launch —
  // the restore-leg ordering every app keeps (`identity-succession.md` § Seed
  // escrow → *Restore path*; tui's `persist_logged_in_identity`, and
  // linux/windows/FaunaKit's synchronous persist ahead of their session build).
  // After the add because `add_account` claims `active` when nothing holds it
  // (`AccountRegistry::persist_restored_predecessors` § Ordering); before the
  // switch and the launch because the restored session resolves
  // predecessor keys once in places — the conversations manager hands them to
  // the `__mls` replica at construction (`libs/fauna-wasm/src/conversations.rs`),
  // and a load that missed them leaves the session's conversations dark until a
  // reload. Fire-and-forget raced that construction. Still run after a failed
  // `register`: the seeds are the only copies left anywhere.
  try {
    await steps.persistRestoredPredecessors();
  } catch (e) {
    steps.onFailure('restored predecessors', e);
  }
  // Unconditional, a failed `register` included: the claim already created
  // the account, so a client stopped before it is stale either way.
  try {
    steps.beginSession();
  } catch (e) {
    steps.onFailure('begin session', e);
  }
  if (registered && steps.activate) {
    try {
      await steps.activate();
    } catch (e) {
      steps.onFailure('activate', e);
    }
  }
  // No path to the app here: the launch's `Online` verdict is the only one.
  let handedOff = false;
  steps.launch(() => {
    if (handedOff) return;
    handedOff = true;
    steps.handoffs();
  });
}
