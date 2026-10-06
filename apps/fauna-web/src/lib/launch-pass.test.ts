// Behavioural cover for `runAtLaterEdge` — when web's launch folder pass runs
// relative to this tab's account runtime. Assertions are on STATE (did the pass
// run, did the caller get control back) and on promise ordering, never on
// elapsed time.

import { assert, assertEquals } from "jsr:@std/assert";
import { runAtLaterEdge } from "./launch-pass.ts";

/** A runtime start the test settles by hand. */
function runtime() {
  let started = false;
  let settle!: () => void;
  const settled = new Promise<void>((r) => (settle = r));
  return {
    started: () => started,
    settled: () => settled,
    start: () => {
      started = true;
      settle();
    },
    fail: () => settle(),
  };
}

Deno.test("a runtime already started runs the pass in line — the caller waits for it", async () => {
  const rt = runtime();
  rt.start();
  const ran: string[] = [];
  await runAtLaterEdge({
    started: rt.started,
    settled: rt.settled,
    stillWanted: () => true,
    pass: async () => {
      await Promise.resolve();
      ran.push("pass");
    },
    skipped: (why) => ran.push(`skipped: ${why}`),
  });
  assertEquals(ran, ["pass"], "the pass finished before the caller moved on");
});

Deno.test("a runtime still starting does not hold the caller, and the pass runs once it has started", async () => {
  const rt = runtime();
  const ran: string[] = [];
  await runAtLaterEdge({
    started: rt.started,
    settled: rt.settled,
    stillWanted: () => true,
    pass: () => {
      ran.push("pass");
      return Promise.resolve();
    },
    skipped: (why) => ran.push(`skipped: ${why}`),
  });
  // The caller has control back — the manager build is behind this await —
  // and nothing ran over the unreadable custody.
  assertEquals(ran, []);
  rt.start();
  await new Promise((r) => setTimeout(r, 0));
  assertEquals(ran, ["pass"], "the pass ran at the runtime's edge, not at the next launch");
});

Deno.test("a runtime that fails to start skips the pass and says so", async () => {
  const rt = runtime();
  const ran: string[] = [];
  await runAtLaterEdge({
    started: rt.started,
    settled: rt.settled,
    stillWanted: () => true,
    pass: () => {
      ran.push("pass");
      return Promise.resolve();
    },
    skipped: (why) => ran.push(`skipped: ${why}`),
  });
  rt.fail();
  await new Promise((r) => setTimeout(r, 0));
  assertEquals(ran.length, 1);
  assert(ran[0].startsWith("skipped: "), "a pass that cannot read custody is reported, never silent");
});

Deno.test("a build superseded while the runtime started never runs the pass", async () => {
  const rt = runtime();
  const ran: string[] = [];
  let wanted = true;
  await runAtLaterEdge({
    started: rt.started,
    settled: rt.settled,
    stillWanted: () => wanted,
    pass: () => {
      ran.push("pass");
      return Promise.resolve();
    },
    skipped: (why) => ran.push(`skipped: ${why}`),
  });
  // An actor switch or the settle deadline lands inside the wait: the engine
  // this pass would write through is no longer this tab's.
  wanted = false;
  rt.start();
  await new Promise((r) => setTimeout(r, 0));
  assertEquals(ran, [], "neither run nor reported — the build's own supersede path speaks");
});
