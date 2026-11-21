/**
 * The set-up and recovery paths' credential buttons, painted from the shared
 * machines' `Possibility` answers (`docs/goal/ui/README.md` § Offer only what
 * is possible, rules 2 and 6; `docs/goal/behavior/onboarding-provisioning.md`
 * § 3d). The SPA derives none of it: `dns-verify-button`,
 * `vps-verify-button`, `provider-connect-button` and each form's `hosted-auth`
 * sign-in button paint from `OnboardingMachine::*_possibility`, the retire
 * view's from `NestRetireMachine::*_possibility` — each blocked answer naming
 * the provider to pick, the check on its way, or the required field left
 * empty — and the machines' gestures ask the same answer first. Reference
 * app: tui (`apps/fauna-tui/src/wizard/`).
 */
import { paintPossibility, type Possibility } from '../possibility.ts';
import type { LocalizedText } from '../i18n/localized.ts';

export type Paint = ReturnType<typeof paintPossibility>;

/** The wizard machine's answers, as its typed surface hands them over. */
export interface WizardCredentialAnswers {
  verifyDnsPossibility(): Possibility;
  verifyVpsPossibility(): Possibility;
  hostedAuthPossibility(form: 'dns' | 'vps', fieldId: string): Possibility;
  connectProviderPossibility(providerId: string): Possibility;
  providerHostedAuthPossibility(providerId: string, fieldId: string): Possibility;
}

/** The wasm `NestRetireMachine`'s two answers, as serde JSON. */
export interface RetireCredentialAnswers {
  verifyPossibilityJson(): string;
  hostedAuthPossibilityJson(fieldId: string): string;
}

const STILL_LOADING: Possibility = {
  state: 'blocked',
  reason: { key: 'common.still_loading' } as LocalizedText,
};

/** Before the machine exists a button is shown but dead, saying it is still
 *  loading. */
function before(): Paint {
  return paintPossibility(STILL_LOADING);
}

export function verifyDnsPaint(m: WizardCredentialAnswers | null | undefined): Paint {
  return m ? paintPossibility(m.verifyDnsPossibility()) : before();
}

export function verifyVpsPaint(m: WizardCredentialAnswers | null | undefined): Paint {
  return m ? paintPossibility(m.verifyVpsPossibility()) : before();
}

export function hostedAuthPaint(
  m: WizardCredentialAnswers | null | undefined,
  form: 'dns' | 'vps',
  fieldId: string,
): Paint {
  return m ? paintPossibility(m.hostedAuthPossibility(form, fieldId)) : before();
}

export function connectProviderPaint(
  m: WizardCredentialAnswers | null | undefined,
  providerId: string,
): Paint {
  return m ? paintPossibility(m.connectProviderPossibility(providerId)) : before();
}

export function providerHostedAuthPaint(
  m: WizardCredentialAnswers | null | undefined,
  providerId: string,
  fieldId: string,
): Paint {
  return m ? paintPossibility(m.providerHostedAuthPossibility(providerId, fieldId)) : before();
}

export function retireVerifyPaint(r: RetireCredentialAnswers | null | undefined): Paint {
  return r ? paintPossibility(JSON.parse(r.verifyPossibilityJson()) as Possibility) : before();
}

export function retireHostedAuthPaint(
  r: RetireCredentialAnswers | null | undefined,
  fieldId: string,
): Paint {
  return r
    ? paintPossibility(JSON.parse(r.hostedAuthPossibilityJson(fieldId)) as Possibility)
    : before();
}
