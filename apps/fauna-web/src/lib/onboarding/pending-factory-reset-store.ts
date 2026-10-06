// Read/clear face of the pending-factory-reset slot — the crash-atomic
// decision point of `fauna.admin.factory_reset` (gap CR-1,
// `docs/goal/architecture/nest/common.md` § Client-state recoverability).
//
// A thin camelCase wrapper over the shared registry's per-actor slot (CR-3):
// storage and namespacing live in shared Rust (`fauna-client-accounts`).
// The launch machine routes the `LaunchWizardEntry::PendingFactoryReset` row
// off the SAME per-actor slot — one store, one shape.
//
// There is deliberately NO save here: the row is written ONLY through
// `mintAndPersistPendingFactoryReset()` (`launch-persistence.ts`), which
// returns the code only after proving the row landed — so a code cannot be
// held, and a reset cannot be dispatched, without a resumable slot on disk.
// Cleared once the re-claim completes (wizard exits `LoggedIn`).

import {
  registryLoadPendingFactoryReset,
  registryDeletePendingFactoryReset,
} from '../wasm-launch';

/** camelCase mirror of Rust's `PendingFactoryResetRecord` (`nest_url`,
 *  `handle`, `claim_code`). */
export interface PendingFactoryResetRecord {
  nestUrl: string;
  handle: string;
  claimCode: string;
}

/**
 * Returns the persisted record for the ACTIVE account, or `null`. A partial
 * or unparseable slot reads as absent rather than seeding the wizard with
 * bogus data (the registry adapter's opaque-JSON contract).
 */
export async function loadPendingFactoryReset(): Promise<PendingFactoryResetRecord | null> {
  const rec = await registryLoadPendingFactoryReset();
  if (!rec) return null;
  return { nestUrl: rec.nest_url, handle: rec.handle, claimCode: rec.claim_code };
}

export async function deletePendingFactoryReset(): Promise<void> {
  await registryDeletePendingFactoryReset();
}
