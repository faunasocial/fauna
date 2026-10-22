// Moment 1's failure path (`long-term-store.md` § Save-failure handling): when
// the app's write of the identity just confirmed fails, the wizard goes back to
// the confirm screen with the error on `error-message` — it must not advance to
// handle entry holding a key that is stored nowhere. The wizard machine owns the
// undo (`identityCommitFailed`); this is web's report to it.

import { t } from '../i18n/strings.ts';
import { rejectionText } from '../rejection.ts';

export interface IdentityCommitSteps {
  /** The registry write (`accountsPersistConfirmedIdentity`): rejects on a
   *  failed commit or read-back. */
  persist: (secret: string, append: boolean) => Promise<unknown>;
  /** The wizard machine's `identityCommitFailed(message)`. */
  commitFailed: (message: string) => void;
  /** Diagnostic sink for the failure. */
  log: (e: unknown) => void;
}

/** Run the moment-1 write; on rejection log it and report the resolved line to
 *  the machine. Resolves once settled — never rejects. */
export async function commitConfirmedIdentity(
  secret: string,
  append: boolean,
  steps: IdentityCommitSteps,
): Promise<void> {
  try {
    await steps.persist(secret, append);
  } catch (e) {
    steps.log(e);
    steps.commitFailed(t.onboarding.session_error.persist_successor({ message: rejectionText(e, String(e)) }));
  }
}
