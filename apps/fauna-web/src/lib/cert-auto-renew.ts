// The certificate auto-renew tick's web seat (`tls-certificates.md` § C.3, *Any
// synced admin device renews*): while a signed-in admin's tab is open, the
// shared tick runs on the shared six-hour cadence whichever page the admin sits
// on — tui's `admin::spawn_auto_renew_cadence`, linux's cadence. The loop is
// `#lib/cert-auto-renew-loop`'s; this module wires the real effects:
//
//   * the credentialed `WasmDnsManagementMachine` (`autoRenewScan` /
//     `autoRenewIssue`, thin bindings over the shared calls), built fresh each
//     pass — its pass writes `AUTO_RENEW_PASS_LOG_LINE` to the console ring;
//   * the `target_nest_id` from linked-nests state (D7): `thisNestId()`, the
//     shared `LinkedNestsMachine::bound_nest_id`, which refuses a nest whose
//     claim disagrees with the identity the connection is bound to — resolved
//     only on a pass that found something due;
//   * an open `admin-dns` page re-reading after a pass (`certAutoRenewPasses`).
//
// The wait is raceable by the e2e `auto_renew_wake` poke (convention 14, the
// web twin of `fauna_e2e_agent::AUTO_RENEW_WAKE`) only under
// `__FAUNA_E2E_AUTOMATION__`, which a production `vite build` constant-folds
// away (convention 15): a shipped tab sleeps the clock.

import { writable } from 'svelte/store';
import { dnsManagementMachineWithCredentials, linkedNestsMachine } from '#lib/rpc';
import { autoRenewPollSecs, ensureWasm } from '#lib/wasm';
import { LoopWake } from '#lib/loop-wake';
import { runCertAutoRenewLoop, type AutoRenewPass } from '#lib/cert-auto-renew-loop';

/** Bumped after every pass that issued or tried something — an open
 *  `admin-dns` page re-reads its machine's snapshot on a change. */
export const certAutoRenewPasses = writable(0);

/** The running loop's wake — one per loop instance, as on tui: a stopped
 *  loop, which lingers until its wait ends, holds a handle nothing fires. A
 *  wake that lands before any loop started is kept for the first one. */
let currentWake = new LoopWake();

function plainSleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/** Test-only: end the loop's current six-hour wait now (or the next one). The
 *  body is constant-folded away in a production build, so a shipped tab has no
 *  way to race the clock. Reached only from `#lib/cert-auto-renew-e2e`. */
export function wakeCertAutoRenewForTest(): void {
  if (__FAUNA_E2E_AUTOMATION__) currentWake.wake();
}

let started = false;

/** Start the tick for the signed-in admin; returns the stop function. */
export function startCertAutoRenew(secretHex: string): () => void {
  let stopped = false;
  // The first loop inherits a wake that arrived before it; later ones start clean.
  const wake = started ? new LoopWake() : currentWake;
  started = true;
  currentWake = wake;
  // A fresh credentialed machine per pass, like the blessed-grant loop's: the
  // machine's provider wiring is fixed when it is built, so one held for six
  // hours would issue through whatever the tab had when the admin gate opened.
  // The scan's machine is the one its issue uses.
  let passMachine: Awaited<ReturnType<typeof dnsManagementMachineWithCredentials>> | null = null;
  void ensureWasm().then(() =>
    runCertAutoRenewLoop({
      pollSecs: autoRenewPollSecs,
      sleep: (ms) => (__FAUNA_E2E_AUTOMATION__ ? wake.sleep(ms) : plainSleep(ms)),
      stopped: () => stopped,
      scan: async () => {
        passMachine = await dnsManagementMachineWithCredentials(secretHex);
        return (await passMachine.autoRenewScan()) as string[];
      },
      resolveTargetNestId: async () =>
        (await (await linkedNestsMachine(secretHex)).thisNestId()) as number[],
      issue: async (domains, targetNestId) => {
        if (!passMachine) throw new Error('no scanned machine for this pass');
        return (await passMachine.autoRenewIssue(domains, new Uint8Array(targetNestId))) as AutoRenewPass;
      },
      warn: (line) => console.warn(line),
      shipped: () => certAutoRenewPasses.update((n) => n + 1),
    }),
  );
  return () => {
    stopped = true;
  };
}
