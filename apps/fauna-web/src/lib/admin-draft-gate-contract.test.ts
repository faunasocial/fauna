// The admin forms' draft-validity contract (`docs/goal/behavior/admin.md`
// § Errors & edge cases → *A form's submit is live only for a draft the nest
// would accept*), asserted over the source as a general invariant (testing.md
// convention 17) — the paint and refusal themselves are pinned by
// `form-gate.test.ts` (`afterGate` included), the predicates in shared Rust
// (`fauna_client_admin::drafts`, `fauna_client_mail_settings`).
//
// Three halves:
//  1. Every admin submit the rule names is painted from ONE answer that calls
//     its shared draft predicate (`<predicate>(` appears in the page), and its
//     handler refuses at the press through `formRefusal` (or the mail page's
//     `admit`) on that same answer before any RPC.
//  2. The paint reads that answer through `formGate(<answer>)`.
//  3. No mail save falls back to the stored value: `parseU32`/`parseU64` (the
//     `?? prev` fallbacks) and the regex list parse are gone.
//
// Crude like its siblings: reads source as TEXT with comments stripped.

import { assert } from "jsr:@std/assert";
import { stripComments } from "./source-contract.ts";

const read = (rel: string) => Deno.readTextFile(new URL(rel, import.meta.url)).then(stripComments);

/** The body of `function <name>(` up to the next function at two-space indent. */
function fnBody(code: string, name: string): string {
  const start = code.search(new RegExp(`(?:async )?function ${name}\\(`));
  assert(start >= 0, `function ${name} not found — did it move? update this contract`);
  const rest = code.slice(start + 1);
  const end = rest.search(/\n {2}(?:async )?function \w/);
  return end < 0 ? rest : rest.slice(0, end);
}

// [page, predicate, answer expression, handler, refusal form in the handler]
const FORMS: [string, string, string, string, RegExp][] = [
  ["users", "adminRegistrationCeilingPossibility", "registrationAnswer", "saveRegistration", /formRefusal\(registrationAnswer\)/],
  ["users", "adminAdmitActorPossibility", "admitAnswer", "admitUser", /formRefusal\(admitAnswer\)/],
  ["settings", "adminTierCapsPossibility", "tierSaveAnswer(tier.name)", "saveTier", /formRefusal\(tierSaveAnswer\(name\)\)/],
  ["settings", "adminTierCreatePossibility", "addAnswer", "addTier", /formRefusal\(addAnswer\)/],
  ["settings", "adminMembershipSavePossibility", "membershipSaveAnswer(name)", "saveMembership", /formRefusal\(membershipSaveAnswer\(name\)\)/],
  ["nest", "adminServingPortPossibility", "servingPortAnswer", "saveServingPort", /formRefusal\(servingPortAnswer\)/],
  ["nest", "adminRegionPossibility", "regionAnswer", "saveRegion", /formRefusal\(regionAnswer\)/],
  ["calendar", "caldavPortDraftPossibility", "portAnswer", "handleSavePort", /formRefusal\(portAnswer\)/],
  ["aliases", "forwarderCreateDraftPossibility", "submitAnswer", "handleCreate", /formRefusal\(submitAnswer\)/],
  ["mail", "mailKnobsDraftPossibility", "spamAnswer", "handleSaveSpam", /admit\(spamAnswer\)/],
  ["mail", "mailKnobsDraftPossibility", "authAnswer", "handleSaveAuth", /admit\(authAnswer\)/],
  ["mail", "mailKnobsDraftPossibility", "submissionAnswer", "handleSaveSubmission", /admit\(submissionAnswer\)/],
  ["mail", "mailKnobsDraftPossibility", "imapAnswer", "handleSaveImap", /admit\(imapAnswer\)/],
  ["mail", "mailKnobsDraftPossibility", "outboundAnswer", "handleSaveOutbound", /admit\(outboundAnswer\)/],
  ["mail", "mailKnobsDraftPossibility", "aliasAnswer", "handleSaveAlias", /admit\(aliasAnswer\)/],
  ["mail", "mailKnobsDraftPossibility", "dmarcAnswer", "handleSaveDmarc", /admit\(dmarcAnswer\)/],
  ["mail", "mailMicrosoftDraftPossibility", "microsoftAnswer", "handleSaveMicrosoft", /admit\(microsoftAnswer\)/],
];

Deno.test("every admin submit is painted and refused from its shared draft answer", async () => {
  for (const [page, predicate, answer, handler, refusal] of FORMS) {
    const code = await read(`../routes/admin/${page}/+page.svelte`);
    assert(code.includes(`${predicate}(`), `${page}: ${predicate} is never asked`);
    assert(
      code.includes(`formGate(${answer}).disabled`),
      `${page}: the submit is not painted from formGate(${answer})`,
    );
    assert(refusal.test(fnBody(code, handler)), `${page}: ${handler} does not refuse at the press`);
  }
});

Deno.test("the mail page's refusal goes on error-message and nothing falls back", async () => {
  const code = await read("../routes/admin/mail/+page.svelte");
  assert(/formRefusal\(answer\)/.test(fnBody(code, "admit")), "admit does not ask formRefusal");
  assert(/error = refusal/.test(fnBody(code, "admit")), "admit does not paint error-message");
  for (const gone of ["parseU32(", "parseU64(", "linesToU64List("]) {
    assert(!code.includes(gone), `mail: ${gone} (a stored-value fallback) is back`);
  }
});
