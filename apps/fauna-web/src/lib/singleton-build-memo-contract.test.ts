// A FAILED singleton build must not be memoized — asserted as a general
// invariant over the source rather than as one more hand-picked outcome
// (testing.md convention 17). Sibling of `manager-gate-contract.test.ts`, and
// aimed at the same seam from the other side: that one covers a user action
// landing *while* the manager is null, this one covers the manager that never
// arrives at all.
//
// ── The class ─────────────────────────────────────────────────────────────
//
// Every actor-scoped singleton in the SPA is built by the same four-line shape
// (the build was a bare async IIFE when this contract was written; it is now
// handed to the settle-deadline guard the second section asserts):
//
//     if (xPromise) return xPromise;          // the memo
//     ...
//     xPromise = guardSingletonBuild('x', async (stillWanted) => { ...await... }, clear);
//     return xPromise;
//
// A promise memo caches a REJECTION exactly as durably as a value. So if the
// build throws once — `ensureWasm()` losing a race, the WS-RPC connect failing,
// the nest answering 500 while the box is loaded — every later caller is handed
// that same rejected promise for the rest of the page's life. The singleton is
// not "slow to build"; it is permanently unbuildable, and nothing retries.
//
// `getConversationsManager` already writes the remedy down at its engine-role
// refusal: "Clear the memo so a later attempt (the receive poll's next tick, or
// a reload after the holding tab closes) can win the role instead of being
// served this same rejection forever." That reasoning is not special to the
// role lock — it is the reasoning for every failure a build can have. The other
// three builders had no such clear at all.
//
// ── What it cost ──────────────────────────────────────────────────────────
//
// The feed manager's memo. Every `test_feed.py` journey
// logs in as the SAME actor, so `agent.js` skips its reset and `store.ts`'s
// identity subscription early-returns on the unchanged `secretHex` — which
// means nothing inside a test module ever re-fires the actor-change handlers,
// and the feed route's remount is served the memo directly. One transient build
// failure under load therefore did not fail ONE journey: it failed every feed
// journey after it in the module, identically, until the next module-boundary
// relaunch. Observed as 3–4 of 22 web journeys red per run, standalone and
// docker alike, each with `post-submit-button` disabled for the full 90 s
// ceiling and an EMPTY `error-message` — because the page's `onActorChange`
// handler awaited the memo without a catch and `store.ts` voids that promise.
//
// ── The rule ──────────────────────────────────────────────────────────────
//
// A module that memoizes an async build must contain, somewhere, a clear of
// that memo on the failure path. Stated as a REMEDY requirement rather than one
// spelling, for the same reason its sibling is: the shapes differ (a `.catch`
// hung off the promise, a `null` assignment inside the IIFE's own catch), and
// pinning one spelling is how the compound null-manager guards walked straight
// past the first manager-gate contract.
//
// Deliberately crude, like its sibling: it reads source as TEXT, so it stays
// honest about what a reader would see. A determined evasion slips it; the
// contract is aimed at the shape that actually recurs — a memo written once and
// never cleared — not at adversaries.

import { stripComments } from "./source-contract.ts";

/** The SPA's memoized actor-scoped async builds: module → the memo variable it
 *  caches its in-flight build in, the module-level `slots` its build assigns,
 *  and the one slot holding the build's PRODUCT, which the memo clear
 *  `retract`s (both the third section below). Every entry here is registered as
 *  an actor-scoped drop (`actorScope.ts`), which is what makes the singleton
 *  account-scoped in the first place. */
const MEMOS: Record<
  string,
  { url: URL; memo: string; slots: string[]; retract: string | null }
> = {
  feed: {
    url: new URL("./feed.ts", import.meta.url),
    memo: "managerPromise",
    slots: ["manager"],
    retract: "manager",
  },
  conversations: {
    url: new URL("./conversations.ts", import.meta.url),
    memo: "managerPromise",
    // `engineRole` too: the MLS-writing role is module state this build
    // acquires across an await, exactly like the manager itself. It is not
    // RETRACTED, though — it is the account's, and the replacement writes
    // under it.
    slots: ["engineRole", "manager"],
    retract: "manager",
  },
  search: {
    url: new URL("./search.ts", import.meta.url),
    memo: "managerPromise",
    slots: ["manager"],
    retract: "manager",
  },
  "event-drafts": {
    url: new URL("./event-drafts.ts", import.meta.url),
    memo: "facePromise",
    // `current` is checked but not retracted: it is the user's live draft,
    // written by every keystroke, not the build's product.
    slots: ["face", "current"],
    retract: "face",
  },
  // The WS-RPC client singleton — the FIFTH memo of this class, and the one the
  // other four are built OVER: `feedManager` / `conversationsManager` /
  // `searchManager` / `eventDrafts` all reach it through `call()` →
  // `getClient()`. The census above originally stopped at the page managers,
  // which left the ROOT of the dependency chain uncovered — and a rejected
  // `createWsRpcClient` is not one rail's problem but every page's, for the rest
  // of the page's life. Its (actor, nest URL) key forces a rebuild only when one
  // of those CHANGES, so the same-actor re-entry this whole contract exists for
  // is precisely the case the key does not cover.
  //
  // No `slots`: its build assigns no module slot — `client = built` happens in
  // `getClient` after the await, behind its own supersede check. What an
  // abandoned client build must not do is REGISTER as live; the third section
  // asserts that separately, because its remedy is a retire, not a skipped write.
  rpc: {
    url: new URL("./rpc.ts", import.meta.url),
    memo: "clientPromise",
    slots: [],
    retract: null,
  },
};

function read(url: URL): string {
  return stripComments(Deno.readTextFileSync(url));
}

/** Does `src` clear `memo` anywhere that is not the actor-scoped drop?
 *
 *  The drop (`resetFeedManager` and friends) clears the memo too, but it only
 *  ever runs on an identity CHANGE — which is exactly the event that does not
 *  happen when the same actor logs in again, and therefore cannot be the
 *  failure-path clear this contract is about. So the reset function's own body
 *  is excluded before counting.
 */
function clearsMemoOffTheResetPath(src: string, memo: string): boolean {
  // Drop each `export function reset*(): void { ... }` body — the drops are all
  // written in that one shape, and they are the assignments we must not count.
  const withoutResets = src.replace(
    /export function reset\w*\(\)\s*:\s*void\s*\{[\s\S]*?\n\}/g,
    "",
  );
  return new RegExp(`\\b${memo}\\s*=\\s*null\\b`).test(withoutResets);
}

for (const [name, { url, memo }] of Object.entries(MEMOS)) {
  Deno.test(
    `singleton build memo — ${name} clears \`${memo}\` on a failed build`,
    () => {
      const src = read(url);

      // Guard the guard: if the memo variable is renamed, this contract must
      // fail loudly rather than silently pass over a file it no longer matches.
      if (!new RegExp(`\\b${memo}\\b`).test(src)) {
        throw new Error(
          `${name}: no \`${memo}\` in the source — this contract's MEMOS table ` +
            `is stale. Point it at the memo variable's new name; do not delete ` +
            `the entry unless the module genuinely stopped memoizing a build.`,
        );
      }

      if (!clearsMemoOffTheResetPath(src, memo)) {
        throw new Error(
          `${name}: \`${memo}\` is assigned a rejected promise and never cleared ` +
            `outside the actor-scoped drop, so ONE failed build is served to every ` +
            `later caller for the rest of the page's life — the drop cannot save ` +
            `it, because the drop runs on an identity CHANGE and the commonest ` +
            `re-entry is the SAME actor mounting the route again. Clear the memo ` +
            `on the failure path (guarded by \`=== \` the promise you installed, ` +
            `so a newer build already in the slot is not thrown away) — see ` +
            `\`feed.ts\`'s \`getFeedManager\` for the shape.`,
        );
      }
    },
  );
}

// ── The second half: a build that never SETTLES ────────────────────────────
//
// The memo rule above covers the build that REJECTS. It cannot see the build
// that never settles at all: `void building.catch(...)` never fires for a
// promise that neither resolves nor rejects, so the memo keeps serving a
// permanently-pending promise and every `await` on it hangs forever — the same
// end state as a cached rejection, reached without a single failure event.
//
// That is not a theoretical arm. `wasm_bindgen_futures` tasks whose poll throws
// a JS exception die MID-POLL: the exception surfaces as a browser `pageerror`
// and the task's JS promise is never settled — measured on this codebase on the
// web launch path (2026-07-17), which is why `onboarding/+page.svelte` has
// carried `armLaunchWatchdog` (45 s) ever since. The consequence that matters
// here is stated in that measurement's own terms: **"the wasm request is
// bounded" proofs are only valid while the task survives its polls.** Every
// deadline these builds rely on — `ensureConnected`'s 15 s throw, a kind's
// 5 s RPC deadline — lives INSIDE the task, so a dead task takes its own
// bounds down with it and no internal budget can fire.
//
// So the two rules are two halves of one property: **a singleton build must
// reach a terminal state.** They are asserted in one file, over one table, on
// purpose — a reader who finds only the memo half re-derives the pending case
// as "already covered", which is exactly the reasoning that has to be blocked.
//
// The remedy is an EXTERNAL settle deadline (`guardSingletonBuild`), because
// only a timer outside the task survives the task's death. It rejects on
// expiry, which routes into the machinery that already exists: the per-page
// `catch` writes `loadError` and logs to the console (web.md § Async manager
// readiness), and the memo clear above fires, so a later mount rebuilds rather
// than inheriting the dead one.
//
// ⚠ It is NOT a retry, and NOT a longer wait. It does not repair the killed
// task and must never be read as a fix for the underlying throw. The budget
// bounds how long a surface may LIE about being ready; it cannot make a slow
// thing pass, so e2e convention 14 is untouched.

/** Every memoized build must be handed to the shared settle deadline. Asserted
 *  as "this module calls the shared guard", not as "this module has a
 *  setTimeout": a private timer per builder would be the fifth copy of a rule
 *  five surfaces need identically (priorities #1/#2), and the whole point of
 *  the census is that it lives in one place. */
const GUARD = "guardSingletonBuild";

for (const [name, { url, memo }] of Object.entries(MEMOS)) {
  Deno.test(
    `singleton build settle deadline — ${name} guards \`${memo}\` with \`${GUARD}\``,
    () => {
      const src = read(url);

      if (!new RegExp(`\\b${GUARD}\\b`).test(src)) {
        throw new Error(
          `${name}: the build memoized in \`${memo}\` is not routed through ` +
            `\`${GUARD}\` (\`$lib/singleton-build.ts\`), so a build that never ` +
            `SETTLES — the wasm-task-died-mid-poll class, whose internal ` +
            `deadlines die with the task — leaves \`${memo}\` holding a ` +
            `permanently pending promise. Every awaiting surface then hangs ` +
            `with no rejection, no catch, no error text and nothing logged. ` +
            `Wrap the build: \`const guarded = ${GUARD}('<label>', building, ` +
            `() => { if (${memo} === guarded) ${memo} = null; });\` and ` +
            `memoize \`guarded\` — see \`search.ts\` for the shape.`,
        );
      }

      // Guard the guard: calling the helper is only meaningful if what the
      // module MEMOIZES is the guarded promise. A module that guards a promise
      // and then stores the raw build would pass a name check while every
      // later caller is served the unbounded one.
      if (!new RegExp(`\\b${memo}\\s*=\\s*guarded\\b`).test(src)) {
        throw new Error(
          `${name}: \`${GUARD}\` is called but \`${memo}\` is not assigned the ` +
            `\`guarded\` promise it returns — so the first caller gets the ` +
            `bounded promise and every later caller is served the raw, ` +
            `unbounded build out of the memo. Memoize the guarded promise.`,
        );
      }
    },
  );
}

// ── The third part: an ABANDONED build writes nothing ─────────────────────
//
// The settle deadline rejects the awaiter, but it cannot stop the build — a
// promise is not cancellable. So a build that is merely slow rather than dead
// is abandoned while still ALIVE, a state no failure path before the deadline
// could produce: a rejection yields nothing, and a resolution was always the
// current one. Such a build can resolve after a replacement has been installed:
//
//   1. build A stalls past the deadline; the awaiter is rejected, the memo
//      cleared;
//   2. a later mount builds B, which installs `manager = B`;
//   3. A, still alive, finally resolves and installs `manager = A`.
//
// The memo now vends B while the slot holds A, and every `manager.` reader —
// the snapshot refreshers, the draft saves, the e2e counters — reads the build
// the page was told had failed. Same actor, so not a cross-actor leak: a
// split-brain between the promise a page awaits and the object it reads.
//
// The remedy is the identity seam's own shape one predicate over:
// `guardSingletonBuild` hands every build a `stillWanted()` that turns false
// when the deadline abandons it, and the build checks it beside
// `stillThisActor()` before each module-state write that follows an await.
// Asserted per SLOT, against the most recent await before each write, because
// that is the property — a check near the first write says nothing about a
// second write three awaits later (`event-drafts.ts`'s `current`).
//
// That covers writes AFTER the abandonment. A build can also be abandoned
// after it installed — dying in a tail that follows the install (the feed
// manager's draft restore, the conversations manager's MLS passes) — and then
// the memo is cleared while the slot still holds it. So the memo clear
// RETRACTS the product slot too, inside its `=== guarded` branch: while the
// memo held this build nothing else could write that slot, so what it holds is
// this build's or nothing. Left filled, every reader of the slot keeps using a
// build the page was told had failed — and the conversations receive poll,
// which rebuilds only on an EMPTY slot, would pump the abandoned engine for the
// rest of the page's life.
//
// `rpc.ts` consults the same predicate with a different remedy: an abandoned
// client arrives holding a socket and a push fan-out, so it is RETIRED on
// arrival rather than merely not stored. The trigger is shared; the remedy
// legitimately differs.

const WANTED = "stillWanted";

/** The build handed to the guard: from the `(stillWanted` parameter list that
 *  opens it to the `guardSingletonBuild(` call it is handed to. Crude on
 *  purpose, like every matcher in this file. */
function buildBody(name: string, src: string): string {
  const start = src.search(new RegExp(`\\(\\s*${WANTED}\\b`));
  const end = src.indexOf(`${GUARD}(`);
  if (start < 0 || end < 0 || end < start) {
    throw new Error(
      `${name}: no \`(${WANTED}) => …\` build ahead of its ` +
        `\`${GUARD}(\` call. The guard hands every build a \`${WANTED}\` ` +
        `predicate that turns false when the settle deadline abandons it — a ` +
        `build that does not take it cannot know it was given up on, and will ` +
        `install its result over the replacement build's.`,
    );
  }
  return src.slice(start, end);
}

for (const [name, { url, memo, slots, retract }] of Object.entries(MEMOS)) {
  if (slots.length === 0) continue;
  Deno.test(
    `abandoned build — ${name} checks both seams before every slot write after an await`,
    () => {
      const body = buildBody(name, read(url));
      for (const slot of slots) {
        const writes = [...body.matchAll(new RegExp(`\\b${slot}\\s*=(?![=>])`, "g"))];
        // Guard the guard: a renamed slot must fail loudly, not pass vacuously.
        if (writes.length === 0) {
          throw new Error(
            `${name}: its build never assigns \`${slot}\` — this contract's ` +
              `\`slots\` entry is stale. Point it at the module state the build ` +
              `now writes; do not delete it unless the build genuinely stopped ` +
              `writing module state.`,
          );
        }
        for (const w of writes) {
          const at = w.index!;
          const lastAwait = body.lastIndexOf("await ", at);
          // A write before the first await is on the caller's synchronous path:
          // nothing can have abandoned the build or switched the actor yet.
          if (lastAwait < 0) continue;
          const between = body.slice(lastAwait, at);
          if (!between.includes(`${WANTED}()`)) {
            throw new Error(
              `${name}: \`${slot}\` is assigned after an await with no ` +
                `\`${WANTED}()\` check in between. If the settle deadline ` +
                `abandoned this build during that await, the write installs its ` +
                `result over the replacement build's — the memo vends one build ` +
                `while the slot holds another. Check \`${WANTED}()\` beside ` +
                `\`stillThisActor()\`, after the await and before the write.`,
            );
          }
          // The identity seam's own rule, per write (`actorScope.ts`: "check it
          // before every write of module-level actor-scoped state that happens
          // after an await"). `actor-generation-contract.test.ts` pins that
          // each rail captures and checks the generation at all; a check near
          // one write says nothing about the next, which is how the
          // conversations build's `engineRole` write stood unchecked after the
          // role-lock await until 2026-09-10 — a switch inside it parked the
          // DEPARTING account's role where the incoming build read it as held.
          if (!between.includes("stillThisActor()")) {
            throw new Error(
              `${name}: \`${slot}\` is assigned after an await with no ` +
                `\`stillThisActor()\` check in between. A switch landing inside ` +
                `that await is not stopped by the drop, which only nulls what is ` +
                `already in the slot — this write puts the departing actor's ` +
                `state back for the incoming one.`,
            );
          }
        }
      }
    },
  );

  Deno.test(`abandoned build — ${name}'s build never re-reads its own slot`, () => {
    // After installing its result, a build's tail must work on the value it
    // BUILT, never on whatever the slot holds now: by the time the tail's next
    // await resumes, the slot may hold a replacement build (after an
    // abandonment) or the incoming actor's (after a switch), and a tail reading
    // `manager.` would run its restore passes on THAT one.
    const body = buildBody(name, read(url));
    for (const slot of slots) {
      const firstWrite = body.search(new RegExp(`\\b${slot}\\s*=(?![=>])`));
      if (firstWrite < 0) continue; // the test above reports this
      const reread = body.slice(firstWrite).search(new RegExp(`\\b${slot}\\s*!?\\s*\\.`));
      if (reread >= 0) {
        throw new Error(
          `${name}: its build reads \`${slot}.\` after assigning it. The slot ` +
            `is shared: a replacement build or the incoming actor's may own it ` +
            `by the time this line runs. Use the local the build constructed.`,
        );
      }
    }
  });

  if (retract === null) continue;
  Deno.test(`abandoned build — ${name}'s memo clear retracts \`${retract}\` with the memo`, () => {
    const src = read(url);
    // The clear: from the guard call to the line that memoizes what it returns.
    const start = src.indexOf(`${GUARD}(`);
    const end = src.search(new RegExp(`\\b${memo}\\s*=\\s*guarded\\b`));
    const clear = start >= 0 && end > start ? src.slice(start, end) : "";
    const owns = clear.search(/[!=]==\s*guarded\b/);
    const retracts = clear.search(new RegExp(`\\b${retract}\\s*=\\s*null\\b`));
    if (owns < 0 || retracts < 0 || retracts < owns) {
      throw new Error(
        `${name}: the memo clear handed to \`${GUARD}\` must also null ` +
          `\`${retract}\`, after its \`=== guarded\` check. A build abandoned ` +
          `AFTER installing leaves the slot holding a build the page was told ` +
          `had failed, and a reader that rebuilds only on an empty slot never ` +
          `rebuilds. While the memo held this build nothing else could write ` +
          `\`${retract}\`, so the retract is safe there — and only there: ` +
          `outside the check it would null a NEWER build's.`,
      );
    }
  });
}

Deno.test(`abandoned build — rpc.ts retires an abandoned client instead of registering it`, () => {
  const body = buildBody("rpc", read(MEMOS.rpc.url));
  const check = body.indexOf(`${WANTED}()`);
  const retire = body.indexOf("retireClient(c)");
  const register = body.indexOf("setOnPushEvent(");
  if (check < 0 || retire < 0 || register < 0 || !(check < retire && retire < register)) {
    throw new Error(
      `rpc.ts: getClient's late-arrival arm must consult \`${WANTED}()\` and ` +
        `retire the client (\`retireClient(c)\`) before registering it as live. ` +
        `An abandoned client build that resolves late would otherwise register ` +
        `its push fan-out and connection-state writes beside the replacement ` +
        `build's, with its socket left open and nobody holding it.`,
    );
  }
});
