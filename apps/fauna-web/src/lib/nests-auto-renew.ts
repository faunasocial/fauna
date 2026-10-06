// The blessed-grant auto-renew loop's web seat (`nests.md` § Expiry / renewal
// → *Duration and blessing*): every app dispatches `AutoRenew` at app
// foreground and then every `AUTO_RENEW_CHECK_SECS`, whichever page the user
// sits on — tui's `nests_renew_tick`, linux's page-owned timer. The sweep
// itself is shared Rust (`LinkedNestsMachine::auto_renew`): it renews only on a
// nest whose PROVEN identity is blessed, each due grant by its own mint-time
// length, and degrades silently to renewing less. This module only schedules.

import { linkedNestsMachineWithTrust } from '$lib/rpc';
import { autoRenewCheckSecs, ensureWasm } from '$lib/wasm';

/** One pass: a fresh trust-enabled machine, `AutoRenew` dispatched. Best-effort
 *  by design — a failure means renewing less this hour, never an error surface
 *  (the grant's liveness says so as it approaches its end). */
export async function runNestsAutoRenew(secretHex: string): Promise<void> {
  try {
    await ensureWasm();
    const machine = await linkedNestsMachineWithTrust(secretHex);
    await machine.dispatch('AutoRenew');
  } catch {
    // Silent, like the shared sweep's own failures.
  }
}

/** Run a pass now and then on the shared cadence; returns the stop function. */
export function startNestsAutoRenew(secretHex: string): () => void {
  void runNestsAutoRenew(secretHex);
  let timer: ReturnType<typeof setInterval> | null = null;
  void ensureWasm().then(() => {
    timer = setInterval(() => void runNestsAutoRenew(secretHex), autoRenewCheckSecs() * 1000);
  });
  return () => {
    if (timer) clearInterval(timer);
  };
}
