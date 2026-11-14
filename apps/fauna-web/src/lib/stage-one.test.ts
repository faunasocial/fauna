// Behavioural cover for `StageOneSlot` — the keep/retire rule of web's
// two-stage conversations build (`account-client-lifecycle.md` § The
// client-side lifecycle → *The trigger fired*, decision (k)). Each test drives
// the slot the way `getConversationsManager` does: a stage one that constructs
// a "manager" and counts the runtime starts it asks for, then a connection
// wait, under the shared settle-deadline guard. Assertions are on state (which
// manager, how many runtime starts), never on elapsed time.

import { assert, assertEquals, assertRejects, assertStrictEquals } from "jsr:@std/assert";
import { StageOneSlot, type StageOneEntry } from "./stage-one.ts";
import { guardSingletonBuild } from "./singleton-build.ts";

type Manager = { id: number };

/** A tab: the slot, a counter of constructions and runtime starts, and one
 *  build attempt shaped like the conversations build. */
function tab() {
  const slot = new StageOneSlot<Manager>();
  let constructed = 0;
  let runtimeStarts = 0;
  const stageOne = async (entry: StageOneEntry<Manager>): Promise<Manager> => {
    await Promise.resolve();
    const m = { id: ++constructed };
    if (!slot.holds(entry)) throw new Error("dropped");
    entry.built = m;
    runtimeStarts += 1;
    return m;
  };
  /** One attempt: stage one, the wait (`nest` decides), then the tail
   *  (`tail` may throw — a failure after the wait). */
  const attempt = (nest: "up" | "down", tail: () => void = () => {}) => {
    let pastTheWait = false;
    let own: StageOneEntry<Manager> | null = null;
    return guardSingletonBuild("test build", async () => {
      own = slot.enter("actor", stageOne);
      const built = await own.promise;
      await Promise.resolve();
      if (nest === "down") throw new Error("WS-RPC client failed to connect within 15s");
      pastTheWait = true;
      tail();
      return built;
    }, () => {
      if (pastTheWait) slot.retire(own);
    });
  };
  return {
    slot,
    attempt,
    constructed: () => constructed,
    runtimeStarts: () => runtimeStarts,
  };
}

Deno.test("a connection-wait failure keeps stage one; the retry reuses the same manager and asks for no second runtime start", async () => {
  const t = tab();
  await assertRejects(() => t.attempt("down"), Error, "failed to connect");
  const first = t.slot.built();
  assert(first !== null, "stage one stands after a failure at the wait — the runtime is hosted offline");
  assertEquals(t.runtimeStarts(), 1);

  await assertRejects(() => t.attempt("down"), Error, "failed to connect");
  const vended = await t.attempt("up");
  assertStrictEquals(vended, first, "the build that reached the nest vends stage one's manager");
  assertEquals(t.constructed(), 1, "no second engine was built in the tab");
  assertEquals(t.runtimeStarts(), 1, "no second runtime start was asked for");
});

Deno.test("a failure after the connection wait retires stage one — the next attempt builds anew", async () => {
  const t = tab();
  await assertRejects(
    () => t.attempt("up", () => { throw new Error("tail failed"); }),
    Error,
    "tail failed",
  );
  assertEquals(t.slot.built(), null, "the whole build is retired");
  const next = await t.attempt("up");
  assertEquals(next.id, 2, "a fresh manager");
  assertEquals(t.constructed(), 2);
});

Deno.test("stage one's own failure drops it — the next attempt builds anew", async () => {
  const slot = new StageOneSlot<Manager>();
  let runs = 0;
  const failing = slot.enter("actor", async () => {
    runs += 1;
    await Promise.resolve();
    throw new Error("role refused");
  });
  await assertRejects(() => failing.promise, Error, "role refused");
  assertEquals(slot.built(), null);
  const next = slot.enter("actor", async (entry) => {
    runs += 1;
    entry.built = { id: 7 };
    return entry.built;
  });
  assert(next !== failing, "a fresh entry, not the failed one served again");
  assertEquals((await next.promise).id, 7);
  assertEquals(runs, 2);
});

Deno.test("a cleared slot (the actor switch) never takes the departing run's product", async () => {
  const slot = new StageOneSlot<Manager>();
  let release!: () => void;
  const gate = new Promise<void>((r) => (release = r));
  const departing = slot.enter("actor-a", async (entry) => {
    await gate;
    if (!slot.holds(entry)) throw new Error("superseded");
    entry.built = { id: 1 };
    return entry.built;
  });
  slot.clear();
  release();
  await assertRejects(() => departing.promise, Error, "superseded");
  assertEquals(slot.built(), null);
});

Deno.test("an entry for another account is not reused", async () => {
  const slot = new StageOneSlot<Manager>();
  const a = slot.enter("actor-a", async (e) => (e.built = { id: 1 }));
  await a.promise;
  const b = slot.enter("actor-b", async (e) => (e.built = { id: 2 }));
  assert(a !== b);
  assertEquals((await b.promise).id, 2);
});
