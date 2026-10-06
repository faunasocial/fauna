// Multi-account switcher glue (Stage 1 web,
// `docs/goal/architecture/long-term-store.md` § Multi-account evolution).
//
// The shared account list + active pointer + per-actor slots live in
// Rust (`fauna_client_accounts`, exposed to wasm as `WasmAccountRegistry` over a
// localStorage-backed `SecretStore` — `libs/fauna-wasm/src/accounts.rs`). This
// module is the thin TS surface the Account-settings switcher + append-mode
// onboarding drive. Web consumes the registry directly (its Svelte bootstrap in
// `store.ts` is the launch-machine equivalent), not the native `RegistryLaunchPersistence`.
//
// The registry is cheap + stateless (every op reads/writes localStorage), so we
// construct one per call and free the wasm handle immediately.
//
// Every wasm MUTATOR is a Promise: tabs share one `localStorage`, so each
// mutator's read-modify-write of `fauna/index` runs inside the origin-wide
// cross-tab mutation lock, taken in shared Rust
// (`fauna_client_accounts::with_web_mutation_lock`, web's twin of the native
// file lock — `account-scoping.md` § Concurrent instances → *Which critical
// sections are real here*). Nothing here takes a lock: a TS caller cannot
// reach an unlocked mutator. Reads stay synchronous.

import { writable, type Readable } from 'svelte/store';
import { ensureWasm, wasmCoreModule, signOutFinishRecorded, signOutPlannedErase } from '$lib/wasm';
import type { LocalizedText } from '$lib/i18n/localized';
import { tabPin, setTabPin, clearTabPin, setTabNestUrl } from './tabPin';
import { actorsServedByAnotherTab } from './webLocks';
import type { WasmAccountRegistry } from '../../static/fauna_wasm.js';
import type { SupervisionSnapshotValue } from './supervisionRestore';

/** One account in the switcher — the JS shape of Rust `AccountEntry`. */
export interface AccountEntry {
  actor_id: string;
  handle: string | null;
  domain: string | null;
  tier: string | null;
  require_confirm_to_activate: boolean;
}

/** One account's session material — the JS shape of Rust `SessionMaterial`, and
 *  the twin of native `FfiSessionMaterial`. Everything a tab needs to build its
 *  authenticated session for one account, in one read. */
export interface SessionMaterial {
  actor_id: string;
  secret_hex: string;
  nest_url: string | null;
  device_id: string | null;
  handle: string | null;
  domain: string | null;
  tier: string | null;
}

/** Construct a fresh `WasmAccountRegistry`, run `fn`, and free the handle. The
 *  ES module is already initialized by `ensureWasm()`; the second dynamic
 *  `import` returns the same cached, initialized namespace. A mutator's
 *  Promise is awaited BEFORE the handle is freed — the wasm side holds the
 *  cross-tab lock across it. */
async function withRegistry<T>(fn: (reg: WasmAccountRegistry) => T | Promise<T>): Promise<T> {
  await ensureWasm();
  const mod = await import('../../static/fauna_wasm.js');
  const reg = new mod.WasmAccountRegistry();
  try {
    return await fn(reg);
  } finally {
    reg.free();
  }
}

/** The synchronous twin of {@link withRegistry}, for the boot-path reads that
 *  cannot await: `loadIdentity()` is called synchronously from `identity.init()`
 *  so the UI gets an instant identity from the registry. Safe because the
 *  registry is stateless and its construction is sync — only `ensureWasm()`'s
 *  module init is async, and every caller here already runs after it (the same
 *  assumption every other sync wasm read makes). Returns `undefined` if wasm
 *  is not initialized yet, so a too-early caller degrades to "no identity yet"
 *  instead of throwing. */
function withRegistrySync<T>(fn: (reg: WasmAccountRegistry) => T): T | undefined {
  let reg: WasmAccountRegistry | undefined;
  try {
    reg = new (wasmCoreModule().WasmAccountRegistry)();
    return fn(reg);
  } catch {
    return undefined;
  } finally {
    reg?.free();
  }
}

/** This tab's session material (the pinned account's, or the store-active
 *  account's when unpinned) — the session-identity read of `account-scoping.md`
 *  § Concurrent instances → *Session identity resolves through the session's
 *  account*. `undefined` when the account has no resolvable secret: the
 *  fail-closed answer, never another account's material. */
export function accountsSessionMaterial(actorId: string): SessionMaterial | undefined {
  return withRegistrySync((r) => r.sessionMaterial(actorId) as SessionMaterial | undefined);
}

/** The ACTIVE account's session material — the one read every unpinned
 *  surface uses for "who is signed in here" (the registry is the only home of
 *  the identity; there is no single-slot key beside it). Synchronous for the
 *  boot-path reads, so `undefined` also when wasm is not initialized yet — the
 *  same "too early" answer [`accountsSessionMaterial`] gives. */
export function accountsActiveSessionMaterial(): SessionMaterial | undefined {
  return withRegistrySync((r) => {
    const active = r.activeActorId();
    return active ? (r.sessionMaterial(active) as SessionMaterial | undefined) : undefined;
  });
}

/** This tab's session material: the pinned account's when the tab is pinned
 *  (fail closed — never another account's), else the active account's. */
export function accountsTabSessionMaterial(): SessionMaterial | undefined {
  const pinned = tabPin();
  return pinned ? accountsSessionMaterial(pinned) : accountsActiveSessionMaterial();
}

/** Boot step, once per page load from `identity.init()`: mint this browser's
 *  install device secret (inside the cross-tab mutation lock, wasm-side),
 *  resolve this tab's account (a pinned tab follows its account's succession
 *  chain), keep the tab's nest URL beside its pin, and pin an unpinned tab to
 *  the active account. The registry is the only source of the identity —
 *  nothing is mirrored anywhere. Returns the actor id this tab serves. */
export async function accountsBoot(): Promise<string | undefined> {
  // A sign-out a closed tab left unfinished is finished first: nothing below
  // may resolve an account the user already signed out of.
  await signOutReconciled();
  return withRegistry(async (r) => {
    const active = r.activeActorId() ?? undefined;
    // The install-secret mint is a registry mutation like any other, and the
    // one every fresh browser races at boot: two tabs opened together would
    // otherwise each mint a secret and end on two device ids for one account.
    // `ensureInstallDeviceSecret` takes the cross-tab lock itself. Never fatal
    // to boot — a page that cannot store the secret gets its failure where it
    // asks for an id (`$lib/device-id`).
    try {
      await r.ensureInstallDeviceSecret();
    } catch (e) {
      console.warn('device id boot step failed:', e);
    }
    const pinned = tabPin();

    if (pinned) {
      // Rider 2 (`account-scoping.md` § Concurrent instances → *The binding
      // follows the account across a succession*): a pin names an ACCOUNT by
      // the id that identified it when the tab was pinned. If a sibling tab
      // or another device has since run the ceremony, the account is the
      // successor — follow the chain and re-point this tab's own pin, exactly
      // as the native `resolve_launch_binding` re-points a process binding.
      const resolved = r.resolvePinnedAccount(pinned);
      // Keep this tab's nest URL beside its pin, so `storedNestUrl()` dials
      // THIS account's home nest rather than the active account's.
      // `undefined` material clears it — the fail-closed read then falls
      // back to the origin, never to another account's nest.
      pinTab(r, resolved);
      return resolved;
    }

    // Unpinned: pin this tab to what it resolved, with its nest URL beside
    // the pin. This is what makes the tab immune to a SIBLING tab's later
    // switch — without it, every later read follows whatever account was
    // activated last, which is precisely the convergence hazard the goal doc
    // names.
    if (active) pinTab(r, active);
    else setTabNestUrl(null);
    return active;
  });
}

/** Pin this tab to `actorId` and keep that account's nest URL beside the pin —
 *  the one shape every re-pin (boot, switch, `LoggedIn` terminal) takes, so a
 *  pin can never sit beside another account's nest URL. */
function pinTab(r: WasmAccountRegistry, actorId: string): void {
  setTabPin(actorId);
  setTabNestUrl((r.sessionMaterial(actorId) as SessionMaterial | undefined)?.nest_url ?? null);
}

/** The account list (add order) for the switcher. */
export function accountsList(): Promise<AccountEntry[]> {
  return withRegistry((r) => JSON.parse(r.listJson()) as AccountEntry[]);
}

/** The active account's actor id (hex), or `undefined`. */
export function accountsActiveActorId(): Promise<string | undefined> {
  return withRegistry((r) => r.activeActorId() ?? undefined);
}

/** Switch the active account: `set_active` + re-pin this tab. The caller
 *  follows with an SPA re-init (a hard nav re-runs the launch flow against the
 *  now-active identity, read back from the registry).
 *
 *  Stage-2 gate (`long-term-store.md` § Multi-account evolution → *Per-account
 *  re-auth*): the registry REFUSES an account whose `require_confirm_to_activate`
 *  flag is set (`ConfirmationRequired`) — a path that skipped the re-auth prompt
 *  fails loudly here, before anything is torn down. The post-confirm path is
 *  [`accountsSwitchConfirmed`]. */
export function accountsSwitch(actorId: string): Promise<void> {
  return withRegistry(async (r) => {
    await r.setActive(actorId);
    // Re-pin THIS tab to the account it just switched to. An explicit switch
    // moves `active`; the pin is what stops the *other* tabs from following along on their next
    // mount, and what makes this tab keep serving its choice if a sibling
    // switches again. "Per-tab UI for choosing the account rides the same
    // switcher affordance" — this is that ride.
    pinTab(r, actorId);
  });
}

/** [`accountsSwitch`], asserting the user has JUST confirmed the in-app re-auth
 *  prompt for this activation (Stage 2). Only ever call adjacent to
 *  `account-activate-reauth-prompt` — the `setActiveConfirmed` call sites are
 *  the audit surface for the gate. */
export function accountsSwitchConfirmed(actorId: string): Promise<void> {
  return withRegistry(async (r) => {
    await r.setActiveConfirmed(actorId);
    pinTab(r, actorId); // see `accountsSwitch`
  });
}

/** Write the per-account "require confirmation to activate" flag — the
 *  `account-require-confirm-toggle` write path. Also marks the flag USER-SET,
 *  which pins the user's choice against the admin auto-default (an explicit OFF
 *  sticks). Therefore only ever call this from a real user gesture — a
 *  programmatic re-sync must go nowhere near it, or merely rendering the page
 *  would consume the user's override right. */
export function accountsSetRequireConfirm(actorId: string, require: boolean): Promise<void> {
  return withRegistry((r) => r.setRequireConfirm(actorId, require));
}

/** The admin auto-default: flip `require_confirm_to_activate` ON iff the user
 *  never touched that account's toggle (`require_confirm_user_set` is the pin —
 *  the shared registry enforces both halves). Idempotent; never turns the flag
 *  off. Call on every `am-i-admin = true` observation for the active account
 *  (`+layout.svelte`'s nav-gate probe). Returns whether this call flipped it. */
export function accountsAutoEnableRequireConfirm(actorId: string): Promise<boolean> {
  return withRegistry((r) => r.autoEnableRequireConfirm(actorId));
}

/** Moment 1 — the confirm-identity commit point, through the shared
 *  `persist_confirmed_identity`: per-actor account created, secret READ BACK (a
 *  store that silently kept nothing is an error, not a success — the one write
 *  whose silent failure destroys an account outright), and activated. With
 *  `append` (the "Add account" wizard over a live session) it writes NOTHING
 *  and only derives the actor id: confirming an appended identity never
 *  changes which identity is canonical — its own terminal registers and
 *  switches. The append rule lives in shared Rust, not here. Returns the actor
 *  id; throws on a dropped write. */
export function accountsPersistConfirmedIdentity(
  secretHex: string,
  append: boolean,
): Promise<string> {
  return withRegistry((r) => r.persistConfirmedIdentity(secretHex, append));
}

/** Persist the predecessor seeds a phrase-only restore recovered (the
 *  wizard's `restoredPredecessorsJson()`), linked to `restoredActor` — the
 *  shared `AccountRegistry::persist_restored_predecessors`. Empty is a no-op. */
export function accountsPersistRestoredPredecessors(
  restoredActor: string | null,
  predecessorsJson: string,
): Promise<void> {
  return withRegistry((r) => r.persistRestoredPredecessors(restoredActor ?? undefined, predecessorsJson));
}

/** Register (or update) an account from its secret hex + optional slots; the
 *  first account becomes active. Returns the actor id. Append-mode "Add account"
 *  calls this on onboarding success. */
export function accountsAdd(
  secretHex: string,
  nestUrl: string | null,
  deviceId: string | null,
): Promise<string> {
  return withRegistry((r) => r.addAccount(secretHex, nestUrl ?? undefined, deviceId ?? undefined));
}

/** The `LoggedIn` terminal — the wizard's success exit — through the shared
 *  `persist_logged_in` moment (`onboarding.md` § Long-term store contract):
 *  the per-actor account is created carrying its home nest URL, activated, and
 *  the pending-invite slot spent. Returns the actor id. Idempotent, so a
 *  re-entered wizard or a resumed claim is safe.
 *
 *  ⚠ Append mode is exempt: "Add account" registers + switches itself
 *  (`accountsAdd` + `accountsSwitch`), and activating here would move `active`
 *  off the live account before that runs.
 *
 *  ⚠ AWAIT it before navigating into the app: this is the ONLY place the home
 *  nest is recorded, and the next `identity.init()` / `storedNestUrl()` read
 *  it back from the per-actor rows. */
export function accountsPersistLoggedIn(
  secretHex: string,
  nestUrl: string,
  deviceId: string | null,
  /** The reach hint: the box's public IP when this session provisioned it, so the
   *  first main-app session opens connected while the domain is still
   *  propagating (`onboarding.md` § Reach hint). `null` on every other path. */
  reachIpv4: string | null,
): Promise<string> {
  return withRegistry(async (r) => {
    const actorId = await r.persistLoggedIn(
      secretHex,
      nestUrl,
      deviceId ?? undefined,
      reachIpv4 ?? undefined,
    );
    // This tab now serves the identity it just onboarded, on the nest it just
    // recorded — the same re-pin an explicit switch does, so the post-`LoggedIn`
    // hand-offs dial the new home nest even from a previously pinned tab.
    pinTab(r, actorId);
    return actorId;
  });
}

/** Remove an account: its per-actor slots and index entry, then its account
 *  store (the wasm side does both, registry first). If it was active, the first
 *  remaining account becomes active. */
export async function accountsRemove(actorId: string): Promise<void> {
  await withRegistry((r) => r.remove(actorId));
}

// ── The sign-out record ──────────────────────────────────────────────────────
//
// A web sign-out is decided once, durably, before its first await, and a load
// that finds the record finishes it (`account-scoping.md` § The scoping
// taxonomy → *Erasure follows scope*, the paragraph "Web's account store is in
// the erase too", decision 2). The record and everything it orders are shared
// Rust (`fauna_client_accounts::SignOutRecord`, `fauna-wasm`'s
// `account_scope`), reached through `$lib/wasm`'s `signOutRecordBegin` (the
// gesture's decision), `signOutWipeOwed` (the identity read's fail-closed
// gate) and the finish below.
//
// The store half of the erase can fail (decision 4): a database delete waits
// behind a connection somebody else holds. The erase is bounded, the account
// stays in the record, and the user is told with the shared residue line —
// `signOutResidue`, which the onboarding page paints as its `sign-out-residue`
// view, whose Remove Again runs {@link signOutFinish}`('retry')`.

const residue = writable<LocalizedText | null>(null);

/** The residue line the last {@link signOutFinish} left: set while account
 *  stores a sign-out could not erase are still in this browser (or the
 *  refusal, when another tab served one of them), `null` after a clean sweep
 *  (which says nothing). Every finish overwrites it, so it always
 *  answers for the newest sweep. State no wizard owns, so no wizard render can
 *  wipe it (`account-scoping.md` § Erasure follows scope, the ⚠ *pin the line*). */
export const signOutResidue: Readable<LocalizedText | null> = { subscribe: residue.subscribe };

/** Finish whatever the sign-out record owes: the credential wipe if it has not
 *  run, then each recorded account's store. The gesture's own tail and
 *  {@link signOutReconciled} both call it. A pin that names no account after it
 *  is dropped — the wipe removed the account it named.
 *
 *  `sweep` says who is asking, because they differ in one question. The
 *  `'gesture'` asked the other-tab refusal before it recorded anything (the
 *  order is refuse, record, stop, wipe, erase), so it does not ask again. A
 *  `'load'` and a `'retry'` (the residue view's Remove Again) sweep stores
 *  somebody may have started serving since, so they put the planned accounts to
 *  the probe first — both run with nobody signed in here, so this tab holds no
 *  role and every one is probed — and erase nothing when another tab serves
 *  one of them; the line then becomes the retry's refusal. */
export async function signOutFinish(sweep: 'gesture' | 'load' | 'retry'): Promise<void> {
  let eraseRefused = false;
  if (sweep !== 'gesture') {
    const planned = await signOutPlannedErase();
    eraseRefused =
      planned.length > 0 && (await actorsServedByAnotherTab(planned, () => false)).length > 0;
  }
  const finish = await signOutFinishRecorded(eraseRefused);
  residue.set(finish.residue ?? null);
  if (finish.found) {
    const pinned = tabPin();
    if (pinned && !accountsSessionMaterial(pinned)) clearTabPin();
  }
}

let reconciled: Promise<void> | null = null;

/** The boot reconcile, once per page load: a load that finds the sign-out
 *  record finishes the sign-out before anything routes on the registry. Every
 *  boot path that reads the registry to decide where the user lands awaits it
 *  first — `accountsBoot()` and the onboarding page's launch machine. Never
 *  rejects: a reconcile that failed leaves the record for the next load. */
export function signOutReconciled(): Promise<void> {
  reconciled ??= signOutFinish('load').catch((e) => {
    console.warn('sign-out reconcile failed; retried at the next load:', e);
  });
  return reconciled;
}

/** Sign-out's credential wipe: erase every account's per-actor slots and the
 *  `fauna/index` blob (`long-term-store.md` § Cleanup contract). Without this
 *  the index keeps pointing at the signed-out account and the next load reads
 *  its still-stored secret back — silently signing the user back in as the
 *  identity they signed out of. */
export async function accountsClearAll(): Promise<void> {
  await withRegistry((r) => r.clearAll());
  // The pin names an account that no longer exists, so it must go with them —
  // a surviving pin would make this tab's next load fail closed (the
  // fail-closed `sessionMaterial` miss) instead of landing on the launch flow.
  clearTabPin();
}

/** Walk away from `actorId`'s nest, keeping the identity: clears its
 *  (nest_url, device_id) slots, so the next launch reads no home nest for it
 *  (`account-scoping.md` § Concurrent instances, the delete corollary). Used
 *  by the admin-nest "factory reset this nest" walk-away. */
export function accountsClearNestBinding(actorId: string): Promise<void> {
  return withRegistry((r) => r.clearNestBinding(actorId));
}

/** The persisted last-known supervision snapshot for `actorId`, or `undefined`
 *  for "no information" (absent or malformed slot — the ruled fail direction,
 *  family-safety.md § Content policy clause 2). Parsed Rust-side by the slot
 *  format's single owner; the write side is `familyStatus`'s own success path
 *  (`libs/fauna-wasm/src/rpc.rs`), so no setter exists here on purpose. */
export function accountsSupervisionSnapshot(
  actorId: string,
): Promise<SupervisionSnapshotValue | undefined> {
  return withRegistry(
    (r) => r.supervisionSnapshot(actorId) as SupervisionSnapshotValue | undefined,
  );
}

/** Update an account's server-data cache (handle/domain/tier) — the rows the
 *  switcher and `loadIdentity()` read. Called after a silent sign-in refresh
 *  and at registration. Rejects (`UnknownActor`) for an identity not in the
 *  index yet (an append-mode identity before its terminal). */
export function accountsUpdateCache(
  actorId: string,
  handle: string | null,
  domain: string | null,
  tier: string | null,
): Promise<void> {
  return withRegistry((r) =>
    r.updateCache(actorId, handle ?? undefined, domain ?? undefined, tier ?? undefined),
  );
}
