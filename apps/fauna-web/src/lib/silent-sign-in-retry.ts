//! When web's background silent sign-in tries again after a failure.
//!
//! `$lib/store`'s `refreshFromServer` is the only place a signed-in reload
//! reaches a working session: web has no launch surface on that path (the
//! layout diverts to the launch flow only when the stored identity is
//! MISSING), so the native launch machine's visible Retry CTA has no web
//! counterpart there. A refresh that gave up on its first transient failure
//! therefore left `identity.registered` false for the whole document — and
//! the succession closing act gates on it (`identity-succession.md` § The
//! RecoveryKey → *At succession*). So the refresh retries, silently
//! (`security.md` § Transport trust → § Post-auth surfacing: background
//! refreshes stay silent for every non-terminal class).
//!
//! Pure, so the decision is unit-tested apart from the wasm and the store.

import { TransientAuthError } from './auth-errors.ts';

/** The wait before each retry, in order; the last entry repeats for as long
 *  as the identity is still the store's. Short first (a blip clears in one),
 *  capped so a long outage costs one challenge per interval, not a storm. */
export const SILENT_SIGN_IN_RETRY_MS: readonly number[] = [1_000, 2_000, 5_000, 10_000, 30_000];

/** How long to wait before retrying after `err` on the `attempt`-th failure
 *  (0-based), or `null` when this failure must not be retried at all.
 *
 *  Only a `TransientAuthError` — the wasm's `transient:` class (anonymous
 *  connect, disconnect, timeout) — is retried. The typed terminal verdicts
 *  escalate before this is consulted, and the opaque `Error` bucket is
 *  deliberately left single-shot: `classifyChallengeError` maps every message
 *  it does not recognise there, so a verdict that ever lost its prefix would
 *  otherwise land in a retry loop instead of being seen (`security.md`
 *  § Post-auth surfacing names exactly that flattening as the failure mode). */
export function silentSignInRetryDelay(err: unknown, attempt: number): number | null {
  if (!(err instanceof TransientAuthError)) return null;
  const i = Math.min(Math.max(attempt, 0), SILENT_SIGN_IN_RETRY_MS.length - 1);
  return SILENT_SIGN_IN_RETRY_MS[i];
}
