// The wizard's "awaiting manual DNS" slot — a thin camelCase wrapper over the
// shared registry's per-actor slot (CR-3), exactly like
// `pending-invite-store.ts`. Written when the wizard exits with
// `WizardOutcome::AwaitingManualDns` and cleared at `LoggedIn` — inside the
// shared `persist_logged_in`, never at the claim itself.
// The launch machine reads the SAME per-actor slot through
// `RegistryLaunchPersistence`, so the record we seed is the record it routed
// on — one store, one shape (`docs/goal/behavior/onboarding.md` § Long-term
// store contract).

import {
  registryLoadAwaitingDns,
  registryLoadAwaitingDnsJson,
  registrySaveAwaitingDns,
  registryClearAwaitingDns,
} from '../wasm-launch';

export interface AwaitingDnsRecord {
  nestUrl: string;
  handle: string;
  /** Opaque to this layer — produced by `awaitingDnsRecordsJson()` and handed
   *  back verbatim to `seedAwaitingManualDnsJson()`. Never hand-built: serde
   *  emits `record_type` while the WASM binding exposes `recordType`, so a
   *  hand-rolled round-trip silently yields an EMPTY record list. */
  recordsJson: string;
  claimCode: string;
}

/**
 * Returns the persisted record for the ACTIVE account, or `null`. A partial
 * record (e.g. one carrying no
 * `handle`) reads as absent rather than half-seeding the wizard.
 */
export async function loadAwaitingDns(): Promise<AwaitingDnsRecord | null> {
  const rec = await registryLoadAwaitingDns();
  if (!rec) return null;
  return {
    nestUrl: rec.nest_url,
    handle: rec.handle,
    recordsJson: rec.dns_records_json,
    claimCode: rec.claim_code,
  };
}

/**
 * The persisted record for the ACTIVE account as the ONE opaque JSON string the
 * registry holds, or `null` — the shape `seedAwaitingManualDnsRecordJson` takes,
 * so a relaunch hands the machine every field the slot carries (the box's
 * built-with identity, its reach address) with nothing re-shaped here.
 */
export async function loadAwaitingDnsJson(): Promise<string | null> {
  return registryLoadAwaitingDnsJson();
}

export async function saveAwaitingDns(rec: AwaitingDnsRecord): Promise<void> {
  const ok = await registrySaveAwaitingDns({
    nest_url: rec.nestUrl,
    handle: rec.handle,
    dns_records_json: rec.recordsJson,
    claim_code: rec.claimCode,
  });
  if (!ok) {
    // Non-fatal for the in-session surface — it renders off the machine's
    // snapshot, not off this slot. It does cost the user the *relaunch*
    // resume, so warn loudly. Mirrors savePendingInvite().
    console.warn('[onboarding] awaiting-dns save refused');
  }
}

/**
 * Clear the slot. Called only at `LoggedIn` — the one clearing moment
 * (`onboarding.md` § Long-term store contract, ratified 2026-09-21); the shared
 * `persist_logged_in` does it on the cold-boot path, and this wrapper is the
 * append arm's explicit call at the same terminal. Never at the claim itself:
 * the slot is what lets a force-quit on the NAT page or the trust offer
 * relaunch back into the resume. The launch row outranks every other row, so a
 * slot left behind pins the admin on "Almost ready" forever. Deletion is
 * deliberately client-side: the shared `LaunchPersistence` trait carries no
 * `delete_awaiting_dns` (the machine never writes the store).
 */
export async function deleteAwaitingDns(): Promise<void> {
  await registryClearAwaitingDns();
}
