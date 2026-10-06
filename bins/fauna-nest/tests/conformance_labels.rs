//! Integration round-trip for `fauna.labels.{attach,list}` — a faithful
//! transport migration of `POST /api/v1/labels` + `GET /api/v1/labels/{id}`
//! (`label_routes`). Reaches `CacheDb` directly through the handler via
//! `state.db.{upsert_content_label,get_content_labels}`.
//!
//! Authority for the wire types: `libs/fauna-protocol/src/labels.rs`.
//! Slice: tracked internally (hub Track B8).
//!
//! ## What the attach door owes, beyond the round-trip
//!
//! `attach` writes onto the plane two nest-side verdicts read that bind **every
//! reader**: the mandatory feed spam guard and the nest-as-publisher region fold.
//! Its gate is therefore an OWNERSHIP question, not a class question — the
//! sanctioned producers for a feed post's labels are the post's author and a
//! holder of that author's `content.label-write` grant (`moderation.md` § Per-row
//! badge data path → *Producers unchanged*), and every row must name its writer
//! so the two verdicts can require attribution. The cases below pin both halves
//! .
//!
//! The grant half has a second gate in front of it — the caller-CLASS arm in
//! `bridge_method_allowlist` — and the grant resolver answers only for an
//! enrolled service user, whose class is never `User`. So the grant arm is
//! reachable only if the class arm admits the holder classes, which it did not
//! until the arm was widened: every principal that
//! could hold a grant was refused before the grant was read, and no case here
//! noticed when the check was disabled outright. The `// ── The grant arm`
//! cases drive a granted holder through the REAL dispatch and are admitted;
//! they redden when the grant check is disabled. Because the class gate and the
//! grant check answer the same code, `fauna.labels.permission_denied`, the
//! refusal cases assert on `details` too — the handler's class gate names the
//! class, the grant check's is `label_handlers::GRANT_REFUSAL_DETAILS` — so a
//! case can no longer pass for the wrong reason. (`dispatch` calls the handler
//! directly, so these cases meet the handler's defense-in-depth class gate; on a
//! live connection the central capability gate in `routes.rs` refuses a class
//! first, with its own text — which is not the grant check's either.) The
//! grant's time bounds and class are pinned too: a lapsed grant, a not-yet-open
//! one, and a read grant standing in for a label-write grant are each refused
//! at the grant check.

mod common;
use common::dispatch;

use std::collections::BTreeMap;
use std::sync::Arc;

use bytes::Bytes;
use serde_bytes::ByteBuf;

use fauna_mls::wrapped_blob::{
    GrantBlob, GrantIndex, GrantWindow, ScopeTuple, derive_recipient_hpke_keypair,
};
use fauna_nest::{
    bridge_method_allowlist::{CallerClass, is_permitted},
    db::{CacheDb, bridge_service_users::BridgeRole},
    label_handlers::{self, GRANT_REFUSAL_DETAILS},
    routes::AppState,
    rpc_router::RpcRouter,
};
use fauna_protocol::{
    RpcError, decode_strict as decode, encode_canonical,
    labels::{
        LabelInput, LabelsAttachReply, LabelsAttachRequest, LabelsListReply, LabelsListRequest,
    },
};

async fn router_with_db_only() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    label_handlers::register_labels_handlers(&mut b);
    (b.build(), state)
}

/// Seed a real post by `author` and hand back the `content_id` the wire takes —
/// the post id's lowercase hex, the form `content_labels.content_id` stores.
/// The door resolves ownership from the store, so a test that attaches has to
/// put a post there first.
async fn seed_post(state: &Arc<AppState>, author: &[u8; 32], post_id: &[u8; 32]) -> String {
    state
        .db
        .insert_post_index_entry(post_id, author, 1_000_000, false, false, "fauna", &[])
        .await
        .unwrap();
    hex::encode(post_id)
}

fn enc<T: serde::Serialize>(v: &T) -> Bytes {
    Bytes::from(encode_canonical(v).unwrap().to_vec())
}

fn label(category: &str, per_mille: i64) -> LabelInput {
    LabelInput {
        category: category.into(),
        confidence_per_mille: per_mille,
        source: None,
        extra: BTreeMap::new(),
    }
}

fn attach(content_id: &str, labels: Vec<LabelInput>) -> Bytes {
    enc(&LabelsAttachRequest {
        content_id: content_id.into(),
        labels,
        extra: BTreeMap::new(),
    })
}

/// The refusal's wire `details` as text (`None` → empty). The class gate and
/// the grant check share one code, so WHICH gate refused is readable only here.
fn details_text(err: &RpcError) -> String {
    match err.details.as_deref() {
        Some(fauna_protocol::Value::String(s)) => s.clone(),
        other => format!("{other:?}"),
    }
}

/// Enrol and approve a bridge service user of `role` whose x25519 key derives
/// from `seed` — the three DB writes behind the admin's `POST pending`, the
/// bridge's self-attestation, and the admin's approve. Returns the x25519
/// pubkey, the key a grant is sealed TO and the key `holder_granted_scopes`
/// resolves the caller by. `approve_bridge_service_user` writes the actor's
/// audit `users` row itself, so the caller is never `create_user`'d here —
/// that would not change its class (the service-user row wins), but it is
/// not what production does.
async fn approve_holder(
    state: &Arc<AppState>,
    actor: &[u8; 32],
    role: BridgeRole,
    seed: &[u8; 32],
    bridge_id: &str,
) -> [u8; 32] {
    let (_secret, public) = derive_recipient_hpke_keypair(seed);
    state
        .db
        .create_pending_bridge_service_user(actor, role, bridge_id)
        .await
        .unwrap();
    state.db.upsert_bridge_x25519(actor, &public).await.unwrap();
    state
        .db
        .approve_bridge_service_user(actor, None)
        .await
        .unwrap();
    public
}

/// Mint `owner`'s `content.label-write` grant over `kind` (`None` = any kind,
/// the shape the client mint issues) to `holder_pubkey`, stored the way
/// `fauna.capabilities.mint` stores it: an open window, and keyless — a
/// label-write tuple carries no `WrappedScopeKey`.
async fn grant_label_write(
    state: &Arc<AppState>,
    owner: &[u8; 32],
    grant_id: &[u8; 16],
    holder_pubkey: &[u8; 32],
    kind: Option<&str>,
) {
    put_grant(
        state,
        owner,
        grant_id,
        holder_pubkey,
        scope_tuple(ScopeTuple::CLASS_CONTENT_LABEL_WRITE, kind),
        GrantWindow(0, u64::MAX),
        i64::MAX,
    )
    .await;
}

fn scope_tuple(class: &str, kind: Option<&str>) -> ScopeTuple {
    ScopeTuple {
        class: class.into(),
        kind: kind.map(Into::into),
        tier: None,
        set: None,
        factor: None,
    }
}

/// Store `owner`'s one-tuple, keyless grant of `scope` to `holder_pubkey` over
/// `window`. The storage row's `epoch_end` — the column the honest-box expiry
/// filter reads — is passed separately, so a case can build a grant whose
/// window has closed, or has not yet opened, the way a mint stores one.
async fn put_grant(
    state: &Arc<AppState>,
    owner: &[u8; 32],
    grant_id: &[u8; 16],
    holder_pubkey: &[u8; 32],
    scope: ScopeTuple,
    window: GrantWindow,
    epoch_end: i64,
) {
    let blob = GrantBlob {
        version: 1,
        kind: GrantBlob::KIND.to_string(),
        index: GrantIndex(owner.to_vec(), grant_id.to_vec()),
        holder: ByteBuf::from(holder_pubkey.to_vec()),
        window,
        scope: vec![scope],
        wrapped_keys: Vec::new(),
    }
    .to_canonical_bytes()
    .expect("encode grant blob");
    state
        .db
        .put_capability_grant(owner, grant_id, holder_pubkey, epoch_end, &blob)
        .await
        .unwrap();
}

#[tokio::test]
async fn attach_then_list_round_trips_ordered_by_confidence() {
    let (router, state) = router_with_db_only().await;
    let actor = [1u8; 32];
    let content_id = seed_post(&state, &actor, &[0xA1u8; 32]).await;

    let reply_bytes = dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.labels.attach",
        attach(&content_id, vec![label("spam", 900), label("nsfw", 1000)]),
    )
    .await
    .expect("attach ok");
    let reply: LabelsAttachReply = decode(&reply_bytes).unwrap();
    assert_eq!(reply.stored, 2);

    let list_bytes = dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.labels.list",
        enc(&LabelsListRequest {
            content_id: content_id.clone(),
            extra: BTreeMap::new(),
        }),
    )
    .await
    .expect("list ok");
    let list: LabelsListReply = decode(&list_bytes).unwrap();
    assert_eq!(list.content_id, content_id);
    assert_eq!(list.labels.len(), 2);
    // ORDER BY confidence DESC → nsfw (1000) first, then spam (900).
    assert_eq!(list.labels[0].category, "nsfw");
    assert_eq!(list.labels[0].confidence_per_mille, 1000);
    assert_eq!(list.labels[1].category, "spam");
    assert_eq!(list.labels[1].confidence_per_mille, 900);
}

#[tokio::test]
async fn list_empty_for_unlabeled_content() {
    let (router, state) = router_with_db_only().await;
    let list_bytes = dispatch(
        &router,
        state,
        [2u8; 32],
        "fauna.labels.list",
        enc(&LabelsListRequest {
            content_id: "post-none".into(),
            extra: BTreeMap::new(),
        }),
    )
    .await
    .expect("list ok");
    let list: LabelsListReply = decode(&list_bytes).unwrap();
    assert!(list.labels.is_empty());
}

#[tokio::test]
async fn attach_rejects_out_of_range_per_mille() {
    let (router, state) = router_with_db_only().await;
    let actor = [3u8; 32];
    // A post the caller owns, so the refusal can only be the range check.
    let content_id = seed_post(&state, &actor, &[0xA3u8; 32]).await;
    let err = dispatch(
        &router,
        state,
        actor,
        "fauna.labels.attach",
        attach(&content_id, vec![label("spam", 1500)]),
    )
    .await
    .expect_err("out-of-range per-mille rejected");
    assert_eq!(err.code, "fauna.labels.invalid_request");
}

#[tokio::test]
async fn attach_rejects_empty_labels() {
    let (router, state) = router_with_db_only().await;
    let actor = [4u8; 32];
    let content_id = seed_post(&state, &actor, &[0xA4u8; 32]).await;
    let err = dispatch(
        &router,
        state,
        actor,
        "fauna.labels.attach",
        attach(&content_id, vec![]),
    )
    .await
    .expect_err("empty labels rejected");
    assert_eq!(err.code, "fauna.labels.invalid_request");
}

/// **The finding this door was reopened for.** A member's `User` class says a
/// member is asking; it never said the member may speak about *this* post. With
/// a class-only gate, M's `spam: 1000` on V's post removed V's post from every
/// feed on the nest (the mandatory `LabelBelow{spam,500}` group) and, under an
/// enrolled authority, replaced V's public page — silently, unattributably, with
/// no remedy. Both arms die at the door: a third party's label is refused.
#[tokio::test]
async fn attach_refuses_a_label_on_another_actors_post() {
    let (router, state) = router_with_db_only().await;
    let victim = [0x11u8; 32];
    let attacker = [0x22u8; 32];
    let content_id = seed_post(&state, &victim, &[0xA5u8; 32]).await;

    let err = dispatch(
        &router,
        state.clone(),
        attacker,
        "fauna.labels.attach",
        attach(&content_id, vec![label("spam", 1000)]),
    )
    .await
    .expect_err("a third party may not label someone else's post");
    assert_eq!(err.code, "fauna.labels.permission_denied");
    // The code alone cannot say which gate refused (the class gate answers the
    // same one). A member clears the class gate and is stopped by the
    // ownership-or-grant check — pin THAT, so this case cannot keep passing if
    // the refusal ever moves to the class gate.
    assert_eq!(
        details_text(&err),
        GRANT_REFUSAL_DETAILS,
        "a member is refused by the ownership-or-grant check, not the class gate"
    );

    // Nothing was written: the plane the two verdicts read is untouched, so the
    // victim's post keeps binding nobody.
    assert!(
        state
            .db
            .get_content_labels("post", &content_id)
            .await
            .unwrap()
            .is_empty(),
        "a refused attach must write no row"
    );
}

/// The author-scope check hardens one more thing for free: the door used to
/// accept a `content_id` naming no post at all (the twin authorized nothing, so
/// it never had to resolve one). There is nothing to own, so there is nothing to
/// label.
#[tokio::test]
async fn attach_refuses_a_content_id_naming_no_post() {
    let (router, state) = router_with_db_only().await;
    let err = dispatch(
        &router,
        state,
        [5u8; 32],
        "fauna.labels.attach",
        attach(&hex::encode([0xEEu8; 32]), vec![label("spam", 900)]),
    )
    .await
    .expect_err("no post, no label");
    assert_eq!(err.code, "fauna.labels.not_found");
}

#[tokio::test]
async fn attach_refuses_a_content_id_that_is_not_a_post_id() {
    let (router, state) = router_with_db_only().await;
    let err = dispatch(
        &router,
        state,
        [6u8; 32],
        "fauna.labels.attach",
        attach("post-abc", vec![label("spam", 900)]),
    )
    .await
    .expect_err("a content_id that is not 32 bytes of hex names no post");
    assert_eq!(err.code, "fauna.labels.invalid_request");
}

/// The other half of the fix: scoping the door must not simply BLIND the
/// surfaces that read it. An author's own label still reaches the projection
/// both reader-binding verdicts fold — which is only true because the row is
/// ATTRIBUTED (the projection admits a row only when its `scanner_id` is a
/// 32-byte non-zero writer id, so a zero-stamped row would vanish here).
#[tokio::test]
async fn an_authors_own_label_is_attributed_and_still_reaches_the_verdicts() {
    let (router, state) = router_with_db_only().await;
    let author = [0x31u8; 32];
    let post_id = [0xA6u8; 32];
    let content_id = seed_post(&state, &author, &post_id).await;

    dispatch(
        &router,
        state.clone(),
        author,
        "fauna.labels.attach",
        attach(&content_id, vec![label("spam", 900)]),
    )
    .await
    .expect("an author may label their own post");

    let entries = state.db.post_label_entries(&post_id).await.unwrap();
    assert_eq!(
        entries.len(),
        1,
        "the author's own verdict still reaches the fold: {entries:?}"
    );
    assert_eq!(entries[0].category, "spam");
    assert_eq!(entries[0].confidence_per_mille, 900);
}

/// The upsert key carries the writer, so two positions with something to say
/// about one post hold two rows — where the shared zero `classifier_id` made
/// every API writer collide on one last-writer-wins row (whoever spoke last
/// owned the category). The projection still reduces to one entry per category,
/// taking the strongest.
#[tokio::test]
async fn two_writers_do_not_share_one_row() {
    let (router, state) = router_with_db_only().await;
    let author = [0x41u8; 32];
    let post_id = [0xA7u8; 32];
    let content_id = seed_post(&state, &author, &post_id).await;

    dispatch(
        &router,
        state.clone(),
        author,
        "fauna.labels.attach",
        attach(&content_id, vec![label("spam", 200)]),
    )
    .await
    .expect("the author's own label");

    // A second sanctioned position (here written at the DB seam, as the room
    // labeler pass writes its own) names itself and lands beside it.
    state
        .db
        .upsert_content_label(
            "post",
            &content_id,
            "spam",
            0.8,
            0,
            &[0x77u8; 32],
            1,
            0,
            None,
            None,
            0,
            &[0x77u8; 32],
            b"",
        )
        .await
        .unwrap();

    let rows = state
        .db
        .get_content_labels("post", &content_id)
        .await
        .unwrap();
    assert_eq!(rows.len(), 2, "one row per writer, not one shared row");

    let entries = state.db.post_label_entries(&post_id).await.unwrap();
    assert_eq!(entries.len(), 1, "still one entry per category");
    assert_eq!(entries[0].confidence_per_mille, 800, "the strongest wins");
}

#[tokio::test]
async fn label_kinds_class_arms_at_allowlist_layer() {
    // `fauna.labels.*` are User-class kinds; Admin inherits them via the
    // deliberate admin ⊇ user override (`bridge_method_allowlist::is_permitted`
    // head). `attach` additionally admits the grant-holder
    // classes — the set `fauna.capabilities.fetch` admits, since a holder is a
    // class that can fetch a grant — while `list` stays User-only and the two
    // bridge classes that hold no grants are denied both. (This test predated
    // the override and asserted Admin denied — stale; corrected. It then pinned
    // `BridgeMda` OUT of `attach`, which is what made the grant arm
    // unreachable — re-decided deliberately.)
    //
    // The class arm is the OUTER gate only: passing it says a member or a
    // holder is asking, never that the caller may label the post it names —
    // that is `attach_refuses_a_label_on_another_actors_post` and the
    // `// ── The grant arm` cases below.
    for kind in ["fauna.labels.attach", "fauna.labels.list"] {
        for class in [CallerClass::User, CallerClass::Admin] {
            assert!(is_permitted(class, kind), "{kind} for {class:?}");
        }
        for class in [CallerClass::BridgeMta, CallerClass::BridgeAtprotoPds] {
            assert!(!is_permitted(class, kind), "{kind} denied for {class:?}");
        }
    }
    for class in [CallerClass::BridgeMda, CallerClass::ContentProcessor] {
        assert!(
            is_permitted(class, "fauna.labels.attach"),
            "attach admits the grant-holder class {class:?}"
        );
        assert!(
            is_permitted(class, "fauna.capabilities.fetch"),
            "{class:?} is a holder class because it can fetch a grant"
        );
        assert!(
            !is_permitted(class, "fauna.labels.list"),
            "list stays User-only ({class:?})"
        );
    }
}

// ── The grant arm — a holder of the author's grant, through the real door ────
//
// `moderation.md` § Per-row badge data path ratifies exactly two callers for a
// feed post's labels: the author, and a holder of the author's
// `content.label-write` grant. The holder half is the third-party case — a
// content-processing position (an in-process content processor, the AUTH'd MDA
// drain) labelling on the author's own, revocable say-so. Each case below
// drives the real dispatch (class gate → payload validation → ownership-or-grant
// check → write) with a holder enrolled and granted the way production enrols
// and grants one; none touches the DB seam for the behaviour under test.

/// **The arm this file was reopened for.** A holder of the author's grant is
/// admitted — through both holder classes, since a grant's holder is whichever
/// enrolled service user the author sealed it to — and the row it writes names
/// the holder, so it reaches the reader-binding projection like the author's
/// own. Disabling the grant check (`if false && granted`) reddens this case;
/// until it existed nothing did.
#[tokio::test]
async fn a_granted_holder_is_admitted_through_the_real_door() {
    for (role, holder, seed, post_id, bridge_id) in [
        (
            BridgeRole::ContentProcessor,
            [0x52u8; 32],
            [0x5Au8; 32],
            [0xA8u8; 32],
            "cp-1",
        ),
        (
            BridgeRole::Mda,
            [0x53u8; 32],
            [0x5Bu8; 32],
            [0xA9u8; 32],
            "mda-1",
        ),
    ] {
        let (router, state) = router_with_db_only().await;
        let author = [0x51u8; 32];
        let content_id = seed_post(&state, &author, &post_id).await;
        let holder_pk = approve_holder(&state, &holder, role.clone(), &seed, bridge_id).await;
        grant_label_write(&state, &author, &[0x11u8; 16], &holder_pk, Some("post")).await;

        let reply_bytes = dispatch(
            &router,
            state.clone(),
            holder,
            "fauna.labels.attach",
            attach(&content_id, vec![label("spam", 900)]),
        )
        .await
        .unwrap_or_else(|e| {
            panic!("a {role:?} holder of the author's grant is admitted, got {e:?}")
        });
        let reply: LabelsAttachReply = decode(&reply_bytes).unwrap();
        assert_eq!(reply.stored, 1, "{role:?}");

        // Attributed to the holder — the row reaches the projection both
        // reader-binding verdicts fold, which admits only a writer-naming row.
        let entries = state.db.post_label_entries(&post_id).await.unwrap();
        assert_eq!(
            entries.len(),
            1,
            "the holder's verdict reaches the fold ({role:?}): {entries:?}"
        );
        assert_eq!(entries[0].category, "spam");
        assert_eq!(entries[0].confidence_per_mille, 900);
    }
}

/// An any-kind grant — the tuple the client mint issues beside
/// `content.read{kind}` — admits the holder to the post plane too.
#[tokio::test]
async fn an_any_kind_grant_admits_the_holder() {
    let (router, state) = router_with_db_only().await;
    let author = [0x61u8; 32];
    let content_id = seed_post(&state, &author, &[0xAAu8; 32]).await;
    let holder = [0x62u8; 32];
    let holder_pk = approve_holder(
        &state,
        &holder,
        BridgeRole::ContentProcessor,
        &[0x6Au8; 32],
        "cp-1",
    )
    .await;
    grant_label_write(&state, &author, &[0x12u8; 16], &holder_pk, None).await;

    dispatch(
        &router,
        state,
        holder,
        "fauna.labels.attach",
        attach(&content_id, vec![label("nsfw", 700)]),
    )
    .await
    .expect("an any-kind label-write grant covers the post plane");
}

/// Passing the class gate grants nothing by itself: a holder class with no
/// grant from THIS author — none at all, one from a different owner, or one
/// over a different kind — is refused by the grant check, with the grant
/// check's `details`, not the class gate's, and writes nothing.
#[tokio::test]
async fn a_holder_class_without_the_authors_grant_is_refused_at_the_grant_check() {
    let (router, state) = router_with_db_only().await;
    let author = [0x71u8; 32];
    let content_id = seed_post(&state, &author, &[0xABu8; 32]).await;
    let someone_else = [0x72u8; 32];

    let ungranted = [0x73u8; 32];
    approve_holder(
        &state,
        &ungranted,
        BridgeRole::ContentProcessor,
        &[0x7Au8; 32],
        "cp-1",
    )
    .await;

    let granted_by_someone_else = [0x74u8; 32];
    let pk = approve_holder(
        &state,
        &granted_by_someone_else,
        BridgeRole::ContentProcessor,
        &[0x7Bu8; 32],
        "cp-2",
    )
    .await;
    grant_label_write(&state, &someone_else, &[0x13u8; 16], &pk, Some("post")).await;

    let granted_over_mail = [0x75u8; 32];
    let pk = approve_holder(
        &state,
        &granted_over_mail,
        BridgeRole::Mda,
        &[0x7Cu8; 32],
        "mda-1",
    )
    .await;
    grant_label_write(&state, &author, &[0x14u8; 16], &pk, Some("mail")).await;

    for (holder, why) in [
        (ungranted, "holds no grant at all"),
        (
            granted_by_someone_else,
            "holds a grant from a different owner",
        ),
        (
            granted_over_mail,
            "holds the author's grant over another kind",
        ),
    ] {
        let err = dispatch(
            &router,
            state.clone(),
            holder,
            "fauna.labels.attach",
            attach(&content_id, vec![label("spam", 1000)]),
        )
        .await
        .expect_err(&format!("a holder that {why} must be refused"));
        assert_eq!(err.code, "fauna.labels.permission_denied", "{why}");
        assert_eq!(
            details_text(&err),
            GRANT_REFUSAL_DETAILS,
            "a holder that {why} clears the class gate and is stopped by the grant check"
        );
    }
    assert!(
        state
            .db
            .get_content_labels("post", &content_id)
            .await
            .unwrap()
            .is_empty(),
        "a refused attach must write no row"
    );
}

/// The other refusal, made distinguishable. A class that can hold no grant
/// (the MTA — `fauna.capabilities.fetch` refuses it) is stopped at the class
/// gate, whose `details` names the class; the grant check never runs. Same
/// code as the case above, different `details` — which is the whole point of
/// asserting on both. (This is the handler's class gate. On a live connection
/// the central gate in `routes.rs` refuses the MTA first, with a `details` that
/// is not the grant check's either.)
#[tokio::test]
async fn a_class_that_holds_no_grants_is_refused_at_the_class_gate() {
    let (router, state) = router_with_db_only().await;
    let author = [0x81u8; 32];
    let content_id = seed_post(&state, &author, &[0xACu8; 32]).await;
    let mta = [0x82u8; 32];
    approve_holder(&state, &mta, BridgeRole::Mta, &[0x8Au8; 32], "mta-1").await;

    let err = dispatch(
        &router,
        state.clone(),
        mta,
        "fauna.labels.attach",
        attach(&content_id, vec![label("spam", 1000)]),
    )
    .await
    .expect_err("the MTA holds no grants and may not call the door");
    assert_eq!(err.code, "fauna.labels.permission_denied");
    let details = details_text(&err);
    assert!(
        details.contains("not permitted for caller class BridgeMta"),
        "the class gate names the class it refused: {details}"
    );
    assert_ne!(
        details, GRANT_REFUSAL_DETAILS,
        "not the grant check's refusal"
    );
}

/// Two API writers through the real door hold two rows. The DB-seam twin
/// (`two_writers_do_not_share_one_row`) fakes its second writer, so it cannot
/// catch a regression to every API writer sharing one zero `classifier_id` —
/// now that the door admits a second principal per post, this case can: zero
/// both stamps in the handler and the two writes collapse to one row.
#[tokio::test]
async fn two_api_writers_hold_two_rows_through_the_real_door() {
    let (router, state) = router_with_db_only().await;
    let author = [0x91u8; 32];
    let post_id = [0xADu8; 32];
    let content_id = seed_post(&state, &author, &post_id).await;
    let holder = [0x92u8; 32];
    let holder_pk = approve_holder(
        &state,
        &holder,
        BridgeRole::ContentProcessor,
        &[0x9Au8; 32],
        "cp-1",
    )
    .await;
    grant_label_write(&state, &author, &[0x15u8; 16], &holder_pk, Some("post")).await;

    for (writer, per_mille) in [(author, 200), (holder, 800)] {
        dispatch(
            &router,
            state.clone(),
            writer,
            "fauna.labels.attach",
            attach(&content_id, vec![label("spam", per_mille)]),
        )
        .await
        .expect("both sanctioned writers are admitted");
    }

    let rows = state
        .db
        .get_content_labels("post", &content_id)
        .await
        .unwrap();
    assert_eq!(rows.len(), 2, "one row per writer, not one shared row");

    let entries = state.db.post_label_entries(&post_id).await.unwrap();
    assert_eq!(entries.len(), 1, "still one entry per category");
    assert_eq!(entries[0].confidence_per_mille, 800, "the strongest wins");
}

// ── The grant arm's time bounds ───────────────────────────────────────────────
//
// A grant is time-bounded (`encryption-at-rest.md` § Capability tiering), so the
// author's grant admits a holder only while its window is open. Every bound
// below sits an hour or more from the wall clock `holder_granted_scopes` reads,
// so no scheduling delay can carry a case across one.

const DAY: u64 = 24 * 3600;

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after the epoch")
        .as_secs()
}

/// `holder` is refused on `content_id` by the grant check — its `details`, not
/// the class gate's — and nothing is written.
async fn assert_refused_at_the_grant_check(
    router: &RpcRouter,
    state: &Arc<AppState>,
    holder: [u8; 32],
    content_id: &str,
    why: &str,
) {
    let err = dispatch(
        router,
        state.clone(),
        holder,
        "fauna.labels.attach",
        attach(content_id, vec![label("spam", 1000)]),
    )
    .await
    .expect_err(&format!("a holder that {why} must be refused"));
    assert_eq!(err.code, "fauna.labels.permission_denied", "{why}");
    assert_eq!(
        details_text(&err),
        GRANT_REFUSAL_DETAILS,
        "a holder that {why} clears the class gate and is stopped by the grant check"
    );
    assert!(
        state
            .db
            .get_content_labels("post", content_id)
            .await
            .unwrap()
            .is_empty(),
        "a refused attach must write no row ({why})"
    );
}

/// A lapsed grant grants nothing. The author's `content.label-write{post}`
/// grant closed an hour ago — its window and its storage row's `epoch_end`
/// both, the shape an unrenewed mint leaves — so the holder is refused rather
/// than admitted on a capability it used to have.
#[tokio::test]
async fn a_holder_whose_grant_has_expired_is_refused_at_the_grant_check() {
    let (router, state) = router_with_db_only().await;
    let author = [0xB1u8; 32];
    let content_id = seed_post(&state, &author, &[0xAEu8; 32]).await;
    let holder = [0xB2u8; 32];
    let holder_pk = approve_holder(
        &state,
        &holder,
        BridgeRole::ContentProcessor,
        &[0xBAu8; 32],
        "cp-1",
    )
    .await;
    let now = now_secs();
    put_grant(
        &state,
        &author,
        &[0x16u8; 16],
        &holder_pk,
        scope_tuple(ScopeTuple::CLASS_CONTENT_LABEL_WRITE, Some("post")),
        GrantWindow(now - 90 * DAY, now - 3600),
        (now - 3600) as i64,
    )
    .await;

    assert_refused_at_the_grant_check(
        &router,
        &state,
        holder,
        &content_id,
        "holds only an expired grant",
    )
    .await;
}

/// A grant whose window has not opened is not yet a capability. The storage
/// filter reads only `epoch_end`, which lies in the future here, so this is
/// the case only the window check refuses.
#[tokio::test]
async fn a_holder_whose_grant_window_has_not_opened_is_refused_at_the_grant_check() {
    let (router, state) = router_with_db_only().await;
    let author = [0xC1u8; 32];
    let content_id = seed_post(&state, &author, &[0xAFu8; 32]).await;
    let holder = [0xC2u8; 32];
    let holder_pk = approve_holder(
        &state,
        &holder,
        BridgeRole::ContentProcessor,
        &[0xCAu8; 32],
        "cp-1",
    )
    .await;
    let now = now_secs();
    put_grant(
        &state,
        &author,
        &[0x17u8; 16],
        &holder_pk,
        scope_tuple(ScopeTuple::CLASS_CONTENT_LABEL_WRITE, Some("post")),
        GrantWindow(now + 30 * DAY, now + 90 * DAY),
        (now + 90 * DAY) as i64,
    )
    .await;

    assert_refused_at_the_grant_check(
        &router,
        &state,
        holder,
        &content_id,
        "holds a grant whose window has not opened",
    )
    .await;
}

// ── The grant arm's class ─────────────────────────────────────────────────────

/// Reading is not writing. A content-processing holder is expected to hold the
/// author's `content.read{post}` — it needs one to classify at all — and that
/// grant must not double as the `content.label-write` grant the door asks for.
#[tokio::test]
async fn a_holder_with_only_the_authors_read_grant_is_refused_at_the_grant_check() {
    let (router, state) = router_with_db_only().await;
    let author = [0xD1u8; 32];
    let content_id = seed_post(&state, &author, &[0xB0u8; 32]).await;
    let holder = [0xD2u8; 32];
    let holder_pk = approve_holder(
        &state,
        &holder,
        BridgeRole::ContentProcessor,
        &[0xDAu8; 32],
        "cp-1",
    )
    .await;
    put_grant(
        &state,
        &author,
        &[0x18u8; 16],
        &holder_pk,
        scope_tuple(ScopeTuple::CLASS_CONTENT_READ, Some("post")),
        GrantWindow(0, u64::MAX),
        i64::MAX,
    )
    .await;

    assert_refused_at_the_grant_check(
        &router,
        &state,
        holder,
        &content_id,
        "holds only the author's read grant",
    )
    .await;
}
