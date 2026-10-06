//! Production-flow round-trips for the **public folder read plane** — the
//! folders re-model phase 4 slice 4f-i (`docs/goal/behavior/folders.md`
//! § Publicly-synced follow owns the behavior; `docs/goal/architecture/
//! federation.md` § The public folder read plane owns the kinds and gates).
//!
//! The four oracles the ratified design names, each driven through the real
//! `fauna.folders.public.fetch` handler over a real `CacheDb`:
//!
//! 1. **The gate** — a private (or shared-but-not-declassified, or absent, or
//!    reserved) folder addressed publicly answers exactly what an *absent*
//!    folder answers: one indistinguishable `not_found` (ST-RES-1 held).
//! 2. **The floor** — rows recorded before the most recent flip-to-public are
//!    never served, and a *second* flip re-stamps the floor higher.
//! 3. **The strip** — `device_id`, `author_actor_id`, `path_sealed` and
//!    `content_key_version` carry no value on this plane.
//! 4. **Flip-back is revoke** — the next read after `public → private` refuses,
//!    and a re-flip resumes the follow under the same stable `folder_id`.
//!
//! Plus the `federation_leg` module at the bottom, which drives the twin
//! `fauna.federation.folder.public.fetch` through the real `FederationRouter`.
//! Both kinds share one core, so what that module is really pinning is the gate
//! that is **absent**: every other kind in the folder family runs
//! `require_foreign_member` first, and this one deliberately does not — so a
//! nest with no relationship to the owner reads a public folder, and the
//! audience check is the only thing between it and a sealed one.
//!
//! Tier: tier_3 (real `fauna-nest` `AppState` + real `CacheDb` — no mocks).

mod common;
use common::dispatch;
use common::encode;

use std::sync::Arc;

use bytes::Bytes;

use fauna_nest::{db::CacheDb, folder_handlers, routes::AppState, rpc_router::RpcRouter};
use fauna_protocol::{
    RpcError, decode_strict as decode, encode_canonical,
    folders::{
        FolderCreateReply, FolderCreateRequest, FolderUpdateReply, FolderUpdateRequest,
        FoldersPublicFetchReply, FoldersPublicFetchRequest,
    },
};

/// The folder's owner.
const OWNER: [u8; 32] = [22u8; 32];
/// A *different* actor doing the following — the whole point of the plane is
/// that it serves someone who is neither the owner nor a roster member.
const FOLLOWER: [u8; 32] = [77u8; 32];

async fn router_and_state() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    folder_handlers::register_folders_handlers(&mut b);
    (b.build(), state)
}

async fn create_folder(
    router: &RpcRouter,
    state: &Arc<AppState>,
    name: &str,
    audience: Option<&str>,
) -> i64 {
    let reply: FolderCreateReply = decode(
        &dispatch(
            router,
            Arc::clone(state),
            OWNER,
            "fauna.folders.create",
            encode(&FolderCreateRequest {
                name: name.into(),
                retention_policy: None,
                audience: audience.map(str::to_string),
                ..Default::default()
            }),
        )
        .await
        .expect("create ok"),
    )
    .unwrap();
    reply.id
}

async fn set_audience(router: &RpcRouter, state: &Arc<AppState>, name: &str, audience: &str) {
    let reply: FolderUpdateReply = decode(
        &dispatch(
            router,
            Arc::clone(state),
            OWNER,
            "fauna.folders.update",
            encode(&FolderUpdateRequest {
                name: name.into(),
                audience: Some(audience.into()),
                ..Default::default()
            }),
        )
        .await
        .unwrap_or_else(|e| panic!("audience → {audience} ok, got {e:?}")),
    )
    .unwrap();
    assert!(reply.ok);
}

/// Record one change on the owner's folder, returning its `seq`. `nth` varies
/// the path so each row is its own file (a same-path re-record would supersede).
async fn record_change(state: &Arc<AppState>, folder_id: i64, nth: u8) -> i64 {
    state
        .db
        .record_sync_change(
            &OWNER,
            &[nth; 32],
            Some(&[nth.wrapping_add(0x40); 32]),
            100 + nth as i64,
            "create",
            Some(folder_id),
            // A device id, an author (the recorder) — precisely the identity
            // metadata the strip must remove downstream.
            Some(&[0xDD; 32]),
            Some(&format!("notes/{nth}.md")),
        )
        .await
        .expect("record ok")
}

async fn fetch_by_name(
    router: &RpcRouter,
    state: &Arc<AppState>,
    name: &str,
    since: i64,
) -> Result<FoldersPublicFetchReply, RpcError> {
    let bytes = dispatch(
        router,
        Arc::clone(state),
        FOLLOWER,
        "fauna.folders.public.fetch",
        encode(&FoldersPublicFetchRequest {
            owner_actor_id: Some(hex::encode(OWNER)),
            folder_name: Some(name.into()),
            since,
            ..Default::default()
        }),
    )
    .await?;
    Ok(decode(&bytes).unwrap())
}

async fn fetch_by_id(
    router: &RpcRouter,
    state: &Arc<AppState>,
    folder_id: i64,
) -> Result<FoldersPublicFetchReply, RpcError> {
    let bytes = dispatch(
        router,
        Arc::clone(state),
        FOLLOWER,
        "fauna.folders.public.fetch",
        encode(&FoldersPublicFetchRequest {
            folder_id: Some(folder_id),
            ..Default::default()
        }),
    )
    .await?;
    Ok(decode(&bytes).unwrap())
}

// ── Oracle 1: the gate, and that every refusal is the SAME refusal ──────────

/// A private folder, a shared-but-not-declassified folder, a reserved rail and
/// a folder that does not exist must all answer **identically**. Anything else
/// is an existence oracle over private state.
#[tokio::test]
async fn every_non_public_address_answers_exactly_what_absence_answers() {
    let (router, state) = router_and_state().await;
    create_folder(&router, &state, "private-docs", None).await;

    let absent = fetch_by_name(&router, &state, "no-such-folder", 0)
        .await
        .expect_err("an absent folder refuses");
    let private = fetch_by_name(&router, &state, "private-docs", 0)
        .await
        .expect_err("a private folder refuses");
    let reserved = fetch_by_name(&router, &state, "__config", 0)
        .await
        .expect_err("a reserved rail refuses");

    assert_eq!(
        private.code, absent.code,
        "a private folder must be indistinguishable from an absent one"
    );
    assert_eq!(
        private.details, absent.details,
        "…including the human-readable half — the detail string is an oracle too"
    );
    assert_eq!(reserved.code, absent.code);
    assert_eq!(reserved.details, absent.details);
    assert_eq!(private.code, "fauna.folders.not_found");
}

/// The owner-scoping half: a public folder is addressed by `(owner, name)`, so
/// the *same name* under a different owner must not resolve.
#[tokio::test]
async fn the_address_is_owner_scoped_not_name_global() {
    let (router, state) = router_and_state().await;
    create_folder(&router, &state, "site", Some("public")).await;

    let bytes = dispatch(
        &router,
        Arc::clone(&state),
        FOLLOWER,
        "fauna.folders.public.fetch",
        encode(&FoldersPublicFetchRequest {
            // Right name, wrong owner.
            owner_actor_id: Some(hex::encode([0x99u8; 32])),
            folder_name: Some("site".into()),
            ..Default::default()
        }),
    )
    .await;
    let err = bytes.expect_err("a name under the wrong owner refuses");
    assert_eq!(err.code, "fauna.folders.not_found");

    // …and the right owner resolves, so the refusal above is the scoping and
    // not a broken fixture.
    fetch_by_name(&router, &state, "site", 0)
        .await
        .expect("the owner's own public folder serves");
}

/// An address naming neither form is a **caller bug**, and must NOT fold into
/// the plane's uniform `not_found` — that would tell a buggy client its request
/// was fine and the folder was missing.
#[tokio::test]
async fn an_address_less_request_is_malformed_not_not_found() {
    let (router, state) = router_and_state().await;
    let err = dispatch(
        &router,
        Arc::clone(&state),
        FOLLOWER,
        "fauna.folders.public.fetch",
        encode(&FoldersPublicFetchRequest::default()),
    )
    .await
    .expect_err("no address refuses");
    assert_eq!(err.code, "fauna.folders.invalid_request");
}

// ── Oracle 2: the public floor ─────────────────────────────────────────────

/// Rows recorded **before** the flip-to-public are never served, rows after it
/// are, and a second flip re-stamps the floor higher.
#[tokio::test]
async fn the_floor_hides_the_private_era_and_re_stamps_on_each_flip() {
    let (router, state) = router_and_state().await;
    let id = create_folder(&router, &state, "site", None).await;

    // Two rows recorded while the folder was PRIVATE.
    let before_a = record_change(&state, id, 1).await;
    let before_b = record_change(&state, id, 2).await;

    set_audience(&router, &state, "site", "public").await;

    // …and two after the declassification.
    let after_a = record_change(&state, id, 3).await;
    let after_b = record_change(&state, id, 4).await;

    let reply = fetch_by_name(&router, &state, "site", 0)
        .await
        .expect("serves");
    let served: Vec<i64> = reply.changes.iter().map(|c| c.seq).collect();
    assert_eq!(
        served,
        vec![after_a, after_b],
        "only the rows recorded from the flip forward cross the public plane"
    );
    assert!(
        !served.contains(&before_a) && !served.contains(&before_b),
        "the private era's rows ({before_a}, {before_b}) must never be served"
    );

    // Flip back and forward again: the floor re-stamps at the CURRENT head, so
    // the rows served during the first public window fall below the new floor.
    set_audience(&router, &state, "site", "private").await;
    set_audience(&router, &state, "site", "public").await;
    let after_reflip = record_change(&state, id, 5).await;

    let reply = fetch_by_name(&router, &state, "site", 0)
        .await
        .expect("serves");
    let served: Vec<i64> = reply.changes.iter().map(|c| c.seq).collect();
    assert_eq!(
        served,
        vec![after_reflip],
        "each →public transition stamps a NEW floor at the then-current head"
    );
}

/// Re-asserting `public` on an already-public folder must **not** re-stamp —
/// that would silently un-serve everything published during the public window.
#[tokio::test]
async fn re_asserting_public_does_not_raise_the_floor() {
    let (router, state) = router_and_state().await;
    let id = create_folder(&router, &state, "site", Some("public")).await;

    let published = record_change(&state, id, 1).await;
    set_audience(&router, &state, "site", "public").await;

    let reply = fetch_by_name(&router, &state, "site", 0)
        .await
        .expect("serves");
    assert_eq!(
        reply.changes.iter().map(|c| c.seq).collect::<Vec<_>>(),
        vec![published],
        "a no-op re-assert of the current audience must leave the floor alone"
    );
}

/// A **born-public** folder has floor 0 — nothing preceded its declassification,
/// so its whole history serves.
#[tokio::test]
async fn a_born_public_folder_serves_from_its_very_first_row() {
    let (router, state) = router_and_state().await;
    let id = create_folder(&router, &state, "site", Some("public")).await;
    let first = record_change(&state, id, 1).await;
    let second = record_change(&state, id, 2).await;

    let reply = fetch_by_name(&router, &state, "site", 0)
        .await
        .expect("serves");
    assert_eq!(
        reply.changes.iter().map(|c| c.seq).collect::<Vec<_>>(),
        vec![first, second]
    );
}

/// The caller's `since` cursor composes with the floor rather than defeating it:
/// the served floor is `max(since, public_floor_seq)`, so a follower cannot
/// reach below the floor by asking for `since = 0`.
#[tokio::test]
async fn the_since_cursor_cannot_page_below_the_floor() {
    let (router, state) = router_and_state().await;
    let id = create_folder(&router, &state, "site", None).await;
    record_change(&state, id, 1).await;
    set_audience(&router, &state, "site", "public").await;
    let after = record_change(&state, id, 2).await;

    // `since = 0` is the widest possible ask.
    let widest = fetch_by_name(&router, &state, "site", 0)
        .await
        .expect("serves");
    assert_eq!(
        widest.changes.iter().map(|c| c.seq).collect::<Vec<_>>(),
        vec![after]
    );

    // …and the cursor still advances normally above the floor.
    let past_it = fetch_by_name(&router, &state, "site", after)
        .await
        .expect("serves");
    assert!(
        past_it.changes.is_empty(),
        "a cursor at the head yields nothing further"
    );
}

// ── Oracle 3: the stripped projection ──────────────────────────────────────

/// The four identity/key fields carry no value on this plane — asserted against
/// a row that demonstrably HAS them (the control), so this witnesses the strip
/// rather than a fixture that never set them.
#[tokio::test]
async fn the_public_projection_strips_identity_and_key_metadata() {
    let (router, state) = router_and_state().await;
    let id = create_folder(&router, &state, "site", Some("public")).await;
    record_change(&state, id, 1).await;

    // Control: the row as recorded really does carry a device id and an author.
    let raw = state
        .db
        .get_sync_changes_for_folder(id, 0, None)
        .await
        .expect("read ok");
    assert_eq!(raw.len(), 1);
    assert!(
        raw[0].device_id.is_some(),
        "the fixture must record a device id, else the strip proves nothing"
    );

    let reply = fetch_by_name(&router, &state, "site", 0)
        .await
        .expect("serves");
    let change = &reply.changes[0];
    assert_eq!(change.device_id, None, "the owner's device fleet");
    assert_eq!(change.author_actor_id, None, "the authorship map");
    assert_eq!(change.path_sealed, None, "the sealed label + its salt");
    assert_eq!(change.content_key_version, None, "the M2 generation");

    // What a follower legitimately needs still rides.
    assert_eq!(change.path.as_deref(), Some("notes/1.md"));
    assert!(
        change.manifest_hash.is_some(),
        "the bytes are fetched by hash"
    );
    assert_eq!(change.size_bytes, 101);
}

/// The reply's folder meta is what a follower **pins**: the stable `folder_id`,
/// the plaintext name, and the home nest's identity for the byte-plane pin.
#[tokio::test]
async fn the_reply_carries_the_meta_a_follower_pins() {
    let (router, state) = router_and_state().await;
    let id = create_folder(&router, &state, "site", Some("public")).await;

    let reply = fetch_by_name(&router, &state, "site", 0)
        .await
        .expect("serves");
    assert_eq!(reply.folder_id, id, "the stable id a follow pins");
    assert_eq!(reply.name, "site");
    assert_eq!(
        reply.home_nest_actor_id,
        Some(hex::encode(state.nest_identity.public_key_bytes())),
        "the byte-plane SPKI-pin trust root"
    );
}

/// Both address forms resolve the **same** folder, and `folder_id` wins when
/// both ride — the precedence a follower depends on once it has pinned.
///
/// ⚠ The design's stated *reason* for pinning — "a later rename never breaks an
/// established follow" — is **not assertable today**: there is no folder-rename
/// path in the product at all (no `fauna.folders.rename` kind, no rename DAO).
/// Driving it would mean hand-writing SQL, which would pin a mechanism that
/// does not exist rather than a behavior. When a rename lands, this test is
/// where its follow-survival oracle belongs.
#[tokio::test]
async fn the_two_address_forms_resolve_the_same_folder_and_id_wins() {
    let (router, state) = router_and_state().await;
    let id = create_folder(&router, &state, "site", Some("public")).await;
    let published = record_change(&state, id, 1).await;

    let by_name = fetch_by_name(&router, &state, "site", 0)
        .await
        .expect("serves");
    let by_id = fetch_by_id(&router, &state, id).await.expect("serves");
    assert_eq!(by_name.folder_id, by_id.folder_id);
    assert_eq!(by_name.name, by_id.name);
    assert_eq!(
        by_id.changes.iter().map(|c| c.seq).collect::<Vec<_>>(),
        vec![published]
    );

    // Precedence: a request carrying BOTH is answered by the pinned id, so a
    // stale cached name on the follower can never redirect an established
    // follow onto a different folder.
    let other = create_folder(&router, &state, "other-site", Some("public")).await;
    let both: FoldersPublicFetchReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            FOLLOWER,
            "fauna.folders.public.fetch",
            encode(&FoldersPublicFetchRequest {
                folder_id: Some(other),
                owner_actor_id: Some(hex::encode(OWNER)),
                folder_name: Some("site".into()),
                ..Default::default()
            }),
        )
        .await
        .expect("serves"),
    )
    .unwrap();
    assert_eq!(both.folder_id, other, "the pinned folder_id wins");
    assert_eq!(both.name, "other-site");
}

// ── Oracle 4: flip-back is revoke, immediately ─────────────────────────────

/// The gate reads the folder's *current* audience per request, so a flip-back
/// closes the plane on the very next read — and a re-flip resumes it under the
/// same stable id.
#[tokio::test]
async fn flip_back_revokes_on_the_next_read_and_a_re_flip_resumes() {
    let (router, state) = router_and_state().await;
    let id = create_folder(&router, &state, "site", Some("public")).await;
    record_change(&state, id, 1).await;

    assert!(
        !fetch_by_id(&router, &state, id)
            .await
            .expect("serves while public")
            .changes
            .is_empty()
    );

    set_audience(&router, &state, "site", "private").await;

    let err = fetch_by_id(&router, &state, id)
        .await
        .expect_err("the next read after a flip-back refuses");
    assert_eq!(
        err.code, "fauna.folders.not_found",
        "revoke is indistinguishable from absence — that IS the semantics"
    );

    // Re-flip: the follow resumes against the same id it pinned.
    set_audience(&router, &state, "site", "public").await;
    let resumed = record_change(&state, id, 2).await;
    let reply = fetch_by_id(&router, &state, id)
        .await
        .expect("serves again");
    assert_eq!(reply.folder_id, id);
    assert_eq!(
        reply.changes.iter().map(|c| c.seq).collect::<Vec<_>>(),
        vec![resumed],
        "…above the floor the re-flip stamped"
    );
}

/// **Zero nest-side follower state**: a read leaves the folder row exactly as it
/// found it. Followers are not enumerable and a follower flood cannot grow nest
/// state, so nothing this plane does may write.
/// The owner's audience attestation (`encryption-at-rest.md` § Readable classes
/// → *The declassification is owner-ATTESTED*): the nest **stores and serves it
/// opaquely**. It comes back byte-for-byte on the list; a flip-back keeps it
/// (the next mint must count above it); a bare update leaves it alone; and the
/// only thing the nest refuses is a blob of the wrong SHAPE — it is never the
/// verifier, so a signature it could not check still round-trips.
#[tokio::test]
async fn the_audience_attestation_is_stored_and_served_opaquely() {
    use fauna_protocol::folders::{AudienceAttestation, FoldersListReply, FoldersListRequest};
    use serde_bytes::ByteBuf;

    let (router, state) = router_and_state().await;
    let id = create_folder(&router, &state, "site", None).await;
    // Deliberately NOT a valid signature: the nest must not care.
    let att = AudienceAttestation {
        owner: ByteBuf::from(OWNER.to_vec()),
        folder_id: id,
        counter: 7,
        sig: ByteBuf::from(vec![0xAB; 64]),
    };
    let update = |req: FolderUpdateRequest| {
        dispatch(
            &router,
            Arc::clone(&state),
            OWNER,
            "fauna.folders.update",
            encode(&req),
        )
    };
    let served = || async {
        let reply: FoldersListReply = decode(
            &dispatch(
                &router,
                Arc::clone(&state),
                OWNER,
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
        let row = reply
            .folders
            .into_iter()
            .find(|fs| fs.name == "site")
            .unwrap();
        (row.audience, row.audience_attestation)
    };

    assert_eq!(served().await, ("private".to_string(), None));

    update(FolderUpdateRequest {
        name: "site".into(),
        audience: Some("public".into()),
        audience_attestation: Some(att.clone()),
        ..Default::default()
    })
    .await
    .expect("attested flip ok");
    assert_eq!(served().await, ("public".to_string(), Some(att.clone())));

    set_audience(&router, &state, "site", "private").await;
    assert_eq!(
        served().await,
        ("private".to_string(), Some(att.clone())),
        "a flip-back keeps the last attestation so the next mint counts above it"
    );

    let misshapen = AudienceAttestation {
        sig: ByteBuf::from(vec![0xAB; 4096]),
        ..att.clone()
    };
    let refused = update(FolderUpdateRequest {
        name: "site".into(),
        audience_attestation: Some(misshapen),
        ..Default::default()
    })
    .await
    .expect_err("a misshapen attestation is refused");
    assert_eq!(refused.code, "fauna.folders.invalid_request");
    assert_eq!(served().await.1, Some(att));
}

#[tokio::test]
async fn a_public_read_writes_nothing() {
    let (router, state) = router_and_state().await;
    let id = create_folder(&router, &state, "site", Some("public")).await;
    record_change(&state, id, 1).await;

    let before = state.db.get_folder_by_id(id).await.unwrap().expect("row");
    for _ in 0..3 {
        fetch_by_id(&router, &state, id).await.expect("serves");
    }
    let after = state.db.get_folder_by_id(id).await.unwrap().expect("row");

    assert_eq!(before.audience, after.audience);
    assert_eq!(
        before.public_floor_seq, after.public_floor_seq,
        "a READ must never move the floor"
    );
    assert_eq!(
        state
            .db
            .get_sync_changes_for_folder(id, 0, None)
            .await
            .unwrap()
            .len(),
        1,
        "and must not mint change rows"
    );
}

// ── The FEDERATION leg ─────────────────────────────────────────────────────

/// The federation twin, driven through the real `FederationRouter`.
///
/// Both kinds share one core, which the tests above exercise — what is distinct
/// here, and what could only be got wrong on this plane, is the **gate that is
/// absent**: every other kind in the folder family runs `require_foreign_member`
/// before touching a row. This one deliberately does not, so a nest with no
/// relationship whatsoever to the owner reads a public folder, and the ONLY
/// thing standing between it and a sealed folder is the audience check.
mod federation_leg {
    use super::*;
    use fauna_nest::federation_handlers::{FedFolderPublicFetchReply, FedFolderPublicFetchRequest};
    use fauna_nest::federation_router::FederationRouter;

    /// A peer nest this deployment has never heard of — no pairing, no channel,
    /// no member row anywhere.
    const STRANGER_NEST: [u8; 32] = [0xAB; 32];

    async fn fed_and_client() -> (FederationRouter, RpcRouter, Arc<AppState>) {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let state = Arc::new(AppState::for_test(db));
        let mut fb = FederationRouter::builder();
        fauna_nest::federation_handlers::register_federation_handlers(&mut fb);
        let mut cb = RpcRouter::builder();
        folder_handlers::register_folders_handlers(&mut cb);
        (fb.build(), cb.build(), state)
    }

    async fn fed_fetch(
        router: &FederationRouter,
        state: &Arc<AppState>,
        req: FedFolderPublicFetchRequest,
    ) -> Result<FedFolderPublicFetchReply, RpcError> {
        let meta = router
            .kind_meta("fauna.federation.folder.public.fetch")
            .expect("the kind is registered");
        let bytes = (meta.handler)(
            Arc::clone(state),
            STRANGER_NEST,
            Bytes::from(encode_canonical(&req).unwrap().to_vec()),
        )
        .await?;
        Ok(decode(&bytes).unwrap())
    }

    fn by_name(name: &str) -> FedFolderPublicFetchRequest {
        FedFolderPublicFetchRequest {
            owner_actor_id: Some(hex::encode(OWNER)),
            folder_name: Some(name.into()),
            folder_id: None,
            since: 0,
            limit: 0,
        }
    }

    /// A stranger nest reads a public folder — no membership, no channel, no
    /// pairing — and gets the floor-filtered, stripped page plus this nest's
    /// identity for the byte-plane pin.
    #[tokio::test]
    async fn a_stranger_nest_reads_a_public_folder_with_no_membership_at_all() {
        let (fed, client, state) = fed_and_client().await;

        let id = create_folder(&client, &state, "site", None).await;
        let private_seq = record_change(&state, id, 1).await;
        set_audience(&client, &state, "site", "public").await;
        let public_seq = record_change(&state, id, 2).await;

        let reply = fed_fetch(&fed, &state, by_name("site"))
            .await
            .expect("a stranger nest reads a public folder");

        assert_eq!(reply.folder_id, id);
        assert_eq!(reply.name, "site");
        assert_eq!(
            reply.home_nest_actor_id,
            Some(hex::encode(state.nest_identity.public_key_bytes()))
        );
        let served: Vec<i64> = reply.changes.iter().map(|c| c.seq).collect();
        assert_eq!(
            served,
            vec![public_seq],
            "the floor applies on the federation plane too — seq {private_seq} \
             was recorded while private"
        );
        let change = &reply.changes[0];
        assert_eq!(change.device_id, None);
        assert_eq!(change.author_actor_id, None);
        assert_eq!(change.path_sealed, None);
        assert_eq!(change.content_key_version, None);
    }

    /// …and the audience check is the only thing holding: the same stranger, the
    /// same handler, a folder that is merely not declassified — refused, folded
    /// into the same answer an absent folder gets.
    #[tokio::test]
    async fn the_same_stranger_is_refused_a_non_public_folder() {
        let (fed, client, state) = fed_and_client().await;

        let id = create_folder(&client, &state, "private-docs", None).await;
        record_change(&state, id, 1).await;

        let private = fed_fetch(&fed, &state, by_name("private-docs"))
            .await
            .expect_err("a private folder refuses on the federation plane");
        let absent = fed_fetch(&fed, &state, by_name("no-such-folder"))
            .await
            .expect_err("an absent folder refuses");
        assert_eq!(private.code, absent.code);
        assert_eq!(private.details, absent.details);
        assert_eq!(private.code, "fauna.folders.not_found");

        // Declassifying is the ONLY change needed to flip that refusal into a
        // served page — proving the audience is genuinely the whole gate, and
        // that nothing else about this stranger was ever admissible.
        set_audience(&client, &state, "private-docs", "public").await;
        fed_fetch(&fed, &state, by_name("private-docs"))
            .await
            .expect("the same stranger now reads it");
    }
}
