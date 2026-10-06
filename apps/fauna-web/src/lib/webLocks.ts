// Cross-tab serialization — web's substitute for the native per-account file
// lock (`docs/goal/architecture/apps/account-scoping.md` § Concurrent instances).
//
// **Why the native mapping does not transfer verbatim.** The ratified design
// narrows exclusivity to three critical sections — schema migration, the
// engine-singleton role, and the conversations-engine role — and guards them
// with a kernel file lock beside `mls_state.db`, one OS process per instance.
// The browser inverts that substrate: tabs are separate JS realms (so each tab
// already has its *own* wasm `MlsEngine`, and there is no shared in-process
// engine to elect an owner over) but share one **origin** (so they share
// `localStorage`, `IndexedDB`, and — because `$lib/device-id`'s per-account id
// persists to `localStorage` — one **device id** per account, hence one MLS
// device leaf). The contention
// on web is therefore over *stored state*, not over process roles, and the
// sections have to be re-derived rather than ported:
//
//   * **The registry mutation section — real, and guarded in shared Rust, not
//     here.** Every registry mutator (the install-secret mint at boot, an add,
//     a switch, a remove, a cache refresh, a flag write) is a read-modify-write
//     of the single `fauna/index` every tab shares, and `localStorage` offers
//     no cross-tab transaction. Native takes a file lock inside the shared
//     registry; a Web Lock is async and the shared mutators are not, so web's
//     lock wraps each wasm mutator entry from outside instead
//     (`fauna_client_accounts::with_web_mutation_lock`, the origin-wide
//     exclusive lock `fauna.accounts.migrate`). Every `WasmAccountRegistry`
//     mutator is therefore a Promise, and a TS caller cannot reach an unlocked
//     one — this module has no registry lock to offer. This is web's whole
//     substitute for the native migration section.
//   * **The engine / conversations-engine role — real, and guarded by
//     [`tryHoldEngineRole`].** Each tab holds its own engine, so nothing
//     contends for the engine *object*; what contends is the shared device leaf
//     and the single account-scoped `provider` replica both tabs CAS-put on the
//     nest. Two engines advancing one leaf's ratchet is exactly the fork the
//     device-owned-epoch invariant forbids, and the replica's three-way merge
//     resolves a true conflict by last-writer-wins, which is lossy for ratchet
//     state. [`engineLockName`] is its name derivation, kept here so the two
//     lock families can never key differently (the same reasoning that keeps
//     native lock files and raise endpoints on one shared derivation).
//
// **The two families take OPPOSITE postures on a missing lock manager, and that
// is deliberate — do not unify them.** `navigator.locks` needs a secure context
// and is absent in some embeddings.
//
//   * **The mutation lock degrades OPEN** (Rust-side, `with_web_mutation_lock`).
//     A missing lock manager must not become an app that refuses to start —
//     the same rule the native lock states as "degrading open on I/O failure
//     (a filesystem hiccup must not become an app that refuses to launch)".
//     The section then runs unguarded, which is precisely the behaviour that
//     shipped before the lock existed: a narrow lost-update window on the
//     index, never a broken switch or sign-out.
//   * **The engine role fails CLOSED** ([`tryHoldEngineRole`]). Its worst case
//     is not a re-run, it is a forked ratchet on user-irrecoverable state, and
//     the native role lock makes exactly this distinction for exactly this
//     reason (`libs/fauna-mls/src/storage.rs`). A tab that cannot prove it is
//     the only writer renders a refusal instead of writing.

/**
 * The per-account conversations-engine role lock's name, in the one scheme
 * every web lock follows — `fauna.<owner>.<role>/<key>`
 * (`account-runtime.md` § Multi-instance concurrency → *Election mechanics*,
 * the 2026-10-01 ruling): the owning crate is `fauna-mls`, the role is the
 * conversations engine's (`mls_state.db.lock` natively), and the key is the
 * actor, because web has one MLS device leaf per (origin, account) and no
 * per-app `mls_state.db` to key on. It is deliberately NOT the runtime's
 * engine-singleton election (`fauna.account-store.engine/<store name>`,
 * taken in shared Rust): two sections, two names, one tab holding both.
 * One derivation, so this family and any future per-account lock cannot
 * disagree about how an account keys.
 */
export function engineLockName(actorId: string): string {
  return `fauna.mls.conversations-engine/${actorId.trim().toLowerCase()}`;
}

/** A held engine role. `release()` hands it to whichever tab is waiting. */
export interface EngineRole {
  release(): void;
}

/**
 * Try to become this account's single MLS-writing tab, holding the role for as
 * long as the returned grant lives. Answers `null` when the role is unavailable
 * — and the caller must then run **no** MLS engine at all.
 *
 * ⚠ **This lock does NOT share the registry mutation lock's posture, and the
 * difference is the whole point.** That lock (Rust-side,
 * `with_web_mutation_lock`) queues and degrades open; both behaviours are
 * wrong here, for reasons the native role lock states directly
 * (`libs/fauna-mls/src/storage.rs`, `SqliteStorage::open` +
 * `acquire_role_lock`):
 *
 *   * **Never queue — try once.** Native uses `try_lock` and hands a held lock
 *     straight back as `StateServedElsewhere`. A queueing acquire would leave
 *     the second tab awaiting a lock the first releases only when it closes,
 *     which is not a wait, it is a hang.
 *   * **Fail CLOSED, not open.** Native: *"a lock-file I/O failure fails closed
 *     — unlike the account instance lock's degrade-open posture, because this
 *     state is class 5 (user-irrecoverable) … degrading open would trade a
 *     near-impossible availability corner for a silent ratchet fork."* The same
 *     trade decides the missing-`navigator.locks` case here: every tab of one
 *     origin shares one device leaf per account (`$lib/device-id` persists
 *     to `localStorage`) and one account-scoped `provider` replica, so an
 *     unguarded pair does not race a file — it forks a ratchet, and the
 *     replica's three-way merge resolves a true conflict last-writer-wins,
 *     which is lossy for exactly that state. A tab that cannot prove it is
 *     alone therefore does not write, and says so.
 *
 * The caller's job on `null` is a *rendered* refusal, never a crash and never a
 * silent second writer — see `$lib/conversations`'s `engineRoleDenied`.
 */
export async function tryHoldEngineRole(actorId: string): Promise<EngineRole | null> {
  // No lock manager → cannot prove solitude → do not write (see the note above).
  if (!hasLockManager()) return null;

  let handOver!: () => void;
  const heldUntilReleased = new Promise<void>((resolve) => {
    handOver = resolve;
  });

  return new Promise<EngineRole | null>((settle) => {
    let settled = false;
    const answer = (role: EngineRole | null) => {
      if (!settled) {
        settled = true;
        settle(role);
      }
    };
    navigator.locks
      // `ifAvailable` is the browser's `try_lock`: the callback is invoked with
      // `null` rather than queued when another tab already holds the role.
      .request(engineLockName(actorId), { mode: 'exclusive', ifAvailable: true }, (lock) => {
        if (!lock) {
          answer(null);
          return;
        }
        answer({ release: handOver });
        // Holding the role IS this promise staying pending: the Web Locks API
        // releases when the callback settles, so the role lives exactly as long
        // as the grant, and a closed tab releases it for free (the native lock's
        // "released when the holder dies — no stale-lock reconciliation").
        return heldUntilReleased;
      })
      // A rejected request (no secure context, a SecurityError embedding) is
      // the fail-closed case, not a reason to write unguarded.
      .catch(() => answer(null));
  });
}

/**
 * The accounts among `actorIds` whose MLS-engine role another tab of this
 * browser profile holds right now — web's leg of *An erase refuses while a
 * sibling serves the account* (`account-scoping.md` § Concurrent instances),
 * the question a sign-out asks before it erases anything.
 *
 * **This tab's own role answers without a probe.** An `ifAvailable` request
 * beside a role this tab already holds would meet its own reflection and refuse
 * every sign-out in the elected tab — the case native closes by putting its own
 * lock down for the probe (`SessionInstanceHolder::without_own_lock`). Web
 * consults instead of releasing: holding the role *is* the proof no other tab
 * does, and a release-and-retake would hand another tab's receive poll a window
 * to win the role this tab still serves. `heldHere` names the accounts whose
 * role this tab holds.
 *
 * **Every other account is probed — `ifAvailable`, never a wait**, which would
 * hang the gesture behind another tab's engine; a granted probe is dropped the
 * moment it lands.
 *
 * **Degrades OPEN — the posture of every native reader of this question**, and
 * the opposite of [`tryHoldEngineRole`]'s, deliberately: here the worst case of
 * a wrong "free" is the pre-ruling sign-out, while a wrong "held" is a browser
 * its owner cannot sign out of. Without a lock manager no tab can hold the role
 * at all (the role fails closed), so nobody is serving and the answer is empty.
 */
export async function actorsServedByAnotherTab(
  actorIds: readonly string[],
  heldHere: (actorId: string) => boolean,
): Promise<string[]> {
  if (!hasLockManager()) return [];
  const served: string[] = [];
  const seen = new Set<string>();
  for (const actorId of actorIds) {
    const name = engineLockName(actorId);
    if (seen.has(name) || heldHere(actorId)) continue;
    seen.add(name);
    const free = await navigator.locks
      .request(name, { mode: 'exclusive', ifAvailable: true }, (lock) => lock != null)
      .catch(() => true);
    if (!free) served.push(actorId);
  }
  return served;
}

/** Why a remove-account refuses, or `null` when it may proceed. */
export type RemoveAccountBlock = 'this_tab' | 'other_tab' | null;

/**
 * Remove-account's question, in the order the ruling fixes (`account-scoping.md`
 * § Concurrent instances → *Remove-account also refuses the account THIS
 * instance serves*): first whether `actorId` is the account this tab serves —
 * its own role says nothing about that, since the probe below skips it — then
 * whether another tab holds its engine role. `servedHere` is this tab's
 * signed-in actor (it follows the per-tab pin), not the registry's active
 * pointer; `heldHere` is [`actorsServedByAnotherTab`]'s.
 */
export async function removeAccountBlock(
  actorId: string,
  servedHere: string | null | undefined,
  heldHere: (actorId: string) => boolean,
): Promise<RemoveAccountBlock> {
  if (servedHere && engineLockName(servedHere) === engineLockName(actorId)) return 'this_tab';
  return (await actorsServedByAnotherTab([actorId], heldHere)).length > 0 ? 'other_tab' : null;
}

/** True when this browser exposes the Web Locks API. */
function hasLockManager(): boolean {
  return typeof navigator !== 'undefined' && navigator.locks != null;
}
