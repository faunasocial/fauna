// The loud surfaces' e2e seams on web — the critical-alert banner's re-sweep
// poke and the connection indicator's pace, plus the two window-claim counters
// the connection-gap journeys read (`test_critical_alert_lifetime.py`,
// `test_connection_gap_rules.py`). Contracts: `fauna_e2e_agent::{ALERT_SWEEP_WAKE,
// RECONNECT_BACKOFF, CONNECTION_REPORTS_KEY, PAINTED_ERRORS_KEY}`; tui is the
// reference leg, and the counting is the natives' own shared Rust, reached
// through the core wasm chunk.
//
// This module exists ONLY in builds made for testing (convention 15): its sole
// importer is `$lib/e2e-automation`, itself reached only through
// `+layout.svelte`'s `if (__FAUNA_E2E_AUTOMATION__)` branch, which a production
// `vite build` constant-folds away — and the wasm halves are `test-helpers` only.
//
// Both commands are triggers, never shortcuts. `alert_sweep_wake` ends the
// production loop's wait, so the loop's own body sweeps (a re-established
// session would run a one-shot pass and pass with the loop deleted);
// `reconnect_backoff` changes the reconnect loop's pace, never the `Unreachable`
// threshold, so every failure the threshold counts is still a real refused dial.

import { registerE2eCommands, type E2eCommandHandler } from './e2e-commands';
import { installPaintedErrorObserver } from './e2e-painted-errors';
import { setReconnectBackoffForTest } from './rpc';
import { alertSweepWakeForTest, connectionReportsForTest, paintedErrorsForTest } from './wasm';

const ACTIONS = ['alert_sweep_wake', 'reconnect_backoff'] as const;

const handler: E2eCommandHandler = async (action, payload) => {
  if (action === 'alert_sweep_wake') {
    // Fire-and-forget: the barrier is `alert_sweep_passes`, never this ack.
    if (!alertSweepWakeForTest()) {
      // Convention 11: no identity's loop is running, so the wake would be
      // acked and read by nobody.
      throw new Error('alert_sweep_wake: no authenticated session, so no sweep loop to wake');
    }
    return null;
  }
  setReconnectBackoffForTest(payload);
  return null;
};

/** Claim the two commands, start the painted-error observer, and publish the
 *  two counters for `web-bridge/agent.js`'s state assembly. */
export function registerLoudSurfaceCommands(): void {
  registerE2eCommands(ACTIONS, handler);
  installPaintedErrorObserver();
  const w = window as unknown as {
    __fauna_connectionReports?: () => unknown;
    __fauna_paintedErrors?: () => unknown;
  };
  w.__fauna_connectionReports = connectionReportsForTest;
  w.__fauna_paintedErrors = paintedErrorsForTest;
}
