//! WS-RPC handlers for the user-facing `fauna.drafts.{get,put}` surface — the
//! client-side `DraftStore` persistence plane (`docs/goal/behavior/file-sync.md`
//! § Drafts Sync).
//!
//! A Fauna app seals its `DraftStore` snapshot under its own `BackupKey`
//! (`libs/fauna-conversations`) and persists the **opaque** blob here, addressed
//! by a rail `path` (`"conversations"`, `"posts"`, `"events"`). The nest never
//! holds that key, so it stores/returns the bytes opaque — no nest path reads
//! drafts in either storage mode. See [`crate::db::drafts`] for the storage half.
//!
//! Caller-class enforcement lives in `bridge_method_allowlist::is_permitted`
//! (`User | Admin` here). The owning actor is the authenticated connection actor
//! — neither kind carries an `actor_id`, so a caller reads/writes only their own
//! drafts (self-scoped, no escalation surface).
//!
//! Storage is **byte-exact opaque**: the blob is stored *raw* (no `encode_blob`
//! wrapping) under its own content hash, with a `sync_changes` row in the
//! `__drafts` folder, and `get` returns exactly those bytes. The client
//! already did the full seal (`bare → zstd → ChaCha20`), so the nest must not
//! re-wrap (the ChaCha20 version byte `0x01` collides with the zstd prefix and
//! `decode_blob` would fail).

use std::time::Duration;

use fauna_protocol::{
    RpcError, decode_strict as decode,
    drafts::{
        GetDraftsReply, GetDraftsRequest, KIND_GET, KIND_PUT, MAX_DRAFTS_BLOB_BYTES,
        PutDraftsReply, PutDraftsRequest,
    },
};

use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

#[cfg(test)]
use crate::routes::AppState;
#[cfg(test)]
use std::sync::Arc;

// ── Helpers ─────────────────────────

use crate::rpc_errors::{encode_reply, internal, malformed};

/// Blob storage isn't configured yet (no `BackupService`) — the nest hasn't
/// finished claim/onboarding. The client surfaces this as a transient error.
fn unavailable(reason: &str) -> RpcError {
    crate::rpc_errors::unavailable_ns("drafts", reason)
}

use crate::bridge_method_allowlist::require_permission_default as require_permission;

/// A draft `path` must be one of the ratified rail keys
/// ([`fauna_protocol::drafts::DRAFT_RAILS`]).
///
/// The enumeration is load-bearing, not cosmetic. This plane bounds an actor's
/// `__drafts` footprint by construction — a fixed path count
/// times [`MAX_DRAFTS_BLOB_BYTES`] — rather than by metering it against the
/// storage quota (see [`crate::db::drafts`] for why the reserved-rail shape
/// takes the count bound instead). A client-chosen key made the count
/// unbounded, and every distinct key is a latest-per-path live row GC pins
/// forever. It is also what lets the plaintext `path` column rest as the frozen
/// machine-authored class `docs/goal/behavior/path-sealing.md` § Deliberate
/// non-seals describes.
fn validate_path(path: &str) -> Result<(), RpcError> {
    if path.is_empty() {
        return Err(malformed("drafts path must not be empty"));
    }
    if !fauna_protocol::drafts::is_ratified_rail(path) {
        return Err(malformed(format!(
            "drafts path {path:?} is not a ratified rail (expected one of {:?})",
            fauna_protocol::drafts::DRAFT_RAILS
        )));
    }
    Ok(())
}

// ── fauna.drafts.get ────────────────────────────────────────────

fn drafts_get_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, KIND_GET).await?;
            let req: GetDraftsRequest = decode(&payload).map_err(malformed)?;
            validate_path(&req.path)?;
            let backup_svc = state
                .backup_service
                .as_ref()
                .ok_or_else(|| unavailable("blob storage not configured"))?;
            let store = backup_svc.local_blob_store();
            let blob = state
                .db
                .get_drafts_blob(&actor_id, &req.path, &store)
                .await
                .map_err(internal)?;
            encode_reply(&GetDraftsReply {
                blob: blob.map(serde_bytes::ByteBuf::from),
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.drafts.put ────────────────────────────────────────────

fn drafts_put_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, KIND_PUT).await?;
            let req: PutDraftsRequest = decode(&payload).map_err(malformed)?;
            validate_path(&req.path)?;
            if req.blob.is_empty() {
                return Err(malformed("drafts blob must not be empty"));
            }
            if req.blob.len() > MAX_DRAFTS_BLOB_BYTES {
                return Err(malformed(format!(
                    "drafts blob {} bytes exceeds {MAX_DRAFTS_BLOB_BYTES}-byte cap",
                    req.blob.len()
                )));
            }
            let backup_svc = state
                .backup_service
                .as_ref()
                .ok_or_else(|| unavailable("blob storage not configured"))?;

            // Store the already-sealed blob RAW (no encode_blob wrapping) so
            // `get` returns the exact bytes the client must unseal. Content-
            // address on the stored bytes themselves.
            let sealed = req.blob.as_ref();
            let hash_bytes: [u8; 32] = *blake3::hash(sealed).as_bytes();
            let hash = fauna_core::data::ContentHash::from_digest_raw(hash_bytes);
            backup_svc
                .local_blob_store()
                .put(&hash, sealed)
                .await
                .map_err(internal)?;
            state
                .db
                .record_drafts_blob_change(&actor_id, &req.path, &hash_bytes, sealed.len() as i64)
                .await
                .map_err(internal)?;
            encode_reply(&PutDraftsReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// ── Registration entry point ────────────────────────────────────

pub fn register_drafts_handlers(b: &mut RpcRouterBuilder) {
    b.add(
        KIND_GET,
        RpcKindMeta {
            // Pure read of the calling actor's drafts blob for a path.
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: drafts_get_handler(),
        },
    );
    b.add(
        KIND_PUT,
        RpcKindMeta {
            // Idempotent overwrite — content-addressed blob, same bytes →
            // same state, so a replay is harmless.
            forbid_replay: false,
            default_deadline: Duration::from_secs(10),
            handler: drafts_put_handler(),
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
    use fauna_protocol::drafts::{
        DRAFT_RAILS, GetDraftsReply, MAX_DRAFTS_BLOB_BYTES, PutDraftsRequest,
    };

    /// A real `AppState` wired to a real `BackupService` over a `DiskBlobStore`
    /// in a tempdir (the `for_test` AppState alone has `backup_service: None`).
    async fn fixture_state_with_backup() -> (Arc<AppState>, tempfile::TempDir) {
        crate::test_support::fixture_state_with_backup().await
    }

    fn put_req(path: &str, blob: &[u8]) -> Bytes {
        Bytes::from(
            encode_canonical(&PutDraftsRequest {
                path: path.to_string(),
                blob: serde_bytes::ByteBuf::from(blob.to_vec()),
                extra: Default::default(),
            })
            .unwrap()
            .to_vec(),
        )
    }

    fn get_req(path: &str) -> Bytes {
        Bytes::from(
            encode_canonical(&GetDraftsRequest {
                path: path.to_string(),
                extra: Default::default(),
            })
            .unwrap()
            .to_vec(),
        )
    }

    async fn get_blob(state: &Arc<AppState>, actor: [u8; 32], path: &str) -> Option<Vec<u8>> {
        let bytes = drafts_get_handler()(state.clone(), actor, get_req(path))
            .await
            .expect("get ok");
        decode::<GetDraftsReply>(&bytes)
            .unwrap()
            .blob
            .map(|b| b.into_vec())
    }

    async fn put_blob(state: &Arc<AppState>, actor: [u8; 32], path: &str, blob: &[u8]) {
        drafts_put_handler()(state.clone(), actor, put_req(path, blob))
            .await
            .unwrap_or_else(|e| panic!("put failed: {}", e.code));
    }

    #[tokio::test]
    async fn put_then_get_round_trips_opaque_bytes() {
        let (state, _tmp) = fixture_state_with_backup().await;
        let user = [5u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        // First byte 0x01 == the ChaCha20 version byte a real sealed draft
        // carries — proves raw storage doesn't mis-`decode_blob` it as zstd.
        let sealed = {
            let mut v = vec![0x01u8];
            v.extend_from_slice(&[0xAB; 200]);
            v
        };
        put_blob(&state, user, "conversations", &sealed).await;
        assert_eq!(
            get_blob(&state, user, "conversations").await.as_deref(),
            Some(sealed.as_slice())
        );
    }

    /// Finding: a rail's *superseded* blobs must stop pinning
    /// themselves against GC. Every `put` registers a fresh blob and appends a
    /// `sync_changes` row, and nothing else in production ever marks a reserved
    /// rail's rows superseded — `superseded_at` has exactly one other writer,
    /// the owner-driven M2 `fauna.sync.changes.supersede` RPC — so without the
    /// write-time collapse the GC reference walk pins **every** draft the user
    /// ever auto-saved, permanently. Not for one GC cycle: forever.
    #[tokio::test]
    async fn a_rewritten_rail_pins_only_its_head_against_gc() {
        let (state, _tmp) = fixture_state_with_backup().await;
        let user = [7u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();

        for i in 1..=4u8 {
            put_blob(&state, user, "conversations", &[0x01, i, i, i]).await;
        }

        // The production GC cutoff (30-minute grace). Rows superseded within
        // the window still pin — these were superseded now, so this asserts the
        // steady state a cycle later, which is what the 3 MiB claim is about.
        let pinned = state
            .db
            .sync_change_manifest_hashes(crate::db::now_epoch_millis() + 1)
            .await
            .unwrap();
        assert_eq!(
            pinned.len(),
            1,
            "each put collapses its predecessor, so only the head blob stays \
             pinned; {} blobs are still pinned",
            pinned.len()
        );

        // The collapse must not cost the reader its current state.
        assert_eq!(
            get_blob(&state, user, "conversations").await.as_deref(),
            Some(&[0x01, 4, 4, 4][..])
        );
    }

    /// The collapse is scoped to the rail it wrote — `path_hash`, not the whole
    /// `__drafts` set. A shared-set collapse would let a `posts` save silently
    /// unpin the live `conversations` blob.
    #[tokio::test]
    async fn the_collapse_is_per_rail_not_per_set() {
        let (state, _tmp) = fixture_state_with_backup().await;
        let user = [8u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();

        put_blob(&state, user, "conversations", &[0x01, 1]).await;
        put_blob(&state, user, "conversations", &[0x01, 2]).await;
        put_blob(&state, user, "posts", &[0x01, 3]).await;

        let pinned = state
            .db
            .sync_change_manifest_hashes(crate::db::now_epoch_millis() + 1)
            .await
            .unwrap();
        assert_eq!(pinned.len(), 2, "one live head per rail, not one per set");
        assert_eq!(
            get_blob(&state, user, "conversations").await.as_deref(),
            Some(&[0x01, 2][..])
        );
        assert_eq!(
            get_blob(&state, user, "posts").await.as_deref(),
            Some(&[0x01, 3][..])
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
        assert_eq!(get_blob(&state, [9u8; 32], "conversations").await, None);
    }

    #[tokio::test]
    async fn put_rejects_empty_blob() {
        let (state, _tmp) = fixture_state_with_backup().await;
        state
            .db
            .create_user(&[5u8; 32], "free", "test")
            .await
            .unwrap();
        let err = drafts_put_handler()(state.clone(), [5u8; 32], put_req("conversations", &[]))
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn put_rejects_empty_path() {
        let (state, _tmp) = fixture_state_with_backup().await;
        state
            .db
            .create_user(&[5u8; 32], "free", "test")
            .await
            .unwrap();
        let err = drafts_put_handler()(state.clone(), [5u8; 32], put_req("", &[0x01, 1]))
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[tokio::test]
    async fn put_overwrites_prior_drafts() {
        let (state, _tmp) = fixture_state_with_backup().await;
        let user = [5u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        put_blob(&state, user, "conversations", &[0x01, 1, 2, 3]).await;
        put_blob(&state, user, "conversations", &[0x01, 9, 9, 9, 9]).await;
        assert_eq!(
            get_blob(&state, user, "conversations").await.as_deref(),
            Some([0x01, 9, 9, 9, 9].as_slice())
        );
    }

    /// Two rails (paths) for the same actor are independent blobs — writing one
    /// must not clobber or shadow the other (the rail-agnostic property).
    #[tokio::test]
    async fn paths_are_independent() {
        let (state, _tmp) = fixture_state_with_backup().await;
        let user = [5u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        put_blob(&state, user, "conversations", &[0x01, 0xAA]).await;
        put_blob(&state, user, "posts", &[0x01, 0xBB]).await;
        assert_eq!(
            get_blob(&state, user, "conversations").await.as_deref(),
            Some([0x01, 0xAA].as_slice())
        );
        assert_eq!(
            get_blob(&state, user, "posts").await.as_deref(),
            Some([0x01, 0xBB].as_slice())
        );
        // A path never written returns None even when others exist.
        assert_eq!(get_blob(&state, user, "events").await, None);
    }

    /// The blob cap (item 1): one byte over is refused, and — the half that
    /// keeps the cap from being a silent downgrade — a blob exactly at the cap
    /// is still accepted and reads back byte-exact.
    #[tokio::test]
    async fn put_refuses_a_blob_over_the_cap_and_accepts_one_at_it() {
        let (state, _tmp) = fixture_state_with_backup().await;
        let user = [5u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();

        let mut too_big = vec![0x01u8; MAX_DRAFTS_BLOB_BYTES + 1];
        too_big[0] = 0x01;
        let err = drafts_put_handler()(state.clone(), user, put_req("conversations", &too_big))
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.protocol.malformed");
        assert_eq!(
            get_blob(&state, user, "conversations").await,
            None,
            "an over-cap put must not have written anything"
        );

        let at_cap = vec![0x01u8; MAX_DRAFTS_BLOB_BYTES];
        put_blob(&state, user, "conversations", &at_cap).await;
        assert_eq!(
            get_blob(&state, user, "conversations").await.as_deref(),
            Some(at_cap.as_slice()),
            "a blob exactly at the cap is legitimate and must round-trip"
        );
    }

    /// The rail enumeration (item 3): every ratified rail is accepted…
    #[tokio::test]
    async fn put_accepts_every_ratified_rail() {
        let (state, _tmp) = fixture_state_with_backup().await;
        let user = [5u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();
        for (i, rail) in DRAFT_RAILS.iter().enumerate() {
            let blob = [0x01u8, i as u8];
            put_blob(&state, user, rail, &blob).await;
            assert_eq!(
                get_blob(&state, user, rail).await.as_deref(),
                Some(blob.as_slice()),
                "ratified rail {rail} must round-trip"
            );
        }
    }

    /// …and anything outside it is refused on BOTH kinds, so an actor cannot
    /// mint unbounded live rows (each of which GC pins forever) by inventing
    /// path keys. `get` is gated too: the enumeration is the plane's vocabulary,
    /// not just a write guard.
    #[tokio::test]
    async fn both_kinds_refuse_a_rail_outside_the_enumeration() {
        let (state, _tmp) = fixture_state_with_backup().await;
        let user = [5u8; 32];
        state.db.create_user(&user, "free", "test").await.unwrap();

        // Includes the sub-path shape the goal doc once left to `DraftStore`:
        // it is not a ratified rail key, so it is refused until ratified.
        for bogus in [
            "conversations/42",
            "Conversations",
            "conversations ",
            "notarail",
            "../../etc/passwd",
        ] {
            let err = drafts_put_handler()(state.clone(), user, put_req(bogus, &[0x01, 7]))
                .await
                .unwrap_err();
            assert_eq!(
                err.code, "fauna.protocol.malformed",
                "put must refuse unratified rail {bogus:?}"
            );
            let err = drafts_get_handler()(state.clone(), user, get_req(bogus))
                .await
                .unwrap_err();
            assert_eq!(
                err.code, "fauna.protocol.malformed",
                "get must refuse unratified rail {bogus:?}"
            );
        }
    }

    #[tokio::test]
    async fn drafts_are_per_actor_isolated() {
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
        put_blob(&state, [1u8; 32], "conversations", &[0x01, 0xAA]).await;
        // A different actor sees no drafts of its own.
        assert_eq!(get_blob(&state, [2u8; 32], "conversations").await, None);
        // The owner still sees its own.
        assert_eq!(
            get_blob(&state, [1u8; 32], "conversations")
                .await
                .as_deref(),
            Some([0x01, 0xAA].as_slice())
        );
    }

    /// The cross-device catch-up shape: device A writes, a *second* read for the
    /// same actor (a "second device") sees A's latest. Two distinct actors on one
    /// nest each keep their own `__drafts` (the per-actor reserved-set property —
    /// proving `__drafts` has the `(name, actor_id)` correctness the other
    /// reserved rails have).
    #[tokio::test]
    async fn two_actors_each_keep_own_drafts() {
        let (state, _tmp) = fixture_state_with_backup().await;
        let alice = [1u8; 32];
        let bob = [2u8; 32];
        state.db.create_user(&alice, "free", "test").await.unwrap();
        state.db.create_user(&bob, "free", "test").await.unwrap();

        put_blob(&state, alice, "conversations", &[0x01, 0xAA]).await;
        put_blob(&state, bob, "conversations", &[0x01, 0xBB]).await;

        assert_eq!(
            get_blob(&state, alice, "conversations").await.as_deref(),
            Some([0x01, 0xAA].as_slice()),
            "alice's drafts must survive bob's write"
        );
        assert_eq!(
            get_blob(&state, bob, "conversations").await.as_deref(),
            Some([0x01, 0xBB].as_slice()),
            "bob must have his own drafts row"
        );
    }
}
