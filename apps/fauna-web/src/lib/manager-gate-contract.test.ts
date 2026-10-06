// The manager-readiness gate contract, asserted as a general invariant over
// the source rather than as one more hand-picked outcome (testing.md
// convention 17).
//
// Web is the one app whose page managers arrive ASYNC after mount (the wasm
// module and the manager build in the browser), so every managed page has a
// window in which its `manager` is null and a `manager?.` call no-ops. A user
// action landing in that window must be REFUSED VISIBLY — a disabled control,
// or the page's error-message surface — never silently dropped: the human
// half of the e2e test-agent contract ("never silently drop a command",
// e2e-conventions.md convention 11), applied to real users. The measured cost
// of the silent shape: the conversations "+" discarded clicks for the ~1 s
// manager build with no feedback at all, which produced 5 failing e2e tests
// across 3 files and a wrong "environmental" triage before the one-line gate
// was found. Owner:
// docs/goal/architecture/apps/web.md § Async manager readiness.
//
// Two enforcement halves:
//  1. A null-manager guard that just RETURNS is BANNED in the managed pages'
//     sources — the refusal must surface itself (set the page's writable error
//     to `t.common.still_loading`) before returning. A genuine non-user-action
//     ordering guard (an $effect, a $derived.by, a subscription callback)
//     declares itself with a same-line `// manager-gate-ok: <reason>` — an
//     exception you have to write down is one the next reader can see.
//
//     This half is stated as a REMEDY requirement ("surface or annotate"),
//     not as one banned spelling, because the banned-spelling form let the
//     class straight back in. The original matcher required `!manager` to be
//     the WHOLE condition (`!manager\s*\)`), so every compound guard —
//     `if (!manager || !id || !composeBody.trim()) return;` — read as
//     compliant while dropping exactly as silently. Five such guards sat in
//     the feed page the whole time this contract claimed the class was closed
//     structurally, three of them reachable: submitReply and
//     handleSubscribeBridgeFeed sit behind buttons that never gate on
//     `feedReady` at all, and handleCompose gates on it without covering its
//     own second condition (`!id`).
//  2. The conversations page routes every handler through its `run()`
//     chokepoint, so its whole gate is ONE guard: run() must keep refusing a
//     null manager ahead of `await fn()`. That shape is immune by
//     construction to a new handler forgetting the rule — the same argument
//     feed-refresh-contract.test.ts makes for the refresh half — and this
//     test is what keeps the guard from being refactored away.
//
// Deliberately crude, like its sibling: this reads source as TEXT, line by
// line, so it stays honest about what a reader would see. A determined
// multi-line evasion (`if (!manager) {\n return; }`) slips the matcher; the
// contract is aimed at the pattern that actually recurs, not at adversaries.

import { stripComments } from "./source-contract.ts";

const PAGES: Record<string, URL> = {
  feed: new URL("../routes/feed/+page.svelte", import.meta.url),
  conversations: new URL("../routes/conversations/+page.svelte", import.meta.url),
};

/** The guard opener — `!manager` anywhere in an `if` condition, first term or
 *  not. Deliberately does NOT try to match the condition's shape: `.trim()`
 *  calls inside it put unbalanced-looking parens on the line, and every
 *  attempt to spell the condition out is one more way for a new one to slip
 *  past (that is precisely how the compound guards got in). */
const GUARD = /\bif\s*\(\s*!manager\b/;
/** The guard's statement being a bare `return` — the drop itself. Matches the
 *  condition's closing paren followed by `return` or `{ return`, the one shape
 *  a single-line silent drop can take. */
const BARE_RETURN = /\)\s*(?:return\b|\{\s*return\b)/;
/** The remedy: the same line hands the user something to see. Either the
 *  shared still-loading string, or an assignment into one of the pages'
 *  writable error surfaces (`loadError`, `bridgeFormError`, …). The `[^=]`
 *  tail keeps an `===` comparison from reading as an assignment. */
const SURFACES_ERROR = /still_loading|Error\s*=[^=]/;

/** Lines that silently drop on a null manager: a `!manager` guard whose whole
 *  body is a `return`, with neither a visible refusal on the line nor a
 *  same-line `manager-gate-ok:` declaration. Block comments are blanked
 *  (newlines kept, so reported line numbers stay true); each line's own
 *  trailing `//` comment is stripped before matching, so prose ABOUT the
 *  banned pattern can never trip it — while the annotation is looked up on the
 *  RAW line, where it lives. */
export function bareGateViolations(src: string): string[] {
  const noBlocks = src.replace(/\/\*[\s\S]*?\*\//g, (m) => m.replace(/[^\n]/g, " "));
  const out: string[] = [];
  noBlocks.split("\n").forEach((line, i) => {
    const code = line.replace(/(^|[^:])\/\/.*$/, "$1");
    if (!GUARD.test(code) || !BARE_RETURN.test(code)) return;
    if (SURFACES_ERROR.test(code)) return;
    if (src.split("\n")[i].includes("manager-gate-ok:")) return;
    out.push(`line ${i + 1}: ${line.trim()}`);
  });
  return out;
}

Deno.test("managed pages — no user action is silently dropped on a null manager", () => {
  for (const [name, url] of Object.entries(PAGES)) {
    const violations = bareGateViolations(Deno.readTextFileSync(url));
    if (violations.length > 0) {
      throw new Error(
        `${name} page: a \`!manager\` guard whose whole body is a \`return\` ` +
          `silently drops a user action while the manager is still building ` +
          `(a compound condition drops exactly as silently as a bare one). ` +
          `Surface the refusal ` +
          `(set the page's writable error to t.common.still_loading) or, for ` +
          `a genuine non-user-action ordering guard, annotate the line with ` +
          `\`// manager-gate-ok: <reason>\`.\n${violations.join("\n")}`,
      );
    }
  }
});

Deno.test("conversations — run() refuses a null manager ahead of fn()", () => {
  const src = Deno.readTextFileSync(PAGES.conversations);
  const declAt = src.indexOf("async function run(");
  if (declAt === -1) {
    throw new Error(
      "conversations page: the run() chokepoint is gone — every handler used " +
        "to route through it, and its null-manager guard was the page's whole " +
        "gate. If the page was restructured, port the guard AND this matcher.",
    );
  }
  const fnCallAt = src.indexOf("await fn()", declAt);
  if (fnCallAt === -1) {
    throw new Error(
      "conversations page: run() no longer awaits fn() — the matcher below " +
        "can't locate the guard window; fix the matcher before trusting it.",
    );
  }
  const head = stripComments(src.slice(declAt, fnCallAt));
  if (!/if\s*\(\s*!manager\s*\)/.test(head) || !head.includes("still_loading")) {
    throw new Error(
      "conversations page: run() must refuse a null manager BEFORE invoking " +
        "fn() — set pageError to t.common.still_loading and return. Without " +
        "it, every handler's `manager?.` no-ops invisibly during the manager " +
        "build window (the measured '+' bug).",
    );
  }
});

// A guard on the guard, red-verified inline: feed the matcher the exact
// mutants it exists to catch (and the two shapes it must allow), so a regex
// edit that stops matching fails HERE rather than passing the page tests
// vacuously — the same failure mode its sibling pins with "the matcher still
// sees the handlers".
Deno.test("the banned-pattern matcher itself still fires", () => {
  const cases: Array<[string, number]> = [
    ["  if (!manager) return;", 1],
    ["  if (!manager) { return; }", 1],
    ["  if (!manager) return; // manager-gate-ok: $effect ordering guard", 0],
    ["  if (!manager) { loadError = t.common.still_loading; return; }", 0],
    ["  // prose mentioning if (!manager) return must not trip", 0],
    ["  /* block prose: if (!manager) return */", 0],
    // The compound shapes the first matcher missed for as long as it existed —
    // every one of these is a real line that stood in the feed page while this
    // file claimed the class was closed. A
    // condition-shape edit that stops catching them fails HERE.
    ["  if (!manager || !id) return;", 1],
    ["  if (!manager || !replyTargetId) return;", 1],
    ["  if (!manager || !newFeedName.trim()) return;", 1],
    ["  if (!manager || !id || !composeBody.trim()) return;", 1],
    ["  if (!manager || !a.trim() || !b.trim()) return;", 1],
    ["  if (!manager || !selectedId) return null;", 1],
    ["  if (!manager || !d) return; // manager-gate-ok: $effect ordering guard", 0],
    ["  if (!manager || !id) { loadError = t.common.still_loading; return; }", 0],
    // An error surface other than the shared string is still a surface.
    ["  if (!manager || !u.trim()) { bridgeFormError = msg; return; }", 0],
    // …but a comparison is not an assignment: `Error ===` must not read as one.
    ["  if (!manager && lastError === null) return;", 1],
  ];
  for (const [snippet, expected] of cases) {
    const got = bareGateViolations(snippet).length;
    if (got !== expected) {
      throw new Error(
        `matcher self-check failed on ${JSON.stringify(snippet)}: expected ` +
          `${expected} violation(s), got ${got}`,
      );
    }
  }
});
