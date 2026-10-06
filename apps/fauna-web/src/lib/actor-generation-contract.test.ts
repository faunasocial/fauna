// The identity seam ahead of the drop, asserted as a general invariant over the
// rails' SOURCE rather than as one more hand-picked outcome (testing.md
// convention 17).
//
// `account-scoping.md` § The scoping taxonomy's in-memory corollary: "dropping
// state is only half of it — the background loops that WRITE that state must be
// retired by the same drop", and a loop with no cancellation handle "cannot be
// stopped by any list, so the seam comes before the drop".
//
// On web the loop is each rail's singleton BUILDER. All three are the same
// shape — an async build memoized into a module-level promise, assigning
// module-level actor-scoped state (`manager`, `face`, `current`) after an
// await — and all three had the same hole: the rail's reset nulls those
// variables, but a promise is not cancellable and a wasm call already in flight
// resolves whatever the switch did, so the in-flight build simply wrote them
// again, for the actor who had just left. The measured consequence on the
// events rail: the incoming actor's New Event form resumed the DEPARTING
// actor's draft, and their first keystroke persisted it under their own
// `BackupKey`.
//
// A runtime test cannot reach these modules — importing any of them pulls
// `svelte/store`, the wasm glue and the whole SPA graph into `deno test` — so
// the seam's BEHAVIOUR is pinned in `actorScope.test.ts` (which imports only the
// seam) and its ADOPTION is pinned here, structurally, on the three rails that
// have to use it. Same division the two sibling contract tests make.
//
// Deliberately crude, like its siblings: this reads source as TEXT. It is aimed
// at the pattern that actually recurs — a new rail, or a rewritten builder, that
// forgets the seam — not at an adversary.

import { stripComments } from "./source-contract.ts";

/** The rails, each a memoized singleton builder that writes actor-scoped
 *  module state after an await. A rail added later belongs here — and the
 *  census is this contract's weak point, exactly as it is its siblings':
 *  `search.ts` was the fourth such builder and sat outside this table until
 *  2026-09-09, unguarded, while the note above still said "the three rails". Searching signs nothing, so the leak was not a key —
 *  it was the departing actor's RESULTS rendered to the incoming one, the same
 *  class 1/4 breach one rail over. The lesson the memo contract already
 *  records applies here too: the drift, not the missing piece of the day, is
 *  the finding. */
const RAILS: Record<string, URL> = {
  "event-drafts.ts": new URL("./event-drafts.ts", import.meta.url),
  "feed.ts": new URL("./feed.ts", import.meta.url),
  "conversations.ts": new URL("./conversations.ts", import.meta.url),
  "search.ts": new URL("./search.ts", import.meta.url),
  "web-publish.ts": new URL("./web-publish.ts", import.meta.url),
};

function sourceOf(url: URL): string {
  return stripComments(Deno.readTextFileSync(url));
}

for (const [name, url] of Object.entries(RAILS)) {
  Deno.test(`${name} captures the actor generation before its build`, () => {
    const src = sourceOf(url);
    if (!/from ['"]\.\/actorScope['"]/.test(src) || !src.includes("sameActorSince")) {
      throw new Error(
        `${name} does not import sameActorSince from actorScope — its singleton ` +
          `builder writes module-level actor-scoped state after an await, so it ` +
          `needs the identity seam (account-scoping.md, the in-memory corollary)`,
      );
    }
  });

  Deno.test(`${name} captures the generation OUTSIDE its async builder`, () => {
    const src = sourceOf(url);
    // The capture has to happen before the first await, on the caller's
    // synchronous path — capturing inside the build would read the generation
    // the switch had already bumped, and compare it to itself forever after.
    // The build is the `async (stillWanted) => { … }` handed to
    // `guardSingletonBuild` (it was an async IIFE until the guard started
    // passing it a predicate); either spelling opens with `async (`.
    const capture = src.indexOf("sameActorSince()");
    const builder = src.search(/\basync\s*\(/);
    if (capture < 0 || builder < 0 || capture > builder) {
      throw new Error(
        `${name} captures the actor generation inside (or after) its async ` +
          `builder — capture it on the synchronous path, before the first await, ` +
          `or the check compares the generation to itself`,
      );
    }
  });

  Deno.test(`${name} re-checks the generation before writing module state`, () => {
    const src = sourceOf(url);
    // The remedy, not one banned spelling: somewhere after the capture the
    // builder must consult the predicate again. One capture and no check is the
    // shape that reads as guarded and is not — the exact failure the sibling
    // contract test's own history warns about.
    const checks = src.match(/stillThisActor\(\)/g)?.length ?? 0;
    if (checks < 1) {
      throw new Error(
        `${name} captures the actor generation but never re-checks it before ` +
          `assigning module-level actor-scoped state`,
      );
    }
  });
}

// The seam itself must keep bumping on the drop — the half that makes every
// check above mean anything. Read from source for the same reason: importing
// actorScope.ts is fine (actorScope.test.ts does), but this one assertion is
// about the ORDER of two statements, which is a source property.
Deno.test("resetActorScopedState bumps the generation before running the drops", () => {
  const src = sourceOf(new URL("./actorScope.ts", import.meta.url));
  const body = src.slice(src.indexOf("export function resetActorScopedState"));
  const bump = body.indexOf("generation += 1");
  const loop = body.indexOf("for (const reset of resets)");
  if (bump < 0 || loop < 0 || bump > loop) {
    throw new Error(
      "resetActorScopedState must bump the actor generation BEFORE running the " +
        "registered drops — an in-flight build resolving between two drops must " +
        "already read as the previous actor's",
    );
  }
});
