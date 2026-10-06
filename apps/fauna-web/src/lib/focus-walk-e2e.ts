// The `focus_move` / `switch_pane` registry entries — convention 17 layer
// (c)'s walk vocabulary (`docs/goal/architecture/e2e-conventions.md`
// § convention 17; `fauna_e2e_agent::{FOCUS_MOVE, SWITCH_PANE}`).
//
// This module exists ONLY in builds made for testing (convention 15): its
// sole importer is `$lib/e2e-automation`, itself reached only through
// `+layout.svelte`'s `if (__FAUNA_E2E_AUTOMATION__)` branch, which a
// production `vite build` constant-folds away.
//
// ## Why these two names are claimed only to throw
//
// Every other web e2e command runs as in-page JS through
// `window.__fauna_callCommand` — but in-page JS has no real key door: an
// untrusted `KeyboardEvent('keydown', {key: 'Tab'})` dispatched from script
// moves no focus at all (measured 2026-08-21, bundled Chromium). The
// browser's own sequential-focus navigation is reachable only from the
// Playwright side, where `page.keyboard.press('Tab')` goes through CDP as a
// TRUSTED key event — so both doors are implemented as **driver-level**
// overrides in `tests/e2e-unified/drivers/web.py` (`focus_move` /
// `switch_pane`), backed by the bridge's bare `/keyboard/press` route, not
// here.
//
// Left unclaimed, a caller reaching either name through `call_command` would
// see the registry's generic "unknown e2e command" — indistinguishable from a
// command nobody has built. Claiming them only to throw a NAMED refusal that
// points at the real door is the same shape android's `sync_inject_locations`
// declared-absence arm uses (`TestAgent.kt`): the difference between "not
// implemented" and "implemented one level up" must be visible to whoever
// reads the failure, not just to whoever wrote this file.
//
// See `docs/goal/architecture/e2e-systematic-ui-walks.md` § Implementation
// status today → the web-leg entry for the full design ruling.

import { registerE2eCommands } from './e2e-commands';

const FOCUS_WALK_COMMANDS = ['focus_move', 'switch_pane'] as const;

/** Register the named refusals for `focus_move` / `switch_pane`. Called once
 *  from `$lib/e2e-automation`, before `installCommandHook()` goes up. */
export function registerFocusWalkCommands(): void {
  registerE2eCommands(FOCUS_WALK_COMMANDS, async (action) => {
    throw new Error(
      `${action} on web is a DRIVER door — Playwright presses the real Tab ` +
        `(tests/e2e-unified/drivers/web.py::${action}); it is not an in-page ` +
        `command because an untrusted KeyboardEvent moves no focus`,
    );
  });
}
