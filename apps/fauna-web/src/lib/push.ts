import { pushVapidKey, pushEnable, pushDisable, pushRearm, pushDropActorRow } from './rpc';
import type { BrowserPushSubscription } from './rpc';
import { toArrayBufferView } from './bytes';
import { pushOptedIn } from './wasm';
import type { Identity } from './types';

// Web push over the shared registration machine
// (`fauna_client_push::registration`, reached through the session's wasm
// `WsRpcClient`): the install opt-in bit, the which-actor record and the three
// leave-shapes are the machine's (`docs/goal/architecture/apps/common.md`
// § Registration), persisted install-scoped in `localStorage` wasm-side
// (`account-scoping.md` class 2 — no sign-out or removal erase touches them).
// What stays here is the one genuinely platform-only step: asking the browser's
// `serviceWorker`/`PushManager` for a subscription to hand the machine.

/** Why an Enable could not register: the browser has no push APIs, or the user
 *  (or the browser) refused the notification permission. */
export class PushUnavailableError extends Error {}

/** Has this browser opted in? The stored bit, never `Notification.permission`
 *  (`docs/goal/ui/settings.md` § Push notifications). */
export { pushOptedIn };

/** This browser's push subscription — `PushManager.subscribe()` on an
 *  already-subscribed registration hands back the existing one without a
 *  prompt. Throws `PushUnavailableError` when the browser cannot push. */
async function browserSubscription(secretHex: string): Promise<BrowserPushSubscription> {
  if (!('serviceWorker' in navigator) || !('PushManager' in window)) {
    throw new PushUnavailableError('push is not available in this browser');
  }
  const registration = await navigator.serviceWorker.register('/service-worker.js');
  const publicKey = await pushVapidKey(secretHex);
  if (!publicKey) throw new PushUnavailableError('this nest does not offer push');
  let subscription: PushSubscription;
  try {
    subscription = await registration.pushManager.subscribe({
      userVisibleOnly: true,
      applicationServerKey: toArrayBufferView(urlBase64ToUint8Array(publicKey)),
    });
  } catch (e) {
    // A refused permission surfaces as `NotAllowedError` (or an `AbortError`
    // naming it): the platform said no, which the toggle reports as such.
    throw new PushUnavailableError(e instanceof Error ? e.message : String(e));
  }
  const keys = subscription.toJSON().keys ?? {};
  if (!keys.p256dh || !keys.auth) {
    throw new PushUnavailableError('the browser returned a subscription without keys');
  }
  return { endpoint: subscription.endpoint, key_p256dh: keys.p256dh, key_auth: keys.auth };
}

/** The toggle switched on: subscribe, then set the bit. A failure leaves the
 *  bit unset, so the toggle settles back off. */
export async function enablePush(secretHex: string): Promise<void> {
  await pushEnable(secretHex, await browserSubscription(secretHex));
}

/** The toggle switched off: the bit clears the moment this is issued (off stays
 *  off even offline), then the row goes. The OS permission is left alone. */
export async function disablePush(secretHex: string): Promise<void> {
  await pushDisable(secretHex);
}

/**
 * The leave-gesture half of "the subscription follows the signed-in identity"
 * (`common.md` § Registration): drop the LEAVING actor's row while its
 * authority is still in hand — the verbs are actor-scoped on the connection
 * actor, so nobody else ever can. Call sites: `performSwitch` (before the
 * switch commits), `identity.logout()` (before the credential erase can strand
 * the secret), and the add-account entry. Never touches the opt-in bit, and
 * best-effort by ruling: a leave gesture must complete offline.
 */
export async function dropActorPushRow(secretHex: string): Promise<void> {
  try {
    await pushDropActorRow(secretHex);
  } catch (e) {
    console.warn('push row drop (leave-gesture, best-effort):', e);
  }
}

/**
 * Identity settle (`onActorChange` in the root layout — first load included,
 * so a reload after a switch or a succession needs no Settings visit):
 * re-register this browser's row under whoever is signed in now, while the
 * install is opted in. Never opts a browser in — an opted-out browser does not
 * even ask `PushManager`. Best-effort: a failure is logged and the settle
 * proceeds.
 */
export async function rearmPush(id: Identity): Promise<void> {
  if (!pushOptedIn()) return;
  try {
    await pushRearm(id.secretHex, await browserSubscription(id.secretHex));
  } catch (e) {
    console.warn('push re-arm (identity settle, best-effort):', e);
  }
}

function urlBase64ToUint8Array(base64String: string): Uint8Array {
  const padding = '='.repeat((4 - base64String.length % 4) % 4);
  const base64 = (base64String + padding).replace(/-/g, '+').replace(/_/g, '/');
  const raw = atob(base64);
  return Uint8Array.from(raw, (c) => c.charCodeAt(0));
}
