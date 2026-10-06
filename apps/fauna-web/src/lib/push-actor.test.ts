import {
  isSubscribed,
  markSubscribed,
  clearSubscribed,
  clearSubscribedActorIf,
  mayHoldRow,
  needsReconcile,
} from './push-actor.ts';

function eq<T>(actual: T, expected: T, msg: string) {
  if (actual !== expected) {
    throw new Error(`${msg}: got ${actual}, want ${expected}`);
  }
}

const ACTOR_A = 'actor-a';
const ACTOR_B = 'actor-b';

// The bug (`succession-aftermath.md` § Implementation status today — "a
// successor's own device does not re-subscribe to push"): a subscription is
// install-scoped, but the nest-side `push_subscriptions` row it produces is
// actor-scoped, so an identity change on an already-subscribed browser
// leaves a live subscription silently bound to an actor no longer signed in.
// `needsReconcile` is the decision that closes it.

Deno.test('needsReconcile — never subscribed on this browser: no-op regardless of actor', () => {
  localStorage.clear();
  eq(isSubscribed(), false, 'precondition: nothing subscribed');
  eq(needsReconcile(ACTOR_A), false, 'nothing to reconcile without a live subscription');
});

Deno.test('needsReconcile — subscribed, no actor recorded yet (legacy install): reconciles once', () => {
  localStorage.clear();
  localStorage.setItem('fauna-push-subscribed', 'true');
  eq(needsReconcile(ACTOR_A), true, 'a pre-fix subscription with no recorded actor self-heals');
});

Deno.test('needsReconcile — subscribed under the SAME actor: no-op', () => {
  localStorage.clear();
  markSubscribed(ACTOR_A);
  eq(needsReconcile(ACTOR_A), false, 'the live subscription already names the signed-in actor');
});

Deno.test('needsReconcile — subscribed under a DIFFERENT actor: reconciles (the succession case)', () => {
  localStorage.clear();
  markSubscribed(ACTOR_A);
  eq(needsReconcile(ACTOR_B), true, 'a successor signing in on the old device must re-arm the subscription');
});

Deno.test('markSubscribed — records both the flag and the actor', () => {
  localStorage.clear();
  markSubscribed(ACTOR_A);
  eq(isSubscribed(), true, 'flag set');
  eq(needsReconcile(ACTOR_A), false, 'and the actor it was set for needs no reconcile');
});

Deno.test('clearSubscribed — drops both the flag and the recorded actor', () => {
  localStorage.clear();
  markSubscribed(ACTOR_A);
  clearSubscribed();
  eq(isSubscribed(), false, 'flag cleared');
  eq(needsReconcile(ACTOR_A), false, 'unsubscribed means nothing to reconcile, even for the last actor');
});

// The leave-gesture half ("the subscription follows the signed-in identity",
// `common.md` § Registration, ruled 2026-08-30): the leaving session drops its
// own nest row, a SUCCESSFUL drop clears the which-actor record, and the
// install intent bit survives every leave-shape — only the user's Disable
// (`clearSubscribed` above) touches it.

Deno.test('clearSubscribedActorIf — matching actor: record cleared, intent bit SURVIVES', () => {
  localStorage.clear();
  markSubscribed(ACTOR_A);
  clearSubscribedActorIf(ACTOR_A);
  eq(isSubscribed(), true, 'a leave-gesture is not an opt-out: the intent bit stays');
  eq(needsReconcile(ACTOR_B), true, 'next identity settle re-arms the incoming actor');
  eq(needsReconcile(ACTOR_A), true, 'even the same actor re-arms — its row is gone');
});

Deno.test('clearSubscribedActorIf — different actor (legacy row drop): record untouched', () => {
  localStorage.clear();
  markSubscribed(ACTOR_B);
  clearSubscribedActorIf(ACTOR_A);
  eq(needsReconcile(ACTOR_B), false, "B's live row is still recorded — dropping A's stale row must not orphan it");
});

Deno.test('mayHoldRow — never subscribed: no drop RPC owed on a leave-gesture', () => {
  localStorage.clear();
  eq(mayHoldRow(), false, 'an install with no push history issues no unsubscribe');
});

Deno.test('mayHoldRow — subscribed: a leave-gesture owes the drop', () => {
  localStorage.clear();
  markSubscribed(ACTOR_A);
  eq(mayHoldRow(), true, 'live row recorded');
});

Deno.test('mayHoldRow — actor record without the intent bit (post-Disable legacy): still owes the drop', () => {
  localStorage.clear();
  markSubscribed(ACTOR_A);
  localStorage.removeItem('fauna-push-subscribed');
  eq(isSubscribed(), false, 'precondition: intent bit gone');
  eq(mayHoldRow(), true, 'any push history counts — a stale row can outlive the intent bit');
});

Deno.test('leave then re-arm — the one-live-row cycle ends pointed at the incoming actor', () => {
  localStorage.clear();
  markSubscribed(ACTOR_A);
  // A leaves (switch/sign-out): drop landed, record cleared, intent kept.
  clearSubscribedActorIf(ACTOR_A);
  // B's identity settles: reconcile re-arms and records B.
  eq(needsReconcile(ACTOR_B), true, 'reconcile fires for the incoming actor');
  markSubscribed(ACTOR_B);
  eq(needsReconcile(ACTOR_B), false, 'B owns the single live row');
  eq(needsReconcile(ACTOR_A), true, 'A no longer does');
});
