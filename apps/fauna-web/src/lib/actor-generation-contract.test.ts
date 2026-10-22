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

// ── Every `webPage` write goes through the seam ───────────────────────────────
//
// `web-publish.ts` is a RAIL (above), but `webPage` is a shared writable store,
// and `WebSettingsSection.svelte` used to write it directly in three places,
// each after awaits and with no capture — a read started as A that landed after
// an in-heap actor change repopulated the store `resetWebPage` had just nulled,
// and B's feed ⋯-menu built B's links on A's origin. The
// rail test cannot see a component, so the writes now live in `web-publish.ts`
// (`setWebPage`/`patchWebPage`, which take the caller's predicate), and these
// pin both halves: nobody else writes the store, and each component handler
// that calls a writer has captured its own generation and hands it over.

function* srcFiles(dir: URL): Generator<URL> {
  for (const e of Deno.readDirSync(dir)) {
    const u = new URL(e.name + (e.isDirectory ? "/" : ""), dir);
    if (e.isDirectory) yield* srcFiles(u);
    else if (/\.(ts|svelte)$/.test(e.name)) yield u;
  }
}

/** Every non-test source file but the seam itself. */
function seamOutsiders(): Array<{ path: string; src: string }> {
  const out: Array<{ path: string; src: string }> = [];
  for (const u of srcFiles(new URL("../", import.meta.url))) {
    if (u.pathname.endsWith("/lib/web-publish.ts")) continue;
    if (u.pathname.endsWith(".test.ts")) continue;
    out.push({ path: u.pathname, src: stripComments(Deno.readTextFileSync(u)) });
  }
  return out;
}

Deno.test("no webPage store write exists outside web-publish.ts", () => {
  // Every spelling of a store write: the method calls, Svelte's `$store = …`
  // auto-subscription assignment, and a destructured `set`/`update`. Measured
  // 2026-10-15: the `$webPage = …` form passed the method-call regex alone.
  const writes = [
    /\bwebPage\s*\.\s*(set|update)\s*\(/,
    /\$webPage\s*(=(?!=)|\+=|\?\?=|\|\|=)/,
    /\{[^}]*\b(set|update)\b[^}]*\}\s*=\s*webPage\b/,
  ];
  const offenders = seamOutsiders()
    .filter(({ src }) => writes.some((re) => re.test(src)))
    .map(({ path }) => path);
  if (offenders.length) {
    throw new Error(
      `webPage is written outside the web-publish.ts seam: ${offenders.join(", ")} — ` +
        `use setWebPage/patchWebPage with the handler's own sameActorSince() predicate`,
    );
  }
});

/** Each `function <name>(…) { … }` in `src` (async or not, any indent, `export`
 *  or not), its body running to the next function header or `</script>` —
 *  crude, like the rest of this file: an arrow-function handler is folded into
 *  the function above it. */
function functionBodies(src: string): Array<{ name: string; body: string }> {
  const header = /^[ \t]*(?:export\s+)?(?:async\s+)?function\s+(\w+)\s*\(/gm;
  const starts = [...src.matchAll(header)].map((m) => ({ name: m[1], at: m.index! }));
  return starts.map(({ name, at }, i) => {
    let end = i + 1 < starts.length ? starts[i + 1].at : src.length;
    const script = src.indexOf("</script>", at);
    if (script >= 0 && script < end) end = script;
    return { name, body: src.slice(at, end) };
  });
}

// The census is DERIVED, not hand-listed: every function anywhere in the SPA
// that calls a guarded writer. A hand-picked list of handler names was this
// test's blind spot — a new handler that captured too late passed silently
// (measured 2026-10-15), the same drift the
// RAILS note above records.
const WRITER_CALL = /\b(setWebPage|patchWebPage)\s*\(/;
const writerCallers = seamOutsiders().flatMap(({ path, src }) =>
  functionBodies(src)
    .filter(({ body }) => WRITER_CALL.test(body))
    .map(({ name, body }) => ({ label: `${path.split("/src/")[1]}::${name}`, body }))
);

Deno.test("the webPage writer census still finds Settings → Web's three handlers", () => {
  // Guards the derivation itself: a parser that silently finds nothing would
  // turn every per-caller test below into a vacuous pass.
  const found = writerCallers.map((c) => c.label);
  for (const name of ["hydrate", "onToggle", "unpublish"]) {
    if (!found.includes(`lib/components/WebSettingsSection.svelte::${name}`)) {
      throw new Error(`writer census lost WebSettingsSection.${name}; found: ${found.join(", ")}`);
    }
  }
});

for (const { label, body } of writerCallers) {
  Deno.test(`${label} guards its own webPage write`, () => {
    const capture = body.indexOf("sameActorSince()");
    const firstAwait = body.indexOf("await ");
    if (capture < 0 || (firstAwait >= 0 && capture > firstAwait)) {
      throw new Error(`${label} must capture sameActorSince() before its first await`);
    }
    if (!/\b(setWebPage|patchWebPage)\([\s\S]*?stillThisActor/.test(body)) {
      throw new Error(`${label} must hand its own stillThisActor to setWebPage/patchWebPage`);
    }
  });
}

/** The body of top-level `export [async] function <name>(` in `src`, to its
 *  closing column-0 brace. */
function exportedBody(src: string, name: string): string {
  const at = src.search(new RegExp(`export (?:async )?function ${name}\\(`));
  if (at < 0) throw new Error(`web-publish.ts must export ${name}`);
  const end = src.indexOf("\n}", at);
  return src.slice(at, end < 0 ? undefined : end);
}

Deno.test("web-publish.ts exports the guarded writers and checks the predicate in each", () => {
  // Bounded to each writer's own body: a fixed-width window from
  // `setWebPage` reached `patchWebPage`'s check, so dropping `setWebPage`'s
  // re-check passed (measured 2026-10-15).
  const src = sourceOf(RAILS["web-publish.ts"]);
  for (const fn of ["setWebPage", "patchWebPage"]) {
    const body = exportedBody(src, fn);
    const check = body.indexOf("stillThisActor()");
    const write = body.search(/\bwebPage\s*\.\s*(set|update)\s*\(/);
    if (check < 0 || write < 0 || check > write) {
      throw new Error(`${fn} must consult stillThisActor() before writing the store`);
    }
  }
});

Deno.test("ensureWebPageHydrated re-checks the generation after its last await", () => {
  // Its own leg: the rail test above counts `stillThisActor()` across the whole
  // file, and the two writers satisfy that count on their own, so dropping this
  // re-check passed (measured 2026-10-15).
  const body = exportedBody(sourceOf(RAILS["web-publish.ts"]), "ensureWebPageHydrated");
  const write = body.indexOf("webPage.set(");
  const lastAwait = body.lastIndexOf("await ", write);
  const check = body.indexOf("stillThisActor()", lastAwait);
  if (write < 0 || lastAwait < 0 || check < 0 || check > write) {
    throw new Error(
      "ensureWebPageHydrated must re-check stillThisActor() between its last await " +
        "and its webPage.set — a hydrate begun as A must not land for B",
    );
  }
});

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
