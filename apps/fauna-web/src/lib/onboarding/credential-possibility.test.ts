// The onboarding and retire credential buttons paint from the machines'
// answers (`docs/goal/ui/README.md` § Offer only what is possible, rules 2
// and 6): a blocked answer is shown disabled with its reason, a possible one
// live, and nothing is painted before the machine exists but the
// still-loading line.
import { assert, assertEquals } from 'jsr:@std/assert@1';
import {
  connectProviderPaint,
  hostedAuthPaint,
  providerHostedAuthPaint,
  retireHostedAuthPaint,
  retireVerifyPaint,
  verifyDnsPaint,
  verifyVpsPaint,
  type Paint,
  type RetireCredentialAnswers,
  type WizardCredentialAnswers,
} from './credential-possibility.ts';
import type { Possibility } from '../possibility.ts';

const possible: Possibility = { state: 'possible' };
const fillRequired = {
  state: 'blocked',
  reason: { key: 'onboarding.credentials.fill_required' },
} as Possibility;
const checking = {
  state: 'blocked',
  reason: { key: 'onboarding.credentials.checking' },
} as Possibility;

function wizard(answer: Possibility): WizardCredentialAnswers {
  return {
    verifyDnsPossibility: () => answer,
    verifyVpsPossibility: () => answer,
    hostedAuthPossibility: () => answer,
    connectProviderPossibility: () => answer,
    providerHostedAuthPossibility: () => answer,
  };
}

function retire(answer: Possibility): RetireCredentialAnswers {
  return {
    verifyPossibilityJson: () => JSON.stringify(answer),
    hostedAuthPossibilityJson: () => JSON.stringify(answer),
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

const WIZARD_BUTTONS: [string, (m: WizardCredentialAnswers | null) => Paint][] = [
  ['dns-verify-button', (m) => verifyDnsPaint(m)],
  ['vps-verify-button', (m) => verifyVpsPaint(m)],
  ['dns hosted-auth', (m) => hostedAuthPaint(m, 'dns', 'token')],
  ['vps hosted-auth', (m) => hostedAuthPaint(m, 'vps', 'token')],
  ['provider-connect-button', (m) => connectProviderPaint(m, 'hetzner')],
  ['provider dialog hosted-auth', (m) => providerHostedAuthPaint(m, 'hetzner', 'token')],
];

const RETIRE_BUTTONS: [string, (r: RetireCredentialAnswers | null) => Paint][] = [
  ['retire vps-verify-button', (r) => retireVerifyPaint(r)],
  ['retire hosted-auth', (r) => retireHostedAuthPaint(r, 'token')],
];

for (const [name, paint] of WIZARD_BUTTONS) {
  Deno.test(`${name}: blocked answer paints disabled with its reason`, () => {
    assertBlocked(paint(wizard(fillRequired)), 'onboarding.credentials.fill_required');
    assertBlocked(paint(wizard(checking)), 'onboarding.credentials.checking');
  });
  Deno.test(`${name}: possible answer paints live`, () => {
    assertLive(paint(wizard(possible)));
  });
  Deno.test(`${name}: no machine yet paints still loading`, () => {
    assertBlocked(paint(null), 'common.still_loading');
  });
}

for (const [name, paint] of RETIRE_BUTTONS) {
  Deno.test(`${name}: blocked answer paints disabled with its reason`, () => {
    assertBlocked(paint(retire(fillRequired)), 'onboarding.credentials.fill_required');
  });
  Deno.test(`${name}: possible answer paints live`, () => {
    assertLive(paint(retire(possible)));
  });
  Deno.test(`${name}: no machine yet paints still loading`, () => {
    assertBlocked(paint(null), 'common.still_loading');
  });
}

Deno.test('provider-connect-button for an unknown provider is not shown', () => {
  const p = connectProviderPaint(wizard({ state: 'absent' }), 'nope');
  assert(!p.shown && p.disabled);
});
