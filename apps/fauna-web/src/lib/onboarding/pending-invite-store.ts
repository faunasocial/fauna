// The wizard's "pending invite" slot — a thin camelCase wrapper over the
// shared registry's per-actor slot (CR-3). Storage and namespacing live in
// shared Rust (`fauna-client-accounts`); this module only maps camelCase ↔ serde
// snake_case over the `registry*` wasm accessors, so the record the wizard
// seeds is byte-for-byte the record the launch machine routes on.

import {
  registryLoadPendingInvite,
  registrySavePendingInvite,
  registryDeletePendingInvite,
} from '../wasm-launch';

export interface PendingInviteRecord {
  nestUrl: string;
  handle: string;
  requestId: string;
  /** Opaque to this layer — `seed_pending_invite` parses it. */
  statusJson: string;
}

/**
 * Returns the persisted record for the ACTIVE account, or `null`. A partial
 * or unparseable slot reads as absent (the registry adapter's opaque-JSON
 * contract), so an interrupted save never seeds the wizard with bogus data.
 */
export async function loadPendingInvite(): Promise<PendingInviteRecord | null> {
  const rec = await registryLoadPendingInvite();
  if (!rec) return null;
  return {
    nestUrl: rec.nest_url,
    handle: rec.handle,
    requestId: rec.request_id,
    statusJson: rec.status_json,
  };
}

export async function savePendingInvite(rec: PendingInviteRecord): Promise<void> {
  const ok = await registrySavePendingInvite({
    nest_url: rec.nestUrl,
    handle: rec.handle,
    request_id: rec.requestId,
    status_json: rec.statusJson,
  });
  if (!ok) {
    // Refused (no active identity yet, or a quota failure inside the store) —
    // non-fatal: the wizard already advanced; the user may have to re-submit
    // the invite request on next launch in this degraded case.
    console.warn('[onboarding] pending-invite save refused');
  }
}

export async function deletePendingInvite(): Promise<void> {
  await registryDeletePendingInvite();
}
