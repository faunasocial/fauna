// The feed page's refresh contract, asserted as a general invariant over the
// source rather than as one more hand-picked outcome (testing.md convention 17).
//
// Web is the only client with no observer seam: `libs/fauna-wasm/src/feed.rs`
// registers no `FeedSnapshotObserver`, so `FeedManager::notify()` reaches
// nothing here and the browser owns the loop by hand. The page states the
// contract at the top of its own `<script>` — "every async manager method is
// followed by refreshFeed(), which re-reads snapshot() into the feedSnapshot
// store" — and enforces it by repeating `finally { refreshFeed(); }` at every
// call site. tui and linux cannot have this problem: both register an observer
// (`apps/fauna-tui/src/feed/mod.rs`, `apps/fauna-linux/src/feed/observer.rs`)
// and repaint off `notify()`.
//
// A contract kept by repetition is a contract one handler can silently drop,
// and one did: `buyUnlockOffer` awaited the manager and never refreshed, so a
// SUCCESSFUL purchase left its own Buy button on screen — the shared-Rust state
// clear landed correctly and the DOM never learned. It presented as "the
// subscribe call never settles", cost a session's diagnosis, and produced two
// wrong hypotheses (a `nest_supports` hang, a swallowed wasm/JS rejection)
// before the cause turned out to be three missing lines. This test is what
// makes that class unrepeatable.
//
// The invariant is deliberately coarse and currently has NO exceptions: every
// function that awaits a manager method also refreshes. Coarse is the point —
// a per-method mutating/read-only split would need a list that rots the moment
// the manager grows a method, whereas "you awaited it, so re-read the
// snapshot" is true by construction and costs a pure reader nothing but one
// extra snapshot read. If a genuine read-only exception ever arrives, add it
// here with its reason rather than weakening the rule; an exception you have
// to write down is one the next reader can see.
//
// (The sibling conversations page solves the same problem structurally instead
// — one `run()` chokepoint whose own `finally` refreshes, described there as
// "the page's UI-action chokepoint". That shape is immune by construction and
// is the better long-term answer for this page too; it is a bigger refactor
// than a bug fix should carry, because the feed handlers' error surfacing
// genuinely differs site to site — some set `loadError`, some deliberately only
// warn — and flattening that is its own bug.)

const FEED_PAGE = new URL("../routes/feed/+page.svelte", import.meta.url);

// Comment stripping is non-negotiable before matching — the scar that proved
// it lives with the shared helper (source-contract.ts, lifted from here when
// manager-gate-contract.test.ts became its second consumer).
import { stripComments } from "./source-contract.ts";

/** Function bodies in the page's `<script>`, split on its 2-space-indented
 *  `function`/`async function` declarations, comments removed. Crude on
 *  purpose: this reads source as text, so it stays honest about what a reader
 *  would see. */
function functionBodies(src: string): Array<{ name: string; body: string }> {
  const code = stripComments(src);
  const decls = [...code.matchAll(/\n {2}(?:async )?function ([A-Za-z0-9_]+)\s*\(/g)]
    .map((m) => ({ at: m.index!, name: m[1] }));
  return decls.map((d, i) => ({
    name: d.name,
    body: code.slice(d.at, i + 1 < decls.length ? decls[i + 1].at : code.length),
  }));
}

Deno.test("feed page — every handler that awaits the manager re-reads the snapshot", () => {
  const src = Deno.readTextFileSync(FEED_PAGE);
  const offenders = functionBodies(src)
    .filter((f) => /await\s+manager[!?]?\./.test(f.body))
    .filter((f) => !f.body.includes("refreshFeed("))
    .map((f) => f.name);

  if (offenders.length > 0) {
    throw new Error(
      `these feed-page handlers await a manager method without calling ` +
        `refreshFeed(): ${offenders.join(", ")}. Web has no FeedSnapshotObserver, ` +
        `so a manager state change that nothing re-reads is invisible to the DOM ` +
        `— the handler looks like it silently did nothing, and the bug presents ` +
        `as a hang far from its cause. Add \`finally { refreshFeed(); }\`, as ` +
        `every other handler on this page does.`,
    );
  }
});

// A guard on the guard: if the page is ever restructured so the matcher stops
// finding the handlers, the test above would pass vacuously — green while
// checking nothing, which is the failure mode convention 7 and 17 both warn
// about. Pin the shape it depends on instead of trusting it.
Deno.test("feed page — the refresh-contract matcher still sees the handlers", () => {
  const src = Deno.readTextFileSync(FEED_PAGE);
  const awaiting = functionBodies(src).filter((f) =>
    /await\s+manager[!?]?\./.test(f.body)
  );
  if (awaiting.length < 10) {
    throw new Error(
      `expected the feed page to have many manager-awaiting handlers, found ` +
        `${awaiting.length} — the page was restructured (or the declaration ` +
        `matcher broke), so the contract test above is no longer checking ` +
        `anything. Fix the matcher before trusting either test.`,
    );
  }
});
