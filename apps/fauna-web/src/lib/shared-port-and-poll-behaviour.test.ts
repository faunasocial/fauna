// The behavioural half of `shared-rpc-port-contract.test.ts`: that file reads
// source as text and so cannot tell a live clause from a decorative mention of
// its operands (the verify-back's M4, S2 and S3 each survived it). These tests
// run the two seams it guards — `portIsStale`, the shared rpc port's refusal, and
// `createActorScopedPoll`, the page machines' actor-scoped 15 s poll — as code.

import {
  createActorScopedPoll,
  portIsStale,
  registeredActorScopedResetCount,
  resetActorScopedState,
  sameActorSince,
} from "./actorScope.ts";

function eq<T>(actual: T, expected: T, msg: string) {
  if (actual !== expected) {
    throw new Error(`${msg}: got ${actual}, want ${expected}`);
  }
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

// ── portIsStale ───────────────────────────────────────────────────────────────
// Each row names the arm it pins; deleting either clause of the predicate
// reddens a row.

Deno.test("portIsStale — the current port is not stale", () => {
  eq(portIsStale("a", "a", "a"), false, "singleton and identity both on the port actor");
  eq(portIsStale("a", null, "a"), false, "fresh singleton, identity on the port actor");
});

Deno.test("portIsStale — the singleton moved to another actor", () => {
  eq(portIsStale("a", "b", "a"), true, "singleton clause alone");
  eq(portIsStale("a", "b", "b"), true, "singleton and identity both moved");
});

Deno.test("portIsStale — the post-retire window: singleton null, identity moved", () => {
  // `retireRpcClient` nulls `clientActorId`; only the identity clause stops a
  // stale port dialling the retired actor here.
  eq(portIsStale("a", null, "b"), true, "identity clause alone");
});

Deno.test("portIsStale — the identity moved while the singleton still matches the port", () => {
  eq(portIsStale("a", "a", "b"), true, "identity clause with a matching singleton");
});

Deno.test("portIsStale — a signed-out store does not make a port stale", () => {
  // Decided: sign-out runs `resetActorScopedState`, which drops every
  // registered machine holding a port, and null is also the boot state.
  eq(portIsStale("a", "a", null), false, "null identity, matching singleton");
  eq(portIsStale("a", null, null), false, "null identity, null singleton (boot)");
});

// ── createActorScopedPoll ─────────────────────────────────────────────────────

Deno.test("createActorScopedPoll — start ticks on the interval, stop ends it", async () => {
  const poll = createActorScopedPoll(5);
  let ticks = 0;
  poll.start(() => true, () => {
    ticks += 1;
  });
  await sleep(40);
  if (ticks < 2) throw new Error(`poll never ticked: ${ticks}`);
  poll.stop();
  const at = ticks;
  await sleep(30);
  eq(ticks, at, "no tick after stop");
  poll.dispose();
});

Deno.test("createActorScopedPoll — start refuses when the actor changed during the mount awaits", async () => {
  const poll = createActorScopedPoll(5);
  const sameActor = sameActorSince();
  resetActorScopedState(); // an identity change landing mid-mount
  let ticks = 0;
  poll.start(sameActor, () => {
    ticks += 1;
  });
  await sleep(30);
  eq(ticks, 0, "a poll started for the outgoing actor never ticks");
  poll.dispose();
});

Deno.test("createActorScopedPoll — the canonical drop stops a running poll", async () => {
  const poll = createActorScopedPoll(5);
  let ticks = 0;
  poll.start(sameActorSince(), () => {
    ticks += 1;
  });
  await sleep(30);
  resetActorScopedState();
  const at = ticks;
  await sleep(30);
  eq(ticks, at, "no tick after the drop");
  poll.dispose();
});

Deno.test("createActorScopedPoll — dispose stops the poll and unregisters the drop", async () => {
  const before = registeredActorScopedResetCount();
  const poll = createActorScopedPoll(5);
  eq(registeredActorScopedResetCount(), before + 1, "registered with the actor-scoped reset");
  let ticks = 0;
  poll.start(sameActorSince(), () => {
    ticks += 1;
  });
  await sleep(20);
  poll.dispose();
  const at = ticks;
  await sleep(30);
  eq(ticks, at, "no tick after dispose");
  eq(registeredActorScopedResetCount(), before, "unregistered");
});
