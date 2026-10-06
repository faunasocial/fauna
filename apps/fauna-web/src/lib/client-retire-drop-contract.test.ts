// Retiring the WS-RPC client must run the app's canonical actor-scope drop —
// asserted as a general invariant over the source rather than as one more
// hand-picked outcome (testing.md convention 17). The fourth sibling of
// `singleton-build-memo-contract.test.ts`, `manager-gate-contract.test.ts` and
// `actor-scope-registration-contract.test.ts`, aimed at the same rebuild seam
// from the one side none of them covers: not a build that rejects, not a user
// action landing while the manager is null, not a handler registered too late —
// but the manager that was built perfectly well and whose TRANSPORT then died
// underneath it.
//
// ── The class ─────────────────────────────────────────────────────────────
//
// `rpc.ts::getClient` keys its singleton on `(actorId, nodeUrl())` and, when
// either changes, calls `retireClient(stale)` — which calls `stale.close()`.
// `WsRpcClient::close` latches a one-way `closed` flag: a closed client never
// reconnects, by construction (`libs/fauna-rpc-wasm/src/client.rs`).
//
// The four page managers do not merely *use* that client, they WRAP it —
// `c.feedManager(...)`, `c.searchManager()`, `c.eventDrafts(...)` construct
// shared-Rust managers generic over `WsRpcClient` and hold the instance they
// were built from. So the moment a retire happens, every manager built over
// that client is holding a permanently dead transport.
//
// And `getFeedManager()` keys on NOTHING. It memoizes one promise and returns
// it to every later caller, so without a drop at the retire it keeps vending
// the dead-transport manager for the rest of the page's life.
//
// ── Why the drop and not a key ─────────────────────────────────────────────
//
// The tempting fix is to give each manager the same `(actorId, nodeUrl())` key
// `getClient` has. That is four copies of one rule — the per-surface divergence
// priorities #1/#2 exist to refuse, and the same argument that put the settle
// deadline in `singleton-build.ts` rather than in four private watchdogs. It is
// also indirect: a manager dies because its client was retired, not because a
// URL string changed, and a key re-derives that fact through a proxy that would
// miss any other reason a client is ever retired.
//
// `account-scoping.md` § The scoping taxonomy already states the rule this
// contract enforces, in these words: there is **exactly one canonical drop per
// app, and every teardown site calls it with no list of its own**. A client
// retire is a teardown site. Until 2026-09-09 it was the one teardown site that
// called nothing.
//
// ── Why the actor half does not cover it ───────────────────────────────────
//
// `store.ts` fires `resetActorScopedState()` from a subscription to the
// identity store, keyed on `secretHex` — and early-returns when it is
// unchanged. A nest change is not an identity change: the same actor, same
// secret, different home nest. So on the nest half of the key, nothing
// anywhere else fires. That is the whole reason this call exists, and the whole
// reason this contract is worth a file.
//
// The journey that reaches it is client-side end to end, with module state
// surviving throughout: `admin-nest`'s factory reset →
// `accountsClearNestBinding()` → `goto('/app/onboarding')` → onboarding's
// `LoggedIn` exit records the new home nest → `goto('/app/feed')`. The
// admin-nest route's own comment says the actor-scoped reset registry does not
// fire on that path.
//
// Deliberately crude, like its siblings: it reads source as TEXT, so it stays
// honest about what a reader would see. A determined evasion slips it; the
// contract is aimed at the shape that actually recurs — a teardown site that
// forgets the drop.

import { stripComments } from "./source-contract.ts";

const RPC = new URL("./rpc.ts", import.meta.url);

/** The canonical drop, as `actorScope.ts` exports it. */
const DROP = "resetActorScopedState";

function rpcSource(): string {
  return stripComments(Deno.readTextFileSync(RPC));
}

/** `getClient`'s body, from its declaration to the next top-level declaration.
 *  Crude on purpose (see the header): the point is what a reader sees in that
 *  function, not a parse. */
function getClientBody(src: string): string {
  const start = src.indexOf("async function getClient(");
  if (start < 0) {
    throw new Error(
      "rpc.ts no longer declares `async function getClient(` — this contract's " +
        "anchor is gone; re-point it at whatever now owns the singleton key.",
    );
  }
  const rest = src.slice(start + 1);
  const end = rest.search(/\n(?:export )?(?:async )?function /);
  return end < 0 ? rest : rest.slice(0, end);
}

Deno.test("rpc.ts imports the canonical actor-scope drop", () => {
  const src = rpcSource();
  if (!new RegExp(`import\\s*\\{[^}]*\\b${DROP}\\b[^}]*\\}\\s*from\\s*'\\./actorScope'`).test(src)) {
    throw new Error(
      `rpc.ts must import ${DROP} from './actorScope' — the client retire is a ` +
        `teardown site and account-scoping.md § The scoping taxonomy says every ` +
        `teardown site calls the one canonical drop.`,
    );
  }
});

Deno.test("the client-key retire runs the canonical drop", () => {
  const body = getClientBody(rpcSource());
  if (!body.includes("retireClient(client)")) {
    throw new Error(
      "getClient no longer retires the superseded client — if the singleton " +
        "lifecycle moved, re-point this contract at its new home rather than " +
        "deleting it.",
    );
  }
  if (!body.includes(`${DROP}()`)) {
    throw new Error(
      `getClient retires the superseded client but never calls ${DROP}(). ` +
        `Everything built over that client wraps it, and close() is one-way, so ` +
        `every memoized manager is left holding a dead transport — and ` +
        `getFeedManager() keys on nothing, so it vends that manager forever. ` +
        `The nest half of the key has no other trigger: store.ts early-returns ` +
        `on an unchanged secretHex.`,
    );
  }
});

Deno.test("the drop runs where the KEY changed, not inside retireClient", () => {
  const src = rpcSource();
  const retire = src.indexOf("function retireClient(");
  if (retire < 0) throw new Error("rpc.ts no longer declares retireClient");
  const rest = src.slice(retire);
  const end = rest.search(/\n(?:export )?(?:async )?function /);
  const retireBody = end < 0 ? rest : rest.slice(0, end);
  if (retireBody.includes(`${DROP}(`)) {
    throw new Error(
      `retireClient must NOT call ${DROP} itself: it also runs on the supersede ` +
        `arms, where the retired instance is a freshly-built client no manager ` +
        `was ever built over — dropping there would throw away the live managers ` +
        `of the client that WON. The drop belongs at the key-change site.`,
    );
  }
});

Deno.test("the drop is ordered after the retire, not before it", () => {
  const body = getClientBody(rpcSource());
  const retire = body.indexOf("retireClient(client)");
  const drop = body.indexOf(`${DROP}()`);
  if (retire < 0 || drop < 0) return; // the tests above already report this
  if (drop < retire) {
    throw new Error(
      "the canonical drop runs BEFORE retireClient(client). Drop after the " +
        "retire: a drop that lands first leaves a window in which a rebuild can " +
        "be handed the still-live stale client.",
    );
  }
});
