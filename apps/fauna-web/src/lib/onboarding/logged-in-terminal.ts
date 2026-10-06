// The onboarding `LoggedIn` terminal's tail, as one sequence the page hands its
// steps to (`routes/onboarding/+page.svelte::handleWizardExit`).

export type LoggedInTerminalStep = 'register' | 'restored predecessors' | 'activate';

export interface LoggedInTerminalSteps {
  /** The terminal's registry write — `persist_logged_in`, or append mode's
   *  `accountsAdd`. Awaited: the feed's `identity.init()` reads the registry
   *  back. */
  register: () => Promise<void>;
  /** The predecessor seeds a phrase-only restore recovered, linked to the
   *  restored identity by name; resolves at once when there are none. */
  persistRestoredPredecessors: () => Promise<void>;
  /** Append mode's switch to the added identity; absent on a first sign-in,
   *  whose identity is already active. Skipped when `register` failed — there
   *  is no added identity to switch to. */
  activate?: () => Promise<void>;
  /** The fire-and-forget post-`LoggedIn` hand-offs (kit, DNS credential,
   *  deployment seed, trust set, serving enablement). */
  handoffs: () => void;
  /** Navigate into the authenticated app. */
  enterApp: () => void;
  /** Log a failed step; no failure here may keep the user out of the app. */
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
  // AWAITED, after the add and before both the switch and entering the app —
  // the restore-leg ordering every app keeps (`identity-succession.md` § Seed
  // escrow → *Restore path*; tui's `persist_logged_in_identity`, and
  // linux/windows/FaunaKit's synchronous persist ahead of their session build).
  // After the add because `add_account` claims `active` when nothing holds it
  // (`AccountRegistry::persist_restored_predecessors` § Ordering); before the
  // switch and the navigation because the restored session resolves
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
  if (registered && steps.activate) {
    try {
      await steps.activate();
    } catch (e) {
      steps.onFailure('activate', e);
    }
  }
  steps.handoffs();
  steps.enterApp();
}
