// `getClient`'s supersede arm: a waiter whose build was superseded by ANOTHER
// actor's fails rather than taking the singleton back (`client-supersede.ts`).
// The model below is getClient's slot reduced to what the livelock needs — an
// actor key, a generation, and an await between the build and the check — and
// it drives the measured shape: two late calls for two actors, interleaved.
// Run with `just web-unit-test`.

import { assert, assertEquals, assertThrows } from "jsr:@std/assert";
import { ClientSupersededError, supersededWaiterNext } from "./client-supersede.ts";

type Next = (wanted: string, current: string | null) => "rejoin" | "refuse";

/** Two callers, each one `getClient` call, resumed alternately after every
 *  await (the interleaving the wizard's two late reads produced). Returns how
 *  many builds ran and how each call ended; gives up at `cap` builds. */
function drive(next: Next, callers: string[], cap = 50) {
  const slot = { actor: null as string | null, gen: 0 };
  let builds = 0;
  const outcome = new Map<string, string>();
  // Each caller: its awaited generation, or null once settled.
  const waiting = new Map<string, number | null>();
  const enter = (actor: string) => {
    if (slot.actor !== actor) {
      slot.actor = actor;
      slot.gen += 1;
      builds += 1;
    }
    waiting.set(actor, slot.gen);
  };
  for (const a of callers) enter(a);
  while (builds < cap) {
    let progressed = false;
    for (const a of callers) {
      const mine = waiting.get(a);
      if (mine === null || mine === undefined) continue;
      progressed = true;
      if (mine === slot.gen) {
        outcome.set(a, "client");
        waiting.set(a, null);
      } else if (next(a, slot.actor) === "rejoin") {
        enter(a);
      } else {
        outcome.set(a, "superseded");
        waiting.set(a, null);
      }
    }
    if (!progressed) break;
  }
  return { builds, outcome };
}

Deno.test("two late calls for two actors settle — the stale one is refused", () => {
  const { builds, outcome } = drive(supersededWaiterNext, ["A", "B"]);
  assert(builds < 5, `the slot ping-ponged: ${builds} builds`);
  assertEquals(outcome.get("B"), "client");
  assertEquals(outcome.get("A"), "superseded");
});

Deno.test("the pre-fix rule (always rejoin) ping-pongs until the cap — the measured storm", () => {
  const { builds, outcome } = drive(() => "rejoin", ["A", "B"]);
  assertEquals(builds, 50);
  assertEquals(outcome.size, 0);
});

Deno.test("a waiter superseded by its own actor's rebuild rejoins it", () => {
  assertEquals(supersededWaiterNext("A", "A"), "rejoin");
});

Deno.test("a waiter whose slot was emptied (retireRpcClient) rejoins", () => {
  assertEquals(supersededWaiterNext("A", null), "rejoin");
});

Deno.test("the refusal names both actors and is typed", () => {
  assertThrows(
    () => {
      throw new ClientSupersededError("aaaaaaaaaaaa", "bbbbbbbbbbbb");
    },
    ClientSupersededError,
    "wanted actor aaaaaaaa, the singleton now serves bbbbbbbb",
  );
});

Deno.test("getClient's supersede arm consults the rule (source contract)", async () => {
  const src = await Deno.readTextFile(new URL("./rpc.ts", import.meta.url));
  const arm = src.indexOf("if (clientPromise !== mine) {");
  assert(arm > 0, "getClient's supersede arm is gone or reshaped — re-point this contract");
  const body = src.slice(arm, arm + 400);
  assert(
    body.includes("supersededWaiterNext(actorId, clientActorId)") &&
      body.includes("throw new ClientSupersededError"),
    "the supersede arm must refuse a waiter for an actor the singleton moved past",
  );
  assert(
    !/if \(clientPromise !== mine\) return getClient\(/.test(src),
    "an unconditional re-entry on supersede is the two-actor ping-pong",
  );
});
