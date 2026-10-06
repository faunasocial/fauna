// The web app's e2e **command** registry — the single owner of
// `window.__fauna_callCommand`, which the Playwright bridge's
// `driver.call_command(action, payload)` drives
// (`tests/e2e-unified/drivers/web.py`).
//
// This module exists ONLY in builds made for testing (testing.md § convention 15):
// its sole importer is `$lib/e2e-automation`, itself reached only through
// `+layout.svelte`'s `if (__FAUNA_E2E_AUTOMATION__)` branch, which a production
// `vite build` constant-folds away.
//
// ## Why a registry rather than one big switch
//
// `__fauna_callCommand` is a single global slot, so a second domain that installed
// its own hook would silently clobber the first — the exact "a command is quietly
// dropped" failure convention 11 exists to prevent, and one that reads downstream
// as a real product bug. Before this module the whole table lived inside
// `$lib/conversations` (already hosting `feed_inject_posts`, which is not a
// conversations command at all), so the second domain to need a command had no
// honest home. Now each domain declares the action names it owns and hands over a
// handler; the registry rejects a duplicate registration at install time instead of
// letting one win at call time.
//
// ## Convention 11: honour it or fail loudly
//
// An unrecognised action **throws**. It must never `return` quietly, log at debug,
// or fall through: the driver's next assertion would then fail on product state
// that is perfectly fine, and the command table is a cross-app contract — implement
// it or refuse it explicitly.

/* eslint-disable @typescript-eslint/no-explicit-any */

/** One command's implementation. `payload` is the already-parsed JSON object the
 *  driver sent (`{}` when it sent nothing). The resolved value is returned to the
 *  driver verbatim; `null` is the conventional "no result". */
export type E2eCommandHandler = (action: string, payload: any) => Promise<unknown>;

const table = new Map<string, E2eCommandHandler>();

/**
 * Claim `actions` for `handler`. Called once per domain from
 * `$lib/e2e-automation`'s install, before the hook goes up.
 *
 * Throws on a duplicate name — two domains claiming one action is a wiring bug
 * whose runtime symptom (whichever registered last wins) is invisible.
 */
export function registerE2eCommands(actions: readonly string[], handler: E2eCommandHandler): void {
  for (const action of actions) {
    const existing = table.get(action);
    if (existing && existing !== handler) {
      throw new Error(
        `e2e command '${action}' is claimed by two handlers — one would silently ` +
          `shadow the other (testing.md § convention 11)`,
      );
    }
    table.set(action, handler);
  }
}

/** Install `window.__fauna_callCommand`. Call after every `registerE2eCommands`. */
export function installCommandHook(): void {
  if (typeof window === 'undefined') return;
  (
    window as unknown as {
      __fauna_callCommand?: (action: string, payloadJson: string) => Promise<unknown>;
    }
  ).__fauna_callCommand = async (action: string, payloadJson: string) => {
    const handler = table.get(action);
    if (!handler) {
      // Loud, and it names what IS implemented: a driver-side typo and a
      // genuinely-unbuilt command are different problems, and the caller cannot
      // tell them apart from "it did nothing".
      throw new Error(
        `unknown e2e command: ${action}. Implemented on web: ` +
          `${[...table.keys()].sort().join(', ')}`,
      );
    }
    const payload: any = payloadJson ? JSON.parse(payloadJson) : {};
    return handler(action, payload);
  };
}
