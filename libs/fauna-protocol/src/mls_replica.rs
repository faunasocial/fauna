//! MLS state-replica WS-RPC payload types — the `fauna.mls.{get,put}` plane.
//!
//! These carry the **opaque sealed `__mls` blobs** (the cross-device MLS state
//! replica: the `provider` snapshot and the per-channel `history/<channel_hex>`
//! slices, each sealed under the owner's `BackupKey` — `libs/fauna-mls::
//! state_replica` / `libs/fauna-conversations::store::history`;
//! `docs/goal/behavior/devices.md` § Cross-device MLS group-state sync,
//! `docs/goal/behavior/file-sync.md` § MLS state replica) between a Fauna
//! app and nest. The nest stores the bytes opaque — it never decrypts them;
//! no nest path reads replica contents in either storage mode. Both kinds are
//! **User-class** (`bridge_method_allowlist.rs`): the nest derives the owning
//! actor from the authenticated connection, so neither request carries an
//! `actor_id` — a user reads/writes only their own replica.
//!
//! Structurally the `fauna.drafts.*` `path` plane **plus an
//! optimistic-concurrency CAS**: the replica carries user-irrecoverable data (own-message plaintext
//! history; live ratchet state), so `put` takes the optimistic-concurrency
//! [`ReplicaBase`] precondition from day one — a lost concurrent write is data
//! loss, not an inconvenience. On a mismatch the nest rejects with
//! `fauna.mls.conflict`; the client re-`get`s → unseals → merges
//! (`merge_provider_replicas` / `merge_history_slices`, both client-side —
//! only the client holds `BackupKey`) → reseals → retries.

use crate::Value;
use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;
use std::collections::BTreeMap;

/// WS-RPC kind: fetch the calling actor's sealed replica blob for `path` (User).
pub const KIND_GET: &str = "fauna.mls.get";
/// WS-RPC kind: persist the calling actor's sealed replica blob for `path` (User).
pub const KIND_PUT: &str = "fauna.mls.put";
/// Error code returned by `fauna.mls.put` when the CAS precondition fails.
pub const CODE_CONFLICT: &str = "fauna.mls.conflict";
/// Error code returned by `fauna.mls.put` when the sealed blob exceeds
/// [`MAX_MLS_REPLICA_BYTES`].
pub const CODE_TOO_LARGE: &str = "fauna.mls.too_large";

/// Max size of a single sealed replica blob (`provider` or `history/<hex>`) the
/// `fauna.mls.put` plane accepts. Leaves 64 KiB of envelope headroom under the
/// 2 MiB WS frame ([`fauna_core::transport::MAX_RPC_WS_MESSAGE_SIZE`]) so an
/// over-size blob is rejected with a clean [`CODE_TOO_LARGE`] domain error
/// rather than a raw frame drop (defense-in-depth + priority #3 parity with the
/// sibling blob handlers' `MAX_WRAPPED_MLS_BYTES` / `MAX_MLS_SNAPSHOT_BYTES`).
/// The shared client wrapper checks this before the put (no wasted round-trip);
/// the nest re-checks it (a non-conforming client can still over-send).
///
/// A `history/<channel_hex>` slice that legitimately grows past this cap needs
/// the **chunked/append-only history path** — a deliberately deferred follow-on
/// (tracked internally: "revisit only if history-blob size becomes a measured
/// problem"). Until
/// then an over-cap slice surfaces the error loudly instead of that channel
/// silently failing to sync.
pub const MAX_MLS_REPLICA_BYTES: usize = fauna_core::transport::MAX_RPC_WS_MESSAGE_SIZE - 64 * 1024;

/// Optimistic-concurrency precondition for [`PutMlsReplicaRequest`] — the
/// compare-and-set base, scoped per `(actor, path)`.
///
/// `base` is required on every put (the base-less blind last-writer-wins put
/// left the wire with the compat-remnant sweep); `Absent` asserts "no blob is
/// stored for this path yet" — which is what catches the concurrent-first-write
/// race. The hash is the `blake3` digest of the **sealed blob bytes the client
/// loaded** (the nest content-addresses on those same bytes), never a re-seal.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
pub enum ReplicaBase {
    /// The client observed no stored blob for this path. The nest stores only
    /// if none exists yet.
    #[default]
    Absent,
    /// The client merged against the stored blob with this 32-byte content
    /// hash. The nest stores only if the current stored hash matches.
    Hash(#[serde(with = "serde_bytes")] [u8; 32]),
}

/// `fauna.mls.get` (User) — fetch the calling actor's sealed replica blob for
/// `path` (`"provider"` or `"history/<channel_hex>"`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct GetMlsReplicaRequest {
    /// The replica path whose blob to fetch.
    pub path: String,
    /// **Ask for the stored blob's content hash only, not its bytes** — the
    /// cheap "did another device write since I last looked?" probe a running
    /// device sends once per receive sweep (`devices.md` § Cross-device MLS
    /// group-state sync → *A sibling-joined group is adopted mid-session*).
    /// The nest answers with [`GetMlsReplicaReply::hash`] and no blob.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hash_only: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are preserved
    /// here and re-emitted on encode (transport.md § Schema and forward-compat
    /// discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.mls.get` reply — the opaque sealed blob, or `None` when the caller
/// has never persisted a blob for this `path`. Replay-safe pure read.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct GetMlsReplicaReply {
    pub blob: Option<ByteBuf>,
    /// The `blake3` content hash of the stored blob — the same bytes
    /// [`ReplicaBase::Hash`] names — or `None` when nothing is stored.
    /// Carried on every reply (so a full read needs no client-side hashing)
    /// and is the whole answer to a [`GetMlsReplicaRequest::hash_only`] probe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<ByteBuf>,
    /// Forward-compat catch-all (see [`GetMlsReplicaRequest::extra`]).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.mls.put` (User) — persist the calling actor's sealed replica blob
/// for `path`, gated on `base` when present. Idempotent for equal bytes and
/// gated for concurrent writers, so replay-safe.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct PutMlsReplicaRequest {
    /// The replica path whose blob to persist.
    pub path: String,
    pub blob: ByteBuf,
    /// Optimistic-concurrency precondition — required. See [`ReplicaBase`].
    pub base: ReplicaBase,
    /// Forward-compat catch-all (see [`GetMlsReplicaRequest::extra`]).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.mls.put` reply — `{ ok: true }` on success; a CAS mismatch is the
/// `fauna.mls.conflict` error, not a reply.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct PutMlsReplicaReply {
    pub ok: bool,
    /// Forward-compat catch-all (see [`GetMlsReplicaRequest::extra`]).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{decode_strict, encode_canonical};

    #[test]
    fn put_request_round_trips_with_each_base() {
        for base in [ReplicaBase::Absent, ReplicaBase::Hash([7u8; 32])] {
            let req = PutMlsReplicaRequest {
                path: "provider".into(),
                blob: ByteBuf::from(vec![0x01, 2, 3]),
                base,
                extra: Default::default(),
            };
            let bytes = encode_canonical(&req).unwrap();
            let back: PutMlsReplicaRequest = decode_strict(&bytes).unwrap();
            assert_eq!(back, req);
        }
    }

    /// The pre-CAS blind put (no `base` key) left the wire with the
    /// compat-remnant sweep: it no longer decodes.
    #[test]
    fn a_base_less_put_is_refused_on_decode() {
        #[derive(serde::Serialize)]
        struct LegacyPut {
            path: String,
            blob: ByteBuf,
        }
        let bytes = encode_canonical(&LegacyPut {
            path: "provider".into(),
            blob: ByteBuf::from(vec![0x01]),
        })
        .unwrap();
        assert!(decode_strict::<PutMlsReplicaRequest>(&bytes).is_err());
    }
}
