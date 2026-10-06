// A singleton build must reach a TERMINAL STATE — the shared settle deadline
// every memoized actor-scoped build in the SPA is wrapped in.
//
// ── Why this exists in the shared layer rather than on a page ───────────────
//
// `singleton-build-memo-contract.test.ts` states the property this enforces and
// the class it comes from; `web.md` § Async manager readiness is the owner.
// The short form: a promise memo caches a REJECTION as durably as a value —
// that half is handled by each builder's memo clear — and it caches a promise
// that NEVER SETTLES exactly as durably again, which no `.catch` can see. A
// `wasm_bindgen_futures` task whose poll throws dies mid-poll: the exception
// surfaces as a browser `pageerror` and its JS promise is never settled, so
// every internal deadline the build relied on dies with it. "The wasm request
// is bounded" proofs hold only while the task survives its polls.
//
// Only a timer OUTSIDE the task survives the task's death, so the deadline is
// external by necessity, not by preference.
//
// The precedent is `onboarding/+page.svelte`'s `armLaunchWatchdog` — added in
// 2026-07-16 after three sessions of phantom root causes on exactly this class,
// and for a long time the SPA's ONLY guarded wasm-driven await. Four singleton
// builders needed the identical rule and none had it; writing a fifth private
// copy into a page is the per-surface divergence priorities #1/#2 exist to
// refuse, so it lives here and the census in
// `singleton-build-memo-contract.test.ts` keeps it complete.
//
// ── What it is not ─────────────────────────────────────────────────────────
//
// NOT a retry: the dead task is not repaired, and this must never be read as a
// fix for whatever threw. NOT a longer wait: the budget bounds how long a
// surface may LIE about being ready, and cannot make a slow build pass — the
// failure it catches is permanent, so e2e convention 14 is untouched.
//
// NOT a cancellation, either — and that is why the build is handed a predicate.
// A promise cannot be stopped, so a build that is merely slow rather than dead
// is ABANDONED while still alive, a state no failure path before the deadline
// could produce (a rejection yields nothing; a resolution was always the
// current one). Left to itself it resolves after a replacement build has been
// installed and writes its own result over it: the memo vends the replacement
// while the module slot holds the abandoned build — a split-brain between the
// promise a page awaits and the object its snapshot refreshers read. So the
// guard hands every build `stillWanted()`, false from the moment the deadline
// gives up on it, and each builder checks it beside `stillThisActor()` before
// every module-state write that follows an await — the identity seam's shape
// (`actorScope.ts`), one predicate over. What a builder does on `false` is its
// own remedy: the page managers skip the write, `rpc.ts` retires the socket.

/** 45 s — the same budget `armLaunchWatchdog` has carried since 2026-07-16, and
 *  for the same reason: it must sit far enough above every bound it backstops
 *  that it can only ever fire on a task that is genuinely dead. The bounds
 *  inside these builds are `ensureConnected`'s 15 s throw (`rpc.ts`) and a
 *  kind's RPC deadline (5 s for `fauna.drafts.get`, the slowest await in the
 *  feed and conversations builds), so a healthy build settles inside ~20 s even
 *  on a saturated box. */
export const SINGLETON_BUILD_DEADLINE_MS = 45_000;

/** Thrown when a memoized build neither resolves nor rejects within its budget.
 *  A distinct type so a caller can tell a dead build from a build that failed
 *  for a reason — the two want the same user-visible treatment but very
 *  different diagnosis. */
export class SingletonBuildStalled extends Error {
  constructor(readonly label: string, readonly budgetMs: number) {
    super(
      `${label}: build neither resolved nor rejected within ${budgetMs / 1000}s — ` +
        `the underlying task is dead (a wasm future whose poll threw takes its own ` +
        `deadlines down with it). Look for a [pageerror] in the browser console.`,
    );
    this.name = 'SingletonBuildStalled';
  }
}

/** Start a memoized singleton build so it always reaches a terminal state.
 *
 *  `build` is called at once, synchronously, and handed `stillWanted`: true
 *  while the guard still wants this build's result, false from the moment the
 *  settle deadline abandons it, and never flipped back. A build that settled
 *  inside its budget stays wanted for good, so a fire-and-forget tail it starts
 *  may keep consulting it. Check it beside `stillThisActor()` before every
 *  module-state write that follows an await — never only once: a check near the
 *  first write says nothing about a write three awaits later.
 *
 *  Returns the promise the caller must MEMOIZE and return — memoizing the raw
 *  build instead would hand the bounded promise to the first caller and the
 *  unbounded one to everybody after it (the contract test pins this).
 *
 *  On any terminal failure — a real rejection, a synchronous throw from `build`,
 *  or the settle deadline — it calls `clearMemo` and logs to the browser
 *  **console**, which is the ring `drivers/web.py::console_log` captures and a
 *  failing e2e dumps. The rejection then reaches the awaiting page's own
 *  `catch`, which sets its error surface (web.md § Async manager readiness — a
 *  handler's catch may not assume the manager stamped the failure).
 *
 *  `clearMemo` is written by the CALL SITE, not here, because only the call site
 *  can compare the slot against its own build: by the time it runs, the
 *  actor-scoped drop may already have installed the INCOMING actor's build, and
 *  clearing unconditionally would throw that live build away.
 *
 *      const stillThisActor = sameActorSince();
 *      const build = async (stillWanted: () => boolean) => {
 *        const built = await feedManager(secretHex);
 *        if (!stillThisActor()) throw new Error('…actor changed…');
 *        if (!stillWanted()) throw new Error('…abandoned…');
 *        manager = built;
 *        return built;
 *      };
 *      const guarded = guardSingletonBuild('feed manager', build, () => {
 *        if (managerPromise === guarded) managerPromise = null;
 *      });
 *      managerPromise = guarded;
 *      return guarded;
 */
export function guardSingletonBuild<T>(
  label: string,
  build: (stillWanted: () => boolean) => Promise<T>,
  clearMemo: () => void,
  budgetMs: number = SINGLETON_BUILD_DEADLINE_MS,
): Promise<T> {
  let abandoned = false;
  const stillWanted = () => !abandoned;
  let building: Promise<T>;
  try {
    building = build(stillWanted);
  } catch (e) {
    // A non-`async` build can throw before it returns a promise; that is the
    // same terminal failure as a rejection, and must reach the same clear
    // rather than escape the caller mid-way through installing its memo.
    building = Promise.reject(e);
  }
  let timer: ReturnType<typeof setTimeout> | null = null;
  const deadline = new Promise<never>((_resolve, reject) => {
    timer = setTimeout(() => {
      timer = null;
      // Flip BEFORE rejecting, so every continuation the rejection schedules —
      // the memo clear, the page's catch, a replacement build it triggers — runs
      // in a world where this build already knows it was given up on.
      abandoned = true;
      reject(new SingletonBuildStalled(label, budgetMs));
    }, budgetMs);
  });
  // `race` and not a wrapper around `building`: the point is a terminal state
  // for the AWAITER even when the build itself has none. The build promise is
  // left alone because it cannot be stopped — if its task is merely slow rather
  // than dead it resolves later, and `stillWanted()` is what keeps that late
  // resolution from installing itself over the replacement build (the header's
  // split-brain). A late throw from it is delivered to `race`'s own handler, so
  // an abandoned build raises no unhandled rejection either way.
  const guarded = Promise.race([building, deadline]).finally(() => {
    if (timer !== null) clearTimeout(timer);
    timer = null;
  });
  void guarded.catch((e) => {
    clearMemo();
    // `[singleton]` is one of the boot-breadcrumb markers the e2e compose
    // diagnostic rescues out of the elided head of the console ring
    // (`actions/feed.py::_compose_console_tail`) — a build death happens early
    // and would otherwise fall off the tail on any page that logs afterwards.
    console.error(`[singleton] ${label} build failed:`, e);
  });
  return guarded;
}
