//! Integration round-trip for `fauna.personalization.model.{fetch,put,delete}`
//! — the sealed personalization-model plane
//! (`docs/goal/behavior/topic-factors.md` § At rest + § Wire & registry;
//! payload types `libs/fauna-protocol/src/personalization.rs`; handlers
//! `bins/fauna-nest/src/personalization_handlers.rs`; at-rest home
//! `bins/fauna-nest/src/db/personalization.rs`).
//!
//! Exercised here: request decode, the factor-namespace gate (v1 accepts the
//! `topic:<hex>` trained-model namespace and the `cues:` engagement-cue rollup
//! namespace — `engagement-cues.md` § Seal + home), the put caps (512 KiB sealed blob, empty blob,
//! `TRAINED_FACTORS_MAX = 32` per-actor create cap with overwrite-at-cap
//! allowed), delete idempotency, cross-actor isolation (owner-scoping by
//! construction on the connection actor), reply encoding, and the allowlist.
//!
//! Tier: tier_3 (real `fauna-nest` `AppState` + real `CacheDb` — no mocks).
//! Matches the feed / posts / conversations conformance harnesses.

mod common;
use common::dispatch;
use common::encode;

use std::sync::Arc;

use serde_bytes::ByteBuf;

use fauna_nest::{
    bridge_method_allowlist::{CallerClass, is_permitted},
    db::CacheDb,
    personalization_handlers::{
        self, PERSONALIZATION_MODEL_MAX_PUT_BYTES, TRAINED_FACTORS_MAX,
        register_personalization_handlers,
    },
    routes::AppState,
    rpc_router::RpcRouter,
};
use fauna_protocol::{
    RpcError, decode_strict as decode,
    personalization::{
        PersonalizationModelDeleteReply, PersonalizationModelDeleteRequest,
        PersonalizationModelFetchReply, PersonalizationModelFetchRequest,
        PersonalizationModelPutReply, PersonalizationModelPutRequest,
    },
};

const KINDS: [&str; 3] = [
    "fauna.personalization.model.fetch",
    "fauna.personalization.model.put",
    "fauna.personalization.model.delete",
];

async fn router_with_db_only() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    register_personalization_handlers(&mut b);
    (b.build(), state)
}

fn topic_key(id_byte: u8) -> String {
    fauna_core::scoring::topic_factor(&[id_byte; 16])
}

/// The reply's blob as a plain byte slice (unwraps the `ByteBuf` newtype).
fn blob_of(reply: &PersonalizationModelFetchReply) -> Option<&[u8]> {
    reply.sealed_blob.as_ref().map(|b| b.as_slice())
}

async fn fetch(
    router: &RpcRouter,
    state: Arc<AppState>,
    actor: [u8; 32],
    factor: &str,
) -> PersonalizationModelFetchReply {
    decode(
        &dispatch(
            router,
            state,
            actor,
            "fauna.personalization.model.fetch",
            encode(&PersonalizationModelFetchRequest {
                factor: factor.into(),
                extra: Default::default(),
            }),
        )
        .await
        .expect("fetch ok"),
    )
    .unwrap()
}

async fn put(
    router: &RpcRouter,
    state: Arc<AppState>,
    actor: [u8; 32],
    factor: &str,
    blob: Vec<u8>,
    sample_count: u32,
) -> Result<PersonalizationModelPutReply, RpcError> {
    dispatch(
        router,
        state,
        actor,
        "fauna.personalization.model.put",
        encode(&PersonalizationModelPutRequest {
            factor: factor.into(),
            sealed_blob: ByteBuf::from(blob),
            sample_count,
            extra: Default::default(),
        }),
    )
    .await
    .map(|bytes| decode(&bytes).unwrap())
}

async fn delete(
    router: &RpcRouter,
    state: Arc<AppState>,
    actor: [u8; 32],
    factor: &str,
) -> Result<PersonalizationModelDeleteReply, RpcError> {
    dispatch(
        router,
        state,
        actor,
        "fauna.personalization.model.delete",
        encode(&PersonalizationModelDeleteRequest {
            factor: factor.into(),
            extra: Default::default(),
        }),
    )
    .await
    .map(|bytes| decode(&bytes).unwrap())
}

// ── fetch(empty) → put → fetch → delete → fetch → delete (idempotent) ──

#[tokio::test]
async fn model_round_trip_and_delete_idempotency() {
    let (router, state) = router_with_db_only().await;
    let actor = [11u8; 32];
    let factor = topic_key(0xAA);

    // Never trained: the documented empty shape.
    let empty = fetch(&router, state.clone(), actor, &factor).await;
    assert_eq!(empty.sealed_blob, None);
    assert_eq!(empty.sample_count, 0);
    assert_eq!(empty.updated_at, 0);

    // Put a sealed blob (opaque bytes — the nest never looks inside).
    let blob = vec![0xC0u8, 0xFF, 0xEE, 0x01, 0x02];
    let reply = put(&router, state.clone(), actor, &factor, blob.clone(), 7)
        .await
        .expect("put ok");
    assert_eq!(reply.status, "ok");

    // Fetch returns the bytes verbatim + the advisory count + a real stamp.
    let got = fetch(&router, state.clone(), actor, &factor).await;
    assert_eq!(blob_of(&got), Some(blob.as_slice()));
    assert_eq!(got.sample_count, 7);
    assert!(got.updated_at > 0, "updated_at is stamped nest-side");

    // Overwrite (last-put-wins) — same row, new bytes + count.
    let blob2 = vec![0xB0u8; 42];
    put(&router, state.clone(), actor, &factor, blob2.clone(), 9)
        .await
        .expect("overwrite ok");
    let got = fetch(&router, state.clone(), actor, &factor).await;
    assert_eq!(blob_of(&got), Some(blob2.as_slice()));
    assert_eq!(got.sample_count, 9);

    // Delete: row existed → deleted: true.
    let del = delete(&router, state.clone(), actor, &factor)
        .await
        .expect("delete ok");
    assert_eq!(del.status, "ok");
    assert!(del.deleted);

    // Back to the empty shape.
    let empty = fetch(&router, state.clone(), actor, &factor).await;
    assert_eq!(empty.sealed_blob, None);
    assert_eq!(empty.sample_count, 0);
    assert_eq!(empty.updated_at, 0);

    // Idempotent: deleting the absent row succeeds with deleted: false.
    let del = delete(&router, state.clone(), actor, &factor)
        .await
        .expect("second delete ok");
    assert_eq!(del.status, "ok");
    assert!(!del.deleted);
}

// ── put caps: oversize / empty blob, per-actor factor cap ──────────────

#[tokio::test]
async fn put_rejects_oversize_and_empty_blobs() {
    let (router, state) = router_with_db_only().await;
    let actor = [12u8; 32];
    let factor = topic_key(0xBB);

    // One byte over the 512 KiB cap → invalid_params, nothing stored.
    let oversize = vec![0u8; PERSONALIZATION_MODEL_MAX_PUT_BYTES + 1];
    let err = put(&router, state.clone(), actor, &factor, oversize, 1)
        .await
        .expect_err("oversize blob rejected");
    assert_eq!(err.code, "fauna.personalization.invalid_params");

    // Exactly at the cap is allowed.
    let at_cap = vec![0u8; PERSONALIZATION_MODEL_MAX_PUT_BYTES];
    put(&router, state.clone(), actor, &factor, at_cap, 1)
        .await
        .expect("blob exactly at the cap accepted");
    delete(&router, state.clone(), actor, &factor)
        .await
        .expect("cleanup delete ok");

    // Empty blob → invalid_params (delete is the way to clear a factor).
    let err = put(&router, state.clone(), actor, &factor, vec![], 0)
        .await
        .expect_err("empty blob rejected");
    assert_eq!(err.code, "fauna.personalization.invalid_params");
    let got = fetch(&router, state.clone(), actor, &factor).await;
    assert_eq!(got.sealed_blob, None, "rejected puts store nothing");
}

#[tokio::test]
async fn put_enforces_per_actor_factor_cap_but_allows_overwrite_at_cap() {
    let (router, state) = router_with_db_only().await;
    let actor = [13u8; 32];

    // Fill to the cap (32 distinct factors).
    let max = u8::try_from(TRAINED_FACTORS_MAX).unwrap();
    for i in 0..max {
        put(&router, state.clone(), actor, &topic_key(i), vec![1, i], 1)
            .await
            .unwrap_or_else(|e| panic!("factor #{i} within the cap accepted: {e:?}"));
    }

    // The 33rd NEW factor is rejected.
    let err = put(&router, state.clone(), actor, &topic_key(max), vec![2], 1)
        .await
        .expect_err("creating past the factor cap rejected");
    assert_eq!(err.code, "fauna.personalization.invalid_params");
    let got = fetch(&router, state.clone(), actor, &topic_key(max)).await;
    assert_eq!(got.sealed_blob, None, "the rejected row was not created");

    // Overwriting an EXISTING row at the cap is always allowed.
    put(
        &router,
        state.clone(),
        actor,
        &topic_key(0),
        vec![9, 9, 9],
        5,
    )
    .await
    .expect("overwrite at the cap allowed");
    let got = fetch(&router, state.clone(), actor, &topic_key(0)).await;
    assert_eq!(blob_of(&got), Some(&[9u8, 9, 9][..]));
    assert_eq!(got.sample_count, 5);

    // Deleting one frees a slot for a new factor.
    assert!(
        delete(&router, state.clone(), actor, &topic_key(1))
            .await
            .expect("delete ok")
            .deleted
    );
    put(&router, state.clone(), actor, &topic_key(max), vec![3], 1)
        .await
        .expect("a freed slot admits a new factor");

    // The cap is per-actor: a different actor starts from zero.
    let other = [14u8; 32];
    put(&router, state.clone(), other, &topic_key(0xEE), vec![4], 1)
        .await
        .expect("another actor is unaffected by this actor's cap");
}

// ── the cues:v1 rollup namespace rides the same sealed model wire ──────
//
// The BackupKey-sealed engagement-cue rollup (`engagement-cues.md` § Seal + home)
// is stored verbatim-opaque in `personalization_models` under `cues:v1` and
// synced via this exact fetch/put/delete wire — the additive prefix-set
// extension (`topic:` → `topic: | cues:`). It is NOT a composition factor, but
// the envelope validation accepts it like any topic key.
#[tokio::test]
async fn cues_rollup_factor_round_trips_on_the_sealed_model_wire() {
    let (router, state) = router_with_db_only().await;
    let actor = [16u8; 32];
    let factor = "cues:v1";

    // Absent → the documented empty shape.
    let empty = fetch(&router, state.clone(), actor, factor).await;
    assert_eq!(empty.sealed_blob, None);

    // Put the opaque sealed rollup, fetch it back verbatim, delete it.
    let rollup = vec![0xCEu8, 0x51, 0xF0, 0x0D];
    put(&router, state.clone(), actor, factor, rollup.clone(), 4)
        .await
        .expect("cues:v1 put accepted on the sealed model wire");
    let got = fetch(&router, state.clone(), actor, factor).await;
    assert_eq!(blob_of(&got), Some(rollup.as_slice()));
    assert_eq!(got.sample_count, 4);
    assert!(got.updated_at > 0);
    assert!(
        delete(&router, state.clone(), actor, factor)
            .await
            .expect("cues:v1 delete accepted")
            .deleted
    );
}

// ── v1 namespace gate: non-topic/non-cues factors rejected on all three kinds ─

#[tokio::test]
async fn non_topic_factors_are_rejected_on_all_kinds() {
    let (router, state) = router_with_db_only().await;
    let actor = [15u8; 32];

    // Built-in transparent factors and the labeler namespace are NOT valid
    // sealed personalization keys (v1 accepts only the `topic:` and `cues:`
    // namespaces — see `cues_rollup_factor_round_trips_on_the_sealed_model_wire`).
    for bad in [
        "engagement".to_string(),
        "muted_keywords".to_string(),
        format!("labeler:{}", "ab".repeat(32)),
        String::new(),
    ] {
        let err = dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.personalization.model.fetch",
            encode(&PersonalizationModelFetchRequest {
                factor: bad.clone(),
                extra: Default::default(),
            }),
        )
        .await
        .expect_err("fetch of a non-topic factor rejected");
        assert_eq!(err.code, "fauna.personalization.invalid_params");

        let err = put(&router, state.clone(), actor, &bad, vec![1], 1)
            .await
            .expect_err("put of a non-topic factor rejected");
        assert_eq!(err.code, "fauna.personalization.invalid_params");

        let err = delete(&router, state.clone(), actor, &bad)
            .await
            .expect_err("delete of a non-topic factor rejected");
        assert_eq!(err.code, "fauna.personalization.invalid_params");
    }
}

// ── cross-actor isolation: a caller only ever touches its own rows ─────

#[tokio::test]
async fn cross_actor_isolation() {
    let (router, state) = router_with_db_only().await;
    let alice = [21u8; 32];
    let bob = [22u8; 32];
    let factor = topic_key(0xCC);

    // Alice trains a model.
    put(&router, state.clone(), alice, &factor, vec![0xA1, 0xA2], 3)
        .await
        .expect("alice put ok");

    // Bob never sees Alice's row under the same factor key…
    let got = fetch(&router, state.clone(), bob, &factor).await;
    assert_eq!(got.sealed_blob, None, "bob cannot read alice's model");
    assert_eq!(got.sample_count, 0);

    // …and Bob's delete of that key touches nothing of Alice's.
    let del = delete(&router, state.clone(), bob, &factor)
        .await
        .expect("bob delete ok");
    assert!(!del.deleted, "bob has no row to delete");
    let got = fetch(&router, state.clone(), alice, &factor).await;
    assert_eq!(
        blob_of(&got),
        Some(&[0xA1u8, 0xA2][..]),
        "alice's row survives bob's delete"
    );

    // Bob's own row under the same key coexists, keyed on his actor id.
    put(&router, state.clone(), bob, &factor, vec![0xB1], 1)
        .await
        .expect("bob put ok");
    let alice_got = fetch(&router, state.clone(), alice, &factor).await;
    let bob_got = fetch(&router, state.clone(), bob, &factor).await;
    assert_eq!(blob_of(&alice_got), Some(&[0xA1u8, 0xA2][..]));
    assert_eq!(blob_of(&bob_got), Some(&[0xB1u8][..]));
}

// ── allowlist ──────────────────────────────────────────────────

#[tokio::test]
async fn personalization_kinds_are_user_facing_at_allowlist_layer() {
    for kind in KINDS {
        // User-facing per-actor preference surface. Admin ⊇ User (an admin
        // is a user who additionally holds the admin role — `is_permitted`'s
        // short-circuit), so both are permitted.
        for class in [CallerClass::User, CallerClass::Admin] {
            assert!(
                is_permitted(class, kind),
                "{kind} should be permitted for {class:?}"
            );
        }
        // Bridges are service identities — the sealed blob is none of their
        // business (they couldn't decrypt it either way).
        for class in [
            CallerClass::BridgeMta,
            CallerClass::BridgeMda,
            CallerClass::ContentProcessor,
        ] {
            assert!(
                !is_permitted(class, kind),
                "{kind} should be denied for {class:?}"
            );
        }
    }
}

// ── replay metadata ────────────────────────────────────────────

#[tokio::test]
async fn replay_metadata_matches_spec() {
    let mut b = RpcRouter::builder();
    personalization_handlers::register_personalization_handlers(&mut b);
    let router = b.build();
    for kind in KINDS {
        let meta = router.kind_meta(kind).expect("kind registered");
        assert!(
            !meta.forbid_replay,
            "{kind} is replay-safe (pure read / idempotent owner-keyed write)"
        );
        assert_eq!(
            meta.default_deadline,
            std::time::Duration::from_secs(5),
            "{kind} deadline is 5s"
        );
    }
}
