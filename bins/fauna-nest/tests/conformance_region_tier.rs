//! The region tier, end to end — W2 (account-data-plane.md § Workstreams) slice 4 of
//! `docs/goal/architecture/dynamic-features.md` (§ The region tier, § Fail
//! posture) over `docs/goal/behavior/region-blocking.md` § The region/authority
//! plumbing.
//!
//! **What these tests are for.** `RuleTier::Region` has existed in the meet, in
//! the wire types and in the attribution since slice 2 with no producer. The
//! risk in giving it one is not that the arithmetic is wrong — `fauna-core`'s
//! own tests cover the verdict and this file's sibling covers the counters — it
//! is that an *illegitimate* document reaches the meet, or that a legitimate one
//! silently stops binding. So every test below asks one of those two questions.
//!
//! The registry these tests verify against is a **local fixture** enrolling a
//! synthetic authority. The compiled-in registry is empty (nobody is enrolled
//! yet), and `the_compiled_in_registry_binds_nobody` is the test that pins what
//! that means in production.
//!
//! Tier: tier_3 (real `fauna-nest` `AppState` + real `CacheDb` — no mocks).

use std::sync::Arc;

use ed25519_dalek::SigningKey;

use fauna_core::feature_gate::{
    Availability, FeaturePolicy, GatedFeature, QuotaDimension, RuleTier, Window, WindowedBounds,
};
use fauna_core::identity::ActorKeypair;
use fauna_core::region_authority::{
    AuthorityKey, PAYLOAD_KIND_CONTENT_POLICY, PAYLOAD_KIND_FEATURE_POLICY, PolicyArtifact,
    RegionCode, RegionEntry, RegionFeaturePolicies, RegionRegistry, sign_artifact,
};
use fauna_nest::{
    db::CacheDb,
    region_tier::{
        Ingested, ingest_feature_policy, ingest_relay_artifact, refold_stored_artifact,
        refresh_relay_once,
    },
    routes::AppState,
    rpc_router::RpcRouter,
};
use fauna_protocol::{
    decode_strict as decode, encode_canonical,
    features::{FeaturesStatusReply, FeaturesStatusRequest},
    region::{
        AdminRegionSetReply, AdminRegionSetRequest, AdminRegionStatusReply, RegionArtifactGetReply,
        RegionArtifactGetRequest,
    },
};

mod common;

const AUTHORITY_SEED: u8 = 42;
const SUCCESSOR_SEED: u8 = 43;

fn build_router() -> RpcRouter {
    let mut b = RpcRouter::builder();
    fauna_nest::feature_gate::register_features_handlers(&mut b);
    fauna_nest::region_tier::register_region_handlers(&mut b);
    fauna_nest::region_relay::register_region_relay_handlers(&mut b);
    b.build()
}

async fn router_with_db() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    (build_router(), state)
}

/// A router over an `AppState` whose two relay demand doors —
/// `region_relay::artifact_get_handler` and
/// `region_tier::demand_situs_content_policies` — verify enrollment against
/// `fixture_registry()` instead of the (today-empty) compiled-in registry,
/// via `AppState::install_region_registry_for_test`. So a first ask or a
/// situs declaration is witnessed enrolled *through the door*, not seeded
/// past it — while the refill a first ask schedules still reads the real
/// compiled-in (empty) registry, so it never reaches the network
/// (`demand_door_registry`'s own doc).
async fn router_with_enrolled_registry() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let mut state = AppState::for_test(db);
    state.install_region_registry_for_test(fixture_registry());
    (build_router(), Arc::new(state))
}

/// See `conformance_feature_gate.rs`: `set_admin_role` alone does not make an
/// admin — the `admin_actor_ids` row is what `is_admin` counts.
async fn seed_admin(state: &Arc<AppState>) -> [u8; 32] {
    let admin = ActorKeypair::generate().actor_id().0;
    state.db.create_user(&admin, "free", "admin").await.unwrap();
    state.db.add_admin_actor(&admin[..]).await.unwrap();
    admin
}

async fn seed_user(state: &Arc<AppState>, handle: &str) -> [u8; 32] {
    let user = ActorKeypair::generate().actor_id().0;
    state.db.create_user(&user, "free", handle).await.unwrap();
    user
}

fn region() -> RegionCode {
    RegionCode::parse("NO").unwrap()
}

fn authority_key() -> SigningKey {
    SigningKey::from_bytes(&[AUTHORITY_SEED; 32])
}

/// A registry enrolling one synthetic authority for `NO`.
fn fixture_registry() -> RegionRegistry {
    RegionRegistry {
        version: 1,
        regions: vec![RegionEntry {
            region: region(),
            authority_name: "Fixture Authority".into(),
            official_domain: "authority.example".into(),
            parent: None,
            keys: vec![AuthorityKey {
                key_id: "k1".into(),
                public_key: authority_key().verifying_key().to_bytes().to_vec(),
                enrolled_at: 0,
                retired_at: None,
            }],
        }],
    }
}

/// A registry revision enrolling a **different** authority for the same region
/// — the curated act behind the ruling: replacing who administers a
/// region starts a fresh sequence space, because the counter belongs to the
/// authority, not to the region code.
fn successor_registry() -> RegionRegistry {
    RegionRegistry {
        version: 2,
        regions: vec![RegionEntry {
            region: region(),
            authority_name: "Successor Authority".into(),
            official_domain: "successor.example".into(),
            parent: None,
            keys: vec![AuthorityKey {
                key_id: "k2".into(),
                public_key: successor_key().verifying_key().to_bytes().to_vec(),
                enrolled_at: 0,
                retired_at: None,
            }],
        }],
    }
}

fn successor_key() -> SigningKey {
    SigningKey::from_bytes(&[SUCCESSOR_SEED; 32])
}

/// The successor authority's own artifact, signed by its own key under its own
/// `key_id`.
fn successor_artifact(sequence: u64, counterparties_per_month: u64) -> PolicyArtifact {
    sign_artifact(
        PolicyArtifact {
            region: region(),
            key_id: "k2".into(),
            sequence,
            issued_at: 1_000,
            payload_kind: PAYLOAD_KIND_FEATURE_POLICY.to_string(),
            payload: encode_canonical(&policy_document(counterparties_per_month))
                .unwrap()
                .to_vec(),
            sig: Vec::new(),
        },
        &successor_key(),
    )
    .unwrap()
}

/// A published feature policy bounding `p2p-share` counterparties per month.
fn policy_document(counterparties_per_month: u64) -> RegionFeaturePolicies {
    let mut document = RegionFeaturePolicies::new();
    document.insert(
        GatedFeature::P2pShare.as_str().to_string(),
        FeaturePolicy {
            availability: Availability::Limit,
            counterparties: WindowedBounds::at(Window::Month, counterparties_per_month),
            ..Default::default()
        },
    );
    document
}

fn artifact_for(
    region: RegionCode,
    sequence: u64,
    payload_kind: &str,
    document: &RegionFeaturePolicies,
) -> PolicyArtifact {
    sign_artifact(
        PolicyArtifact {
            region,
            key_id: "k1".into(),
            sequence,
            issued_at: 1_000,
            payload_kind: payload_kind.to_string(),
            payload: encode_canonical(document).unwrap().to_vec(),
            sig: Vec::new(),
        },
        &authority_key(),
    )
    .unwrap()
}

fn artifact(sequence: u64, counterparties_per_month: u64) -> PolicyArtifact {
    artifact_for(
        region(),
        sequence,
        PAYLOAD_KIND_FEATURE_POLICY,
        &policy_document(counterparties_per_month),
    )
}

async fn declare(router: &RpcRouter, state: &Arc<AppState>, admin: [u8; 32], code: Option<&str>) {
    let req = encode_canonical(&AdminRegionSetRequest {
        region: code.map(|c| RegionCode::parse(c).unwrap()),
        extra: Default::default(),
    })
    .unwrap();
    let raw = common::call_raw(
        router,
        state.clone(),
        "fauna.admin.region.set",
        admin,
        req.clone(),
    )
    .await
    .expect("declaration accepted");
    let reply: AdminRegionSetReply = decode(&raw).unwrap();
    assert!(reply.ok);
}

async fn region_status(
    router: &RpcRouter,
    state: &Arc<AppState>,
    admin: [u8; 32],
) -> AdminRegionStatusReply {
    let raw = common::call_raw(
        router,
        state.clone(),
        "fauna.admin.region.get",
        admin,
        encode_canonical(&fauna_protocol::region::AdminRegionStatusRequest::default())
            .unwrap()
            .clone(),
    )
    .await
    .expect("status read");
    decode(&raw).unwrap()
}

async fn features_status(
    router: &RpcRouter,
    state: &Arc<AppState>,
    actor: [u8; 32],
) -> FeaturesStatusReply {
    let raw = common::call_raw(
        router,
        state.clone(),
        "fauna.features.status",
        actor,
        encode_canonical(&FeaturesStatusRequest::default())
            .unwrap()
            .clone(),
    )
    .await
    .expect("status read");
    decode(&raw).unwrap()
}

fn p2p_counterparties_month(reply: &FeaturesStatusReply) -> Option<(u64, RuleTier)> {
    let item = reply
        .features
        .iter()
        .find(|f| f.feature == GatedFeature::P2pShare)?;
    let cell = item
        .policy
        .bounds(QuotaDimension::Counterparties)
        .get(Window::Month)?;
    Some((cell.limit, cell.tier))
}

// ── The happy path ────────────────────────────────────────────────────────

/// The whole point of the slice: an authority's published bound reaches the
/// meet, tightens it, and is **attributed to the region** on the surface that
/// tells a user who restricts them.
#[tokio::test]
async fn an_accepted_artifact_binds_and_is_attributed_to_the_region() {
    let (router, state) = router_with_db().await;
    let admin = seed_admin(&state).await;
    let user = seed_user(&state, "bound").await;

    // Before: the only bound is tier 1's constant, attributed to it.
    let before = features_status(&router, &state, user).await;
    let (structural_limit, tier) = p2p_counterparties_month(&before).expect("tier-1 bound");
    assert_eq!(tier, RuleTier::Structural);
    assert!(before.region.is_none(), "no region claims a fresh nest");

    declare(&router, &state, admin, Some("NO")).await;
    let accepted = ingest_feature_policy(&state, artifact(1, 3), None, &fixture_registry())
        .await
        .unwrap();
    assert_eq!(accepted, Ingested::Accepted { sequence: 1 });

    let after = features_status(&router, &state, user).await;
    let (limit, tier) = p2p_counterparties_month(&after).expect("region bound");
    assert_eq!(limit, 3);
    assert_eq!(tier, RuleTier::Region);
    assert!(
        3 < structural_limit,
        "the fixture must actually tighten, or this test proves nothing"
    );

    // § Transparency & auditability: the active document's identity + version.
    let doc = after.region.expect("the document in force is named");
    assert_eq!(doc.region, region());
    assert_eq!(doc.authority_name, "Fixture Authority");
    assert_eq!(doc.sequence, 1);
    assert_eq!(doc.key_id, "k1");
}

/// A tier can only tighten. A region document *looser* than tier 1 must not
/// raise the ceiling — the meet is what refuses it, and this pins that the
/// region tier is composed through the meet rather than applied on top of it.
#[tokio::test]
async fn a_region_document_cannot_relax_past_tier_one() {
    let (router, state) = router_with_db().await;
    let admin = seed_admin(&state).await;
    let user = seed_user(&state, "unbound").await;
    declare(&router, &state, admin, Some("NO")).await;

    let structural = p2p_counterparties_month(&features_status(&router, &state, user).await)
        .expect("tier-1 bound")
        .0;
    ingest_feature_policy(
        &state,
        artifact(1, structural + 1_000),
        None,
        &fixture_registry(),
    )
    .await
    .unwrap();

    let (limit, tier) = p2p_counterparties_month(&features_status(&router, &state, user).await)
        .expect("bound survives");
    assert_eq!(limit, structural);
    assert_eq!(tier, RuleTier::Structural);
}

// ── Illegitimate documents ────────────────────────────────────────────────

/// **The production state.** Nobody is enrolled, so a perfectly-signed artifact
/// binds nobody and every deployment runs at tier-1 constants — § Fail posture's
/// ratified "a deployment no region claims" arm, reached structurally.
#[tokio::test]
async fn the_compiled_in_registry_binds_nobody() {
    let (router, state) = router_with_db().await;
    let admin = seed_admin(&state).await;
    let user = seed_user(&state, "unclaimed").await;
    declare(&router, &state, admin, Some("NO")).await;

    let outcome = ingest_feature_policy(
        &state,
        artifact(1, 3),
        None,
        &fauna_core::region_authority::compiled_in_registry(),
    )
    .await
    .unwrap();
    assert!(matches!(outcome, Ingested::Refused(_)), "{outcome:?}");

    let (_, tier) = p2p_counterparties_month(&features_status(&router, &state, user).await)
        .expect("tier-1 still binds");
    assert_eq!(tier, RuleTier::Structural);

    // And the admin screen says why: declared, but no authority is enrolled.
    let status = region_status(&router, &state, admin).await;
    assert_eq!(status.declared, Some(region()));
    assert!(!status.enrolled);
    assert!(
        !status.stale,
        "an unenrolled region has no channel to be stale"
    );
}

/// **The `region_refresh` half of the defect this row fixes, pinned at the
/// storage seam.** `region_get_handler`'s `enrolled` gate has no test seam
/// (`registry_snapshot` always reads the real `compiled_in_registry`, empty
/// at version 0 — deliberately, not a placeholder), so an end-to-end pin
/// through the admin RPC can never see `enrolled: true` in this suite; the
/// pure staleness derivation itself (`region_tier::refresh_staleness`) is
/// instead pinned with an injected clock by `region_tier.rs`'s own unit tests,
/// and the storage half — `reached_at` moves only on a reach,
/// `first_attempted_at` is set once — by `db/region_tier.rs`'s
/// `reached_at_moves_only_on_a_reach_and_first_attempted_at_is_stable`. This
/// test pins the remaining seam a conformance level can reach: `checked_at`
/// keeps moving on every attempt while `reached_at` (and so
/// `last_checked_at`) does not, once a real worker pass runs it through
/// `CacheDb::record_region_refresh`.
#[tokio::test]
async fn checked_at_keeps_moving_while_reached_at_does_not() {
    let (router, state) = router_with_db().await;
    let admin = seed_admin(&state).await;
    declare(&router, &state, admin, Some("NO")).await;

    state
        .db
        .record_region_refresh(PAYLOAD_KIND_FEATURE_POLICY, Some("bad signature"), true)
        .await;
    let reached = region_status(&router, &state, admin)
        .await
        .last_checked_at
        .expect("a refusal is a reached channel");

    // Two more failed attempts: `checked_at` (a diagnostic, not surfaced
    // directly) moves each time, but `last_checked_at` — sourced from
    // `reached_at` — must not, because neither attempt reached the channel.
    state
        .db
        .record_region_refresh(PAYLOAD_KIND_FEATURE_POLICY, Some("fetch failed"), false)
        .await;
    state
        .db
        .record_region_refresh(
            PAYLOAD_KIND_FEATURE_POLICY,
            Some("fetch failed again"),
            false,
        )
        .await;

    let status = region_status(&router, &state, admin).await;
    assert_eq!(
        status.last_checked_at,
        Some(reached),
        "a failing worker must not advance `last_checked_at` past the last \
         time the channel actually answered"
    );
    assert_eq!(status.last_error.as_deref(), Some("fetch failed again"));
}

/// An artifact for a region this deployment does not declare is refused even
/// though it is legitimately signed and legitimately published — it is simply
/// not this deployment's.
#[tokio::test]
async fn an_artifact_for_another_region_is_refused() {
    let (router, state) = router_with_db().await;
    let admin = seed_admin(&state).await;
    declare(&router, &state, admin, Some("NO")).await;

    let other = artifact_for(
        RegionCode::parse("SE").unwrap(),
        1,
        PAYLOAD_KIND_FEATURE_POLICY,
        &policy_document(3),
    );
    let outcome = ingest_feature_policy(&state, other, None, &fixture_registry())
        .await
        .unwrap();
    assert!(matches!(outcome, Ingested::Refused(_)), "{outcome:?}");
}

/// A nest that declares nothing ingests nothing. This is the arm that keeps a
/// deployment which never opted into a region from being bound by one.
#[tokio::test]
async fn an_undeclared_nest_ingests_nothing() {
    let (_router, state) = router_with_db().await;
    let outcome = ingest_feature_policy(&state, artifact(1, 3), None, &fixture_registry())
        .await
        .unwrap();
    assert!(matches!(outcome, Ingested::Refused(_)), "{outcome:?}");
}

/// The content plane's document must not be read as this plane's, even from the
/// right authority for the right region.
#[tokio::test]
async fn a_content_policy_artifact_is_not_ingested_as_a_feature_policy() {
    let (router, state) = router_with_db().await;
    let admin = seed_admin(&state).await;
    declare(&router, &state, admin, Some("NO")).await;

    let content = artifact_for(
        region(),
        1,
        PAYLOAD_KIND_CONTENT_POLICY,
        &policy_document(3),
    );
    let outcome = ingest_feature_policy(&state, content, None, &fixture_registry())
        .await
        .unwrap();
    assert!(matches!(outcome, Ingested::Refused(_)), "{outcome:?}");
}

/// **Last-known-good is un-rollbackable.** Replaying an older, looser artifact
/// must not relax a live bound — otherwise anyone holding a copy of yesterday's
/// public policy could undo today's.
#[tokio::test]
async fn a_replayed_older_artifact_does_not_relax_a_live_bound() {
    let (router, state) = router_with_db().await;
    let admin = seed_admin(&state).await;
    let user = seed_user(&state, "bound").await;
    declare(&router, &state, admin, Some("NO")).await;

    ingest_feature_policy(&state, artifact(5, 3), None, &fixture_registry())
        .await
        .unwrap();
    let outcome = ingest_feature_policy(&state, artifact(4, 500), None, &fixture_registry())
        .await
        .unwrap();
    assert!(matches!(outcome, Ingested::Refused(_)), "{outcome:?}");

    let (limit, tier) = p2p_counterparties_month(&features_status(&router, &state, user).await)
        .expect("the tighter bound survives");
    assert_eq!(limit, 3);
    assert_eq!(tier, RuleTier::Region);
}

/// **the floor goes with the row.** The replay defence's
/// floor must survive a change of situs. Before this pinned it, the floor was a
/// projection of the stored artifact row (`accepted_region_sequence` was
/// `get_region_artifact(..).map(|a| a.sequence)`) and the retirement path
/// hard-deletes that row — so withdraw-then-re-declare the *same* region reset
/// the floor to `None`, and `verify_artifact` skips the monotonicity check
/// entirely on `None`. An older, still-validly-signed artifact was accepted
/// again: exactly the silent relaxation `region-blocking.md` § Fail posture
/// forbids. Signatures cannot help here — an old artifact stays validly signed
/// forever, which is the whole reason the floor exists.
#[tokio::test]
async fn a_situs_round_trip_does_not_reset_the_replay_floor() {
    let (router, state) = router_with_db().await;
    let admin = seed_admin(&state).await;
    let user = seed_user(&state, "bound").await;
    declare(&router, &state, admin, Some("NO")).await;
    ingest_feature_policy(&state, artifact(5, 3), None, &fixture_registry())
        .await
        .unwrap();

    // The round trip: withdraw the declaration, then re-declare the SAME
    // region. No second region is needed to reach the reset.
    declare(&router, &state, admin, None).await;
    declare(&router, &state, admin, Some("NO")).await;

    let outcome = ingest_feature_policy(&state, artifact(4, 500), None, &fixture_registry())
        .await
        .unwrap();
    assert!(
        matches!(outcome, Ingested::Refused(_)),
        "an artifact at a sequence already accepted must stay refused across a \
         situs round trip — the floor is the replay defence, and an admin \
         action must not reset it: {outcome:?}"
    );
    // The retirement dropped the region tier, so tier-1's structural constants
    // bind again (the same end state `withdrawing_the_declaration_retires_the_
    // region_tier` pins). What must NOT appear is a `Region` bound: that would
    // mean the replayed document folded back in.
    let (_, tier) = p2p_counterparties_month(&features_status(&router, &state, user).await)
        .expect("tier-1 binds again after the retirement");
    assert_eq!(
        tier,
        RuleTier::Structural,
        "the replayed document must not bind — a refused artifact folds nothing in"
    );
}

/// The floor must not wedge the plane shut: after the same round trip, the
/// authority's *next* artifact is accepted and binds normally. A floor that
/// survives retirement is only correct if it still admits the sequences it is
/// supposed to.
#[tokio::test]
async fn a_situs_round_trip_still_accepts_a_newer_artifact() {
    let (router, state) = router_with_db().await;
    let admin = seed_admin(&state).await;
    let user = seed_user(&state, "bound").await;
    declare(&router, &state, admin, Some("NO")).await;
    ingest_feature_policy(&state, artifact(5, 3), None, &fixture_registry())
        .await
        .unwrap();

    declare(&router, &state, admin, None).await;
    declare(&router, &state, admin, Some("NO")).await;

    let outcome = ingest_feature_policy(&state, artifact(6, 7), None, &fixture_registry())
        .await
        .unwrap();
    assert!(
        matches!(outcome, Ingested::Accepted { .. }),
        "the surviving floor must still admit a genuinely newer artifact: {outcome:?}"
    );
    let (limit, tier) = p2p_counterparties_month(&features_status(&router, &state, user).await)
        .expect("the new document binds");
    assert_eq!(limit, 7);
    assert_eq!(tier, RuleTier::Region);
}

/// **The floor belongs to the authority, not to the region code.** A curated
/// registry revision that hands a region to a *different* authority starts a
/// fresh sequence space — the successor's counter is its own, and its first
/// artifact is legitimately at a low sequence. Keying the floor on the
/// authority is what keeps a surviving floor from becoming a permanent wedge
/// that no one can clear (there is no operator surface to clear it, by product
/// invariant), while still refusing a replay from the authority that issued it.
#[tokio::test]
async fn a_newly_enrolled_authority_starts_its_own_sequence_space() {
    let (router, state) = router_with_db().await;
    let admin = seed_admin(&state).await;
    let user = seed_user(&state, "bound").await;
    declare(&router, &state, admin, Some("NO")).await;
    ingest_feature_policy(&state, artifact(5, 3), None, &fixture_registry())
        .await
        .unwrap();

    // Fauna re-curates: the region's entry now names a different authority
    // with its own key. Its first document is sequence 1.
    let outcome = ingest_feature_policy(
        &state,
        successor_artifact(1, 9),
        None,
        &successor_registry(),
    )
    .await
    .unwrap();
    assert!(
        matches!(outcome, Ingested::Accepted { .. }),
        "a different authority's first artifact must not be refused against the \
         previous authority's floor — the sequence spaces are unrelated: {outcome:?}"
    );
    let (limit, tier) = p2p_counterparties_month(&features_status(&router, &state, user).await)
        .expect("the successor's document binds");
    assert_eq!(limit, 9);
    assert_eq!(tier, RuleTier::Region);
}

/// a floor left behind a stored artifact's sequence is a state
/// `put_region_artifact`'s own atomic upsert can no longer produce, but it is
/// exactly the state a hand-restored `nest.db` could still leave on disk. The boot seed
/// is the ONLY repair for that: it re-runs unconditionally every boot
/// (`db/migrations.rs`'s `MIGRATIONS_REGION_TIER`, not behind a version gate),
/// so it must actually raise a lagging floor rather than decline to touch it.
/// An `INSERT OR IGNORE` seed keeps whatever the floor already says on a key
/// conflict — the repair declining to run in the only case it was built for —
/// so this test writes exactly that lagging state directly (bypassing the
/// router, which can never construct it) and asserts the next `CacheDb::open`
/// raises the floor to match the stored artifact.
#[tokio::test]
async fn a_boot_replaying_migrations_raises_a_lagging_replay_floor() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("nest.db");

    {
        let db = CacheDb::open(&db_path).unwrap();
        let conn = db.conn().await;
        conn.execute(
            "INSERT INTO region_artifacts
                 (region, payload_kind, sequence, issued_at, key_id, authority_name,
                  envelope, accepted_at)
             VALUES ('NO', ?1, 500, 1, 'k1', 'authority-a', X'00', 1)",
            rusqlite::params![PAYLOAD_KIND_FEATURE_POLICY],
        )
        .unwrap();
        // Below the artifact row's sequence — a state the atomic writer above
        // can no longer produce, standing in for a hand-restored `nest.db`.
        conn.execute(
            "INSERT INTO region_sequence_floor
                 (region, payload_kind, authority_name, sequence, updated_at)
             VALUES ('NO', ?1, 'authority-a', 4, 1)",
            rusqlite::params![PAYLOAD_KIND_FEATURE_POLICY],
        )
        .unwrap();
        // `db` drops here, closing the connection — the next `CacheDb::open`
        // below is a genuinely fresh boot over the same file, not a second
        // handle onto the same open database.
    }

    let db = CacheDb::open(&db_path).unwrap();
    let conn = db.conn().await;
    let floor: i64 = conn
        .query_row(
            "SELECT sequence FROM region_sequence_floor
             WHERE region = 'NO' AND payload_kind = ?1 AND authority_name = 'authority-a'",
            rusqlite::params![PAYLOAD_KIND_FEATURE_POLICY],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        floor, 500,
        "the boot seed must raise a lagging floor to match the stored artifact's \
         sequence — an `INSERT OR IGNORE` seed leaves it at 4, disarming the \
         replay defence for any validly-signed artifact in (4, 500]"
    );
}

// ── Documents that must stop binding ──────────────────────────────────────

/// A newer artifact replaces the whole document, so a feature the authority
/// **stopped naming** stops being bound by the region — *"no opinion at this
/// tier"*, not a bound left quietly in force by a document that no longer says
/// it.
#[tokio::test]
async fn a_feature_dropped_from_a_newer_document_stops_being_region_bound() {
    let (router, state) = router_with_db().await;
    let admin = seed_admin(&state).await;
    let user = seed_user(&state, "bound").await;
    declare(&router, &state, admin, Some("NO")).await;

    ingest_feature_policy(&state, artifact(1, 3), None, &fixture_registry())
        .await
        .unwrap();
    let empty = artifact_for(
        region(),
        2,
        PAYLOAD_KIND_FEATURE_POLICY,
        &RegionFeaturePolicies::new(),
    );
    ingest_feature_policy(&state, empty, None, &fixture_registry())
        .await
        .unwrap();

    let (_, tier) = p2p_counterparties_month(&features_status(&router, &state, user).await)
        .expect("tier-1 binds again");
    assert_eq!(tier, RuleTier::Structural);
}

/// Withdrawing the declaration retires the tier. A deployment that moves its
/// legal situs must not stay bound by the authority that no longer claims it.
#[tokio::test]
async fn withdrawing_the_declaration_retires_the_region_tier() {
    let (router, state) = router_with_db().await;
    let admin = seed_admin(&state).await;
    let user = seed_user(&state, "bound").await;
    declare(&router, &state, admin, Some("NO")).await;
    ingest_feature_policy(&state, artifact(1, 3), None, &fixture_registry())
        .await
        .unwrap();

    declare(&router, &state, admin, None).await;

    let after = features_status(&router, &state, user).await;
    let (_, tier) = p2p_counterparties_month(&after).expect("tier-1 binds again");
    assert_eq!(tier, RuleTier::Structural);
    assert!(after.region.is_none());
    assert!(
        region_status(&router, &state, admin)
            .await
            .declared
            .is_none()
    );
}

/// Re-declaring a *different* region does the same: one authority's statement
/// about its own region must not survive into another's.
#[tokio::test]
async fn re_declaring_a_different_region_retires_the_old_document() {
    let (router, state) = router_with_db().await;
    let admin = seed_admin(&state).await;
    let user = seed_user(&state, "bound").await;
    declare(&router, &state, admin, Some("NO")).await;
    ingest_feature_policy(&state, artifact(1, 3), None, &fixture_registry())
        .await
        .unwrap();

    declare(&router, &state, admin, Some("SE")).await;

    let (_, tier) = p2p_counterparties_month(&features_status(&router, &state, user).await)
        .expect("tier-1 binds again");
    assert_eq!(tier, RuleTier::Structural);
}

/// **De-listing an authority actually de-lists it.** A registry revision that
/// removes a key is Fauna saying this party is no longer legitimate; the
/// re-fold is what makes that retire the document it published, rather than
/// leaving its restrictions in force forever.
#[tokio::test]
async fn a_de_listed_authoritys_document_is_retired_on_refold() {
    let (router, state) = router_with_db().await;
    let admin = seed_admin(&state).await;
    let user = seed_user(&state, "bound").await;
    declare(&router, &state, admin, Some("NO")).await;
    ingest_feature_policy(&state, artifact(1, 3), None, &fixture_registry())
        .await
        .unwrap();

    // A later registry revision no longer enrols anyone.
    refold_stored_artifact(&state, &RegionRegistry::default())
        .await
        .unwrap();

    let after = features_status(&router, &state, user).await;
    let (_, tier) = p2p_counterparties_month(&after).expect("tier-1 binds again");
    assert_eq!(tier, RuleTier::Structural);
    assert!(after.region.is_none());
}

/// The re-fold is otherwise a no-op — it must not refuse the stored artifact as
/// a replay of itself, which is the shape that would silently retire every live
/// region document at every boot.
#[tokio::test]
async fn a_refold_against_the_same_registry_keeps_the_document_binding() {
    let (router, state) = router_with_db().await;
    let admin = seed_admin(&state).await;
    let user = seed_user(&state, "bound").await;
    declare(&router, &state, admin, Some("NO")).await;
    ingest_feature_policy(&state, artifact(1, 3), None, &fixture_registry())
        .await
        .unwrap();

    refold_stored_artifact(&state, &fixture_registry())
        .await
        .unwrap();

    let (limit, tier) = p2p_counterparties_month(&features_status(&router, &state, user).await)
        .expect("the bound survives a re-fold");
    assert_eq!(limit, 3);
    assert_eq!(tier, RuleTier::Region);
}

/// **An unreadable *payload* is not an authority withdrawing** (§ Fail posture —
/// the undecodable-document clause).
///
/// The re-fold used to `clear_tier` when a stored artifact still verified but
/// its payload no longer decoded — *actively removing* every Region-tier row,
/// which is a sharper inversion of last-known-good than the read-side skip was:
/// the bounds that were in force and perfectly readable are deleted because a
/// re-derivation failed. § Fail posture says a fetched policy *"stays in force
/// until replaced — never silently relaxes"*, and a failed re-derivation
/// replaces nothing.
///
/// Contrast the arm deliberately left alone: an artifact that no longer
/// **verifies** is a de-list, and de-listing must retire the document
/// (`a_de_listed_authoritys_document_is_retired_on_refold`). The two failures
/// look alike one line apart and mean opposite things.
#[tokio::test]
async fn a_stored_artifact_whose_payload_stops_decoding_keeps_the_bounds_it_last_folded() {
    let (router, state) = router_with_db().await;
    let admin = seed_admin(&state).await;
    let user = seed_user(&state, "bound").await;
    declare(&router, &state, admin, Some("NO")).await;
    ingest_feature_policy(&state, artifact(1, 3), None, &fixture_registry())
        .await
        .unwrap();

    // The stored envelope becomes one this build can verify but no longer read —
    // a decoder tightening across an upgrade, or a corrupted row. Written
    // through the store because the ingress refuses such an artifact at the
    // door, which is exactly why it can only ever arrive as an *already stored*
    // one.
    let mut undecodable = artifact(2, 3);
    undecodable.payload = encode_canonical(&"not a policy document".to_string())
        .unwrap()
        .to_vec();
    let undecodable = fauna_core::region_authority::sign_artifact(undecodable, &authority_key())
        .expect("re-sign after replacing the payload");
    let verified = fauna_core::region_authority::verify_artifact(
        undecodable,
        &fixture_registry(),
        1_000,
        None,
    )
    .expect("it still verifies — that is the whole point");
    state.db.put_region_artifact(&verified).await.unwrap();

    refold_stored_artifact(&state, &fixture_registry())
        .await
        .unwrap();

    let (limit, tier) = p2p_counterparties_month(&features_status(&router, &state, user).await)
        .expect("the last-known-good bound must still bind");
    assert_eq!(limit, 3);
    assert_eq!(tier, RuleTier::Region);
}

// ── An unreadable declaration (corruption, not absence) ──────────────────
//
// `dynamic-features.md` § Fail posture, the unreadable-declaration clause: an unreadable `nest_region.region` stays fully-open
// for *enforcement* (the situs is not a restriction), and the Region-tier rows
// the last fold wrote keep binding (last-known-good) — but the reads must not
// mistake the corruption for absence: a document that still binds must still be
// named, and the write seam must still retire state it can no longer key on.

/// Corrupt the declaration column underneath the running nest — the state a
/// torn write or page corruption leaves, unreachable through any API.
async fn corrupt_declaration(state: &Arc<AppState>) {
    let conn = state.db.conn().await;
    let n = conn
        .execute("UPDATE nest_region SET region = 'not a region'", [])
        .unwrap();
    assert_eq!(n, 1, "the corruption must land on a declared row");
}

/// **The transparency invariant under corruption** (§ Transparency &
/// auditability: *"there is no restriction you cannot see"*): the bounds keep
/// binding — so the reads must keep naming the document that binds them,
/// recovering its identity from the artifact store, and the admin read must say
/// the declaration is corrupt rather than "never declared".
#[tokio::test]
async fn a_corrupted_declaration_does_not_hide_the_binding_document() {
    let (router, state) = router_with_db().await;
    let admin = seed_admin(&state).await;
    let user = seed_user(&state, "bound").await;
    declare(&router, &state, admin, Some("NO")).await;
    ingest_feature_policy(&state, artifact(1, 3), None, &fixture_registry())
        .await
        .unwrap();

    corrupt_declaration(&state).await;

    // The meet is unchanged: the folded bounds keep binding and keep their
    // attribution — this half held before the fix and must keep holding.
    let status = features_status(&router, &state, user).await;
    let (limit, tier) = p2p_counterparties_month(&status).expect("the bound still binds");
    assert_eq!(limit, 3);
    assert_eq!(tier, RuleTier::Region);

    // The transparency read still names the document those bounds come from.
    let doc = status
        .region
        .expect("a binding document must stay visible — identity included");
    assert_eq!(doc.region, region());
    assert_eq!(doc.authority_name, "Fixture Authority");
    assert_eq!(doc.sequence, 1);

    // The admin read reports the truth: not "never declared" — the row is
    // there and unreadable, and the document it admitted still binds.
    let admin_read = region_status(&router, &state, admin).await;
    assert_eq!(admin_read.declared, None);
    assert!(admin_read.declaration_unreadable);
    let doc = admin_read
        .feature_policy
        .expect("the admin read names the binding document too");
    assert_eq!(doc.sequence, 1);
}

/// A corrupted declaration with **no** document ever accepted: the reads
/// report the corruption and nothing else — no identity is invented.
#[tokio::test]
async fn a_corrupted_declaration_with_no_document_reports_only_the_corruption() {
    let (router, state) = router_with_db().await;
    let admin = seed_admin(&state).await;
    let user = seed_user(&state, "unbound").await;
    declare(&router, &state, admin, Some("NO")).await;

    corrupt_declaration(&state).await;

    let status = features_status(&router, &state, user).await;
    assert!(status.region.is_none(), "no document exists to name");
    let admin_read = region_status(&router, &state, admin).await;
    assert_eq!(admin_read.declared, None);
    assert!(admin_read.declaration_unreadable);
    assert!(admin_read.feature_policy.is_none());
}

/// **Re-declaring over a corrupted declaration is a change of situs and must
/// retire the old region's document** — the write seam cannot read which region
/// is outgoing, so its retirement must not depend on being able to.
#[tokio::test]
async fn re_declaring_over_a_corrupted_declaration_retires_the_old_document() {
    let (router, state) = router_with_db().await;
    let admin = seed_admin(&state).await;
    let user = seed_user(&state, "bound").await;
    declare(&router, &state, admin, Some("NO")).await;
    ingest_feature_policy(&state, artifact(1, 3), None, &fixture_registry())
        .await
        .unwrap();

    corrupt_declaration(&state).await;
    declare(&router, &state, admin, Some("SE")).await;

    let after = features_status(&router, &state, user).await;
    let (_, tier) = p2p_counterparties_month(&after).expect("tier-1 binds again");
    assert_eq!(
        tier,
        RuleTier::Structural,
        "the old region's bounds must not survive a change of situs"
    );
    assert!(after.region.is_none());
    assert!(
        state
            .db
            .get_region_artifact(&region(), PAYLOAD_KIND_FEATURE_POLICY)
            .await
            .unwrap()
            .is_none(),
        "the old region's artifact must be retired"
    );
}

/// Withdrawing over a corrupted declaration retires everything, exactly as an
/// ordinary withdrawal does.
#[tokio::test]
async fn withdrawing_over_a_corrupted_declaration_retires_everything() {
    let (router, state) = router_with_db().await;
    let admin = seed_admin(&state).await;
    let user = seed_user(&state, "bound").await;
    declare(&router, &state, admin, Some("NO")).await;
    ingest_feature_policy(&state, artifact(1, 3), None, &fixture_registry())
        .await
        .unwrap();

    corrupt_declaration(&state).await;
    declare(&router, &state, admin, None).await;

    let after = features_status(&router, &state, user).await;
    let (_, tier) = p2p_counterparties_month(&after).expect("tier-1 binds again");
    assert_eq!(tier, RuleTier::Structural);
    assert!(after.region.is_none());
    assert!(
        state
            .db
            .get_region_artifact(&region(), PAYLOAD_KIND_FEATURE_POLICY)
            .await
            .unwrap()
            .is_none()
    );
    let admin_read = region_status(&router, &state, admin).await;
    assert_eq!(admin_read.declared, None);
    assert!(
        !admin_read.declaration_unreadable,
        "the corrupt row is gone"
    );
}

/// **Re-declaring the *same* region over a corrupted declaration is the
/// recovery path, and it must not cost the document its bounds**: the kept
/// region's artifact survives the seam's retirement, so the tier is left
/// exactly as its fold wrote it — no gap until some later refresh.
#[tokio::test]
async fn re_declaring_the_same_region_over_a_corrupted_declaration_keeps_the_document() {
    let (router, state) = router_with_db().await;
    let admin = seed_admin(&state).await;
    let user = seed_user(&state, "bound").await;
    declare(&router, &state, admin, Some("NO")).await;
    ingest_feature_policy(&state, artifact(1, 3), None, &fixture_registry())
        .await
        .unwrap();

    corrupt_declaration(&state).await;
    declare(&router, &state, admin, Some("NO")).await;

    let after = features_status(&router, &state, user).await;
    let (limit, tier) = p2p_counterparties_month(&after).expect("the bound survives recovery");
    assert_eq!(limit, 3);
    assert_eq!(tier, RuleTier::Region);
    let doc = after.region.expect("the document is named again");
    assert_eq!(doc.sequence, 1);
    let admin_read = region_status(&router, &state, admin).await;
    assert_eq!(admin_read.declared, Some(region()));
    assert!(!admin_read.declaration_unreadable);
}

// ── Who may declare ───────────────────────────────────────────────────────

/// The situs is the admin's to declare and nobody else's. An ordinary user
/// reaching this kind could bind **every** account on the nest to a region's
/// policy — or unbind them from one.
#[tokio::test]
async fn an_ordinary_user_cannot_declare_the_region() {
    let (router, state) = router_with_db().await;
    let user = seed_user(&state, "ordinary").await;

    let req = encode_canonical(&AdminRegionSetRequest {
        region: Some(region()),
        extra: Default::default(),
    })
    .unwrap();
    let err = common::call_raw(
        &router,
        state.clone(),
        "fauna.admin.region.set",
        user,
        req.clone(),
    )
    .await
    .expect_err("an ordinary user must be refused");
    assert!(
        err.code.contains("permission") || err.code.contains("denied"),
        "expected a permission refusal, got {:?}",
        err.code
    );
    assert_eq!(state.db.get_declared_region().await.unwrap(), None);

    // …and cannot read the admin surface either.
    common::call_raw(
        &router,
        state.clone(),
        "fauna.admin.region.get",
        user,
        encode_canonical(&fauna_protocol::region::AdminRegionStatusRequest::default())
            .unwrap()
            .clone(),
    )
    .await
    .expect_err("the admin read is admin-only");
}

/// The user's own window onto the region tier is the transparency read, which is
/// User-class — boundary 4's *"every active restriction is visible to the person
/// it binds"* must not be gated behind an admin surface.
#[tokio::test]
async fn an_ordinary_user_can_see_the_region_document_that_binds_them() {
    let (router, state) = router_with_db().await;
    let admin = seed_admin(&state).await;
    let user = seed_user(&state, "bound").await;
    declare(&router, &state, admin, Some("NO")).await;
    ingest_feature_policy(&state, artifact(1, 3), None, &fixture_registry())
        .await
        .unwrap();

    let doc = features_status(&router, &state, user)
        .await
        .region
        .expect("the bound user can see what binds them");
    assert_eq!(doc.authority_name, "Fixture Authority");
    assert_eq!(doc.sequence, 1);
}

/// § The transparency log, the pre-log rule stated in code: an artifact the
/// log serves **without** inclusion evidence is admitted only while this nest's
/// anchor is the compiled-in empty one. Once a head has been accepted — here
/// planted straight into the store, since no witnessed log exists to fetch
/// from — the same evidence-less serve is refused, and the document that was
/// binding keeps binding (§ Fail posture: a refusal writes nothing).
#[tokio::test]
async fn an_evidence_less_artifact_is_admitted_only_in_the_pre_log_era() {
    use fauna_core::region_authority::{AnchorState, ObjectId};

    let (router, state) = router_with_db().await;
    let admin = seed_admin(&state).await;
    let user = seed_user(&state, "bound").await;
    declare(&router, &state, admin, Some("NO")).await;

    let anchor = state.db.region_log_anchor().await.unwrap();
    assert!(
        anchor.is_pre_log(),
        "a fresh nest on today's build is pre-log"
    );
    assert_eq!(
        ingest_feature_policy(&state, artifact(1, 3), None, &fixture_registry())
            .await
            .unwrap(),
        Ingested::Accepted { sequence: 1 }
    );

    // The nest has now accepted a witnessed head (planted: the log is unpublished).
    let past = AnchorState::with_last_accepted(Some(ObjectId::of_commit(b"tree x\n\n")));
    state.db.put_region_log_anchor(&past).await.unwrap();
    assert_eq!(state.db.region_log_anchor().await.unwrap(), past);

    let outcome = ingest_feature_policy(&state, artifact(2, 2), None, &fixture_registry())
        .await
        .unwrap();
    match outcome {
        Ingested::Refused(why) => assert!(why.contains("no inclusion evidence"), "{why}"),
        other => panic!("evidence-less artifact admitted past the pre-log era: {other:?}"),
    }
    let (limit, _) = p2p_counterparties_month(&features_status(&router, &state, user).await)
        .expect("the sequence-1 document still binds");
    assert_eq!(
        limit, 3,
        "a refusal leaves the last accepted document in force"
    );
}

/// Evidence the log *does* serve is checked, and today — the witness roster
/// empty at version 0 — it is refused rather than waved through: the quorum is
/// the go-live bar, and an artifact carrying evidence that fails is refused
/// even in the pre-log era.
#[tokio::test]
async fn an_artifact_whose_served_evidence_fails_is_refused() {
    use fauna_core::region_authority::{Checkpoint, InclusionEvidence, ObjectId, REGION_LOG_ID};

    let (router, state) = router_with_db().await;
    let admin = seed_admin(&state).await;
    declare(&router, &state, admin, Some("NO")).await;

    let commit =
        b"tree 6ef19b41225c5369f1c104d45d8d85efa9b057b53b14b4b9b939dd74decc5321\n\nx".to_vec();
    let head = ObjectId::of_commit(&commit);
    let evidence = InclusionEvidence {
        head,
        chain: vec![serde_bytes::ByteBuf::from(commit)],
        trees: vec![],
        checkpoint: Checkpoint {
            log_id: REGION_LOG_ID.into(),
            head,
            extra: Default::default(),
        },
        cosignatures: vec![],
        extra: Default::default(),
    };
    let outcome =
        ingest_feature_policy(&state, artifact(1, 3), Some(evidence), &fixture_registry())
            .await
            .unwrap();
    assert!(matches!(outcome, Ingested::Refused(_)), "{outcome:?}");
    assert!(
        state.db.region_log_anchor().await.unwrap().is_pre_log(),
        "a refusal does not move the anchor"
    );
    assert!(
        state
            .db
            .get_region_artifact(&region(), PAYLOAD_KIND_FEATURE_POLICY)
            .await
            .unwrap()
            .is_none(),
        "a refusal stores nothing"
    );
}

// ── The app relay: `fauna.region.artifact.get` ──────────────────────────────
//
// `region-blocking.md` § The content plane → *How an app obtains its region's
// policy*. The network fetch reads the compiled-in log URL, which these tests
// cannot and must not reach — so, exactly as the feature-plane tests above do,
// the artifact enters through the ingest seam with the fixture registry, and
// what is under test is everything the relay does with it.

/// A content-policy artifact from the fixture authority, with a payload that is
/// **not a policy document at all**. The relay must pass it through untouched:
/// it is a relay, not a trust point, and an app newer than its nest may read a
/// document version the nest's build cannot. A relay that decoded the payload
/// would refuse this one — which is exactly what this fixture exists to catch.
fn opaque_content_policy(sequence: u64) -> PolicyArtifact {
    sign_artifact(
        PolicyArtifact {
            region: region(),
            key_id: "k1".into(),
            sequence,
            issued_at: 1_000,
            payload_kind: PAYLOAD_KIND_CONTENT_POLICY.to_string(),
            payload: format!("a document this nest cannot read, v{sequence}").into_bytes(),
            sig: Vec::new(),
        },
        &authority_key(),
    )
    .unwrap()
}

async fn relay_get(
    router: &RpcRouter,
    state: &Arc<AppState>,
    user: [u8; 32],
    region_code: &str,
    payload_kind: &str,
) -> RegionArtifactGetReply {
    let req = encode_canonical(&RegionArtifactGetRequest {
        region: RegionCode::parse(region_code).unwrap(),
        payload_kind: payload_kind.to_string(),
        extra: Default::default(),
    })
    .unwrap();
    let raw = common::call_raw(
        router,
        state.clone(),
        "fauna.region.artifact.get",
        user,
        req,
    )
    .await
    .expect("an app may read its region's artifact through its own nest");
    decode(&raw).unwrap()
}

/// **An app reads its region's published content policy through its own nest,
/// byte for byte.** The first ask is "no document" — the fresh-subject arm of
/// § Fail posture, not an error; once the artifact is in the cache the same
/// ask returns it, exactly as the authority signed it, opaque payload
/// included.
///
/// `router_with_enrolled_registry` installs `fixture_registry()` as the
/// demand door's override, so the first ask itself — through the real
/// `fauna.region.artifact.get` handler, not a seed past it — is what records
/// the demand: an ask reaching an ENROLLED registry, the same seam
/// `refresh_relay_once` below verifies against.
#[tokio::test]
async fn an_app_reads_its_regions_published_content_policy_through_the_relay() {
    let (router, state) = router_with_enrolled_registry().await;
    let user = seed_user(&state, "reader").await;

    let first = relay_get(&router, &state, user, "NO", PAYLOAD_KIND_CONTENT_POLICY).await;
    assert_eq!(first.envelope, None, "nothing fetched yet: no document");
    assert!(!first.stale, "a pair asked for a moment ago is not stale");
    assert!(
        state
            .db
            .relay_artifact(&region(), PAYLOAD_KIND_CONTENT_POLICY)
            .await
            .unwrap()
            .is_some(),
        "the door's own first ask against an enrolled region is the demand — \
         the worker now has a reason to fetch it"
    );

    let artifact = opaque_content_policy(7);
    let outcome = ingest_relay_artifact(&state, artifact.clone(), None, &fixture_registry())
        .await
        .unwrap();
    assert_eq!(outcome, Ingested::Accepted { sequence: 7 });

    let second = relay_get(&router, &state, user, "NO", PAYLOAD_KIND_CONTENT_POLICY).await;
    assert_eq!(
        second.envelope,
        Some(artifact),
        "the envelope the authority signed, returned unchanged — opaque payload \
         and signature intact, so the app verifies it itself"
    );
    assert_eq!(second.evidence, None, "the pre-log era serves no evidence");
    assert!(
        second.last_checked_at.is_some(),
        "an acceptance is a reached log"
    );
    assert!(!second.stale);
}

/// **A region nobody enrols answers "no document" and records no demand at
/// all** . The relay's generalisation of rule 2: the door itself
/// checks enrolment before writing anything, so a region with no enrolled
/// authority grows no row in `region_relay_cache` — not even one the worker
/// would later discover unenrolled — and the pair can never read as stale
/// either, a channel that does not exist cannot go quiet.
#[tokio::test]
async fn an_unenrolled_region_answers_no_document_and_records_no_demand() {
    let (router, state) = router_with_db().await;
    let user = seed_user(&state, "abroad").await;
    let sweden = RegionCode::parse("SE").unwrap();

    let reply = relay_get(&router, &state, user, "SE", PAYLOAD_KIND_CONTENT_POLICY).await;
    assert_eq!(reply.envelope, None);
    assert!(
        state
            .db
            .relay_artifact(&sweden, PAYLOAD_KIND_CONTENT_POLICY)
            .await
            .unwrap()
            .is_none(),
        "an unenrolled region's ask must record no demand row — the door checks \
         enrolment before writing, not just the worker"
    );

    // The worker's pass changes nothing either, since there is no row to see —
    // with a registry that enrols NO only.
    refresh_relay_once(&state, &fixture_registry()).await;
    assert!(
        state
            .db
            .relay_artifact(&sweden, PAYLOAD_KIND_CONTENT_POLICY)
            .await
            .unwrap()
            .is_none(),
        "the tick does not manufacture a row for a region it has never enrolled"
    );

    let again = relay_get(&router, &state, user, "SE", PAYLOAD_KIND_CONTENT_POLICY).await;
    assert_eq!(again.envelope, None);
    assert!(!again.stale, "never attempted, so never stale");
}

/// **A refused artifact leaves the relayed one in place** — rule 3, and the
/// anti-replay floor reused rather than duplicated: the same
/// `region_sequence_floor` the feature plane raises, keyed (region, kind,
/// authority), refuses an older validly-signed artifact, and the cache keeps
/// answering with the newer one.
#[tokio::test]
async fn a_refused_older_artifact_leaves_the_relayed_one_in_place() {
    let (router, state) = router_with_db().await;
    let user = seed_user(&state, "reader").await;

    let newer = opaque_content_policy(5);
    assert_eq!(
        ingest_relay_artifact(&state, newer.clone(), None, &fixture_registry())
            .await
            .unwrap(),
        Ingested::Accepted { sequence: 5 }
    );
    let older = ingest_relay_artifact(&state, opaque_content_policy(3), None, &fixture_registry())
        .await
        .unwrap();
    assert!(
        matches!(older, Ingested::Refused(_)),
        "an older validly-signed artifact is a replay: {older:?}"
    );

    let reply = relay_get(&router, &state, user, "NO", PAYLOAD_KIND_CONTENT_POLICY).await;
    assert_eq!(
        reply.envelope,
        Some(newer),
        "the refusal wrote nothing — the newer artifact still answers"
    );
}

/// **De-listing an authority retires what the relay holds for it** — the third
/// ruling the region tier binds this plane to. The next pass re-verifies the
/// cached envelope against the current registry; one whose authority is gone no
/// longer verifies and stops being relayed. The demand row stays, so the pair
/// is fetched again if the region is ever re-enrolled.
#[tokio::test]
async fn de_listing_the_authority_retires_what_the_relay_holds() {
    let (router, state) = router_with_db().await;
    let user = seed_user(&state, "reader").await;
    ingest_relay_artifact(&state, opaque_content_policy(2), None, &fixture_registry())
        .await
        .unwrap();

    let de_listed = RegionRegistry {
        version: 2,
        regions: Vec::new(),
    };
    refresh_relay_once(&state, &de_listed).await;

    let reply = relay_get(&router, &state, user, "NO", PAYLOAD_KIND_CONTENT_POLICY).await;
    assert_eq!(
        reply.envelope, None,
        "an envelope whose authority left the registry is no longer relayed"
    );
    assert!(
        state
            .db
            .relay_artifact(&region(), PAYLOAD_KIND_CONTENT_POLICY)
            .await
            .unwrap()
            .is_some(),
        "the demand survives, so a re-enrolled region is fetched again"
    );
}

/// **Nothing submits to the relay, and no admin act changes what it answers.**
/// The kind set around it is exactly the three kinds the owner doc names — the
/// admin's situs read and declaration, and the app's read — with no sibling
/// that submits, edits or overrides an artifact (region-blocking.md invariant
/// 5). And the one admin act there is, declaring a situs, does not touch the
/// relay: the app's answer is the same before and after.
#[tokio::test]
async fn nothing_submits_to_the_relay_and_no_admin_act_changes_its_answer() {
    let region_kinds: Vec<String> = fauna_protocol::kind::KindRegistry::full()
        .iter()
        .map(|(kind, _)| kind.to_string())
        .filter(|kind| kind.starts_with("fauna.region.") || kind.starts_with("fauna.admin.region"))
        .collect();
    let mut region_kinds = region_kinds;
    region_kinds.sort();
    assert_eq!(
        region_kinds,
        vec![
            "fauna.admin.region.get".to_string(),
            "fauna.admin.region.set".to_string(),
            "fauna.region.artifact.get".to_string(),
        ],
        "no kind submits an artifact and no admin kind reaches the relay"
    );

    let (router, state) = router_with_db().await;
    let admin = seed_admin(&state).await;
    let user = seed_user(&state, "reader").await;
    let artifact = opaque_content_policy(4);
    ingest_relay_artifact(&state, artifact.clone(), None, &fixture_registry())
        .await
        .unwrap();
    let before = relay_get(&router, &state, user, "NO", PAYLOAD_KIND_CONTENT_POLICY).await;

    declare(&router, &state, admin, Some("SE")).await;
    declare(&router, &state, admin, None).await;

    let after = relay_get(&router, &state, user, "NO", PAYLOAD_KIND_CONTENT_POLICY).await;
    assert_eq!(before.envelope, Some(artifact));
    assert_eq!(
        after.envelope, before.envelope,
        "declaring and withdrawing a situs leaves the relay's answer untouched"
    );
}

// ── The nest-as-publisher leg ───────────────────────────────────────────────
//
// `region-blocking.md` § The content plane → *The nest-as-publisher leg*: the
// nest applies its declared situs's content policy to the public web pages it
// renders from published posts, and to nothing else. The pages are static, so
// what is under test is that every event that changes what is in force — a
// declaration, a withdrawal, an accepted document, a retired one — reaches the
// page by itself, without any publish happening to re-render it.

/// A router with the region kinds and the posts kinds, over an `AppState` whose
/// web-content service renders into a real disk blob store and verifies region
/// documents against the fixture registry, and whose demand doors also verify
/// enrollment against it (`install_region_registry_for_test`) — the one
/// registry this test hands render, relay AND declaration, as production
/// hands all three the compiled-in one.
async fn router_with_web_site() -> (RpcRouter, Arc<AppState>, tempfile::TempDir) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let blobs = tempfile::tempdir().unwrap();
    let store = Arc::new(fauna_nest::blob_store::DiskBlobStore::new(blobs.path()).unwrap());
    let mut state = AppState {
        web_content_service: Some(Arc::new(
            fauna_nest::web_content::service::WebContentService::new(db.clone(), store)
                .with_region_registry(fixture_registry()),
        )),
        ..AppState::for_test(db)
    };
    state.install_region_registry_for_test(fixture_registry());
    let state = Arc::new(state);
    let mut b = RpcRouter::builder();
    fauna_nest::region_tier::register_region_handlers(&mut b);
    fauna_nest::region_relay::register_region_relay_handlers(&mut b);
    fauna_nest::posts_handlers::register_posts_handlers(&mut b);
    (b.build(), state, blobs)
}

/// The fixture authority's content policy: block anything labelled `nsfw` at
/// ≥ 500 per-mille, with its own reason.
fn blocking_content_policy(sequence: u64) -> PolicyArtifact {
    use fauna_core::region_policy::{
        ContentPolicyDocument, ContentRule, ContentVerdict, GRAMMAR_VERSION, REASON_DEFAULT_KEY,
    };
    let document = ContentPolicyDocument {
        version: GRAMMAR_VERSION,
        rules: vec![ContentRule {
            factor: "nsfw".into(),
            min_permille: 500,
            verdict: ContentVerdict::Block,
            reason_code: "FX-7".into(),
            reason: std::collections::BTreeMap::from([(
                REASON_DEFAULT_KEY.to_string(),
                "Withheld under the Fixture Act, section 7.".to_string(),
            )]),
            extra: Default::default(),
        }],
        scorers: Vec::new(),
        extra: Default::default(),
    };
    sign_artifact(
        PolicyArtifact {
            region: region(),
            key_id: "k1".into(),
            sequence,
            issued_at: 1_000,
            payload_kind: PAYLOAD_KIND_CONTENT_POLICY.to_string(),
            payload: encode_canonical(&document).unwrap().to_vec(),
            sig: Vec::new(),
        },
        &authority_key(),
    )
    .unwrap()
}

/// Store and web-publish one text post of `author`'s; returns its id and the
/// exact bytes stored.
async fn publish_text_post(
    state: &Arc<AppState>,
    author: [u8; 32],
    slug: &str,
    text: &str,
) -> ([u8; 32], Vec<u8>) {
    use fauna_core::data::{Post, PostBody, Timestamp};
    let post = Post {
        author: fauna_core::identity::ActorId(author),
        created_at: Timestamp(1_700_000_000_000_000),
        body: PostBody::Text {
            content: text.into(),
            facets: vec![],
        },
        references: vec![],
        expires_at: None,
        gated: None,
        content_warning: None,
        origin: None,
    };
    let data = fauna_core::encoding::canonical_encode(&post).unwrap();
    let post_id: [u8; 32] = *blake3::hash(&data).as_bytes();
    state.db.put_post(&post_id, &data, None).await.unwrap();
    state
        .db
        .publish_web_post(&author, &post_id, slug)
        .await
        .unwrap();
    (post_id, data)
}

/// The rendered bytes of `author`'s public `path`, as served.
async fn public_page(state: &Arc<AppState>, author: &[u8; 32], path: &str) -> String {
    let row = state
        .db
        .get_web_rendered(author, path)
        .await
        .unwrap()
        .unwrap_or_else(|| panic!("{path} is rendered"));
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&row.blob_hash);
    let bytes = state
        .web_content_service
        .as_ref()
        .unwrap()
        .blob_store()
        .get(&fauna_core::data::ContentHash::from_digest_raw(hash))
        .await
        .unwrap()
        .expect("the rendered blob exists");
    String::from_utf8(bytes).unwrap()
}

/// **The situs's content policy governs the nest's public page, and nothing
/// else** — end to end over the real handlers, with no test-side render after
/// the first: each change of what is in force re-renders the page by itself.
///
/// A labelled post and a clean one are published. Declaring the situs records
/// the publisher leg's own demand for its content policy; the authority's
/// document arriving through the relay replaces the labelled post's public page
/// with the reasoned placeholder while the clean one is untouched; the
/// authenticated `fauna.posts.get` of the blocked post still returns its bytes
/// verbatim (the in-app surface applies the viewer's own region, never the
/// nest's); withdrawing the declaration brings the body back; re-declaring
/// applies the cached document again; and de-listing the authority retires the
/// document and brings the body back once more.
#[tokio::test]
async fn the_situs_content_policy_governs_the_public_page_and_nothing_else() {
    let (router, state, _blobs) = router_with_web_site().await;
    let admin = seed_admin(&state).await;
    let author = seed_user(&state, "publisher").await;
    let (blocked, blocked_bytes) =
        publish_text_post(&state, author, "blocked", "a-situs-blocked-body-3d9e").await;
    publish_text_post(&state, author, "clean", "a-situs-clean-body-81f5").await;
    // The label NAMES ITS WRITER — a 32-byte non-zero `scanner_id`. The
    // publisher fold reads only attributed rows, so a fixture row with the empty
    // `scanner_id` this test used to pass would exercise nothing at all.
    state
        .db
        .upsert_content_label(
            "post",
            &hex::encode(blocked),
            "nsfw",
            0.9,
            0,
            b"fixture-classifier",
            1,
            0,
            None,
            None,
            0,
            &[0x5Au8; 32],
            b"",
        )
        .await
        .unwrap();
    state
        .web_content_service
        .as_ref()
        .unwrap()
        .render_published_posts(&author)
        .await
        .unwrap();
    const PAGE: &str = "post/blocked.html";
    const BODY: &str = "a-situs-blocked-body-3d9e";
    const NOTICE: &str = "Not shown in NO — blocked under the policy of Fixture Authority";
    assert!(public_page(&state, &author, PAGE).await.contains(BODY));

    // Declare: nothing is in force yet, but the leg's own demand door
    // (`demand_situs_content_policies`) records it itself — no app may ever
    // ask for the nest's own situs. `router_with_web_site` installs
    // `fixture_registry()` as this `AppState`'s demand-door override, so the
    // declaration door checks an ENROLLED registry, not the empty compiled-in
    // one, and this is the door's own ask, not a seed past it.
    declare(&router, &state, admin, Some("NO")).await;
    assert!(
        state
            .db
            .relay_artifact(&region(), PAYLOAD_KIND_CONTENT_POLICY)
            .await
            .unwrap()
            .is_some(),
        "declaring a situs asks the relay for its content policy"
    );
    assert!(public_page(&state, &author, PAGE).await.contains(BODY));

    // The authority's document arrives: the page changes by itself.
    assert_eq!(
        ingest_relay_artifact(
            &state,
            blocking_content_policy(1),
            None,
            &fixture_registry()
        )
        .await
        .unwrap(),
        Ingested::Accepted { sequence: 1 }
    );
    let page = public_page(&state, &author, PAGE).await;
    assert!(page.contains(NOTICE), "the reasoned placeholder: {page}");
    assert!(
        page.contains("Withheld under the Fixture Act, section 7."),
        "the authority's reason, verbatim: {page}"
    );
    assert!(!page.contains(BODY), "the body is withheld: {page}");
    for listing in ["index.html", "feed.xml"] {
        let listing_bytes = public_page(&state, &author, listing).await;
        assert!(!listing_bytes.contains(BODY), "{listing}: {listing_bytes}");
        assert!(
            listing_bytes.contains("a-situs-clean-body-81f5"),
            "an unlabelled post is untouched in {listing}: {listing_bytes}"
        );
    }
    let clean = public_page(&state, &author, "post/clean.html").await;
    assert!(!clean.contains("fauna-region-notice"), "{clean}");

    // The in-app read of the same post is untouched.
    let raw = common::dispatch(
        &router,
        state.clone(),
        author,
        "fauna.posts.get",
        bytes::Bytes::from(
            encode_canonical(&fauna_protocol::posts::PostGetRequest {
                post_id: hex::encode(blocked),
                extra: Default::default(),
            })
            .unwrap()
            .to_vec(),
        ),
    )
    .await
    .expect("the author reads their own post");
    let reply: fauna_protocol::posts::PostGetReply = decode(&raw).unwrap();
    assert_eq!(reply.body.into_vec(), blocked_bytes);
    assert!(reply.legal_takedown.is_none());

    // Withdraw: the body is back.
    declare(&router, &state, admin, None).await;
    let page = public_page(&state, &author, PAGE).await;
    assert!(
        page.contains(BODY),
        "withdrawal re-renders the body: {page}"
    );
    assert!(!page.contains(NOTICE), "{page}");

    // Re-declare: the cached document applies again at once.
    declare(&router, &state, admin, Some("NO")).await;
    assert!(public_page(&state, &author, PAGE).await.contains(NOTICE));

    // De-list the authority: the relay retires the document and the page
    // follows it.
    refresh_relay_once(&state, &RegionRegistry::default()).await;
    let page = public_page(&state, &author, PAGE).await;
    assert!(
        page.contains(BODY),
        "a retired document binds no page: {page}"
    );
    assert!(!page.contains(NOTICE), "{page}");
}

// ---------------------------------------------------------------------------
// The owed-render mark rides the policy-moving write's own transaction
// (`web-content-hosting.md` § Routing, render, serving → *A revoke is
// durable*). Three writes can move the content policy in force for the
// declared situs, and each one is followed — after its commit — by the walk
// that re-renders every publishing site. A nest that stops in between must
// restart OWING those renders; otherwise every page keeps serving under a
// policy that no longer governs it, with nothing owed and nothing periodic to
// notice.
//
// Each pin below arranges exactly that torn state: it calls the policy write
// alone, with the walk that follows it in the handler never reached, and then
// runs the boot drain. The page assertion is `public_page` — the rendered row
// plus its blob, the door the rest of this file uses — not the `Host`-header
// serve door of `conformance_web.rs`: what is under test is the mark, and a
// render that never ran leaves the old bytes in both.
// ---------------------------------------------------------------------------

const TORN_PAGE: &str = "post/blocked.html";
const TORN_BODY: &str = "a-torn-blocked-body-6b04";
const TORN_NOTICE: &str = "Not shown in NO — blocked under the policy of Fixture Authority";

/// A rendered publishing site: one `nsfw`-labelled post and one clean one,
/// rendered once under no region policy at all, so the labelled body is on the
/// public page. Returns `(admin, author)`.
async fn a_rendered_publishing_site(state: &Arc<AppState>) -> ([u8; 32], [u8; 32]) {
    let admin = seed_admin(state).await;
    let author = seed_user(state, "publisher").await;
    let (blocked, _) = publish_text_post(state, author, "blocked", TORN_BODY).await;
    publish_text_post(state, author, "clean", "a-torn-clean-body-4c21").await;
    // The label names its writer — an unattributed row the publisher fold
    // skips would make every assertion below vacuous.
    state
        .db
        .upsert_content_label(
            "post",
            &hex::encode(blocked),
            "nsfw",
            0.9,
            0,
            b"fixture-classifier",
            1,
            0,
            None,
            None,
            0,
            &[0x5Au8; 32],
            b"",
        )
        .await
        .unwrap();
    state
        .web_content_service
        .as_ref()
        .unwrap()
        .render_published_posts(&author)
        .await
        .unwrap();
    assert!(
        public_page(state, &author, TORN_PAGE)
            .await
            .contains(TORN_BODY),
        "the site starts with the labelled body on its public page"
    );
    assert!(
        state.db.list_web_render_owed().await.unwrap().is_empty(),
        "a completed render leaves nothing owed"
    );
    (admin, author)
}

/// The fixture authority's blocking content policy as a `VerifiedArtifact`,
/// for a pin that stores it through the relay writer directly rather than
/// through `ingest_relay_artifact` (which would run the walk too).
fn verified_blocking_policy(sequence: u64) -> fauna_core::region_authority::VerifiedArtifact {
    // `1_000` is the fixture's own `issued_at`; `verify_artifact` reads `now`
    // only for its issued-in-future bound, so nothing here reads a clock.
    fauna_core::region_authority::verify_artifact(
        blocking_content_policy(sequence),
        &fixture_registry(),
        1_000,
        None,
    )
    .unwrap()
}

/// Drain, and assert the drained render moved the page: `expect_notice` is
/// whether the policy now in force withholds the labelled body.
async fn the_boot_drain_pays_the_render(
    state: &Arc<AppState>,
    author: &[u8; 32],
    expect_notice: bool,
) {
    let wcs = state.web_content_service.as_ref().unwrap();
    assert_eq!(
        wcs.drain_owed_renders().await.unwrap().failed,
        0,
        "no site failed to render"
    );
    let page = public_page(state, author, TORN_PAGE).await;
    assert_eq!(
        page.contains(TORN_NOTICE),
        expect_notice,
        "the drained render folds the policy now in force: {page}"
    );
    assert_eq!(
        page.contains(TORN_BODY),
        !expect_notice,
        "the body follows it: {page}"
    );
    assert!(
        state.db.list_web_render_owed().await.unwrap().is_empty(),
        "a successful render discharges the marker"
    );
}

/// **The situs declaration write owes the renders it makes necessary.**
/// Declaring a region puts every cached content policy on its chain in force,
/// so the `nest_region` write is the atomic decision point: it carries the
/// mark for every publishing site in its own transaction, and a nest torn
/// before `rerender_public_sites` restarts owing those renders.
#[tokio::test]
async fn a_situs_declaration_torn_before_its_walk_leaves_every_publishing_site_owed() {
    let (_router, state, _blobs) = router_with_web_site().await;
    let (_admin, author) = a_rendered_publishing_site(&state).await;
    // The document is cached while the nest is undeclared, so it binds nothing
    // yet and the ingest renders nothing: the declaration below is the one
    // write that moves what is in force.
    assert_eq!(
        ingest_relay_artifact(
            &state,
            blocking_content_policy(1),
            None,
            &fixture_registry()
        )
        .await
        .unwrap(),
        Ingested::Accepted { sequence: 1 }
    );
    assert!(state.db.list_web_render_owed().await.unwrap().is_empty());

    state.db.set_declared_region(&region(), true).await.unwrap();

    assert_eq!(
        state.db.list_web_render_owed().await.unwrap(),
        vec![author],
        "the declaration's OWN transaction owes the publishing site its render"
    );
    assert!(
        public_page(&state, &author, TORN_PAGE)
            .await
            .contains(TORN_BODY),
        "the torn state: the policy is in force and the page still serves the body"
    );
    the_boot_drain_pays_the_render(&state, &author, true).await;
}

/// **The relay cache write owes the renders it makes necessary.** A newer
/// content policy accepted for a region on the declared situs's chain changes
/// what every public page folds, so `put_relay_artifact` carries the mark in
/// the same transaction as the envelope it stores.
#[tokio::test]
async fn a_relayed_policy_torn_before_its_walk_leaves_every_publishing_site_owed() {
    let (router, state, _blobs) = router_with_web_site().await;
    let (admin, author) = a_rendered_publishing_site(&state).await;
    // Declared through the real door, with nothing cached yet: the door's own
    // walk runs and discharges, and the page is unchanged.
    declare(&router, &state, admin, Some("NO")).await;
    assert!(
        state.db.list_web_render_owed().await.unwrap().is_empty(),
        "the declaration's walk paid its own mark"
    );
    assert!(
        public_page(&state, &author, TORN_PAGE)
            .await
            .contains(TORN_BODY)
    );

    state
        .db
        .put_relay_artifact(&verified_blocking_policy(1), None, true)
        .await
        .unwrap();

    assert_eq!(
        state.db.list_web_render_owed().await.unwrap(),
        vec![author],
        "the relay write's OWN transaction owes the publishing site its render"
    );
    assert!(
        public_page(&state, &author, TORN_PAGE)
            .await
            .contains(TORN_BODY),
        "the torn state: the document is in force and the page still serves the body"
    );
    the_boot_drain_pays_the_render(&state, &author, true).await;
}

/// **Retiring a relayed policy owes the renders it makes necessary — the
/// mirror case.** De-listing the authority takes the document out of force,
/// which RESTORES pages it withheld; a nest torn between the retirement and
/// the walk would otherwise keep withholding them with nothing owed.
#[tokio::test]
async fn a_retired_policy_torn_before_its_walk_leaves_every_publishing_site_owed() {
    let (router, state, _blobs) = router_with_web_site().await;
    let (admin, author) = a_rendered_publishing_site(&state).await;
    declare(&router, &state, admin, Some("NO")).await;
    assert_eq!(
        ingest_relay_artifact(
            &state,
            blocking_content_policy(1),
            None,
            &fixture_registry()
        )
        .await
        .unwrap(),
        Ingested::Accepted { sequence: 1 }
    );
    let page = public_page(&state, &author, TORN_PAGE).await;
    assert!(page.contains(TORN_NOTICE), "the policy is in force: {page}");
    assert!(state.db.list_web_render_owed().await.unwrap().is_empty());

    state
        .db
        .retire_relay_artifact(&region(), PAYLOAD_KIND_CONTENT_POLICY, true)
        .await
        .unwrap();

    assert_eq!(
        state.db.list_web_render_owed().await.unwrap(),
        vec![author],
        "the retirement's OWN transaction owes the publishing site its render"
    );
    let page = public_page(&state, &author, TORN_PAGE).await;
    assert!(
        page.contains(TORN_NOTICE),
        "the torn state: nothing is in force and the page still withholds the body: {page}"
    );
    the_boot_drain_pays_the_render(&state, &author, false).await;
}

/// **Withdrawing the declaration owes the renders it makes necessary.** The
/// declaration door has two writes over `nest_region` and this is the other
/// one: withdrawing takes every cached document out of force, so the pages it
/// withheld come back — and a nest torn between the `DELETE` and the walk
/// would keep withholding them with nothing owed.
#[tokio::test]
async fn a_situs_withdrawal_torn_before_its_walk_leaves_every_publishing_site_owed() {
    let (router, state, _blobs) = router_with_web_site().await;
    let (admin, author) = a_rendered_publishing_site(&state).await;
    declare(&router, &state, admin, Some("NO")).await;
    assert_eq!(
        ingest_relay_artifact(
            &state,
            blocking_content_policy(1),
            None,
            &fixture_registry()
        )
        .await
        .unwrap(),
        Ingested::Accepted { sequence: 1 }
    );
    let page = public_page(&state, &author, TORN_PAGE).await;
    assert!(page.contains(TORN_NOTICE), "the policy is in force: {page}");
    assert!(state.db.list_web_render_owed().await.unwrap().is_empty());

    assert!(state.db.clear_declared_region(true).await.unwrap());

    assert_eq!(
        state.db.list_web_render_owed().await.unwrap(),
        vec![author],
        "the withdrawal's OWN transaction owes the publishing site its render"
    );
    let page = public_page(&state, &author, TORN_PAGE).await;
    assert!(
        page.contains(TORN_NOTICE),
        "the torn state: no situs is declared and the page still withholds the body: {page}"
    );
    the_boot_drain_pays_the_render(&state, &author, false).await;
}
