// The admin pages' "controls wait for their read" contract
// (`docs/goal/behavior/admin.md` § Errors & edge cases, *Controls wait for
// their read*), asserted over the source as a general invariant (testing.md
// convention 17) — the paint, the published reason, the list line and the
// confirm press themselves are pinned by `admin-read-gate.test.ts`, the
// answers in shared Rust. The per-page list mirrors FaunaKit's
// `AdminControlsWaitForTheirReadTests.swift`.
//
// Each listed control:
//  1. is painted from the page's read answer (its page asks the shared face);
//  2. publishes the painted reason as `data-reason={reasonAttr(...)}` — the
//     automation `reason` attr (`drivers/base.py::painted_reason`);
//  3. its handler asks the answer again before any write.
//
// Crude like its siblings: reads source as TEXT with comments stripped.

import { assert } from "jsr:@std/assert";
import { stripComments } from "./source-contract.ts";

const read = (rel: string) => Deno.readTextFile(new URL(`../routes/admin/${rel}/+page.svelte`, import.meta.url)).then(stripComments);

/** The opening tag carrying `data-testid={IDS.<id>}` (the first one). */
function tagOf(code: string, id: string): string {
  const at = code.indexOf(`data-testid={IDS.${id}}`);
  assert(at >= 0, `IDS.${id} not found`);
  const open = code.lastIndexOf("<", at);
  let close = code.indexOf(">", at);
  while (code[close - 1] === "=") close = code.indexOf(">", close + 1);
  return code.slice(open, close + 1);
}

/** The body of `function <name>(` up to the next function at two-space indent. */
function fnBody(code: string, name: string): string {
  const start = code.search(new RegExp(`(?:async )?function ${name}\\(`));
  assert(start >= 0, `function ${name} not found — did it move? update this contract`);
  const rest = code.slice(start + 1);
  const end = rest.search(/\n {2}(?:async )?function \w/);
  return end < 0 ? rest : rest.slice(0, end);
}

// [page, the shared face the page asks, gated element ids, [handler, refusal]]
const PAGES: [string, string, string[], [string, RegExp][]][] = [
  ["nest", "writeAfterReadPossibility(", [
    "ADMIN_SERVICE_PAIRING_TOGGLE",
    "ADMIN_NEST_SERVING_PORT_INPUT",
    "ADMIN_NEST_SERVING_PORT_SAVE_BUTTON",
  ], [["togglePairing", /formRefusal\(pairingAnswer\)/], ["saveServingPort", /formRefusal\(servingPortAnswer\)/]]],
  ["nest", "selectPossibilityJson()", [
    "ADMIN_NEST_NAT_MODE_PUBLIC_RADIO",
    "ADMIN_NEST_NAT_MODE_PRIVATE_RADIO",
    "ADMIN_NEST_NAT_MODE_SAVE_BUTTON",
  ], [["selectNatMode", /formRefusal\(natSelectAnswer\)/], ["saveNatMode", /formRefusal\(natSaveAnswer\)/]]],
  ["mail", "writePossibility()", [
    "ADMIN_MAIL_ENABLED_TOGGLE",
    "ADMIN_MAIL_AUTO_ENABLE_NEW_USERS_TOGGLE",
    "ADMIN_MAIL_SPAM_BASELINE_STANDING_TOGGLE",
    "ADMIN_MAIL_SPAM_SAVE_BUTTON",
    "ADMIN_MAIL_AUTH_SAVE_BUTTON",
    "ADMIN_MAIL_SUBMISSION_SAVE_BUTTON",
    "ADMIN_MAIL_IMAP_SAVE_BUTTON",
    "ADMIN_MAIL_OUTBOUND_SAVE_BUTTON",
    "ADMIN_MAIL_ALIAS_SAVE_BUTTON",
    "ADMIN_MAIL_DMARC_SAVE_BUTTON",
    "ADMIN_MAIL_MICROSOFT_SAVE_BUTTON",
    "ADMIN_MAIL_PUBLISH_SPAM_BASELINE_BUTTON",
    "ADMIN_MAIL_LIST_UNSUBSCRIBE_SECRET_ROTATE_BUTTON",
    "ADMIN_MAIL_FORWARD_SRS_SECRET_ROTATE_BUTTON",
  ], [
    ["handleToggleEnabled", /formRefusal\(enabled \? enableAnswer : writeAnswer\)/],
    ["handleToggleAutoEnableNewUsers", /admitWrite\(\)/],
    ["handleToggleBaselineStanding", /admitWrite\(\)/],
    ["press", /admitWrite\(\)/],
  ]],
  ["calendar", "togglePossibility()", [
    "ADMIN_CALENDAR_ENABLED_TOGGLE",
    "ADMIN_CALENDAR_CALDAV_PORT_INPUT",
    "ADMIN_CALENDAR_CALDAV_PORT_SAVE_BUTTON",
  ], [["handleToggleEnabled", /formRefusal\(/], ["handleSavePort", /formRefusal\(portAnswer\)/]]],
  ["contacts", "togglePossibility()", ["ADMIN_CONTACTS_CARDDAV_ENABLED_TOGGLE"], [["handleToggleEnabled", /formRefusal\(/]]],
  ["files", "togglePossibility()", ["ADMIN_FILES_WEBDAV_ENABLED_TOGGLE"], [["handleToggleEnabled", /formRefusal\(/]]],
  ["aliases", "addPossibility()", [
    "ADMIN_ALIASES_FORWARDER_ADD_DOMAIN_SELECT",
    "ADMIN_ALIASES_FORWARDER_ADD_PATTERN_INPUT",
    "ADMIN_ALIASES_FORWARDER_ADD_TARGET_INPUT",
    "ADMIN_ALIASES_FORWARDER_ADD_SUBMIT_BUTTON",
  ], [["handleCreate", /formRefusal\(submitAnswer\)/]]],
  ["settings", "writeAfterReadPossibility(tiersRead", [
    "ADMIN_SETTINGS_TIER_ADD_NAME_INPUT",
    "ADMIN_SETTINGS_TIER_ADD_BUTTON",
  ], [["addTier", /formRefusal\(addAnswer\)/]]],
  ["bridges-pending", "confirmPossibility()", [
    "ADMIN_BRIDGES_REJECT_CONFIRM_BUTTON",
    "ADMIN_BRIDGES_ROTATE_CONFIRM_BUTTON",
    "ADMIN_BRIDGES_PENDING_SECTION",
    "ADMIN_BRIDGES_APPROVED_SECTION",
  ], [["confirmArmed", /beginConfirmPress\(machine\)/]]],
];

Deno.test("every read-decided admin control asks its answer, publishes its reason and refuses at the press", async () => {
  for (const [page, face, ids, handlers] of PAGES) {
    const code = await read(page);
    assert(code.includes(face), `${page}: ${face} is never asked`);
    for (const id of ids) {
      assert(/data-reason=\{reasonAttr\(/.test(tagOf(code, id)), `${page}: ${id} does not publish its painted reason`);
    }
    for (const [handler, refusal] of handlers) {
      assert(refusal.test(fnBody(code, handler)), `${page}: ${handler} does not ask the answer at the press`);
    }
  }
});

Deno.test("every read-gated admin page re-reads on entry and on reconnect, through the one trigger", async () => {
  for (const page of ["nest", "mail", "calendar", "contacts", "files", "aliases", "settings"]) {
    const code = await read(page);
    assert(code.includes("readOnEntryAndReconnect("), `${page}: does not re-read on entry and reconnect`);
    assert(!/onMount\([^)]*\{[^}]*\bload\w*\(/.test(code), `${page}: reads in onMount, which a same-route goto never re-runs`);
  }
});

Deno.test("a machine-backed page paints its read word once the wasm core is up, before the machine exists", async () => {
  for (const [page, fn] of [
    ["mail", "readPolicy"],
    ["calendar", "readMachine"],
    ["contacts", "readMachine"],
    ["files", "readMachine"],
    ["aliases", "readMachine"],
  ]) {
    const body = fnBody(await read(page), fn);
    assert(/await ensureWasm\(\);\s*repaint\(\);/.test(body), `${page}: ${fn} leaves the controls unpainted while the machine is built`);
  }
});

Deno.test("no toggle paints its state from a guessed false before its read", async () => {
  for (const [page, id, flag] of [
    ["calendar", "ADMIN_CALENDAR_ENABLED_TOGGLE", "readResolved"],
    ["contacts", "ADMIN_CONTACTS_CARDDAV_ENABLED_TOGGLE", "readResolved"],
    ["files", "ADMIN_FILES_WEBDAV_ENABLED_TOGGLE", "readResolved"],
    ["mail", "ADMIN_MAIL_ENABLED_TOGGLE", "policyRead"],
    ["mail", "ADMIN_MAIL_AUTO_ENABLE_NEW_USERS_TOGGLE", "policyRead"],
  ]) {
    const tag = tagOf(await read(page), id);
    assert(new RegExp(`checked=\\{${flag} && `).test(tag), `${page}: ${id} is checked before its read resolved`);
  }
});

Deno.test("the bridges and forwarder lists paint their empty text only once read", async () => {
  for (const [page, empty] of [
    ["bridges-pending", "t.admin.bridges_pending.empty"],
    ["bridges-pending", "t.admin.bridges_pending.approved_empty"],
    ["aliases", "t.admin.aliases_page.no_forwarders"],
  ]) {
    const code = await read(page);
    const at = code.indexOf(`{${empty}}`);
    assert(at >= 0, `${page}: ${empty} not found`);
    const before = code.slice(Math.max(0, at - 200), at);
    assert(/\{#if listReason !== null\}[\s\S]*\{:else if/.test(before), `${page}: ${empty} is painted before the list read resolved`);
  }
});
