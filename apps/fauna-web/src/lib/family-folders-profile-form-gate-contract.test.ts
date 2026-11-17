// The family, folders and profile form-submit contract (`docs/goal/ui/README.md`
// § Offer only what is possible, rule 4's form case; `family-safety.md`,
// `folders.md`, `profile.md` § Errors & edge cases), asserted over the source
// as a general invariant (testing.md convention 17) — the paint and refusal
// themselves are pinned by `form-gate.test.ts`, the predicates in shared Rust
// (`fauna_client_family::forms`, `fauna_client_folders::forms`,
// `fauna_client_subscriptions::forms`). The twin of
// `feed-form-gate-contract.test.ts`.
//
// Four halves:
//  1. Every gated submit handler ASKS its shared predicate through
//     `formRefusal(askPossibility(() => <predicate>(…)))` before it runs, so a
//     blocked draft refuses with the painted reason instead of a bare return.
//  2. Every gated submit button is painted from a `formGate` of that predicate.
//  3. The transfer refuses with its OWN reason keys — the contact-add string is
//     never reused — and no app-local hex/blank check stands in for a predicate.
//  4. The folder share form closes only once the share has been issued: the
//     close sits after the awaited `foldersShareSet`, inside the `try`, and a
//     not-ready app (no live session) refuses before anything — the form and
//     what was typed stay. The Family tab gate reads `familyHasSurface`.
//
// Crude like its siblings: reads source as TEXT with comments stripped.

import { assert } from "jsr:@std/assert";
import { stripComments } from "./source-contract.ts";

const read = (rel: string) => Deno.readTextFile(new URL(rel, import.meta.url)).then(stripComments);

const FAMILY = "../routes/family/+page.svelte";
const FOLDERS = "./components/FoldersSection.svelte";
const PROFILE = "../routes/profile/[[actorId]]/+page.svelte";
const REDEEM = "./components/payments/ClaimRedeem.svelte";
const CLAIM = "./components/payments/ClaimSection.svelte";
const PROVIDER = "./components/payments/ProviderSection.svelte";

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

const HANDLERS: [string, string, string][] = [
  // [file, handler, predicate]
  [FAMILY, "addContact", "familyContactAddPossibility"],
  [FAMILY, "proposeTransfer", "familyTransferPossibility"],
  [FAMILY, "savePolicy", "familyPolicySavePossibility"],
  [FOLDERS, "doFollow", "folderFollowPossibility"],
  [FOLDERS, "doShare", "folderSharePossibility"],
  [PROFILE, "saveForm", "subscriptionTierFormPossibility"],
  // The redeem's answer is the `payments` gate folded with the code draft
  // (`redeemAnswer`, pinned to the code predicate below).
  [REDEEM, "redeem", "redeemAnswer"],
  // The own-tier and mail gates (folders.md / profile.md § Errors & edge
  // cases): each answer folds the page's read word, pinned below.
  [FOLDERS, "changeWebdav", "webdavAnswer"],
  [FOLDERS, "changePaywallTier", "paywallAnswer"],
  [FOLDERS, "wizardNext", "folderWizardNextPossibility"],
  [FOLDERS, "wizardCreate", "folderWizardCreatePossibility"],
  [CLAIM, "mint", "mintAnswer"],
  [PROVIDER, "openForm", "addAnswer"],
];

Deno.test("every gated family/folders/profile submit asks its shared predicate before it runs", async () => {
  for (const [file, handler, predicate] of HANDLERS) {
    const body = fnBody(await read(file), handler);
    assert(asks(body, predicate), `${file} ${handler} does not refuse through formRefusal(askPossibility(() => ${predicate}(…)))`);
    // The refusal comes before any await (nothing runs on a blocked draft).
    const refuseAt = body.indexOf("formRefusal(");
    const awaitAt = body.indexOf("await ");
    assert(awaitAt < 0 || refuseAt < awaitAt, `${file} ${handler}: something is awaited before the refusal`);
  }
});

const BUTTONS: [string, string, string, string][] = [
  // [file, data-testid expression, gate variable, predicate]
  [FAMILY, "IDS.FAMILY_CONTACT_ADD_BUTTON", "contactAddGate", "familyContactAddPossibility"],
  [FAMILY, "IDS.FAMILY_TRANSFER_BUTTON", "transferGate", "familyTransferPossibility"],
  [FAMILY, "IDS.FAMILY_POLICY_SAVE_BUTTON", "policySaveGate", "familyPolicySavePossibility"],
  [FOLDERS, "IDS.FOLDER_FOLLOW_CONFIRM", "followGate", "folderFollowPossibility"],
  [FOLDERS, "IDS.FOLDER_SHARE_CONFIRM", "shareGate", "folderSharePossibility"],
  [PROFILE, "IDS.SUBSCRIPTION_TIER_FORM_SAVE", "tierSaveGate", "subscriptionTierFormPossibility"],
  [REDEEM, "PAYMENTS_IDS.SUBSCRIPTION_CLAIM_REDEEM_BUTTON", "gate", "redeemAnswer"],
  [FOLDERS, "IDS.WIZARD_NEXT_BUTTON", "wizardNextGate", "folderWizardNextPossibility"],
  [FOLDERS, "IDS.WIZARD_CREATE_BUTTON", "wizardCreateGate", "folderWizardCreatePossibility"],
  [CLAIM, "PAYMENTS_IDS.SUBSCRIPTION_CLAIM_MINT_BUTTON", "mintGate", "mintAnswer"],
  [PROVIDER, "PAYMENTS_IDS.SUBSCRIPTION_PROVIDER_ADD_BUTTON", "addGate", "addAnswer"],
];

/** The `<button>…</button>` carrying `data-testid={<id>}` (an arrow's `=>`
 *  makes the tag's own `>` unfindable by search, so take it whole). */
function buttonTag(code: string, id: string): string {
  const at = code.indexOf(`data-testid={${id}}`);
  assert(at >= 0, `${id} not found`);
  return code.slice(code.lastIndexOf("<button", at), code.indexOf("</button>", at));
}

Deno.test("every gated family/folders/profile submit is painted from its formGate (disabled + reason on title)", async () => {
  for (const [file, id, gate, predicate] of BUTTONS) {
    const code = await read(file);
    const decl = code.match(new RegExp(`${gate}\\s*=\\s*\\$derived\\(\\s*formGate\\(([\\s\\S]*?)\\n  \\);`));
    assert(decl, `${file}: ${gate} is not a $derived formGate`);
    assert(decl[1].includes(`${predicate}(`), `${file}: ${gate} is not painted from ${predicate}`);
    const tag = buttonTag(code, id);
    assert(tag.includes(`${gate}.disabled`), `${file} ${id}: disabled does not read ${gate}`);
    assert(tag.includes(`${gate}.reason`), `${file} ${id}: title= does not carry ${gate}'s reason`);
  }
});

Deno.test("the redeem's answer folds the shared code predicate after the payments gate", async () => {
  const body = fnBody(await read(REDEEM), "redeemAnswer");
  assert(/andPossibility\(\s*featureAnswer\(/.test(body), "redeemAnswer does not start from the payments gate");
  assert(body.includes("subscriptionRedeemCodePossibility(claimCode)"), "redeemAnswer does not fold the code predicate");
});

Deno.test("the transfer has its own reasons and no app-local id check stands in for a predicate", async () => {
  const code = await read(FAMILY);
  assert(!/contact_add_invalid_actor_id/.test(code), "a hard-coded contact-add string is back on the family page");
  for (const handler of ["addContact", "proposeTransfer"]) {
    const body = fnBody(code, handler);
    assert(!/actionError = t\.family\./.test(body), `${handler}: a hard-coded refusal string is back`);
  }
});

Deno.test("the folder share form closes only once the share is issued; follow/share never return silently", async () => {
  const code = await read(FOLDERS);
  const share = fnBody(code, "doShare");
  // Readiness folds in the LIVE conversations session, never a fresh build.
  assert(/function shareReady\(\)[\s\S]*?conversationsManagerIfReady\(\) !== null/.test(code), "share readiness does not read the live session");
  assert(!/getConversationsManager\(\)/.test(share), "doShare builds a session instead of using the live one");
  const tryAt = share.indexOf("try {");
  const sendAt = share.indexOf("await mgr.foldersShareSet(", tryAt);
  const closeAt = share.indexOf("shareDialogFs = null;");
  const clearAt = share.indexOf("shareInput = '';");
  const catchAt = share.indexOf("} catch (e: unknown) {");
  assert(tryAt >= 0 && sendAt > tryAt, "doShare: no awaited share inside try");
  assert(closeAt > sendAt && closeAt < catchAt, "doShare: the form closes outside the success path");
  assert(clearAt > sendAt && clearAt < catchAt, "doShare: what was typed is dropped outside the success path");
  for (const handler of ["doShare", "doFollow"]) {
    const body = fnBody(code, handler);
    assert(!/if \(!raw\) return;|if \(!owner \|\| !name\) return;/.test(body), `${handler}: a silent blank-box return is back`);
    assert(/if \(refusal !== null\) \{ error = refusal; return; \}/.test(body), `${handler}: the refusal does not reach error-message`);
  }
});

Deno.test("the Family tab gate reads the shared has-family rule", async () => {
  const code = await read("./components/RootShell.svelte");
  const decl = code.match(/let hasFamilyRelationship = \$derived\(([\s\S]*?)\n  \);/);
  assert(decl, "hasFamilyRelationship moved? update this contract");
  assert(/familyHasSurface\(/.test(decl[1]), "the Family tab gate does not read familyHasSurface");
  assert(!/\.length \?\? 0\) > 0/.test(decl[1]), "an SPA-local wards/transfers re-derivation is back");
});

Deno.test("the feature-limit editor's Save is painted from, and refuses by, the shared draft answer", async () => {
  const code = await read("./components/FeaturePolicyEditor.svelte");
  const write = fnBody(code, "write");
  assert(
    /formRefusal\(\s*askPossibility\(\s*\(\)\s*=>\s*editor\.savePossibility\(/.test(write),
    "write(false) must ask editor.savePossibility() before anything runs",
  );
  const repaint = fnBody(code, "repaint");
  assert(/editor\.savePossibility\(/.test(repaint), "every repaint re-reads the save answer");
  const saves = code.match(/FEATURE_POLICY_EDITOR_SAVE_BUTTON\}[\s\S]*?onclick/g) ?? [];
  assert(saves.length === 3, `three tiers' Save buttons, found ${saves.length}`);
  for (const s of saves) {
    assert(/saveGate\.disabled/.test(s) && /saveGate\.reason/.test(s), "each Save paints from saveGate");
  }
});

// The own-tier and mail gates (`folders.md` § Errors & edge cases, *Own-tier
// and mail gates*; `profile.md` § Errors & edge cases, *A tier before what
// sells it*): an unanswered read is never "you have none". Each answer hands
// the shared predicate the page's READ WORD, a failed read never resets the
// answered value to "none", and the cross-app `folder-paywall-hint` marks the
// answered-no-tier line alone.
Deno.test("the own-tier and mail gates keep a pending or failed read apart from 'none'", async () => {
  const folders = await read(FOLDERS);
  const answers: [string, string, string, RegExp][] = [
    [folders, "webdavAnswer", "folderWebdavServePossibility(webdavRead,", /webdavRead = readFailed\(webdavRead\)/],
    [folders, "paywallAnswer", "folderPaywallPossibility(ownTiersRead,", /ownTiersRead = readFailed\(ownTiersRead\)/],
  ];
  for (const [code, fn, call, failed] of answers) {
    assert(fnBody(code, fn).includes(call), `${fn} does not hand the predicate its read word`);
    assert(failed.test(code), `${fn}: a failed read does not record readFailed`);
  }
  assert(!/canServeWebdav = false/.test(folders), "a failed custody read reset the answer to 'no mail key'");
  assert(!/ownTiers = \[\]/.test(folders), "a failed tier read reset the answer to 'no tier'");
  for (const [gate, id] of [["webdavGate", "IDS.FOLDER_WEBDAV_TOGGLE"], ["paywallGate", "IDS.FOLDER_PAYWALL_TIER_SELECT"]]) {
    assert(new RegExp(`${gate}\\s*=\\s*\\$derived\\(\\s*formGate\\(`).test(folders), `${gate} is not a $derived formGate`);
    const at = folders.indexOf(`data-testid={${id}}`);
    assert(at >= 0, `${id} not found`);
    // The control's attributes up to its handler (the toggle and select both
    // put `onchange` last).
    const tag = folders.slice(at, folders.indexOf("onchange", at));
    assert(tag.includes(`disabled={${gate}.disabled}`), `${id}: disabled does not read ${gate}`);
  }
  const hint = folders.indexOf("data-testid={IDS.FOLDER_PAYWALL_HINT}");
  const guard = folders.lastIndexOf("{#if", hint);
  assert(/\{#if ownTiersRead === 'resolved'\}/.test(folders.slice(guard, hint)), "folder-paywall-hint is shown for an unanswered read");
  assert(!/continueEnabled\(|create_enabled/.test(folders), "the wizard paints from a page-local read of the snapshot's flags");

  const page = await read(PROFILE);
  assert(/tiers = await subscriptionsTiersList\(s\);\s*tiersRead = 'resolved';/.test(page), "the profile does not mark the tier read answered");
  assert(/tiersRead = readFailed\(tiersRead\)/.test(page), "the profile does not record a failed tier read");
  for (const [file, fn, call] of [[CLAIM, "mintAnswer", "subscriptionClaimMintPossibility(tiersRead,"], [PROVIDER, "addAnswer", "subscriptionProviderAddPossibility(tiersRead,"]]) {
    assert(fnBody(await read(file), fn).includes(call), `${file} ${fn} does not hand the predicate its read word`);
  }
  // The provider form opens only past the refusal.
  const open = fnBody(await read(PROVIDER), "openForm");
  assert(open.indexOf("formRefusal(") < open.indexOf("formVisible = true"), "the provider form opens before the add's answer is asked");
});
