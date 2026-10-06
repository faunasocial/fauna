// An actor-scope seam must be registered BEFORE the first `await` of the mount
// that registers it — asserted as a general invariant over the source rather
// than as one more hand-picked outcome (testing.md convention 17). Sibling of
// `singleton-build-memo-contract.test.ts` and `manager-gate-contract.test.ts`,
// aimed at the same rebuild seam from a third side: those two cover a build
// that rejects and a user action landing while the manager is null; this one
// covers the handler that was never registered in time to hear about the
// identity at all.
//
// ── The class ─────────────────────────────────────────────────────────────
//
// A page that owns actor-scoped state registers two things with `$lib/actorScope`:
//
//     registerActorScopedReset(dropMyCaches);   // the drop
//     onActorChange(async (id) => { ...rebuild... });   // the rebuild
//
// Registration is what makes the page hear an identity change. `onActorChange`
// also fires the handler immediately when `get(identity)` is already non-null,
// which is why a late registration usually still works — but that is a
// FALLBACK, not the contract: it covers "the identity was already there", never
// "the identity arrived while I was booting". A page that registers after an
// `await` has a window — a whole wasm module instantiation, on a loaded box —
// in which an identity change fires no drop and no rebuild for that page, and
// nothing anywhere says so. `account-scoping.md` § The scoping taxonomy names
// that outcome exactly: silent by construction.
//
// ── What it cost ──────────────────────────────────────────────────────────
//
// The feed page's `onMount` opened on `await ensureWasm()` and registered
// twenty-two lines later. The e2e agent's
// `set_state` login lands squarely inside that window — the browser console on
// every red carried `[actor-scope] reset failed: WASM not initialized`, which
// can only print while the module is still null, i.e. before the page's own
// `ensureWasm()` resolved. So the identity change genuinely did arrive while
// the page's handler was not yet registered.
//
// ── The rule ──────────────────────────────────────────────────────────────
//
// Inside an `onMount(async …)`, no `await` may precede the first
// `registerActorScopedReset(` / `onActorChange(`. A page whose registration is
// module-level or in a synchronous `onMount` satisfies this trivially — that is
// the shape `media/+page.svelte` and `+layout.svelte` already have, and the one
// the others are held to.
//
// Handlers that genuinely need wasm keep awaiting it THEMSELVES: `getFeedManager()`
// and `getConversationsManager()` both do, and the media page's handler opens on
// `await ensureWasm()` inside the callback. Registering early costs nothing and
// removes the window.
//
// Deliberately crude, like its siblings: it reads source as TEXT, so it stays
// honest about what a reader would see. A determined evasion slips it; the
// contract is aimed at the shape that actually recurs.

import { stripComments } from "./source-contract.ts";

/** Every SPA surface that registers an actor-scope seam. Kept as an explicit
 *  table, like `singleton-build-memo-contract.test.ts`'s `MEMOS`, so that a
 *  route which stops registering fails this contract loudly instead of
 *  silently dropping out of it. */
const SURFACES: Record<string, URL> = {
  "routes/+layout.svelte": new URL(
    "../routes/+layout.svelte",
    import.meta.url,
  ),
  "routes/feed/+page.svelte": new URL(
    "../routes/feed/+page.svelte",
    import.meta.url,
  ),
  "routes/conversations/+page.svelte": new URL(
    "../routes/conversations/+page.svelte",
    import.meta.url,
  ),
  "routes/media/+page.svelte": new URL(
    "../routes/media/+page.svelte",
    import.meta.url,
  ),
};

const REGISTRATION = /\b(?:registerActorScopedReset|onActorChange)\s*\(/;

/** The body of the first `onMount(async …)` in `src`, or `null` when the file
 *  has no async mount (a synchronous `onMount`, or a module-level
 *  registration — both of which have no window to open).
 *
 *  Brace-matched rather than regexed to the end: these mounts are hundreds of
 *  lines and contain every brace shape there is. */
function asyncMountBody(src: string): string | null {
  const open = src.search(/onMount\(\s*async\b/);
  if (open === -1) return null;
  const start = src.indexOf("{", open);
  if (start === -1) return null;
  let depth = 0;
  for (let i = start; i < src.length; i++) {
    if (src[i] === "{") depth++;
    else if (src[i] === "}") {
      depth--;
      if (depth === 0) return src.slice(start, i + 1);
    }
  }
  return src.slice(start);
}

for (const [name, url] of Object.entries(SURFACES)) {
  Deno.test(
    `actor-scope registration — ${name} registers before its first await`,
    () => {
      const src = stripComments(Deno.readTextFileSync(url));

      // Guard the guard: a surface that no longer registers anything means this
      // table is stale, and a stale table is a contract that silently covers
      // nothing.
      if (!REGISTRATION.test(src)) {
        throw new Error(
          `${name}: no \`registerActorScopedReset(\` / \`onActorChange(\` in the ` +
            `source — this contract's SURFACES table is stale. Remove the entry ` +
            `if the surface genuinely stopped owning actor-scoped state; do not ` +
            `leave it pointing at a file it no longer describes.`,
        );
      }

      const body = asyncMountBody(src);
      // No async mount ⇒ the registration is module-level or in a synchronous
      // `onMount`; there is no await for it to be late to.
      if (body === null) return;

      const reg = body.search(REGISTRATION);
      // Registered outside the async mount (module level) ⇒ nothing to be late
      // to either.
      if (reg === -1) return;

      const firstAwait = body.search(/\bawait\b/);
      if (firstAwait !== -1 && firstAwait < reg) {
        throw new Error(
          `${name}: \`onMount(async …)\` awaits before it registers its ` +
            `actor-scope seam, so an identity change landing in that window ` +
            `fires NO drop and NO rebuild for this surface — and nothing says ` +
            `so, which is the silent-by-construction outcome ` +
            `\`account-scoping.md\` § The scoping taxonomy bans. Move ` +
            `\`registerActorScopedReset(\` + \`onActorChange(\` above the first ` +
            `await; anything in the handler that needs wasm awaits it itself ` +
            `(see \`routes/media/+page.svelte\`).`,
        );
      }
    },
  );
}
