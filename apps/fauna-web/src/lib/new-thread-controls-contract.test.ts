// The new-thread composer offers only what it can send
// (`docs/goal/ui/conversations.md` § Errors & edge cases → *The new-thread
// composer offers only what it can send*; `docs/goal/ui/README.md` § Offer
// only what is possible, rules 6 and 7), asserted over the source as a general
// invariant (testing.md convention 17). The answer itself is shared Rust's
// (`new_thread_possibility`, pinned there and over its wasm twin in
// `libs/fauna-wasm/tests/new_thread_control_possibility.rs`); this pins that the
// SPA paints the three controls from it, asks it before acting, and never lets a
// Keep failure go unsaid.
//
// Crude like its siblings: reads source as TEXT with comments stripped.

import { assert } from "jsr:@std/assert";
import { stripComments } from "./source-contract.ts";

const read = (rel: string) => Deno.readTextFile(new URL(rel, import.meta.url)).then(stripComments);
const PAGE = "../routes/conversations/+page.svelte";

/** The body of `function <name>(` up to the next top-level function. */
function fnBody(code: string, name: string): string {
  const start = code.search(new RegExp(`(?:async )?function ${name}\\(`));
  assert(start >= 0, `function ${name} not found — did it move? update this contract`);
  const rest = code.slice(start + 1);
  const end = rest.search(/\n {2}(?:async )?function \w/);
  return end < 0 ? rest : rest.slice(0, end);
}

/** Every `<…data-testid={IDS.<id>}…>` element opening tag (or the enclosing label). */
function tagsOf(code: string, id: string): string[] {
  const out: string[] = [];
  let at = code.indexOf(`data-testid={IDS.${id}}`);
  assert(at >= 0, `IDS.${id} not found`);
  while (at >= 0) {
    const open = code.lastIndexOf("<", at);
    out.push(code.slice(open, code.indexOf(">", at) + 1));
    at = code.indexOf(`data-testid={IDS.${id}}`, at + 1);
  }
  return out;
}

Deno.test("the page asks the shared new-thread answer", async () => {
  const page = await read(PAGE);
  assert(/newThreadControlPossibility\(/.test(page), "the page never asks newThreadControlPossibility");
});

Deno.test("every new-thread handler asks the answer its control painted before it acts", async () => {
  const page = await read(PAGE);
  for (const [handler, control] of [
    ["send", "send"],
    ["toggleTopic", "topic"],
    ["stageAttachments", "attach"],
  ]) {
    assert(
      fnBody(page, handler).includes(`newThreadPossible('${control}')`),
      `${handler} does not ask newThreadPossible('${control}')`,
    );
  }
  assert(
    /answer\.state === 'blocked'\) pageError = resolveLocalized\(answer\.reason\)/.test(
      fnBody(page, "newThreadPossible"),
    ),
    "a refused new-thread control does not say why on error-message",
  );
});

Deno.test("the three new-thread controls paint from the answer", async () => {
  const page = await read(PAGE);
  // Send: both its buttons (the room-founding one and the plain one) are
  // disabled from the new-thread answer in new mode.
  for (const tag of tagsOf(page, "DM_SEND_BUTTON")) {
    assert(/disabled=\{(ntc\.send\.disabled|sendBlocked)\}/.test(tag), `Send paints off something else: ${tag}`);
  }
  assert(/sendBlocked = \$derived\(mode === 'new' \? ntc\.send\.disabled/.test(page), "sendBlocked ignores ntc in new mode");
  for (const tag of tagsOf(page, "ATTACHMENT_BUTTON")) {
    assert(/disabled=\{attachBlocked\}/.test(tag), `attach paints off something else: ${tag}`);
  }
  assert(/attachBlocked = \$derived\(mode === 'new' \? ntc\.attach\.disabled/.test(page), "attachBlocked ignores ntc in new mode");
  assert(
    tagsOf(page, "TOPIC_TOGGLE_BUTTON").some((tag) => /disabled=\{ntc\.topic\.disabled\}/.test(tag)),
    "the new-thread topic toggle does not paint from ntc",
  );
  assert(/\{:else if mode === 'new' && ntc\.topic\.shown\}/.test(page), "the new-thread topic toggle is not guarded by its answer");
});

Deno.test("a blocked new-thread control carries its reason on the control itself", async () => {
  // The reason rides the tagged element's own `title` — what a person hovering
  // the control reads and what the e2e driver's `blocked_reason` reads — never
  // only a wrapping label's (`ui/README.md` § Offer only what is possible, rule 2).
  const page = await read(PAGE);
  for (const tag of tagsOf(page, "ATTACHMENT_BUTTON")) {
    assert(/title=\{mode === 'new' && ntc\.attach\.reason/.test(tag), `attach carries no reason of its own: ${tag}`);
  }
  assert(
    tagsOf(page, "TOPIC_TOGGLE_BUTTON").some((tag) => /title=\{ntc\.topic\.reason/.test(tag)),
    "the new-thread topic toggle carries no reason of its own",
  );
});

Deno.test("a failed Keep says so on error-message", async () => {
  const page = await read(PAGE);
  assert(
    fnBody(page, "keepMember").includes("t.settings.recovery_kit.review_keep_failed("),
    "keepMember's failure is not the review_keep_failed sentence",
  );
});
