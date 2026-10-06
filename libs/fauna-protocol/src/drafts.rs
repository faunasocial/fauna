//! Draft-persistence WS-RPC payload types — the `fauna.drafts.{get,put}` plane.
//!
//! These carry the **opaque sealed `__drafts` blob** (a `DraftStore` snapshot
//! sealed under the owner's `BackupKey`, per `libs/fauna-conversations` +
//! `docs/goal/behavior/file-sync.md` § Drafts Sync) between a Fauna app and
//! nest. The nest stores the bytes opaque — it never decrypts them; no nest path
//! reads drafts in either storage mode. Both kinds are **User-class**
//! (`bridge_method_allowlist.rs`): the nest derives the owning actor from the
//! authenticated connection, so neither request carries an `actor_id` — a user
//! reads/writes only their own drafts.
//!
//! **Why a `path` dimension.** Drafts are inherently multi-rail (the Feed,
//! Conversations, and Events compose surfaces). `path` keys the rail's blob (e.g. `"conversations"`,
//! `"posts"`, `"events"`) within the one `__drafts` reserved folder, so the
//! nest plane is rail-agnostic and the per-rail client legs land additively with
//! no nest change. The nest treats `path` as an opaque key, hashed to a
//! `path_hash`. Internal layout is the
//! `DraftStore` v2 implementation's choice (design `2026-05-13-drafts-at-rest-
//! design.md` §146); this plane only fixes that a blob is addressed by `path`.

use crate::Value;
use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;
use std::collections::BTreeMap;

/// WS-RPC kind: fetch the calling actor's sealed drafts blob for `path` (User).
/// One source of truth for the client wrapper, the nest router, and the
/// caller-class allowlist.
pub const KIND_GET: &str = "fauna.drafts.get";
/// WS-RPC kind: persist the calling actor's sealed drafts blob for `path` (User).
pub const KIND_PUT: &str = "fauna.drafts.put";

/// The **closed** set of rail keys the `fauna.drafts.{get,put}` wire accepts —
/// one per compose surface (`docs/goal/behavior/reserved-folders.md`
/// § Drafts Sync). One source of truth for the client wrappers and the nest's
/// validation, so neither side re-lists the vocabulary.
///
/// **Why closed rather than an opaque key.** The nest bounds a reserved rail by
/// a fixed path count times a per-blob cap — not by metering it against the
/// storage quota. Drafts has one path per rail, so the enumeration *is* the
/// path-count half of that bound. Leaving the key
/// client-chosen made an actor's `__drafts` footprint unbounded in the **count**
/// of live rows, each of which GC pins forever
/// (`bins/fauna-nest/src/backup/gc.rs` — latest-per-path is live by definition).
///
/// It is also what makes the resting-plaintext `path` honest: a value the nest
/// constrains to three frozen constants, identical on every deployment and every
/// account, is the same class as the machine-authored labels
/// `docs/goal/behavior/path-sealing.md` § Deliberate non-seals already exempts —
/// where a free-form client-chosen string was not.
///
/// Growing this set is additive and nest-gated: a client sending a rail an older
/// nest does not know is refused, so a new rail ships only once nests admit it.
pub const DRAFT_RAILS: [&str; 3] = [RAIL_CONVERSATIONS, RAIL_POSTS, RAIL_EVENTS];

/// The conversations compose rail's [`DRAFT_RAILS`] key — reference this
/// rather than retyping the literal, so a leg's `RAIL`/`EVENTS_RAIL`-style
/// constant is a pointer at the wire's enumeration, not an independently
/// hand-typed copy of it.
pub const RAIL_CONVERSATIONS: &str = "conversations";
/// The feed compose rail's [`DRAFT_RAILS`] key — see [`RAIL_CONVERSATIONS`].
pub const RAIL_POSTS: &str = "posts";
/// The events compose rail's [`DRAFT_RAILS`] key — see [`RAIL_CONVERSATIONS`].
pub const RAIL_EVENTS: &str = "events";

/// Whether `path` is one of the [`DRAFT_RAILS`] the drafts plane accepts.
pub fn is_ratified_rail(path: &str) -> bool {
    DRAFT_RAILS.contains(&path)
}

/// Hard ceiling on a single sealed `__drafts` blob, refused before any
/// blob-store write — the drafts twin of `config_handlers`'
/// `MAX_CONFIG_BLOB_BYTES`.
///
/// Sized deliberately rather than copied. A rail's blob is the whole rail's
/// draft set already **zstd-compressed and then sealed** client-side, so a
/// megabyte of ciphertext stands for several megabytes of draft prose — orders
/// of magnitude past any real compose backlog — while keeping the same
/// half-of-the-2-MiB-frame headroom (`fauna_core::transport::
/// MAX_RPC_WS_MESSAGE_SIZE`) that leaves room for the request's own CBOR
/// framing. With [`DRAFT_RAILS`] closed at three, this cap bounds an actor's
/// **live** `__drafts` footprint at 3 MiB.
///
/// ⚠ **Live is not resting, and these two constants alone never bounded the
/// resting cost**. Every
/// `put` registers a *fresh* blob and appends a row, so what rests is the live
/// 3 MiB plus the rail's not-yet-reclaimed superseded blobs — a *ratio*,
/// (writes per GC cycle) × 1 MiB per rail, not a constant. That term is finite
/// only because the nest's recorder collapses each rail's history as it writes
/// the new head; without it the pin is permanent. Rule + the properties a rail
/// must satisfy: `docs/goal/behavior/file-versions.md` § Retention.
pub const MAX_DRAFTS_BLOB_BYTES: usize = 1024 * 1024;

/// `fauna.drafts.get` (User) — fetch the calling actor's sealed drafts blob for
/// a rail `path`. The owning actor is the authenticated caller.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct GetDraftsRequest {
    /// The rail key whose blob to fetch (e.g. `"conversations"`).
    pub path: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are preserved
    /// here and re-emitted on encode (transport.md § Schema and forward-compat
    /// discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.drafts.get` reply — the opaque sealed blob, or `None` when the caller
/// has never persisted drafts for this `path` (the client then starts from an
/// empty `DraftStore`). Replay-safe pure read.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct GetDraftsReply {
    pub blob: Option<ByteBuf>,
    /// Forward-compat catch-all (see [`GetDraftsRequest::extra`]).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.drafts.put` (User) — persist the calling actor's sealed drafts blob
/// for `path`, overwriting any prior one. Idempotent overwrite (same bytes
/// twice → same state), so replay-safe.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct PutDraftsRequest {
    /// The rail key whose blob to persist (e.g. `"conversations"`).
    pub path: String,
    pub blob: ByteBuf,
    /// Forward-compat catch-all (see [`GetDraftsRequest::extra`]).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.drafts.put` reply — `{ ok: true }` on success; the typed wrapper
/// discards it (success/failure is carried by the `Result`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct PutDraftsReply {
    pub ok: bool,
    /// Forward-compat catch-all (see [`GetDraftsRequest::extra`]).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{decode_strict as decode, encode_canonical};

    #[test]
    fn get_request_and_empty_blob_reply_round_trip() {
        let req = GetDraftsRequest {
            path: "conversations".into(),
            extra: Default::default(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let back: GetDraftsRequest = decode(&bytes).unwrap();
        assert_eq!(back, req);

        // `None` blob (caller has never saved) round-trips as a single Option —
        // not the nested-Option footgun the dag-cbor wire can't represent.
        let reply = GetDraftsReply {
            blob: None,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let back: GetDraftsReply = decode(&bytes).unwrap();
        assert_eq!(back, reply);
    }

    #[test]
    fn get_reply_with_blob_round_trips() {
        let reply = GetDraftsReply {
            blob: Some(ByteBuf::from(vec![0x01u8, 0xDE, 0xAD, 0xBE, 0xEF])),
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let back: GetDraftsReply = decode(&bytes).unwrap();
        assert_eq!(back, reply);
    }

    #[test]
    fn put_request_and_reply_round_trip() {
        let req = PutDraftsRequest {
            path: "conversations".into(),
            blob: ByteBuf::from(vec![0x01u8, 2, 3, 4, 5]),
            extra: Default::default(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let back: PutDraftsRequest = decode(&bytes).unwrap();
        assert_eq!(back, req);

        let reply = PutDraftsReply {
            ok: true,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let back: PutDraftsReply = decode(&bytes).unwrap();
        assert_eq!(back, reply);
    }
}
