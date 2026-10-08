//! Integration round-trip for the folder management surface —
//! `fauna.folders.*` (CRUD + members + devices + schedule + lease) and
//! `fauna.sync.conflicts.{list,report,resolve}`. A behavior-preserving
//! transport migration of the bearer-authed user folder routes
//! (`user_folder_routes` + `lease_routes`); the handlers reuse the same
//! `CacheDb` methods the twins call. These tests exercise the WS-RPC layer:
//! request decode, reply shapes, the `User | Admin` allowlist, the
//! `invalid_request` / `not_found` / `conflict` / `permission_denied`
//! mappings, cross-actor isolation, and replay metadata.
//!
//! Authority for the wire types: `libs/fauna-protocol/src/folders.rs`.
//!
//! Tier: tier_3 (real `fauna-nest` `AppState` + real `CacheDb` — no mocks).

mod common;
use common::dispatch;
use common::encode;

use std::sync::Arc;

use bytes::Bytes;

use fauna_nest::{
    bridge_method_allowlist::{CallerClass, is_permitted},
    db::CacheDb,
    folder_handlers,
    routes::AppState,
    rpc_router::RpcRouter,
    sync_handlers,
};
use fauna_protocol::{
    ByteBuf, decode_strict as decode,
    folders::{
        ConflictCandidate, ConflictReportReply, ConflictReportRequest, ConflictResolveReply,
        ConflictResolveRequest, ConflictsListReply, ConflictsListRequest, FolderCreateReply,
        FolderCreateRequest, FolderDeleteReply, FolderDeleteRequest, FolderDevicesReply,
        FolderDevicesRequest, FolderUpdateReply, FolderUpdateRequest, FoldersListReply,
        FoldersListRequest, LeaseAcquireReply, LeaseAcquireRequest, LeaseReleaseReply,
        LeaseReleaseRequest, MemberRemoveReply, MemberRemoveRequest, MembersListReply,
        MembersListRequest, NestPlacePolicy, PlaceFlags, PlacesSetReply, PlacesSetRequest,
    },
    sync::{SyncChangesListReply, SyncChangesListRequest},
};

const ALL_KINDS: [&str; 13] = [
    "fauna.folders.create",
    "fauna.folders.list",
    "fauna.folders.update",
    "fauna.folders.delete",
    "fauna.folders.devices",
    "fauna.folders.members.list",
    "fauna.folders.members.remove",
    "fauna.folders.places.set",
    "fauna.folders.lease.acquire",
    "fauna.folders.lease.release",
    "fauna.sync.conflicts.list",
    "fauna.sync.conflicts.report",
    "fauna.sync.conflicts.resolve",
];

async fn router_and_state() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    folder_handlers::register_folders_handlers(&mut b);
    (b.build(), state)
}

/// Create a folder named `name` for `actor` via the create kind,
/// returning the new id. The set is born under the client-minted set nonce
/// ([`common::SET_NONCE`]) its writers sign under.
async fn create_set(router: &RpcRouter, state: &Arc<AppState>, actor: [u8; 32], name: &str) -> i64 {
    let reply: FolderCreateReply = decode(
        &dispatch(
            router,
            Arc::clone(state),
            actor,
            "fauna.folders.create",
            encode(&FolderCreateRequest {
                name: name.into(),
                retention_policy: None,
                set_nonce: common::set_nonce_field(),
                ..Default::default()
            }),
        )
        .await
        .expect("create ok"),
    )
    .unwrap();
    reply.id
}

/// Sign a conflict report as the reporting engine does
/// (`ChangeSigner::sign_report`): a RESOLVED report mints a winner head row, so
/// it carries the reporter's writer signature over that row under the set's
/// stored nonce ([`common::SET_NONCE`]) — an unsigned one is refused
/// `signature_required`. An unresolved report mints no row and is left as is.
fn signed_report(
    mut req: ConflictReportRequest,
    kp: &fauna_core::identity::ActorKeypair,
) -> ConflictReportRequest {
    common::direct_signer(kp)
        .sign_report(&mut req, common::SET_NONCE)
        .expect("the fixture report signs");
    req
}

/// Sign a choose-winner resolve of `conflict` (as listed by
/// `fauna.sync.conflicts.list`) as the chooser does
/// (`ChangeSigner::sign_choose_winner`) — over the winner head row the nest
/// mints, under the set's stored nonce ([`common::SET_NONCE`]).
fn signed_choose_winner(
    mut req: ConflictResolveRequest,
    conflict: &fauna_protocol::folders::SyncConflict,
    kp: &fauna_core::identity::ActorKeypair,
) -> ConflictResolveRequest {
    common::direct_signer(kp)
        .sign_choose_winner(&mut req, conflict, common::SET_NONCE)
        .expect("the fixture choose-winner signs");
    req
}

// ── create + list ────────────────────────────────────────────────────────────

#[tokio::test]
async fn create_then_list_round_trip() {
    let (router, state) = router_and_state().await;
    let actor = [11u8; 32];

    let created: FolderCreateReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.folders.create",
            encode(&FolderCreateRequest {
                name: "photos".into(),
                retention_policy: Some(r#"{"max_snapshots":7,"max_age_days":30}"#.into()),
                ..Default::default()
            }),
        )
        .await
        .expect("create ok"),
    )
    .unwrap();
    assert_eq!(created.name, "photos");

    let list: FoldersListReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.folders.list",
            encode(&FoldersListRequest {
                include_shared_with_me: None,
                extra: Default::default(),
            }),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();
    assert_eq!(list.folders.len(), 1);
    let fs = &list.folders[0];
    assert_eq!(fs.id, created.id);
    assert_eq!(fs.name, "photos");
    assert_eq!(
        fs.retention_policy.as_deref(),
        Some(r#"{"max_snapshots":7,"max_age_days":30}"#)
    );
    assert_eq!(fs.include_paths, None, "a create rests no path list");
    assert_eq!(fs.exclude_paths, None, "a create rests no path list");
}

/// Paths are content (`encryption-at-rest.md` § Carve-outs) and their seal is
/// salted by the row id this create mints, so a create carries no path list at
/// all: a client still spelling the retired plaintext pair rides rule-4 `extra`
/// and nothing rests. The lists arrive on the first keyed `fauna.folders.update`.
#[tokio::test]
async fn create_rests_no_plaintext_path_list() {
    let (router, state) = router_and_state().await;
    let actor = [13u8; 32];
    let mut req = FolderCreateRequest {
        name: "photos".into(),
        ..Default::default()
    };
    for key in ["include_paths", "exclude_paths"] {
        req.extra.insert(
            key.into(),
            fauna_protocol::Value::List(vec![fauna_protocol::Value::String(
                "/home/alice/photos".into(),
            )]),
        );
    }
    dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.folders.create",
        encode(&req),
    )
    .await
    .expect("create ok");

    let row = state
        .db
        .get_folder_for_actor("photos", &actor)
        .await
        .unwrap()
        .expect("row");
    assert_eq!(row.include_paths, None);
    assert_eq!(row.exclude_paths, None);
}

/// The keyed create's carriers rest as sent: `name_sealed` and
/// `retention_policy_sealed` verbatim, and the resting `name_hash` is the one the
/// request named (the nest derives it from `name` and refuses a disagreeing one).
#[tokio::test]
async fn keyed_create_rests_its_seals_and_refuses_a_foreign_name_hash() {
    let (router, state) = router_and_state().await;
    let actor = [14u8; 32];
    let hash = fauna_core::path_crypto::set_name_hash("photos");

    let refused = dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.folders.create",
        encode(&FolderCreateRequest {
            name: "photos".into(),
            name_hash: Some(fauna_protocol::ByteBuf::from(
                fauna_core::path_crypto::set_name_hash("other").to_vec(),
            )),
            ..Default::default()
        }),
    )
    .await
    .expect_err("a name_hash that is not the hash of name is refused");
    assert_eq!(refused.code, "fauna.folders.invalid_request");

    dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.folders.create",
        encode(&FolderCreateRequest {
            name: "photos".into(),
            name_hash: Some(fauna_protocol::ByteBuf::from(hash.to_vec())),
            retention_policy: Some(r#"{"max_snapshots":7,"max_age_days":30}"#.into()),
            name_sealed: Some(fauna_protocol::ByteBuf::from(vec![0xa1u8; 24])),
            retention_policy_sealed: Some(fauna_protocol::ByteBuf::from(vec![0xb2u8; 24])),
            ..Default::default()
        }),
    )
    .await
    .expect("create ok");

    // A sealed set rests no plaintext name: it is addressed by its hash only.
    let row = state
        .db
        .get_folder_for_actor_by_name_hash(&hash, &actor)
        .await
        .unwrap()
        .expect("row");
    assert_eq!(row.name, "");
    assert_eq!(row.name_hash.as_deref(), Some(&hash[..]));
    assert_eq!(row.name_sealed.as_deref(), Some(&[0xa1u8; 24][..]));
    assert_eq!(
        row.retention_policy_sealed.as_deref(),
        Some(&[0xb2u8; 24][..])
    );
}

/// A sealed create travels by its hash alone (`path-sealing.md` § the set-name
/// plane): the nest mints the row from `name_hash` + `name_sealed` and never
/// sees the name. It refuses a hash-only create with no seal (nothing would
/// ever name the row), a malformed hash, and a `public` one (the name is the
/// URL segment); a second create of the same hash is the usual `conflict`.
#[tokio::test]
async fn a_sealed_create_by_hash_alone_mints_the_row_and_refuses_the_unnameable() {
    let (router, state) = router_and_state().await;
    let actor = [15u8; 32];
    let hash = fauna_core::path_crypto::set_name_hash("Tax returns 2026");
    let by_hash = |sealed: bool| FolderCreateRequest {
        name: String::new(),
        name_hash: Some(ByteBuf::from(hash.to_vec())),
        name_sealed: sealed.then(|| ByteBuf::from(vec![0xa1u8; 24])),
        ..Default::default()
    };
    let create = |req: FolderCreateRequest| {
        dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.folders.create",
            encode(&req),
        )
    };

    for (why, req) in [
        ("no seal", by_hash(false)),
        (
            "a short hash",
            FolderCreateRequest {
                name_hash: Some(ByteBuf::from(vec![1u8; 16])),
                ..by_hash(true)
            },
        ),
        (
            "a public create",
            FolderCreateRequest {
                audience: Some("public".into()),
                ..by_hash(true)
            },
        ),
    ] {
        let refused = create(req).await.expect_err(why);
        assert_eq!(refused.code, "fauna.folders.invalid_request", "{why}");
    }

    create(by_hash(true))
        .await
        .expect("a sealed create by hash");
    let bytes = folder_row_bytes(&state, &hash).await;
    assert!(bytes.starts_with(b"NULL"), "the row rests no name");
    let row = state
        .db
        .get_folder_for_actor_by_name_hash(&hash, &actor)
        .await
        .unwrap()
        .expect("addressed by its hash");
    assert_eq!(row.name_sealed.as_deref(), Some(&[0xa1u8; 24][..]));

    let again = create(by_hash(true)).await.expect_err("a duplicate");
    assert_eq!(again.code, "fauna.folders.conflict");
}

/// Every byte the `folders` row addressed by `hash` rests in its name and its
/// two companions — the plaintext name rendered by SQL `quote`, so a NULL reads
/// as the four bytes `NULL`.
async fn folder_row_bytes(state: &Arc<AppState>, hash: &[u8; 32]) -> Vec<u8> {
    let conn = state.db.conn().await;
    conn.query_row(
        "SELECT quote(name), name_hash, name_sealed FROM folders WHERE name_hash = ?1",
        [hash.as_slice()],
        |r| {
            let mut bytes: Vec<u8> = r.get::<_, String>(0)?.into_bytes();
            bytes.extend(r.get::<_, Vec<u8>>(1)?);
            bytes.extend(r.get::<_, Option<Vec<u8>>>(2)?.unwrap_or_default());
            Ok(bytes)
        },
    )
    .expect("the row is addressed by its hash")
}

/// A sealed set's name rests only sealed (`path-sealing.md` § the set-name
/// plane, schema 114): after a keyed create and after a settings edit, no byte
/// of the row holds the plaintext name, and the set stays addressable by its
/// hash. A →public flip must carry the name — it becomes the folder's URL
/// segment — and restores it, refusing a name that does not hash to the row;
/// the flip back to private rests it sealed again.
#[tokio::test]
async fn a_sealed_sets_name_rests_only_sealed_and_a_public_flip_restores_it() {
    let (router, state) = router_and_state().await;
    let actor = [31u8; 32];
    let name = "Tax returns 2026";
    let hash = fauna_core::path_crypto::set_name_hash(name);
    let by_hash = || FolderUpdateRequest {
        name: String::new(),
        name_hash: Some(ByteBuf::from(hash.to_vec())),
        ..Default::default()
    };
    let rests_plaintext = |bytes: &[u8]| bytes.windows(name.len()).any(|w| w == name.as_bytes());

    dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.folders.create",
        encode(&FolderCreateRequest {
            name: name.into(),
            name_hash: Some(ByteBuf::from(hash.to_vec())),
            name_sealed: Some(ByteBuf::from(vec![0xa1u8; 24])),
            ..Default::default()
        }),
    )
    .await
    .expect("keyed create ok");
    assert!(
        !rests_plaintext(&folder_row_bytes(&state, &hash).await),
        "create"
    );
    let row = state
        .db
        .get_folder_for_actor_by_name_hash(&hash, &actor)
        .await
        .unwrap()
        .expect("addressed by hash");
    assert_eq!(row.name, "", "the blanked name reads as the empty sentinel");

    dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.folders.update",
        encode(&FolderUpdateRequest {
            conflict_policy: Some("latest_wins_always".into()),
            ..by_hash()
        }),
    )
    .await
    .expect("a settings edit ok");
    assert!(
        !rests_plaintext(&folder_row_bytes(&state, &hash).await),
        "settings edit"
    );

    let refused = dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.folders.update",
        encode(&FolderUpdateRequest {
            audience: Some("public".into()),
            ..by_hash()
        }),
    )
    .await
    .expect_err("a public flip without the name is refused");
    assert_eq!(refused.code, "fauna.folders.invalid_request");
    let refused = dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.folders.update",
        encode(&FolderUpdateRequest {
            name: "Someone else's".into(),
            audience: Some("public".into()),
            ..by_hash()
        }),
    )
    .await
    .expect_err("a public flip naming another set is refused");
    assert_eq!(refused.code, "fauna.folders.invalid_request");

    dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.folders.update",
        encode(&FolderUpdateRequest {
            name: name.into(),
            audience: Some("public".into()),
            ..by_hash()
        }),
    )
    .await
    .expect("a public flip with the name ok");
    assert!(
        rests_plaintext(&folder_row_bytes(&state, &hash).await),
        "a public folder's name is its URL segment"
    );

    dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.folders.update",
        encode(&FolderUpdateRequest {
            audience: Some("private".into()),
            ..by_hash()
        }),
    )
    .await
    .expect("the flip back ok");
    assert!(
        !rests_plaintext(&folder_row_bytes(&state, &hash).await),
        "flip back"
    );
}

/// A folder has no type (`folders.md` § Implementation status today, the mode
/// contraction's design pass): a create that still carries a `mode` key rides
/// rule-4 `extra` and is IGNORED — no refusal arm, since no older client exists.
/// The folder is created, and neither the reply nor the row spells a mode.
#[tokio::test]
async fn create_ignores_a_stray_mode_key() {
    let (router, state) = router_and_state().await;
    let actor = [12u8; 32];
    let mut req = FolderCreateRequest {
        name: "x".into(),
        ..Default::default()
    };
    req.extra
        .insert("mode".into(), fauna_protocol::Value::String("bogus".into()));
    let reply: FolderCreateReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.folders.create",
            encode(&req),
        )
        .await
        .expect("a stray mode key is ignored, never refused"),
    )
    .unwrap();
    assert_eq!(reply.name, "x");
    assert!(
        !reply.extra.contains_key("mode"),
        "the reply spells no mode"
    );
    let fs = state
        .db
        .get_folder_for_actor("x", &actor)
        .await
        .unwrap()
        .expect("the folder was created");
    assert!(!fs.custody_copy, "a client create is never a custody copy");
}

#[tokio::test]
async fn create_duplicate_name_conflict() {
    let (router, state) = router_and_state().await;
    let actor = [13u8; 32];
    create_set(&router, &state, actor, "dup").await;
    let err = dispatch(
        &router,
        state,
        actor,
        "fauna.folders.create",
        encode(&FolderCreateRequest {
            name: "dup".into(),
            retention_policy: None,
            ..Default::default()
        }),
    )
    .await
    .expect_err("dup name → conflict");
    assert_eq!(err.code, "fauna.folders.conflict");
}

// ── update / delete ──────────────────────────────────────────────────────────

#[tokio::test]
async fn update_existing_and_missing() {
    let (router, state) = router_and_state().await;
    let actor = [14u8; 32];
    create_set(&router, &state, actor, "docs").await;

    let ok: FolderUpdateReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.folders.update",
            encode(&FolderUpdateRequest {
                name: "docs".into(),
                retention_policy: None,
                include_paths: Some(vec![".git".into()]),
                include_paths_sealed: Some(ByteBuf::from(vec![0x5au8; 24])),
                exclude_paths: None,
                webdav_enabled: None,
                ..Default::default()
            }),
        )
        .await
        .expect("update ok"),
    )
    .unwrap();
    assert!(ok.ok);

    let err = dispatch(
        &router,
        state,
        actor,
        "fauna.folders.update",
        encode(&FolderUpdateRequest {
            name: "nope".into(),
            retention_policy: None,
            include_paths: None,
            exclude_paths: None,
            webdav_enabled: None,
            ..Default::default()
        }),
    )
    .await
    .expect_err("update missing → not_found");
    assert_eq!(err.code, "fauna.folders.not_found");
}

/// Paths are content (`encryption-at-rest.md` § Carve-outs): a non-empty
/// selective-sync list sent without its seal is refused and rests nothing; an
/// empty list names no path and still clears keyless.
#[tokio::test]
async fn an_unsealed_path_list_is_refused_and_an_empty_one_clears() {
    let (router, state) = router_and_state().await;
    let actor = [16u8; 32];
    create_set(&router, &state, actor, "docs").await;
    let update = |req: FolderUpdateRequest| {
        dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.folders.update",
            encode(&req),
        )
    };

    for req in [
        FolderUpdateRequest {
            name: "docs".into(),
            include_paths: Some(vec!["/home/a/tax".into()]),
            ..Default::default()
        },
        FolderUpdateRequest {
            name: "docs".into(),
            exclude_paths: Some(vec!["/home/a/tax".into()]),
            ..Default::default()
        },
    ] {
        let err = update(req).await.expect_err("an unsealed list");
        assert_eq!(err.code, "fauna.folders.invalid_request");
    }
    let row = state
        .db
        .get_folder_for_actor("docs", &actor)
        .await
        .unwrap()
        .expect("row");
    assert_eq!((row.include_paths, row.exclude_paths), (None, None));

    update(FolderUpdateRequest {
        name: "docs".into(),
        include_paths: Some(vec![]),
        ..Default::default()
    })
    .await
    .expect("an empty list clears keyless");
}

/// S5b (`file-sync.md` § Sealed names & paths): `fauna.folders.update`
/// resolves hash-first. An **empty** plaintext `name` alongside `name_hash`
/// proves the hash did the work — the post-flip call shape, where the client
/// no longer sends a plaintext name at all. This also pins the double-resolve
/// fix: the update mutation itself (not just the webdav validation check)
/// must key off the resolved row, not the empty wire `name`.
#[tokio::test]
async fn update_addresses_by_name_hash_with_empty_plaintext_name() {
    let (router, state) = router_and_state().await;
    let actor = [22u8; 32];
    create_set(&router, &state, actor, "docs").await;
    let hash = fauna_core::path_crypto::set_name_hash("docs");

    let ok: FolderUpdateReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.folders.update",
            encode(&FolderUpdateRequest {
                name: String::new(),
                name_hash: Some(ByteBuf::from(hash.to_vec())),
                conflict_policy: Some("latest_wins_always".into()),
                ..Default::default()
            }),
        )
        .await
        .expect("hash-addressed update ok"),
    )
    .unwrap();
    assert!(ok.ok);

    // The mutation actually landed on the SAME row the hash addresses (not a
    // no-op silently swallowed) — read back by the still-resting plaintext
    // name (dual-write phase).
    let list: FoldersListReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.folders.list",
            encode(&FoldersListRequest {
                include_shared_with_me: None,
                extra: Default::default(),
            }),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();
    assert_eq!(list.folders.len(), 1);
    assert_eq!(
        list.folders[0].conflict_policy.as_deref(),
        Some("latest_wins_always")
    );

    // A malformed (non-32-byte) hash is refused, never silently downgraded to
    // the (here, also-empty) name arm.
    let err = dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.folders.update",
        encode(&FolderUpdateRequest {
            name: String::new(),
            name_hash: Some(ByteBuf::from(vec![1u8, 2, 3])),
            ..Default::default()
        }),
    )
    .await
    .expect_err("a 3-byte hash is not a BLAKE3 digest");
    assert_eq!(err.code, "fauna.folders.invalid_request");
}

/// **The nest place's snapshot policy round-trips, and an unset folder stays
/// invisible on the wire** (folders re-model phase 2 § Places → the nest place;
/// `backup-restore.md` § 8 *The nest place's snapshot policy*).
///
/// Three assertions, and the middle one is the reason this test exists rather
/// than a bare set-and-read:
///
/// 1. A fresh folder projects **no** `nest_place` at all. The field is additive,
///    and a folder whose owner never touched the policy must serialize exactly as
///    it did before the field existed — an always-present `Some(default)` would
///    be a silent wire change for every existing folder and would also claim a
///    policy the folder does not have.
/// 2. Setting it round-trips through `update` → `list`.
/// 3. **Sending the struct with a knob omitted CLEARS that knob** back to unset,
///    rather than leaving the previous value. That is the contract
///    `NestPlacePolicy` documents (sent whole, applied whole) and the only way
///    three states per knob survive a wire that cannot round-trip a nested
///    `Option` — if it silently degraded to "leave unchanged", a user choosing
///    "use the default" would keep the old cadence forever and nothing would say
///    so.
#[tokio::test]
async fn the_nest_place_policy_round_trips_and_is_applied_whole() {
    let (router, state) = router_and_state().await;
    let actor = [37u8; 32];
    create_set(&router, &state, actor, "papers").await;

    async fn nest_place_of(
        router: &RpcRouter,
        state: &Arc<AppState>,
        actor: [u8; 32],
    ) -> Option<NestPlacePolicy> {
        let list: FoldersListReply = decode(
            &dispatch(
                router,
                Arc::clone(state),
                actor,
                "fauna.folders.list",
                encode(&FoldersListRequest {
                    include_shared_with_me: None,
                    extra: Default::default(),
                }),
            )
            .await
            .expect("list ok"),
        )
        .unwrap();
        list.folders
            .iter()
            .find(|f| f.name == "papers")
            .expect("the owner's own set is listed")
            .nest_place
            .clone()
    }

    // 1. Untouched ⇒ absent, not `Some(default)`.
    assert_eq!(
        nest_place_of(&router, &state, actor).await,
        None,
        "a folder whose owner never set a nest-place policy must project no \
         `nest_place` at all — an additive field that always appears is a wire \
         change for every existing folder"
    );

    // 2. Set both knobs → both round-trip.
    let set = |place: NestPlacePolicy| {
        let router = &router;
        let state = Arc::clone(&state);
        async move {
            let reply: FolderUpdateReply = decode(
                &dispatch(
                    router,
                    state,
                    actor,
                    "fauna.folders.update",
                    encode(&FolderUpdateRequest {
                        name: "papers".into(),
                        nest_place: Some(place),
                        ..Default::default()
                    }),
                )
                .await
                .expect("a nest-place update is accepted"),
            )
            .unwrap();
            assert!(reply.ok);
        }
    };

    set(NestPlacePolicy {
        snapshots: Some(false),
        quiet_secs: Some(900),
        extra: Default::default(),
    })
    .await;
    assert_eq!(
        nest_place_of(&router, &state, actor).await,
        Some(NestPlacePolicy {
            snapshots: Some(false),
            quiet_secs: Some(900),
            extra: Default::default(),
        }),
        "both knobs must round-trip through update → list"
    );

    // 3. Re-send with `quiet_secs` omitted ⇒ that knob clears, the other keeps
    //    the value this same request carries. Applied whole, per the contract.
    set(NestPlacePolicy {
        snapshots: Some(false),
        quiet_secs: None,
        extra: Default::default(),
    })
    .await;
    assert_eq!(
        nest_place_of(&router, &state, actor).await,
        Some(NestPlacePolicy {
            snapshots: Some(false),
            quiet_secs: None,
            extra: Default::default(),
        }),
        "the policy is applied WHOLE — a knob omitted from the struct clears back \
         to unset (the nest-wide default), it does not linger"
    );

    // And clearing every knob returns the folder to projecting nothing, which is
    // the same resting state it started in — the round trip closes.
    set(NestPlacePolicy::default()).await;
    assert_eq!(
        nest_place_of(&router, &state, actor).await,
        None,
        "a fully-cleared policy rests unset again and drops off the wire"
    );
}

/// The version-retention bounds (`file-versions.md` § Retention ruling 1) ride
/// `fauna.folders.update` sent-whole/applied-whole like the nest place above,
/// and rest as the canonical JSON — with the binds-nothing policy resting as
/// SQL **NULL** (the honest keep-everything value), never a zero-JSON that
/// would make NotSet two shapes at rest.
#[tokio::test]
async fn version_retention_round_trips_and_a_cleared_policy_rests_null() {
    use fauna_protocol::folders::VersionRetention;

    let (router, state) = router_and_state().await;
    let actor = [39u8; 32];
    create_set(&router, &state, actor, "papers").await;

    async fn version_retention_of(
        router: &RpcRouter,
        state: &Arc<AppState>,
        actor: [u8; 32],
    ) -> Option<VersionRetention> {
        let list: FoldersListReply = decode(
            &dispatch(
                router,
                Arc::clone(state),
                actor,
                "fauna.folders.list",
                encode(&FoldersListRequest {
                    include_shared_with_me: None,
                    extra: Default::default(),
                }),
            )
            .await
            .expect("list ok"),
        )
        .unwrap();
        list.folders
            .iter()
            .find(|f| f.name == "papers")
            .expect("the owner's own set is listed")
            .version_retention
            .clone()
    }

    let set = |vr: VersionRetention| {
        let router = &router;
        let state = Arc::clone(&state);
        async move {
            let reply: FolderUpdateReply = decode(
                &dispatch(
                    router,
                    state,
                    actor,
                    "fauna.folders.update",
                    encode(&FolderUpdateRequest {
                        name: "papers".into(),
                        version_retention: Some(vr),
                        ..Default::default()
                    }),
                )
                .await
                .expect("a version-retention update is accepted"),
            )
            .unwrap();
            assert!(reply.ok);
        }
    };

    // 1. Untouched ⇒ absent, not `Some(default)` — the wire bytes of every
    //    existing folder are unchanged by this field existing.
    assert_eq!(version_retention_of(&router, &state, actor).await, None);

    // 2. Set → the typed bounds round-trip through update → list.
    set(VersionRetention {
        max_versions_per_path: 5,
        max_age_days: 30,
        extra: Default::default(),
    })
    .await;
    assert_eq!(
        version_retention_of(&router, &state, actor).await,
        Some(VersionRetention {
            max_versions_per_path: 5,
            max_age_days: 30,
            extra: Default::default(),
        })
    );
    // At rest: the canonical JSON the pruner parses.
    let fs = state
        .db
        .get_folder_for_actor("papers", &actor)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        fs.version_retention.as_deref(),
        Some(r#"{"max_versions_per_path":5,"max_age_days":30}"#)
    );

    // 3. The binds-nothing policy (the editor's "clear" gesture) rests as NULL
    //    and drops off the wire — keep-everything has ONE shape at rest.
    set(VersionRetention::default()).await;
    assert_eq!(version_retention_of(&router, &state, actor).await, None);
    let fs = state
        .db
        .get_folder_for_actor("papers", &actor)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        fs.version_retention, None,
        "a cleared policy rests as SQL NULL, never a zero-JSON"
    );
}

/// A negative quiet period is **refused**, never normalized: clamping it to 0
/// would quietly turn "wait for quiet" into "cut on every scheduler tick", and
/// the caller would never learn its request was rewritten.
#[tokio::test]
async fn a_negative_nest_place_quiet_period_is_refused() {
    let (router, state) = router_and_state().await;
    let actor = [38u8; 32];
    create_set(&router, &state, actor, "papers").await;

    let err = dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.folders.update",
        encode(&FolderUpdateRequest {
            name: "papers".into(),
            nest_place: Some(NestPlacePolicy {
                quiet_secs: Some(-1),
                ..Default::default()
            }),
            ..Default::default()
        }),
    )
    .await
    .expect_err("a negative quiet period is not a cadence");
    assert_eq!(err.code, "fauna.folders.bad_request");

    // And nothing was written on the way to the refusal.
    let list: FoldersListReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.folders.list",
            encode(&FoldersListRequest {
                include_shared_with_me: None,
                extra: Default::default(),
            }),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();
    assert_eq!(
        list.folders
            .iter()
            .find(|f| f.name == "papers")
            .expect("the set is listed")
            .nest_place,
        None,
        "a refused update must leave the policy untouched"
    );
}

/// A quiet period over the ceiling is refused like a negative one
/// (2026-10-08): an unbounded value let one account's own folder panic the nest-wide
/// snapshot scheduler. The ceiling itself is still accepted.
#[tokio::test]
async fn a_nest_place_quiet_period_over_the_ceiling_is_refused() {
    let (router, state) = router_and_state().await;
    let actor = [39u8; 32];
    create_set(&router, &state, actor, "papers").await;

    let update = |quiet_secs: i64| {
        encode(&FolderUpdateRequest {
            name: "papers".into(),
            nest_place: Some(NestPlacePolicy {
                quiet_secs: Some(quiet_secs),
                ..Default::default()
            }),
            ..Default::default()
        })
    };

    for over in [NestPlacePolicy::MAX_QUIET_SECS + 1, i64::MAX] {
        let err = dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.folders.update",
            update(over),
        )
        .await
        .expect_err("a quiet period past the ceiling is not a cadence");
        assert_eq!(err.code, "fauna.folders.bad_request");
    }

    dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.folders.update",
        update(NestPlacePolicy::MAX_QUIET_SECS),
    )
    .await
    .expect("the ceiling itself is a valid quiet period");
}

/// The per-set WebDAV serve flag (`folders.webdav_enabled`) round-trips through
/// `fauna.folders.update` and is projected on the owner's list row. Every
/// non-reserved folder may be served — a folder has no type, so the former
/// sync-type gate retired with the mode (`webdav-server.md` § What the namespace
/// is) — and a reserved `__` set never is (the update refuses it whole).
#[tokio::test]
async fn webdav_serve_flag_serves_every_non_reserved_folder() {
    let (router, state) = router_and_state().await;
    let actor = [21u8; 32];
    create_set(&router, &state, actor, "docs").await;

    // Flag a folder for serving → accepted.
    let ok: FolderUpdateReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.folders.update",
            encode(&FolderUpdateRequest {
                name: "docs".into(),
                retention_policy: None,
                include_paths: None,
                exclude_paths: None,
                webdav_enabled: Some(true),
                ..Default::default()
            }),
        )
        .await
        .expect("serve-on for a folder is accepted"),
    )
    .unwrap();
    assert!(ok.ok);

    // The owner's list row projects the flag (owner_summary).
    let list: FoldersListReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.folders.list",
            encode(&FoldersListRequest {
                include_shared_with_me: None,
                extra: Default::default(),
            }),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();
    let docs = list
        .folders
        .iter()
        .find(|s| s.name == "docs")
        .expect("docs row present");
    assert!(
        docs.webdav_enabled,
        "the flagged set projects webdav_enabled"
    );

    // Any other ordinary folder is servable the same way…
    create_set(&router, &state, actor, "vault").await;
    let ok: FolderUpdateReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.folders.update",
            encode(&FolderUpdateRequest {
                name: "vault".into(),
                webdav_enabled: Some(true),
                ..Default::default()
            }),
        )
        .await
        .expect("every non-reserved folder is servable"),
    )
    .unwrap();
    assert!(ok.ok);

    // …while a reserved rail is never served.
    state
        .db
        .get_or_create_reserved_folder(&actor, "mail")
        .await
        .expect("a rail mints");
    let err = dispatch(
        &router,
        state,
        actor,
        "fauna.folders.update",
        encode(&FolderUpdateRequest {
            name: "__mail".into(),
            webdav_enabled: Some(true),
            ..Default::default()
        }),
    )
    .await
    .expect_err("a reserved rail is never served");
    assert_eq!(err.code, "fauna.folders.invalid_request");
}

#[tokio::test]
async fn delete_then_not_found() {
    let (router, state) = router_and_state().await;
    let actor = [15u8; 32];
    create_set(&router, &state, actor, "trash").await;

    let ok: FolderDeleteReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.folders.delete",
            encode(&FolderDeleteRequest {
                name: "trash".into(),
                ..Default::default()
            }),
        )
        .await
        .expect("delete ok"),
    )
    .unwrap();
    assert!(ok.ok);

    let err = dispatch(
        &router,
        state,
        actor,
        "fauna.folders.delete",
        encode(&FolderDeleteRequest {
            name: "trash".into(),
            ..Default::default()
        }),
    )
    .await
    .expect_err("re-delete → not_found");
    assert_eq!(err.code, "fauna.folders.not_found");
}

/// A folder that has a snapshot (plus a member and a `parent_id` snapshot
/// chain) must still be deletable. The bare `DELETE FROM folders` used to hit
/// a FOREIGN KEY constraint violation on the non-cascading `snapshots` /
/// `folder_members` children (and the `snapshots.parent_id` self-FK) and
/// surface to the client as `fauna.folders.internal`). (The former
/// `folder_destinations` FK died with the phantom rail, 2026-08-18.)
#[tokio::test]
async fn delete_set_with_snapshot_and_children() {
    let (router, state) = router_and_state().await;
    let actor = [0x5e_u8; 32];
    let fs_id = create_set(&router, &state, actor, "docs").await;

    // A registered device, added as a member, exercises the folder_members FK.
    let device = [0xcd_u8; 32];
    state
        .db
        .register_sync_device(&actor, &device, "laptop", None, "sync")
        .await
        .unwrap();
    state
        .db
        .add_folder_member(fs_id, &device, &PlaceFlags::default_place())
        .await
        .unwrap();
    // Two snapshots in a parent→child chain (distinct created_at to dodge the
    // UNIQUE(folder_id, created_at) constraint) exercise the snapshots FK and
    // the snapshots.parent_id self-FK.
    let s1 = state.db.insert_snapshot_at(fs_id, 1_000).await.unwrap();
    let s2 = state.db.insert_snapshot_at(fs_id, 2_000).await.unwrap();
    state.db.set_snapshot_parent(s2, s1).await.unwrap();
    assert_eq!(state.db.list_snapshots(fs_id).await.unwrap().len(), 2);

    // Delete must succeed despite the children.
    let ok: FolderDeleteReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.folders.delete",
            encode(&FolderDeleteRequest {
                name: "docs".into(),
                ..Default::default()
            }),
        )
        .await
        .expect("delete of a set with a snapshot must succeed"),
    )
    .unwrap();
    assert!(ok.ok);

    // The set and all of its snapshot rows are gone.
    assert!(
        state
            .db
            .get_folder_for_actor("docs", &actor)
            .await
            .unwrap()
            .is_none(),
        "folder row must be deleted"
    );
    assert!(
        state.db.list_snapshots(fs_id).await.unwrap().is_empty(),
        "snapshot rows must be deleted with the set"
    );
    assert!(
        state.db.get_folder_members(fs_id).await.unwrap().is_empty(),
        "member rows must be deleted with the set"
    );
}

/// The `__mls` replica path the reserved-rail tests seed through the
/// production `fauna.mls.put` door.
const RAIL_PATH: &str = "provider";

/// `fauna.folders.delete` refuses the caller's own reserved rails.
/// The property is SURVIVAL, not the error code: the `__mls` replica's sealed
/// blob is reachable only through its `sync_changes` row (`db/mls_replica.rs`),
/// and the material in it — the device's MLS state and own-message history — is
/// irrecoverable, so the row must still be there afterwards
/// (`principles.md` § No user-data loss; `nest/common.md` § Client-state
/// recoverability). The guard-keys-on-the-RESOLVED-name half (a hash-addressed
/// rail with an empty `req.name`) is pinned in-crate
/// (`folder_handlers::tests::delete_guard_keys_on_the_resolved_name_not_req_name`):
/// arranging it needs the booted-nest shape (the boot hash-companion pass stamps
/// `name_hash` on rails; a fresh mint leaves it NULL), which integration tests
/// — built without `cfg(test)` — cannot set up.
#[tokio::test]
async fn delete_refuses_the_callers_own_mls_rail_and_the_rail_survives() {
    let (router, state) = router_and_state().await;
    let actor = [0x7a_u8; 32];

    // Seed a GENUINE rail through the production `fauna.mls.put` path: mints
    // `__mls` + appends the replica path's sync_changes row.
    let manifest = [0xab_u8; 32];
    assert!(
        state
            .db
            .cas_record_mls_replica_blob_change(&actor, RAIL_PATH, None, &manifest, 512)
            .await
            .unwrap()
    );
    let head_before = state
        .db
        .get_mls_replica_hash(&actor, RAIL_PATH)
        .await
        .unwrap();
    assert!(head_before.is_some(), "the rail's sync_changes row exists");

    let err = dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.folders.delete",
        encode(&FolderDeleteRequest {
            name: "__mls".into(),
            ..Default::default()
        }),
    )
    .await
    .expect_err("a reserved rail must not be deletable (name-addressed)");
    assert_eq!(err.code, "fauna.folders.invalid_request");

    // THE property: the rail row and its sync_changes row both survived.
    assert!(
        state
            .db
            .get_folder_for_actor("__mls", &actor)
            .await
            .unwrap()
            .is_some(),
        "the __mls rail row must survive the delete attempt"
    );
    assert_eq!(
        state
            .db
            .get_mls_replica_hash(&actor, RAIL_PATH)
            .await
            .unwrap(),
        head_before,
        "the rail's sync_changes row must survive the delete attempt"
    );

    // Control: a non-reserved set still deletes fine through the same kind.
    create_set(&router, &state, actor, "docs").await;
    let ok: FolderDeleteReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.folders.delete",
            encode(&FolderDeleteRequest {
                name: "docs".into(),
                ..Default::default()
            }),
        )
        .await
        .expect("a non-reserved set still deletes"),
    )
    .unwrap();
    assert!(ok.ok);
}

/// Mint `name` for `actor` the way the two nest-side provisioners do
/// (`federation_handlers::resolve_backup_custody_set`,
/// `sync_handlers::writable_or_provisioned_backup_set`): a **custody copy**,
/// the one row shape no client can create (`reserved-folders.md`
/// § Destination capability).
async fn provision_custody_copy(state: &Arc<AppState>, actor: &[u8; 32], name: &str) -> i64 {
    state
        .db
        .create_folder_with_options(
            name,
            actor,
            fauna_nest::db::FolderOptions {
                custody_copy: true,
                ..Default::default()
            },
        )
        .await
        .expect("the provisioner mints the custody copy")
}

/// The management surface refuses the reserved namespace WHOLE on create
/// (`reserved-folders.md` § The management surface refuses the namespace —
/// whole): a custody copy is minted by the nest's own provisioners, so no
/// client create — whatever else it carries — may name a `__` set.
#[tokio::test]
async fn create_refuses_every_reserved_name() {
    let (router, state) = router_and_state().await;
    let actor = [0x7a_u8; 32];
    for name in ["__mail", "__config", "__conv/abcd"] {
        let err = dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.folders.create",
            encode(&FolderCreateRequest {
                name: name.into(),
                ..Default::default()
            }),
        )
        .await
        .expect_err("a reserved name is the nest's, never a client create's");
        assert_eq!(err.code, "fauna.folders.invalid_request", "{name}");
        assert!(
            state
                .db
                .get_folder_for_actor(name, &actor)
                .await
                .unwrap()
                .is_none(),
            "{name}: nothing was created"
        );
    }
}

/// The carve-out the delete guard must NOT close: a custody copy provisioned on
/// a DESTINATION nest, and an owner-authenticated delete of it IS the
/// documented owner-side destination-removal teardown (`backups.md` /
/// `backup-destinations.md` § Destination-removal / supersede handshake).
#[tokio::test]
async fn delete_still_allows_a_custody_copy() {
    let (router, state) = router_and_state().await;
    let actor = [0x7b_u8; 32];
    provision_custody_copy(&state, &actor, "__mail").await;

    let ok: FolderDeleteReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.folders.delete",
            encode(&FolderDeleteRequest {
                name: "__mail".into(),
                ..Default::default()
            }),
        )
        .await
        .expect("the destination-removal handshake must stay deletable"),
    )
    .unwrap();
    assert!(ok.ok);
    assert!(
        state
            .db
            .get_folder_for_actor("__mail", &actor)
            .await
            .unwrap()
            .is_none(),
        "the custody copy is gone — chunks reclaim on the next GC"
    );
}

/// The rail mint refuses to adopt a custody copy, always — it never re-classes
/// one (`reserved-folders.md` § Destination capability: the role holding live
/// state wins, the newcomer is refused). Re-classing would drop the copy out of
/// the GC's manifest-class walk and reclaim a live offsite backup. Reachable
/// only where one nest is both a home and a backup destination for the same
/// actor, which the enroll flow does not produce.
#[tokio::test]
async fn the_rail_mint_refuses_to_adopt_a_custody_copy() {
    let (_router, state) = router_and_state().await;
    let actor = [0x8a_u8; 32];
    provision_custody_copy(&state, &actor, "__mail").await;

    let err = state
        .db
        .get_or_create_reserved_folder(&actor, "mail")
        .await
        .expect_err("the rail mint must refuse a custody copy, not adopt it");
    assert!(
        err.to_string().contains("custody copy"),
        "the refusal must name the collision, got: {err}"
    );

    // THE property: the custody copy is untouched, so the GC's manifest-class
    // walk still sees it and its offsite chunks stay live.
    let fs = state
        .db
        .get_folder_for_actor("__mail", &actor)
        .await
        .unwrap()
        .expect("the custody copy survives");
    assert!(fs.custody_copy, "a custody copy keeps its class");
    assert!(
        state
            .db
            .is_pure_backup_destination("mail", &actor)
            .await
            .unwrap(),
        "the custody copy still reads as a pure-backup destination"
    );

    // …while a rail the mint makes itself is a rail: not a pure-backup
    // destination, and never deletable through the user kind.
    let other = [0x8b_u8; 32];
    state
        .db
        .get_or_create_reserved_folder(&other, "mail")
        .await
        .expect("a fresh rail mints");
    assert!(
        !state
            .db
            .is_pure_backup_destination("mail", &other)
            .await
            .unwrap()
    );
}

/// `update` refuses a reserved RESOLVED name whole (`reserved-folders.md`
/// § The management surface refuses the namespace — whole): no field of a
/// reserved set is a client's to set — a rail's serve flag, its policy, its
/// seal — and a custody copy's no more than a rail's. The rail survives the
/// attempted update→delete journey intact.
#[tokio::test]
async fn update_refuses_a_reserved_name_whole() {
    let (router, state) = router_and_state().await;
    let actor = [0x7c_u8; 32];

    let manifest = [0xcd_u8; 32];
    assert!(
        state
            .db
            .cas_record_mls_replica_blob_change(&actor, RAIL_PATH, None, &manifest, 256)
            .await
            .unwrap()
    );
    let head_before = state
        .db
        .get_mls_replica_hash(&actor, RAIL_PATH)
        .await
        .unwrap();

    for req in [
        FolderUpdateRequest {
            name: "__mls".into(),
            webdav_enabled: Some(true),
            ..Default::default()
        },
        FolderUpdateRequest {
            name: "__mls".into(),
            conflict_policy: Some("latest_wins_always".into()),
            ..Default::default()
        },
        // An empty update is refused too: the namespace, not the field, decides.
        FolderUpdateRequest {
            name: "__mls".into(),
            ..Default::default()
        },
    ] {
        let err = dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.folders.update",
            encode(&req),
        )
        .await
        .expect_err("no field of a reserved set is a client's to set");
        assert_eq!(err.code, "fauna.folders.invalid_request");
    }

    // A custody copy is refused the same way.
    provision_custody_copy(&state, &actor, "__mail").await;
    let err = dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.folders.update",
        encode(&FolderUpdateRequest {
            name: "__mail".into(),
            include_paths: Some(vec!["/x".into()]),
            ..Default::default()
        }),
    )
    .await
    .expect_err("a custody copy is the nest's too");
    assert_eq!(err.code, "fauna.folders.invalid_request");

    // The delete still sees a rail and refuses; the rail's row is intact.
    let err = dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.folders.delete",
        encode(&FolderDeleteRequest {
            name: "__mls".into(),
            ..Default::default()
        }),
    )
    .await
    .expect_err("a rail is undeletable");
    assert_eq!(err.code, "fauna.folders.invalid_request");
    assert_eq!(
        state
            .db
            .get_mls_replica_hash(&actor, RAIL_PATH)
            .await
            .unwrap(),
        head_before,
        "the journey left the rail's sync_changes row intact"
    );
}

// ── members + devices ─────────────────────────────────────────────────────────

#[tokio::test]
async fn places_set_list_remove() {
    let (router, state) = router_and_state().await;
    let actor = [16u8; 32];
    create_set(&router, &state, actor, "shared").await;
    // set_folder_place_flags requires the device registered to the actor.
    let device = [0xab_u8; 32];
    state
        .db
        .register_sync_device(&actor, &device, "laptop", None, "sync")
        .await
        .unwrap();
    let device_hex = hex::encode(device);

    let set: PlacesSetReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.folders.places.set",
            encode(&PlacesSetRequest {
                name: "shared".into(),
                device_id: device_hex.clone(),
                flags: PlaceFlags::default_place(),
                ..Default::default()
            }),
        )
        .await
        .expect("places.set ok"),
    )
    .unwrap();
    assert!(set.ok);
    assert_eq!(set.device_id, device_hex);

    let members: MembersListReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.folders.members.list",
            encode(&MembersListRequest {
                name: "shared".into(),
                ..Default::default()
            }),
        )
        .await
        .expect("list members ok"),
    )
    .unwrap();
    assert_eq!(members.members.len(), 1);
    assert_eq!(members.members[0].device_id, device_hex);
    assert_eq!(members.members[0].flags, PlaceFlags::default_place());

    let removed: MemberRemoveReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.folders.members.remove",
            // Hash-addressed alone (S5b): the nest resolves the owner's set
            // without the plaintext name.
            encode(&MemberRemoveRequest {
                device_id: device_hex.clone(),
                name_hash: Some(fauna_protocol::ByteBuf::from(
                    fauna_core::path_crypto::set_name_hash("shared").to_vec(),
                )),
                ..Default::default()
            }),
        )
        .await
        .expect("remove ok"),
    )
    .unwrap();
    assert!(removed.ok);

    let err = dispatch(
        &router,
        state,
        actor,
        "fauna.folders.members.remove",
        encode(&MemberRemoveRequest {
            name: "shared".into(),
            device_id: device_hex,
            ..Default::default()
        }),
    )
    .await
    .expect_err("re-remove → not_found");
    assert_eq!(err.code, "fauna.folders.not_found");
}

/// `places.set` rewrites a seat whole, end to end: the flow (folders re-model
/// § Places) is client sends `fauna.folders.places.set` with flags →
/// `places_set_handler` resolves the folder for the actor and calls
/// `set_folder_place_flags` → `upsert_folder_place` writes the three flag
/// columns → `members.list` projects the row onto `FolderMember.flags`. A
/// re-set replaces the point, never adds a second seat.
#[tokio::test]
async fn places_set_rewrites_a_seat_whole() {
    let (router, state) = router_and_state().await;
    let actor = [23u8; 32];
    create_set(&router, &state, actor, "places").await;
    let device = [0xcd_u8; 32];
    state
        .db
        .register_sync_device(&actor, &device, "laptop", None, "sync")
        .await
        .unwrap();
    let device_hex = hex::encode(device);

    let archive = PlaceFlags::archive_place();
    let set: PlacesSetReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.folders.places.set",
            encode(&PlacesSetRequest {
                name: "places".into(),
                device_id: device_hex.clone(),
                flags: archive.clone(),
                ..Default::default()
            }),
        )
        .await
        .expect("places.set ok"),
    )
    .unwrap();
    assert!(set.ok);
    assert_eq!(set.flags, archive);
    let members = roster(&router, &state, actor, "places").await.members;
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].device_id, device_hex);
    assert_eq!(members[0].flags, archive, "the roster carries what was set");

    dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.folders.places.set",
        encode(&PlacesSetRequest {
            name: "places".into(),
            device_id: device_hex.clone(),
            flags: PlaceFlags::default_place(),
            ..Default::default()
        }),
    )
    .await
    .expect("re-set ok");
    let members = roster(&router, &state, actor, "places").await.members;
    assert_eq!(members.len(), 1, "still one seat, not two");
    assert_eq!(members[0].flags, PlaceFlags::default_place());
}

/// `FoldersClient` over this file's in-process router — the shared client
/// helper runs its real read-then-write against the real handlers, with no
/// socket in between (the dispatch door every test above uses).
struct RouterRequester<'a> {
    router: &'a RpcRouter,
    state: &'a Arc<AppState>,
    actor: [u8; 32],
}

impl fauna_protocol::requester::RpcRequester for RouterRequester<'_> {
    type Error = String;

    async fn request<Req, Reply>(&self, kind: &'static str, payload: Req) -> Result<Reply, String>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        let bytes = dispatch(
            self.router,
            Arc::clone(self.state),
            self.actor,
            kind,
            encode(&payload),
        )
        .await
        .map_err(|e| e.code.clone())?;
        decode(&bytes).map_err(|e| e.to_string())
    }
}

async fn roster(
    router: &RpcRouter,
    state: &Arc<AppState>,
    actor: [u8; 32],
    name: &str,
) -> MembersListReply {
    decode(
        &dispatch(
            router,
            Arc::clone(state),
            actor,
            "fauna.folders.members.list",
            encode(&MembersListRequest {
                name: name.into(),
                ..Default::default()
            }),
        )
        .await
        .expect("list members ok"),
    )
    .unwrap()
}

/// **A local presence writes the place it needs** (`file-sync.md` § 4): the
/// shared `FoldersClient::ensure_place` every bind path calls enrols a
/// place-less device at the default point, and a second call writes nothing.
///
/// Flow: the bind path calls `ensure_place` → `members.list` reports no row for
/// this device → `places.set` with `PlaceFlags::default_place()` →
/// `set_folder_place_flags` writes the row → `members.list` now reports exactly
/// that one seat at the default point; the repeat sees the row and sends
/// nothing.
#[tokio::test]
async fn ensure_place_enrols_a_placeless_device_once_at_the_default_point() {
    let (router, state) = router_and_state().await;
    let actor = [24u8; 32];
    create_set(&router, &state, actor, "bound").await;
    let device = [0xce_u8; 32];
    state
        .db
        .register_sync_device(&actor, &device, "laptop", None, "sync")
        .await
        .unwrap();
    let device_hex = hex::encode(device);
    let client = fauna_client_folders::FoldersClient::new(RouterRequester {
        router: &router,
        state: &state,
        actor,
    });
    assert!(
        roster(&router, &state, actor, "bound")
            .await
            .members
            .is_empty(),
        "precondition: the device holds no place"
    );

    let first = client.ensure_place("bound", &device_hex).await.unwrap();
    assert_eq!(first, fauna_client_folders::EnsurePlaceOutcome::Enrolled);
    let members = roster(&router, &state, actor, "bound").await.members;
    assert_eq!(members.len(), 1, "exactly one seat: {members:?}");
    assert_eq!(members[0].device_id, device_hex);
    assert_eq!(members[0].flags, PlaceFlags::default_place());

    let second = client
        .ensure_place("bound", &device_hex.to_uppercase())
        .await
        .unwrap();
    assert_eq!(
        second,
        fauna_client_folders::EnsurePlaceOutcome::AlreadyPlaced,
        "a re-bind writes nothing, whatever case the device id arrives in"
    );
    assert_eq!(
        roster(&router, &state, actor, "bound").await.members,
        members
    );
}

/// `ensure_place` never rewrites a place the device already holds: a seat the
/// user set to the archive point stays the archive point through a re-bind —
/// the enrol fills a gap, it is not a reset.
#[tokio::test]
async fn ensure_place_leaves_an_existing_place_alone() {
    let (router, state) = router_and_state().await;
    let actor = [25u8; 32];
    create_set(&router, &state, actor, "kept").await;
    let device = [0xcf_u8; 32];
    state
        .db
        .register_sync_device(&actor, &device, "nas", None, "sync")
        .await
        .unwrap();
    let device_hex = hex::encode(device);
    let archive = PlaceFlags::archive_place();
    let client = fauna_client_folders::FoldersClient::new(RouterRequester {
        router: &router,
        state: &state,
        actor,
    });
    client
        .places_set(PlacesSetRequest {
            name: "kept".into(),
            device_id: device_hex.clone(),
            flags: archive.clone(),
            ..Default::default()
        })
        .await
        .unwrap();

    let outcome = client.ensure_place("kept", &device_hex).await.unwrap();
    assert_eq!(
        outcome,
        fauna_client_folders::EnsurePlaceOutcome::AlreadyPlaced
    );
    let members = roster(&router, &state, actor, "kept").await.members;
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].flags, archive, "the archive seat survives");
}
/// Every one of the eight flag points is stored and projected exactly as sent
/// — never rounded to a neighbouring point, which would hand the seat
/// behavior nobody asked for in a space where one direction deletes files.
#[tokio::test]
async fn places_set_stores_every_flag_point_exactly() {
    let (router, state) = router_and_state().await;
    let actor = [24u8; 32];
    create_set(&router, &state, actor, "points").await;
    let device = [0xce_u8; 32];
    state
        .db
        .register_sync_device(&actor, &device, "laptop", None, "sync")
        .await
        .unwrap();
    for o in [false, true] {
        for a in [false, true] {
            for d in [false, true] {
                let point = PlaceFlags::new(o, a, d);
                let reply: PlacesSetReply = decode(
                    &dispatch(
                        &router,
                        Arc::clone(&state),
                        actor,
                        "fauna.folders.places.set",
                        encode(&PlacesSetRequest {
                            name: "points".into(),
                            device_id: hex::encode(device),
                            flags: point.clone(),
                            ..Default::default()
                        }),
                    )
                    .await
                    .expect("every point is writable"),
                )
                .unwrap();
                assert_eq!(reply.flags, point);
                let members = roster(&router, &state, actor, "points").await.members;
                assert_eq!(members.len(), 1);
                assert_eq!(members[0].flags, point, "({o}, {a}, {d}) was rounded");
            }
        }
    }
}

/// The role-speaking `members.add` door retired with the role contraction —
/// the kind is no longer routed — and `places.set` refuses a malformed device
/// id.
#[tokio::test]
async fn members_add_is_gone_and_places_set_rejects_bad_hex() {
    let (router, state) = router_and_state().await;
    let actor = [17u8; 32];
    create_set(&router, &state, actor, "s").await;

    assert!(
        router.kind_meta("fauna.folders.members.add").is_none(),
        "the retired door is not routed"
    );

    let bad_hex = dispatch(
        &router,
        state,
        actor,
        "fauna.folders.places.set",
        encode(&PlacesSetRequest {
            name: "s".into(),
            device_id: "zz".into(),
            flags: PlaceFlags::default_place(),
            ..Default::default()
        }),
    )
    .await
    .expect_err("bad hex");
    assert_eq!(bad_hex.code, "fauna.folders.invalid_request");
}

#[tokio::test]
async fn devices_empty_and_missing() {
    let (router, state) = router_and_state().await;
    let actor = [18u8; 32];
    create_set(&router, &state, actor, "empty").await;

    let devices: FolderDevicesReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.folders.devices",
            encode(&FolderDevicesRequest {
                name: "empty".into(),
                ..Default::default()
            }),
        )
        .await
        .expect("devices ok"),
    )
    .unwrap();
    assert!(devices.devices.is_empty());

    let err = dispatch(
        &router,
        state,
        actor,
        "fauna.folders.devices",
        encode(&FolderDevicesRequest {
            name: "nope".into(),
            ..Default::default()
        }),
    )
    .await
    .expect_err("missing set → not_found");
    assert_eq!(err.code, "fauna.folders.not_found");
}

// ── lease ──────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn lease_acquire_conflict_release() {
    let (router, state) = router_and_state().await;
    let actor = [20u8; 32];
    create_set(&router, &state, actor, "locked").await;
    let dev_a = "aa".repeat(32);
    let dev_b = "bb".repeat(32);

    let acq: LeaseAcquireReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.folders.lease.acquire",
            encode(&LeaseAcquireRequest {
                name: "locked".into(),
                device_id: dev_a,
                ..Default::default()
            }),
        )
        .await
        .expect("acquire ok"),
    )
    .unwrap();
    assert!(acq.acquired);

    // A different device cannot acquire while the first holds it.
    let conflict = dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.folders.lease.acquire",
        encode(&LeaseAcquireRequest {
            name: "locked".into(),
            device_id: dev_b,
            ..Default::default()
        }),
    )
    .await
    .expect_err("second device → conflict");
    assert_eq!(conflict.code, "fauna.folders.conflict");

    let rel: LeaseReleaseReply = decode(
        &dispatch(
            &router,
            state,
            actor,
            "fauna.folders.lease.release",
            encode(&LeaseReleaseRequest {
                name: "locked".into(),
                device_id: "aa".repeat(32),
                ..Default::default()
            }),
        )
        .await
        .expect("release ok"),
    )
    .unwrap();
    assert!(rel.released);
}

#[tokio::test]
async fn lease_cross_actor_folds_to_not_found() {
    // ST-RES-1: the owner-scoped lookup folds non-owner and
    // absent to ONE `not_found`, so a cross-actor probe can't distinguish
    // "exists but not mine" from "doesn't exist" (no name-existence oracle).
    let (router, state) = router_and_state().await;
    let owner = [21u8; 32];
    let attacker = [22u8; 32];
    create_set(&router, &state, owner, "mine").await;

    let err = dispatch(
        &router,
        Arc::clone(&state),
        attacker,
        "fauna.folders.lease.acquire",
        encode(&LeaseAcquireRequest {
            name: "mine".into(),
            device_id: "cc".repeat(32),
            ..Default::default()
        }),
    )
    .await
    .expect_err("cross-actor lease → not_found (ST-RES-1 fold)");
    assert_eq!(err.code, "fauna.folders.not_found");
}

// ── conflicts ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn conflicts_report_list_resolve() {
    let (router, state) = router_and_state().await;
    let actor = [23u8; 32];
    create_set(&router, &state, actor, "cf").await;

    let report: ConflictReportReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.sync.conflicts.report",
            encode(&ConflictReportRequest {
                path_sealed: Some(fauna_protocol::ByteBuf::from(
                    b"e2e-synthetic-seal".to_vec(),
                )),
                folder: "cf".into(),
                device_id: "dd".repeat(32),
                path: "/cf/a.txt".into(),
                conflict_type: "concurrent_edit".into(),
                details: Some("two devices".into()),
                candidates: Default::default(),
                ..Default::default()
            }),
        )
        .await
        .expect("report ok"),
    )
    .unwrap();

    let list: ConflictsListReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.sync.conflicts.list",
            encode(&ConflictsListRequest {
                ..Default::default()
            }),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();
    assert_eq!(list.conflicts.len(), 1);
    assert_eq!(list.conflicts[0].id, report.id);
    assert_eq!(list.conflicts[0].folder, "cf");
    // S9 flip: the conflict's plaintext rests '' — hash + seal address it.
    assert_eq!(list.conflicts[0].path, "");

    let resolved: ConflictResolveReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.sync.conflicts.resolve",
            encode(&ConflictResolveRequest {
                id: report.id,
                winning_manifest_hash: None,
                extra: Default::default(),
                winner_signature: None,
                winner_signer_key: None,
            }),
        )
        .await
        .expect("resolve ok"),
    )
    .unwrap();
    assert!(resolved.resolved);

    let err = dispatch(
        &router,
        state,
        actor,
        "fauna.sync.conflicts.resolve",
        encode(&ConflictResolveRequest {
            id: report.id,
            winning_manifest_hash: None,
            extra: Default::default(),
            winner_signature: None,
            winner_signer_key: None,
        }),
    )
    .await
    .expect_err("re-resolve → not_found");
    assert_eq!(err.code, "fauna.sync.not_found");
}

/// A RESOLVED report is a change-recording rail, so it must fire the nudge.
///
/// `report_conflict` lands the loser's retention row and the winner head row
/// "every device converges on via normal catch-up" — but until 2026-08-01
/// nothing told the other devices to catch up, so they waited out the rescan
/// interval (300 s by default) with the merged result already on the nest. That
/// is the same gap `notify_sync_changed` closed for `record_change_core`: the
/// rails must agree. Measured by the two-seat twin's leg 4 — the seat that
/// auto-merged pushed the merged head and its peer never heard.
#[tokio::test]
async fn a_resolved_conflict_report_nudges_the_other_devices() {
    let (router, state) = router_and_state().await;
    let kp = common::signing_actor(24);
    let actor = kp.actor_id().0;
    create_set(&router, &state, actor, "cfn").await;

    let (_conn, mut rx) = state.ws.subscribe(actor);

    let winner = "ab".repeat(32);
    dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.sync.conflicts.report",
        encode(&signed_report(
            ConflictReportRequest {
                path_sealed: Some(fauna_protocol::ByteBuf::from(
                    b"e2e-synthetic-seal".to_vec(),
                )),
                folder: "cfn".into(),
                device_id: "dd".repeat(32),
                path: "/cfn/merged.txt".into(),
                conflict_type: "concurrent_edit".into(),
                details: Some("auto-resolved: clean three-way merge".into()),
                candidates: vec![ConflictCandidate {
                    manifest_hash: winner.clone(),
                    device_id: "dd".repeat(32),
                    size_bytes: 153,
                    created_at: 1,
                    ..Default::default()
                }],
                resolution: Some("merged".into()),
                winning_manifest_hash: Some(winner),
                winning_size_bytes: Some(153),
                ..Default::default()
            },
            &kp,
        )),
    )
    .await
    .expect("resolved report ok");

    let bytes = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect(
            "no fauna.sync.changed push after a resolved report — the winner head \
             row landed and no device was told to pull it",
        )
        .expect("push channel closed");
    let frame = fauna_protocol::decode_frame(&bytes).expect("decode frame");
    let push = match frame {
        fauna_protocol::Frame::Push(p) => p,
        other => panic!("expected Push frame, got {other:?}"),
    };
    match fauna_protocol::PushEvent::from_push(&push.kind, push.payload) {
        fauna_protocol::PushEvent::SyncChanged(p) => assert_eq!(p.folder, "cfn"),
        other => panic!("expected SyncChanged push, got {}", other.kind()),
    }
}

/// …and an UNRESOLVED (chooser) report does not, because it records no change
/// row. Keeps the nudge tied to "there is something to pull" rather than to
/// "an RPC happened" — a nudge on every report would wake every device of every
/// participant for a row none of them can act on.
#[tokio::test]
async fn an_unresolved_conflict_report_does_not_nudge() {
    let (router, state) = router_and_state().await;
    let actor = [25u8; 32];
    create_set(&router, &state, actor, "cfu").await;

    let (_conn, mut rx) = state.ws.subscribe(actor);

    dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.sync.conflicts.report",
        encode(&ConflictReportRequest {
            path_sealed: Some(fauna_protocol::ByteBuf::from(
                b"e2e-synthetic-seal".to_vec(),
            )),
            folder: "cfu".into(),
            device_id: "dd".repeat(32),
            path: "/cfu/a.txt".into(),
            conflict_type: "concurrent_edit".into(),
            details: Some("two devices".into()),
            candidates: Default::default(),
            ..Default::default()
        }),
    )
    .await
    .expect("unresolved report ok");

    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(300), rx.recv())
            .await
            .is_err(),
        "an unresolved chooser report records no change row, so it must not nudge"
    );
}

/// Path-sealing S5c-2: `SyncConflict.folder` ships the same set-name seal +
/// salt pair `fauna.folders.list`/`fauna.media.list` already carry, so a
/// conflict row stays renderable once the plaintext `folder` scrubs.
/// Owner/participant-audience surface — no per-reader projection, unlike
/// `media.list`'s Q5-admin arm (`docs/goal/behavior/file-sync.md`
/// § Sealed names & paths).
#[tokio::test]
async fn conflict_list_ships_the_set_name_seal_and_salt_pair() {
    let (router, state) = router_and_state().await;
    let actor = [26u8; 32];
    create_set(&router, &state, actor, "cf-sealed").await;

    let _report: ConflictReportReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.sync.conflicts.report",
            encode(&ConflictReportRequest {
                path_sealed: Some(fauna_protocol::ByteBuf::from(
                    b"e2e-synthetic-seal".to_vec(),
                )),
                folder: "cf-sealed".into(),
                device_id: "dd".repeat(32),
                path: "/cf-sealed/a.txt".into(),
                conflict_type: "concurrent_edit".into(),
                details: None,
                candidates: Default::default(),
                ..Default::default()
            }),
        )
        .await
        .expect("report ok"),
    )
    .unwrap();

    // Only a keyed writer stamps the seal; stamp it directly, as the engine's
    // bind/serve catch-up pass would (mirrors
    // `conformance_media_list.rs::the_set_name_pair_ships_to_the_owner_and_to_a_roster_member`).
    let sealed = vec![0xEDu8; 48];
    state
        .db
        .update_folder_for_user(
            "cf-sealed",
            &actor,
            fauna_nest::db::FolderUpdate {
                name_sealed: Some(&sealed),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let list: ConflictsListReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.sync.conflicts.list",
            encode(&ConflictsListRequest {
                ..Default::default()
            }),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();
    assert_eq!(list.conflicts.len(), 1);
    let row = &list.conflicts[0];
    assert_eq!(
        row.folder_sealed.as_deref().map(|b| b.to_vec()),
        Some(sealed),
        "the conflict row carries the set-name seal verbatim"
    );
    assert_eq!(
        row.folder_hash.as_deref().map(|b| b.to_vec()),
        Some(fauna_core::path_crypto::set_name_hash("cf-sealed").to_vec()),
        "and the salt that opens it"
    );
}

/// The pair is strictly a pair: a set whose seal was never stamped ships
/// neither half on its conflict rows, so no reader ever receives a bare salt
/// with nothing to open.
#[tokio::test]
async fn conflict_list_ships_neither_half_for_an_unstamped_set() {
    let (router, state) = router_and_state().await;
    let actor = [27u8; 32];
    create_set(&router, &state, actor, "cf-unstamped").await;
    let _report: ConflictReportReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.sync.conflicts.report",
            encode(&ConflictReportRequest {
                path_sealed: Some(fauna_protocol::ByteBuf::from(
                    b"e2e-synthetic-seal".to_vec(),
                )),
                folder: "cf-unstamped".into(),
                device_id: "dd".repeat(32),
                path: "/cf-unstamped/a.txt".into(),
                conflict_type: "concurrent_edit".into(),
                details: None,
                candidates: Default::default(),
                ..Default::default()
            }),
        )
        .await
        .expect("report ok"),
    )
    .unwrap();

    let list: ConflictsListReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.sync.conflicts.list",
            encode(&ConflictsListRequest {
                ..Default::default()
            }),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();
    assert_eq!(list.conflicts.len(), 1);
    assert_eq!(
        list.conflicts[0].folder_sealed, None,
        "nothing stamped this set's name yet"
    );
    assert_eq!(
        list.conflicts[0].folder_hash, None,
        "so no bare salt ships either"
    );
}

/// Auto-resolve (file-sync.md § Conflicts, ratified 2026-07-10): a report
/// carrying `resolution` + `winning_manifest_hash` lands ALREADY RESOLVED —
/// the default (unresolved-only) list omits it, the `include_resolved` review
/// list returns it with its resolution metadata and candidates (the candidate
/// `content_key_version` echoed for the sealed-set re-point).
#[tokio::test]
async fn conflict_report_pre_resolved_feeds_review_list_not_chooser() {
    let (router, state) = router_and_state().await;
    let kp = common::signing_actor(25);
    let actor = kp.actor_id().0;
    create_set(&router, &state, actor, "ar").await;

    let report: ConflictReportReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.sync.conflicts.report",
            encode(&signed_report(
                ConflictReportRequest {
                    path_sealed: Some(fauna_protocol::ByteBuf::from(
                        b"e2e-synthetic-seal".to_vec(),
                    )),
                    folder: "ar".into(),
                    device_id: "dd".repeat(32),
                    path: "/ar/notes.txt".into(),
                    conflict_type: "concurrent_edit".into(),
                    details: None,
                    candidates: vec![
                        ConflictCandidate {
                            manifest_hash: "aa".repeat(32),
                            device_id: "dd".repeat(32),
                            size_bytes: 10,
                            created_at: 1_700_000_000,
                            content_key_version: Some(3),
                            ..Default::default()
                        },
                        ConflictCandidate {
                            manifest_hash: "bb".repeat(32),
                            device_id: "ee".repeat(32),
                            size_bytes: 12,
                            created_at: 1_700_000_001,
                            ..Default::default()
                        },
                    ],
                    resolution: Some("latest_wins".into()),
                    winning_manifest_hash: Some("bb".repeat(32)),
                    ..Default::default()
                },
                &kp,
            )),
        )
        .await
        .expect("pre-resolved report ok"),
    )
    .unwrap();

    // The blocking-chooser surface (unresolved-only) never sees it.
    let unresolved: ConflictsListReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.sync.conflicts.list",
            encode(&ConflictsListRequest {
                ..Default::default()
            }),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();
    assert!(
        unresolved.conflicts.is_empty(),
        "a pre-resolved conflict must not appear on the unresolved (chooser) surface"
    );

    // The review list returns it, resolution metadata + candidates intact.
    let review: ConflictsListReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.sync.conflicts.list",
            encode(&ConflictsListRequest {
                include_resolved: Some(true),
                ..Default::default()
            }),
        )
        .await
        .expect("review list ok"),
    )
    .unwrap();
    assert_eq!(review.conflicts.len(), 1);
    let c = &review.conflicts[0];
    assert_eq!(c.id, report.id);
    assert!(c.resolved_at.is_some(), "row lands resolved");
    assert_eq!(c.resolution.as_deref(), Some("latest_wins"));
    assert_eq!(c.winning_manifest_hash, Some("bb".repeat(32)));
    assert_eq!(c.candidates.len(), 2);
    assert_eq!(c.candidates[0].content_key_version, Some(3));
    assert_eq!(c.candidates[1].content_key_version, None);

    // Already resolved ⇒ the chooser's resolve is a not_found, not a re-resolve.
    let err = dispatch(
        &router,
        state,
        actor,
        "fauna.sync.conflicts.resolve",
        encode(&ConflictResolveRequest {
            id: report.id,
            winning_manifest_hash: None,
            extra: Default::default(),
            winner_signature: None,
            winner_signer_key: None,
        }),
    )
    .await
    .expect_err("resolve of a pre-resolved row → not_found");
    assert_eq!(err.code, "fauna.sync.not_found");
}

/// A resolved report must name a valid resolution kind and carry the winner.
#[tokio::test]
async fn conflict_report_resolved_shape_is_validated() {
    let (router, state) = router_and_state().await;
    let actor = [26u8; 32];
    create_set(&router, &state, actor, "arv").await;

    let base = ConflictReportRequest {
        path_sealed: Some(fauna_protocol::ByteBuf::from(
            b"e2e-synthetic-seal".to_vec(),
        )),
        folder: "arv".into(),
        device_id: "dd".repeat(32),
        path: "/arv/x.txt".into(),
        conflict_type: "concurrent_edit".into(),
        ..Default::default()
    };

    let err = dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.sync.conflicts.report",
        encode(&ConflictReportRequest {
            path_sealed: Some(fauna_protocol::ByteBuf::from(
                b"e2e-synthetic-seal".to_vec(),
            )),
            resolution: Some("merged".into()),
            winning_manifest_hash: None,
            ..base.clone()
        }),
    )
    .await
    .expect_err("resolution without winner rejected");
    assert_eq!(err.code, "fauna.sync.invalid_request");

    let err = dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.sync.conflicts.report",
        encode(&ConflictReportRequest {
            path_sealed: Some(fauna_protocol::ByteBuf::from(
                b"e2e-synthetic-seal".to_vec(),
            )),
            resolution: Some("coin_toss".into()),
            winning_manifest_hash: Some("bb".repeat(32)),
            ..base
        }),
    )
    .await
    .expect_err("unknown resolution kind rejected");
    assert_eq!(err.code, "fauna.sync.invalid_request");
}

/// Per-set conflict policy (file-sync.md § Conflicts): defaults to `auto` on
/// create, round-trips through `fauna.folders.update` → `list`, and a
/// setter with an unknown value is rejected (readers degrade, setters don't).
#[tokio::test]
async fn conflict_policy_defaults_updates_and_validates() {
    let (router, state) = router_and_state().await;
    let actor = [27u8; 32];
    create_set(&router, &state, actor, "pol").await;

    let list: FoldersListReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.folders.list",
            encode(&FoldersListRequest {
                include_shared_with_me: None,
                extra: Default::default(),
            }),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();
    assert_eq!(
        list.folders[0].conflict_policy.as_deref(),
        Some("auto"),
        "fresh set defaults to the ratified auto policy"
    );

    let updated: FolderUpdateReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.folders.update",
            encode(&FolderUpdateRequest {
                name: "pol".into(),
                conflict_policy: Some("latest_wins_always".into()),
                ..Default::default()
            }),
        )
        .await
        .expect("update ok"),
    )
    .unwrap();
    assert!(updated.ok);

    let list: FoldersListReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.folders.list",
            encode(&FoldersListRequest {
                include_shared_with_me: None,
                extra: Default::default(),
            }),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();
    assert_eq!(
        list.folders[0].conflict_policy.as_deref(),
        Some("latest_wins_always")
    );

    let err = dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.folders.update",
        encode(&FolderUpdateRequest {
            name: "pol".into(),
            conflict_policy: Some("hologram".into()),
            ..Default::default()
        }),
    )
    .await
    .expect_err("unknown policy rejected");
    assert_eq!(err.code, "fauna.folders.invalid_request");

    // Create-time policy (the global-default stamp): a create carrying
    // conflict_policy lands on the row atomically; an unknown value is
    // rejected by the same setter rule as update.
    let _: FolderCreateReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.folders.create",
            encode(&FolderCreateRequest {
                name: "pol2".into(),
                conflict_policy: Some("latest_wins_always".into()),
                ..Default::default()
            }),
        )
        .await
        .expect("create with policy ok"),
    )
    .unwrap();
    let list: FoldersListReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.folders.list",
            encode(&FoldersListRequest {
                include_shared_with_me: None,
                extra: Default::default(),
            }),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();
    let pol2 = list
        .folders
        .iter()
        .find(|f| f.name == "pol2")
        .expect("pol2 listed");
    assert_eq!(pol2.conflict_policy.as_deref(), Some("latest_wins_always"));

    let err = dispatch(
        &router,
        state,
        actor,
        "fauna.folders.create",
        encode(&FolderCreateRequest {
            name: "pol3".into(),
            conflict_policy: Some("hologram".into()),
            ..Default::default()
        }),
    )
    .await
    .expect_err("unknown create-time policy rejected");
    assert_eq!(err.code, "fauna.folders.invalid_request");
}

#[tokio::test]
async fn conflict_report_unknown_set_not_found() {
    let (router, state) = router_and_state().await;
    let err = dispatch(
        &router,
        state,
        [24u8; 32],
        "fauna.sync.conflicts.report",
        encode(&ConflictReportRequest {
            path_sealed: Some(fauna_protocol::ByteBuf::from(
                b"e2e-synthetic-seal".to_vec(),
            )),
            folder: "ghost".into(),
            device_id: "ee".repeat(32),
            path: "/x".into(),
            conflict_type: "t".into(),
            details: None,
            candidates: Default::default(),
            ..Default::default()
        }),
    )
    .await
    .expect_err("unknown set → not_found");
    assert_eq!(err.code, "fauna.sync.not_found");
}

/// The rich flow (Slice 2): a conflict
/// reported with candidate versions lists them; resolving by choosing a winner
/// records the choice AND propagates it as an ordinary `sync_changes` row, so
/// every *other* device converges via the normal `changes.list` catch-up.
#[tokio::test]
async fn conflicts_choose_winner_propagates() {
    // Both folder and sync handlers: the propagated winner is asserted via
    // `fauna.sync.changes.list` (the catch-up vehicle).
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    folder_handlers::register_folders_handlers(&mut b);
    sync_handlers::register_sync_handlers(&mut b);
    let router = b.build();

    let kp = common::signing_actor(31);
    let actor = kp.actor_id().0;
    create_set(&router, &state, actor, "cf").await;

    let local_dev = "aa".repeat(32);
    let incoming_dev = "bb".repeat(32);
    let local_manifest = "11".repeat(32);
    let incoming_manifest = "22".repeat(32);

    // Daemon reports the conflict with both diverging versions as candidates.
    let report: ConflictReportReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.sync.conflicts.report",
            encode(&ConflictReportRequest {
                path_sealed: Some(fauna_protocol::ByteBuf::from(
                    b"e2e-synthetic-seal".to_vec(),
                )),
                folder: "cf".into(),
                device_id: local_dev.clone(),
                path: "/cf/a.txt".into(),
                conflict_type: "concurrent_edit".into(),
                details: None,
                candidates: vec![
                    ConflictCandidate {
                        manifest_hash: local_manifest.clone(),
                        device_id: local_dev.clone(),
                        size_bytes: 10,
                        created_at: 100,
                        ..Default::default()
                    },
                    ConflictCandidate {
                        manifest_hash: incoming_manifest.clone(),
                        device_id: incoming_dev.clone(),
                        size_bytes: 20,
                        created_at: 200,
                        ..Default::default()
                    },
                ],
                ..Default::default()
            }),
        )
        .await
        .expect("report ok"),
    )
    .unwrap();

    // list returns the conflict WITH its candidates.
    let list: ConflictsListReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.sync.conflicts.list",
            encode(&ConflictsListRequest {
                ..Default::default()
            }),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();
    assert_eq!(list.conflicts.len(), 1);
    let conflict = &list.conflicts[0];
    assert_eq!(conflict.id, report.id);
    assert_eq!(conflict.candidates.len(), 2, "both candidates round-trip");
    let hashes: Vec<&str> = conflict
        .candidates
        .iter()
        .map(|c| c.manifest_hash.as_str())
        .collect();
    assert!(hashes.contains(&local_manifest.as_str()));
    assert!(hashes.contains(&incoming_manifest.as_str()));

    // Resolve by choosing the incoming version as the winner.
    let resolved: ConflictResolveReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.sync.conflicts.resolve",
            encode(&signed_choose_winner(
                ConflictResolveRequest {
                    id: report.id,
                    winning_manifest_hash: Some(incoming_manifest.clone()),
                    extra: Default::default(),
                    winner_signature: None,
                    winner_signer_key: None,
                },
                conflict,
                &kp,
            )),
        )
        .await
        .expect("resolve ok"),
    )
    .unwrap();
    assert!(resolved.resolved);
    assert_eq!(
        resolved.winning_manifest_hash.as_deref(),
        Some(incoming_manifest.as_str()),
        "reply echoes the chosen winner"
    );

    // The conflict is now resolved — it drops out of the unresolved list.
    let after: ConflictsListReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.sync.conflicts.list",
            encode(&ConflictsListRequest {
                ..Default::default()
            }),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();
    assert!(
        after.conflicts.is_empty(),
        "resolved conflict no longer listed"
    );

    // The winner is propagated as an ordinary change row → visible to every
    // *other* device via the normal catch-up query.
    let changes: SyncChangesListReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.sync.changes.list",
            encode(&SyncChangesListRequest {
                folder: Some("cf".into()),
                device_id: None,
                since: 0,
                ..Default::default()
            }),
        )
        .await
        .expect("changes ok"),
    )
    .unwrap();
    let winner_change = changes
        .changes
        .iter()
        .find(|c| c.path_hash == hex::encode(fauna_core::sync::path_hash("/cf/a.txt")))
        .expect("propagated change present");
    assert_eq!(
        winner_change.manifest_hash.as_deref(),
        Some(incoming_manifest.as_str())
    );
    assert_eq!(
        winner_change.device_id.as_deref(),
        Some(incoming_dev.as_str()),
        "attributed to the winning candidate's device"
    );
    assert_eq!(winner_change.change_type, "modify");
    assert_eq!(winner_change.size_bytes, 20);

    // The winning device itself sees the change as a self-echo (excluded), so
    // it won't redundantly re-download what it already has.
    let from_winner: SyncChangesListReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.sync.changes.list",
            encode(&SyncChangesListRequest {
                folder: Some("cf".into()),
                device_id: Some(incoming_dev.clone()),
                since: 0,
                ..Default::default()
            }),
        )
        .await
        .expect("changes ok"),
    )
    .unwrap();
    assert!(
        from_winner
            .changes
            .iter()
            .all(|c| c.path.as_deref() != Some("/cf/a.txt")),
        "winning device's own propagated change is self-echo-excluded"
    );
}

/// Auto-resolve propagation (file-sync.md § Conflicts + § File Versions): a
/// pre-resolved report transactionally RETAINS the reporter's losing version
/// as an ordinary change row (a listable, GC-pinned version) and propagates
/// the winner as the new head row — no client-side record ordering, no crash
/// window. Attribution follows the choose-winner precedent (self-echo-skip).
#[tokio::test]
async fn pre_resolved_report_retains_loser_and_propagates_winner() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    folder_handlers::register_folders_handlers(&mut b);
    sync_handlers::register_sync_handlers(&mut b);
    let router = b.build();

    let kp = common::signing_actor(32);
    let actor = kp.actor_id().0;
    create_set(&router, &state, actor, "ap").await;

    let reporter_dev = "aa".repeat(32);
    let other_dev = "bb".repeat(32);
    let local_manifest = "11".repeat(32);
    let incoming_manifest = "22".repeat(32);
    let merged_manifest = "33".repeat(32);

    let candidates = vec![
        ConflictCandidate {
            manifest_hash: local_manifest.clone(),
            device_id: reporter_dev.clone(),
            size_bytes: 10,
            created_at: 100,
            content_key_version: Some(3),
            ..Default::default()
        },
        ConflictCandidate {
            manifest_hash: incoming_manifest.clone(),
            device_id: other_dev.clone(),
            size_bytes: 20,
            created_at: 200,
            ..Default::default()
        },
    ];

    let changes_for = |path: &'static str, state: &Arc<AppState>| {
        let router = &router;
        let state = Arc::clone(state);
        async move {
            let reply: SyncChangesListReply = decode(
                &dispatch(
                    router,
                    state,
                    actor,
                    "fauna.sync.changes.list",
                    encode(&SyncChangesListRequest {
                        folder: Some("ap".into()),
                        device_id: None,
                        since: 0,
                        ..Default::default()
                    }),
                )
                .await
                .expect("changes ok"),
            )
            .unwrap();
            reply
                .changes
                .into_iter()
                .filter(|c| c.path_hash == hex::encode(fauna_core::sync::path_hash(path)))
                .collect::<Vec<_>>()
        }
    };

    // Case 1 — incoming wins (latest_wins, winner IS a candidate): the
    // reporter's local loser is retained first, then the winner head row,
    // attributed to the winning candidate's device.
    dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.sync.conflicts.report",
        encode(&signed_report(
            ConflictReportRequest {
                path_sealed: Some(fauna_protocol::ByteBuf::from(
                    b"e2e-synthetic-seal".to_vec(),
                )),
                folder: "ap".into(),
                device_id: reporter_dev.clone(),
                path: "/ap/a.txt".into(),
                conflict_type: "concurrent_edit".into(),
                details: None,
                candidates: candidates.clone(),
                resolution: Some("latest_wins".into()),
                winning_manifest_hash: Some(incoming_manifest.clone()),
                ..Default::default()
            },
            &kp,
        )),
    )
    .await
    .expect("pre-resolved report ok");

    let rows = changes_for("/ap/a.txt", &state).await;
    assert_eq!(rows.len(), 2, "loser retention row + winner head row");
    let (loser, winner) = (&rows[0], &rows[1]);
    assert_eq!(
        loser.manifest_hash.as_deref(),
        Some(local_manifest.as_str())
    );
    assert_eq!(loser.device_id.as_deref(), Some(reporter_dev.as_str()));
    assert_eq!(loser.size_bytes, 10);
    assert_eq!(
        loser.content_key_version,
        Some(3),
        "loser row echoes the candidate's sealed generation"
    );
    assert_eq!(
        winner.manifest_hash.as_deref(),
        Some(incoming_manifest.as_str()),
        "winner is the head (higher seq)"
    );
    assert_eq!(
        winner.device_id.as_deref(),
        Some(other_dev.as_str()),
        "winner row attributed to the winning candidate's device"
    );
    assert_eq!(winner.size_bytes, 20);
    assert!(winner.seq > loser.seq, "winner is the newer head");

    // Case 2 — merged winner (not a candidate): needs winning_size_bytes,
    // attributed to the reporter (who wrote the merged result locally).
    dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.sync.conflicts.report",
        encode(&signed_report(
            ConflictReportRequest {
                path_sealed: Some(fauna_protocol::ByteBuf::from(
                    b"e2e-synthetic-seal".to_vec(),
                )),
                folder: "ap".into(),
                device_id: reporter_dev.clone(),
                path: "/ap/b.txt".into(),
                conflict_type: "concurrent_edit".into(),
                details: None,
                candidates: candidates.clone(),
                resolution: Some("merged".into()),
                winning_manifest_hash: Some(merged_manifest.clone()),
                winning_size_bytes: Some(15),
                winning_content_key_version: Some(4),
                ..Default::default()
            },
            &kp,
        )),
    )
    .await
    .expect("merged report ok");

    let rows = changes_for("/ap/b.txt", &state).await;
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0].manifest_hash.as_deref(),
        Some(local_manifest.as_str()),
        "local parent retained"
    );
    let winner = &rows[1];
    assert_eq!(
        winner.manifest_hash.as_deref(),
        Some(merged_manifest.as_str())
    );
    assert_eq!(winner.device_id.as_deref(), Some(reporter_dev.as_str()));
    assert_eq!(winner.size_bytes, 15);
    assert_eq!(winner.content_key_version, Some(4));

    // A merged winner WITHOUT its size is rejected (nothing lands). Sent
    // unsigned on purpose: a writer cannot sign it (the winner row's size is
    // exactly what is missing), and the shape refusal precedes the signature
    // gate.
    let err = dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.sync.conflicts.report",
        encode(&ConflictReportRequest {
            path_sealed: Some(fauna_protocol::ByteBuf::from(
                b"e2e-synthetic-seal".to_vec(),
            )),
            folder: "ap".into(),
            device_id: reporter_dev.clone(),
            path: "/ap/b2.txt".into(),
            conflict_type: "concurrent_edit".into(),
            details: None,
            candidates: candidates.clone(),
            resolution: Some("merged".into()),
            winning_manifest_hash: Some(merged_manifest.clone()),
            ..Default::default()
        }),
    )
    .await
    .expect_err("merged winner without size rejected");
    assert_eq!(err.code, "fauna.sync.invalid_request");
    assert!(changes_for("/ap/b2.txt", &state).await.is_empty());

    // The minted rows are metered (2026-09-28) like any record: a negative
    // declared winner size answers the record door's typed `invalid_size`,
    // and nothing lands.
    let err = dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.sync.conflicts.report",
        encode(&signed_report(
            ConflictReportRequest {
                path_sealed: Some(fauna_protocol::ByteBuf::from(
                    b"e2e-synthetic-seal".to_vec(),
                )),
                folder: "ap".into(),
                device_id: reporter_dev.clone(),
                path: "/ap/b3.txt".into(),
                conflict_type: "concurrent_edit".into(),
                details: None,
                candidates: candidates.clone(),
                resolution: Some("merged".into()),
                winning_manifest_hash: Some(merged_manifest.clone()),
                winning_size_bytes: Some(-1_000_000),
                ..Default::default()
            },
            &kp,
        )),
    )
    .await
    .expect_err("negative winner size refused");
    assert_eq!(err.code, "fauna.sync.invalid_size");
    assert!(changes_for("/ap/b3.txt", &state).await.is_empty());

    // Case 3 — local wins: the winner IS the reporter's own candidate, so no
    // separate loser row (the incoming version is already a recorded change on
    // the other device's side); exactly one head row, self-echo-skipped by the
    // reporter.
    dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.sync.conflicts.report",
        encode(&signed_report(
            ConflictReportRequest {
                path_sealed: Some(fauna_protocol::ByteBuf::from(
                    b"e2e-synthetic-seal".to_vec(),
                )),
                folder: "ap".into(),
                device_id: reporter_dev.clone(),
                path: "/ap/c.txt".into(),
                conflict_type: "concurrent_edit".into(),
                details: None,
                candidates: candidates.clone(),
                resolution: Some("latest_wins".into()),
                winning_manifest_hash: Some(local_manifest.clone()),
                ..Default::default()
            },
            &kp,
        )),
    )
    .await
    .expect("local-wins report ok");

    let rows = changes_for("/ap/c.txt", &state).await;
    assert_eq!(
        rows.len(),
        1,
        "no self-loser row when the local version wins"
    );
    assert_eq!(
        rows[0].manifest_hash.as_deref(),
        Some(local_manifest.as_str())
    );
    assert_eq!(rows[0].device_id.as_deref(), Some(reporter_dev.as_str()));
}

/// `conflicts.md` § *Retention rows are transparent to the licence* (ruled
/// 2026-09-27): the resolved report's winner row carries the reporter's
/// `winning_derived_through` EXACTLY as sent. The retired gap-2 upgrade raised
/// it to the just-minted loser row's seq whenever no same-path row intervened
/// — a nest-assigned seq no reporter can sign, so an upgraded row could never
/// verify. Here the claim is the path's previous head, adjacent to the loser
/// row: exactly the shape the upgrade used to fire on.
#[tokio::test]
async fn a_resolved_reports_winner_claim_is_minted_as_sent_beside_an_adjacent_loser_row() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    folder_handlers::register_folders_handlers(&mut b);
    sync_handlers::register_sync_handlers(&mut b);
    let router = b.build();

    let kp = common::signing_actor(33);
    let actor = kp.actor_id().0;
    create_set(&router, &state, actor, "ap").await;
    let reporter_dev = "aa".repeat(32);
    let other_dev = "bb".repeat(32);
    let path = "/ap/adjacent.txt";
    let report =
        |local: &str, incoming: &str, merged: &str, claim: Option<i64>| ConflictReportRequest {
            path_sealed: Some(fauna_protocol::ByteBuf::from(
                b"e2e-synthetic-seal".to_vec(),
            )),
            folder: "ap".into(),
            device_id: reporter_dev.clone(),
            path: path.into(),
            conflict_type: "concurrent_edit".into(),
            candidates: vec![
                ConflictCandidate {
                    manifest_hash: local.into(),
                    device_id: reporter_dev.clone(),
                    size_bytes: 10,
                    created_at: 100,
                    ..Default::default()
                },
                ConflictCandidate {
                    manifest_hash: incoming.into(),
                    device_id: other_dev.clone(),
                    size_bytes: 20,
                    created_at: 200,
                    ..Default::default()
                },
            ],
            resolution: Some("merged".into()),
            winning_manifest_hash: Some(merged.into()),
            winning_size_bytes: Some(15),
            winning_derived_through: claim,
            winning_carries_novelty: Some(true),
            ..Default::default()
        };
    let rows_on_path = || {
        let router = &router;
        let state = Arc::clone(&state);
        async move {
            let reply: SyncChangesListReply = decode(
                &dispatch(
                    router,
                    state,
                    actor,
                    "fauna.sync.changes.list",
                    encode(&SyncChangesListRequest {
                        folder: Some("ap".into()),
                        since: 0,
                        ..Default::default()
                    }),
                )
                .await
                .expect("changes ok"),
            )
            .unwrap();
            reply
                .changes
                .into_iter()
                .filter(|c| c.path_hash == hex::encode(fauna_core::sync::path_hash(path)))
                .collect::<Vec<_>>()
        }
    };

    // A first report gives the path a head the second can claim.
    dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.sync.conflicts.report",
        encode(&signed_report(
            report(&"11".repeat(32), &"22".repeat(32), &"33".repeat(32), None),
            &kp,
        )),
    )
    .await
    .expect("first report ok");
    let head = rows_on_path().await.last().expect("a head row").seq;

    // The second report claims that head; its loser row lands at head + 1
    // with nothing between — the adjacency the upgrade keyed on.
    dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.sync.conflicts.report",
        encode(&signed_report(
            report(
                &"44".repeat(32),
                &"55".repeat(32),
                &"66".repeat(32),
                Some(head),
            ),
            &kp,
        )),
    )
    .await
    .expect("second report ok");
    let rows = rows_on_path().await;
    let (loser, winner) = (&rows[rows.len() - 2], &rows[rows.len() - 1]);
    assert_eq!(loser.is_retention, Some(true));
    assert_eq!(
        loser.seq,
        head + 1,
        "the loser row is adjacent to the claim"
    );
    assert_eq!(
        winner.derived_through,
        Some(head),
        "the winner's claim is minted exactly as sent — never upgraded to the loser row's seq"
    );
    assert_ne!(
        winner.is_resolution,
        Some(true),
        "a winner carrying the reporter's novelty mints edit-class"
    );
}

/// Choosing a `winning_manifest_hash` that is not one of the conflict's
/// recorded candidates is rejected with `fauna.sync.bad_candidate`
/// (devices.md § Errors), and the conflict stays unresolved.
#[tokio::test]
async fn conflicts_resolve_rejects_unknown_winner() {
    let (router, state) = router_and_state().await;
    let kp = common::signing_actor(32);
    let actor = kp.actor_id().0;
    create_set(&router, &state, actor, "cf2").await;

    let report: ConflictReportReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.sync.conflicts.report",
            encode(&ConflictReportRequest {
                path_sealed: Some(fauna_protocol::ByteBuf::from(
                    b"e2e-synthetic-seal".to_vec(),
                )),
                folder: "cf2".into(),
                device_id: "cc".repeat(32),
                path: "/cf2/b.txt".into(),
                conflict_type: "concurrent_edit".into(),
                details: None,
                candidates: vec![ConflictCandidate {
                    manifest_hash: "33".repeat(32),
                    device_id: "cc".repeat(32),
                    size_bytes: 5,
                    created_at: 1,
                    ..Default::default()
                }],
                ..Default::default()
            }),
        )
        .await
        .expect("report ok"),
    )
    .unwrap();

    // A SIGNED choose naming a hash that is not one of the recorded
    // candidates: the chooser signs over the listed conflict's real candidate
    // (no writer can sign a winner the conflict does not hold — the statement
    // is built from the candidate row), then the request is re-pointed at an
    // unknown hash. An unsigned choose would be refused `signature_required`
    // first; this one reaches the candidate check the test pins.
    let listed: ConflictsListReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.sync.conflicts.list",
            encode(&ConflictsListRequest {
                ..Default::default()
            }),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();
    let conflict = listed
        .conflicts
        .iter()
        .find(|c| c.id == report.id)
        .expect("the reported conflict is listed");
    let mut choose = signed_choose_winner(
        ConflictResolveRequest {
            id: report.id,
            winning_manifest_hash: Some("33".repeat(32)),
            extra: Default::default(),
            winner_signature: None,
            winner_signer_key: None,
        },
        conflict,
        &kp,
    );
    // Not a recorded candidate.
    choose.winning_manifest_hash = Some("99".repeat(32));
    let err = dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.sync.conflicts.resolve",
        encode(&choose),
    )
    .await
    .expect_err("unknown winner → bad_candidate");
    assert_eq!(err.code, "fauna.sync.bad_candidate");

    // The conflict is untouched — still listed, still resolvable mark-only.
    let list: ConflictsListReply = decode(
        &dispatch(
            &router,
            state,
            actor,
            "fauna.sync.conflicts.list",
            encode(&ConflictsListRequest {
                ..Default::default()
            }),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();
    assert_eq!(
        list.conflicts.len(),
        1,
        "rejected resolve left conflict open"
    );
}

// ── cross-actor isolation ──────────────────────────────────────────────────────

#[tokio::test]
async fn cross_actor_update_delete_not_found() {
    let (router, state) = router_and_state().await;
    let owner = [25u8; 32];
    let attacker = [26u8; 32];
    create_set(&router, &state, owner, "secret").await;

    let upd = dispatch(
        &router,
        Arc::clone(&state),
        attacker,
        "fauna.folders.update",
        encode(&FolderUpdateRequest {
            name: "secret".into(),
            retention_policy: None,
            include_paths: None,
            exclude_paths: None,
            webdav_enabled: None,
            ..Default::default()
        }),
    )
    .await
    .expect_err("cross-actor update → not_found");
    assert_eq!(upd.code, "fauna.folders.not_found");

    let del = dispatch(
        &router,
        state,
        attacker,
        "fauna.folders.delete",
        encode(&FolderDeleteRequest {
            name: "secret".into(),
            ..Default::default()
        }),
    )
    .await
    .expect_err("cross-actor delete → not_found");
    assert_eq!(del.code, "fauna.folders.not_found");
}

// ── malformed payload ──────────────────────────────────────────────────────────

#[tokio::test]
async fn rejects_malformed_payload() {
    let (router, state) = router_and_state().await;
    let err = dispatch(
        &router,
        state,
        [27u8; 32],
        "fauna.folders.create",
        Bytes::from_static(&[0xff, 0xff, 0xff]),
    )
    .await
    .expect_err("malformed payload rejected");
    assert_eq!(err.code, "fauna.protocol.malformed");
}

// ── reserved rails are closed to the generic record kind ─────────────────────

/// `fauna.sync.changes.record` REFUSES a reserved (`__`) rail that is not a
/// backup destination — so no client-side writer can reach a rail through the
/// generic record kind, `__index` included.
///
/// **Why this test exists, and why it names `__index`.** The content-index
/// builder's publisher seam (`content-index.md` § Ingest triggers, v1 — then
/// *still owed by S3: the fauna-sync publisher implementation*; the seam is
/// today `fauna_client_index::SegmentRail`) was scoped
/// against a plan to upload a sealed segment with `SyncEngine::upload_bytes`
/// and then record it with this kind. That plan is **dead on arrival on two
/// independent counts**, and neither is visible from the client side — which is
/// exactly why it survived a design pass and a landed engine method with zero
/// callers:
///
/// 1. **This refusal** (`sync_handlers::record_change_core`): a reserved set
///    takes device-sync changes only as a custody copy, and a
///    rail minted by `get_or_create_reserved_folder` is never one (it
///    must not be — a backup record lands in `backup_custody` with `seq = 0` and
///    deliberately never enters the `sync_changes` device-pull feed, which is
///    the exact replication `content-index.md` § Where the index is built
///    requires of `__index`).
/// 2. **The GC's direct-blob classification** (`db/sync_storage.rs`
///    `fold_direct_blob_refs`): a hash referenced only by reserved sets is
///    pinned as *the blob itself* and never walked for chunks. A chunked
///    manifest recorded here would have its live chunks swept — silent,
///    unrecoverable data loss on a user at-rest store.
///
/// The rails' actual shape is what S2's own pin stages
/// (`index_survives_nest_restart.rs`): the whole blob in the blob store, with a
/// `sync_changes` row written **at the DB layer** pointing at that blob hash —
/// the `__drafts` structure (`db/drafts.rs`
/// `record_drafts_blob_change`). A client reaches it through a dedicated kind,
/// never this one.
#[tokio::test]
async fn changes_record_refuses_a_reserved_rail_so_no_client_writer_can_use_it() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    folder_handlers::register_folders_handlers(&mut b);
    sync_handlers::register_sync_handlers(&mut b);
    let router = b.build();

    let actor = [0x1d_u8; 32];
    let device = [0x2d_u8; 32];
    state
        .db
        .register_sync_device(&actor, &device, "laptop", None, "write")
        .await
        .unwrap();

    // Mint the `__index` rail exactly as a feature rail does — lazily, through
    // the shared get-or-create. This is also the answer to "does a fresh nest
    // need `fauna.sync.register` / folder provisioning before the first
    // `__index` write": no, the rail mints itself here.
    let fs_id = state
        .db
        .get_or_create_reserved_folder(&actor, "index")
        .await
        .unwrap();

    let err = dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.sync.changes.record",
        encode(&fauna_protocol::sync::SyncChangeRecordRequest {
            folder: "__index".into(),
            device_id: hex::encode(device),
            path: "__index/mail/seg-00000001.idx".into(),
            manifest_hash: Some("ab".repeat(32)),
            size_bytes: 4096,
            change_type: "create".into(),
            // A seal is supplied so the refusal proven here is the
            // *reserved-rail* arm, not the S9 seal gate firing first: only a
            // reserved BACKUP DESTINATION is exempt from `path_sealed` (its
            // paths are coordinator-synthetic segment names), and `__index`
            // is not one — the sealless twin of this request is pinned by
            // `changes_record_grants_no_plaintext_exemption_to_a_reserved_rail`
            // below.
            path_sealed: Some(fauna_protocol::ByteBuf::from(
                b"e2e-synthetic-seal".to_vec(),
            )),
            ..Default::default()
        }),
    )
    .await
    .expect_err("a reserved rail must not take a generic device-sync record");
    assert_eq!(err.code, "fauna.sync.invalid_request");

    // THE property: nothing was recorded. A partial write here would be the
    // silent half-state — a journal row whose chunks the GC then sweeps.
    let changes = state
        .db
        .get_sync_changes_for_folder(fs_id, 0, None)
        .await
        .unwrap();
    assert!(
        changes.is_empty(),
        "the refused record must leave no `sync_changes` row on the rail"
    );
}

/// A reserved (`__`) NAME alone buys no plaintext-path exemption from the S9
/// flip — only a **custody copy** (`is_reserved_custody_copy`:
/// flag AND name) does, because that is the one set class whose paths are
/// coordinator-synthetic segment names rather than user content
/// (`encryption-at-rest.md` § Conformance, the held-for-friends row: "a backup
/// *destination* never receives real paths").
///
/// **Why this pin exists (the S9 forward-watch).**
/// At the flip the exemption in `record_change_core` keyed on the bare
/// reserved name, while its stated justification was about *authorship*. The
/// only thing stopping a reserved-named, non-backup set from resting a
/// plaintext `path` in `sync_changes` was the wholesale reserved-rail refusal
/// *after* the exemption — a refusal that exists for a different hazard (the
/// GC's direct-blob classification) and that W2.3 (account-data-plane.md § Workstreams)-style explicit routing is
/// already licensed to supersede. Had it ever been relaxed, the write
/// exemption and the S9 scrub's reserved-rail inventory exemption would have
/// composed into exactly the plaintext parking lot the flip removes. Keying
/// the exemption on the custody-routing predicate makes that composition
/// unrepresentable, and this test is the observable: a sealless record on a
/// non-backup reserved rail now fails the **seal gate** (`path_seal_required`),
/// which runs *before* the reserved-rail refusal. Revert the predicate to the
/// bare name and this reds with `invalid_request` instead — the refusal
/// catching what the exemption let through.
///
/// The positive control — a reserved backup destination DOES record sealless —
/// is `conformance_backup_writer_grant.rs` / `conformance_backup_generation_client.rs`
/// (`FedBackupChangesRecordRequest { path_sealed: None, .. }` accepted).
#[tokio::test]
async fn changes_record_grants_no_plaintext_exemption_to_a_reserved_rail() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    folder_handlers::register_folders_handlers(&mut b);
    sync_handlers::register_sync_handlers(&mut b);
    let router = b.build();

    let actor = [0x1e_u8; 32];
    let device = [0x2e_u8; 32];
    state
        .db
        .register_sync_device(&actor, &device, "laptop", None, "write")
        .await
        .unwrap();
    let fs_id = state
        .db
        .get_or_create_reserved_folder(&actor, "index")
        .await
        .unwrap();

    let err = dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.sync.changes.record",
        encode(&fauna_protocol::sync::SyncChangeRecordRequest {
            folder: "__index".into(),
            device_id: hex::encode(device),
            path: "__index/mail/seg-00000001.idx".into(),
            manifest_hash: Some("ab".repeat(32)),
            size_bytes: 4096,
            change_type: "create".into(),
            path_sealed: None,
            ..Default::default()
        }),
    )
    .await
    .expect_err("a sealless record on a non-backup reserved rail must be refused");
    assert_eq!(
        err.code, "fauna.sync.path_seal_required",
        "the reserved name must not exempt a non-backup rail from the S9 seal gate \
         (a `fauna.sync.invalid_request` here means the exemption let the sealless \
         record through and only the later reserved-rail refusal caught it)"
    );

    let changes = state
        .db
        .get_sync_changes_for_folder(fs_id, 0, None)
        .await
        .unwrap();
    assert!(
        changes.is_empty(),
        "the refused record must leave no `sync_changes` row on the rail"
    );
}

// ── replay metadata ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn replay_metadata_matches_spec() {
    let (router, _state) = router_and_state().await;
    for kind in ALL_KINDS {
        let m = router.kind_meta(kind).expect("kind registered");
        assert!(!m.forbid_replay, "{kind} does not forbid replay");
        assert_eq!(m.default_deadline, std::time::Duration::from_secs(5));
    }
}

// ── allowlist ────────────────────────────────────────────────────────────────────

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

// ─────────────────────────────────────────────────────────────────────────────
// Owner-authed lazy provisioning of a reserved backup destination set
//
// `docs/goal/architecture/message-segment-store.md` § Client-device custodian
// (pull) → *Restore*: "the reserved sets get-or-create idempotently server-side
// (the federation relay's lazy-provision rule applied to the owner-authed arm)".
// Its consumer is the re-seed delivery leg (`fauna_sync_engine::reseed`), which
// pushes a custodian device's corpus onto a nest that holds nothing for the
// owner yet — enroll-first, so there is no pre-enrollment write surface to mint
// the set through, and the first custody record must do it.
// ─────────────────────────────────────────────────────────────────────────────

/// An `AppState` with a real blob store, so the custody arm's server-derived
/// charge can actually be computed (it refuses rather than charging zero when
/// the bytes are not held, which is the whole point of deriving it).
async fn state_with_blob_store() -> (Arc<AppState>, tempfile::TempDir) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let tmp = tempfile::tempdir().unwrap();
    let svc = fauna_nest::backup::service::BackupService::new(
        db.clone(),
        None,
        false,
        tmp.path().to_path_buf(),
        None,
    )
    .unwrap();
    let mut st = AppState::for_test(db);
    st.backup_service = Some(Arc::new(svc));
    (Arc::new(st), tmp)
}

/// Put a blob the way the chunk/manifest POST routes do — at-rest framed in the
/// store, wire length metered in `blob_metadata`. This stands in for the
/// delivery leg's byte push, which lands on those routes.
async fn seed_blob(state: &Arc<AppState>, body: &[u8], kind: &str) -> [u8; 32] {
    use fauna_core::data::ContentHash;
    let store = state.backup_service.as_ref().unwrap().local_blob_store();
    let digest = ContentHash::of_raw(body).digest();
    let framed = fauna_nest::backup::encode_blob(body, None, false).unwrap();
    store
        .put(&ContentHash::from_digest_raw(digest), &framed)
        .await
        .unwrap();
    state
        .db
        .put_blob_metadata(&digest, body.len() as i64, kind, None, None)
        .await
        .unwrap();
    digest
}

/// Seed one chunk plus a manifest naming it, exactly as a delivered segment
/// looks after the leg's byte push. Returns the manifest hash the custody
/// record carries.
async fn seed_delivered_segment(state: &Arc<AppState>) -> [u8; 32] {
    use fauna_core::chunk::ChunkManifest;
    use fauna_core::data::ContentHash;
    let chunk = seed_blob(state, &[0x5Cu8; 512], "chunk").await;
    let manifest = ChunkManifest {
        file_hash: ContentHash::from_digest_raw([0x0Fu8; 32]),
        total_size: 512,
        chunk_hashes: vec![ContentHash::from_digest_raw(chunk)],
        chunk_sizes: vec![512],
        stored_hashes: None,
        sealed_hashes: None,
        min_reader: None,
    };
    let body = fauna_core::encoding::canonical_encode(&manifest).unwrap();
    seed_blob(state, &body, "manifest").await
}

fn custody_record(folder: &str, device: [u8; 32], path: &str, manifest: [u8; 32]) -> Bytes {
    encode(&fauna_protocol::sync::SyncChangeRecordRequest {
        folder: folder.into(),
        device_id: hex::encode(device),
        path: path.into(),
        manifest_hash: Some(hex::encode(manifest)),
        size_bytes: 512,
        change_type: "create".into(),
        // No `path_sealed`: a reserved backup destination's custody paths are
        // machine-authored routing keys — the one class exempt from the S9 seal
        // gate. This is exactly what `reseed::ReseedDelivery::record` sends.
        ..Default::default()
    })
}

fn record_router() -> RpcRouter {
    let mut b = RpcRouter::builder();
    folder_handlers::register_folders_handlers(&mut b);
    sync_handlers::register_sync_handlers(&mut b);
    b.build()
}

/// **The door the re-seed delivery leg needs.** A nest that holds no set for the
/// owner takes their first custody record: the reserved backup destination set
/// is minted as a custody copy and the row lands on the custody plane with
/// `seq = 0` — never in the `sync_changes` device-pull feed, which is what makes
/// the device-pull exclusion structural.
#[tokio::test]
async fn a_first_custody_record_provisions_the_owners_reserved_backup_set() {
    let (state, _tmp) = state_with_blob_store().await;
    let router = record_router();
    let actor = [0x7au8; 32];
    let device = [0x7du8; 32];
    state
        .db
        .register_sync_device(&actor, &device, "ipad", None, "write")
        .await
        .unwrap();

    // Precondition: this nest holds nothing named `__mail` for the owner.
    assert!(
        state
            .db
            .get_folder_for_actor("__mail", &actor)
            .await
            .unwrap()
            .is_none(),
        "the fixture must start with no set, or this proves nothing"
    );

    let manifest = seed_delivered_segment(&state).await;
    let reply: fauna_protocol::sync::SyncChangeRecordReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.sync.changes.record",
            custody_record(
                "__mail",
                device,
                &format!("{}/seg-00000001.dat", hex::encode(actor)),
                manifest,
            ),
        )
        .await
        .expect("the first custody record must provision its set, not 404"),
    )
    .unwrap();
    assert_eq!(
        reply.seq, 0,
        "backup custody is write-only, never sequenced"
    );

    let fs = state
        .db
        .get_folder_for_actor("__mail", &actor)
        .await
        .unwrap()
        .expect("the set must exist after the record");
    assert!(
        fs.custody_copy,
        "the provisioner marks the row a custody copy"
    );

    let custody = state
        .db
        .list_backup_custody(&actor, None, 10)
        .await
        .unwrap();
    assert_eq!(
        custody.len(),
        1,
        "the record must land on the custody plane"
    );

    let changes = state
        .db
        .get_sync_changes_for_folder(fs.id, 0, None)
        .await
        .unwrap();
    assert!(
        changes.is_empty(),
        "destination custody must never enter the device-pull feed"
    );
}

/// Idempotent, exactly as the federated arm is: a second delivery pass over the
/// same set does not mint a second set and does not fail. A re-seed that tore
/// mid-delivery re-runs, and re-running must converge.
#[tokio::test]
async fn provisioning_is_idempotent_across_delivery_passes() {
    let (state, _tmp) = state_with_blob_store().await;
    let router = record_router();
    let actor = [0x7bu8; 32];
    let device = [0x7du8; 32];
    state
        .db
        .register_sync_device(&actor, &device, "ipad", None, "write")
        .await
        .unwrap();
    let manifest = seed_delivered_segment(&state).await;
    let path = format!("{}/seg-00000001.dat", hex::encode(actor));

    for pass in 0..2 {
        dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.sync.changes.record",
            custody_record("__mail", device, &path, manifest),
        )
        .await
        .unwrap_or_else(|e| panic!("pass {pass} failed: {e:?}"));
    }

    let custody = state
        .db
        .list_backup_custody(&actor, None, 10)
        .await
        .unwrap();
    assert_eq!(custody.len(), 1, "custody is latest-per-path, not append");
}

/// **The narrowness that makes the door safe.** Only a name one of the two
/// shared derivations can produce is mintable — not any `__` name, and not a
/// reserved rail that is no backup destination at all.
///
/// ⚠ The folder-mirror axis is **admitted here**, and deliberately so: it was
/// added to this owner-authed door (goal
/// `message-segment-store.md` § Client-device custodian → *Restore*, ratified
/// 2026-08-22) because re-seed delivers a custodian's covered-folder mirrors
/// under exactly those names. Its name embeds a *source nest id*, which the
/// **federated** arm must never trust a writer to declare — that refusal lives
/// in `parse_reserved_backup_set_name` and is untouched — but on this door the
/// id is safe: the set is minted under the caller's own actor id and paid for
/// by their own quota, so a wrong id mis-files the owner's own corpus for their
/// own later materialize and reaches nobody else. The positive half is
/// [`a_folder_mirror_backup_set_name_is_mintable_on_the_owner_authed_door`];
/// what stays refused is the rowid floor, below.
///
/// Each refusal must also leave no set behind.
#[tokio::test]
async fn only_a_derivable_segment_set_name_is_mintable() {
    let (state, _tmp) = state_with_blob_store().await;
    let router = record_router();
    let actor = [0x7cu8; 32];
    let device = [0x7du8; 32];
    state
        .db
        .register_sync_device(&actor, &device, "ipad", None, "write")
        .await
        .unwrap();
    let manifest = seed_delivered_segment(&state).await;

    for name in [
        // Not a reserved-backup-set name at all.
        "__nope",
        // Reserved feature rails — never custody copies, and minting a
        // custody copy under these names would hand a client the very door
        // `changes_record_refuses_a_reserved_rail_so_no_client_writer_can_use_it`
        // exists to keep shut.
        "__index",
        "__config",
        // The folder-mirror axis with a folder id BELOW the rowid floor. The
        // shape is otherwise correct and `/7` is admitted (see the positive
        // test), so this pins the floor at the door: the derivation is total
        // and renders `0` and `-1` happily, and admitting one would let an
        // owner-authed caller mint reserved-shaped sets naming folders that can
        // never exist.
        "__folder/aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899/0",
        // A malformed conv scope.
        "__conv/not-hex",
        // A 64-char but UPPERCASE conv scope. `hex` decodes it happily, so only
        // the parser's re-derivation catches it — and it must, because every
        // other arm of the stack derives the lowercase spelling: minting this
        // would give one channel two custody sets that never converge.
        "__conv/AABBCCDDEEFF00112233445566778899AABBCCDDEEFF00112233445566778899",
        // A conv scope of the wrong length.
        "__conv/aabb",
        // An ordinary folder that simply does not exist.
        "photos",
    ] {
        let err = dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.sync.changes.record",
            custody_record(name, device, "aa/seg-00000001.dat", manifest),
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.code, "fauna.sync.not_found",
            "{name} must not be mintable by a client write"
        );
        assert!(
            state
                .db
                .get_folder_for_actor(name, &actor)
                .await
                .unwrap()
                .is_none(),
            "the refused record for {name} must leave no set behind"
        );
    }
}

/// The folder-mirror axis at the owner-authed door — the half
/// that a change opened and left with no coverage in this file, while the
/// refusal list above went on asserting the opposite until it was corrected.
/// Re-seed delivers a custodian's covered-folder mirrors under exactly this
/// name, so a 404 here would have failed every folder on delivery to a fresh
/// nest.
#[tokio::test]
async fn a_folder_mirror_backup_set_name_is_mintable_on_the_owner_authed_door() {
    let (state, _tmp) = state_with_blob_store().await;
    let router = record_router();
    let actor = [0x7fu8; 32];
    let device = [0x7du8; 32];
    state
        .db
        .register_sync_device(&actor, &device, "ipad", None, "write")
        .await
        .unwrap();
    let manifest = seed_delivered_segment(&state).await;

    // Built through the one shared derivation, never hand-spelled — a literal
    // here would pass while the formatter drifted underneath it.
    let source_nest = [0xA5u8; 32];
    let name = fauna_core::data::folder_backup_set_name(&source_nest, 7);
    dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.sync.changes.record",
        custody_record(&name, device, "aa/seg-00000001.dat", manifest),
    )
    .await
    .expect("a derivable folder-mirror set name must provision");

    let fs = state
        .db
        .get_folder_for_actor(&name, &actor)
        .await
        .unwrap()
        .expect("the folder-mirror set must exist after the record");
    assert!(
        fs.custody_copy,
        "the provisioner marks the row a custody copy"
    );
}

/// **The record the re-seed delivery leg's covered-folder arm actually sends**
/// (`fauna_sync_engine::reseed::ReseedDelivery::record_folder`, built
/// 2026-08-29) — a folder-mirror custody record that CARRIES a `path_sealed`,
/// which no test at this door had ever exercised.
///
/// Every other custody record in this file sends `path_sealed: None`, because
/// the segment arm's paths are machine-authored routing keys and the reserved
/// backup class is exempt from the S9 seal gate. The folder arm is the
/// exception in the ratified design: a covered-folder materialize is pure row
/// re-homing from `(path_hash, path_sealed, manifest_hash)`, and a live folder
/// set is NOT one of the three plaintext-path classes, so a mirror row that
/// arrived without the name could never be re-homed at all
/// (`../../../docs/goal/architecture/message-segment-store.md` § Client-device
/// custodian (pull) → *Restore*).
///
/// The exemption is written as "sealless is allowed here", not "sealed is
/// refused here" — but that is a property of one boolean in
/// `record_change_core`, and nothing pinned it. This pins it: the seal rides
/// the same door, the same provisioning branch and the same charge derivation,
/// and lands on the custody plane rather than the device-pull feed.
///
/// The read-back is asserted too, as of 2026-08-30: the column's first reader
/// landed with the covered-folder materialize arm, which selects it through
/// `list_backup_custody_in_set` and re-homes it verbatim onto the live row. The
/// end-to-end proof that the re-homed name is the source's own is this file's
/// sibling in `nest_backup_coordinator.rs` § Phase 3, the covered-folder arm;
/// what belongs *here* is the door-level half — the seal survives the record
/// door and rests where the reader looks for it.
#[tokio::test]
async fn a_folder_mirror_custody_record_may_carry_its_sealed_name() {
    let (state, _tmp) = state_with_blob_store().await;
    let router = record_router();
    let actor = [0x71u8; 32];
    let device = [0x7du8; 32];
    state
        .db
        .register_sync_device(&actor, &device, "ipad", None, "write")
        .await
        .unwrap();
    let manifest = seed_delivered_segment(&state).await;

    let source_nest = [0xA6u8; 32];
    let name = fauna_core::data::folder_backup_set_name(&source_nest, 11);
    // The leaf the delivery leg sends: the SOURCE row's `path_hash`,
    // hex-spelled — exactly what the source nest's own coordinator records
    // (`segment_backup.rs`'s `rest_path`), so the two corpora key alike.
    let leaf = hex::encode([0x3Bu8; 32]);
    let sealed_name = b"an opaque SealedLabel over the source path".to_vec();
    let sealed_expected = sealed_name.clone();

    let req = fauna_protocol::sync::SyncChangeRecordRequest {
        folder: name.clone(),
        device_id: hex::encode(device),
        path: leaf.clone(),
        manifest_hash: Some(hex::encode(manifest)),
        size_bytes: 512,
        change_type: "create".into(),
        path_sealed: Some(fauna_protocol::ByteBuf::from(sealed_name)),
        ..Default::default()
    };
    let reply: fauna_protocol::sync::SyncChangeRecordReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.sync.changes.record",
            encode(&req),
        )
        .await
        .expect("a sealed folder-mirror custody record must be accepted, not refused"),
    )
    .unwrap();
    assert_eq!(
        reply.seq, 0,
        "backup custody is write-only, never sequenced"
    );

    let fs = state
        .db
        .get_folder_for_actor(&name, &actor)
        .await
        .unwrap()
        .expect("the folder-mirror set must exist after the record");
    assert!(
        fs.custody_copy,
        "the provisioner marks the row a custody copy"
    );

    let custody = state
        .db
        .list_backup_custody(&actor, None, 10)
        .await
        .unwrap();
    assert_eq!(
        custody.len(),
        1,
        "the record must land on the custody plane"
    );
    assert_eq!(custody[0].folder_name, name);
    assert_eq!(
        custody[0].path_hash,
        fauna_core::sync::path_hash(&leaf).to_vec(),
        "the custody row keys on a hash of the LEAF alone — delivering the \
         store's full local path instead would fork the target's keying"
    );

    let changes = state
        .db
        .get_sync_changes_for_folder(fs.id, 0, None)
        .await
        .unwrap();
    assert!(
        changes.is_empty(),
        "a sealed record must not slip into the device-pull feed either"
    );

    // The read-back, through the per-set read the materialize arm uses — the
    // seal has to be RETRIEVABLE, not merely accepted, or the folder arm has
    // nothing to re-home and the whole covered-folder ceremony stops at
    // delivery.
    let in_set = state
        .db
        .list_backup_custody_in_set(&actor, &name)
        .await
        .unwrap();
    assert_eq!(in_set.len(), 1);
    assert_eq!(
        in_set[0].path_sealed.as_deref(),
        Some(&sealed_expected[..]),
        "the sealed name rests verbatim and reads back verbatim: this nest holds \
         no key that opens it, so it can only ever hand back exactly what arrived"
    );
    assert_eq!(
        in_set[0].path.as_deref(),
        Some(leaf.as_str()),
        "and beside it the leaf, which IS the source's path hash hex-spelled — \
         the address the re-homed live row is keyed on"
    );
}

/// The channel-scoped kind's own branch, which the fixed-name kinds do not
/// exercise: a canonical `__conv/<64 lowercase hex>` name IS mintable, so the
/// refusals above are the parser rejecting *malformed* names rather than the
/// conv branch being dead. Pairs with the uppercase-scope refusal: same 32
/// bytes, one canonical spelling, one custody set.
#[tokio::test]
async fn a_canonical_channel_scoped_backup_set_name_is_mintable() {
    let (state, _tmp) = state_with_blob_store().await;
    let router = record_router();
    let actor = [0x7eu8; 32];
    let device = [0x7du8; 32];
    state
        .db
        .register_sync_device(&actor, &device, "ipad", None, "write")
        .await
        .unwrap();
    let manifest = seed_delivered_segment(&state).await;

    let channel = [0xC0u8; 32];
    let name = format!("__conv/{}", hex::encode(channel));
    dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.sync.changes.record",
        custody_record(&name, device, "aa/seg-00000001.dat", manifest),
    )
    .await
    .expect("a canonical conv backup set name must provision");

    let fs = state
        .db
        .get_folder_for_actor(&name, &actor)
        .await
        .unwrap()
        .expect("the conv set must exist after the record");
    assert!(
        fs.custody_copy,
        "the provisioner marks the row a custody copy"
    );
}
