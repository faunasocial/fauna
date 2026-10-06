//! Personalization-model WS-RPC payload types — the
//! `fauna.personalization.model.{fetch,put,delete}` plane
//! (`docs/goal/behavior/topic-factors.md` § Wire & registry).
//!
//! These carry a user's **sealed personalization-model blob** (v1: the
//! trainable `topic:<hex>` factor's `TopicModel`) between a Fauna app and
//! its nest. The blob is sealed **client-side under the BackupKey** (the
//! account-state seal pipeline, `topic-factors.md` § At rest) — it is
//! **nest-opaque from birth**: unlike the spam model there is no server-side
//! train path at any point in the lifecycle, so the nest stores the bytes
//! verbatim in `personalization_models` and never decrypts them.
//!
//! All three kinds are **User-class** (`bridge_method_allowlist.rs`): the
//! nest derives the owning actor from the authenticated connection, so no
//! request carries an `actor_id` — a caller reads/writes/deletes only their
//! own rows.
//!
//! `sample_count` is **ADVISORY** — a client-reported training-event count
//! used as an adopt-if-larger reconcile hint across devices
//! (`topic-factors.md` § Placement). The nest never validates it against the
//! sealed blob (it can't — the blob is opaque) and never interprets it.
//!
//! Kind registry entries live in `kind.rs::register_personalization_kinds`.

use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;
use std::collections::BTreeMap;

use crate::Value;

/// Max trained factors per actor (`topic-factors.md` § At rest → Caps:
/// "> TRAINED_FACTORS_MAX = 32 factors per actor rejected at create";
/// compositions themselves cap at 64 entries). One owner for both enforcement
/// points: the nest's create-cap at `model.put` (a NEW `(actor, factor)` row
/// beyond this is rejected; overwriting an existing row is always allowed) and
/// the client's registry-add guard
/// (`fauna_client_config::add_trained_factor`). Hard-coded Rust — no human
/// chooses it.
pub const TRAINED_FACTORS_MAX: usize = 32;

/// The reserved delegable kind every `personalization_models` blob is sealed
/// under — topic models, the `cues:v1` rollup, text models alike
/// (`topic-factors.md` § At rest → *Re-keyed for the third-party plane*): the
/// blob's key is `DelegableSchedule::for_kind(MODEL_SEAL_KIND)`'s entry key, and
/// that pair is the grant twin of the `fauna:personalization:rw` scope. One
/// spelling for the sealing apps and the grant minter. Not a WS-RPC kind.
pub const MODEL_SEAL_KIND: &str = "fauna.personalization.model";

/// WS-RPC kind: fetch the calling actor's sealed model blob for one factor
/// (User). One source of truth for the client wrapper, the nest router, and
/// the caller-class allowlist.
pub const KIND_MODEL_FETCH: &str = "fauna.personalization.model.fetch";
/// WS-RPC kind: persist the calling actor's sealed model blob for one factor
/// (User).
pub const KIND_MODEL_PUT: &str = "fauna.personalization.model.put";
/// WS-RPC kind: delete the calling actor's sealed model blob for one factor
/// (User).
pub const KIND_MODEL_DELETE: &str = "fauna.personalization.model.delete";

// ── fauna.personalization.model.fetch ───────────────────────────────────────

/// Fetch the caller's sealed model blob for `factor`. v1 accepts only the
/// `topic:<32-lowercase-hex>` namespace (`fauna_core::scoring::is_topic_factor`);
/// future sealed factor kinds widen the accepted namespaces, additively.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PersonalizationModelFetchRequest {
    /// The factor key, e.g. `topic:<32-lowercase-hex>`.
    pub factor: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.personalization.model.fetch` reply. An absent row (never trained on
/// any device, or deleted) is `sealed_blob: None, sample_count: 0,
/// updated_at: 0` — the client then starts from a fresh empty model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PersonalizationModelFetchReply {
    /// The sealed blob exactly as the last put stored it, or `None` when no
    /// row exists.
    pub sealed_blob: Option<ByteBuf>,
    /// ADVISORY client-reported training-event count (adopt-if-larger
    /// reconcile hint) — echoed, never validated (module docs).
    pub sample_count: u32,
    /// Nest-side epoch seconds of the last put; `0` when no row exists.
    pub updated_at: i64,
    /// Forward-compat catch-all (transport.md rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.personalization.model.put ─────────────────────────────────────────

/// Persist the caller's sealed model blob for `factor`, overwriting any prior
/// row (idempotent overwrite — replay-safe; a concurrent two-device put is
/// last-put-wins, `topic-factors.md` § Placement). The nest validates the
/// factor namespace, rejects empty blobs and blobs over the 512 KiB cap, and
/// rejects a NEW row beyond the per-actor factor cap (overwrite of an
/// existing row is always allowed).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PersonalizationModelPutRequest {
    /// The factor key, e.g. `topic:<32-lowercase-hex>`.
    pub factor: String,
    /// The model, sealed client-side under the BackupKey — nest-opaque.
    pub sealed_blob: ByteBuf,
    /// ADVISORY training-event count (module docs) — stored verbatim.
    pub sample_count: u32,
    /// Forward-compat catch-all (transport.md rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.personalization.model.put` reply — `status: "ok"` on success
/// (failures ride the `RpcError` channel).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PersonalizationModelPutReply {
    pub status: String,
    /// Forward-compat catch-all (transport.md rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.personalization.model.delete ──────────────────────────────────────

/// Delete the caller's sealed model row for `factor`. Idempotent: deleting an
/// absent row succeeds with `deleted: false`. This is the user's own
/// UI-driven destruction of user-revocable data (`topic-factors.md` § At
/// rest) — the registry remove rides `fauna.state.personalization`, separately.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PersonalizationModelDeleteRequest {
    /// The factor key, e.g. `topic:<32-lowercase-hex>`.
    pub factor: String,
    /// Forward-compat catch-all (transport.md rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.personalization.model.delete` reply — `status: "ok"`; `deleted`
/// says whether a row actually existed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PersonalizationModelDeleteReply {
    pub status: String,
    pub deleted: bool,
    /// Forward-compat catch-all (transport.md rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::test_support::assert_round_trips;
    use crate::codec::{decode_strict as decode, encode_canonical};

    fn topic_key() -> String {
        format!("topic:{}", "ab".repeat(16))
    }

    #[test]
    fn fetch_request_and_replies_round_trip() {
        assert_round_trips(&PersonalizationModelFetchRequest {
            factor: topic_key(),
            extra: BTreeMap::new(),
        });
        // Absent row: `None` blob round-trips as a single Option — not the
        // nested-Option footgun the dag-cbor wire can't represent.
        let absent = PersonalizationModelFetchReply {
            sealed_blob: None,
            sample_count: 0,
            updated_at: 0,
            extra: BTreeMap::new(),
        };
        assert_round_trips(&absent);
        assert_round_trips(&PersonalizationModelFetchReply {
            sealed_blob: Some(ByteBuf::from(vec![0xDEu8, 0xAD, 0xBE, 0xEF])),
            sample_count: 12,
            updated_at: 1_700_000_000,
            extra: BTreeMap::new(),
        });
    }

    #[test]
    fn put_request_and_reply_round_trip() {
        assert_round_trips(&PersonalizationModelPutRequest {
            factor: topic_key(),
            sealed_blob: ByteBuf::from(vec![1u8, 2, 3, 4, 5]),
            sample_count: 3,
            extra: BTreeMap::new(),
        });
        assert_round_trips(&PersonalizationModelPutReply {
            status: "ok".into(),
            extra: BTreeMap::new(),
        });
    }

    #[test]
    fn delete_request_and_reply_round_trip() {
        assert_round_trips(&PersonalizationModelDeleteRequest {
            factor: topic_key(),
            extra: BTreeMap::new(),
        });
        assert_round_trips(&PersonalizationModelDeleteReply {
            status: "ok".into(),
            deleted: false,
            extra: BTreeMap::new(),
        });
    }

    #[test]
    fn unknown_keys_from_a_newer_peer_are_preserved() {
        // Additive-everywhere: a newer peer's extra key survives a
        // decode → re-encode round trip through an older reader.
        #[derive(serde::Serialize)]
        struct NewerFetchReply {
            sealed_blob: Option<ByteBuf>,
            sample_count: u32,
            updated_at: i64,
            reconcile_hint: String,
        }
        let bytes = encode_canonical(&NewerFetchReply {
            sealed_blob: None,
            sample_count: 7,
            updated_at: 42,
            reconcile_hint: "adopt-if-larger".into(),
        })
        .unwrap();
        let back: PersonalizationModelFetchReply = decode(&bytes).unwrap();
        assert_eq!(back.sample_count, 7);
        assert_eq!(
            back.extra.get("reconcile_hint"),
            Some(&Value::String("adopt-if-larger".into()))
        );
        let re = encode_canonical(&back).unwrap();
        let back2: PersonalizationModelFetchReply = decode(&re).unwrap();
        assert_eq!(back, back2);
    }
}
