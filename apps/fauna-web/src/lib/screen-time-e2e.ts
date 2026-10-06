// The `screen_time_heartbeat` e2e command — testing.md convention 14's fake
// clock + "run_now poke" for the screen-time usage heartbeat
// (`family-safety.md` § Screen time, Slice E). The browser twin of linux's
// `screen_time_heartbeat` test-agent command.
//
// This module exists ONLY in builds made for testing (testing.md § convention
// 15). Its sole importer is `$lib/e2e-automation`, itself reached only through
// `+layout.svelte`'s `if (__FAUNA_E2E_AUTOMATION__)` branch, which a production
// `vite build` constant-folds away — so `advanceTestClock` has no caller in a
// production bundle and the skew it writes stays 0.
//
// ## Why a fake clock rather than a wait
//
// Screen time is the one family pillar whose behavior genuinely is a function
// of elapsed time, so it is exactly the shape testing.md § point 14 rules
// DEFUNCT: a test that slept for a real heartbeat interval would take minutes
// and still give an untrustworthy verdict under the load these machines
// actually run at. Advancing the clock moves accrual, the report cadence and
// the lock verdict together, consistently and instantly.
//
// ## What it drives, and what it does not
//
// The poke advances the clock and then runs the production heartbeat step
// (`tickUsageHeartbeat` — the same call the layout's one-minute interval
// makes), so the wire path under test is the real one: the client's own
// `familyUsageReport` call and the reply feeding back into the lock. None of
// the *rules* are faked — the cadence, the accrual cap, the failure re-credit
// and the "lock-screen time is not use" rule all live in shared Rust and are
// proven exhaustively at tier_1 in `fauna_core::screen_time::tests`.

import { get } from 'svelte/store';

import { registerE2eCommands } from '$lib/e2e-commands';
import { identity } from '$lib/store';
import { advanceTestClock, tickUsageHeartbeat } from '$lib/screenTime.svelte';

const SCREEN_TIME_COMMANDS = ['screen_time_heartbeat'] as const;

/** Register `screen_time_heartbeat`. Called once from `$lib/e2e-automation`. */
export function registerScreenTimeCommands(): void {
  registerE2eCommands(SCREEN_TIME_COMMANDS, async (action, payload) => {
    const raw = (payload as { minutes?: unknown })?.minutes ?? 0;
    const minutes = Number(raw);
    if (!Number.isFinite(minutes)) {
      // Convention 11: a non-numeric `minutes` used to reach `advanceTestClock`
      // as NaN, which moves the skew nowhere — the command acked green, the
      // clock never advanced, and the accrual assertion failed as a screen-time
      // product bug several steps later.
      throw new Error(
        `${action}: \`minutes\` must be a finite number, got ${JSON.stringify(raw)}`,
      );
    }
    advanceTestClock(minutes * 60);
    const secret = get(identity)?.secretHex;
    if (!secret) {
      // Loud, per convention 11 — the sibling arms (`silent_sign_in`,
      // `atproto_delegation_advance_clock`, `backup_audit_run_now`) all refuse
      // by name here. This arm used to `if (secret)` its way past a missing
      // identity: the clock moved, no heartbeat was sent, and the driver read a
      // green ack, so the missing usage row surfaced downstream as a budget
      // regression that does not exist.
      throw new Error(
        `${action}: no identity on this seat — the usage heartbeat is only ` +
          `meaningful on an authenticated session`,
      );
    }
    await tickUsageHeartbeat(secret);
    return null;
  });
}
