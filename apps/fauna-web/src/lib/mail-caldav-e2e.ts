// The `enable_caldav_mailbox` e2e command — web's twin of linux's and tui's arm
// of the same name (`apps/fauna-tui/src/automation.rs`), so the shared helper
// `tests/e2e-unified/helpers/mail_dedicated_nest.py::mint_caldav_mailbox` drives
// every wired app through one spelling (convention 11's cross-app command table).
//
// This module exists ONLY in builds made for testing (convention 15): its sole
// importer is `$lib/e2e-automation`, itself reached only through
// `+layout.svelte`'s `if (__FAUNA_E2E_AUTOMATION__)` branch, which a production
// `vite build` constant-folds away.
//
// ## What the command drives
//
// The production mint: the shared mail-settings machine's
// `enableCaldavMailboxWith{Password,GeneratedPassword}`, the same face web's
// first-setup glue calls for a CalDAV-only user. It is a fixture step (mint the
// logged-in actor's CalDAV mailbox material) that a test runs before asserting
// on what a page renders, never the gesture under test.
//
// ## The reply
//
// The outcome is published as `caldav_mailbox_reply` in the test agent's state,
// in linux's exact wire shape — `{ok: true}` / `{ok: false, error}` — and absent
// until a run completes, which is how the helper tells "not finished" from
// "failed". It is cleared when a run starts and set before the command resolves,
// so the driver's first poll never reads a stale value. A failed mint is
// reported there, not thrown: the helper's own assertion names it.

import { get } from 'svelte/store';

import { registerE2eCommands } from '$lib/e2e-commands';
import { mailSettingsMachine } from '$lib/rpc';
import { identity } from '$lib/store';
import { accountsTabSessionMaterial } from '$lib/accounts';

type CaldavMailboxReply = { ok: true } | { ok: false; error: string };

/** The agent reads this slot (`tests/e2e-unified/web-bridge/agent.js`). */
function setReply(reply: CaldavMailboxReply | undefined): void {
  // This module is automation-only (its sole importer is `$lib/e2e-automation`),
  // so the guard is belt-and-braces at runtime — but convention 15's web pin
  // holds every `__fauna_*` write to a block-precise standard, not a
  // file-level one, and a module-private setter has no exported installer for
  // the reachability rung to walk.
  if (__FAUNA_E2E_AUTOMATION__) {
    (window as unknown as { __fauna_caldav_mailbox_reply?: CaldavMailboxReply })
      .__fauna_caldav_mailbox_reply = reply;
  }
}

const MAIL_CALDAV_COMMANDS = ['enable_caldav_mailbox'] as const;

/** Register `enable_caldav_mailbox`. Called once from `$lib/e2e-automation`.
 *  Payload: `{password?: string}` — a password mints a `default` credential the
 *  test knows (so a stock CalDAV client can AUTH as this actor); absent, one is
 *  generated. */
export function registerMailCaldavCommands(): void {
  registerE2eCommands(MAIL_CALDAV_COMMANDS, async (_action, p) => {
    setReply(undefined);
    try {
      const secret = get(identity)?.secretHex ?? accountsTabSessionMaterial()?.secret_hex;
      if (!secret) throw new Error('no identity on this seat (not logged in?)');
      const machine = await mailSettingsMachine(secret);
      if (typeof p.password === 'string' && p.password !== '') {
        await machine.enableCaldavMailboxWithPassword('Default', p.password);
      } else {
        await machine.enableCaldavMailboxWithGeneratedPassword('Default');
      }
      setReply({ ok: true });
    } catch (e) {
      setReply({ ok: false, error: String(e) });
    }
    return null;
  });
}
