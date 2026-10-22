//! How `#lib/store`'s identity store applies a registry re-read.
//!
//! `identity.init()` runs on every route's `onMount` and re-reads the identity
//! from the account registry, which does not persist `registered` (it is the
//! live-session signal, `types.ts`). The background refresh that sets it is
//! single-flight and sticky per secret, so once it has settled a later
//! `init()` for the same identity does not run it again — a plain `set()` of
//! the re-read therefore dropped the verified session for the rest of the
//! document. That is what stranded web's succession closing act: the
//! successor's refresh landed, the settings route's `init()` re-read the
//! registry, and `session.authenticated` never came back
//! (`identity-succession.md` § The RecoveryKey → *At succession*).
//!
//! So `registered` is monotonic for one identity within one document: a
//! re-read of the SAME secret keeps it; any other identity starts unverified.
//!
//! Pure, so the rule is unit-tested apart from the wasm and the store.

import type { Identity } from './types.ts';

/** The identity to store when a registry re-read yields `incoming` while the
 *  store holds `current`: `incoming` verbatim, except that a verified session
 *  for the same secret survives. */
export function keepVerifiedSession(
  current: Identity | null,
  incoming: Identity | null,
): Identity | null {
  if (incoming && current?.registered && current.secretHex === incoming.secretHex) {
    return { ...incoming, registered: true };
  }
  return incoming;
}
