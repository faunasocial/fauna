// The personal form submits' contract (`docs/goal/ui/settings.md` § Errors &
// edge cases, *Personal form submits paint from their draft*;
// `docs/goal/ui/connected-apps.md` § Errors & edge cases; mechanics
// `docs/goal/ui/README.md` § Offer only what is possible), asserted over the
// source as a general invariant (testing.md convention 17) — the paint and
// refusal themselves are pinned by `form-gate.test.ts`, the predicates in
// shared Rust (`fauna_client_config::muted_word_add_possibility`,
// `fauna_client_personalization::drafts`, `LabelerCatalogSnapshot::
// subscribe_possibility`, `fauna_client_connected_apps::connect_code_possibility`,
// `ConsentCardRow::approve_possibility`). The twin of
// `family-folders-profile-form-gate-contract.test.ts`.
//
// Two halves:
//  1. Every gated press ASKS its shared answer through
//     `formRefusal(askPossibility(() => <answer>(…)))` before anything is
//     awaited, so a blocked draft refuses with the painted reason instead of a
//     bare return.
//  2. Every gated button reads a `formGate`'s `disabled` and carries its
//     reason on `title`, and no app-local blank check stands in for an answer.
//
// Crude like its siblings: reads source as TEXT with comments stripped.

import { assert } from "jsr:@std/assert";
import { stripComments } from "./source-contract.ts";

const read = (rel: string) => Deno.readTextFile(new URL(rel, import.meta.url)).then(stripComments);

const SETTINGS = "../routes/settings/[[subpage]]/+page.svelte";
const PERSONALIZATION = "./components/PersonalizationSection.svelte";
const LABELERS = "./components/LabelerCatalogSection.svelte";
const CONNECTED = "./components/ConnectedAppsSection.svelte";

/** The body of `function <name>(` up to the next function at `indent`. */
function fnBody(code: string, name: string, indent = "  "): string {
  const start = code.search(new RegExp(`(?:async )?function ${name}\\(`));
  assert(start >= 0, `function ${name} not found — did it move? update this contract`);
  const rest = code.slice(start + 1);
  const end = rest.search(new RegExp(`\\n${indent}(?:async )?function \\w`));
  return end < 0 ? rest : rest.slice(0, end);
}

function asks(body: string, answer: string): boolean {
  return new RegExp(`formRefusal\\(\\s*askPossibility\\(\\s*\\(\\)\\s*=>\\s*${answer}\\(`).test(body);
}

const HANDLERS: [string, string, string][] = [
  // [file, handler, answer]
  [SETTINGS, "addMutedWord", "mutedWordAddPossibility"],
  [SETTINGS, "saveSpamPreferences", "spamSavePreferencesPossibility"],
  [PERSONALIZATION, "submitTopic", "trainedFactorNamePossibility"],
  [PERSONALIZATION, "openPublishSheet", "publishOpenAnswer"],
  [PERSONALIZATION, "submitPublish", "publishSubmitAnswer"],
  [LABELERS, "subscribe", "subscribePossibility"],
  [CONNECTED, "submitCode", "connectCodePossibility"],
  [CONNECTED, "resolveRequest", "requestApprovePossibility"],
];

Deno.test("every personal form press asks its shared answer before anything runs", async () => {
  for (const [file, handler, answer] of HANDLERS) {
    const body = fnBody(await read(file), handler);
    assert(asks(body, answer), `${file} ${handler} does not refuse through formRefusal(askPossibility(() => ${answer}(…)))`);
    const refuseAt = body.indexOf("formRefusal(");
    const awaitAt = body.indexOf("await ");
    assert(awaitAt < 0 || refuseAt < awaitAt, `${file} ${handler}: something is awaited before the refusal`);
  }
});

const BUTTONS: [string, string, string][] = [
  // [file, data-testid expression, gate expression]
  [SETTINGS, "IDS.MUTED_WORD_ADD_BUTTON", "mutedWordAddGate"],
  [SETTINGS, "IDS.SAVE_SPAM_PREFS", "saveSpamGate"],
  [PERSONALIZATION, "IDS.PERSONALIZATION_TRAINED_FACTOR_CREATE_BUTTON", "nameGate"],
  [PERSONALIZATION, "IDS.PERSONALIZATION_TRAINED_FACTOR_PUBLISH_BUTTON", "publishGate"],
  [PERSONALIZATION, "IDS.PERSONALIZATION_TRAINED_FACTOR_PUBLISH_SUBMIT_BUTTON", "publishSubmitGate"],
  [LABELERS, "IDS.LABELER_CATALOG_ITEM_SUBSCRIBE_BUTTON", "subscribeGates[i]"],
  [CONNECTED, "IDS.CONNECTED_APPS_CONNECT_SUBMIT", "connectGate"],
  [CONNECTED, "IDS.CONNECTED_APPS_REQUEST_APPROVE", "approveGate"],
];

/** The `<button>…</button>` carrying `data-testid={<id>}`. */
function buttonTag(code: string, id: string): string {
  const at = code.indexOf(`data-testid={${id}}`);
  assert(at >= 0, `${id} not found`);
  return code.slice(code.lastIndexOf("<button", at), code.indexOf("</button>", at));
}

Deno.test("every personal form submit is painted from its formGate (disabled + reason on title)", async () => {
  for (const [file, id, gate] of BUTTONS) {
    const tag = buttonTag(await read(file), id);
    assert(tag.includes(`${gate}`) && tag.includes(".disabled"), `${file} ${id}: disabled does not read ${gate}`);
    assert(tag.includes(".reason"), `${file} ${id}: title= does not carry ${gate}'s reason`);
    assert(!/disabled=\{!\w+\.trim\(\)/.test(tag), `${file} ${id}: an app-local blank check is back`);
  }
});
