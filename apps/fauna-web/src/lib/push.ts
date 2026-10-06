import { pushVapidKey, pushSubscribe, pushUnsubscribe } from './rpc';
import { toArrayBufferView } from './bytes';
import { actorIdFromSecret } from './wasm';
import {
  isSubscribed,
  markSubscribed,
  clearSubscribed,
  clearSubscribedActorIf,
  mayHoldRow,
  needsReconcile,
} from './push-actor';
import { getDeviceId } from './device-id';
import type { Identity } from './types';

export { isSubscribed };

// The three nest hops ride WS-RPC (`fauna.push.{vapid_key,subscribe,
// unsubscribe}`) via `$lib/rpc`; the browser `serviceWorker`/`PushManager`
// work below is genuinely platform-only and stays here.
export async function subscribeToPush(secretHex: string): Promise<boolean> {
  if (!('serviceWorker' in navigator) || !('PushManager' in window)) {
    return false;
  }

  const registration = await navigator.serviceWorker.register('/service-worker.js');

  const publicKey = await pushVapidKey(secretHex);
  if (!publicKey) return false;

  const subscription = await registration.pushManager.subscribe({
    userVisibleOnly: true,
    applicationServerKey: toArrayBufferView(urlBase64ToUint8Array(publicKey)),
  });

  const keys = subscription.toJSON().keys!;

  await pushSubscribe(secretHex, {
    device_id: getDeviceId(actorIdFromSecret(secretHex)),
    endpoint: subscription.endpoint,
    key_p256dh: keys.p256dh,
    key_auth: keys.auth,
  });

  return true;
}

export async function unsubscribeFromPush(secretHex: string): Promise<void> {
  await pushUnsubscribe(secretHex, getDeviceId(actorIdFromSecret(secretHex)));
}

export async function requestAndSubscribe(secretHex: string): Promise<boolean> {
  const ok = await subscribeToPush(secretHex);
  if (ok) markSubscribed(actorIdFromSecret(secretHex));
  return ok;
}

export async function unsubscribe(secretHex: string): Promise<void> {
  await unsubscribeFromPush(secretHex);
  clearSubscribed();
}

/**
 * The leave-gesture half of "the subscription follows the signed-in identity"
 * (`docs/goal/architecture/apps/common.md` § Registration, ruled 2026-08-30):
 * drop the LEAVING actor's `push_subscriptions` row while its authority is
 * still in hand — the verbs are actor-scoped on the connection actor, so
 * nobody else ever can. Call sites: `performSwitch` (before the switch
 * commits), `identity.logout()` (before the credential erase can strand the
 * secret), and the add-account entry (the append wizard is the one switch
 * commit that no longer holds the outgoing authority).
 *
 * Unlike `unsubscribe` above (the user's Disable), this NEVER touches the
 * install intent bit — clearing it here would turn every sign-out into the
 * silent opt-out the re-arm rule forbids. Best-effort by ruling: a leave
 * gesture must complete offline, so failures are swallowed and the stranded
 * row is left to the ruling's three reapers (re-adopt, succession burn,
 * endpoint death). Only a drop that actually landed clears the which-actor
 * record — a failed one leaves the row live, so the record stays true.
 */
export async function dropActorPushRow(secretHex: string): Promise<void> {
  if (!mayHoldRow()) return;
  try {
    await pushUnsubscribe(secretHex, getDeviceId(actorIdFromSecret(secretHex)));
    clearSubscribedActorIf(actorIdFromSecret(secretHex));
  } catch (e) {
    console.warn('push row drop (leave-gesture, best-effort):', e);
  }
}

/**
 * Re-arm this browser's push subscription for whichever actor is signed in
 * now, when it was last armed for a *different* one — the successor's own
 * device never re-subscribing to push (`docs/goal/behavior/
 * succession-aftermath.md` § Implementation status today). A subscription is
 * install-scoped (this function's own `isSubscribed()` gate), but the nest's
 * `push_subscriptions` row it produced is actor-scoped: succession mints a
 * brand new actor id with no row of its own, and — by the same design that
 * makes the table unrevocable from any app — nothing ever moves the old row
 * over. Silently off is the symptom; this is the fix.
 *
 * Call on every identity settle (`onActorChange` in the root layout — fires
 * on first load too, which is deliberate: a page reload after a succession
 * must not require a Settings visit to notice). No-ops unless this browser
 * already completed the permission dance once: it must never *opt a device
 * into* push, only keep an existing subscription pointed at the right actor.
 * `subscribeToPush` replays the same P-256/auth keys and endpoint the browser
 * already holds — `PushManager.subscribe()` on an already-subscribed
 * registration returns the existing subscription rather than prompting again
 * — so this never surfaces a permission prompt.
 */
export async function reconcileSubscriptionActor(id: Identity): Promise<void> {
  if (!needsReconcile(id.actorId)) return;
  const ok = await subscribeToPush(id.secretHex);
  if (ok) markSubscribed(id.actorId);
}

function urlBase64ToUint8Array(base64String: string): Uint8Array {
  const padding = '='.repeat((4 - base64String.length % 4) % 4);
  const base64 = (base64String + padding).replace(/-/g, '+').replace(/_/g, '/');
  const raw = atob(base64);
  return Uint8Array.from(raw, (c) => c.charCodeAt(0));
}
