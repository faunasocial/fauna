// Behavioural cover for the SPA's store-change relay: every notice reaches the
// pages open at that moment, the relay re-arms with the count it was handed,
// and it ends with the runtime. Assertions are on state and promise ordering,
// never on elapsed time.

import { assertEquals } from "jsr:@std/assert";
import { onStoreChange, relayStoreChanges } from "./store-change.ts";

/** A wasm face the test answers by hand, one wait at a time. */
function face() {
  const asked: number[] = [];
  let answer!: (count: number | undefined) => void;
  let waiting!: () => void;
  let armed = new Promise<void>((r) => (waiting = r));
  return {
    asked,
    changedAfter: (seen: number) => {
      asked.push(seen);
      waiting();
      return new Promise<number | undefined>((r) => (answer = r));
    },
    /** Answer the pending wait once the relay has armed it. */
    answer: async (count: number | undefined) => {
      await armed;
      armed = new Promise<void>((r) => (waiting = r));
      answer(count);
    },
    armed: () => armed,
  };
}

Deno.test("a notice re-runs every open page's load, and the relay re-arms with the count it was handed", async () => {
  const f = face();
  const ran: string[] = [];
  const closeA = onStoreChange(() => ran.push("a"));
  const closeB = onStoreChange(() => ran.push("b"));
  const faults: unknown[] = [];
  const relay = relayStoreChanges(f.changedAfter, (e) => faults.push(e));

  await f.answer(1);
  await f.armed();
  assertEquals(ran, ["a", "b"], "both open pages re-read on the notice");
  assertEquals(f.asked, [0, 1], "the first wait is from 0, the next from the count handed back");

  closeA();
  await f.answer(4);
  await f.armed();
  assertEquals(ran, ["a", "b", "b"], "a closed page is no longer re-driven");
  assertEquals(f.asked, [0, 1, 4]);

  await f.answer(undefined);
  await relay;
  assertEquals(faults, [], "the runtime stopping is not a fault");
  closeB();
});

Deno.test("a listener that throws does not starve the others, and a rejecting face ends the relay", async () => {
  const ran: string[] = [];
  const closeBad = onStoreChange(() => {
    throw new Error("page load blew up");
  });
  const closeGood = onStoreChange(() => ran.push("good"));
  const faults: string[] = [];
  let calls = 0;
  await relayStoreChanges(
    () => (++calls === 1 ? Promise.resolve(1) : Promise.reject(new Error("face fault"))),
    (e) => faults.push(String(e)),
  );
  assertEquals(ran, ["good"], "the page after the throwing one still re-read");
  assertEquals(faults, ["Error: page load blew up", "Error: face fault"]);
  closeBad();
  closeGood();
});
