//! tier_3 coverage for the client-set NAT axis (`fauna.setup.nat_mode`) — the
//! commit ceremony (`nat_mode_core::commit_nat_mode_core`), the mutable upsert,
//! the boot reconcile (`resolve_node_mode`: DB row wins / config seed fallback),
//! the dependent-service re-eval (`mta_should_run` keys on the new mode), and
//! factory-reset clearing. Mirrors `storage_mode_api.rs` but for the mutable NAT
//! axis (no write-once `mode_conflict`). Calls the transport-agnostic core
//! directly (the WS handler is a thin adapter unit-tested in
//! `nat_mode_handlers`). Design tracked internally (client-set NAT mode,
//! 2026-06-15).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use ed25519_dalek::{Signer, SigningKey};
use fauna_nest::config::NodeMode;
use fauna_nest::db::CacheDb;
use fauna_nest::mode_commit::ModeCommitError;
use fauna_nest::nat_mode_core::{commit_nat_mode_core, resolve_node_mode};
use fauna_nest::token_store::TokenStore;

static HARNESS_COUNTER: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> i64 {
    fauna_core::data::Timestamp::now_millis() as i64
}

fn random_signing_key() -> SigningKey {
    use ring::rand::{SecureRandom, SystemRandom};
    let mut b = [0u8; 32];
    SystemRandom::new().fill(&mut b).unwrap();
    SigningKey::from_bytes(&b)
}

/// Canonical signed bytes for a nat-mode set (`fauna.setup.nat_mode`) — the
/// tagged, nest-bound single-source builder.
fn nat_mode_signed_bytes(
    mode_str: &str,
    actor_id_hex: &str,
    ts_ms: i64,
    nest_hex: &str,
) -> Vec<u8> {
    fauna_protocol::nat_mode::nat_mode_signed_message(mode_str, actor_id_hex, ts_ms, nest_hex)
}

/// The retired UNBOUND form's bytes — `SETUP_NAT_MODE_V1 ‖ mode ‖ \n ‖ actor ‖
/// \n ‖ ts` — built by hand because no shipped signer produces them any more
/// (the builder left with the compat-remnant sweep, 2026-09-24); the pins
/// below assert they are REFUSED.
fn unbound_v1_signed_bytes(mode_str: &str, actor_id_hex: &str, ts_ms: i64) -> Vec<u8> {
    let body = format!("{mode_str}\n{actor_id_hex}\n{ts_ms}");
    fauna_protocol::sig_domain::domain_separated(
        fauna_protocol::sig_domain::SETUP_NAT_MODE_V1,
        body.as_bytes(),
    )
}

/// This harness's own identity, 64-hex — what every commit binds to.
fn nest_hex(h: &Harness) -> String {
    hex::encode(h.state.nest_identity.public_key_bytes())
}

struct Harness {
    data_dir: tempfile::TempDir,
    db_path: String,
    db: Arc<CacheDb>,
    state: Arc<fauna_nest::routes::AppState>,
}

/// A nest with a real on-disk DB inside a temp dir, unclaimed (claim-code file
/// present), NAT mode unset (so `node_mode` follows the `seed` arg, default
/// `Public`).
async fn harness() -> Harness {
    harness_with_seed(NodeMode::Public).await
}

async fn harness_with_seed(seed: NodeMode) -> Harness {
    harness_with_seed_and_identity(seed, [1u8; 32]).await
}

/// [`harness_with_seed`] with a caller-chosen nest-identity seed — the
/// cross-nest tests need two harnesses with DISTINCT `NestIdentity`s (two
/// independently-claimed nests, the PROBE-482-B shape).
async fn harness_with_seed_and_identity(seed: NodeMode, identity_seed: [u8; 32]) -> Harness {
    let _ = HARNESS_COUNTER.fetch_add(1, Ordering::Relaxed);
    let data_dir = tempfile::tempdir().unwrap();
    let db_path = data_dir.path().join("nest.db");
    std::fs::write(data_dir.path().join("claim-code"), "ABCDEF").unwrap();

    let db = Arc::new(CacheDb::open(&db_path).unwrap());
    let token_store = Arc::new(TokenStore::new());

    let config = Arc::new(fauna_nest::config::NestConfig {
        nest: fauna_nest::config::NestSection {
            mode: seed,
            listen: "127.0.0.1:0".into(),
            db_path: db_path.to_string_lossy().into_owned(),
            ..Default::default()
        },
        bridges: None,
        submission: None,
        acme: None,
        email: None,
        update: Default::default(),
    });

    let nest_identity = Arc::new(fauna_nest::nest_identity::NestIdentity::from_seed(
        &identity_seed,
    ));
    let security_notifier = Arc::new(fauna_nest::security_notify::SecurityNotifier::new(
        db.clone(),
        Arc::new(fauna_nest::nest_identity::NestIdentity::from_seed(
            &[2u8; 32],
        )),
    ));

    // node_mode tracks the resolved boot value (no row yet ⇒ the seed), exactly
    // as `start_server` populates it.
    let resolved = resolve_node_mode(&db, &config).await;
    let state = fauna_nest::routes::AppState {
        config,
        nest_identity,
        security_notifier,
        auth: fauna_nest::state::AuthState {
            token_store: token_store.clone(),
            ..Default::default()
        },
        node_mode: Arc::new(tokio::sync::RwLock::new(resolved)),
        ..fauna_nest::routes::AppState::for_test(db.clone())
    };
    let state = Arc::new(state);

    Harness {
        db_path: db_path.to_string_lossy().into_owned(),
        data_dir,
        db,
        state,
    }
}

/// Claim admin in-process via the transport-agnostic core so an admin actor
/// exists and the claim-code file is deleted. Returns the admin's signing key.
async fn claim_admin(h: &Harness) -> SigningKey {
    let sk = random_signing_key();
    claim_admin_as(h, &sk).await;
    sk
}

/// [`claim_admin`] with a caller-held key — the cross-nest tests claim the
/// SAME actor as admin on two independent harnesses (the PROBE-482-B shape:
/// one hosted admin, several nests).
async fn claim_admin_as(h: &Harness, sk: &SigningKey) {
    use fauna_nest::claim_core::claim_admin_core;
    let actor_hex = hex::encode(sk.verifying_key().to_bytes());
    let ts = now_ms();
    let msg = fauna_protocol::claim::claim_admin_signed_message(
        &sk.verifying_key().to_bytes(),
        ts as u64,
    );
    let sig = hex::encode(sk.sign(&msg).to_bytes());
    claim_admin_core(
        &h.state, &actor_hex, ts as u64, &sig, "ABCDEF", "admin", None,
    )
    .await
    .expect("claim-admin should succeed");
}

/// Sign + commit a nat-mode set as `signer` at an **explicit** timestamp.
///
/// The timestamp is a parameter rather than an internal `now_ms()` because the
/// signed message carries no nonce: two commits of the *same* mode by the same
/// admin in the same millisecond are byte-identical, so the second is refused
/// as a replay once the signature is single-use
/// (`auth_core::ReplayGuard::check_and_record`, given
/// `mode_commit::MAX_MODE_COMMIT_AGE_SECS`). A test that re-commits a mode
/// therefore says which timestamps it means, instead of resting on the work
/// between two calls happening to straddle a millisecond boundary — the
/// wall-clock dependency e2e convention 14 rules out.
async fn commit_as_at(
    h: &Harness,
    signer: &SigningKey,
    mode_str: &str,
    ts: i64,
) -> Result<fauna_nest::nat_mode_core::NatModeOutcome, ModeCommitError> {
    let actor_hex = hex::encode(signer.verifying_key().to_bytes());
    let own = nest_hex(h);
    let sig = hex::encode(
        signer
            .sign(&nat_mode_signed_bytes(mode_str, &actor_hex, ts, &own))
            .to_bytes(),
    );
    commit_nat_mode_core(&h.state, mode_str, &actor_hex, ts, &sig, &own).await
}

/// [`commit_as_at`] at the current wall clock — for the tests that commit a
/// given mode only once, where no two signatures can coincide.
async fn commit_as(
    h: &Harness,
    signer: &SigningKey,
    mode_str: &str,
) -> Result<fauna_nest::nat_mode_core::NatModeOutcome, ModeCommitError> {
    commit_as_at(h, signer, mode_str, now_ms()).await
}

#[tokio::test]
async fn commit_sets_mode_and_is_mutable() {
    let h = harness().await;
    let admin = claim_admin(&h).await;
    // `public` is committed twice below, so the timestamps are pinned apart
    // rather than left to the clock (see `commit_as_at`).
    let base = now_ms();

    // Set private — the row persists and the live AppState.node_mode swaps.
    let out = commit_as_at(&h, &admin, "private", base)
        .await
        .expect("set private");
    assert_eq!(out.mode, NodeMode::Private);
    assert_eq!(h.db.get_nat_mode().await.unwrap(), Some(NodeMode::Private));
    assert_eq!(*h.state.node_mode.read().await, NodeMode::Private);

    // Mutable: flip back to public (the behavior that distinguishes the NAT
    // axis from the write-once storage mode — no mode_conflict).
    let out = commit_as_at(&h, &admin, "public", base + 1)
        .await
        .expect("flip to public");
    assert_eq!(out.mode, NodeMode::Public);
    assert_eq!(h.db.get_nat_mode().await.unwrap(), Some(NodeMode::Public));
    assert_eq!(*h.state.node_mode.read().await, NodeMode::Public);

    // Idempotent re-set of the same mode succeeds — a fresh signature over a
    // later timestamp, which is what an honest client always sends.
    commit_as_at(&h, &admin, "public", base + 2)
        .await
        .expect("idempotent re-set");
    assert_eq!(*h.state.node_mode.read().await, NodeMode::Public);
}

#[tokio::test]
async fn flip_re_evaluates_mta_should_run() {
    // A public→private flip must make `mta_should_run` (the MTA supervisor gate)
    // false on the resolved mode, and private→public restore it. The handler's
    // `apply_node_mode_change` reconciles the supervisor against exactly this.
    let h = harness().await;
    let admin = claim_admin(&h).await;

    commit_as(&h, &admin, "private").await.expect("set private");
    let mode = *h.state.node_mode.read().await;
    assert!(
        !fauna_nest::mail_enable::mta_should_run(mode, true),
        "private nest never runs the MTA, even with mail enabled"
    );

    commit_as(&h, &admin, "public")
        .await
        .expect("flip to public");
    let mode = *h.state.node_mode.read().await;
    assert!(
        fauna_nest::mail_enable::mta_should_run(mode, true),
        "public nest with mail enabled runs the MTA"
    );
}

#[tokio::test]
async fn rejects_unknown_mode() {
    let h = harness().await;
    let admin = claim_admin(&h).await;
    let err = commit_as(&h, &admin, "relay").await.unwrap_err();
    assert!(matches!(err, ModeCommitError::InvalidRequest(_)));
    // No row written on a rejected set.
    assert_eq!(h.db.get_nat_mode().await.unwrap(), None);
}

#[tokio::test]
async fn rejects_bad_signature() {
    let h = harness().await;
    let admin = claim_admin(&h).await;
    let actor_hex = hex::encode(admin.verifying_key().to_bytes());
    let ts = now_ms();
    // A signature over the wrong bytes (a different mode) must fail verification.
    let bad_sig = hex::encode(
        admin
            .sign(&nat_mode_signed_bytes(
                "public",
                &actor_hex,
                ts,
                &nest_hex(&h),
            ))
            .to_bytes(),
    );
    let err = commit_nat_mode_core(&h.state, "private", &actor_hex, ts, &bad_sig, &nest_hex(&h))
        .await
        .unwrap_err();
    assert!(matches!(err, ModeCommitError::SignatureFailed(_)));
}

#[tokio::test]
async fn rejects_stale_timestamp() {
    let h = harness().await;
    let admin = claim_admin(&h).await;
    let actor_hex = hex::encode(admin.verifying_key().to_bytes());
    let stale = now_ms() - 10 * 60 * 1000; // 10 min ago (> ±300 s)
    let sig = hex::encode(
        admin
            .sign(&nat_mode_signed_bytes(
                "private",
                &actor_hex,
                stale,
                &nest_hex(&h),
            ))
            .to_bytes(),
    );
    let err = commit_nat_mode_core(&h.state, "private", &actor_hex, stale, &sig, &nest_hex(&h))
        .await
        .unwrap_err();
    assert!(matches!(err, ModeCommitError::InvalidRequest(_)));
}

#[tokio::test]
async fn rejects_non_admin() {
    let h = harness().await;
    let _admin = claim_admin(&h).await;
    // A different, non-admin key signs a well-formed request.
    let stranger = random_signing_key();
    let err = commit_as(&h, &stranger, "private").await.unwrap_err();
    assert!(matches!(err, ModeCommitError::NotAdmin));
}

#[tokio::test]
async fn rejects_before_claim() {
    // No admin yet — an unclaimed nest rejects any set.
    let h = harness().await;
    let would_be_admin = random_signing_key();
    let err = commit_as(&h, &would_be_admin, "private").await.unwrap_err();
    assert!(matches!(err, ModeCommitError::NotClaimed));
}

#[tokio::test]
async fn setup_status_reports_seed_then_flips_after_commit() {
    // The anonymous setup-status heartbeat reports the resolved NAT axis
    // (`node_mode`) — the seed before any commit, the committed value after —
    // which is what seeds the wizard's `nat_mode_choice` pre-selection
    // (onboarding.md § 3b-bis). Flow: FAUNA_MODE seed → AppState.node_mode →
    // setup_status_core → SetupStatusReply.node_mode ("private") → client
    // `SetupStatus.node_mode` → `reset_nat_mode_snapshot` pre-selection.
    let h = harness_with_seed(NodeMode::Private).await;
    let s = fauna_nest::discovery_core::setup_status_core(&h.state, false).await;
    assert_eq!(s.node_mode, NodeMode::Private, "pre-commit: the seed");

    let admin = claim_admin(&h).await;
    commit_as(&h, &admin, "public")
        .await
        .expect("flip to public");
    let s = fauna_nest::discovery_core::setup_status_core(&h.state, false).await;
    assert_eq!(s.node_mode, NodeMode::Public, "post-commit: the set value");
}

#[tokio::test]
async fn boot_reconcile_db_row_wins_else_seed() {
    // Seed = private (a home-relay box's FAUNA_MODE posture); no row ⇒ private.
    let h = harness_with_seed(NodeMode::Private).await;
    assert_eq!(
        resolve_node_mode(&h.db, &h.state.config).await,
        NodeMode::Private
    );

    // A client-set row wins over the seed, in the opposite direction.
    h.db.set_nat_mode(NodeMode::Public).await.unwrap();
    assert_eq!(
        resolve_node_mode(&h.db, &h.state.config).await,
        NodeMode::Public
    );
}

#[tokio::test]
async fn factory_reset_clears_nat_mode_row() {
    // The NAT mode lives only in the DB (no marker file); factory reset wipes the
    // DB, so a reset box re-enters mode-unresolved and falls back to the seed.
    let h = harness_with_seed(NodeMode::Public).await;
    let admin = claim_admin(&h).await;
    commit_as(&h, &admin, "private").await.expect("set private");
    assert_eq!(h.db.get_nat_mode().await.unwrap(), Some(NodeMode::Private));

    // Drop the live DB handle so the file lock is released, then run the reset
    // (stages a marker, then wipes on the next "boot").
    let db_path = h.db_path.clone();
    let data_dir_path = h.data_dir.path().to_path_buf();
    let blob_dir = data_dir_path.join("blobs").to_string_lossy().into_owned();
    drop(h);

    // The reset clears the blob dir's contents (keeping the dir) — it must exist.
    std::fs::create_dir_all(&blob_dir).unwrap();
    fauna_nest::factory_reset::stage_factory_reset(&data_dir_path, "NEWCODE").unwrap();
    let ran = fauna_nest::factory_reset::maybe_run_factory_reset(&db_path, &blob_dir).unwrap();
    assert!(ran, "marker present ⇒ reset runs");

    // Reopen the (wiped + re-migrated) DB: no row ⇒ resolves to the seed.
    let db = Arc::new(CacheDb::open(&db_path).unwrap());
    assert_eq!(db.get_nat_mode().await.unwrap(), None);
}

/// PROBE-482-A as a standing regression pin.
///
/// `fauna.setup.nat_mode` rides the **anonymous** connection where "the
/// signature *is* the auth (no bearer)". Until this pin, nothing consumed a
/// verified mode-commit signature, so the wire triple `(actor_id, timestamp,
/// signature)` was a bearer token for the NAT posture for the whole ±300 s
/// freshness window: capture it once, re-submit it verbatim with no new
/// signature and no admin involvement, and the posture moves.
///
/// That is not a bookkeeping write — `apply_node_mode_change` step 3 reconciles
/// the MTA supervisor, so a private→public replay brings the perimeter SMTP
/// parser up on 25/465/587 on a box the admin deliberately made private.
///
/// The guard is the one the ratified sibling ceremony on this same connection
/// already uses (`transport.md` § Pre-identity (anonymous) connection: direct
/// auth "makes a verified signature **single-use** within its window"), so a
/// consumed signature is refused with the *opaque* signature failure — no
/// replay-vs-bad-signature oracle.
#[tokio::test]
async fn a_consumed_nat_mode_signature_cannot_move_the_posture_again() {
    let h = harness().await;
    let admin = claim_admin(&h).await;
    let actor_hex = hex::encode(admin.verifying_key().to_bytes());

    // The admin sets `public`. Capture the exact wire triple the client sent.
    let ts = now_ms();
    let sig = hex::encode(
        admin
            .sign(&nat_mode_signed_bytes(
                "public",
                &actor_hex,
                ts,
                &nest_hex(&h),
            ))
            .to_bytes(),
    );
    commit_nat_mode_core(&h.state, "public", &actor_hex, ts, &sig, &nest_hex(&h))
        .await
        .expect("the admin's own set must succeed");
    assert_eq!(*h.state.node_mode.read().await, NodeMode::Public);

    // The admin then goes private — the perimeter parser is commanded down.
    commit_as(&h, &admin, "private").await.expect("set private");
    assert_eq!(*h.state.node_mode.read().await, NodeMode::Private);

    // Replay: the captured triple re-submitted byte-identically.
    let replayed =
        commit_nat_mode_core(&h.state, "public", &actor_hex, ts, &sig, &nest_hex(&h)).await;

    assert!(
        matches!(replayed, Err(ModeCommitError::SignatureFailed(_))),
        "a consumed mode-commit signature must be refused, got {replayed:?}"
    );
    assert_eq!(
        *h.state.node_mode.read().await,
        NodeMode::Private,
        "the replay must not roll the NAT posture back"
    );
    assert_eq!(h.db.get_nat_mode().await.unwrap(), Some(NodeMode::Private));
    assert!(
        !fauna_nest::mail_enable::mta_should_run(*h.state.node_mode.read().await, true),
        "the replay must not bring the perimeter SMTP parser back up"
    );
}

/// PROBE-482-B's V1 arm is CLOSED — the retired unbound form is refused at
/// every nest, its own included.
///
/// A V1 blob named no nest, so one signature was valid at every nest where
/// that actor was admin. Through the transition window nest B accepted a
/// blob produced for nest A (the pin this replaces,
/// `a_v1_blob_is_still_accepted_cross_nest_through_the_transition_window`,
/// asserted that residual on purpose so the retirement would be a deliberate
/// flip). Flipped 2026-09-24 by the compat-remnant sweep: the V1 verify arm is
/// gone, so a V1 signature fails at the nest it was minted for AND at the
/// other, whatever `nest_id` is stapled beside it.
#[tokio::test]
async fn an_unbound_v1_blob_is_refused_at_every_nest() {
    let nest_a = harness_with_seed_and_identity(NodeMode::Public, [3u8; 32]).await;
    let nest_b = harness_with_seed_and_identity(NodeMode::Public, [4u8; 32]).await;
    let admin = random_signing_key();
    claim_admin_as(&nest_a, &admin).await;
    claim_admin_as(&nest_b, &admin).await;
    let actor_hex = hex::encode(admin.verifying_key().to_bytes());

    // The admin signs one V1 blob, intending it for nest A.
    let ts = now_ms();
    let sig = hex::encode(
        admin
            .sign(&unbound_v1_signed_bytes("private", &actor_hex, ts))
            .to_bytes(),
    );
    for nest in [&nest_a, &nest_b] {
        let own = nest_hex(nest);
        let refused =
            commit_nat_mode_core(&nest.state, "private", &actor_hex, ts, &sig, &own).await;
        assert!(
            matches!(refused, Err(ModeCommitError::SignatureFailed(_))),
            "a V1 signature must be refused, got {refused:?}"
        );
        assert_eq!(*nest.state.node_mode.read().await, NodeMode::Public);
        assert_eq!(nest.db.get_nat_mode().await.unwrap(), None);
    }
}

/// PROBE-482-B closed — the nest-bound V2 form.
///
/// A V2 blob signs the target nest's identity into the message
/// (`nat_mode_signed_message`), so the blob the admin minted for nest A is
/// refused by an independently-claimed nest B where the same actor is also
/// admin — the cross-nest arm the single-use guard (same-nest, per-nest store)
/// cannot reach. The refusal is the clear `invalid_request` naming the
/// mismatch, not the opaque signature failure: the nest_id is public,
/// requester-supplied cleartext, so naming it is self-diagnosis, not an oracle.
#[tokio::test]
async fn a_v2_blob_bound_to_one_nest_is_refused_by_another() {
    let nest_a = harness_with_seed_and_identity(NodeMode::Public, [5u8; 32]).await;
    let nest_b = harness_with_seed_and_identity(NodeMode::Public, [6u8; 32]).await;
    let admin = random_signing_key();
    claim_admin_as(&nest_a, &admin).await;
    claim_admin_as(&nest_b, &admin).await;
    let actor_hex = hex::encode(admin.verifying_key().to_bytes());
    let nest_a_hex = hex::encode(nest_a.state.nest_identity.public_key_bytes());

    // The admin signs one V2 blob bound to nest A; nest A accepts it.
    let ts = now_ms();
    let sig = hex::encode(
        admin
            .sign(&fauna_protocol::nat_mode::nat_mode_signed_message(
                "private",
                &actor_hex,
                ts,
                &nest_a_hex,
            ))
            .to_bytes(),
    );
    commit_nat_mode_core(&nest_a.state, "private", &actor_hex, ts, &sig, &nest_a_hex)
        .await
        .expect("the admin's own V2 set at its bound nest must succeed");
    assert_eq!(*nest_a.state.node_mode.read().await, NodeMode::Private);

    // The captured blob, re-submitted verbatim at nest B, is refused — the
    // bound identity is not B's — and B's posture does not move.
    let replayed =
        commit_nat_mode_core(&nest_b.state, "private", &actor_hex, ts, &sig, &nest_a_hex).await;
    assert!(
        matches!(replayed, Err(ModeCommitError::InvalidRequest(_))),
        "a V2 blob bound to another nest must be refused, got {replayed:?}"
    );
    assert_eq!(*nest_b.state.node_mode.read().await, NodeMode::Public);
    assert_eq!(nest_b.db.get_nat_mode().await.unwrap(), None);
}

/// A request whose `nest_id` names the receiving nest but whose signature
/// was minted over the retired **V1** bytes is refused: only the nest-bound
/// bytes are ever verified, so an attacker cannot take a captured V1 blob,
/// staple the target's public `nest_id` beside it, and pass it off as
/// nest-bound.
#[tokio::test]
async fn a_v1_signature_cannot_be_promoted_to_v2_by_stapling_a_nest_id() {
    let h = harness_with_seed_and_identity(NodeMode::Public, [7u8; 32]).await;
    let admin = claim_admin(&h).await;
    let actor_hex = hex::encode(admin.verifying_key().to_bytes());
    let nest_hex = hex::encode(h.state.nest_identity.public_key_bytes());

    let ts = now_ms();
    let v1_sig = hex::encode(
        admin
            .sign(&unbound_v1_signed_bytes("private", &actor_hex, ts))
            .to_bytes(),
    );
    let promoted =
        commit_nat_mode_core(&h.state, "private", &actor_hex, ts, &v1_sig, &nest_hex).await;
    assert!(
        matches!(promoted, Err(ModeCommitError::SignatureFailed(_))),
        "a V1 signature under a stapled nest_id must fail V2 verification, got {promoted:?}"
    );
    assert_eq!(*h.state.node_mode.read().await, NodeMode::Public);
}

/// The V2 verifier now demands the `nest_id` spelling match this
/// nest's own hex identity exactly, and always builds the signed message from
/// its own identity rather than the requester's casing of it. Before the fix,
/// `eq_ignore_ascii_case` accepted an upper-cased spelling of the same
/// identity AND the V2 message was built from the requester-supplied `bound`,
/// so the same logical commit had 2^64 valid byte encodings that differed
/// only in `nest_id` casing — a non-canonicality class inside a signature
/// construction. This pins the fix: an upper-cased (still byte-identical)
/// spelling of this nest's own identity is refused outright, not silently
/// signed over.
#[tokio::test]
async fn a_case_varied_nest_id_is_refused_not_silently_signed_over() {
    let h = harness_with_seed_and_identity(NodeMode::Public, [8u8; 32]).await;
    let admin = claim_admin(&h).await;
    let actor_hex = hex::encode(admin.verifying_key().to_bytes());
    let own_hex = hex::encode(h.state.nest_identity.public_key_bytes());
    let upper_hex = own_hex.to_uppercase();
    assert_ne!(
        upper_hex, own_hex,
        "the seeded identity must contain a hex letter for this pin to mean anything"
    );

    // The admin signs a V2 blob bound to the UPPERCASE spelling of this
    // nest's own identity -- exactly what a pre-fix nest would have accepted.
    let ts = now_ms();
    let sig = hex::encode(
        admin
            .sign(&fauna_protocol::nat_mode::nat_mode_signed_message(
                "private", &actor_hex, ts, &upper_hex,
            ))
            .to_bytes(),
    );
    let result = commit_nat_mode_core(&h.state, "private", &actor_hex, ts, &sig, &upper_hex).await;
    assert!(
        matches!(result, Err(ModeCommitError::InvalidRequest(_))),
        "a case-varied nest_id must be refused, got {result:?}"
    );
    assert_eq!(*h.state.node_mode.read().await, NodeMode::Public);
}

/// The single-use guard binds the **signature**, not the actor or the mode, so
/// an admin's ordinary re-set — a fresh timestamp, hence fresh signed bytes —
/// is untouched. Without this the guard would wedge the admin panel's own
/// flip-and-flip-back after the first commit.
#[tokio::test]
async fn a_freshly_signed_re_set_is_not_a_replay() {
    let h = harness().await;
    let admin = claim_admin(&h).await;

    let base = now_ms();
    commit_as_at(&h, &admin, "public", base)
        .await
        .expect("set public");
    commit_as_at(&h, &admin, "private", base + 1)
        .await
        .expect("set private");
    commit_as_at(&h, &admin, "public", base + 2)
        .await
        .expect("a re-signed set of an earlier mode is not a replay");
    assert_eq!(*h.state.node_mode.read().await, NodeMode::Public);
}
