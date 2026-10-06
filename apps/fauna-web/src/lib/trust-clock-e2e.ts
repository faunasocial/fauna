// The `trust_facet_advance_clock` e2e command — web's twin of tui's
// `automation.rs` arm of the same name and the UniFFI apps' agent arms. All of
// them drive the same shared Rust seam
// (`fauna_client_capabilities::trust_clock`).
//
// This module exists ONLY in builds made for testing (testing.md § convention
// 15): its one importer, `$lib/e2e-automation`, sits behind `+layout.svelte`'s
// `if (__FAUNA_E2E_AUTOMATION__)`, so a production `vite build` folds it away.
//
// The repaint: the Nests page's hydrate reads the clock, and on tui/linux the
// driver's per-poll navigate re-hydrates it. On web it does not — re-setting the
// same settings route never remounts `NestsSection` — so the mounted page
// installs a rehydrate hook this command calls after moving the clock: the
// page's own `hydrate()` → snapshot → render chain, only the clock moved (the
// `$lib/atproto-delegation-e2e` shape). Unlike that command, a missing hook is
// NOT an error: a journey resets the clock before it ever opens the Nests page,
// and the next mount hydrates against the moved clock anyway.
//
// Scope on web: the grant fold (`fauna-wasm`'s pair machine) only. The custody
// receipt fold runs in the `wasm-folders` chunk, which has no test flavor, so
// its freshness is not moved here — the only journey that ages a receipt is the
// custody ceremony, tui-only (`CustodyActions.require_supported`).

import { trustSetClockOffsetForTest } from '$lib/wasm';
import { registerE2eCommands } from '$lib/e2e-commands';

/** The Nests page's own re-hydrate callback, installed while it is mounted. */
let rehydrateHook: (() => Promise<void>) | null = null;

/** Install (or, with `null`, clear on unmount) the Nests page's re-hydrate
 *  callback. Called from `NestsSection`'s own `__FAUNA_E2E_AUTOMATION__` branch. */
export function setTrustClockRehydrateHook(hook: (() => Promise<void>) | null): void {
  rehydrateHook = hook;
}

const TRUST_CLOCK_COMMANDS = ['trust_facet_advance_clock'] as const;

/** Register `trust_facet_advance_clock`. Called once from `$lib/e2e-automation`. */
export function registerTrustClockCommands(): void {
  registerE2eCommands(TRUST_CLOCK_COMMANDS, async (_action, p) => {
    // `0` is the reset every lapse test owes its successor — the offset is
    // module-wide and nothing auto-clears it.
    // Set the offset FIRST: the rehydrate below is what folds against it.
    trustSetClockOffsetForTest(Number(p.now_offset_secs ?? 0));
    await rehydrateHook?.();
    return null;
  });
}
