// The signed-out forms' submits paint from their shared draft answers and ask
// them again at the click (`docs/goal/ui/README.md` § Offer only what is
// possible, rule 4's form case and rule 6): `import-submit-button`,
// `recovery-entry-submit-button`, the door's `sessions-lockout-button`,
// `handle-check-button`, `invite-code-check-button`,
// `claim-code-submit-button`, `unlock-submit-button` and both
// `identity-stolen-button`s.
//
// The predicates are shared Rust and pinned there, blank / malformed /
// complete each (`fauna_onboarding_machine::drafts`,
// `OnboardingMachine::{claim_code_submit,handle_check}_possibility`,
// `fauna_client_accounts::passphrase_possibility`,
// `fauna_client_recovery::ceremony::stolen_entry_possibility`). The runtime
// halves below run web's own paint and refusal over each form's three answers
// as the predicate gives them (the reason keys are the predicates'); the source
// halves pin that each page reads its answer and keeps no gate of its own
// (testing.md convention 17). Crude like its siblings: the source halves read
// the files as TEXT with comments stripped.
import { assert, assertEquals } from 'jsr:@std/assert@1';
import { askPossibility, formGate, formRefusal } from '../form-gate.ts';
import { resolveLocalized, type LocalizedText } from '../i18n/localized.ts';
import type { Possibility } from '../possibility.ts';
import { stripComments } from '../source-contract.ts';

const read = (rel: string) => Deno.readTextFile(new URL(rel, import.meta.url)).then(stripComments);

const blocked = (key: string, args: Record<string, string> = {}): Possibility => ({
  state: 'blocked',
  reason: { key, args } as LocalizedText,
});
const POSSIBLE: Possibility = { state: 'possible' };

/** Each form's blank, malformed and complete drafts, answered as its shared
 *  predicate answers them. A form whose only draft check is blankness has no
 *  malformed arm (`null`). */
const FORMS: Record<string, { blank: Possibility; malformed: Possibility | null; complete: Possibility }> = {
  'import-submit-button': {
    blank: blocked('onboarding.identity_import.invalid_secret'),
    malformed: blocked('onboarding.identity_import.invalid_secret'),
    complete: POSSIBLE,
  },
  'recovery-entry-submit-button': {
    blank: blocked('onboarding.recovery_entry.invalid_kit'),
    malformed: blocked('onboarding.recovery_entry.account_malformed'),
    complete: POSSIBLE,
  },
  'sessions-lockout-button': {
    blank: blocked('onboarding.emergency_lockout.invalid_secret'),
    malformed: blocked('sessions.lockout_wrong_word', { word: 'LOCK' }),
    complete: POSSIBLE,
  },
  'handle-check-button': {
    blank: blocked('onboarding.handle_check.idle'),
    malformed: null,
    complete: POSSIBLE,
  },
  'invite-code-check-button': {
    blank: blocked('onboarding.invite_request.oob_idle'),
    malformed: null,
    complete: POSSIBLE,
  },
  'claim-code-submit-button': {
    blank: blocked('onboarding.claim_code.idle'),
    malformed: blocked('common.in_flight'),
    complete: POSSIBLE,
  },
  'unlock-submit-button': {
    blank: blocked('unlock.error_empty'),
    malformed: blocked('unlock.error_mismatch'),
    complete: POSSIBLE,
  },
  'identity-stolen-button': {
    blank: blocked('settings.recovery_kit.kit_phrase_required'),
    malformed: blocked('settings.recovery_kit.stolen_confirm_placeholder'),
    complete: POSSIBLE,
  },
};

for (const [id, drafts] of Object.entries(FORMS)) {
  Deno.test(`${id}: a blank or malformed draft is disabled with its reason and refused by it; a complete one goes`, () => {
    for (const answer of [drafts.blank, drafts.malformed].filter((a): a is Possibility => a !== null)) {
      const reason = resolveLocalized((answer as { reason: LocalizedText }).reason);
      assert(reason !== '', `${id}: the reason ${JSON.stringify(answer)} resolves to nothing`);
      assertEquals(formGate(askPossibility(() => answer)), { shown: true, disabled: true, reason });
      assertEquals(formRefusal(askPossibility(() => answer)), reason);
    }
    assertEquals(formGate(askPossibility(() => drafts.complete)).disabled, false);
    assertEquals(formRefusal(askPossibility(() => drafts.complete)), null);
  });
}

Deno.test('an answer not readable yet (the chunk still loading) is dead, and the click says so', () => {
  const gate = formGate(askPossibility(() => { throw new Error('wasm not initialized'); }));
  assert(gate.disabled);
  assert(formRefusal(null) !== null);
});

/** Every opening tag carrying `data-testid={IDS.<id>}`. */
function tagsOf(code: string, id: string): string[] {
  const out: string[] = [];
  let at = code.indexOf(`data-testid={IDS.${id}}`);
  assert(at >= 0, `IDS.${id} not found`);
  while (at >= 0) {
    const open = code.lastIndexOf('<', at);
    let close = code.indexOf('>', at);
    while (code[close - 1] === '=') close = code.indexOf('>', close + 1);
    out.push(code.slice(open, close + 1));
    at = code.indexOf(`data-testid={IDS.${id}}`, at + 1);
  }
  return out;
}

/** The body of `function <name>` (its first 900 characters). */
function bodyOf(code: string, name: string): string {
  const at = code.indexOf(`function ${name}(`);
  assert(at >= 0, `function ${name} not found`);
  return code.slice(at, at + 900);
}

Deno.test('the onboarding page paints each signed-out submit from its answer and asks it again', async () => {
  const page = await read('../../routes/onboarding/+page.svelte');
  const sites: [string, string][] = [
    ['IMPORT_SUBMIT_BUTTON', 'importGate'],
    ['RECOVERY_ENTRY_SUBMIT_BUTTON', 'recoveryEntryGate'],
    ['SESSIONS_LOCKOUT_BUTTON', 'lockoutGate'],
    ['HANDLE_CHECK_BUTTON', 'handleCheckGate'],
    ['INVITE_CODE_CHECK_BUTTON', 'inviteCodeGate'],
    ['CLAIM_CODE_SUBMIT_BUTTON', 'claimCodeGate'],
    ['IDENTITY_STOLEN_BUTTON', 'stolenEntryGate'],
  ];
  for (const [id, gate] of sites) {
    for (const tag of tagsOf(page, id)) {
      assert(tag.includes(`${gate}.disabled`), `${id} paints off something else: ${tag}`);
      assert(tag.includes(`title={${gate}.reason`), `${id} keeps its reason out of eyeshot: ${tag}`);
    }
  }
  for (const [fn, answer] of [
    ['importIdentity', 'importAnswer'],
    ['submitRecoveryEntry', 'recoveryEntryAnswer'],
    ['submitEmergencyLockout', 'lockoutAnswer'],
    ['onClaimCodeSubmit', 'claimCodeAnswer'],
    ['succeedFromLocked', 'stolenEntryAnswer'],
  ]) {
    assert(bodyOf(page, fn).includes(`refuse(${answer})`), `${fn} does not ask ${answer} before it acts`);
  }
  for (const tag of [...tagsOf(page, 'HANDLE_CHECK_BUTTON'), ...tagsOf(page, 'INVITE_CODE_CHECK_BUTTON')]) {
    assert(/refuse\((handleCheckAnswer|inviteCodeAnswer)\)/.test(tag), `the click does not ask again: ${tag}`);
  }
  // No gate of the page's own is left.
  assert(!page.includes("!== 'SUCCEED'"), 'the page compares the stolen confirm word itself');
  assert(!page.includes('LOCKOUT_CONFIRM_WORD'), 'the page keeps its own LOCK literal');
  assert(!page.includes('!handleValue.trim()'), 'handle-check keeps its own emptiness gate');
  assert(!page.includes('!oobCode.trim()'), 'invite-code-check keeps its own emptiness gate');
  assert(!/disabled=\{!\(claimCodeSnapshotValue\?\.submit_enabled/.test(page), 'claim submit paints off the snapshot bool');
});

Deno.test('Settings paints identity-stolen-button from stolenEntryPossibility and asks it again', async () => {
  const page = await read('../../routes/settings/[[subpage]]/+page.svelte');
  assert(page.includes('stolenEntryPossibility(recoveryPhrase, stolenConfirm)'), 'Settings never asks the stolen answer');
  for (const tag of tagsOf(page, 'IDENTITY_STOLEN_BUTTON')) {
    assert(tag.includes('stolenEntryGate.disabled'), `the stolen button paints off something else: ${tag}`);
  }
  assert(bodyOf(page, 'doSucceedIdentity').includes('formRefusal(askPossibility(stolenEntryAnswer))'),
    'doSucceedIdentity does not ask the stolen answer before the ceremony');
  assert(!page.includes("!== 'SUCCEED'"), 'Settings compares the stolen confirm word itself');
});

Deno.test('the unlock gate paints its submit from passphrasePossibility and keeps no check of its own', async () => {
  const gate = await read('../components/UnlockGate.svelte');
  assert(/passphrasePossibility\(passphrase, gate === 'create' \? confirm : null\)/.test(gate),
    'the gate never asks the shared passphrase answer');
  for (const tag of tagsOf(gate, 'UNLOCK_SUBMIT_BUTTON')) {
    assert(tag.includes('submitGate.disabled'), `the unlock submit paints off something else: ${tag}`);
  }
  assert(bodyOf(gate, 'submit').includes('formRefusal(askPossibility(submitAnswer))'), 'submit does not ask again');
  assert(!gate.includes('!passphrase)'), 'the gate keeps its own emptiness check');
  assert(!gate.includes('passphrase !== confirm'), 'the gate keeps its own mismatch check');
});
