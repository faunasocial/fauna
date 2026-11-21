// The onboarding wizard's remaining gates paint from the machine's answers
// (`docs/goal/ui/README.md` § Offer only what is possible, rules 2 and 6): a
// blocked answer is shown disabled with its reason, a possible one live, and
// before the machine exists only the still-loading line.
import { assert, assertEquals } from 'jsr:@std/assert@1';
import {
  dnsContinuePaint,
  handleContinuePaint,
  inviteContinuePaint,
  inviteSubmitPaint,
  manualDnsCoverPaint,
  natModeConfirmPaint,
  providersContinuePaint,
  provisioningStartPaint,
  type Paint,
  type WizardGateAnswers,
} from './wizard-possibility.ts';
import type { Possibility } from '../possibility.ts';

const possible: Possibility = { state: 'possible' };
const blocked = (key: string) => ({ state: 'blocked', reason: { key } }) as Possibility;

function machine(answer: Possibility): WizardGateAnswers {
  return {
    providersContinuePossibility: () => answer,
    goalCoverPossibility: () => answer,
    provisioningStartPossibility: () => answer,
    dnsContinuePossibility: () => answer,
    handleContinuePossibility: () => answer,
    inviteContinuePossibility: () => answer,
    inviteSubmitPossibility: () => answer,
    natModeSubmitPossibility: () => answer,
  };
}

function assertLive(p: Paint) {
  assert(p.shown && !p.disabled);
  assertEquals(p.reason, null);
}

function assertBlocked(p: Paint, key: string) {
  assert(p.shown && p.disabled);
  assertEquals((p.reason as { key: string } | null)?.key, key);
}

const GATES: [string, (m: WizardGateAnswers | null) => Paint][] = [
  ['provider-choice-continue-button', providersContinuePaint],
  ['manual provider row', manualDnsCoverPaint],
  ['provisioning-start-button', provisioningStartPaint],
  ['dns-config-continue-button', dnsContinuePaint],
  ['handle-entry-continue-button', handleContinuePaint],
  ['invite-request-continue-button', inviteContinuePaint],
  ['invite-request-submit-button', inviteSubmitPaint],
  ['nat-mode-confirm-button', natModeConfirmPaint],
];

for (const [name, paint] of GATES) {
  Deno.test(`${name}: blocked answer paints disabled with its reason`, () => {
    assertBlocked(paint(machine(blocked('some.reason'))), 'some.reason');
  });
  Deno.test(`${name}: possible answer paints live`, () => {
    assertLive(paint(machine(possible)));
  });
  Deno.test(`${name}: no machine yet paints still loading`, () => {
    assertBlocked(paint(null), 'common.still_loading');
  });
}

Deno.test('the manual row asks for the Dns goal with the Manual cover', () => {
  let asked: unknown[] = [];
  const m = machine(possible);
  m.goalCoverPossibility = (goal, cover) => {
    asked = [goal, cover];
    return possible;
  };
  manualDnsCoverPaint(m);
  assertEquals(asked, ['Dns', 'Manual']);
});
