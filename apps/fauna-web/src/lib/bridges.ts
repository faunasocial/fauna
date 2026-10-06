// Bridge-management API — a thin TS seam over the WS-RPC façade (`rpc.ts`).
//
// The HTTP twins of these routes (`GET /api/v1/bridges`, `.../link`, …) were
// deleted by the nest WS-RPC sweep, so every call now rides a `fauna.bridges.*`
// kind. The wire types live in `rpc.ts` (mirroring `fauna_protocol::bridges_ui`
// field-by-field) and are re-exported here under their historical names so the
// bridges page imports are unchanged.

import * as rpc from './rpc';
import type { BridgeStatus, BridgeFollow, BridgeLinkChallengeReply, BridgeLinkReply } from './rpc';

export type {
  BridgeIdentity,
  BridgeSettingOption,
  BridgeSetting,
  BridgeLinkField,
  BridgeLinkMode,
  BridgeFollow,
} from './rpc';

// `BridgeInfo` / `BridgeLinkResponse` keep their names but are now the wire
// types verbatim (`BridgeStatus` adds the `error` slot the HTTP twin omitted).
export type BridgeInfo = BridgeStatus;
export type BridgeLinkResponse = BridgeLinkReply;

// --- Bridge management ---

export function listBridges(secretHex: string): Promise<BridgeInfo[]> {
  return rpc.bridgesList(secretHex);
}

export function linkBridge(
  secretHex: string,
  bridgeId: string,
  mode: string,
  fields: Record<string, string>,
): Promise<BridgeLinkResponse> {
  // `mode` is its own typed field; `fields` are the per-mode params
  // (`CborValue::Map` on the wire) — composed Rust-side in rpc.rs.
  return rpc.bridgesLink(secretHex, bridgeId, mode, fields);
}

// The proof-of-possession challenge an external signer signs before `linkBridge`
// in that mode is accepted (Nostr `nip07` — see `$lib/nostr` `nip07LinkFields`).
export function linkBridgeChallenge(
  secretHex: string,
  bridgeId: string,
  mode: string,
): Promise<BridgeLinkChallengeReply> {
  return rpc.bridgesLinkChallenge(secretHex, bridgeId, mode);
}

export function unlinkBridge(secretHex: string, bridgeId: string): Promise<void> {
  return rpc.bridgesUnlink(secretHex, bridgeId);
}

export function updateBridgeSettings(
  secretHex: string,
  bridgeId: string,
  settings: Record<string, unknown>,
): Promise<void> {
  return rpc.bridgesSetSettings(secretHex, bridgeId, settings);
}

// --- Bridge follows ---

export function listBridgeFollows(secretHex: string, bridgeId: string): Promise<BridgeFollow[]> {
  return rpc.bridgesListFollows(secretHex, bridgeId);
}

export function addBridgeFollow(
  secretHex: string,
  bridgeId: string,
  id: string,
  petname?: string,
  extra?: Record<string, unknown>,
): Promise<void> {
  return rpc.bridgesAddFollow(secretHex, bridgeId, id, petname, extra);
}

export function removeBridgeFollow(
  secretHex: string,
  bridgeId: string,
  followId: string,
): Promise<void> {
  return rpc.bridgesRemoveFollow(secretHex, bridgeId, followId);
}
