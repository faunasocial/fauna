// The `family_notify_check_now` e2e command — testing.md convention 14's
// "run_now poke" for `$lib/familyNotify`'s flush cadence.
//
// This module exists ONLY in builds made for testing (testing.md § convention 15).
// Its sole importer is `$lib/e2e-automation`, itself reached only through
// `+layout.svelte`'s `if (__FAUNA_E2E_AUTOMATION__)` branch, which a production
// `vite build` constant-folds away.
//
// ## What it drives, and what it does not
//
// It pokes the production flush path directly (`notifyBuffer.checkNow()` →
// the same private `flushIfDue()` the real `CHECK_INTERVAL_MS` interval would
// call) rather than waiting out the real timer, which `familyNotify.ts`'s
// `ensureTimer()` doesn't even arm under this same agent (see its own comment) —
// so under test the poke is the ONLY thing that ever checks. The cadence GATE
// itself (`NOTIFY_REPORT_MIN_INTERVAL_SECS`, ≤hourly) is untouched: this injects
// no clock offset, because the buffer's first flush is eager (no prior flush),
// which is the only case any test here needs.

import { notifyBuffer } from '$lib/familyNotify';
import { registerE2eCommands } from '$lib/e2e-commands';

const FAMILY_NOTIFY_COMMANDS = ['family_notify_check_now'] as const;

/** Register `family_notify_check_now`. Called once from `$lib/e2e-automation`. */
export function registerFamilyNotifyCommands(): void {
  registerE2eCommands(FAMILY_NOTIFY_COMMANDS, async (_action, _payload) => {
    notifyBuffer.checkNow();
    return null;
  });
}
