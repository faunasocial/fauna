// The post-claim serving enablement's refusal for good, said on the page the
// person is on (`onboarding.md` § 3b *The step completes; it is not fired
// once*). The shared step (`applyServingEnablement`, wasm
// `applyPostClaimServingEnablement`) resolves to the page's `error-message`
// text when a step was refused for good, else `undefined`. It is the
// `LoggedIn` handoff's fire-and-forget, so it usually answers after the
// redirect to the feed: the text goes through the shell's
// `fauna-message-update` channel, which the root layout's `MessageBanner`
// paints on whichever page is mounted (`#lib/message-update`). tui is the lead
// (`DataMessage::ServingEnablementRefused`).

import type { MessageUpdate } from './message-update.ts';

/** The banner update a run's answer makes: the refusal as the shell's error,
 *  or none when every step completed. */
export function servingEnablementRefusalUpdate(refusal: string | undefined): MessageUpdate | null {
  return refusal ? { error: refusal } : null;
}

/** Dispatch the run's refusal, if any, on the shell message channel. Returns
 *  whether an update was sent. */
export function surfaceServingEnablementRefusal(
  refusal: string | undefined,
  target: EventTarget = window,
): boolean {
  const detail = servingEnablementRefusalUpdate(refusal);
  if (detail === null) return false;
  target.dispatchEvent(new CustomEvent('fauna-message-update', { detail }));
  return true;
}
