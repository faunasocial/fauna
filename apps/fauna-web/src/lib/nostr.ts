import {
  nostrBadges,
  nostrPublishSigned,
} from './rpc';
import { linkBridgeChallenge } from './bridges';

// --- Connected apps (NIP-46 bunker) ---
// The nest is the user's NIP-46 signer (`docs/goal/ui/nostr.md` § The nest as
// the user's NIP-46 signer): third-party Nostr apps sign via the user's box.
// The *Connected apps* roster rides `fauna.nostr.bunker.*` via the typed
// wrappers in `$lib/rpc.ts`, re-exported here so the Nostr page keeps a single
// import surface.

export type { BunkerInvite, BunkerApp } from './rpc';
export {
  nostrBunkerCreateInvite,
  nostrBunkerList,
  nostrBunkerRevoke,
  nostrBunkerSetLabel,
} from './rpc';

// --- Zap signers (the NIP-57 trust root) ---
// `docs/goal/behavior/monetization.md` § Zap receipts — the trust model.
// Rides `fauna.nostr.zap_signers.*` via the typed wrappers in `$lib/rpc.ts`,
// re-exported here so the Nostr page keeps a single import surface.

export type { ZapSignerEntry } from './rpc';
export {
  nostrZapSignersList,
  nostrZapSignersAdd,
  nostrZapSignersRemove,
} from './rpc';

// --- NIP-07 browser extension ---

export function hasNip07(): boolean {
  return typeof window !== 'undefined' && 'nostr' in window;
}

export async function nip07GetPublicKey(): Promise<string> {
  return (window as any).nostr.getPublicKey();
}

export async function nip07SignEvent(event: any): Promise<any> {
  return (window as any).nostr.signEvent(event);
}

// The `nip07` link's params, proof of possession included (`nostr.md` § Errors
// & edge cases → *Proof of possession*): the nest mints a challenge (an unsigned
// kind-22242 event), the extension signs it, and the signed event rides back
// inside `fauna.bridges.link`'s params as NIP-01 JSON — the same carriage as
// `publishSignedEvent`. The nest links the key the SIGNATURE proves. This is
// the one sanctioned app-glue branch on the Nostr page (§ Architectural rules
// #4); both link sites (the Nostr page and the generic bridge card) call it.
export async function nip07LinkFields(
  secretHex: string,
  bridgeId: string,
): Promise<Record<string, string>> {
  const pubkey: string = await nip07GetPublicKey();
  const challenge = await linkBridgeChallenge(secretHex, bridgeId, 'nip07');
  const signed = await nip07SignEvent(challenge.payload);
  return { pubkey, proof_json: JSON.stringify(signed) };
}

// The NIP-07 flow's publish leg rides `nostr.events.publish_signed` WS-RPC
// (the native-content HTTP→WS-RPC rip — the deleted
// `POST /api/v1/nostr/publish-signed` route). The extension signs; the nest
// verifies + relay-enqueues.
export async function publishSignedEvent(secretHex: string, signedEvent: any): Promise<void> {
  await nostrPublishSigned(secretHex, JSON.stringify(signedEvent));
}

// --- Badges ---
// `nostr.badges.list` WS-RPC (the deleted `GET /api/v1/nostr/badges/{pubkey}`).

export type { NostrBadgeItem as NostrBadge } from './rpc';

export async function getNostrBadges(
  secretHex: string,
  pubkey: string,
): Promise<{ badges: import('./rpc').NostrBadgeItem[] }> {
  return { badges: await nostrBadges(secretHex, pubkey) };
}
