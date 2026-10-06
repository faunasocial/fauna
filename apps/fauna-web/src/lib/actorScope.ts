// The SPA's one seam for **in-memory** actor-scoped state.
//
// `account-scoping.md` § The scoping taxonomy binds this: the switch/sign-out
// isolation contract forbids account B *rendering* or modifying account A's local
// state, and its in-memory corollary says a live cache, a manager instance or a
// decrypted-bytes blob URL is account-scoped by class 1/4 exactly as its on-disk
// twin would be. On web the account-scoped data that survives a switch is almost
// entirely in memory: the manager singletons, the content-policy cache, and
// per-route caches on components.
//
// ── Why a registry rather than a reset call site ──────────────────────────────
//
// Before this module the drop was **hand-listed in three places** (`identity.login`,
// `identity.logout`, and the e2e hook), each naming the same two functions. Every
// piece of actor-scoped state added since had to be remembered at all three, and
// each time it was not, the result was a silent wrong-actor render:
//
//   * `login()`'s add-account door did not reset the managers at all.
//   * the feed route's six per-post caches were not on any list — they outlived the
//     actor and permanently suppressed the incoming actor's lazy resolves.
//   * `contentPolicy.svelte.ts`'s module state is on no list even now — a switch
//     kept the outgoing account's guardian content floor.
//
// So nothing is hand-listed: the registration lives **next to the state it drops**,
// and adding actor-scoped state means adding one `registerActorScopedReset` call in
// the module that owns it — never editing a switch handler that lives somewhere
// else and that nobody thinks about.
//
// ── Why it is driven by the identity, not by a lifecycle event ────────────────
//
// `store.ts` fires this from a single subscription to the identity store, keyed on
// `secretHex`, so it runs on *any* identity change — whether it went through
// `identity.login()` or a bare `identity.set()` (the e2e agent's session patch
// deliberately bypasses `login()`, which needs WASM). Page/route lifecycle is NOT a
// safe trigger: an in-app switch is a same-route `goto`, which does not remount the
// component, and production's full reload is a coincidence of how the switcher
// happens to navigate rather than a guarantee anything enforces.
//
// Ordering is part of the contract: `store.ts` fires **every** reset before **any**
// actor-change handler, so a route's caches are already clear when its own rebuild
// runs. Registering a reset that itself rebuilds state would break that — resets
// drop, handlers rebuild.

/** Registered drops, in registration order. A `Set` so an unregister is exact and
 *  a double registration cannot double-fire. */
const resets = new Set<() => void>();

// ── The seam before the drop ──────────────────────────────────────────────────
//
// `account-scoping.md` § The scoping taxonomy's in-memory corollary: "dropping
// state is only half of it — the background loops that WRITE that state must be
// retired by the same drop", and a loop holding no cancellation handle "cannot be
// stopped by any list, so the seam comes before the drop".
//
// On web the loops are the rails' singleton BUILDERS: each is an async IIFE that
// assigns module state (`manager`, `face`, `current`) *after* an await, so a
// switch landing mid-build is not stopped by a reset that only nulls those
// variables — the in-flight build simply writes them again, for the actor that
// just left. There is no `Job` to cancel: a promise is not cancellable, and a
// wasm call already in flight resolves whatever the caller does. So the seam is
// the third sanctioned form, linux's: a generation the drop bumps, captured at
// launch, re-checked before every write.
let generation = 0;

/**
 * Capture the current actor generation; the returned predicate answers "is the
 * actor this work was started for still the current one?".
 *
 * Call it at the TOP of any async build (before the first await) and check it
 * before every write of module-level actor-scoped state that happens after an
 * await. The two must be separate calls — capturing inside the check would
 * always compare the generation to itself.
 */
export function sameActorSince(): () => boolean {
  const started = generation;
  return () => generation === started;
}

/** The current actor generation — for the tests that pin this seam. */
export function actorGeneration(): number {
  return generation;
}

/**
 * Register a drop for one piece of in-memory actor-scoped state.
 *
 * Call it in the module that owns the state (module-level state) or in `onMount`
 * (component-local state), and call the returned unregister in `onDestroy` so a
 * destroyed component's closure does not outlive it.
 *
 * The reset must be **idempotent** and must not rebuild: it may run when nothing
 * is stale (boot, a repeat switch), and rebuilding here would race the
 * actor-change handlers that run after every reset.
 */
export function registerActorScopedReset(reset: () => void): () => void {
  resets.add(reset);
  return () => {
    resets.delete(reset);
  };
}

/**
 * Drop every registered piece of in-memory actor-scoped state.
 *
 * Called by `store.ts` on an identity change, ahead of the actor-change handlers.
 * One failing reset must not strand the others — a half-dropped switch is the
 * silent wrong-actor render this whole seam exists to prevent — so each runs
 * guarded.
 */
export function resetActorScopedState(): void {
  // Bump FIRST, ahead of every drop: a reset that itself awaits nothing still
  // runs before the actor-change handlers, and an in-flight build that resolves
  // between two `reset()` calls must already read as the previous actor's.
  generation += 1;
  for (const reset of resets) {
    try {
      reset();
    } catch (e) {
      // Deliberately console-only: this runs inside the identity store's `set`,
      // where there is no page to surface an error on and no caller to return one
      // to. A reset that throws is a bug in that reset, not a reason to abandon
      // the rest of the switch.
      console.error('[actor-scope] reset failed:', e);
    }
  }
}

/** How many drops are registered — for the unit tests that pin this seam. */
export function registeredActorScopedResetCount(): number {
  return resets.size;
}
