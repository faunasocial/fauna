//! Slice-1 registry-wire proof for the community-labeler registry
//! (`fauna.labelers.*`, labeler-registry design §§ 1–5).
//!
//! Drives the **real** publish/subscribe core logic (`publish_labeler_core`,
//! `subscribe_labeler_core` from `fauna_nest::labeler_handlers`) over an
//! in-memory `CacheDb`, exercising: signature/hash/compile validation →
//! monotonic store → list (metadata-only) → inspect (full bytes) → subscribe
//! (asserting BOTH the `labeler_subscriptions` row AND the `model_versions`
//! `labeler:<hex>` registration — the load-bearing link that makes the re-score
//! obligation owe a re-score) → unsubscribe. No RPC/WS harness, no WASM
//! execution, no Go/FFI (those are Slices 2–3).

use ed25519_dalek::Signer;

use fauna_core::data::Timestamp;
use fauna_core::encoding::{canonical_encode, content_hash};
use fauna_core::identity::ActorKeypair;
use fauna_core::scoring::{
    AlgorithmLabeler, LabelerInput, LabelerOutput, ScorerLimits, labeler_factor,
};
use fauna_nest::db::CacheDb;
use fauna_nest::db::labelers::MAX_LABELERS_PER_CALLER;
use fauna_nest::labeler_handlers::{
    list_labelers_core, publish_labeler_core, subscribe_labeler_core,
};

/// A fixed authenticated-caller identity for the tests that don't exercise the
/// per-caller quota (most of them) — distinct from any labeler's signing keypair.
/// The caller only matters to the F1 caller-cap test below, which rotates keypairs
/// under its own fixed caller.
const TEST_CALLER: [u8; 32] = [0xC0u8; 32];

/// A minimal valid WASM (WAT) module carrying the labeler ABI exports. It only
/// needs to *compile* under `LabelerRuntime::load_module` (the publish
/// validation gate) — Slice 1 never runs it.
const MINIMAL_LABELER_WAT: &str = r#"
(module
  (memory (export "memory") 1)
  (func (export "alloc") (param i32) (result i32) i32.const 0)
  (func (export "label") (param i32 i32) (result i32) i32.const 0))
"#;

/// Build a signed `AlgorithmLabeler` (+ its canonical metadata bytes) over
/// `wasm_bytes`, signed by `kp` (whose `actor_id` becomes the labeler's
/// `algorithm_id` == signer). Mirrors the `GrantEvent::sign` zero-sig-canonical
/// convention that `validate_labeler_publish` verifies against.
fn signed_labeler(kp: &ActorKeypair, version: u64, wasm_bytes: &[u8]) -> Vec<u8> {
    signed_labeler_at(kp, version, wasm_bytes, LabelerOutput::default())
}

/// [`signed_labeler`] declaring `output_schema` — the module's output label-ABI
/// stamp (`content-moderation-and-ranking.md` § Tier-3 → *The output half of
/// the `label()` ABI*).
fn signed_labeler_at(
    kp: &ActorKeypair,
    version: u64,
    wasm_bytes: &[u8],
    output_schema: LabelerOutput,
) -> Vec<u8> {
    let mut labeler = AlgorithmLabeler {
        algorithm_id: kp.actor_id(),
        version,
        wasm_hash: content_hash(wasm_bytes),
        wasm_size: wasm_bytes.len() as u64,
        input_schema: LabelerInput {
            needs_text: true,
            needs_hashtags: false,
            needs_media_metadata: false,
            needs_author: false,
            needs_attachment_bytes: false,
        },
        output_schema,
        resource_limits: ScorerLimits {
            max_memory_bytes: 16 * 1024 * 1024,
            max_cpu_microseconds: 100_000,
        },
        updated_at: Timestamp(1_700_000_000_000_000),
        signature: vec![0u8; 64],
    };
    // Sign over the canonical metadata with `signature` zeroed.
    let bytes = canonical_encode(&labeler).expect("canonical encode");
    let sig = kp.signing_key().sign(&bytes);
    labeler.signature = sig.to_bytes().to_vec();
    canonical_encode(&labeler).expect("canonical encode signed")
}

#[tokio::test]
async fn publish_list_inspect_subscribe_unsubscribe_registers_model_version() {
    let db = CacheDb::open_in_memory().unwrap();
    let kp = ActorKeypair::generate();
    let labeler_id = kp.actor_id().0;
    let factor = labeler_factor(&kp.actor_id());
    let wasm = MINIMAL_LABELER_WAT.as_bytes().to_vec();
    let metadata = signed_labeler(&kp, 1, &wasm);

    // ── publish (signature + hash + compile validation + store) ──
    let (stored_id, stored_ver) =
        publish_labeler_core(&db, &TEST_CALLER, &metadata, &wasm, "post", "wasm")
            .await
            .expect("publish a valid signed labeler");
    assert_eq!(stored_id, labeler_id);
    assert_eq!(stored_ver, 1);

    // ── list returns it (metadata-only, no bytes) ──
    let list = db.list_labelers().await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].labeler_id, labeler_id.to_vec());
    assert_eq!(list[0].version, 1);
    assert_eq!(list[0].factor, factor);
    assert_eq!(list[0].publisher_actor, labeler_id.to_vec());
    assert_eq!(list[0].content_kind, "post");

    // ── inspect returns the full record + WASM bytes ──
    let rec = db.get_labeler(&labeler_id).await.unwrap().unwrap();
    assert_eq!(rec.metadata_blob, metadata);
    assert_eq!(rec.wasm_bytes, wasm);

    // ── subscribe → subscription row AND model_versions registration ──
    let owner = [0x42u8; 32];
    let grant = [0x77u8; 16];
    let registered_factor = subscribe_labeler_core(&db, &owner, &labeler_id, Some(&grant))
        .await
        .expect("subscribe");
    assert_eq!(registered_factor, factor);

    // (a) the subscription row exists, linking the grant + registered version.
    let sub = db.get_subscription(&owner, &labeler_id).await.unwrap();
    let sub = sub.expect("labeler_subscriptions row must exist after subscribe");
    assert_eq!(sub.owner_actor, owner.to_vec());
    assert_eq!(sub.grant_id, Some(grant.to_vec()));
    assert_eq!(sub.subscribed_ver, 1);

    // (b) THE LOAD-BEARING LINK: model_versions has the labeler:<hex> row at the
    // labeler's version — this is what makes content_scores_behind owe a
    // re-score for the owner's content (design § 5 step 3).
    let mv = db.get_model_version(&factor).await.unwrap();
    assert_eq!(
        mv,
        Some(1),
        "model_versions must carry {factor} at v1 after subscribe"
    );

    // ── unsubscribe → the subscription row is gone (idempotent) ──
    assert!(db.delete_subscription(&owner, &labeler_id).await.unwrap());
    assert!(
        db.get_subscription(&owner, &labeler_id)
            .await
            .unwrap()
            .is_none(),
        "subscription row must be gone after unsubscribe"
    );
    // The registered model version is NOT rolled back on unsubscribe (the drain
    // going dark is the capability-revoke path, design § 5 "unsubscribe"); the
    // registry watermark is monotonic.
    assert_eq!(db.get_model_version(&factor).await.unwrap(), Some(1));
}

/// The personalization home's subscribed-labelers facet reads `list`'s per-row
/// `subscribed` flag (`fauna-client-labelers` catalog manager) rather than a
/// second "list my subscriptions" round-trip. Prove `list_labelers_core` stamps
/// it correctly: true for the caller who subscribed, false for one who didn't,
/// and it flips back to false after unsubscribe.
#[tokio::test]
async fn publish_stores_a_newer_label_abi_unexamined_and_inspects_it_back() {
    // Ruled: the nest's publish gate does not read `label_abi` — the revision
    // is a contract between the module and whatever runs it, and a nest
    // refusing a revision it does not know would make an older nest refuse a
    // newer client's artifact (the text-model `version` precedent). The gate
    // still verifies the signature and compiles the module.
    let db = CacheDb::open_in_memory().unwrap();
    let kp = ActorKeypair::generate();
    let labeler_id = kp.actor_id().0;
    let wasm = MINIMAL_LABELER_WAT.as_bytes().to_vec();
    let newer = fauna_core::scoring::LABEL_ABI_CURRENT + 1;
    let metadata = signed_labeler_at(&kp, 1, &wasm, LabelerOutput { label_abi: newer });

    publish_labeler_core(&db, &TEST_CALLER, &metadata, &wasm, "post", "wasm")
        .await
        .expect("a newer label_abi publishes: the gate stores it unexamined");

    let rec = db.get_labeler(&labeler_id).await.unwrap().unwrap();
    assert_eq!(rec.metadata_blob, metadata, "stored byte for byte");
    let seen: AlgorithmLabeler =
        fauna_core::encoding::canonical_decode(&rec.metadata_blob).unwrap();
    assert_eq!(
        seen.output_schema.label_abi, newer,
        "the stamp inspects back intact"
    );
}

#[tokio::test]
async fn list_stamps_the_callers_own_subscribed_flag() {
    let db = CacheDb::open_in_memory().unwrap();
    let kp = ActorKeypair::generate();
    let labeler_id = kp.actor_id().0;
    let wasm = MINIMAL_LABELER_WAT.as_bytes().to_vec();
    let metadata = signed_labeler(&kp, 1, &wasm);
    publish_labeler_core(&db, &TEST_CALLER, &metadata, &wasm, "post", "wasm")
        .await
        .expect("publish");

    let subscriber = [0x42u8; 32];
    let bystander = [0x99u8; 32];

    // Before anyone subscribes, both callers see it as unsubscribed.
    let before = list_labelers_core(&db, &subscriber).await.unwrap();
    assert_eq!(before.len(), 1);
    assert!(!before[0].subscribed);

    subscribe_labeler_core(&db, &subscriber, &labeler_id, None)
        .await
        .expect("subscribe");

    // The subscriber's own `list` now shows it subscribed …
    let subscriber_view = list_labelers_core(&db, &subscriber).await.unwrap();
    assert!(subscriber_view[0].subscribed);
    // … but a bystander's `list` (same labeler, different caller) still doesn't —
    // subscriptions are per-owner, never leaked cross-user.
    let bystander_view = list_labelers_core(&db, &bystander).await.unwrap();
    assert!(!bystander_view[0].subscribed);

    db.delete_subscription(&subscriber, &labeler_id)
        .await
        .unwrap();
    let after_unsub = list_labelers_core(&db, &subscriber).await.unwrap();
    assert!(
        !after_unsub[0].subscribed,
        "unsubscribe flips it back to false"
    );
}

#[tokio::test]
async fn publish_rejects_tampered_signature() {
    let db = CacheDb::open_in_memory().unwrap();
    let kp = ActorKeypair::generate();
    let wasm = MINIMAL_LABELER_WAT.as_bytes().to_vec();
    let mut metadata = signed_labeler(&kp, 1, &wasm);
    // Flip a byte in the (canonical) metadata so the signature no longer verifies.
    let last = metadata.len() - 1;
    metadata[last] ^= 0xFF;
    let err = publish_labeler_core(&db, &TEST_CALLER, &metadata, &wasm, "post", "wasm")
        .await
        .expect_err("a tampered labeler must be rejected");
    // Either the signature check or the strict decode rejects it; both are
    // client-facing errors, never a stored labeler.
    assert!(db.list_labelers().await.unwrap().is_empty());
    let _ = err; // shape asserted by the empty-catalog check above
}

#[tokio::test]
async fn publish_rejects_wrong_hash() {
    let db = CacheDb::open_in_memory().unwrap();
    let kp = ActorKeypair::generate();
    let wasm = MINIMAL_LABELER_WAT.as_bytes().to_vec();
    // Sign metadata that claims the correct hash for `wasm` …
    let metadata = signed_labeler(&kp, 1, &wasm);
    // … but publish DIFFERENT bytes (still valid WASM) so wasm_size/hash mismatch.
    let other_wasm = b"\0asm\x01\0\0\0".to_vec(); // valid WASM header, wrong content
    let err = publish_labeler_core(&db, &TEST_CALLER, &metadata, &other_wasm, "post", "wasm")
        .await
        .expect_err("wasm_hash/size mismatch must be rejected");
    assert!(db.list_labelers().await.unwrap().is_empty());
    let _ = err;
}

#[tokio::test]
async fn republish_requires_strictly_increasing_version() {
    let db = CacheDb::open_in_memory().unwrap();
    let kp = ActorKeypair::generate();
    let wasm = MINIMAL_LABELER_WAT.as_bytes().to_vec();

    let v1 = signed_labeler(&kp, 1, &wasm);
    publish_labeler_core(&db, &TEST_CALLER, &v1, &wasm, "post", "wasm")
        .await
        .unwrap();

    // re-publishing v1 (equal version) is rejected …
    let err = publish_labeler_core(&db, &TEST_CALLER, &v1, &wasm, "post", "wasm")
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.protocol.malformed");

    // … but a strictly-newer version is accepted and replaces the row.
    let v2 = signed_labeler(&kp, 2, &wasm);
    let (_, ver) = publish_labeler_core(&db, &TEST_CALLER, &v2, &wasm, "post", "wasm")
        .await
        .unwrap();
    assert_eq!(ver, 2);
    assert_eq!(db.list_labelers().await.unwrap().len(), 1);
    assert_eq!(
        db.get_labeler(&kp.actor_id().0)
            .await
            .unwrap()
            .unwrap()
            .version,
        2
    );
}

#[tokio::test]
async fn subscribe_to_absent_labeler_is_not_found() {
    let db = CacheDb::open_in_memory().unwrap();
    let owner = [0x01u8; 32];
    let missing = [0x02u8; 32];
    let err = subscribe_labeler_core(&db, &owner, &missing, None)
        .await
        .expect_err("subscribing to an unknown labeler must fail");
    // `fauna.labelers.not_found`, not the central `fauna.bridges.not_found`:
    // the handler builds it through `not_found_ns("labelers", …)`, which is the
    // per-namespace convention (`rpc_errors::not_found_ns_uses_the_namespaced_code`).
    // This assertion still read `bridges` — stale since the error-namespace
    // sweep — and was failing on `origin/main` independently of this track.
    assert_eq!(err.code, "fauna.labelers.not_found");
}

/// Slice-3a end-to-end (nest): a **mail-kind** labeler is publishable (D7
/// `content_kind`), and **subscribing seeds the owner's mail backlog** so the
/// re-score obligation scan actually owes a re-score — closing the new-factor
/// gap (design revision 2026-07-07, point 3). The BEFORE/AFTER assertions around
/// `subscribe` are the red-green delta: registering the version alone owes
/// nothing; the seed is what makes the worklist non-empty.
#[tokio::test]
async fn subscribe_mail_labeler_seeds_owner_backlog_so_worklist_owes_a_rescore() {
    use fauna_nest::db::bridge_routing::ScanResultRow;

    let db = CacheDb::open_in_memory().unwrap();
    let kp = ActorKeypair::generate();
    let labeler_id = kp.actor_id().0;
    let factor = labeler_factor(&kp.actor_id());
    let wasm = MINIMAL_LABELER_WAT.as_bytes().to_vec();
    let metadata = signed_labeler(&kp, 1, &wasm);

    // Publish a MAIL-kind labeler (the holder-served vehicle; D7). The signed
    // `AlgorithmLabeler` is unchanged — the kind rides the publish envelope.
    let (_id, _ver) = publish_labeler_core(&db, &TEST_CALLER, &metadata, &wasm, "mail", "wasm")
        .await
        .expect("publish a mail-kind labeler");
    // …and the declared kind is projected into the catalog for subscribers.
    let list = db.list_labelers().await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].content_kind, "mail");

    // The owner has one delivered mail item — its `message_id` IS the 32-byte
    // content id the drain later unseals + scores.
    let owner = [0x42u8; 32];
    let content_id = [0xABu8; 32];
    db.insert_scan_result(&ScanResultRow {
        message_id: content_id,
        received_at: 1_700_000_000,
        scanned_at: 1_700_000_000,
        clamav_verdict: "clean".into(),
        clamav_signature: None,
        rspamd_score_raw: None,
        rspamd_score_scaled: None,
        rspamd_flagged_rules: None,
        rspamd_score_breakdown: None,
        action_taken: "delivered".into(),
        delivered_to_actor: Some(owner),
    })
    .await
    .expect("record a delivered mail scan row");

    // BEFORE subscribe: the `labeler:<id>` factor has no `content_scores` rows,
    // so the owner-scoped obligation scan owes nothing — the new-factor gap.
    let before = db
        .content_scores_behind_for_owner(&factor, &owner, 1, 100)
        .await
        .unwrap();
    assert!(
        before.is_empty(),
        "no obligation before subscribe (the new-factor gap the seed closes)"
    );

    // Subscribe (public-only path, grant_id None) → registers the version AND
    // seeds the owner's mail backlog.
    let registered = subscribe_labeler_core(&db, &owner, &labeler_id, None)
        .await
        .expect("subscribe to the mail labeler");
    assert_eq!(registered, factor);

    // AFTER subscribe: the owner's mail item is behind version 0 < registered 1,
    // so it surfaces in the worklist — the drain now has a unit of work.
    let after = db
        .content_scores_behind_for_owner(&factor, &owner, 1, 100)
        .await
        .unwrap();
    assert_eq!(
        after.len(),
        1,
        "subscribe must seed the owner's mail backlog so the drain owes a re-score"
    );
    assert_eq!(after[0].content_id, content_id.to_vec());
    assert_eq!(after[0].content_kind, "mail");
    assert_eq!(after[0].actor_id.as_deref(), Some(&owner[..]));
    assert_eq!(after[0].scorer_version, 0);
}

/// A `post`-kind labeler (the default) seeds **nothing** on subscribe —
/// post-content-at-rest does not exist yet, so there is no item universe to
/// materialize an obligation over (design: post drains arrive with their holder).
#[tokio::test]
async fn subscribe_post_labeler_seeds_no_backlog() {
    let db = CacheDb::open_in_memory().unwrap();
    let kp = ActorKeypair::generate();
    let labeler_id = kp.actor_id().0;
    let factor = labeler_factor(&kp.actor_id());
    let wasm = MINIMAL_LABELER_WAT.as_bytes().to_vec();
    let metadata = signed_labeler(&kp, 1, &wasm);

    publish_labeler_core(&db, &TEST_CALLER, &metadata, &wasm, "post", "wasm")
        .await
        .expect("publish a post labeler");
    assert_eq!(db.list_labelers().await.unwrap()[0].content_kind, "post");

    let owner = [0x42u8; 32];
    subscribe_labeler_core(&db, &owner, &labeler_id, None)
        .await
        .expect("subscribe to the post labeler");

    let after = db
        .content_scores_behind_for_owner(&factor, &owner, 1, 100)
        .await
        .unwrap();
    assert!(
        after.is_empty(),
        "a post-kind labeler has no content-at-rest universe to seed in v1"
    );
}

/// A publish that declares no `content_kind` is rejected as `malformed` (never
/// stored) — every publisher names its kind; there is no default.
#[tokio::test]
async fn publish_rejects_empty_content_kind() {
    let db = CacheDb::open_in_memory().unwrap();
    let kp = ActorKeypair::generate();
    let wasm = MINIMAL_LABELER_WAT.as_bytes().to_vec();
    let metadata = signed_labeler(&kp, 1, &wasm);
    let err = publish_labeler_core(&db, &TEST_CALLER, &metadata, &wasm, "", "wasm")
        .await
        .expect_err("an empty content_kind must be rejected");
    assert_eq!(err.code, "fauna.protocol.malformed");
    assert!(
        db.list_labelers().await.unwrap().is_empty(),
        "a rejected publish must not store a labeler"
    );
}

/// A publish that declares an unsupported `content_kind` is rejected as
/// `malformed` (never stored) — the nest validates the allowed set.
#[tokio::test]
async fn publish_rejects_unknown_content_kind() {
    let db = CacheDb::open_in_memory().unwrap();
    let kp = ActorKeypair::generate();
    let wasm = MINIMAL_LABELER_WAT.as_bytes().to_vec();
    let metadata = signed_labeler(&kp, 1, &wasm);
    let err = publish_labeler_core(&db, &TEST_CALLER, &metadata, &wasm, "video", "wasm")
        .await
        .expect_err("an unsupported content_kind must be rejected");
    assert_eq!(err.code, "fauna.protocol.malformed");
    assert!(
        db.list_labelers().await.unwrap().is_empty(),
        "a rejected publish must not store a labeler"
    );
}

/// Security review, at the real publish path (`publish_labeler_core`): the
/// per-publisher quota keys on the self-signed `algorithm_id`, so a `publish` loop
/// with a **fresh signing keypair per labeler** evades it (each publisher owns one
/// row). The authenticated caller is the un-rotatable identity; with it fixed, the
/// loop hits the per-caller cap. This proves the caller is threaded from the real
/// publish path into the quota, not just that the DB-layer cap exists. Each
/// publish is a fully valid signed artifact over a real compiling module.
#[tokio::test]
async fn publish_loop_with_rotating_keypair_hits_caller_cap() {
    let db = CacheDb::open_in_memory().unwrap();
    let caller = [0xE1u8; 32]; // one fixed enrolled identity for the whole loop
    let wasm = MINIMAL_LABELER_WAT.as_bytes().to_vec();

    for _ in 0..MAX_LABELERS_PER_CALLER {
        // A FRESH keypair each iteration → a distinct `algorithm_id`/publisher, so
        // the per-publisher cap never fires (the evasion the old code allowed).
        let kp = ActorKeypair::generate();
        let metadata = signed_labeler(&kp, 1, &wasm);
        publish_labeler_core(&db, &caller, &metadata, &wasm, "post", "wasm")
            .await
            .expect("each fresh-keypair publish is under the per-publisher cap");
    }
    assert_eq!(
        db.list_labelers().await.unwrap().len(),
        MAX_LABELERS_PER_CALLER,
        "all fresh-keypair publishes were accepted (per-publisher cap never fired)"
    );

    // The next fresh-keypair publish by the same caller is rejected as `malformed`
    // (the caller-quota rejection is client-actionable, not a server error).
    let kp = ActorKeypair::generate();
    let metadata = signed_labeler(&kp, 1, &wasm);
    let err = publish_labeler_core(&db, &caller, &metadata, &wasm, "post", "wasm")
        .await
        .expect_err("rotating the keypair must NOT evade the per-caller cap");
    assert_eq!(err.code, "fauna.protocol.malformed");
    assert_eq!(
        db.list_labelers().await.unwrap().len(),
        MAX_LABELERS_PER_CALLER,
        "the over-cap publish must not be stored"
    );

    // A DIFFERENT authenticated caller still has its own independent quota.
    let other_caller = [0xE2u8; 32];
    let kp = ActorKeypair::generate();
    let metadata = signed_labeler(&kp, 1, &wasm);
    publish_labeler_core(&db, &other_caller, &metadata, &wasm, "post", "wasm")
        .await
        .expect("a different caller's quota is independent");
}

// ── Block A: the List-labeler artifact kind (design D8–D14) ──────────────────

/// Canonical dag-cbor List artifact bytes over `(content_id, score)` entries
/// (caller keeps them strictly ascending — the canonical form publish enforces).
/// The List→feed proof this row shape existed FOR (frame § Tier-3 artifact
/// kinds — "the composed `query_feed` will be the first reader of these
/// rows"): publish a 2-entry List → subscribe (nest materializes the tier-3
/// rows) → a composed feed ordering over `labeler:<id>` ranks by the list's
/// scores, beating recency; a negative weight flips it. End-to-end over the
/// production materialization path — no hand-seeded bus rows.
#[tokio::test]
async fn subscribed_list_labeler_drives_composed_feed_ordering() {
    let db = CacheDb::open_in_memory().unwrap();
    let kp = ActorKeypair::generate();
    let labeler_id = kp.actor_id().0;
    let factor = labeler_factor(&kp.actor_id());
    let author = [0x98u8; 32];

    // post_a older/high-listed, post_b newer/low-listed.
    let post_a = [0x1Au8; 32];
    let post_b = [0x1Bu8; 32];
    db.insert_post_index_entry(&post_a, &author, 1_700_000_000, false, false, "fauna", &[])
        .await
        .unwrap();
    db.insert_post_index_entry(&post_b, &author, 1_700_000_100, false, false, "fauna", &[])
        .await
        .unwrap();

    let artifact = list_artifact_bytes(&[(post_a, 900), (post_b, 100)]);
    let metadata = signed_labeler(&kp, 1, &artifact);
    publish_labeler_core(&db, &TEST_CALLER, &metadata, &artifact, "post", "list")
        .await
        .expect("publish list labeler");
    subscribe_labeler_core(&db, &[0x42u8; 32], &labeler_id, None)
        .await
        .expect("subscribe");

    let composition = [fauna_core::scoring::CompositionEntry {
        factor: factor.clone(),
        weight_permille: 1000,
    }];
    let rows = db
        .query_feed_scored(
            &[],
            fauna_core::scoring::FilterCombination::All,
            &[],
            None,
            10,
            &composition,
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0].post_id.as_slice(),
        &post_a,
        "the subscribed List's 900 beats the newer post's 100"
    );
    assert_eq!(rows[0].score, Some(900.0));
    assert_eq!(rows[1].score, Some(100.0));

    // Negative weight: the same List as a penalty — order flips.
    let penalty = [fauna_core::scoring::CompositionEntry {
        factor,
        weight_permille: -1000,
    }];
    let rows = db
        .query_feed_scored(
            &[],
            fauna_core::scoring::FilterCombination::All,
            &[],
            None,
            10,
            &penalty,
        )
        .await
        .unwrap();
    assert_eq!(rows[0].post_id.as_slice(), &post_b);
}

/// Build the artifact bytes through the **shared publisher-side builder** every
/// app publishes with (`build_list_artifact`), so the honest-path tests
/// exercise the real producer rather than a parallel re-derivation of the
/// canonical form.
fn list_artifact_bytes(entries: &[([u8; 32], i64)]) -> Vec<u8> {
    fauna_core::scoring::build_list_artifact(None, entries.to_vec()).expect("build list artifact")
}

/// Encode `entries` **verbatim** — no sort, no dedup, no validation — for the
/// adversarial gate tests. The nest's publish gate exists to defend against a
/// malicious or buggy publisher, so its negative cases must be able to produce
/// bytes the honest `build_list_artifact` refuses to emit by construction. Do
/// not "fix" these call sites onto the shared builder: that would silently turn
/// every rejection test into an acceptance test.
fn raw_list_artifact_bytes(entries: &[([u8; 32], i64)]) -> Vec<u8> {
    let artifact = fauna_core::scoring::LabelerListArtifact {
        entries: entries
            .iter()
            .map(|(id, score)| fauna_core::scoring::ListEntry {
                content_id: serde_bytes::ByteBuf::from(id.to_vec()),
                score: *score,
            })
            .collect(),
        name: None,
    };
    canonical_encode(&artifact).expect("encode list artifact")
}

/// The full List lifecycle (design D12): publish → subscribe materializes
/// `content_scores` rows nest-side (only for ids in `content_meta`, with
/// `actor_id = NULL` — the report:spam public-post shape), NO `model_versions`
/// registration (no holder, no drain), post-arrival join, republish resync
/// (drops withdrawn entries, updates scores/version), and last-unsubscribe
/// withdraw.
#[tokio::test]
async fn list_labeler_lifecycle_materializes_and_withdraws_bus_rows() {
    use fauna_nest::labeler_handlers::unsubscribe_labeler_core;

    let db = CacheDb::open_in_memory().unwrap();
    let kp = ActorKeypair::generate();
    let labeler_id = kp.actor_id().0;
    let factor = labeler_factor(&kp.actor_id());
    let author = [0x99u8; 32];

    // post_a exists on this nest (content_meta row); post_b does NOT (yet).
    let post_a = [0x0Au8; 32];
    let post_b = [0x0Bu8; 32];
    db.insert_post_index_entry(&post_a, &author, 1_700_000_000, false, false, "fauna", &[])
        .await
        .expect("index post_a");

    // Publish the List: a=900, b=400 (strictly ascending ids).
    let artifact = list_artifact_bytes(&[(post_a, 900), (post_b, 400)]);
    let metadata = signed_labeler(&kp, 1, &artifact);
    publish_labeler_core(&db, &TEST_CALLER, &metadata, &artifact, "post", "list")
        .await
        .expect("publish a valid list labeler");
    let list = db.list_labelers().await.unwrap();
    assert_eq!(list[0].artifact_kind, "list");
    // Publish alone materializes NOTHING (subscription-gated).
    assert!(db.get_content_scores(&post_a).await.unwrap().is_empty());

    // A grant on a List subscribe is malformed (public-only — nothing to grant).
    let owner = [0x42u8; 32];
    let err = subscribe_labeler_core(&db, &owner, &labeler_id, Some(&[0x77u8; 16]))
        .await
        .expect_err("a list labeler must reject a grant_id");
    assert_eq!(err.code, "fauna.protocol.malformed");

    // Subscribe (public-only) → the nest materializes the bus rows.
    let registered = subscribe_labeler_core(&db, &owner, &labeler_id, None)
        .await
        .expect("subscribe to the list labeler");
    assert_eq!(registered, factor);

    // post_a (on-nest) got its row: factor / score / tier 3 / version 1 …
    let scores_a = db.get_content_scores(&post_a).await.unwrap();
    assert_eq!(scores_a.len(), 1);
    assert_eq!(scores_a[0].factor, factor);
    assert_eq!(scores_a[0].score, 900);
    assert_eq!(scores_a[0].tier, 3);
    assert_eq!(scores_a[0].scorer_version, 1);
    // … with actor_id = NULL (the crux: a public post has no owner scope — the
    // report:spam post-row precedent; feed composition JOINs by content_id).
    assert_eq!(db.content_score_owner(&post_a).await.unwrap(), None);
    // post_b is not on this nest → NO blind row.
    assert!(db.get_content_scores(&post_b).await.unwrap().is_empty());
    // NO model_versions registration — a List has no holder and no drain (the
    // report:spam outside-builtin_factor_versions precedent).
    assert_eq!(db.get_model_version(&factor).await.unwrap(), None);

    // post_b arrives later → the post-arrival join writes its row on ingest.
    db.insert_post_index_entry(&post_b, &author, 1_700_000_100, false, false, "fauna", &[])
        .await
        .expect("index post_b");
    let scores_b = db.get_content_scores(&post_b).await.unwrap();
    assert_eq!(scores_b.len(), 1, "post-arrival join must write the row");
    assert_eq!(scores_b[0].score, 400);

    // Republish v2: post_a rescored to 700, post_b DROPPED → resync updates a,
    // withdraws b (subscription still live).
    let artifact_v2 = list_artifact_bytes(&[(post_a, 700)]);
    let metadata_v2 = signed_labeler(&kp, 2, &artifact_v2);
    publish_labeler_core(
        &db,
        &TEST_CALLER,
        &metadata_v2,
        &artifact_v2,
        "post",
        "list",
    )
    .await
    .expect("republish the list at v2");
    let scores_a = db.get_content_scores(&post_a).await.unwrap();
    assert_eq!(scores_a[0].score, 700);
    assert_eq!(scores_a[0].scorer_version, 2);
    assert!(
        db.get_content_scores(&post_b).await.unwrap().is_empty(),
        "an entry dropped by the new version is withdrawn"
    );

    // A second subscriber; the FIRST unsubscribe leaves the rows (still one
    // subscriber), the LAST withdraws them (derived, recreatable — D12c).
    let owner2 = [0x43u8; 32];
    subscribe_labeler_core(&db, &owner2, &labeler_id, None)
        .await
        .expect("second subscriber");
    unsubscribe_labeler_core(&db, &owner, &labeler_id)
        .await
        .expect("first unsubscribe");
    assert_eq!(
        db.get_content_scores(&post_a).await.unwrap().len(),
        1,
        "rows stay while a subscriber remains"
    );
    unsubscribe_labeler_core(&db, &owner2, &labeler_id)
        .await
        .expect("last unsubscribe");
    assert!(
        db.get_content_scores(&post_a).await.unwrap().is_empty(),
        "the last unsubscribe withdraws the factor's rows"
    );
}

/// v1 Lists are public-post-only: a `mail` List is rejected at publish (mail is
/// inherently per-recipient, never public — design D10).
#[tokio::test]
async fn publish_rejects_mail_list() {
    let db = CacheDb::open_in_memory().unwrap();
    let kp = ActorKeypair::generate();
    let artifact = list_artifact_bytes(&[([0x01u8; 32], 500)]);
    let metadata = signed_labeler(&kp, 1, &artifact);
    let err = publish_labeler_core(&db, &TEST_CALLER, &metadata, &artifact, "mail", "list")
        .await
        .expect_err("a mail-kind list must be rejected");
    assert_eq!(err.code, "fauna.protocol.malformed");
    assert!(db.list_labelers().await.unwrap().is_empty());
}

/// A non-canonical List artifact (unsorted / duplicate / out-of-range score) is
/// rejected at publish, never stored (design D9/D10) — and an unknown or empty
/// artifact_kind is malformed.
#[tokio::test]
async fn publish_rejects_noncanonical_list_artifact_and_unknown_kind() {
    let db = CacheDb::open_in_memory().unwrap();
    let kp = ActorKeypair::generate();

    // Unsorted entries (2 before 1) — raw, since the shared builder would sort
    // them into canonical form and there would be nothing left to reject.
    let unsorted = raw_list_artifact_bytes(&[([0x02u8; 32], 10), ([0x01u8; 32], 10)]);
    let metadata = signed_labeler(&kp, 1, &unsorted);
    let err = publish_labeler_core(&db, &TEST_CALLER, &metadata, &unsorted, "post", "list")
        .await
        .expect_err("an unsorted list must be rejected");
    assert_eq!(err.code, "fauna.protocol.malformed");

    // Out-of-range score — raw, for the same reason (the builder refuses it).
    let out_of_range = raw_list_artifact_bytes(&[([0x01u8; 32], 1001)]);
    let metadata = signed_labeler(&kp, 1, &out_of_range);
    let err = publish_labeler_core(&db, &TEST_CALLER, &metadata, &out_of_range, "post", "list")
        .await
        .expect_err("an out-of-range score must be rejected");
    assert_eq!(err.code, "fauna.protocol.malformed");

    // Unknown artifact_kind.
    let ok_list = list_artifact_bytes(&[([0x01u8; 32], 500)]);
    let metadata = signed_labeler(&kp, 1, &ok_list);
    let err = publish_labeler_core(&db, &TEST_CALLER, &metadata, &ok_list, "post", "sql")
        .await
        .expect_err("an unknown artifact_kind must be rejected");
    assert_eq!(err.code, "fauna.protocol.malformed");

    // An empty artifact_kind is refused too — every publisher names its kind,
    // and there is no older-peer default to fall back on.
    let err = publish_labeler_core(&db, &TEST_CALLER, &metadata, &ok_list, "post", "")
        .await
        .expect_err("an empty artifact_kind must be rejected");
    assert_eq!(err.code, "fauna.protocol.malformed");

    assert!(db.list_labelers().await.unwrap().is_empty());
}

// ── v2 `text-model` artifact kind (frame § Tier-3 artifact kinds, ratified
// 2026-08-13; mechanism owner `behavior/topic-factors.md` § Publishing) ──────

/// The canonical bytes a real publisher emits, through the shared builder — the
/// [`list_artifact_bytes`] convention.
fn text_model_artifact_bytes(
    more_docs: u32,
    less_docs: u32,
    ngrams: &[(&str, u32, u32)],
) -> Vec<u8> {
    fauna_core::scoring::build_text_model_artifact(
        None,
        more_docs,
        less_docs,
        ngrams
            .iter()
            .map(|(g, m, l)| ((*g).to_string(), *m, *l))
            .collect(),
    )
    .expect("build text-model artifact")
}

/// Encode a vocabulary **verbatim** at an explicit `version` — no sort, no
/// prune, no validation. The [`raw_list_artifact_bytes`] rule applies here too:
/// the gate's negative cases must be able to produce bytes the honest builder
/// refuses by construction, so do NOT "fix" these call sites onto the builder.
fn raw_text_model_artifact_bytes_at_version(
    version: u16,
    more_docs: u32,
    less_docs: u32,
    ngrams: &[(&str, u32, u32)],
) -> Vec<u8> {
    let artifact = fauna_core::scoring::TextModelArtifact {
        version,
        more_docs,
        less_docs,
        ngrams: ngrams
            .iter()
            .map(|(g, m, l)| fauna_core::scoring::TextModelNgram {
                ngram: (*g).to_string(),
                more: *m,
                less: *l,
            })
            .collect(),
        name: None,
    };
    canonical_encode(&artifact).expect("encode text-model artifact")
}

fn raw_text_model_artifact_bytes(
    more_docs: u32,
    less_docs: u32,
    ngrams: &[(&str, u32, u32)],
) -> Vec<u8> {
    raw_text_model_artifact_bytes_at_version(
        fauna_core::scoring::TEXT_MODEL_ARTIFACT_VERSION,
        more_docs,
        less_docs,
        ngrams,
    )
}

/// A `text-model` labeler is **evaluated at the subscriber's client, never the
/// nest**: subscribe records the `labeler_subscriptions` row and nothing else —
/// no `content_scores`, no `model_versions`, no backlog seed, no grant.
#[tokio::test]
async fn subscribing_to_a_text_model_labeler_records_the_row_and_nothing_else() {
    let db = CacheDb::open_in_memory().unwrap();
    let kp = ActorKeypair::generate();
    let labeler_id = kp.actor_id().0;
    let factor = labeler_factor(&kp.actor_id());
    let author = [0x99u8; 32];

    // A post this nest has, so "no rows" cannot pass for the trivial reason that
    // the nest had no content to attach a row to.
    let post = [0x0Au8; 32];
    db.insert_post_index_entry(&post, &author, 1_700_000_000, false, false, "fauna", &[])
        .await
        .expect("index post");

    let artifact = text_model_artifact_bytes(4, 2, &[("cat", 4, 1), ("orange", 3, 0)]);
    let metadata = signed_labeler(&kp, 1, &artifact);
    publish_labeler_core(
        &db,
        &TEST_CALLER,
        &metadata,
        &artifact,
        "post",
        "text-model",
    )
    .await
    .expect("publish a valid text-model labeler");
    assert_eq!(
        db.list_labelers().await.unwrap()[0].artifact_kind,
        "text-model"
    );

    // A grant is meaningless: the client scores only content it already reads.
    let owner = [0x42u8; 32];
    let err = subscribe_labeler_core(&db, &owner, &labeler_id, Some(&[0x77u8; 16]))
        .await
        .expect_err("a text-model labeler must reject a grant_id");
    assert_eq!(err.code, "fauna.protocol.malformed");

    let registered = subscribe_labeler_core(&db, &owner, &labeler_id, None)
        .await
        .expect("subscribe to the text-model labeler");
    assert_eq!(registered, factor);

    assert!(
        db.get_content_scores(&post).await.unwrap().is_empty(),
        "the nest must not evaluate a text-model labeler — no bus rows"
    );
    assert_eq!(
        db.get_model_version(&factor).await.unwrap(),
        None,
        "no model_versions entry: nothing to drain, no holder"
    );
}

/// ⚠ The frame's **republish resync clause**: when a new version's kind is not
/// `list`, the nest **withdraws** that labeler's materialized List rows.
///
/// Without it, a factor that upgrades List → Model leaves its old nest-side
/// scores standing forever — and they would keep composing into every
/// subscriber's feed under the same factor key the client is now *also* scoring
/// client-side, double-counting a model the publisher has already replaced.
#[tokio::test]
async fn republishing_a_list_as_a_text_model_withdraws_the_materialized_rows() {
    let db = CacheDb::open_in_memory().unwrap();
    let kp = ActorKeypair::generate();
    let labeler_id = kp.actor_id().0;
    let author = [0x99u8; 32];
    let post = [0x0Au8; 32];
    db.insert_post_index_entry(&post, &author, 1_700_000_000, false, false, "fauna", &[])
        .await
        .expect("index post");

    // v1 as a List, subscribed → the nest materializes a row.
    let list = list_artifact_bytes(&[(post, 900)]);
    let metadata = signed_labeler(&kp, 1, &list);
    publish_labeler_core(&db, &TEST_CALLER, &metadata, &list, "post", "list")
        .await
        .expect("publish v1 as a list");
    let owner = [0x42u8; 32];
    subscribe_labeler_core(&db, &owner, &labeler_id, None)
        .await
        .expect("subscribe");
    assert_eq!(
        db.get_content_scores(&post).await.unwrap().len(),
        1,
        "precondition: the List materialized a row"
    );

    // v2 as a Model — same factor, same publisher identity, ordinary version
    // bump (the client-side upgrade path `publish_trained_factor_model` drives).
    let model = text_model_artifact_bytes(4, 2, &[("cat", 4, 1), ("orange", 3, 0)]);
    let metadata_v2 = signed_labeler(&kp, 2, &model);
    publish_labeler_core(
        &db,
        &TEST_CALLER,
        &metadata_v2,
        &model,
        "post",
        "text-model",
    )
    .await
    .expect("republish v2 as a text-model");

    assert!(
        db.get_content_scores(&post).await.unwrap().is_empty(),
        "a kind-changing republish must withdraw the factor's List rows"
    );
    assert_eq!(
        db.list_labelers().await.unwrap()[0].artifact_kind,
        "text-model",
        "and the stored kind follows the new version"
    );
}

/// The publish gate refuses a text-model artifact that is out of contract —
/// including one below the **distinct-document prune floor**, which is the
/// boundary that makes the anti-quote floor a property of the artifact rather
/// than of the publishing client's diligence.
#[tokio::test]
async fn publish_rejects_a_text_model_artifact_out_of_contract() {
    let db = CacheDb::open_in_memory().unwrap();
    let kp = ActorKeypair::generate();

    // Below the prune floor: a single-document n-gram is a quote.
    let quote = raw_text_model_artifact_bytes(9, 9, &[("secret", 1, 0)]);
    let metadata = signed_labeler(&kp, 1, &quote);
    let err = publish_labeler_core(&db, &TEST_CALLER, &metadata, &quote, "post", "text-model")
        .await
        .expect_err("a below-floor n-gram must be rejected at the gate");
    assert_eq!(err.code, "fauna.protocol.malformed");

    // Non-canonical order.
    let unsorted = raw_text_model_artifact_bytes(9, 9, &[("b", 3, 0), ("a", 3, 0)]);
    let metadata = signed_labeler(&kp, 1, &unsorted);
    let err = publish_labeler_core(
        &db,
        &TEST_CALLER,
        &metadata,
        &unsorted,
        "post",
        "text-model",
    )
    .await
    .expect_err("an unsorted vocabulary must be rejected");
    assert_eq!(err.code, "fauna.protocol.malformed");

    // A count above its class document counter.
    let impossible = raw_text_model_artifact_bytes(2, 9, &[("a", 3, 0)]);
    let metadata = signed_labeler(&kp, 1, &impossible);
    let err = publish_labeler_core(
        &db,
        &TEST_CALLER,
        &metadata,
        &impossible,
        "post",
        "text-model",
    )
    .await
    .expect_err("a count above its doc counter must be rejected");
    assert_eq!(err.code, "fauna.protocol.malformed");

    // Mail is per-recipient, never public — the List's rule, restated for v2.
    let ok = text_model_artifact_bytes(4, 2, &[("cat", 4, 1)]);
    let metadata = signed_labeler(&kp, 1, &ok);
    let err = publish_labeler_core(&db, &TEST_CALLER, &metadata, &ok, "mail", "text-model")
        .await
        .expect_err("a mail text-model must be rejected");
    assert_eq!(err.code, "fauna.protocol.malformed");

    assert!(db.list_labelers().await.unwrap().is_empty());
}

/// ⚠ COMPAT PIN. A **newer client** may publish a text-model artifact at a
/// tokenizer `version` this nest has never heard of, and the nest must store it
/// — the version is the *subscriber's* contract, not the nest's, and a
/// subscriber meeting an unknown version goes inert rather than mis-scoring.
/// Rejecting here would break the bidirectional compatibility
/// `version-compatibility.md` requires within a major version.
#[tokio::test]
async fn publish_accepts_a_future_tokenizer_version() {
    let db = CacheDb::open_in_memory().unwrap();
    let kp = ActorKeypair::generate();

    let future = raw_text_model_artifact_bytes_at_version(99, 4, 2, &[("cat", 4, 1)]);
    let metadata = signed_labeler(&kp, 1, &future);
    publish_labeler_core(&db, &TEST_CALLER, &metadata, &future, "post", "text-model")
        .await
        .expect("an older nest must accept a newer client's tokenizer version");
    assert_eq!(db.list_labelers().await.unwrap().len(), 1);
}

/// Over the 64 KiB text-model cap — the gate that bounds the worst-case bytes,
/// beside the 1 MiB wasm cap.
#[tokio::test]
async fn publish_rejects_an_oversize_text_model_artifact() {
    use fauna_nest::db::labelers::TEXT_MODEL_ARTIFACT_MAX_BYTES;

    let db = CacheDb::open_in_memory().unwrap();
    let kp = ActorKeypair::generate();

    // A full 512-n-gram vocabulary of long n-grams: the vocabulary cap and the
    // byte cap are independent bounds, and this pins that the byte one bites.
    let filler = "x".repeat(200);
    let grams: Vec<(String, u32, u32)> = (0..512)
        .map(|i| (format!("{filler}{i:04}"), 3u32, 0u32))
        .collect();
    let artifact = fauna_core::scoring::build_text_model_artifact(None, 3, 0, grams)
        .expect("the vocabulary itself is in contract");
    assert!(
        artifact.len() > TEXT_MODEL_ARTIFACT_MAX_BYTES,
        "fixture check: this artifact must actually exceed the byte cap"
    );

    let metadata = signed_labeler(&kp, 1, &artifact);
    let err = publish_labeler_core(
        &db,
        &TEST_CALLER,
        &metadata,
        &artifact,
        "post",
        "text-model",
    )
    .await
    .expect_err("an oversize text-model artifact must be rejected");
    assert_eq!(err.code, "fauna.protocol.malformed");
}
