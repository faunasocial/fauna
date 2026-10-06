// Actor-id byte/hex helpers shared by the admin surfaces. The `fauna.admin.*`
// WS-RPC replies carry raw 32-byte actor ids, which serde_wasm_bindgen surfaces
// as a `Uint8Array` or a plain `number[]` depending on the path; `toBytes`
// normalizes both, and `actorHex` renders the lowercase-hex form used as a stable
// per-row key / title via the shared Rust `hexFull` (no local byte→hex hand-roll —
// priority #2/#4). Single home so the admin-users hub and the admin-dns catch-all
// picker don't re-derive the wire-shape normalization.

import { hexFull } from '$lib/wasm';

/** Normalize a raw actor id (either wire shape) to a `Uint8Array`. */
export function toBytes(actorId: Uint8Array | number[]): Uint8Array {
  return actorId instanceof Uint8Array ? actorId : Uint8Array.from(actorId);
}

/** Lowercase, zero-padded full hex of a raw actor id — the actor-id display/key
 *  label, delegating byte→hex rendering to shared Rust (`hexFull`). */
export function actorHex(actorId: Uint8Array | number[]): string {
  return hexFull(toBytes(actorId));
}

/** Parse a user-typed hex actor id back to bytes, or `null` when the input
 *  isn't hex (odd length / non-hex digits) — the inverse of `actorHex` for the
 *  hand-entry surfaces (`family-contact-add-input`: v1 takes a hex actor id, no
 *  handle resolution — family-safety.md § App surface). Mirrors windows'
 *  `Convert.FromHexString` + `FormatException` contract: shape-only validation
 *  here, the nest re-validates that the actor exists. */
export function actorIdFromHex(input: string): Uint8Array | null {
  return bytesFromHex(input);
}

/** Decode machine-produced hex to bytes with NO validation — the fast path for
 *  hex the app itself minted or a machine snapshot handed back (secret hex from
 *  the identity store, post/actor ids off the wire, e2e echo payloads), where a
 *  malformed string is a programming error rather than an input state. Anything
 *  user-typed goes through `bytesFromHex`/`actorIdFromHex` below instead, whose
 *  `null` is the "not hex" answer the entry surfaces render. Single home for
 *  what used to be three drifting per-file copies (sweep's census). */
export function hexToBytes(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}

/** Parse any lowercase/uppercase hex string to bytes, or `null` when the input
 *  isn't hex (odd length / non-hex digits). The shape-only half of
 *  `actorIdFromHex`, split out because actor ids are not the only hex-carrying
 *  field a machine snapshot hands the SPA — `SnapshotFileRow.manifest_hash`
 *  crosses the wasm boundary as hex for the same boundary reason, and the
 *  download walk needs it back as bytes. */
export function bytesFromHex(input: string): Uint8Array | null {
  const hex = input.trim();
  if (hex.length === 0 || hex.length % 2 !== 0 || !/^[0-9a-fA-F]+$/.test(hex)) return null;
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}
