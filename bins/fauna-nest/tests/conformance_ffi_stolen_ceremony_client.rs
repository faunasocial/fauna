//! The FFI stolen-identity driver against a real nest — `run_stolen_ceremony`,
//! the one body behind `succession_succeed_with_held_kit` and
//! `succession_succeed_stolen_locked`, driven through the session-less export.
//!
//! Goal doc: `docs/goal/behavior/identity-succession.md` § Implementation status
//! today — the FFI ceremony persists the successor seed, verifies it **by
//! read-back** (never off `SecretStore::set`'s return, which is infallible by
//! signature), and records the predecessor → successor link before any app
//! switches accounts.
//!
//! # What the sibling pins leave open
//!
//! `conformance_locked_succession_ceremony.rs` drives the *shared* composition
//! (`succeed_stolen_identity`), and `fauna-ffi`'s in-module tests pin the
//! read-back function `successor_reads_back` on its own. Neither drives the
//! *driver* that hands the verdict on: a driver that read back the
//! predecessor's actor, reported `persisted: true` unconditionally, or skipped
//! the link step stayed green under both. The dangerous direction is
//! `persisted: true` over a seed that never landed — the app tells the user the
//! key is saved, tears its session down, and the only copy of the key the
//! account now belongs to goes with it (`fauna-client-recovery`'s
//! `ceremony.rs`, the stolen-arm persist note). So this file pins the FFI face:
//! the verdict over a store whose writes land and one whose writes vanish, and
//! the link the landed arm records.
//!
//! Built with `recovery-aftermath` (the default-shipping configuration), so the
//! link step under test is `record_landed_succession`.

mod common;
use common::connected_client;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use ed25519_dalek::SigningKey;
use fauna_core::identity::ActorKeypair;
use fauna_core::recovery::RecoveryKey;
use fauna_ffi::{
    FfiAccountRegistry, FfiLandedSuccession, FfiSecretStore, FfiSuccessorStorePath,
    succession_succeed_stolen_locked,
};
use fauna_nest::db::CacheDb;
use fauna_nest::nest_identity::NestIdentity;
use fauna_nest::routes::{AppState, RegistrationConfig};
use fauna_nest::state::AuthState;
use fauna_nest::token_store::TokenStore;

const OLD_SEED: [u8; 32] = [0x12; 32];
/// The RecoveryKey the kit registers — and, as bare 64-hex, the phrase the
/// user pastes.
const ROOT: [u8; 32] = [0x22; 32];

fn old() -> ActorKeypair {
    ActorKeypair::from_secret(OLD_SEED)
}

/// A platform secure store whose writes land.
#[derive(Default)]
struct KeepingStore(Mutex<HashMap<String, String>>);

impl FfiSecretStore for KeepingStore {
    fn get(&self, key: String) -> Option<String> {
        self.0.lock().unwrap().get(&key).cloned()
    }
    fn set(&self, key: String, value: String) {
        self.0.lock().unwrap().insert(key, value);
    }
    fn delete(&self, key: String) {
        self.0.lock().unwrap().remove(&key);
    }
}

/// A platform secure store whose writes vanish — a keychain write the OS
/// silently drops. `set` returns cleanly, exactly as `SecretStore::set`'s
/// infallible signature lets it.
struct VanishingStore;

impl FfiSecretStore for VanishingStore {
    fn get(&self, _key: String) -> Option<String> {
        None
    }
    fn set(&self, _key: String, _value: String) {}
    fn delete(&self, _key: String) {}
}

struct ScratchMls(std::path::PathBuf);

impl FfiSuccessorStorePath for ScratchMls {
    fn mls_db_path(&self, successor_actor_hex: String) -> String {
        let dir = self.0.join(successor_actor_hex);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("mls_state.db").to_string_lossy().into_owned()
    }
}

/// A nest over a real socket with the handler families the ceremony reaches:
/// the pre-identity recovery plane, auth (challenge/verify), discovery.
async fn start() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let authority = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    let key = SigningKey::from_bytes(&[0x52; 32]);
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    db.set_nest_keypair(key.as_bytes(), &key.verifying_key().to_bytes())
        .await
        .unwrap();
    let state = Arc::new(AppState {
        nest_identity: Arc::new(NestIdentity::from_seed(&[0x52; 32])),
        nest_signing_key: Some(key),
        rpc_router: Arc::new({
            let mut b = fauna_nest::rpc_router::RpcRouter::builder();
            fauna_nest::auth_handlers::register_auth_handlers(&mut b);
            fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
            fauna_nest::recovery_handlers::register_recovery_handlers(&mut b);
            fauna_nest::account_handlers::register_account_handlers(&mut b);
            b.build()
        }),
        auth: AuthState {
            token_store: Arc::new(TokenStore::new()),
            registration: RegistrationConfig {
                handle_domain: Some(authority.clone()),
                ..Default::default()
            },
            ..Default::default()
        },
        ..AppState::for_test(db)
    });
    state
        .db
        .create_user(&old().actor_id().0, "free", "alice")
        .await
        .unwrap();
    let app = fauna_nest::build_router(state);
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    format!("http://{authority}")
}

/// Mint the kit while the owner can still sign in, then run the stolen-identity
/// ceremony through the FFI export over `store`. Returns the landed arm and the
/// registry the app would read afterwards.
async fn land_through_ffi(
    store: Arc<dyn FfiSecretStore>,
) -> (FfiLandedSuccession, Arc<FfiAccountRegistry>) {
    let base = start().await;
    let signed_in = connected_client(&base, old()).await;
    fauna_client_recovery::create_kit_with_root(
        &fauna_client_recovery::RecoveryClient::new(Arc::clone(&signed_in)),
        &old(),
        None,
        RecoveryKey::from_bytes(ROOT),
        &[],
    )
    .await
    .expect("the kit ceremony lands");
    drop(signed_in);

    let accounts = FfiAccountRegistry::new(store);
    let scratch = tempfile::tempdir().unwrap();
    let outcome = succession_succeed_stolen_locked(
        base,
        OLD_SEED.to_vec(),
        hex::encode(ROOT),
        Arc::clone(&accounts),
        Arc::new(ScratchMls(scratch.path().join("mls"))),
        // The at-rest store root the departed-fleet read uses — never the
        // platform location from a test.
        Some(scratch.path().join("store").to_string_lossy().into_owned()),
    )
    .await
    .expect("the ceremony starts: the seed is 32 bytes");
    let landed = outcome
        .landed
        .unwrap_or_else(|| panic!("the ceremony must land: {}", outcome.kind));
    (landed, accounts)
}

#[tokio::test]
async fn a_seed_that_lands_is_reported_saved_and_the_predecessor_is_linked() {
    let (landed, accounts) = land_through_ffi(Arc::new(KeepingStore::default())).await;

    // The read-back is of the SUCCESSOR's actor, against the seed handed back.
    assert!(
        landed.persisted,
        "a successor seed that reads back from the account store is reported saved"
    );
    let stored = accounts
        .session_material(landed.new_actor_id_hex.clone())
        .expect("the successor's row is in the registry");
    assert_eq!(
        stored.secret_hex.as_str(),
        landed.successor_secret_hex.as_str()
    );

    // The landed arm recorded predecessor → successor before the call returned
    // — where a bound launch follows the account, and where the corpus re-seal
    // finds the retired identity's store and BackupKey.
    assert_eq!(
        accounts.predecessors_of(landed.new_actor_id_hex.clone()),
        vec![old().actor_id().to_hex()],
        "the driver links the predecessor to the successor on the landed arm"
    );
}

#[tokio::test]
async fn a_seed_that_does_not_land_is_never_reported_saved() {
    let (landed, _accounts) = land_through_ffi(Arc::new(VanishingStore)).await;

    // The account moved on the nest all the same; the seed handed back is the
    // only copy, so the app must put it on screen — never say it is saved.
    assert!(
        !landed.persisted,
        "a successor seed whose write vanished is never reported saved"
    );
    assert_eq!(landed.successor_secret_hex.len(), 64);
}
