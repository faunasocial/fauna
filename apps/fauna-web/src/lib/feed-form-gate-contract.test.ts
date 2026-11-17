// The feed, search and nostr form-submit contract (`docs/goal/ui/README.md`
// § Offer only what is possible, rule 4's form case and rule 7; `feed.md`,
// `search.md`, `nostr.md` § Errors & edge cases), asserted over the source as
// a general invariant (testing.md convention 17) — the paint and refusal
// themselves are pinned by `form-gate.test.ts`, the predicates in shared Rust.
//
// Three halves:
//  1. Every gated submit handler ASKS its shared predicate through
//     `formRefusal(askPossibility(() => <predicate>(…)))` before it runs, so a
//     blocked draft refuses with the painted reason instead of a bare return.
//  2. Every gated submit button is painted from a `formGate` of that predicate
//     (its `disabled=` reads the gate, its `title=` the reason).
//  3. The four feed forms close ONLY on a saved result: the compose dialog
//     closes on `handleCompose()`'s `true`; the reply dialog's close sits
//     behind its `saved` flag, never ahead of the send; create-feed and the
//     bridge-subscribe form close inside the `try` after the awaited save, and
//     their `catch` paints `error-message` (`loadError`).
//
// Crude like its siblings: reads source as TEXT with comments stripped.

import { assert } from "jsr:@std/assert";
import { stripComments } from "./source-contract.ts";

const read = (rel: string) => Deno.readTextFile(new URL(rel, import.meta.url)).then(stripComments);

/** The body of `function <name>(` up to the next function at `indent`. */
function fnBody(code: string, name: string, indent = "  "): string {
  const start = code.search(new RegExp(`(?:async )?function ${name}\\(`));
  assert(start >= 0, `function ${name} not found — did it move? update this contract`);
  const rest = code.slice(start + 1);
  const end = rest.search(new RegExp(`\\n${indent}(?:async )?function \\w`));
  return end < 0 ? rest : rest.slice(0, end);
}

function asks(body: string, predicate: string): boolean {
  return new RegExp(`formRefusal\\(\\s*askPossibility\\(\\s*\\(\\)\\s*=>\\s*${predicate}\\(`).test(body);
}

const HANDLERS: [string, string, string, string][] = [
  // [file, handler, predicate, indent]
  ["../routes/feed/+page.svelte", "handleCompose", "feedPostSubmitPossibility", "  "],
  ["../routes/feed/+page.svelte", "handleCreateFeed", "feedCreatePossibility", "  "],
  ["../routes/feed/+page.svelte", "handleSubscribeBridgeFeed", "feedBridgeSubscribePossibility", "  "],
  ["./feed-post-host.svelte.ts", "submitReply", "feedReplySubmitPossibility", "  "],
  ["../routes/search/+page.svelte", "handleSearch", "searchQueryPossibility", "  "],
  ["./components/NostrSettingsSection.svelte", "linkAccount", "nostrLinkPossibility", "  "],
  ["./components/NostrSettingsSection.svelte", "addRelay", "nostrAddRelayPossibility", "  "],
  ["./components/NostrSettingsSection.svelte", "addFollow", "nostrAddFollowPossibility", "  "],
  // The read-gated affordances (`ui/README.md` § Offer only what is
  // possible, rules 5, 6): each handler asks the page's whole answer — the
  // read's, the features gate's, ANDed with the draft's where there is one.
  ["./components/NostrSettingsSection.svelte", "addZapSigner", "zapSignerAddAnswer", "  "],
  ["./components/payments/ClaimRedeem.svelte", "redeem", "redeemAnswer", "  "],
  ["../routes/profile/[[actorId]]/+page.svelte", "follow", "followAnswer", "  "],
  ["../routes/profile/[[actorId]]/+page.svelte", "toggleBlock", "blockAnswer", "  "],
  ["../routes/notifications/+page.svelte", "markAllRead", "markReadAnswer", "  "],
  ["../routes/notifications/+page.svelte", "clearAll", "clearAnswer", "  "],
];

Deno.test("every gated submit asks its shared predicate before it runs", async () => {
  for (const [file, handler, predicate, indent] of HANDLERS) {
    const body = fnBody(await read(file), handler, indent);
    assert(asks(body, predicate), `${file} ${handler} does not refuse through formRefusal(askPossibility(() => ${predicate}(…)))`);
  }
});

const BUTTONS: [string, string, string][] = [
  // [file, ui id constant, gate variable]
  ["./components/FeedComposeBar.svelte", "POST_SUBMIT_BUTTON", "submitGate"],
  ["../routes/feed/+page.svelte", "POST_SUBMIT_BUTTON", "postGate"],
  ["../routes/feed/+page.svelte", "CREATE_FEED", "createFeedGate"],
  ["../routes/feed/+page.svelte", "BRIDGE_FORM_SUBSCRIBE_BUTTON", "bridgeSubscribeGate"],
  ["./components/FeedReplyDialog.svelte", "FEED_REPLY_SUBMIT_BUTTON", "submitGate"],
  ["../routes/search/+page.svelte", "SEARCH_SUBMIT_BUTTON", "submitGate"],
  ["./components/NostrSettingsSection.svelte", "NOSTR_LINK_BUTTON", "linkGate"],
  ["./components/NostrSettingsSection.svelte", "NOSTR_ADD_RELAY", "addRelayGate"],
  ["./components/NostrSettingsSection.svelte", "NOSTR_ADD_FOLLOW", "addFollowGate"],
  ["./components/NostrSettingsSection.svelte", "NOSTR_ZAP_SIGNER_ADD_BTN", "zapSignerAddGate"],
  ["../routes/profile/[[actorId]]/+page.svelte", "PROFILE_FOLLOW_BUTTON", "followGate"],
  ["../routes/profile/[[actorId]]/+page.svelte", "PROFILE_BLOCK_BUTTON", "blockGate"],
  ["../routes/notifications/+page.svelte", "NOTIFICATION_MARK_READ", "markReadGate"],
  ["../routes/notifications/+page.svelte", "NOTIFICATION_CLEAR", "clearGate"],
];

/** The page answers the read-gated affordances compose: each from its shared
 *  wasm answer, never an app-local test of the read or the rows. */
const ANSWERS: [string, string, string[]][] = [
  // [file, answer function, shared asks it must make]
  ["./components/NostrSettingsSection.svelte", "zapSignerAddAnswer", ["zapSignerPlaneAnswer", "nostrZapSignerAddPossibility"]],
  ["./components/NostrSettingsSection.svelte", "zapSignerPlaneAnswer", ["featureAnswer", "featuresGatePossibility", "'zaps'"]],
  ["./components/payments/ClaimRedeem.svelte", "redeemAnswer", ["featureAnswer", "'payments'"]],
  ["../routes/notifications/+page.svelte", "markReadAnswer", ["notificationsMarkAllReadPossibility(listRead)"]],
  ["../routes/notifications/+page.svelte", "clearAnswer", ["notificationsClearAllPossibility(listRead, notifications.length)"]],
];

Deno.test("every read-gated answer is the shared one", async () => {
  for (const [file, fn, calls] of ANSWERS) {
    const body = fnBody(await read(file), fn);
    for (const call of calls) assert(body.includes(call), `${file} ${fn} does not ask ${call}`);
  }
});

Deno.test("the redeem block is left out whole when the payments plane is absent", async () => {
  const code = await read("./components/payments/ClaimRedeem.svelte");
  const shownAt = code.indexOf("{#if gate.shown}");
  assert(shownAt >= 0, "the redeem block is not behind gate.shown");
  for (const marker of ["redeem_claim_title", "SUBSCRIPTION_CLAIM_REDEEM_INPUT", "SUBSCRIPTION_CLAIM_REDEEM_BUTTON"]) {
    assert(code.indexOf(marker, shownAt) > shownAt, `${marker} sits outside the gate.shown block`);
  }
  assert(/disabled=\{gate\.disabled\}/.test(code), "the redeem button does not read the gate");
});

/** The `<button>…</button>` carrying `data-testid={IDS.<id>}` (an arrow's
 *  `=>` makes the tag's own `>` unfindable by search, so take it whole). */
function buttonTag(code: string, id: string): string {
  const at = code.indexOf(`data-testid={IDS.${id}}`);
  assert(at >= 0, `IDS.${id} not found`);
  return code.slice(code.lastIndexOf("<button", at), code.indexOf("</button>", at));
}

Deno.test("every gated submit is painted from its formGate (disabled + reason on title)", async () => {
  for (const [file, id, gate] of BUTTONS) {
    const code = await read(file);
    assert(new RegExp(`${gate}\\s*=\\s*\\$derived\\(\\s*formGate\\(`).test(code), `${file}: ${gate} is not a $derived formGate`);
    const tag = buttonTag(code, id);
    assert(tag.includes(`${gate}.disabled`), `${file} ${id}: disabled= does not read ${gate}`);
    assert(tag.includes(`${gate}.reason`), `${file} ${id}: title= does not carry ${gate}'s reason`);
  }
});

Deno.test("the Connected apps section is offered from the shared signing-mode answer", async () => {
  const code = await read("./components/NostrSettingsSection.svelte");
  assert(/nostrConnectedAppsPossibility\(/.test(code), "Connected apps does not read nostrConnectedAppsPossibility");
  assert(!/mode === 'generated'/.test(code), "an app-local signing-mode check is back");
});

Deno.test("the compose dialog closes only on a saved post", async () => {
  const code = await read("../routes/feed/+page.svelte");
  assert(/async function handleCompose\(\): Promise<boolean>/.test(code), "handleCompose no longer answers whether it saved");
  assert(/return succeeded;/.test(fnBody(code, "handleCompose")), "handleCompose does not return its success");
  assert(/if \(await handleCompose\(\)\) showComposeDialog = false;/.test(code), "the compose dialog closes without asking whether the post saved");
  assert(!/await handleCompose\(\);\s*showComposeDialog = false/.test(code), "the optimistic close is back");
});

Deno.test("the reply dialog closes only on a saved reply", async () => {
  const body = fnBody(await read("./feed-post-host.svelte.ts"), "submitReply");
  const send = body.search(/manager\.(reply|replyPublicConfirmed|prepareSealedReply)\(/);
  assert(send >= 0, "submitReply sends nothing?");
  const closes = [...body.matchAll(/closeReplyDialog\(\)/g)].map((m) => m.index!);
  assert(closes.length > 0, "submitReply never closes the dialog");
  for (const at of closes) {
    assert(at > send, "submitReply closes the dialog before the send answered (optimistic close)");
    assert(/if \(saved[^)]*\)\s*$/.test(body.slice(Math.max(0, at - 60), at)), "a close is not behind the saved flag");
  }
});

for (const [handler, closeStmt] of [
  ["handleCreateFeed", "showCreateForm = false;"],
  ["handleSubscribeBridgeFeed", "showBridgeSubscribeForm = false;"],
] as const) {
  Deno.test(`${handler} closes only after its save, and a failure paints error-message`, async () => {
    const body = fnBody(await read("../routes/feed/+page.svelte"), handler);
    const tryAt = body.indexOf("try {");
    const awaitAt = body.indexOf("await manager.", tryAt);
    const closeAt = body.indexOf(closeStmt);
    const catchAt = body.indexOf("} catch (e) {");
    assert(tryAt >= 0 && awaitAt > tryAt, `${handler}: no awaited save inside try`);
    assert(closeAt > awaitAt && closeAt < catchAt, `${handler}: the form closes outside the success path`);
    const catchBody = body.slice(catchAt, body.indexOf("} finally {", catchAt));
    assert(/loadError = /.test(catchBody), `${handler}: a failed save does not reach error-message`);
  });
}
