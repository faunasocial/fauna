// Behavioural cover for `guardSingletonBuild` — the mechanism its source
// contract (`singleton-build-memo-contract.test.ts`) only asserts the ADOPTION
// of. A source matcher proves five modules call it; these prove that calling it
// does the thing, which is the half a text contract structurally cannot reach.
//
// Budgets here are deliberately tiny (single-digit ms) and every assertion is
// on STATE — did it reject, was the memo cleared, is this the stall error —
// never on elapsed time. The one timing fact any of them depends on is that a
// promise resolved on the microtask queue beats a `setTimeout`, which is an
// ordering guarantee of the event loop, not a race with the wall clock.

import { assert, assertEquals, assertInstanceOf } from "jsr:@std/assert";
import {
  guardSingletonBuild,
  SINGLETON_BUILD_DEADLINE_MS,
  SingletonBuildStalled,
} from "./singleton-build.ts";

Deno.test("a build that resolves is passed straight through, memo untouched", async () => {
  let cleared = false;
  const guarded = guardSingletonBuild(
    "ok",
    () => Promise.resolve("built"),
    () => (cleared = true),
    5,
  );
  assertEquals(await guarded, "built");
  // Give the deadline every chance to fire late if the timer were not cleared.
  await new Promise((r) => setTimeout(r, 20));
  assert(!cleared, "a successful build must not clear the memo");
});

Deno.test("a build that REJECTS still clears the memo and keeps its own error", async () => {
  let cleared = false;
  const boom = new Error("connect failed");
  const guarded = guardSingletonBuild(
    "rejects",
    () => Promise.reject(boom),
    () => (cleared = true),
    5_000,
  );
  const err = await guarded.then(() => null, (e) => e);
  // The original reason survives — the deadline must not mask a real failure
  // behind a stall report, or the page's error surface names the wrong thing.
  assertEquals(err, boom);
  assert(cleared, "a rejected build must clear the memo so a later mount rebuilds");
});

Deno.test("a build that NEVER SETTLES rejects at the deadline and clears the memo", async () => {
  let cleared = false;
  // The shape this whole helper exists for: a task that died mid-poll leaves a
  // promise with no terminal state at all, and every deadline inside it died
  // with it. Nothing here ever settles `never`.
  const never = new Promise<string>(() => {});
  const guarded = guardSingletonBuild("dead", () => never, () => (cleared = true), 5);
  const err = await guarded.then(() => null, (e) => e);
  assertInstanceOf(err, SingletonBuildStalled);
  assertEquals(err.label, "dead");
  assert(cleared, "a stalled build must clear the memo — this is the whole point");
  // The message has to point at the witness, or the next reader repeats the
  // three runs that looked everywhere but the pageerror.
  assert(err.message.includes("pageerror"), "the stall error names its witness");
});

Deno.test("the deadline does not fire on a build that settles first", async () => {
  let cleared = false;
  const slowish = new Promise<string>((r) => setTimeout(() => r("late"), 5));
  const guarded = guardSingletonBuild(
    "slow",
    () => slowish,
    () => (cleared = true),
    5_000,
  );
  assertEquals(await guarded, "late");
  assert(!cleared, "a build inside its budget is not a stall");
});

Deno.test("a build that throws SYNCHRONOUSLY rejects and clears like any other failure", async () => {
  // The build is a function now, and a plain (non-`async`) one can throw before
  // it ever returns a promise. That must land in the same terminal state as a
  // rejection — not escape `guardSingletonBuild` as an exception its caller
  // never expected, leaving the memo slot unassigned mid-statement.
  let cleared = false;
  const boom = new Error("threw before returning a promise");
  const guarded = guardSingletonBuild(
    "sync-throw",
    () => {
      throw boom;
    },
    () => (cleared = true),
    5_000,
  );
  const err = await guarded.then(() => null, (e) => e);
  assertEquals(err, boom);
  assert(cleared, "a synchronous throw must clear the memo too");
});

// ── Abandonment: the deadline gives up on a build it cannot stop ───────────
//
// The settle deadline rejects the AWAITER; it cannot stop the build, because a
// promise is not cancellable. So a pathologically slow build — not dead, just
// slow — is abandoned while still alive, and may resolve after a replacement
// build has already been installed. `stillWanted` is how the build finds out.

Deno.test("the build is handed stillWanted — true while it runs and after it settled in time", async () => {
  let stillWanted: (() => boolean) | null = null;
  const guarded = guardSingletonBuild(
    "wanted",
    (w) => {
      stillWanted = w;
      return Promise.resolve("built");
    },
    () => {},
    5,
  );
  // Handed synchronously, before the first await — a build captures it the way
  // it captures `sameActorSince()`, on the caller's synchronous path.
  assert(stillWanted !== null, "the guard must hand the build its predicate when it starts it");
  assert(stillWanted!(), "a build is wanted while it runs");
  assertEquals(await guarded, "built");
  // Past the budget: a build that settled in time is never abandoned later, so
  // a fire-and-forget tail it started may keep consulting the predicate.
  await new Promise((r) => setTimeout(r, 20));
  assert(stillWanted!(), "a build that settled inside its budget stays wanted");
});

Deno.test("the deadline ABANDONS the build — stillWanted() is false from then on", async () => {
  let stillWanted!: () => boolean;
  const guarded = guardSingletonBuild(
    "abandoned",
    (w) => {
      stillWanted = w;
      return new Promise<string>(() => {});
    },
    () => {},
    5,
  );
  const err = await guarded.then(() => null, (e) => e);
  assertInstanceOf(err, SingletonBuildStalled);
  assert(!stillWanted(), "a build the deadline gave up on must be told so");
});

Deno.test("an abandoned build that resolves LATE does not overwrite the replacement's slot", async () => {
  // The three-step interleaving, in the page managers' own shape: a module slot
  // the build assigns (`manager = built`), a memo holding the guarded promise,
  // and a clear that only empties the memo if it still holds THIS build.
  let slot: string | null = null;
  let memo: Promise<string> | null = null;
  function getManager(result: Promise<string>): Promise<string> {
    if (memo) return memo;
    const guarded = guardSingletonBuild(
      "page manager",
      async (stillWanted) => {
        const built = await result;
        if (!stillWanted()) throw new Error("page manager: build abandoned");
        slot = built;
        return built;
      },
      () => {
        if (memo === guarded) memo = null;
      },
      5,
    );
    memo = guarded;
    return guarded;
  }

  // 1. Build A stalls past its budget: the awaiter is rejected, the memo cleared.
  let resolveA!: (v: string) => void;
  const a = getManager(new Promise<string>((r) => (resolveA = r)));
  assertInstanceOf(await a.then(() => null, (e) => e), SingletonBuildStalled);
  assertEquals(memo, null, "the stalled build's memo is cleared");

  // 2. A later mount builds B, which resolves and fills the slot.
  const b = getManager(Promise.resolve("B"));
  assertEquals(await b, "B");
  assertEquals(slot, "B");

  // 3. A — still alive — finally resolves. A macrotask boundary drains every
  // continuation it queued: an event-loop ordering fact, not a wall-clock wait.
  resolveA("A");
  await new Promise((r) => setTimeout(r, 0));
  assertEquals(
    slot,
    "B",
    "the abandoned build overwrote the slot: the memo now vends B while the slot " +
      "holds A — the split-brain between the promise a page awaits and the object " +
      "its snapshot refreshers read",
  );
  assert(memo === b, "the memo still vends the replacement build");
});

Deno.test("a build abandoned AFTER installing is retracted with its memo, and the next call rebuilds", async () => {
  // The other order: the build installs, then dies in the tail that follows
  // (the feed manager's draft restore, the conversations manager's MLS passes).
  // `stillWanted` cannot help — the write already happened — so the memo clear
  // retracts it, inside its `=== guarded` branch, where nothing else can have
  // written the slot.
  let slot: string | null = null;
  let memo: Promise<string> | null = null;
  let builds = 0;
  function getManager(tail: Promise<void>): Promise<string> {
    if (memo) return memo;
    const name = `build ${++builds}`;
    const guarded = guardSingletonBuild(
      "page manager",
      async (stillWanted) => {
        const built = await Promise.resolve(name);
        if (!stillWanted()) throw new Error("page manager: build abandoned");
        slot = built;
        await tail;
        return built;
      },
      () => {
        if (memo !== guarded) return;
        memo = null;
        slot = null;
      },
      5,
    );
    memo = guarded;
    return guarded;
  }

  const dead = getManager(new Promise<void>(() => {}));
  assertInstanceOf(await dead.then(() => null, (e) => e), SingletonBuildStalled);
  assertEquals(
    slot,
    null,
    "the slot still holds the abandoned build — every reader keeps using a build " +
      "the page was told had failed, and a reader that rebuilds only on an empty " +
      "slot never rebuilds",
  );
  assertEquals(await getManager(Promise.resolve()), "build 2");
  assertEquals(slot, "build 2");
});

Deno.test("the default budget sits well above every bound it backstops", () => {
  // `ensureConnected`'s 15 s throw plus the 5 s `fauna.drafts.get` kind
  // deadline are the slowest awaits inside these builds. The guard is a
  // backstop for a DEAD task, so it may only fire when those cannot — a budget
  // at or under their sum would turn a merely-loaded box into a false stall.
  const INTERNAL_BOUNDS_MS = 15_000 + 5_000;
  assert(
    SINGLETON_BUILD_DEADLINE_MS > 2 * INTERNAL_BOUNDS_MS,
    `the settle deadline (${SINGLETON_BUILD_DEADLINE_MS}ms) must sit far above ` +
      `the ${INTERNAL_BOUNDS_MS}ms of internal bounds it backstops`,
  );
});
