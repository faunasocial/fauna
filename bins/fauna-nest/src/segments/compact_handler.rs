//! `fauna.segments.compact` WS-RPC handler — manual compaction trigger
//! for an optional `(kind, actor_id)` scope. Drops the periodic worker's
//! 25% tombstone-fraction gate to 0%; runs the same compaction code
//! path under the same advisory lock.
//!
//! Authorization (same as the retired `POST /api/v1/segments/compact`):
//! - Scoped mail / no-kind (`actor_id = Some`): bearer must equal
//!   `actor_id` (owner) or be a nest admin.
//! - Scoped conv (`kind = "conv"`, `actor_id = Some(channel)`): the scope
//!   is a `channel_id`, not an actor, so authorize on channel membership —
//!   `bearer ∈ list_channel_actors(channel)` (consistent with conv
//!   ingest/reads) or nest admin.
//! - Whole-nest (`actor_id = None`): admin-only.
//!
//! Pure-backup destinations are refused with
//! `fauna.segments.pure_backup_destination` (they hold opaque chunks,
//! not local plaintext-framed segments the compaction engine can
//! process). The whole-nest variant skips this gate at the route level;
//! the compaction worker threads the same predicate through its per-actor
//! loop (`compaction.rs::resolve_actors`, Plan 4 T4).
//!
//! See `docs/goal/architecture/message-segment-store.md` § Manual
//! compact endpoint and `docs/goal/behavior/backup-restore.md` § Manual
//! compact (on demand).

use std::time::Duration;

use bytes::Bytes;

use fauna_protocol::segments::{CompactReply, CompactRequest, CompactScopeEcho};
use fauna_protocol::{RpcError, Value, decode_strict as decode, encode_canonical};

use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};
use crate::segments::compaction::{CompactScope, CompactionTrigger, CompactionWorker};

pub fn register_compact_handler(b: &mut RpcRouterBuilder) {
    b.add(
        "fauna.segments.compact",
        RpcKindMeta {
            forbid_replay: false,
            // Same as `fauna.subscriptions.tiers.create` / `subscribe`:
            // compaction can take seconds on large mail spools.
            default_deadline: Duration::from_secs(30),
            handler: compact_handler(),
        },
    );
}

fn malformed(reason: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::malformed_ns("segments", reason)
}

fn permission_denied(reason: &str) -> RpcError {
    crate::rpc_errors::permission_denied_ns("segments", reason)
}

fn pure_backup_destination() -> RpcError {
    crate::rpc_errors::pure_backup_destination_ns(
        "segments",
        "compaction requires local plaintext-framed segments; this destination holds opaque chunks only",
    )
}

fn lock_contention() -> RpcError {
    let mut e = RpcError::new(
        "fauna.segments.lock_contention",
        "error.segments.lock_contention",
    );
    e.details = Some(Box::new(Value::String(
        "snapshot GC or another compaction is in progress".into(),
    )));
    e
}

fn internal(reason: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::internal_ns("segments", reason)
}

pub fn compact_handler() -> RpcHandler {
    Box::new(|state, bearer, payload| {
        Box::pin(async move {
            let req: CompactRequest = decode(&payload).map_err(malformed)?;

            // Authorization. The scope key (`actor_id`) means different things
            // per kind, so the membership test differs:
            // - conv (`kind="conv"`): the scope is the `channel_id`, not an
            //   actor. Owner-equality (`bearer == channel`) is never true, so a
            //   conv request authorizes on channel membership — `bearer ∈
            //   list_channel_actors(channel)` (consistent with conv
            //   ingest/reads) OR nest admin.
            // - mail / post / no-kind (`kind=None`): the scope is the bearer's
            //   own actor (a post's scope is its author), so owner-equality
            //   (`bearer == actor`) OR nest admin.
            // - whole-nest (`actor_id=None`): admin-only.
            let actor_id_bytes: Option<[u8; 32]> = req.actor_id.as_ref().map(|a| a.0);
            if let Some(a) = &actor_id_bytes {
                let is_admin = state.db.is_admin(&bearer).await.map_err(internal)?;
                let authorized = if req.kind.as_deref() == Some("conv") {
                    is_admin
                        || state
                            .db
                            .list_channel_actors(a)
                            .await
                            .map_err(internal)?
                            .contains(&bearer)
                } else {
                    is_admin || bearer == *a
                };
                if !authorized {
                    return Err(if req.kind.as_deref() == Some("conv") {
                        permission_denied("not a channel member or admin")
                    } else {
                        permission_denied("not owner or admin")
                    });
                }
            } else {
                let is_admin = state.db.is_admin(&bearer).await.map_err(internal)?;
                if !is_admin {
                    return Err(permission_denied("whole-nest compact requires admin"));
                }
            }

            // Reject pure-backup destinations on the scoped path. The whole-
            // nest variant skips this gate; the worker threads the predicate
            // through its per-actor loop.
            if let Some(a) = &actor_id_bytes {
                let pure_backup = state
                    .db
                    .is_pure_backup_destination(req.kind.as_deref().unwrap_or("mail"), a)
                    .await
                    .map_err(internal)?;
                if pure_backup {
                    return Err(pure_backup_destination());
                }
            }

            let scope = CompactScope {
                kind: req.kind.clone(),
                scope_id: actor_id_bytes,
            };
            let worker = CompactionWorker::for_manual(state.clone());
            let report = worker
                .run_once(scope.clone(), CompactionTrigger::Manual)
                .await
                .map_err(internal)?;

            // `acquired_lock = false` distinguishes lock contention (another
            // GC/compaction in flight) from "no work to do" (which returns
            // success with `segments_rewritten = 0`).
            if !report.acquired_lock {
                return Err(lock_contention());
            }

            let reply = CompactReply {
                segments_rewritten: report.segments_rewritten,
                errors: report.errors,
                scope: CompactScopeEcho {
                    kind: scope.kind,
                    actor_id: req.actor_id,
                    extra: std::collections::BTreeMap::new(),
                },
                extra: std::collections::BTreeMap::new(),
            };

            encode_canonical(&reply)
                .map(|v| Bytes::from(v.to_vec()))
                .map_err(|e| internal(format!("encode reply: {e}")))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CacheDb;
    use crate::routes::AppState;
    use crate::segments::test_helpers::floor;
    use crate::segments::{CalPlacementSegmentManager, MailPlacementSegmentManager};
    use fauna_core::identity::ActorId;
    use fauna_protocol::encode_canonical;
    use fauna_segment_store::SegmentManager;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn build_state() -> (TempDir, Arc<AppState>) {
        let tmp = TempDir::new().expect("tempdir");
        let db = Arc::new(CacheDb::open_in_memory().expect("in-memory CacheDb"));
        let mut state = AppState::for_test(db.clone());
        state.mail_segments = Arc::new(SegmentManager::new(tmp.path().to_path_buf(), "mail"));
        // Conv segments share the same tempdir (kind = "conv" → `__conv/...`
        // subtree); without this override `conv_segments` keeps its default
        // PID-shared dir, which collides across tests in the binary on a
        // reused channel id (mirrors `compaction.rs`'s `build_state`).
        state.conv_segments = Arc::new(SegmentManager::new(tmp.path().to_path_buf(), "conv"));
        // Post segments share the same tempdir (kind = "post" → `__post/...`
        // subtree); override the default PID-shared dir for the same
        // cross-test-isolation reason as mail/conv.
        state.post_segments = Arc::new(SegmentManager::new(tmp.path().to_path_buf(), "post"));
        state.mail_placement = Arc::new(MailPlacementSegmentManager::new(tmp.path().to_path_buf()));
        state.cal_placement = Arc::new(CalPlacementSegmentManager::new(tmp.path().to_path_buf()));
        (tmp, Arc::new(state))
    }

    async fn append_n(state: &Arc<AppState>, actor: &[u8; 32], n: usize) -> Vec<fauna_cbor::Cid> {
        let mut cids = Vec::with_capacity(n);
        for i in 0..n {
            // Unique body per record — identical bytes would dedup under
            // content-hash identity.
            let o = crate::segments::mail::append_record(
                &state.mail_segments,
                &state.db,
                actor,
                &fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(
                    format!("sealed-body-{i}").into_bytes(),
                ),
                &fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(
                    b"sealed-hint".to_vec(),
                ),
                floor(1_712_000_000_000),
            )
            .await
            .expect("append");
            cids.push(o.cid);
        }
        cids
    }

    /// Append `n` raw post bodies to `author`'s `__post` segment store,
    /// returning their record CIDs in order. Posts carry no envelope, so the
    /// block IS the body and `record_cid = Cid::of_dag_cbor(body)`.
    async fn append_n_posts(
        state: &Arc<AppState>,
        author: &[u8; 32],
        n: usize,
    ) -> Vec<fauna_cbor::Cid> {
        let mut cids = Vec::with_capacity(n);
        for i in 0..n {
            let body = format!("post-body-{i}").into_bytes();
            let o = crate::segments::post::append_body(
                &state.post_segments,
                &state.db,
                author,
                &body,
                1_712_000_000_000,
            )
            .await
            .expect("append post");
            cids.push(o.record_cid);
        }
        cids
    }

    fn encode_req<T: serde::Serialize>(req: &T) -> Bytes {
        Bytes::from(encode_canonical(req).expect("encode req").to_vec())
    }

    fn decode_reply<T: serde::de::DeserializeOwned>(b: &Bytes) -> T {
        fauna_cbor::decode_strict(b).expect("decode reply")
    }

    /// Happy path: owner posts a scoped compact request; one segment is
    /// finalised and has some tombstoned rows; the handler rewrites it
    /// and returns a reply with `segments_rewritten >= 1`.
    #[tokio::test]
    async fn happy_path_owner_scoped_compact() {
        let (_tmp, state) = build_state();
        let actor = [0xaau8; 32];

        let cids = append_n(&state, &actor, 3).await;
        state
            .mail_segments
            .finalize_open(&actor)
            .await
            .expect("finalize");

        // The record's REAL minted identity — under content-hash filing a
        // synthetic id matches no row, so a tombstone on one marks nothing and
        // the compaction below would find nothing to rewrite.
        state
            .db
            .segment_records_mark_tombstoned(&actor, "mail", 1, &cids[0])
            .await
            .expect("tombstone");

        let req = CompactRequest {
            kind: Some("mail".into()),
            actor_id: Some(ActorId(actor)),
            extra: std::collections::BTreeMap::new(),
        };
        let bytes = compact_handler()(state, actor, encode_req(&req))
            .await
            .expect("compact ok");
        let reply: CompactReply = decode_reply(&bytes);
        assert!(
            reply.segments_rewritten >= 1,
            "expected at least one rewritten segment; got {reply:?}"
        );
        assert_eq!(reply.errors, 0);
        assert_eq!(reply.scope.kind.as_deref(), Some("mail"));
        assert_eq!(reply.scope.actor_id.as_ref().map(|a| a.0), Some(actor));
    }

    /// Non-owner, non-admin caller gets `permission_denied`.
    #[tokio::test]
    async fn forbidden_non_owner_non_admin() {
        let (_tmp, state) = build_state();
        let owner = [0xbbu8; 32];
        let other = [0xccu8; 32];

        let req = CompactRequest {
            kind: None,
            actor_id: Some(ActorId(owner)),
            extra: std::collections::BTreeMap::new(),
        };
        let err = compact_handler()(state, other, encode_req(&req))
            .await
            .expect_err("non-owner must be rejected");
        assert_eq!(err.code, "fauna.segments.permission_denied");
    }

    /// Whole-nest scope requires admin; non-admin caller gets
    /// `permission_denied`.
    #[tokio::test]
    async fn whole_nest_scope_requires_admin() {
        let (_tmp, state) = build_state();
        let non_admin = [0xffu8; 32];

        let req = CompactRequest {
            kind: None,
            actor_id: None,
            extra: std::collections::BTreeMap::new(),
        };
        let err = compact_handler()(state, non_admin, encode_req(&req))
            .await
            .expect_err("non-admin whole-nest must be rejected");
        assert_eq!(err.code, "fauna.segments.permission_denied");
    }

    /// Pure-backup destination gets `pure_backup_destination`.
    #[tokio::test]
    async fn rejects_pure_backup_destination() {
        let (_tmp, state) = build_state();
        let actor = [0x77u8; 32];
        state
            .db
            .create_folder_with_options(
                "__mail",
                &actor,
                crate::db::FolderOptions {
                    custody_copy: true,
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let req = CompactRequest {
            kind: None,
            actor_id: Some(ActorId(actor)),
            extra: std::collections::BTreeMap::new(),
        };
        let err = compact_handler()(state, actor, encode_req(&req))
            .await
            .expect_err("pure-backup destination must be rejected");
        assert_eq!(err.code, "fauna.segments.pure_backup_destination");
    }

    /// Owner posts a scoped `kind="post"` compact request; one finalised
    /// segment has a tombstoned row → the handler rewrites it. Proves the
    /// worker→`segments_for_kind`→`rewrite_bucket`→`post::compact_bucket`
    /// wiring (the `post::compact_bucket` unit test proves the core mechanics;
    /// this proves the manual-handler + owner-equality-auth path).
    #[tokio::test]
    async fn happy_path_owner_scoped_compact_post() {
        let (_tmp, state) = build_state();
        let author = [0x5au8; 32];

        let cids = append_n_posts(&state, &author, 3).await;
        state
            .post_segments
            .finalize_open(&author)
            .await
            .expect("finalize");

        // Tombstone the first post so the bucket has reclaimable garbage.
        state
            .db
            .segment_records_mark_tombstoned(&author, "post", 1, &cids[0])
            .await
            .expect("tombstone");

        let req = CompactRequest {
            kind: Some("post".into()),
            actor_id: Some(ActorId(author)),
            extra: std::collections::BTreeMap::new(),
        };
        let bytes = compact_handler()(state, author, encode_req(&req))
            .await
            .expect("compact ok");
        let reply: CompactReply = decode_reply(&bytes);
        assert!(
            reply.segments_rewritten >= 1,
            "expected at least one rewritten post segment; got {reply:?}"
        );
        assert_eq!(reply.errors, 0);
        assert_eq!(reply.scope.kind.as_deref(), Some("post"));
        assert_eq!(reply.scope.actor_id.as_ref().map(|a| a.0), Some(author));
    }

    /// A `__post` reserved custody-copy set (a pure-backup destination)
    /// refuses `kind="post"` compaction (Destination-capability gate 2):
    /// opaque chunks have no local plaintext-framed segments to rewrite.
    /// Proves the `is_pure_backup_destination` "post" arm + the route gate.
    #[tokio::test]
    async fn rejects_pure_backup_destination_post() {
        let (_tmp, state) = build_state();
        let author = [0x6au8; 32];
        state
            .db
            .create_folder_with_options(
                "__post",
                &author,
                crate::db::FolderOptions {
                    custody_copy: true,
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let req = CompactRequest {
            kind: Some("post".into()),
            actor_id: Some(ActorId(author)),
            extra: std::collections::BTreeMap::new(),
        };
        let err = compact_handler()(state, author, encode_req(&req))
            .await
            .expect_err("pure-backup post destination must be rejected");
        assert_eq!(err.code, "fauna.segments.pure_backup_destination");
    }

    /// ordinary actor passes the pure-backup gate; with no segments
    /// seeded, the handler returns success with `segments_rewritten = 0`.
    #[tokio::test]
    async fn proceeds_when_a_rail() {
        let (_tmp, state) = build_state();
        let actor = [0x88u8; 32];
        state
            .db
            .create_folder_with_options(
                "__mail",
                &actor,
                crate::db::FolderOptions {
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let req = CompactRequest {
            kind: None,
            actor_id: Some(ActorId(actor)),
            extra: std::collections::BTreeMap::new(),
        };
        let bytes = compact_handler()(state, actor, encode_req(&req))
            .await
            .expect("sync actor proceeds");
        let reply: CompactReply = decode_reply(&bytes);
        assert_eq!(reply.segments_rewritten, 0);
    }

    /// Admin can trigger whole-nest compaction.
    #[tokio::test]
    async fn admin_can_trigger_whole_nest_compact() {
        let (_tmp, state) = build_state();
        let admin = [0x01u8; 32];
        state.db.add_admin_actor(&admin).await.expect("add admin");

        let req = CompactRequest {
            kind: None,
            actor_id: None,
            extra: std::collections::BTreeMap::new(),
        };
        let bytes = compact_handler()(state, admin, encode_req(&req))
            .await
            .expect("admin whole-nest ok");
        let reply: CompactReply = decode_reply(&bytes);
        assert_eq!(reply.segments_rewritten, 0);
        assert_eq!(reply.errors, 0);
        assert!(reply.scope.kind.is_none());
        assert!(reply.scope.actor_id.is_none());
    }

    /// Malformed BARE payload gets `malformed`.
    #[tokio::test]
    async fn malformed_payload() {
        let (_tmp, state) = build_state();
        let actor = [0xddu8; 32];
        let err = compact_handler()(state, actor, Bytes::from_static(b"not-bare-cbor"))
            .await
            .expect_err("malformed payload must be rejected");
        assert_eq!(err.code, "fauna.segments.malformed");
    }

    // -----------------------------------------------------------------------
    // Conv-kind authorization (Plan 8 T3)
    //
    // For `kind="conv"` the scoped request's `actor_id` is a `channel_id`, so
    // the handler authorizes on channel membership (`bearer ∈
    // list_channel_actors(channel)`) or nest admin — not owner-equality. The
    // compaction itself flows through the worker (T2); these tests assert the
    // auth branch, with the happy-path test also exercising a real rewrite.
    // Distinct channel ids per test (for-test segment dirs are PID-shared).
    // -----------------------------------------------------------------------

    /// Append `n` conv records into `channel` at a fixed `received_at` (one
    /// bucket → one segment).
    async fn append_n_conv(state: &Arc<AppState>, channel: &[u8; 32], n: usize) {
        for i in 0..n {
            crate::segments::conv::append(
                &state.conv_segments,
                &state.db,
                channel,
                format!("conv-body-{i}").as_bytes(),
                1_712_000_000_000,
            )
            .await
            .expect("conv append");
        }
    }

    /// A channel member can compact their channel. Seed a tombstoned conv
    /// segment so the manual (0 %) threshold rewrites it; assert success
    /// (`segments_rewritten >= 1`) and no `permission_denied`.
    #[tokio::test]
    async fn conv_channel_member_can_compact() {
        let (_tmp, state) = build_state();
        let channel = [0x71u8; 32];
        let member = [0x72u8; 32];
        state
            .db
            .register_actor_channel(&member, &channel)
            .await
            .expect("register member");

        append_n_conv(&state, &channel, 4).await;
        state
            .conv_segments
            .finalize_open(&channel)
            .await
            .expect("flush");
        // Tombstone the two oldest so the closed bucket crosses the 0 %
        // manual threshold.
        let n = crate::segments::conv::tombstone_up_to_seq(&state.db, &[channel], 2)
            .await
            .expect("tombstone");
        assert_eq!(n, 2);

        let req = CompactRequest {
            kind: Some("conv".into()),
            actor_id: Some(ActorId(channel)),
            extra: std::collections::BTreeMap::new(),
        };
        let bytes = compact_handler()(state, member, encode_req(&req))
            .await
            .expect("member compact ok");
        let reply: CompactReply = decode_reply(&bytes);
        assert_eq!(reply.errors, 0);
        assert!(
            reply.segments_rewritten >= 1,
            "expected the tombstoned conv segment to be rewritten; got {reply:?}"
        );
        assert_eq!(reply.scope.kind.as_deref(), Some("conv"));
        assert_eq!(reply.scope.actor_id.as_ref().map(|a| a.0), Some(channel));
    }

    /// A non-member, non-admin caller posting a scoped conv compact request is
    /// `permission_denied` (owner-equality would never grant access either —
    /// the scope is a channel id).
    #[tokio::test]
    async fn conv_non_member_non_admin_denied() {
        let (_tmp, state) = build_state();
        let channel = [0x73u8; 32];
        let member = [0x74u8; 32];
        let stranger = [0x75u8; 32];
        // `member` is in the channel; `stranger` is not.
        state
            .db
            .register_actor_channel(&member, &channel)
            .await
            .expect("register member");

        let req = CompactRequest {
            kind: Some("conv".into()),
            actor_id: Some(ActorId(channel)),
            extra: std::collections::BTreeMap::new(),
        };
        let err = compact_handler()(state, stranger, encode_req(&req))
            .await
            .expect_err("non-member must be rejected");
        assert_eq!(err.code, "fauna.segments.permission_denied");
    }

    /// A nest admin who is not a channel member can compact any channel's conv
    /// segments. No segments seeded → success with `segments_rewritten = 0`
    /// (the point is that the admin clears the membership gate).
    #[tokio::test]
    async fn conv_admin_can_compact_any_channel() {
        let (_tmp, state) = build_state();
        let channel = [0x76u8; 32];
        let admin = [0x02u8; 32];
        state.db.add_admin_actor(&admin).await.expect("add admin");

        let req = CompactRequest {
            kind: Some("conv".into()),
            actor_id: Some(ActorId(channel)),
            extra: std::collections::BTreeMap::new(),
        };
        let bytes = compact_handler()(state, admin, encode_req(&req))
            .await
            .expect("admin conv compact ok");
        let reply: CompactReply = decode_reply(&bytes);
        assert_eq!(reply.errors, 0);
        assert_eq!(reply.segments_rewritten, 0);
        assert_eq!(reply.scope.kind.as_deref(), Some("conv"));
    }

    /// Gate 2 (Plan 9): even a channel member cannot manually compact a channel
    /// whose `__conv/<hex>` reserved set is a custody copy — the pure-backup gate
    /// fires after the channel-membership auth. (The DAO conv arm derives the
    /// per-channel set name; see `is_pure_backup_destination`.)
    #[tokio::test]
    async fn conv_member_rejected_on_pure_backup_destination() {
        let (_tmp, state) = build_state();
        let channel = [0x77u8; 32];
        let member = [0x78u8; 32];
        state
            .db
            .register_actor_channel(&member, &channel)
            .await
            .expect("register member");
        let name = format!("__conv/{}", hex::encode(channel));
        state
            .db
            .create_folder_with_options(
                &name,
                &channel,
                crate::db::FolderOptions {
                    custody_copy: true,
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let req = CompactRequest {
            kind: Some("conv".into()),
            actor_id: Some(ActorId(channel)),
            extra: std::collections::BTreeMap::new(),
        };
        let err = compact_handler()(state, member, encode_req(&req))
            .await
            .expect_err("pure-backup channel must be rejected");
        assert_eq!(err.code, "fauna.segments.pure_backup_destination");
    }
}
