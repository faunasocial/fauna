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
    beginSession: () => order.push("begin"),
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
  assertEquals(order, ["register", "persist", "begin", "handoffs", "enter"]);
});

Deno.test("a failed predecessor persist is reported and never strands the user", async () => {
  const failures: string[] = [];
  let entered = false;
  await runLoggedInTerminal({
    register: () => Promise.resolve(),
    persistRestoredPredecessors: () => Promise.reject(new Error("quota")),
    beginSession: () => {},
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
    beginSession: () => order.push("begin"),
    handoffs: () => order.push("handoffs"),
    enterApp: () => order.push("enter"),
    onFailure: (step) => order.push(`failure:${step}`),
  });
  assertEquals(order, ["failure:register", "persist", "begin", "handoffs", "enter"]);
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
    beginSession: () => order.push("begin"),
    handoffs: () => order.push("handoffs"),
    enterApp: () => order.push("enter"),
    onFailure: () => order.push("failure"),
  });
  await new Promise((r) => setTimeout(r, 0));
  // The switch is what makes the added identity active and builds its session.
  assertEquals(order, ["add", "persist"]);
  persist.resolve();
  await done;
  assertEquals(order, ["add", "persist", "begin", "switch", "handoffs", "enter"]);
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
    beginSession: () => order.push("begin"),
    handoffs: () => order.push("handoffs"),
    enterApp: () => order.push("enter"),
    onFailure: (step) => order.push(`failure:${step}`),
  });
  assertEquals(order, ["failure:register", "persist", "begin", "handoffs", "enter"]);
});

// The session a claim starts runs over the account the claim created, never
// over a client built before it: an identity committed ahead of its account
// can have had its WS-RPC client stopped for good by the box it then claimed
// (`not_registered`), and the hand-offs that follow dial through that client.
Deno.test("the session begins after the registry write and before the hand-offs and entry", async () => {
  const order: string[] = [];
  await runLoggedInTerminal({
    register: () => {
      order.push("register");
      return Promise.resolve();
    },
    persistRestoredPredecessors: () => {
      order.push("persist");
      return Promise.resolve();
    },
    beginSession: () => order.push("begin"),
    handoffs: () => order.push("handoffs"),
    enterApp: () => order.push("enter"),
    onFailure: (step) => order.push(`failure:${step}`),
  });
  // After the persist too: the delivered handlers include the succession
  // aftermath, which reads the predecessors the persist just landed.
  assertEquals(order, ["register", "persist", "begin", "handoffs", "enter"]);
});

Deno.test("a failed session begin is reported and never strands the user", async () => {
  const order: string[] = [];
  await runLoggedInTerminal({
    register: () => Promise.resolve(),
    persistRestoredPredecessors: () => Promise.resolve(),
    beginSession: () => {
      throw new Error("retire");
    },
    handoffs: () => order.push("handoffs"),
    enterApp: () => order.push("enter"),
    onFailure: (step, e) => order.push(`failure:${step}: ${(e as Error).message}`),
  });
  assertEquals(order, ["failure:begin session: retire", "handoffs", "enter"]);
});
