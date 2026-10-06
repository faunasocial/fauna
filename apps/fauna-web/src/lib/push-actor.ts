// Pure, import-free `localStorage` state for whether THIS browser's push
// subscription currently points at the right actor. Split out of `push.ts`
// so it stays unit-testable via plain `deno test` (`push-actor.test.ts`):
// `push.ts` itself uses the extensionless imports every other SPA module
// does (the Vite/bundler convention), which pulls in `./rpc` → `svelte/store`
// et al — a graph `deno test` cannot resolve without the full app tooling.
// This module has no imports at all, so it carries none of that.

const SUBSCRIBED_KEY = 'fauna-push-subscribed';
// Which actor the nest-side `push_subscriptions` row is currently registered
// under. Unlike SUBSCRIBED_KEY (this browser did the permission dance once,
// ever), this tracks WHICH identity's row is live — the row is actor-scoped
// server-side (`fauna_client_push::PushClient`'s doc: "subscribe/unsubscribe
// are actor-scoped device management on the connection actor"), so a later
// identity change on the same browser (a succession completing, or switching
// accounts) leaves the OLD actor's row live and mints none for the new one —
// see `needsReconcile` below (`docs/goal/behavior/succession-aftermath.md`
// § Implementation status today — "a successor's own device does not
// re-subscribe to push").
const SUBSCRIBED_ACTOR_KEY = 'fauna-push-subscribed-actor';

// Both keys are install-scoped, not account-scoped (`docs/goal/architecture/
// apps/account-scoping.md` § The scoping taxonomy, class 2 — "the web push
// device id (a per-browser transport handle)"): they describe this browser's
// push transport, not any one signed-in identity, so they are deliberately
// NOT keyed by actor id and no sign-out/account-removal ERASE touches them.
// The actor key still moves on a leave-gesture, by a different mechanism: the
// leaving session drops its own nest row ("the subscription follows the
// signed-in identity", `common.md` § Registration), and a SUCCESSFUL drop
// clears the actor key — the row it recorded no longer exists — while the
// intent bit survives every leave-shape (only the user's Disable clears it).

export function isSubscribed(): boolean {
  return localStorage.getItem(SUBSCRIBED_KEY) === 'true';
}

/** Record that this browser's subscription now points at `actorId`. */
export function markSubscribed(actorId: string): void {
  localStorage.setItem(SUBSCRIBED_KEY, 'true');
  localStorage.setItem(SUBSCRIBED_ACTOR_KEY, actorId);
}

/** Forget this browser's subscription (an unsubscribe landed). */
export function clearSubscribed(): void {
  localStorage.removeItem(SUBSCRIBED_KEY);
  localStorage.removeItem(SUBSCRIBED_ACTOR_KEY);
}

/**
 * Might this browser hold a live (or stale, from an earlier subscribe) `push_subscriptions`
 * row worth dropping on a leave-gesture? Gates the best-effort unsubscribe in
 * `push.ts::dropActorPushRow` so an install that never touched push issues no
 * RPC on every switch/sign-out. Deliberately wider than `isSubscribed()`: a
 * legacy install can carry another actor's row from before the
 * follow-the-signed-in-identity ruling (`common.md` § Registration) even
 * after a Disable cleared the intent bit, so any push history counts.
 */
export function mayHoldRow(): boolean {
  return isSubscribed() || localStorage.getItem(SUBSCRIBED_ACTOR_KEY) !== null;
}

/**
 * A leave-gesture's drop landed for `actorId`: forget that its row is live —
 * but only if the record actually points at it (under the one-live-row
 * contract it does; a leave-gesture can be dropping a stale row while
 * the record names a different, still-live actor). Never touches the intent
 * bit: the install stays subscribed, and the next identity settle re-arms via
 * `needsReconcile` (a cleared record !== any actor).
 */
export function clearSubscribedActorIf(actorId: string): void {
  if (localStorage.getItem(SUBSCRIBED_ACTOR_KEY) === actorId) {
    localStorage.removeItem(SUBSCRIBED_ACTOR_KEY);
  }
}

/**
 * Does the signed-in actor need this browser's push subscription re-armed
 * under it? True for a legacy install too: `SUBSCRIBED_ACTOR_KEY` postdates
 * `SUBSCRIBED_KEY`, so an already-subscribed browser that has never recorded
 * one reconciles once on its first identity settle after this shipped —
 * never opting a fresh device INTO push, only keeping an existing
 * subscription pointed at the right actor.
 */
export function needsReconcile(actorId: string): boolean {
  return isSubscribed() && localStorage.getItem(SUBSCRIBED_ACTOR_KEY) !== actorId;
}
