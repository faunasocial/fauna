//! The stolen-identity ceremony as a locked-out app runs it: the shared
//! composition `fauna_client_recovery::ceremony::succeed_stolen_identity`
//! against a real socket, with **no bearer anywhere** (`devices.md` § The two
//! panic buttons, point 5: the ceremony "must be reachable from a locked-out
//! app and must ride an anonymous connection"; § The locked state).
//!
//! `conformance_succession.rs` pins the nest half — a pre-built statement
//! lands through a lockout. What it cannot say is whether the *client's*
//! composition reaches that door without a session: until this file the three
//! native drivers each handed `succeed_with_held_kit` their signed-in
//! requester, which a lock has just torn down. This drives the one composition
//! every app's locked surface and Settings section call.
//!
//! The account is a **non-admin** one on purpose: admins are exempt from the
//! verify lock (`login.md` § Silent Challenge, ruled 2026-10-01), so a locked
//! admin would still sign in and the precondition below would prove nothing.

mod common;
use common::connected_client;

use std::sync::Arc;

use ed25519_dalek::SigningKey;
use fauna_client::NestClient;
use fauna_client_accounts::{AccountRegistry, InMemorySecretStore, SecretStore};
use fauna_client_recovery::ceremony::{StolenOutcome, succeed_stolen_identity};
use fauna_core::identity::ActorKeypair;
use fauna_core::recovery::RecoveryKey;
use fauna_nest::db::CacheDb;
use fauna_nest::nest_identity::NestIdentity;
use fauna_nest::routes::{AppState, RegistrationConfig};
use fauna_nest::state::AuthState;
use fauna_nest::token_store::TokenStore;

const OLD_SEED: [u8; 32] = [0x11; 32];
/// The RecoveryKey the kit registers — and, as bare 64-hex, the phrase the
/// user pastes into `recovery-entry-phrase-field`.
const ROOT: [u8; 32] = [0x21; 32];

fn old() -> ActorKeypair {
    ActorKeypair::from_secret(OLD_SEED)
}

/// A nest over a real socket with the handler families the ceremony reaches:
/// the pre-identity recovery plane, auth (challenge/verify), discovery.
async fn start() -> (String, Arc<AppState>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let authority = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    let key = SigningKey::from_bytes(&[0x51; 32]);
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    db.set_nest_keypair(key.as_bytes(), &key.verifying_key().to_bytes())
        .await
        .unwrap();
    let state = Arc::new(AppState {
        nest_identity: Arc::new(NestIdentity::from_seed(&[0x51; 32])),
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
    let app = fauna_nest::build_router(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (format!("http://{authority}"), state)
}

/// The refusal a sign-in as `keypair` meets, or `None` when it signs in. Every
/// app bearer mints through challenge/verify (`devices.md` § Implementation
/// status today, gap 3), so this is the `fauna.auth.verify` answer.
async fn sign_in_refusal(base: &str, keypair: ActorKeypair) -> Option<String> {
    let client = NestClient::new(base.to_string(), keypair);
    match client.connect().await {
        Ok(()) => None,
        Err(e) => Some(
            client
                .auth()
                .last_auth_refusal()
                .map_or_else(|| format!("transport: {e}"), |r| r.code),
        ),
    }
}

#[tokio::test]
async fn a_locked_out_app_reaches_its_successor_with_no_bearer() {
    let (base, state) = start().await;
    let old_id = old().actor_id().0;
    state
        .db
        .create_user(&old_id, "free", "alice")
        .await
        .unwrap();

    // The kit, minted while the owner could still sign in.
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

    // The lock, as either lockout kind leaves the account: `locked_until` set
    // and every live authority stripped.
    let until = fauna_core::data::Timestamp::now_secs() + 86_400;
    state
        .db
        .set_locked_until(&old_id, Some(until))
        .await
        .unwrap();
    state.revoke_actor_authority(&old_id).await;
    assert_eq!(
        sign_in_refusal(&base, old()).await.as_deref(),
        Some("fauna.auth.account_locked"),
        "precondition: verify refuses the locked (non-admin) account — no bearer can be minted"
    );

    // The ceremony, exactly as the locked surface calls it: the nest URL, the
    // seed this device still holds, the pasted kit. No session, no engine.
    let store: Arc<dyn SecretStore> = Arc::new(InMemorySecretStore::new());
    let accounts = AccountRegistry::new(store);
    let scratch = tempfile::tempdir().unwrap();
    let outcome = succeed_stolen_identity(
        &base,
        &old(),
        &hex::encode(ROOT),
        None,
        &accounts,
        |successor_hex| scratch.path().join(successor_hex).join("mls_state.db"),
    )
    .await;
    let StolenOutcome::Landed(landed) = outcome else {
        panic!(
            "the ceremony must land through the lock over an anonymous connection: {}",
            outcome.kind()
        );
    };

    // The successor seed was persisted by the composition itself, before
    // anything that could fail (`identity-succession.md` § Implementation
    // status today — client-only-resident key material).
    let stored = accounts
        .secrets(&landed.new_actor_id.to_hex())
        .expect("the successor seed reads back from the account store");
    assert_eq!(
        stored.secret_hex.as_str(),
        landed.successor_secret_hex.as_str()
    );

    // And the successor's first mint is not locked: the succession transaction
    // never inherits `locked_until` (`identity-succession.md` § Implementation
    // status today, *The transaction*).
    let successor = ActorKeypair::from_secret_hex(&landed.successor_secret_hex).unwrap();
    assert_eq!(
        sign_in_refusal(&base, successor).await,
        None,
        "the successor signs in at once — the thief's lock did not ride across"
    );
    // While the retired identity is now refused as superseded, not as locked:
    // the lock no longer describes this account at all.
    assert_eq!(
        sign_in_refusal(&base, old()).await.as_deref(),
        Some("fauna.auth.superseded")
    );
}
