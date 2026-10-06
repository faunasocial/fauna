// Shared helper for the SPA's source-contract tests (feed-refresh-contract,
// manager-gate-contract): tests that assert an invariant over a page's SOURCE
// TEXT rather than over one more hand-picked runtime outcome (testing.md
// convention 17). Lifted out of feed-refresh-contract.test.ts when a second
// contract test arrived, so the comment-stripping discipline cannot drift
// between them.

/** Strip comments before matching. Non-negotiable, and the reason is a live
 *  scar: the first cut of the feed refresh contract searched raw bodies for
 *  `refreshFeed(`, and the handler's own explanatory comment — which names
 *  `refreshFeed()` — satisfied it. Reverting the real call left the test
 *  GREEN. A source matcher that reads prose is a vacuous assertion wearing a
 *  test's clothes, so strip first; the red-verify is what caught it.
 *
 *  `//` only starts a comment when it isn't part of a `://` scheme — the pages
 *  have URLs in string literals, and eating the rest of those lines would make
 *  a matcher wrong in the other direction.
 *
 *  `/*` has the same problem and a nastier failure, found 2026-09-07 by the
 *  post-image source contract: the feed page's file-picker carries
 *  `accept="image/​*,video/​*"`, two `/*` inside a string attribute with no `*​/` of
 *  their own. A bare `\/\*[\s\S]*?\*\/` pairs each of them with the NEXT REAL
 *  closer, so every block comment after that line is mispaired and real markup
 *  between them is eaten — the feed page's post-detail `<img>` vanished from the
 *  stripped source and the new contract read zero elements there. It did not
 *  surface earlier only because the two original consumers read `<script>`
 *  function bodies, all of which sit above that line. Requiring the opener to
 *  follow whitespace/`(`/`:`/`,` (a real comment always does; a MIME wildcard
 *  never does — it follows a word character) restores the pairing. The guard-on-
 *  the-guard test is what caught this rather than a silently-vacuous pass. */
export function stripComments(s: string): string {
  return s
    .replace(/(^|[\s(:,])\/\*[\s\S]*?\*\//g, "$1 ")
    .replace(/(^|[^:])\/\/[^\n]*/g, "$1");
}
