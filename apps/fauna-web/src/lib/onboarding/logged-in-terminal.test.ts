// Deno tests for the onboarding `LoggedIn` terminal's tail ordering. Run via:
//
//     just web-unit-test
//
// The pin is the restore-leg ordering `identity-succession.md` § Seed escrow →
// *Restore path* requires of every app: a phrase-only restore's predecessor
// seeds reach the account registry BEFORE the restored session is built. Web's
// session resolves them once in places — the conversations manager hands
// `predecessor_backup_keys` to the `__mls` replica at construction
// (`libs/fauna-wasm/src/conversations.rs`), and a replica load that misses
// them fails permanently for the session — so entering the app ahead of the
// persist left the restored session's conversations dark until a reload.
import { assert, assertEquals } from "jsr:@std/assert";
import { runLoggedInTerminal } from "./logged-in-terminal.ts";

function deferred(): { promise: Promise<void>; resolve: () => void; reject: (e: unknown) => void } {
  let resolve!: () => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<void>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

Deno.test("the app is entered only once the restored predecessors have landed", async () => {
  const persist = deferred();
  const order: string[] = [];
  const done = runLoggedInTerminal({
    register: () => {
      order.push("register");
      return Promise.resolve();
    },
    persistRestoredPredecessors: () => {
      order.push("persist");
      return persist.promise;
    },
    handoffs: () => order.push("handoffs"),
    enterApp: () => order.push("enter"),
    onFailure: () => order.push("failure"),
  });
  // Drain every queued turn: nothing but a landed persist may release entry.
  for (let i = 0; i < 20; i++) await Promise.resolve();
  await new Promise((r) => setTimeout(r, 0));
  assertEquals(order, ["register", "persist"]);
  persist.resolve();
  await done;
  assertEquals(order, ["register", "persist", "handoffs", "enter"]);
});

Deno.test("a failed predecessor persist is reported and never strands the user", async () => {
  const failures: string[] = [];
  let entered = false;
  await runLoggedInTerminal({
    register: () => Promise.resolve(),
    persistRestoredPredecessors: () => Promise.reject(new Error("quota")),
    handoffs: () => {},
    enterApp: () => {
      entered = true;
    },
    onFailure: (step, e) => failures.push(`${step}: ${(e as Error).message}`),
  });
  assert(entered);
  assertEquals(failures, ["restored predecessors: quota"]);
});

Deno.test("a failed registry write still persists the predecessors and enters the app", async () => {
  const order: string[] = [];
  await runLoggedInTerminal({
    register: () => Promise.reject(new Error("locked")),
    persistRestoredPredecessors: () => {
      order.push("persist");
      return Promise.resolve();
    },
    handoffs: () => order.push("handoffs"),
    enterApp: () => order.push("enter"),
    onFailure: (step) => order.push(`failure:${step}`),
  });
  assertEquals(order, ["failure:register", "persist", "handoffs", "enter"]);
});

Deno.test("an added restore lands its predecessors between the add and the switch", async () => {
  const persist = deferred();
  const order: string[] = [];
  const done = runLoggedInTerminal({
    register: () => {
      order.push("add");
      return Promise.resolve();
    },
    persistRestoredPredecessors: () => {
      order.push("persist");
      return persist.promise;
    },
    activate: () => {
      order.push("switch");
      return Promise.resolve();
    },
    handoffs: () => order.push("handoffs"),
    enterApp: () => order.push("enter"),
    onFailure: () => order.push("failure"),
  });
  await new Promise((r) => setTimeout(r, 0));
  // The switch is what makes the added identity active and builds its session.
  assertEquals(order, ["add", "persist"]);
  persist.resolve();
  await done;
  assertEquals(order, ["add", "persist", "switch", "handoffs", "enter"]);
});

Deno.test("a failed add still lands the predecessors but switches to nothing", async () => {
  const order: string[] = [];
  await runLoggedInTerminal({
    register: () => Promise.reject(new Error("exists")),
    persistRestoredPredecessors: () => {
      order.push("persist");
      return Promise.resolve();
    },
    activate: () => {
      order.push("switch");
      return Promise.resolve();
    },
    handoffs: () => order.push("handoffs"),
    enterApp: () => order.push("enter"),
    onFailure: (step) => order.push(`failure:${step}`),
  });
  assertEquals(order, ["failure:register", "persist", "handoffs", "enter"]);
});
