// Launch-slot gates + the factory-reset mint rail.
//
// There is NO web implementation of `LaunchPersistence` anymore (CR-3): the
// shared `LaunchMachine` (via `libs/fauna-wasm-launch`) routes on the shared
// `RegistryLaunchPersistence` over the per-actor registry store in
// localStorage — the same seam Android/Windows/Apple consume over UniFFI and
// linux/tui consume natively, so web can no longer hand the machine a
// bespoke single-slot store. No `legacy/*` global key is ever read or
// written here — see `libs/fauna-client-accounts/src/web_store.rs`
// (`docs/goal/architecture/long-term-store.md` § Multi-account evolution).
//
// The gates below read through the SAME registry accessors the machine
// branches on, so the gate and the machine can never disagree about whether
// a row fires.

import { loadPendingFactoryReset, deletePendingFactoryReset } from './pending-factory-reset-store';
import {
  mintAndPersistPendingFactoryResetWasm,
  registryHasIdentity,
  registryLoadAwaitingDns,
  registryLoadPendingFactoryReset,
} from '../wasm-launch';

/**
 * True when the long-term store holds an identity AND an awaiting-manual-dns
 * slot — i.e. the `WizardAt{AwaitingManualDns}` row applies.
 *
 * Read through the *same* registry seam the machine branches on. (linux's
 * `has_awaiting_dns_slot()` is the same gate for the same reason: "already
 * constructs the machine" is not the same as "reaches the row" — what matters
 * is the guard in front of it.)
 */
export async function hasAwaitingDnsSlot(): Promise<boolean> {
  return (await registryHasIdentity()) && (await registryLoadAwaitingDns()) !== null;
}

/**
 * True when the long-term store holds an identity AND a pending-factory-reset
 * slot — i.e. the `WizardAt{PendingFactoryReset}` row applies (the row the
 * shared machine checks before every other one). Same gate discipline as
 * `hasAwaitingDnsSlot()`.
 */
export async function hasPendingFactoryResetSlot(): Promise<boolean> {
  return (await registryHasIdentity()) && (await registryLoadPendingFactoryReset()) !== null;
}

/**
 * Mint the post-reset claim code, persist it, and PROVE it is on disk — the
 * single atomic decision point that closes gap CR-1
 * (`docs/goal/architecture/nest/common.md` § Client-state recoverability).
 *
 * Call this BEFORE dispatching `fauna.admin.factory_reset`, and pin the returned
 * code onto the request (`factoryReset(secretHex, code)`). On any throw the
 * caller MUST abort the dispatch: a reset dispatched without a persisted code is
 * precisely the bug this closes — the wiped box would boot with a code that
 * existed only in a reply the client may never render, leaving it at the
 * fresh/unclaimed floor yet un-claimable.
 *
 * The shared rail already reads the row back before handing out a code
 * (`SecretStore::set` is infallible on every platform, so "saved" is a claim to
 * verify, not to trust). The re-read here is a second, TS-side proof through
 * the same accessors the wizard will seed from.
 */
export async function mintAndPersistPendingFactoryReset(
  nestUrl: string,
  handle: string,
): Promise<string> {
  const code = await mintAndPersistPendingFactoryResetWasm(nestUrl, handle);
  const stored = await loadPendingFactoryReset();
  if (!code || !stored || stored.claimCode !== code) {
    // Never leave a code we cannot resume behind, either.
    await deletePendingFactoryReset();
    throw new Error(
      'pending-factory-reset: the claim code was not persisted — factory reset not dispatched',
    );
  }
  return code;
}
