// The one account switch web runs — from Settings' account switcher and from
// the switcher `launch_retry` carries when the device holds more than one
// account (`onboarding.md` § Wizard exit handling → *The finished wizard
// holds*; `long-term-store.md` § Multi-account evolution). Both surfaces call
// the same three steps, so the re-auth gate, the leave gestures and the
// relaunch cannot drift between them:
//
//   1. `switchReauthTarget` — the Stage-2 question, asked BEFORE anything runs;
//   2. `performAccountSwitch` — activation first, then the leave gestures, then
//      the full-document relaunch as the new active account;
//   3. `switchRefusalText` — the refusal line a surface paints in its
//      `error-message` when step 2 rejects (nothing was torn down).

import { resolve } from '$app/paths';
import { accountsList, accountsSwitch, accountsSwitchConfirmed, type AccountEntry } from '#lib/accounts';
import { dropActorPushRow } from '#lib/push';
import { flushDraftRailsNow } from '#lib/drafts-leave-flush';
import { rejectionText } from '#lib/rejection';
import { accountDisplayLabel } from '#lib/wasm';
import { t } from '#lib/i18n/strings';

/** A switch target whose activation waits on the in-app re-auth prompt. */
export interface ReauthTarget {
  actorId: string;
  label: string;
}

/** One row of an account switcher, in registry order. */
export interface SwitcherRow {
  actorId: string;
  label: string;
  isActive: boolean;
}

/** The switcher rows over a registry list, `servedActor` the account this tab
 *  serves (the active, here unreachable, one) — compared case-insensitively,
 *  as every switcher does. */
export function switcherRows(accounts: AccountEntry[], servedActor: string | undefined): SwitcherRow[] {
  const served = servedActor?.toLowerCase();
  return accounts.map((a) => ({
    actorId: a.actor_id,
    label: accountDisplayLabel(a.handle, a.actor_id),
    isActive: a.actor_id.toLowerCase() === served,
  }));
}

/** The Stage-2 gate in front of the switch: the target when it is flagged
 *  `require_confirm_to_activate` (the caller shows the in-app re-auth prompt),
 *  else `null` (switch at once). The flag is read FRESH from the registry,
 *  never off a rendered row: the admin auto-default can write it after the
 *  surface loaded its list, and a stale row would skip the prompt (the registry
 *  would still refuse — fail-closed, but a dead-feeling click). Rejects when
 *  the registry cannot be read. */
export async function switchReauthTarget(actorId: string): Promise<ReauthTarget | null> {
  const flagged = (await accountsList()).find(
    (a) => a.actor_id === actorId && a.require_confirm_to_activate,
  );
  return flagged ? { actorId, label: accountDisplayLabel(flagged.handle, flagged.actor_id) } : null;
}

/** Activate `actorId` and relaunch the tab as it. Rejects — before anything is
 *  torn down — when the registry refuses the activation; on success the
 *  document navigates away and the promise never meaningfully settles.
 *
 *  `confirmed` is the Stage-2 re-auth bit: plain `set_active` REFUSES a flagged
 *  account, so a path that skipped the prompt fails loudly here. Only a re-auth
 *  prompt's confirm passes `true` — those call sites are the gate's audit
 *  surface. `outgoingSecretHex` is the account being left (its push row is
 *  dropped while its authority is still in hand), `null` when there is none. */
export async function performAccountSwitch(
  actorId: string,
  confirmed: boolean,
  outgoingSecretHex: string | null,
): Promise<void> {
  // set_active + re-pin this tab, then a full reload re-runs the launch flow
  // against the now-active identity, read back from the registry (web's
  // teardown+rebuild; the admin shell reveals itself via the per-actor
  // `am-i-admin` check).
  if (confirmed) {
    await accountsSwitchConfirmed(actorId);
  } else {
    await accountsSwitch(actorId);
  }
  // The leave-gesture push drop (`common.md` § Registration): the OUTGOING
  // actor's row falls while its authority is still in hand — awaited (not
  // fire-and-forget) because the hard reload below would kill an in-flight
  // call; best-effort inside the helper, so an offline switch still switches.
  // The incoming actor re-arms via the reload's `rearmPush`. AFTER the
  // activation, never before: a refused switch leaves the user where they were,
  // and that includes their notifications (`multiple-accounts` outcome 11).
  if (outgoingSecretHex) await dropActorPushRow(outgoingSecretHex);
  // The leave-flush (`reserved-folders.md` § The leave-flush promise —
  // switching to another account is a leave door): every drafts rail saved NOW
  // on the outgoing session, awaited, because the hard reload below retires
  // that session and `pagehide`'s own fire-and-forget flush gets no reliable
  // async work after it. After the activation, like the push drop. Each rail is
  // bounded, so an offline switch still switches.
  await flushDraftRailsNow();
  // Count the teardown at its INITIATION point — synchronously, in the same job
  // as the navigation below, never after it (the navigation never returns).
  // Convention 14's negative-assert observable
  // (`fauna_e2e_agent::SESSION_GENERATION_KEY`); the write is a
  // `sessionStorage` bump precisely so it survives the document swap.
  //
  // Reached through the installed `window.__fauna_*` hook rather than a static
  // `#lib/generation-e2e` import, so convention 15 holds in the strong form the
  // other hooks have: a production build constant-folds this branch away AND
  // never bundles the module. A dynamic import here would be wrong for a
  // different reason — it resolves a microtask later, and the navigation below
  // does not wait.
  if (__FAUNA_E2E_AUTOMATION__) {
    (window as unknown as { __fauna_recordSessionTeardown?: () => void })
      .__fauna_recordSessionTeardown?.();
  }
  // Raw navigation (full document reload) — `base` ('/app') + the route, so
  // /app/feed. A hard reload re-runs the launch flow against the now-active
  // identity; a soft `goto` would not rebuild the wasm WS-RPC/MLS session. The
  // work a finished wizard deferred is never carried across: it belongs to its
  // own account and runs only at that account's `Online`.
  window.location.assign(resolve('/feed'));
}

/** The refusal line for a rejected [`performAccountSwitch`]: the shared
 *  `switch_refused_copy` the wasm registry rejects with, else the plain
 *  refusal naming `account`. */
export function switchRefusalText(e: unknown, account: string): string {
  return rejectionText(e, t.settings.switch_refused({ account }));
}
