//! Integration round-trip for the share-link control plane —
//! `fauna.share.{create,list,revoke}`. A **net-new** feature (no HTTP twin): the
//! client mints a real self-verifying `ShareToken`, the handler decodes+verifies
//! it and registers its metadata; `list`/`revoke` read it back. These tests
//! exercise the WS-RPC layer: request decode, reply shapes, the `User | Admin`
//! allowlist, the `invalid_request` / `not_found` / `permission_denied`
//! mappings, author-binding, cross-actor isolation, idempotency, and replay
//! metadata.
//!
//! Authority for the wire types: `libs/fauna-protocol/src/share.rs`.
//!
//! Tier: tier_3 (real `fauna-nest` `AppState` + real `CacheDb` — no mocks; a
//! real signed token minted via `fauna_core::share::ShareToken`).

mod common;
use common::dispatch;
use common::encode;

use std::sync::Arc;

use bytes::Bytes;

use fauna_core::{
    identity::ActorKeypair,
    share::{ShareToken, token_id_from_base64url},
};
use fauna_nest::{
    bridge_method_allowlist::{CallerClass, is_permitted},
    db::CacheDb,
    routes::AppState,
    rpc_router::RpcRouter,
    share_handlers,
};
use fauna_protocol::{
    RpcError, decode_strict as decode,
    share::{
        ShareCreateReply, ShareCreateRequest, ShareListReply, ShareListRequest, ShareRevokeReply,
        ShareRevokeRequest,
    },
};

const ALL_KINDS: [&str; 3] = [
    "fauna.share.create",
    "fauna.share.list",
    "fauna.share.revoke",
];

/// A realistic far-future expiry (year 2100) for tokens that should not expire
/// during the test.
const FUTURE: u64 = 4_102_444_800;

async fn router_and_state() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    share_handlers::register_share_handlers(&mut b);
    (b.build(), state)
}

/// Mint a self-verifying ShareToken for `kp` and return its base64url form.
fn mint(
    kp: &ActorKeypair,
    manifest: [u8; 32],
    filename: &str,
    expires: u64,
    public: bool,
) -> String {
    ShareToken::new(manifest, kp.actor_id(), filename.into(), expires, public)
        .to_base64url(kp)
        .unwrap()
}

async fn create(
    router: &RpcRouter,
    state: &Arc<AppState>,
    actor: [u8; 32],
    token: String,
) -> Result<ShareCreateReply, RpcError> {
    let bytes = dispatch(
        router,
        Arc::clone(state),
        actor,
        "fauna.share.create",
        encode(&ShareCreateRequest {
            token,
            filename_sealed: serde_bytes::ByteBuf::from(SEAL.to_vec()),
            ..Default::default()
        }),
    )
    .await?;
    Ok(decode(&bytes).unwrap())
}

async fn list(router: &RpcRouter, state: &Arc<AppState>, actor: [u8; 32]) -> ShareListReply {
    let bytes = dispatch(
        router,
        Arc::clone(state),
        actor,
        "fauna.share.list",
        encode(&ShareListRequest {
            extra: Default::default(),
        }),
    )
    .await
    .expect("list ok");
    decode(&bytes).unwrap()
}

/// A stand-in for the author's sealed filename: the nest stores it opaque.
const SEAL: &[u8] = &[0x5E, 0xA1, 0xED];

// ── create + list ────────────────────────────────────────────────────────────

#[tokio::test]
async fn create_then_list_round_trip() {
    let (router, state) = router_and_state().await;
    let kp = ActorKeypair::generate();
    let actor = kp.actor_id().0;
    let manifest = [0xAB; 32];
    let token = mint(&kp, manifest, "report.pdf", FUTURE, true);
    let expected_id = hex::encode(token_id_from_base64url(&token).unwrap());

    let created = create(&router, &state, actor, token)
        .await
        .expect("create ok");
    assert_eq!(created.share.token_id, expected_id);
    assert_eq!(created.share.filename_sealed.as_slice(), SEAL);
    assert_eq!(created.share.manifest_hash, hex::encode(manifest));
    assert!(created.share.public);
    assert!(!created.share.revoked);
    assert_eq!(created.share.expires_at, FUTURE as i64);

    let listed = list(&router, &state, actor).await;
    assert_eq!(listed.shares.len(), 1);
    assert_eq!(listed.shares[0].token_id, expected_id);
}

#[tokio::test]
async fn create_is_idempotent() {
    let (router, state) = router_and_state().await;
    let kp = ActorKeypair::generate();
    let actor = kp.actor_id().0;
    // `public: true` — the only registrable form since finding closed
    // (restricted shares are born gated, `a_non_public_token_is_refused_at_mint`
    // below). This test is about idempotency, not the flag.
    let token = mint(&kp, [2; 32], "a.txt", FUTURE, true);

    create(&router, &state, actor, token.clone())
        .await
        .expect("first create");
    create(&router, &state, actor, token)
        .await
        .expect("second create idempotent");
    assert_eq!(list(&router, &state, actor).await.shares.len(), 1);
}

#[tokio::test]
async fn create_rejects_wrong_author() {
    let (router, state) = router_and_state().await;
    let author = ActorKeypair::generate();
    let other = ActorKeypair::generate();
    let token = mint(&author, [1; 32], "a.txt", FUTURE, true);

    // Register as a DIFFERENT actor than the token's author → permission_denied.
    let err = create(&router, &state, other.actor_id().0, token)
        .await
        .expect_err("wrong author rejected");
    assert_eq!(err.code, "fauna.share.permission_denied");
}

/// **Restricted shares are BORN GATED**. `/share/{token}` serves only public
/// tokens, so registering a `public: false` one would hand the sharer a dead
/// link — the same failure the E2EE type gate prevents, refused the same way,
/// at share time rather than at open time.
///
/// The flag stays on the wire deliberately: it is the door to gate when
/// restricted shares are actually built, which needs a real audience on
/// `ShareToken` plus the app UI that sets it. What it is *not* any more is a
/// flag that silently bought nothing — the serve-side check it used to reach
/// compared the presented caller to nothing at all.
#[tokio::test]
async fn a_non_public_token_is_refused_at_mint() {
    let (router, state) = router_and_state().await;
    let kp = ActorKeypair::generate();
    let actor = kp.actor_id().0;
    let token = mint(&kp, [7; 32], "a.txt", FUTURE, false);

    let err = create(&router, &state, actor, token)
        .await
        .expect_err("a public: false token must not register");
    assert_eq!(err.code, "fauna.share.restricted_shares_unimplemented");
    assert!(
        list(&router, &state, actor).await.shares.is_empty(),
        "a refused registration must leave no row behind"
    );
}

#[tokio::test]
async fn create_rejects_invalid_token() {
    let (router, state) = router_and_state().await;
    // Valid base64url, but not a valid signed EmbedAsBytes token.
    let err = create(&router, &state, [5u8; 32], "bm90LWEtdG9rZW4".into())
        .await
        .expect_err("invalid token rejected");
    assert_eq!(err.code, "fauna.share.invalid_request");
}

// ── the sealed filename (share-links.md § The filename rests sealed) ─────────

async fn create_sealed(
    router: &RpcRouter,
    state: &Arc<AppState>,
    actor: [u8; 32],
    token: String,
    filename_sealed: Vec<u8>,
) -> Result<ShareCreateReply, RpcError> {
    let bytes = dispatch(
        router,
        Arc::clone(state),
        actor,
        "fauna.share.create",
        encode(&ShareCreateRequest {
            token,
            filename_sealed: serde_bytes::ByteBuf::from(filename_sealed),
            ..Default::default()
        }),
    )
    .await?;
    Ok(decode(&bytes).unwrap())
}

/// A registration rests the author's seal and the name in no other form: the
/// reply and the list are both read back from the stored row, and both carry
/// the seal verbatim.
#[tokio::test]
async fn a_sealed_name_reads_back_sealed_and_never_rests_plaintext() {
    let (router, state) = router_and_state().await;
    let kp = ActorKeypair::generate();
    let actor = kp.actor_id().0;
    let token = mint(&kp, [0x5A; 32], "holiday.jpg", FUTURE, true);
    let seal = vec![0xC0, 0xFF, 0xEE];

    let created = create_sealed(&router, &state, actor, token, seal.clone())
        .await
        .expect("create ok");
    assert_eq!(created.share.filename_sealed.as_slice(), &seal[..]);

    let listed = list(&router, &state, actor).await;
    assert_eq!(listed.shares.len(), 1);
    assert_eq!(listed.shares[0].filename_sealed.as_slice(), &seal[..]);
}

/// A registration without the sealed name would list as a row nobody can name
/// — refused rather than rested, whatever name the token itself signs.
#[tokio::test]
async fn a_sealless_registration_is_refused() {
    let (router, state) = router_and_state().await;
    let kp = ActorKeypair::generate();
    let token = mint(&kp, [0x5C; 32], "notes.txt", FUTURE, true);
    let err = create_sealed(&router, &state, kp.actor_id().0, token.clone(), vec![])
        .await
        .expect_err("sealless refused");
    assert_eq!(err.code, "fauna.share.invalid_request");
    assert!(
        list(&router, &state, kp.actor_id().0)
            .await
            .shares
            .is_empty()
    );
    // The same token WITH a seal registers.
    create_sealed(&router, &state, kp.actor_id().0, token, vec![1, 2])
        .await
        .expect("sealed ok");
}

/// An oversized seal is refused before anything rests.
#[tokio::test]
async fn an_oversized_sealed_name_is_refused() {
    let (router, state) = router_and_state().await;
    let kp = ActorKeypair::generate();
    let token = mint(&kp, [0x5D; 32], "a.txt", FUTURE, true);
    let err = create_sealed(
        &router,
        &state,
        kp.actor_id().0,
        token,
        vec![0u8; 64 * 1024],
    )
    .await
    .expect_err("oversized refused");
    assert_eq!(err.code, "fauna.share.invalid_request");
    assert!(
        list(&router, &state, kp.actor_id().0)
            .await
            .shares
            .is_empty()
    );
}

#[tokio::test]
async fn max_expiry_saturates_to_i64_max() {
    let (router, state) = router_and_state().await;
    let kp = ActorKeypair::generate();
    let token = mint(&kp, [1; 32], "a.txt", u64::MAX, true);
    let created = create(&router, &state, kp.actor_id().0, token)
        .await
        .expect("create ok");
    assert_eq!(created.share.expires_at, i64::MAX);
}

#[tokio::test]
async fn list_is_per_actor() {
    let (router, state) = router_and_state().await;
    let a = ActorKeypair::generate();
    let b = ActorKeypair::generate();
    create(
        &router,
        &state,
        a.actor_id().0,
        mint(&a, [1; 32], "a.txt", FUTURE, true),
    )
    .await
    .expect("a creates");

    assert!(
        list(&router, &state, b.actor_id().0)
            .await
            .shares
            .is_empty()
    );
    assert_eq!(list(&router, &state, a.actor_id().0).await.shares.len(), 1);
}

// ── revoke ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn create_revoke_then_listed_revoked() {
    let (router, state) = router_and_state().await;
    let kp = ActorKeypair::generate();
    let actor = kp.actor_id().0;
    let token = mint(&kp, [3; 32], "a.txt", FUTURE, true);
    let created = create(&router, &state, actor, token)
        .await
        .expect("create ok");
    let token_id = created.share.token_id.clone();

    let revoked: ShareRevokeReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.share.revoke",
            encode(&ShareRevokeRequest {
                token_id: token_id.clone(),
                extra: Default::default(),
            }),
        )
        .await
        .expect("revoke ok"),
    )
    .unwrap();
    assert!(revoked.ok);

    assert!(list(&router, &state, actor).await.shares[0].revoked);

    // Re-revoke is idempotent — the UPDATE still matches the owned row.
    let again: ShareRevokeReply = decode(
        &dispatch(
            &router,
            state,
            actor,
            "fauna.share.revoke",
            encode(&ShareRevokeRequest {
                token_id,
                extra: Default::default(),
            }),
        )
        .await
        .expect("re-revoke ok"),
    )
    .unwrap();
    assert!(again.ok);
}

#[tokio::test]
async fn revoke_unknown_token_not_found() {
    let (router, state) = router_and_state().await;
    let err = dispatch(
        &router,
        state,
        [6u8; 32],
        "fauna.share.revoke",
        encode(&ShareRevokeRequest {
            token_id: "ab".repeat(32),
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("unknown → not_found");
    assert_eq!(err.code, "fauna.share.not_found");
}

#[tokio::test]
async fn revoke_rejects_bad_hex() {
    let (router, state) = router_and_state().await;
    let err = dispatch(
        &router,
        state,
        [6u8; 32],
        "fauna.share.revoke",
        encode(&ShareRevokeRequest {
            token_id: "zz".into(),
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("bad hex");
    assert_eq!(err.code, "fauna.share.invalid_request");
}

#[tokio::test]
async fn cross_actor_revoke_not_found() {
    let (router, state) = router_and_state().await;
    let owner = ActorKeypair::generate();
    let attacker = ActorKeypair::generate();
    let token = mint(&owner, [4; 32], "secret.pdf", FUTURE, true);
    let created = create(&router, &state, owner.actor_id().0, token)
        .await
        .expect("create ok");

    // Attacker cannot revoke the owner's token.
    let err = dispatch(
        &router,
        Arc::clone(&state),
        attacker.actor_id().0,
        "fauna.share.revoke",
        encode(&ShareRevokeRequest {
            token_id: created.share.token_id.clone(),
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("cross-actor revoke → not_found");
    assert_eq!(err.code, "fauna.share.not_found");

    // And it stays unrevoked for the owner.
    assert!(!list(&router, &state, owner.actor_id().0).await.shares[0].revoked);
}

// ── malformed payload ──────────────────────────────────────────────────────────

#[tokio::test]
async fn rejects_malformed_payload() {
    let (router, state) = router_and_state().await;
    let err = dispatch(
        &router,
        state,
        [7u8; 32],
        "fauna.share.create",
        Bytes::from_static(&[0xff, 0xff, 0xff]),
    )
    .await
    .expect_err("malformed payload rejected");
    assert_eq!(err.code, "fauna.protocol.malformed");
}

// ── replay metadata + allowlist ──────────────────────────────────────────────────

#[tokio::test]
async fn replay_metadata_matches_spec() {
    let (router, _state) = router_and_state().await;
    for kind in ALL_KINDS {
        let m = router.kind_meta(kind).expect("kind registered");
        assert!(!m.forbid_replay, "{kind} does not forbid replay");
        assert_eq!(m.default_deadline, std::time::Duration::from_secs(5));
    }
}

#[tokio::test]
async fn allowlist_user_admin_permitted_bridges_denied() {
    for kind in ALL_KINDS {
        for class in [CallerClass::User, CallerClass::Admin] {
            assert!(is_permitted(class, kind), "{kind} permitted for {class:?}");
        }
        for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
            assert!(!is_permitted(class, kind), "{kind} denied for {class:?}");
        }
    }
}

// ── E2EE (content-key) manifest mint gate — INFO-2 ───────────────────────────
//
// The public `GET /share/{token}` cannot serve a content-key (M2 / end-to-end
// encrypted) file — the nest holds no content key, so serving refuses with 403
// (`share_routes.rs` step 8b). The mint-side twin: `fauna.share.create` refuses
// to REGISTER a token whose manifest is positively identified as E2EE
// (`stored_hashes.is_some()`), so the sharer gets an honest error at share time
// instead of handing out a dead link. Best-effort only: an absent manifest
// (mint-before-upload), no backup service, or an undecodable blob registers as
// before — the serve-side gate stays authoritative.

/// `router_and_state` plus a real `BackupService`/`DiskBlobStore` so the create
/// handler can inspect stored manifests (mirrors
/// `conformance_content_key_chunk_route::start_test_nest`).
async fn router_and_state_with_store() -> (
    RpcRouter,
    Arc<AppState>,
    Arc<fauna_nest::backup::service::BackupService>,
) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let blob_dir = tempfile::tempdir().unwrap();
    let blob_path = blob_dir.path().to_path_buf();
    std::mem::forget(blob_dir); // outlive the test; never reused
    let backup_svc = Arc::new(
        fauna_nest::backup::service::BackupService::new(db.clone(), None, false, blob_path, None)
            .unwrap(),
    );
    let state = Arc::new(AppState {
        backup_service: Some(backup_svc.clone()),
        ..AppState::for_test(db)
    });
    let mut b = RpcRouter::builder();
    share_handlers::register_share_handlers(&mut b);
    (b.build(), state, backup_svc)
}

/// Encode + store `manifest` in the nest's local blob store the way the backup
/// plane does (`encode_blob` framing, content-addressed by the raw manifest
/// bytes — the hash a `ShareToken` carries), returning that 32-byte digest.
async fn put_manifest(
    svc: &fauna_nest::backup::service::BackupService,
    manifest: &fauna_core::chunk::ChunkManifest,
) -> [u8; 32] {
    let raw = fauna_core::encoding::canonical_encode(manifest).unwrap();
    let hash = fauna_core::data::ContentHash::of_raw(&raw);
    let blob =
        fauna_nest::backup::encode_blob(&raw, svc.encryption_key(), svc.compression()).unwrap();
    svc.local_blob_store().put(&hash, &blob).await.unwrap();
    hash.digest()
}

/// A minimal one-chunk manifest; `e2ee` decides whether it carries the
/// ciphertext `stored_hashes` (the content-key marker `share_routes.rs`
/// refuses on).
fn manifest_fixture(e2ee: bool) -> fauna_core::chunk::ChunkManifest {
    let chunk_plain = fauna_core::data::ContentHash::of_raw(b"plaintext chunk");
    fauna_core::chunk::ChunkManifest {
        file_hash: chunk_plain,
        total_size: 15,
        chunk_hashes: vec![chunk_plain],
        chunk_sizes: vec![15],
        stored_hashes: e2ee
            .then(|| vec![fauna_core::data::ContentHash::of_raw(b"sealed ciphertext")]),
        sealed_hashes: None,
        min_reader: None,
    }
}

#[tokio::test]
async fn create_refuses_e2ee_manifest() {
    let (router, state, svc) = router_and_state_with_store().await;
    let kp = ActorKeypair::generate();
    let manifest_hash = put_manifest(&svc, &manifest_fixture(true)).await;

    let token = mint(&kp, manifest_hash, "sealed.bin", FUTURE, true);
    let err = create(&router, &state, kp.actor_id().0, token)
        .await
        .expect_err("E2EE manifest must not register a share token");
    assert_eq!(err.code, "fauna.share.unsupported_encrypted_set");

    // Refusal means no registry row.
    assert!(
        list(&router, &state, kp.actor_id().0)
            .await
            .shares
            .is_empty()
    );
}

#[tokio::test]
async fn create_registers_plain_manifest_with_store() {
    let (router, state, svc) = router_and_state_with_store().await;
    let kp = ActorKeypair::generate();
    let manifest_hash = put_manifest(&svc, &manifest_fixture(false)).await;

    let token = mint(&kp, manifest_hash, "plain.txt", FUTURE, true);
    create(&router, &state, kp.actor_id().0, token)
        .await
        .expect("plain (non-content-key) manifest registers");
}

#[tokio::test]
async fn create_registers_when_manifest_absent() {
    // Mint-before-upload stays legal: a token over a manifest hash that is not
    // (yet) in the blob store registers — the gate fires only on a POSITIVELY
    // identified E2EE manifest; the serve-side 403 is the authoritative gate.
    let (router, state, _svc) = router_and_state_with_store().await;
    let kp = ActorKeypair::generate();

    let token = mint(&kp, [0xEE; 32], "not-yet-uploaded.txt", FUTURE, true);
    create(&router, &state, kp.actor_id().0, token)
        .await
        .expect("absent manifest registers (mint-before-upload)");
}

// ── fragment-keyed private links (`share-links.md` § The private-file extension) ──
//
// A sealed file in an owner-only folder: its chunks rest AEAD-sealed under the
// owner's root (the M2 shape — `stored_hashes` present). A fragment-keyed link
// registers the file's key envelope with the token; `GET /share/{token}` then
// answers three things — the viewer page, the manifest + envelope, the
// ciphertext chunks by index — and never a plaintext byte. The sealed-manifest
// 403 for an UNDECLARED token is the untouched authoritative gate.

/// The owner root the seeded file's chunks are sealed under.
const OWNER_ROOT: [u8; 32] = [0x42; 32];
/// The marker the stand-in SPA build's viewer entry (`share-viewer.html`) carries.
const VIEWER_MARKER: &str = "fauna-share-viewer-stand-in";
/// The marker the stand-in SPA build's app shell (`index.html`) carries.
const APP_SHELL_MARKER: &str = "fauna-app-shell-stand-in";

struct PrivateNest {
    router: RpcRouter,
    state: Arc<AppState>,
    kp: ActorKeypair,
    content: Vec<u8>,
    manifest_hash: [u8; 32],
    chunk_count: usize,
    link_key: [u8; 32],
    sealed_envelope: Vec<u8>,
}

/// Distinct, incompressible-ish bytes: a file several chunks long whose every
/// window is unique, so "no plaintext window in a response" is a real check.
fn private_content() -> Vec<u8> {
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    (0..300_000)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

/// A nest with a blob store, a stand-in SPA build, a registered author and one
/// sealed file seeded exactly as the sync engine uploads it.
async fn private_nest() -> PrivateNest {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let blob_dir = tempfile::tempdir().unwrap();
    let blob_path = blob_dir.path().to_path_buf();
    std::mem::forget(blob_dir);
    let static_dir = tempfile::tempdir().unwrap();
    // The SPA build's two entries: the app shell, and the viewer page the
    // private arm must answer instead — never the app shell, whose root layout
    // loads the identity store (`share-links.md` rule 3).
    std::fs::write(
        static_dir.path().join("index.html"),
        format!("<!doctype html><title>Fauna</title><p>{APP_SHELL_MARKER}</p>"),
    )
    .unwrap();
    std::fs::write(
        static_dir.path().join("share-viewer.html"),
        format!("<!doctype html><title>Fauna</title><p>{VIEWER_MARKER}</p>"),
    )
    .unwrap();
    let static_path = static_dir.path().to_str().unwrap().to_string();
    std::mem::forget(static_dir);

    let svc = Arc::new(
        fauna_nest::backup::service::BackupService::new(db.clone(), None, false, blob_path, None)
            .unwrap(),
    );
    let base = AppState::for_test(db.clone());
    let mut config = (*base.config).clone();
    config.nest.static_dir = Some(static_path);
    config.nest.domain = Some("nest.example".into());
    let state = Arc::new(AppState {
        backup_service: Some(svc.clone()),
        config: Arc::new(config),
        ..base
    });

    let kp = ActorKeypair::generate();
    db.create_user(&kp.actor_id().0, "free", "author")
        .await
        .unwrap();

    // The chunker keeps a file under 8 MB whole, so the several-chunk manifest
    // a large file gets is laid out by hand: the same shape, at a size the
    // plaintext-window check can afford.
    let content = private_content();
    let chunks: Vec<(fauna_core::data::ContentHash, Vec<u8>)> = content
        .chunks(40_000)
        .map(|c| (fauna_core::data::ContentHash::of_raw(c), c.to_vec()))
        .collect();
    let mut manifest = fauna_core::chunk::ChunkManifest {
        file_hash: fauna_core::data::ContentHash::of_raw(&content),
        total_size: content.len() as u64,
        chunk_hashes: chunks.iter().map(|(h, _)| *h).collect(),
        chunk_sizes: chunks.iter().map(|(_, c)| c.len() as u64).collect(),
        stored_hashes: None,
        sealed_hashes: None,
        min_reader: None,
    };
    assert!(chunks.len() >= 2, "the fixture must span several chunks");
    let store = svc.local_blob_store();
    let mut stored = Vec::new();
    for (hash, data) in &chunks {
        let (store_key, body) =
            fauna_core::chunk_seal::seal_chunk_body(hash, data, &OWNER_ROOT).unwrap();
        let blob = fauna_nest::backup::encode_blob(&body, svc.encryption_key(), false).unwrap();
        store.put(&store_key, &blob).await.unwrap();
        stored.push(store_key);
    }
    manifest.stored_hashes = Some(stored);
    let manifest_hash = put_manifest(&svc, &manifest).await;

    let envelope = fauna_core::share::KeyEnvelope::derive(
        &OWNER_ROOT,
        "holiday-plan.txt".into(),
        manifest.file_hash,
        manifest.chunk_hashes.clone(),
    );
    let link_key = fauna_core::share::generate_link_key();
    let sealed_envelope = envelope.seal(&link_key).unwrap();

    let mut b = RpcRouter::builder();
    share_handlers::register_share_handlers(&mut b);
    PrivateNest {
        router: b.build(),
        state,
        kp,
        content,
        manifest_hash,
        chunk_count: chunks.len(),
        link_key,
        sealed_envelope,
    }
}

impl PrivateNest {
    fn private_token(&self) -> String {
        ShareToken::fragment_keyed(self.manifest_hash, self.kp.actor_id(), FUTURE)
            .to_base64url(&self.kp)
            .unwrap()
    }

    async fn register(
        &self,
        token: &str,
        key_envelope: Option<Vec<u8>>,
    ) -> Result<ShareCreateReply, RpcError> {
        let bytes = dispatch(
            &self.router,
            Arc::clone(&self.state),
            self.kp.actor_id().0,
            "fauna.share.create",
            encode(&ShareCreateRequest {
                token: token.to_string(),
                key_envelope: key_envelope.map(serde_bytes::ByteBuf::from),
                filename_sealed: serde_bytes::ByteBuf::from(SEAL.to_vec()),
                ..Default::default()
            }),
        )
        .await?;
        Ok(decode(&bytes).unwrap())
    }

    /// GET `path` over the real router; `navigation` sends a browser's
    /// top-level `Accept`. Returns `(status, headers, body)`.
    async fn get(
        &self,
        path: &str,
        navigation: bool,
    ) -> (axum::http::StatusCode, axum::http::HeaderMap, Vec<u8>) {
        use tower::ServiceExt as _;
        let app = fauna_nest::build_router(self.state.clone());
        let mut req = axum::http::Request::builder().uri(path);
        if navigation {
            req = req.header("accept", "text/html,application/xhtml+xml");
        }
        let resp = app
            .oneshot(req.body(axum::body::Body::empty()).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let headers = resp.headers().clone();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec();
        (status, headers, body)
    }

    /// Whether `body` carries any 32-byte window of the file's plaintext.
    fn leaks_plaintext(&self, body: &[u8]) -> bool {
        if body.len() < 32 {
            return false;
        }
        let windows: std::collections::HashSet<&[u8]> = body.windows(32).collect();
        self.content
            .windows(32)
            .step_by(16)
            .any(|w| windows.contains(w))
    }
}

#[tokio::test]
async fn a_private_link_registers_and_serves_only_ciphertext() {
    let nest = private_nest().await;
    let token = nest.private_token();
    let created = nest
        .register(&token, Some(nest.sealed_envelope.clone()))
        .await
        .expect("a declared token over a sealed manifest registers with its envelope");
    assert!(created.share.key_in_fragment);

    // The navigation GET: the viewer page, under the `/app/` headers.
    let (status, headers, body) = nest.get(&format!("/share/{token}"), true).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert!(
        headers["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/html"),
        "the viewer is an HTML page"
    );
    assert!(String::from_utf8_lossy(&body).contains(VIEWER_MARKER));
    assert!(!String::from_utf8_lossy(&body).contains(APP_SHELL_MARKER));
    assert_eq!(headers["referrer-policy"], "no-referrer");
    assert_eq!(headers["x-frame-options"], "DENY");
    assert!(!nest.leaks_plaintext(&body));

    // The manifest + envelope: the manifest is the one the signed token names.
    let (status, _, body) = nest.get(&format!("/share/{token}/manifest"), false).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert!(!nest.leaks_plaintext(&body));
    let reply: fauna_protocol::share::ShareFragmentManifest = decode(&body).unwrap();
    assert_eq!(
        fauna_core::data::ContentHash::of_raw(&reply.manifest).digest(),
        nest.manifest_hash
    );
    assert_eq!(
        reply.key_envelope.as_slice(),
        nest.sealed_envelope.as_slice()
    );
    let envelope =
        fauna_core::share::KeyEnvelope::open(&nest.link_key, &reply.key_envelope).unwrap();
    assert_eq!(envelope.filename, "holiday-plan.txt");

    // The chunks, by index: ciphertext only — and the envelope opens them back
    // into exactly the file.
    let mut ciphertexts = Vec::new();
    for i in 0..nest.chunk_count {
        let (status, _, body) = nest.get(&format!("/share/{token}/chunk/{i}"), false).await;
        assert_eq!(status, axum::http::StatusCode::OK, "chunk {i}");
        assert!(!nest.leaks_plaintext(&body), "chunk {i} leaked plaintext");
        ciphertexts.push(body);
    }
    assert_eq!(envelope.open_file(&ciphertexts).unwrap(), nest.content);
    let (status, _, _) = nest
        .get(&format!("/share/{token}/chunk/{}", nest.chunk_count), false)
        .await;
    assert_eq!(
        status,
        axum::http::StatusCode::NOT_FOUND,
        "past the last chunk"
    );
}

#[tokio::test]
async fn revoking_a_private_link_turns_every_arm_410() {
    let nest = private_nest().await;
    let token = nest.private_token();
    let created = nest
        .register(&token, Some(nest.sealed_envelope.clone()))
        .await
        .unwrap();
    let revoked = dispatch(
        &nest.router,
        Arc::clone(&nest.state),
        nest.kp.actor_id().0,
        "fauna.share.revoke",
        encode(&ShareRevokeRequest {
            token_id: created.share.token_id,
            extra: Default::default(),
        }),
    )
    .await;
    assert!(revoked.is_ok());
    for (path, navigation) in [
        (format!("/share/{token}"), true),
        (format!("/share/{token}/manifest"), false),
        (format!("/share/{token}/chunk/0"), false),
    ] {
        let (status, _, body) = nest.get(&path, navigation).await;
        assert_eq!(status, axum::http::StatusCode::GONE, "{path}");
        assert!(!nest.leaks_plaintext(&body));
    }
}

/// The pin that keeps the gate: the SAME sealed manifest under a token that
/// does not declare the fragment key is still refused 403 on the plain route,
/// and the private arm's sub-routes do not exist for it.
#[tokio::test]
async fn a_sealed_manifest_under_an_undeclared_token_is_still_403() {
    let nest = private_nest().await;
    let token = mint(&nest.kp, nest.manifest_hash, "", FUTURE, true);
    let (status, _, body) = nest.get(&format!("/share/{token}"), true).await;
    assert_eq!(status, axum::http::StatusCode::FORBIDDEN);
    assert!(!nest.leaks_plaintext(&body));
    for sub in ["manifest", "chunk/0"] {
        let (status, _, _) = nest.get(&format!("/share/{token}/{sub}"), false).await;
        assert_eq!(status, axum::http::StatusCode::NOT_FOUND, "{sub}");
    }
}

#[tokio::test]
async fn a_declared_token_over_a_plaintext_manifest_is_refused_at_mint() {
    let (router, state, svc) = router_and_state_with_store().await;
    let kp = ActorKeypair::generate();
    let manifest_hash = put_manifest(&svc, &manifest_fixture(false)).await;
    let token = ShareToken::fragment_keyed(manifest_hash, kp.actor_id(), FUTURE)
        .to_base64url(&kp)
        .unwrap();
    let bytes = dispatch(
        &router,
        Arc::clone(&state),
        kp.actor_id().0,
        "fauna.share.create",
        encode(&ShareCreateRequest {
            token,
            key_envelope: Some(serde_bytes::ByteBuf::from(vec![1u8; 64])),
            filename_sealed: serde_bytes::ByteBuf::from(SEAL.to_vec()),
            ..Default::default()
        }),
    )
    .await;
    let err = bytes.expect_err("nothing to key: the manifest is plaintext");
    assert_eq!(err.code, "fauna.share.invalid_request");
    assert!(
        list(&router, &state, kp.actor_id().0)
            .await
            .shares
            .is_empty()
    );
}

#[tokio::test]
async fn the_envelope_travels_only_with_a_declared_token() {
    let nest = private_nest().await;
    // A declared token without its envelope: refused (it could never open).
    let err = nest
        .register(&nest.private_token(), None)
        .await
        .expect_err("a private link needs its envelope");
    assert_eq!(err.code, "fauna.share.invalid_request");
    // An envelope on an undeclared token: refused (a key resting for nothing).
    let public = mint(&nest.kp, [0xEE; 32], "a.txt", FUTURE, true);
    let err = nest
        .register(&public, Some(nest.sealed_envelope.clone()))
        .await
        .expect_err("an envelope on a public link is refused");
    assert_eq!(err.code, "fauna.share.invalid_request");
    // A declared token carrying a plaintext filename: refused — the name would
    // ride every request log.
    let named = ShareToken {
        filename: "holiday-plan.txt".into(),
        ..ShareToken::fragment_keyed(nest.manifest_hash, nest.kp.actor_id(), FUTURE)
    }
    .to_base64url(&nest.kp)
    .unwrap();
    let err = nest
        .register(&named, Some(nest.sealed_envelope.clone()))
        .await
        .expect_err("a private link's token carries no name");
    assert_eq!(err.code, "fauna.share.invalid_request");
    assert!(
        list(&nest.router, &nest.state, nest.kp.actor_id().0)
            .await
            .shares
            .is_empty()
    );
}

/// A declared token that was never registered has no envelope anywhere, so
/// no arm can serve it.
#[tokio::test]
async fn an_unregistered_private_link_serves_nothing() {
    let nest = private_nest().await;
    let token = nest.private_token();
    for (path, navigation) in [
        (format!("/share/{token}"), true),
        (format!("/share/{token}/manifest"), false),
        (format!("/share/{token}/chunk/0"), false),
    ] {
        let (status, _, _) = nest.get(&path, navigation).await;
        assert_eq!(status, axum::http::StatusCode::NOT_FOUND, "{path}");
    }
}

/// In central web-app-origin mode the viewer navigation takes the same `302`
/// the `/app/` rule gives: the path carried over, no fragment in the target —
/// so the browser keeps the link key across the hop and sends it nowhere.
#[tokio::test]
async fn in_central_mode_the_viewer_navigation_redirects_like_app() {
    let nest = private_nest().await;
    let token = nest.private_token();
    nest.register(&token, Some(nest.sealed_envelope.clone()))
        .await
        .unwrap();
    nest.state.web_app_origin.store(Arc::new(
        fauna_protocol::web_app_origin::WebAppOrigin::Central,
    ));
    let (status, headers, _) = nest.get(&format!("/share/{token}"), true).await;
    assert_eq!(status, axum::http::StatusCode::FOUND);
    let location = headers["location"].to_str().unwrap();
    assert!(location.contains(&format!("/share/{token}")), "{location}");
    assert!(location.contains("nest=nest.example"), "{location}");
    assert!(!location.contains('#'));
    assert_eq!(headers["referrer-policy"], "no-referrer");
    // The data arms are not navigations: they answer the same bytes in either
    // mode.
    let (status, _, _) = nest.get(&format!("/share/{token}/manifest"), false).await;
    assert_eq!(status, axum::http::StatusCode::OK);
}
