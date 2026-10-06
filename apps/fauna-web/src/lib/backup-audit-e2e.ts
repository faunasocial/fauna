// The `backup_audit_run_now` e2e command — web's twin of linux's
// `backup_audit::{set_rerun_hook, poke_rerun}` + `set_clock_offset_secs`
// (`apps/fauna-linux/src/backup_audit.rs`).
//
// This module exists ONLY in builds made for testing (testing.md § convention 15).
// Two importers, both gated: `$lib/e2e-automation` (itself behind
// `+layout.svelte`'s `if (__FAUNA_E2E_AUTOMATION__)`) registers the command, and the
// Backups page installs its rerun hook inside its own
// `if (__FAUNA_E2E_AUTOMATION__)` branch — so a production `vite build` folds both
// away and this chunk never ships.
//
// ## What the command drives, and what it does not
//
// It pokes the **production** audit path: the Backups page's own `refreshAudit`,
// same `backupAuditRunPass`, same render. Not a test-only shortcut — a shortcut
// would prove nothing about what a user sees. The one thing it injects is *time*
// (`backupAuditSetClockOffsetForTest`), because the two thresholds worth proving are
// multi-day by construction: `AUDIT_MIN_INTERVAL` is 24 h and `AUDIT_OVERDUE` is
// 7 d, and testing.md § convention 14 forbids sleeping out a window instead of
// moving the clock. The destination connection, its `fauna.backup.custody.list`
// reply, the sampled bytes, and this client's own observation high-water all stay
// real — nothing about the audit's *finding* is injectable.

import { backupAuditSetClockOffsetForTest } from '$lib/wasm';
import { registerE2eCommands } from '$lib/e2e-commands';

/** The Backups page's own audit-refresh callback, installed while the page is
 *  mounted. `null` when the page has never been opened this session. */
let rerunHook: (() => Promise<void>) | null = null;

/** Install (or, with `null`, clear on unmount) the page's audit-refresh callback.
 *  Called from the Backups page's own `__FAUNA_E2E_AUTOMATION__` branch. */
export function setBackupAuditRerunHook(hook: (() => Promise<void>) | null): void {
  rerunHook = hook;
}

const BACKUP_AUDIT_COMMANDS = ['backup_audit_run_now'] as const;

/** Register `backup_audit_run_now`. Called once from `$lib/e2e-automation`. */
export function registerBackupAuditCommands(): void {
  registerE2eCommands(BACKUP_AUDIT_COMMANDS, async (_action, p) => {
    // Set the offset FIRST: the pass stamps `last_attempt_at`/`last_passed_at`
    // through this same clock, so shifting it after the pass would record the
    // wrong instant. `0` is the reset every audit test owes its successor — the
    // offset is a process-wide static and nothing auto-clears it.
    backupAuditSetClockOffsetForTest(Number(p.now_offset_secs ?? 0));
    if (!rerunHook) {
      // Loud, per convention 11: a command against a page that was never opened
      // must fail with a diagnosis, not silently do nothing — otherwise the
      // driver's next assertion reports a product bug that does not exist.
      throw new Error(
        'backup_audit_run_now: the Backups page is not mounted, so there is no ' +
          'audit to re-run. Navigate to Backups first.',
      );
    }
    await rerunHook();
    return null;
  });
}
