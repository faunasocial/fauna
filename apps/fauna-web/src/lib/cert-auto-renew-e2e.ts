// The `auto_renew_wake` e2e command — testing.md convention 14's `run_now` poke
// for the certificate auto-renew tick (`#lib/cert-auto-renew`), the web twin of
// `fauna_e2e_agent::AUTO_RENEW_WAKE` (tui's and linux's `LoopWake`, android's
// debug `AutoRenewWake`).
//
// This module exists ONLY in builds made for testing (testing.md § convention 15).
// Its sole importer is `#lib/e2e-automation`, itself reached only through
// `+layout.svelte`'s `if (__FAUNA_E2E_AUTOMATION__)` branch, which a production
// `vite build` constant-folds away.
//
// ## What it drives, and what it does not
//
// It ends the loop's six-hour WAIT only: the pass that follows is the
// production one — the shared `autoRenewScan` / `autoRenewIssue` on the
// credentialed machine, the bound nest id — and the cadence is untouched (no
// compressed clock). A wake that lands while a pass runs, or before the loop
// started, is kept for the next wait (`#lib/loop-wake`).

import { wakeCertAutoRenewForTest } from '#lib/cert-auto-renew';
import { registerE2eCommands } from '#lib/e2e-commands';

/** Mirrors `fauna_e2e_agent::AUTO_RENEW_WAKE`. */
const AUTO_RENEW_WAKE = 'auto_renew_wake';

/** Register `auto_renew_wake` and its window hook. Called once from `#lib/e2e-automation`. */
export function registerCertAutoRenewCommands(): void {
  registerE2eCommands([AUTO_RENEW_WAKE], async (_action, _payload) => {
    wakeCertAutoRenewForTest();
    return null;
  });
  if (typeof window !== 'undefined') {
    (window as unknown as { __fauna_autoRenewWakeForTest?: () => void }).__fauna_autoRenewWakeForTest =
      wakeCertAutoRenewForTest;
  }
}
