/**
 * The onboarding wizard's remaining gates, painted from the shared machine's
 * `Possibility` answers (`docs/goal/ui/README.md` § Offer only what is
 * possible, rules 2 and 6): `provider-choice-continue-button`, the manual
 * provider row, `provisioning-start-button`, `dns-config-continue-button`,
 * `handle-entry-continue-button`, `invite-request-continue-button`,
 * `invite-request-submit-button` and `nat-mode-confirm-button`. The SPA
 * derives none of it — each button is disabled exactly while its answer is
 * blocked, with the answer's reason beside it. Reference app: tui
 * (`apps/fauna-tui/src/wizard/`). Sibling of `credential-possibility.ts`.
 */
import { paintPossibility, type Possibility } from '../possibility.ts';
import type { LocalizedText } from '../i18n/localized.ts';

export type Paint = ReturnType<typeof paintPossibility>;

/** The wizard machine's gate answers, as its typed surface hands them over. */
export interface WizardGateAnswers {
  providersContinuePossibility(): Possibility;
  goalCoverPossibility(goal: 'Domain' | 'Dns' | 'Vps', cover: unknown): Possibility;
  provisioningStartPossibility(): Possibility;
  dnsContinuePossibility(): Possibility;
  handleContinuePossibility(): Possibility;
  inviteContinuePossibility(): Possibility;
  inviteSubmitPossibility(): Possibility;
  natModeSubmitPossibility(): Possibility;
}

const STILL_LOADING: Possibility = {
  state: 'blocked',
  reason: { key: 'common.still_loading' } as LocalizedText,
};

type Machine = WizardGateAnswers | null | undefined;

/** Before the machine exists a button is shown but dead, saying it is still
 *  loading. */
function paint(m: Machine, ask: (m: WizardGateAnswers) => Possibility): Paint {
  return paintPossibility(m ? ask(m) : STILL_LOADING);
}

export const providersContinuePaint = (m: Machine): Paint =>
  paint(m, (x) => x.providersContinuePossibility());

/** The manual provider row: it covers the DNS goal itself, so it paints from
 *  the answer to picking `Manual` for `Dns`. */
export const manualDnsCoverPaint = (m: Machine): Paint =>
  paint(m, (x) => x.goalCoverPossibility('Dns', 'Manual'));

export const provisioningStartPaint = (m: Machine): Paint =>
  paint(m, (x) => x.provisioningStartPossibility());

export const dnsContinuePaint = (m: Machine): Paint =>
  paint(m, (x) => x.dnsContinuePossibility());

export const handleContinuePaint = (m: Machine): Paint =>
  paint(m, (x) => x.handleContinuePossibility());

export const inviteContinuePaint = (m: Machine): Paint =>
  paint(m, (x) => x.inviteContinuePossibility());

export const inviteSubmitPaint = (m: Machine): Paint =>
  paint(m, (x) => x.inviteSubmitPossibility());

export const natModeConfirmPaint = (m: Machine): Paint =>
  paint(m, (x) => x.natModeSubmitPossibility());
