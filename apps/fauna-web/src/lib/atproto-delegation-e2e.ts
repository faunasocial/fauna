// The `atproto_delegation_advance_clock` e2e command — web's twin of linux's
// `main.rs` arm of the same name, tui's `automation.rs` arm, and apple's
// `DelegationClockTestCommand.swift`. All four drive the same shared Rust seam
// (`fauna_atproto_settings_machine::delegation_clock`).
//
// This module exists ONLY in builds made for testing (testing.md § convention
// 15). Two importers, both gated: `$lib/e2e-automation` (itself behind
// `+layout.svelte`'s `if (__FAUNA_E2E_AUTOMATION__)`) registers the command,
// and the AT Protocol settings page installs its rehydrate hook inside its own
// `if (__FAUNA_E2E_AUTOMATION__)` branch — so a production `vite build` folds
// both away and this chunk never ships.
//
// ## Why a clock at all, and why only this one
//
// The D10 delegation window is ~90 days (`DELEGATION_WINDOW_SECS`) and the
// `expiring_soon` warning opens ~14 days before it, so the two liveness
// transitions worth proving are unreachable by waiting — and convention 14
// forbids sleeping out a window instead of moving the clock. The offset is
// scoped to the row's RENDER comparison and nothing else: minting always stamps
// the real wall clock, so a faked mint would produce a future-dated cert the
// nest's provision-time check has never seen for real.
//
// ## What it drives
//
// The production repaint path, not a test-only shortcut: setting the offset
// changes nothing on its own, because the page paints off the machine's
// snapshot. The rehydrate hook re-`refresh()`es the machine — the same call the
// page's own 15 s timer makes — so the new liveness reaches the leaf through
// the same `refresh_delegation` → `snapshot()` → `applySnapshot()` chain a real
// lapse would take. (linux's arm nudges `notify_atproto_rehydrate` for exactly
// this reason; this is the browser shape of the same nudge.)

import { atprotoDelegationSetClockOffsetForTest } from '$lib/wasm-atproto-settings';
import { registerE2eCommands } from '$lib/e2e-commands';

/** The AT Protocol page's own refresh-and-repaint callback, installed while the
 *  page is mounted. `null` when the page has never been opened this session. */
let rehydrateHook: (() => Promise<void>) | null = null;

/** Install (or, with `null`, clear on unmount) the page's rehydrate callback.
 *  Called from the AT Protocol page's own `__FAUNA_E2E_AUTOMATION__` branch. */
export function setAtprotoDelegationRehydrateHook(hook: (() => Promise<void>) | null): void {
  rehydrateHook = hook;
}

const ATPROTO_DELEGATION_COMMANDS = ['atproto_delegation_advance_clock'] as const;

/** Register `atproto_delegation_advance_clock`. Called once from
 *  `$lib/e2e-automation`. */
export function registerAtprotoDelegationCommands(): void {
  registerE2eCommands(ATPROTO_DELEGATION_COMMANDS, async (_action, p) => {
    // Set the offset FIRST: the rehydrate below is what reads it, so shifting
    // it afterwards would repaint the OLD liveness and the driver would poll a
    // `state` attr that never moves. `0` is the reset every lapse test owes its
    // successor — the offset is module-wide and nothing auto-clears it.
    await atprotoDelegationSetClockOffsetForTest(Number(p.now_offset_secs ?? 0));
    if (!rehydrateHook) {
      // Loud, per convention 11: a command against a page that was never opened
      // must fail with a diagnosis, not silently do nothing — otherwise the
      // driver's next assertion reports a delegation-liveness product bug that
      // does not exist.
      throw new Error(
        'atproto_delegation_advance_clock: the AT Protocol settings page is not ' +
          'mounted, so there is no delegation row to re-render. Navigate to ' +
          'Settings → AT Protocol first.',
      );
    }
    await rehydrateHook();
    return null;
  });
}
