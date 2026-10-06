// This browser's sync device id — ONE PER ACCOUNT, never one for every account
// on the browser (`docs/goal/architecture/apps/sync-agent-credentials.md`
// § Credential model, the 2026-09-20 ruling).
//
// The id is the account's persisted `fauna/{actor}/device_id` slot when it has
// one, else `derive_device_id(install_secret, actor_id)` over a random install
// secret kept in this origin's `localStorage` — so the same account signing
// back in after a sign-out gets the same id, and a nest serving two accounts
// cannot link them through a shared one. The rules live in shared Rust
// (`fauna_client_accounts::AccountRegistry::device_id_for_actor`), reached over
// wasm; this module is the synchronous TS face every feature calls.
//
// Web registers no `sync_devices` row of its own, so the id's jobs here are
// attribution (media / folder records, conflict re-points, task delegation),
// the push-subscription key, and the Devices page's this-device marker.
//
// The one boot-time duty this module relies on runs in `accountsBoot`
// (`ensureInstallDeviceSecret`, a registry mutator and so inside the cross-tab
// mutation lock wasm-side): the install secret is minted there, so two tabs
// opened together end on one secret.

import { wasmCoreModule } from '$lib/wasm';

/** The device id `actorId` presents from this browser. Stable per account.
 *  Throws when no stable id exists (the store dropped the install secret's
 *  write) — callers surface it like any other failed operation rather than
 *  present an id the next read cannot reproduce. Needs wasm initialized, which
 *  every signed-in surface already is. */
export function getDeviceId(actorId: string): string {
  const reg = new (wasmCoreModule().WasmAccountRegistry)();
  try {
    return reg.deviceIdForActor(actorId);
  } finally {
    reg.free();
  }
}
