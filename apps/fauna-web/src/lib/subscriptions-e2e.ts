// The `subscriptions_hold_author_drain` / `subscriptions_release_author_drain`
// e2e commands — web's twins of tui's `automation.rs` arms of the same names and
// the linux/android agent arms. All of them drive the same shared Rust seam
// (`fauna_client_subscriptions::orchestration::hold_author_drain_for_test`), a
// flag the author pump's `drain_auto_approvals` reads after its queue read, so
// a payment-backed subscribe request stands in the author's §2 requests while
// the test reads its paid badge (`monetization.md` § Pillar 3).
//
// This module exists ONLY in builds made for testing (testing.md § convention
// 15): its one importer, `#lib/e2e-automation`, sits behind `+layout.svelte`'s
// `if (__FAUNA_E2E_AUTOMATION__)`, so a production `vite build` folds it away.

import { subscriptionsSetAuthorDrainHeldForTest } from '#lib/wasm';
import { registerE2eCommands } from '#lib/e2e-commands';

const SUBSCRIPTIONS_COMMANDS = [
  'subscriptions_hold_author_drain',
  'subscriptions_release_author_drain',
] as const;

/** Register the author-drain hold commands. Called once from `#lib/e2e-automation`. */
export function registerSubscriptionsCommands(): void {
  registerE2eCommands(SUBSCRIPTIONS_COMMANDS, async (action) => {
    subscriptionsSetAuthorDrainHeldForTest(action === 'subscriptions_hold_author_drain');
    return null;
  });
}
