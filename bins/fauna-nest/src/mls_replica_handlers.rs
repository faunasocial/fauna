//! WS-RPC handlers for the user-facing `fauna.mls.{get,put}` surface — the
//! cross-device MLS state-replica plane (`docs/goal/behavior/devices.md`
//! § Cross-device MLS group-state sync; `docs/goal/behavior/file-sync.md`
//! § MLS state replica).
//!
//! A Fauna app seals each replica blob (`provider` /
//! `history/<channel_hex>`) under its own `BackupKey` and persists the
//! **opaque** bytes here. The nest never holds that key, so it stores/returns
//! the bytes opaque — no nest path reads replica contents in either storage
//! mode. Structurally [`crate::drafts_handlers`] plus a CAS: the replica carries user-irrecoverable
//! data, so `put` enforces the [`ReplicaBase`] precondition and rejects a
//! moved store with `fauna.mls.conflict` (the client merges client-side and
//! retries — only the client holds `BackupKey`).
//!
//! Caller-class enforcement lives in `bridge_method_allowlist::is_permitted`
//! (`User | Admin` here). The owning actor is the authenticated connection
//! actor — neither kind carries an `actor_id`, so a caller reads/writes only
//! their own replica (self-scoped, no escalation surface).

use std::time::Duration;

use fauna_protocol::{
    RpcError, Value, decode_strict as decode,
    mls_replica::{
        CODE_TOO_LARGE, GetMlsReplicaReply, GetMlsReplicaRequest, KIND_GET, KIND_PUT,
        MAX_MLS_REPLICA_BYTES, PutMlsReplicaReply, PutMlsReplicaRequest,
    },
};

use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

#[cfg(test)]
use crate::routes::AppState;
#[cfg(test)]
use std::sync::Arc;

// ── Helpers (mirror drafts_handlers.rs) ─────────────────────────

use crate::rpc_errors::{encode_reply, internal, malformed};

fn unavailable(reason: &str) -> RpcError {
    crate::rpc_errors::unavailable_ns("mls", reason)
}

/// The CAS precondition did not match the stored state — the client re-`get`s,
/// merges, and retries (`merge_provider_replicas` / `merge_history_slices`).
fn conflict() -> RpcError {
    crate::rpc_errors::bare_conflict_ns("mls")
}

/// The sealed blob exceeds [`MAX_MLS_REPLICA_BYTES`] — defense-in-depth so an
/// over-size blob is a clean domain error, not a raw WS-frame drop (the shared
/// client wrapper checks this before the put; a non-conforming client can still
/// over-send). A `history/<hex>` slice hitting this needs the deferred chunked
/// history path (design § 2).
fn too_large(size: usize) -> RpcError {
    let mut e = RpcError::new(CODE_TOO_LARGE, "error.mls.too_large");
    e.details = Some(Box::new(Value::String(format!(
        "sealed replica blob is {size} bytes, over the {MAX_MLS_REPLICA_BYTES}-byte cap"
    ))));
    e
}

use crate::bridge_method_allowlist::require_permission_default as require_permission;

/// A replica `path` must be non-empty (`"provider"`, `"history/<hex>"`).
fn validate_path(path: &str) -> Result<(), RpcError> {
    if path.is_empty() {
        return Err(malformed("mls replica path must not be empty"));
    }
    Ok(())
}

// ── fauna.mls.get ───────────────────────────────────────────────

fn mls_get_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, KIND_GET).await?;
            let req: GetMlsReplicaRequest = decode(&payload).map_err(malformed)?;
            validate_path(&req.path)?;
            let backup_svc = state
                .backup_service
                .as_ref()
                .ok_or_else(|| unavailable("blob storage not configured"))?;
            let store = backup_svc.local_blob_store();
            // The hash is the manifest's, never recomputed from the bytes: a
            // `hash_only` probe must not pay for the blob it exists to avoid
            // fetching, and it is the same digest the CAS gate compares.
            let hash = state
                .db
                .get_mls_replica_hash(&actor_id, &req.path)
                .await
                .map_err(internal)?;
            let blob = if req.hash_only || hash.is_none() {
                None
            } else {
                state
                    .db
                    .get_mls_replica_blob(&actor_id, &req.path, &store)
                    .await
                    .map_err(internal)?
            };
            encode_reply(&GetMlsReplicaReply {
                blob: blob.map(serde_bytes::ByteBuf::from),
                hash: hash.map(|h| serde_bytes::ByteBuf::from(h.to_vec())),
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.mls.put ───────────────────────────────────────────────

fn mls_put_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, KIND_PUT).await?;
            let req: PutMlsReplicaRequest = decode(&payload).map_err(malformed)?;
            validate_path(&req.path)?;
            if req.blob.is_empty() {
                return Err(malformed("mls replica blob must not be empty"));
            }
            if req.blob.len() > MAX_MLS_REPLICA_BYTES {
                return Err(too_large(req.blob.len()));
            }
            let backup_svc = state
                .backup_service
                .as_ref()
                .ok_or_else(|| unavailable("blob storage not configured"))?;

            // Store the already-sealed blob RAW (no encode_blob wrapping) so
            // `get` returns the exact bytes the client must unseal. On a CAS
            // conflict the blob is left unreferenced — GC-reclaimable, because
            // the db method registers metadata before the gate.
            let sealed = req.blob.as_ref();
            let hash_bytes: [u8; 32] = *blake3::hash(sealed).as_bytes();
            let hash = fauna_core::data::ContentHash::from_digest_raw(hash_bytes);
            backup_svc
                .local_blob_store()
                .put(&hash, sealed)
                .await
                .map_err(internal)?;
            let stored = state
                .db
                .cas_record_mls_replica_blob_change(
                    &actor_id,
                    &req.path,
                    Some(&req.base),
                    &hash_bytes,
                    sealed.len() as i64,
                )
                .await
                .map_err(internal)?;
            if !stored {
                return Err(conflict());
            }
            encode_reply(&PutMlsReplicaReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// ── Registration entry point ────────────────────────────────────

pub fn register_mls_replica_handlers(b: &mut RpcRouterBuilder) {
    b.add(
        KIND_GET,
        RpcKindMeta {
            // Pure read of the calling actor's replica blob for a path.
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: mls_get_handler(),
        },
    );
    b.add(
        KIND_PUT,
        RpcKindMeta {
            // Content-addressed + CAS-gated: an equal-bytes replay is
            // idempotent, a stale replay conflicts. Safe either way.
            forbid_replay: false,
            default_deadline: Duration::from_secs(10),
            handler: mls_put_handler(),
        },
    );
}

#[cfg(test)]
use bytes::Bytes;
#[cfg(test)]
use fauna_protocol::encode_canonical;

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_protocol::mls_replica::ReplicaBase;

    async fn fixture_state_with_backup() -> (Arc<AppState>, tempfile::TempDir) {
        crate::test_support::fixture_state_with_backup().await
    }

    fn put_req(path: &str, blob: &[u8], base: ReplicaBase) -> Bytes {
        Bytes::from(
            encode_canonical(&PutMlsReplicaRequest {
                path: path.to_string(),
                blob: serde_bytes::ByteBuf::from(blob.to_vec()),
                base,
                extra: Default::default(),
            })
            .unwrap()
            .to_vec(),
        )
    }

    fn get_req(path: &str) -> Bytes {
        Bytes::from(
            encode_canonical(&GetMlsReplicaRequest {
                path: path.to_string(),
                hash_only: false,
                extra: Default::default(),
            })
            .unwrap()
            .to_vec(),
        )
    }

    /// The `hash_only` probe: a running device's once-per-sweep "did a
    /// sibling write?" question must cost a manifest row, never the blob.
    /// Pins the three shapes — the probe answers with the stored blob's
    /// `blake3` and no bytes, a full read carries the same hash beside the
    /// bytes, and an empty path answers neither.
    #[tokio::test]
    async fn a_hash_only_probe_answers_with_the_digest_and_no_blob() {
        let (state, _tmp) = fixture_state_with_backup().await;
        let user = [12u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();

        let probe = |path: &str| {
            Bytes::from(
                encode_canonical(&GetMlsReplicaRequest {
                    path: path.to_string(),
                    hash_only: true,
                    extra: Default::default(),
                })
                .unwrap()
                .to_vec(),
            )
        };
        let empty = decode::<GetMlsReplicaReply>(
            &mls_get_handler()(state.clone(), user, probe("provider"))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!((empty.blob, empty.hash), (None, None), "nothing stored yet");

        let blob = [0x01u8, 0x02, 0x03];
        put_blob(&state, user, "provider", &blob, Some(ReplicaBase::Absent))
            .await
            .unwrap();
        let want = serde_bytes::ByteBuf::from(blake3::hash(&blob).as_bytes().to_vec());

        let probed = decode::<GetMlsReplicaReply>(
            &mls_get_handler()(state.clone(), user, probe("provider"))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(probed.blob, None, "a probe carries no bytes");
        assert_eq!(
            probed.hash,
            Some(want.clone()),
            "…only the stored blob's digest"
        );

        let full = decode::<GetMlsReplicaReply>(
            &mls_get_handler()(state.clone(), user, get_req("provider"))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(full.blob.map(|b| b.into_vec()), Some(blob.to_vec()));
        assert_eq!(full.hash, Some(want), "a full read carries the same digest");
    }

    async fn get_blob(state: &Arc<AppState>, actor: [u8; 32], path: &str) -> Option<Vec<u8>> {
        let bytes = mls_get_handler()(state.clone(), actor, get_req(path))
            .await
            .expect("get ok");
        decode::<GetMlsReplicaReply>(&bytes)
            .unwrap()
            .blob
            .map(|b| b.into_vec())
    }

    async fn put_blob(
        state: &Arc<AppState>,
        actor: [u8; 32],
        path: &str,
        blob: &[u8],
        base: Option<ReplicaBase>,
    ) -> Result<(), RpcError> {
        // `None` plants: CAS against whatever is stored now (the base-less
        // blind put left the wire with the compat-remnant sweep).
        let base = match base {
            Some(b) => b,
            None => match get_blob(state, actor, path).await {
                Some(cur) => ReplicaBase::Hash(*blake3::hash(&cur).as_bytes()),
                None => ReplicaBase::Absent,
            },
        };
        mls_put_handler()(state.clone(), actor, put_req(path, blob, base))
            .await
            .map(|_| ())
    }

    /// Finding, the `__mls` member of the class: a reserved rail's
    /// superseded blobs must stop pinning themselves against GC. This rail is
    /// the highest-cadence of the three — the `provider` snapshot is rewritten
    /// on every epoch change — so an uncollapsed history pins a blob per epoch,
    /// permanently. Per-`path_hash`, so `provider` and each `history/<channel>`
    /// slice collapse independently.
    #[tokio::test]
    async fn a_rewritten_rail_pins_only_its_head_against_gc() {
        let (state, _tmp) = fixture_state_with_backup().await;
        let user = [11u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();

        for i in 1..=4u8 {
            put_blob(&state, user, "provider", &[0x01, i, i], None)
                .await
                .unwrap();
        }
        put_blob(&state, user, "history/ab", &[0x01, 9], None)
            .await
            .unwrap();

        let pinned = state
            .db
            .sync_change_manifest_hashes(crate::db::now_epoch_millis() + 1)
            .await
            .unwrap();
        assert_eq!(
            pinned.len(),
            2,
            "one live head per rail path (provider + history/ab); {} pinned",
            pinned.len()
        );
        assert_eq!(
            get_blob(&state, user, "provider").await.as_deref(),
            Some(&[0x01, 4, 4][..])
        );
        assert_eq!(
            get_blob(&state, user, "history/ab").await.as_deref(),
            Some(&[0x01, 9][..])
        );
    }

    #[tokio::test]
    async fn put_then_get_round_trips_opaque_bytes() {
        let (state, _tmp) = fixture_state_with_backup().await;
        let user = [5u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        // First byte 0x01 == the ChaCha20 version byte a real sealed blob
        // carries — proves raw storage doesn't mis-decode it as zstd.
        let sealed = {
            let mut v = vec![0x01u8];
            v.extend_from_slice(&[0xCD; 300]);
            v
        };
        put_blob(&state, user, "provider", &sealed, Some(ReplicaBase::Absent))
            .await
            .unwrap();
        assert_eq!(
            get_blob(&state, user, "provider").await.as_deref(),
            Some(sealed.as_slice())
        );
    }

    #[tokio::test]
    async fn get_on_empty_returns_none() {
        let (state, _tmp) = fixture_state_with_backup().await;
        state
            .db
            .create_user(&[9u8; 32], "free", "test")
            .await
            .unwrap();
        assert_eq!(get_blob(&state, [9u8; 32], "provider").await, None);
    }

    /// The day-one CAS shape (unlike `__drafts`): a first write asserts
    /// `Absent`; a concurrent second `Absent` write conflicts instead of
    /// clobbering — the concurrent-first-write race two fresh devices hit.
    #[tokio::test]
    async fn cas_absent_stores_once_then_conflicts() {
        let (state, _tmp) = fixture_state_with_backup().await;
        let user = [5u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        put_blob(
            &state,
            user,
            "provider",
            &[0x01, 1],
            Some(ReplicaBase::Absent),
        )
        .await
        .unwrap();
        let err = put_blob(
            &state,
            user,
            "provider",
            &[0x01, 2],
            Some(ReplicaBase::Absent),
        )
        .await
        .expect_err("stale Absent must conflict");
        assert_eq!(err.code, "fauna.mls.conflict");
        // The first write stands.
        assert_eq!(
            get_blob(&state, user, "provider").await.as_deref(),
            Some([0x01, 1].as_slice())
        );
    }

    #[tokio::test]
    async fn cas_hash_match_stores_and_stale_hash_conflicts() {
        let (state, _tmp) = fixture_state_with_backup().await;
        let user = [5u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        let v1 = [0x01u8, 1];
        put_blob(&state, user, "provider", &v1, Some(ReplicaBase::Absent))
            .await
            .unwrap();
        let base1: [u8; 32] = *blake3::hash(&v1).as_bytes();

        let v2 = [0x01u8, 2];
        put_blob(
            &state,
            user,
            "provider",
            &v2,
            Some(ReplicaBase::Hash(base1)),
        )
        .await
        .unwrap();
        assert_eq!(
            get_blob(&state, user, "provider").await.as_deref(),
            Some(v2.as_slice())
        );

        // A third writer still holding base1 must conflict.
        let err = put_blob(
            &state,
            user,
            "provider",
            &[0x01, 3],
            Some(ReplicaBase::Hash(base1)),
        )
        .await
        .expect_err("stale hash must conflict");
        assert_eq!(err.code, "fauna.mls.conflict");
    }

    /// A base-less put — the pre-CAS blind overwrite, which left the wire with
    /// the compat-remnant sweep — is malformed, never a last-writer-wins store.
    #[tokio::test]
    async fn a_base_less_put_is_malformed() {
        let (state, _tmp) = fixture_state_with_backup().await;
        let user = [5u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        #[derive(serde::Serialize)]
        struct LegacyPut {
            path: String,
            blob: serde_bytes::ByteBuf,
        }
        let legacy = Bytes::from(
            encode_canonical(&LegacyPut {
                path: "provider".into(),
                blob: serde_bytes::ByteBuf::from(vec![0x01, 1]),
            })
            .unwrap()
            .to_vec(),
        );
        let err = mls_put_handler()(state.clone(), user, legacy)
            .await
            .expect_err("a base-less put is refused");
        assert_eq!(err.code, "fauna.protocol.malformed");
        assert_eq!(get_blob(&state, user, "provider").await, None);
    }

    /// `provider` and per-channel `history/*` paths are independent blobs, and
    /// the CAS scopes per path (a write to one never conflicts the other).
    #[tokio::test]
    async fn paths_are_independent_and_cas_scopes_per_path() {
        let (state, _tmp) = fixture_state_with_backup().await;
        let user = [5u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        put_blob(
            &state,
            user,
            "provider",
            &[0x01, 0xAA],
            Some(ReplicaBase::Absent),
        )
        .await
        .unwrap();
        put_blob(
            &state,
            user,
            "history/aa11",
            &[0x01, 0xBB],
            Some(ReplicaBase::Absent),
        )
        .await
        .expect("Absent on a different path must not conflict");
        assert_eq!(
            get_blob(&state, user, "provider").await.as_deref(),
            Some([0x01, 0xAA].as_slice())
        );
        assert_eq!(
            get_blob(&state, user, "history/aa11").await.as_deref(),
            Some([0x01, 0xBB].as_slice())
        );
        assert_eq!(get_blob(&state, user, "history/bb22").await, None);
    }

    #[tokio::test]
    async fn replicas_are_per_actor_isolated() {
        let (state, _tmp) = fixture_state_with_backup().await;
        state
            .db
            .create_user(&[1u8; 32], "free", "test")
            .await
            .unwrap();
        state
            .db
            .create_user(&[2u8; 32], "free", "test")
            .await
            .unwrap();
        put_blob(&state, [1u8; 32], "provider", &[0x01, 0xAA], None)
            .await
            .unwrap();
        assert_eq!(get_blob(&state, [2u8; 32], "provider").await, None);
        assert_eq!(
            get_blob(&state, [1u8; 32], "provider").await.as_deref(),
            Some([0x01, 0xAA].as_slice())
        );
    }

    #[tokio::test]
    async fn put_rejects_empty_blob_and_empty_path() {
        let (state, _tmp) = fixture_state_with_backup().await;
        state
            .db
            .create_user(&[5u8; 32], "free", "test")
            .await
            .unwrap();
        let err = mls_put_handler()(
            state.clone(),
            [5u8; 32],
            put_req("provider", &[], ReplicaBase::Absent),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
        let err = mls_put_handler()(
            state.clone(),
            [5u8; 32],
            put_req("", &[0x01], ReplicaBase::Absent),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    /// A blob over `MAX_MLS_REPLICA_BYTES` is rejected with the clean
    /// `fauna.mls.too_large` domain error (defense-in-depth vs. a raw frame drop);
    /// a blob exactly at the cap is accepted.
    #[tokio::test]
    async fn put_rejects_oversize_blob_and_accepts_at_cap() {
        let (state, _tmp) = fixture_state_with_backup().await;
        let user = [5u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();

        let over = vec![0x01u8; MAX_MLS_REPLICA_BYTES + 1];
        let err = mls_put_handler()(
            state.clone(),
            user,
            put_req("provider", &over, ReplicaBase::Absent),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "fauna.mls.too_large");
        // The over-size put stored nothing.
        assert_eq!(get_blob(&state, user, "provider").await, None);

        // Exactly at the cap is accepted.
        let at_cap = vec![0x01u8; MAX_MLS_REPLICA_BYTES];
        put_blob(&state, user, "provider", &at_cap, Some(ReplicaBase::Absent))
            .await
            .expect("at-cap blob must be accepted");
    }
}
