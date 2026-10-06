//! Integration tests for the post-pipeline strict flip.
//!
//! `fauna.posts.create` must surface a `fauna.posts.ingest_failed` `RpcError`
//! (whose `details` carries the `ingest rejected: post_*` reason) for every
//! `IngestRejectReason::Post*` variant, and succeed on a well-formed signed
//! post. The reject mapping lives in the shared `ingest_post_core` (reused by
//! the WS-RPC handler) — this exercises it through `SealedStorage`, the one
//! `Storage` impl (`docs/goal/architecture/nest/storage-modes.md`), which
//! always runs the strict verifier.
//!
//! Migrated from the deleted `POST /api/v1/posts` HTTP twin to in-process
//! router dispatch (T4); mirrors the
//! `conformance_posts.rs` dispatch harness.
//!
//! Tier: tier_3 (real `AppState` + real `CacheDb` + real `SealedStorage`).

use std::collections::BTreeMap;
use std::sync::Arc;

use bytes::Bytes;
use serde_bytes::ByteBuf;

use fauna_core::identity::ActorKeypair;
use fauna_nest::backup::service::BackupService;
use fauna_nest::db::CacheDb;
use fauna_nest::posts_handlers;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest::token_store::TokenStore;
use fauna_protocol::{RpcError, encode_canonical, posts::PostCreateRequest};

mod common;
use common::signed_text_post;

/// Boot a nest as an in-process router (no HTTP server) and return the
/// `fauna.posts.create` router, the state, and a registered actor.
/// `AppState::for_test` already installs `SealedStorage` — no swap needed.
async fn boot_router() -> (RpcRouter, Arc<AppState>, ActorKeypair) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let dir = tempfile::tempdir().unwrap();
    let dir_path = dir.path().to_path_buf();
    std::mem::forget(dir); // keep the acme/storage dir alive for the test
    let backup_svc =
        Arc::new(BackupService::new(db.clone(), None, false, dir_path.clone(), None).unwrap());
    let token_store = Arc::new(TokenStore::new());

    let state = AppState {
        backup_service: Some(backup_svc),
        auth: fauna_nest::state::AuthState {
            token_store,
            ..Default::default()
        },
        ..AppState::for_test(db.clone())
    };
    let state = Arc::new(state);

    let kp = ActorKeypair::generate();
    db.create_user(&kp.actor_id().0, "free", "test")
        .await
        .unwrap();

    let mut b = RpcRouter::builder();
    posts_handlers::register_posts_handlers(&mut b);
    (b.build(), state, kp)
}

async fn create(
    router: &RpcRouter,
    state: Arc<AppState>,
    actor: [u8; 32],
    body: Vec<u8>,
) -> Result<Bytes, RpcError> {
    let req = PostCreateRequest {
        body: ByteBuf::from(body),
        extra: BTreeMap::new(),
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let meta = router
        .kind_meta("fauna.posts.create")
        .expect("fauna.posts.create registered");
    (meta.handler)(state, actor, payload).await
}

/// Assert the create dispatch failed with `fauna.posts.ingest_failed` whose
/// `details` carries the expected `post_*` reject reason.
fn assert_ingest_rejected(err: &RpcError, reason: &str) {
    assert_eq!(err.code, "fauna.posts.ingest_failed", "code: {err:?}");
    let details = format!("{:?}", err.details);
    assert!(
        details.contains(reason),
        "expected reject reason {reason:?} in details {details:?}"
    );
}

#[tokio::test]
async fn well_formed_signed_post_accepted() {
    let (router, state, kp) = boot_router().await;
    let body = signed_text_post(&kp, "hello fauna");
    create(&router, state, kp.actor_id().0, body)
        .await
        .expect("well-formed signed post accepted");
}

/// Own-write: the connection may only create a post signed as ITSELF — the
/// profile door's rule (`profile_handlers.rs`). Another account's validly
/// signed post is refused with `post_uploader_not_author`, and nothing is
/// stored. The author's own upload of the same bytes is accepted.
#[tokio::test]
async fn anothers_signed_post_rejected_post_uploader_not_author() {
    let (router, state, uploader) = boot_router().await;
    let author = ActorKeypair::generate();
    state
        .db
        .create_user(&author.actor_id().0, "free", "test")
        .await
        .unwrap();
    let body = signed_text_post(&author, "not yours to post");
    let post_id = *blake3::hash(&body).as_bytes();

    let err = create(&router, state.clone(), uploader.actor_id().0, body.clone())
        .await
        .expect_err("a re-upload of another account's post is refused");
    assert_ingest_rejected(&err, "post_uploader_not_author");
    assert!(
        state.db.get_post(&post_id).await.unwrap().is_none(),
        "a refused post leaves no row"
    );

    create(&router, state, author.actor_id().0, body)
        .await
        .expect("the author's own upload of the same post is accepted");
}

/// The own-write bind holds for a delegated authoring sub-key too: the post's
/// `author` is the identity the cert chain ties the sub-key to, and the
/// identity's own connection uploads it.
#[tokio::test]
async fn delegated_subkey_post_accepted_from_its_identity() {
    let (router, state, identity) = boot_router().await;
    let sub = ActorKeypair::generate();
    let body = common::delegated_text_post(&identity, &sub, "via my sub-key");
    create(&router, state, identity.actor_id().0, body)
        .await
        .expect("a certified sub-key post is accepted from its identity");
}

#[tokio::test]
async fn undecodable_bytes_rejected_post_decode() {
    let (router, state, kp) = boot_router().await;
    let err = create(
        &router,
        state,
        kp.actor_id().0,
        b"not a valid dag-cbor-encoded Post".to_vec(),
    )
    .await
    .expect_err("undecodable bytes rejected");
    assert_ingest_rejected(&err, "post_decode");
}

#[tokio::test]
async fn tampered_post_rejected_post_signature_mismatch() {
    use fauna_core::encoding::{EmbedAsBytes, canonical_decode, canonical_encode};
    let (router, state, kp) = boot_router().await;
    let body = signed_text_post(&kp, "hello fauna");
    // Flip a byte in the envelope's signature portion (bytes 36..100; bytes
    // 0..36 are the CID and stay intact). The CID still matches the inner
    // bytes so the canonical-form check passes, but ed25519 verify fails on
    // the corrupted signature → strict path maps to PostSignatureMismatch.
    // Tampering wire.bytes directly would break canonical form and hit
    // PostDecode first.
    let mut wire: EmbedAsBytes = canonical_decode(&body).unwrap();
    wire.envelope[36] ^= 0xff;
    let tampered = canonical_encode(&wire).unwrap();
    let err = create(&router, state, kp.actor_id().0, tampered)
        .await
        .expect_err("tampered post rejected");
    assert_ingest_rejected(&err, "post_signature_mismatch");
}

#[tokio::test]
async fn gated_zero_encrypted_ref_rejected_post_gated_ref_zero() {
    use fauna_core::data::{ContentHash, Post, PostBody, Timestamp};
    use fauna_core::encoding::sign_and_pack;
    use fauna_core::subscription::types::{GatedInfo, KeyAccess, MlsGroupId};
    let (router, state, kp) = boot_router().await;
    let post = Post {
        author: kp.actor_id(),
        created_at: Timestamp::now(),
        body: PostBody::Text {
            content: "preview".into(),
            facets: vec![],
        },
        references: vec![],
        expires_at: None,
        gated: Some(GatedInfo {
            encrypted_ref: ContentHash::from_digest_raw([0u8; 32]),
            key_access: KeyAccess::Room {
                group_id: MlsGroupId(b"g".to_vec()),
                epoch: 0,
                generation: None,
            },
            tier: "gold".into(),
            tier_rank: 2,
            seal_id: fauna_core::data::ContentHash::from_digest_raw([0x5e; 32]),
            attachment_refs: vec![],
        }),
        content_warning: None,
        origin: None,
    };
    let bytes = sign_and_pack(&kp, &post).unwrap();
    let err = create(&router, state, kp.actor_id().0, bytes)
        .await
        .expect_err("gated zero encrypted_ref rejected");
    assert_ingest_rejected(&err, "post_gated_ref_zero");
}

/// A followed author future-dating a post would otherwise pin it atop every
/// follower's chronological feed (`docs/goal/ui/feed.md` § The read model,
/// row 730). Ingest refuses it outright rather than reordering around it.
#[tokio::test]
async fn future_created_at_rejected_post_created_at_in_future() {
    use fauna_core::data::{Post, PostBody, Timestamp};
    use fauna_core::encoding::sign_and_pack;
    let (router, state, kp) = boot_router().await;
    let an_hour_ahead = Timestamp(Timestamp::now().0 + 3_600_000_000);
    let post = Post {
        author: kp.actor_id(),
        created_at: an_hour_ahead,
        body: PostBody::Text {
            content: "from the future".into(),
            facets: vec![],
        },
        references: vec![],
        expires_at: None,
        gated: None,
        content_warning: None,
        origin: None,
    };
    let bytes = sign_and_pack(&kp, &post).unwrap();
    let err = create(&router, state, kp.actor_id().0, bytes)
        .await
        .expect_err("future-dated post rejected");
    assert_ingest_rejected(&err, "post_created_at_in_future");
}

/// The past stays legal — an archive import re-authors a post at its
/// original, long-past instant, and ingest must not treat that as suspect
/// (`docs/goal/behavior/archive-import.md` § What each category becomes).
#[tokio::test]
async fn decade_old_created_at_accepted() {
    use fauna_core::data::{Post, PostBody, Timestamp};
    use fauna_core::encoding::sign_and_pack;
    let (router, state, kp) = boot_router().await;
    let a_decade_ago = Timestamp(Timestamp::now().0 - 10 * 365 * 24 * 3_600_000_000);
    let post = Post {
        author: kp.actor_id(),
        created_at: a_decade_ago,
        body: PostBody::Text {
            content: "from the archive".into(),
            facets: vec![],
        },
        references: vec![],
        expires_at: None,
        gated: None,
        content_warning: None,
        origin: None,
    };
    let bytes = sign_and_pack(&kp, &post).unwrap();
    create(&router, state, kp.actor_id().0, bytes)
        .await
        .expect("a decade-old created_at is accepted");
}

#[tokio::test]
async fn gated_empty_mls_group_id_rejected_post_key_access_inconsistent() {
    use fauna_core::data::{ContentHash, Post, PostBody, Timestamp};
    use fauna_core::encoding::sign_and_pack;
    use fauna_core::subscription::types::{GatedInfo, KeyAccess, MlsGroupId};
    let (router, state, kp) = boot_router().await;
    let post = Post {
        author: kp.actor_id(),
        created_at: Timestamp::now(),
        body: PostBody::Text {
            content: "preview".into(),
            facets: vec![],
        },
        references: vec![],
        expires_at: None,
        gated: Some(GatedInfo {
            encrypted_ref: ContentHash::from_digest_raw([1u8; 32]),
            key_access: KeyAccess::Room {
                group_id: MlsGroupId(Vec::new()),
                epoch: 1,
                generation: None,
            },
            tier: "gold".into(),
            tier_rank: 2,
            seal_id: fauna_core::data::ContentHash::from_digest_raw([0x5e; 32]),
            attachment_refs: vec![],
        }),
        content_warning: None,
        origin: None,
    };
    let bytes = sign_and_pack(&kp, &post).unwrap();
    let err = create(&router, state, kp.actor_id().0, bytes)
        .await
        .expect_err("gated empty mls group_id rejected");
    assert_ingest_rejected(&err, "post_key_access_inconsistent");
}
