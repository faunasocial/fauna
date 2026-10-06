// The feed composer's submit contract, asserted over the page source (testing.md
// convention 17 — a general invariant, not one more hand-picked outcome).
//
// `handleCompose` awaits for seconds — provenance read, thumbnail, blob uploads,
// `fauna.posts.create`, the post-submit reload — and the composer stays editable
// throughout. Two things follow, and each was a real defect:
//
//  1. It must read the composer ONCE, at the click, and send only that value.
//     A live read after any await sends whatever the user has typed since: the
//     next post's text under this post's photo.
//  2. Its success path must clear only what was sent (`$lib/compose-sent`'s
//     `clearSent`), never reset the fields wholesale. The wholesale reset erased
//     the user's NEXT post whenever the page's own reload put this post on
//     screen before this submit resolved — measured as `test_feed.py`'s second
//     compose sitting disabled for the full 90 s with `compose-file-ready` gone
//     and an empty `error-message`. Owner: feed.md § User
//     actions, the `post-submit-button` row.
//
// Crude on purpose, like its siblings: the function body is found as TEXT, from
// its opener to the next top-level function of the component script.

import { assert } from "jsr:@std/assert";
import { stripComments } from "./source-contract.ts";

const PAGE = new URL("../routes/feed/+page.svelte", import.meta.url);

/** The page-local composer fields `handleCompose` must not read live. */
const LIVE_FIELDS = [
  "composeBody",
  "composeTags",
  "composeFile",
  "composeFileData",
  "gateTier",
  "gatePreview",
  "sellSelected",
  "sellPrice",
  "sellAskingPrice",
  "sellSubscribersFree",
];

/** `handleCompose`'s body, comments stripped, or `null` when it is not found. */
export function handleComposeBody(src: string): string | null {
  const code = stripComments(src);
  const start = code.indexOf("async function handleCompose(");
  if (start < 0) return null;
  // The next function declared at the component script's own indent ends it.
  const rest = code.slice(start + 1);
  const end = rest.search(/\n  (?:async )?function \w/);
  return end < 0 ? rest : rest.slice(0, end);
}

/** Every way `body` breaks the contract, as human-readable findings. */
export function composeSubmitViolations(body: string): string[] {
  const found: string[] = [];
  const capture = body.indexOf("const sent = readCompose();");
  if (capture < 0) {
    found.push("no `const sent = readCompose();` capture — the composer is not read once at the click");
    return found;
  }
  const afterCapture = body.slice(capture);
  for (const field of LIVE_FIELDS) {
    // A bare read or write of the live field; `sent.<field>` is the sanctioned form.
    const live = new RegExp(`(?<![.\\w])${field}\\b`);
    if (live.test(afterCapture)) {
      found.push(`\`${field}\` is read or written live after the capture — use \`sent.${field}\` / \`clearSent\``);
    }
  }
  if (!/clearSent\(\s*readCompose\(\)\s*,\s*sent\s*,/.test(afterCapture)) {
    found.push("the success path does not clear through `clearSent(readCompose(), sent, …)`");
  }
  return found;
}

Deno.test("handleCompose sends its click-time capture and clears only what it sent", async () => {
  const body = handleComposeBody(await Deno.readTextFile(PAGE));
  assert(body !== null, "handleCompose not found in feed/+page.svelte — did it move? update this contract");
  const violations = composeSubmitViolations(body);
  assert(violations.length === 0, `feed composer submit contract broken:\n  - ${violations.join("\n  - ")}`);
});

Deno.test("the contract rejects the shape it was written against", () => {
  // The pre-fix shape, reduced: a live read after an await and a wholesale reset.
  const old = `
  async function handleCompose(): Promise<void> {
    if (!composeBody.trim()) return;
    composing = true;
    try {
      await upload();
      manager.updateCompose(composeBody.trim(), composeTags, null);
    } finally {
      composeBody = '';
      composeFile = null;
    }
  }
  function next() {}`;
  const body = handleComposeBody(old);
  assert(body !== null);
  assert(composeSubmitViolations(body).length > 0, "a wholesale-reset, live-read handleCompose must be flagged");
});
