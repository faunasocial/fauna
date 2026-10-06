// The identity domain's e2e commands — the cross-app `silent_sign_in` trigger
// for the post-auth nest-identity re-check (channel 3, the background silent
// challenge), plus web's own `bearer_force_refresh` (channel 1, the bearer
// re-mint) — a command the other six apps have no arm for and therefore refuse
// loudly by construction (their agents' own unmatched-command fallback,
// `tests/test_agent_refuses_unknown_command.py`), which is exactly what
// convention 11 asks of a command only one app implements.
// (`docs/goal/architecture/security.md` § Transport trust → § Post-auth
// surfacing; `tests/e2e-unified/tests/test_nest_identity_pin_post_auth.py`).
//
// This module exists ONLY in builds made for testing (convention 15): its sole
// importer is `$lib/e2e-automation`, itself reached only through
// `+layout.svelte`'s `if (__FAUNA_E2E_AUTOMATION__)` branch, which a production
// `vite build` constant-folds away.
//
// ## What the commands are allowed to be
//
// Triggers, never shortcuts. `silent_sign_in` runs the production background
// refresh — `identity.refreshFromNest` → `refreshFromServer` → `silentSignIn`
// → `challengeVerify`'s possession-verify + pin compare → the catch that
// escalates. `bearer_force_refresh` runs the production re-mint —
// `getAuthToken(secret, undefined, true)` → wasm `challengeVerify`'s
// possession-verify against the TOFU pin → the catch that escalates
// (`$lib/api.ts`). Either way what the test observes is the real pipeline
// reacting to a real verdict; a command that painted the identity-changed
// surface directly (or called `escalateNestIdentityChanged` itself) would
// assert nothing about whether web can actually *detect* a mid-session
// identity change, which is the entire claim under test.
//
// `silent_sign_in`'s name matches the native agents' command (linux/tui/apple
// all expose it) so the shared test module drives all five wired apps through
// one spelling — the command table is a cross-app contract (convention 11).
// `bearer_force_refresh` has no native counterpart yet (native channel 1 is
// itself untested today), so it is web-only by construction rather than by an
// unenforced convention.
//
// `launch_refresh_token` is the cross-app wrong-clock refresh witness's
// ceremony leg (`fauna_e2e_agent::LAUNCH_REFRESH_TOKEN`, case M): the same
// production re-mint as `bearer_force_refresh`, but AWAITED, so the ack lands
// after the outcome and a refused re-mint fails the command loudly
// (convention 11) instead of being swallowed. Its observable is the
// `launch_token` state key (`__fauna_launchToken`, `$lib/api`'s
// `homeBearerScheduleForTest`).

import { get } from 'svelte/store';

import { getAuthToken } from './api';
import { registerE2eCommands, type E2eCommandHandler } from './e2e-commands';
import { identity } from './store';
import { accountsTabSessionMaterial } from './accounts';

const ACTIONS = ['silent_sign_in', 'bearer_force_refresh', 'launch_refresh_token'] as const;

const handler: E2eCommandHandler = async (action) => {
  // Prefer the live store's identity over the tab's registry account: on a seat
  // that switched accounts they can differ for one tick, and the refresh must
  // run for the identity the session is actually holding.
  const secret = get(identity)?.secretHex ?? accountsTabSessionMaterial()?.secret_hex;
  if (!secret) {
    throw new Error(
      `${action}: no identity on this seat — the post-auth re-check is only ` +
        'meaningful on an authenticated session',
    );
  }
  if (action === 'silent_sign_in') {
    // Started, NOT awaited — and that is the cross-app contract, not a
    // shortcut: the shared test acks the command first and waits on the
    // *surface* for the verdict ("call_command blocks until the agent acks the
    // command; the verdict then routes to the surface"). Awaiting would be
    // actively wrong on web, where routing the verdict means a document
    // navigation: the driver's `evaluate` is still holding this promise open,
    // so the context would be torn down under the very call waiting on it.
    //
    // Nothing is dropped by not awaiting. `refreshFromServer` owns the whole
    // ceremony including its own catch (it never rejects), the escalation is
    // fired from inside it, and the observable the test reads is the blocking
    // surface — a latency-independent state behind a generous ceiling
    // (convention 14), not this ack's timing.
    void identity.refreshFromNest(secret);
    return null;
  }
  if (action === 'launch_refresh_token') {
    // Awaited: the witness reads the new session right after the ack. No
    // navigation can tear this call down — the refresh is on the healthy home
    // nest, and a refusal rejects here, which the command hook reports.
    await getAuthToken(secret, undefined, true);
    return null;
  }
  // `bearer_force_refresh`: same fire-and-forget shape and the same reason —
  // `getAuthToken`'s own catch is what calls `escalateIfNestIdentityChanged`
  // (only for the home nest; `$lib/api.ts`), so the rejection this throws is
  // already handled before it reaches here. Swallow it rather than let it
  // become an unhandled promise rejection; a *valid* refresh never rejects at
  // all, so there is nothing else this call site needs to observe.
  void getAuthToken(secret, undefined, true).catch(() => {});
  return null;
};

/** Claim the identity domain's command names. Called from `$lib/e2e-automation`
 *  before `installCommandHook()`. */
export function registerIdentityCommands(): void {
  registerE2eCommands(ACTIONS, handler);
}
