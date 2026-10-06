// This tab's account runtime — web's host of the account driver
// (`account-client-lifecycle.md` § The client-side lifecycle → *The trigger
// fired*, ruling (4)). The hosting itself is shared Rust
// (`fauna_account_plane::web_host`, reached through `fauna-wasm`'s
// `account_runtime` module); this file is only its lifecycle beside the
// conversations manager's:
//
// - **Start** once the manager exists (`$lib/conversations` calls
//   `startAccountRuntimeFor` right after installing it). The runtime
//   registers the account-plane conversations seams on that manager at its
//   store-ready edge — the same edge every native app uses. Only the tab that
//   won the account's MLS-writing role builds a manager, so a second tab
//   hosts no runtime of its own.
// - **Stop**, and the caller says why (`account-client-lifecycle.md` § The
//   client-side lifecycle → *Ruling (4), the teardown rider*).
//   `'sign-out'` is the sign-out gesture's own stop, run by `identity.logout()`
//   before it clears the identity: it retires this browser's enrollment over
//   the session the tab still holds, and is awaited under the hosts' one stop
//   budget because the credential wipe and the store erase follow it.
//   `'account-switch'` is every other stop — `resetConversationsManager` (the
//   actor-scoped teardown) fires it on each identity change — and leaves the
//   machine enrolled. Either returns once the store and its Web Lock are
//   released.
//
// - **The store-change notice** (`$lib/store-change`) is relayed from the
//   started runtime to the open store-backed pages, and ends with it.
//
// A start that fails leaves the tab on the blob rail and says so in the log —
// the native degrade posture; nothing on any page waits on it.
//
// **The account port** (`account-client-lifecycle.md` § The client-side
// lifecycle → *The account port*): a page machine in another wasm chunk
// reaches this runtime through `sharedAccountPort(secretHex)`, the ONE
// implementation of the typed `SharedAccountPort` the chunks declare — each
// call a door name and canonical bytes, run by the core chunk's
// `accountPortCall`. A port is minted for one account and every call carries
// that account's actor id, so a machine that outlives an account switch is
// refused by the next account's runtime rather than writing into it.

import {
  logMessage,
  accountRuntimeShutdown,
  accountRuntimeShutdownForSignOut,
  accountRuntimeStopBudgetMs,
  accountPortCall,
  accountStoreChangedAfter,
  actorIdFromSecret,
} from './wasm';
import { relayStoreChanges } from './store-change';
import { startAccountRuntime } from './rpc';
import type { SharedAccountPort, WasmConversationsManager } from '../../static/fauna_wasm.js';

/** The account the running (or starting) runtime is for, or `null`. */
let runtimeActor: string | null = null;
/** The in-flight or settled start, so a stop can wait it out first. */
let starting: Promise<void> | null = null;
/** Whether `starting` settled STARTED — the runtime is up and the account
 *  store readable. False while the start is in flight and after a failed one. */
let started = false;
/** The account a sign-out stop has run for and whose identity this tab has not
 *  dropped yet. No runtime starts for it in between: a manager rebuilt inside
 *  that window would re-open the store the erase is about to delete. Cleared
 *  by the next stop — the actor-scoped reset's, which follows the identity
 *  change. */
let signedOutActor: string | null = null;

/** Why a runtime is stopped — the browser's spelling of the shared
 *  `StopReason`: a sign-out retires the enrollment, anything else keeps it. */
export type StopReason = 'sign-out' | 'account-switch';

/** Start this tab's runtime for `actorId` over `manager` — once per account;
 *  a repeat for the same account is a no-op. Never throws: a failed start is
 *  logged and cleared, so the next manager build may try again. */
export function startAccountRuntimeFor(
  secretHex: string,
  actorId: string,
  manager: WasmConversationsManager,
): void {
  if (runtimeActor === actorId || signedOutActor === actorId) return;
  runtimeActor = actorId;
  started = false;
  const attempt: Promise<void> = startAccountRuntime(secretHex, manager).then(
    () => {
      // Only the attempt still in the slot speaks: a stop (or the next
      // account's start) since then owns the flag.
      if (starting !== attempt) return;
      started = true;
      // The store-change notice for the open pages (`$lib/store-change`): one
      // relay per started runtime, ended by the runtime's own stop.
      void relayStoreChanges(accountStoreChangedAfter, (e) =>
        logMessage('warn', 'fauna_web::store_change', `store-change notice: ${e}`),
      );
    },
    (e) => {
      logMessage(
        'warn',
        'fauna_web::account_runtime',
        `account runtime not started — this tab stays on the blob rail: ${e}`,
      );
      if (starting === attempt) {
        runtimeActor = null;
        starting = null;
      }
    },
  );
  starting = attempt;
}

/** The account port for `secretHex`'s account — what a chunk machine that
 *  needs the store is wired with (`DevicesMachine.setAccountPort`). The actor
 *  id is bound here, at mint; the core chunk refuses the call when no runtime
 *  runs or the running one serves another account. Needs the core chunk
 *  loaded (it derives the actor id). */
export function sharedAccountPort(secretHex: string): SharedAccountPort {
  const actorIdHex = actorIdFromSecret(secretHex);
  return {
    call: (door: string, payload: Uint8Array) => accountPortCall(actorIdHex, door, payload),
  };
}

/** Stop this tab's runtime for `reason`. Waits for an in-flight start to
 *  settle first, so the store it opened is the one shut down.
 *
 *  A `'sign-out'` stop is bounded by the hosts' one stop budget, the in-flight
 *  start waited out inside it: the user asked to be signed out, so a stop that
 *  overruns is logged and the erase proceeds anyway. A tab with no runtime
 *  retires nothing and returns at once. */
export async function stopAccountRuntime(reason: StopReason): Promise<void> {
  const pending = starting;
  signedOutActor = reason === 'sign-out' ? runtimeActor : null;
  runtimeActor = null;
  starting = null;
  started = false;
  const stop = (async () => {
    if (pending) await pending;
    if (reason === 'sign-out') await accountRuntimeShutdownForSignOut();
    else await accountRuntimeShutdown();
  })();
  if (reason !== 'sign-out') return stop;
  let lapse: ReturnType<typeof setTimeout> | undefined;
  const lapsed = new Promise<'lapsed'>((resolve) => {
    lapse = setTimeout(() => resolve('lapsed'), accountRuntimeStopBudgetMs());
  });
  try {
    const outcome = await Promise.race([stop.then(() => 'stopped' as const), lapsed]);
    if (outcome === 'lapsed') {
      logMessage(
        'warn',
        'fauna_web::account_runtime',
        'sign-out: the account runtime did not stop within its budget — erasing anyway',
      );
    }
  } catch (e) {
    logMessage('warn', 'fauna_web::account_runtime', `sign-out: the account runtime's stop failed: ${e}`);
  } finally {
    clearTimeout(lapse);
  }
}

/** Resolves once this tab's in-flight runtime start (if any) has settled —
 *  for a read that rests on the account store, so a page entered while the
 *  runtime is still starting reads the store rather than nothing. Never
 *  rejects: a failed start settles too (the read then degrades). */
export async function accountRuntimeSettled(): Promise<void> {
  if (starting) await starting;
}

/** Has this tab's runtime started — is the account store readable right now?
 *  The synchronous half of `accountRuntimeSettled`: a launch pass that rests
 *  on the store runs in line when this is true and waits for the settle when
 *  it is not (`$lib/launch-pass`). */
export function accountRuntimeStarted(): boolean {
  return started;
}
