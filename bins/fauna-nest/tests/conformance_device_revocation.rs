//! deleting a device ends EVERY authority its renewal grant
//! conferred: the grant, the ability to re-attach it, and every session it
//! already minted (`docs/goal/architecture/apps/sync-agent.md` § Credential
//! model — the "revocation is the control" sentence, made true).
//!
//! The chain under test is the production one end to end: a root-signed
//! device grant (`common::fresh_device_grant`), the real
//! `fauna.sync.register` / `fauna.sync.device_grant.register` handlers, the
//! real `fauna.auth.device_handshake` mint (`auth_core::device_auth_core` —
//! the handler is a thin wrapper over it), the real
//! `fauna.sync.devices.delete` handler, and the real `TokenStore` every
//! authenticated call validates against. Nothing stubbed.
//!
//! Tier: tier_3 (real handlers + real token store + real DB). Every assertion
//! is on latency-independent state (e2e convention 14): no sleeps, no
//! wall-clock asserts — expiry never participates; revocation is what is
//! being proven.

mod common;

use std::sync::Arc;

use bytes::Bytes;
use ed25519_dalek::Signer;
use fauna_core::identity::ActorKeypair;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest::{auth_core, db::CacheDb, session_handlers, sync_handlers};
use fauna_protocol::encode_canonical;

/// One in-process nest with the sync + session control surfaces registered,
/// plus the account whose root key signs the grants.
async fn nest() -> (RpcRouter, Arc<AppState>, ActorKeypair) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    sync_handlers::register_sync_handlers(&mut b);
    session_handlers::register_sessions_handlers(&mut b);
    let account = ActorKeypair::generate();
    common::seed_dispatch_actor(&state.db, &account.actor_id().0).await;
    (b.build(), state, account)
}

/// Dispatch one kind through the real registered handler, as the actor.
async fn dispatch<Req, Reply>(
    router: &RpcRouter,
    state: &Arc<AppState>,
    actor: [u8; 32],
    kind: &str,
    payload: Req,
) -> Result<Reply, fauna_protocol::RpcError>
where
    Req: serde::Serialize,
    Reply: serde::de::DeserializeOwned,
{
    let meta = router.kind_meta(kind).expect("kind registered");
    let bytes = Bytes::from(encode_canonical(&payload).unwrap().to_vec());
    let reply = (meta.handler)(Arc::clone(state), actor, bytes).await?;
    Ok(fauna_protocol::decode_strict(&reply).expect("reply decodes"))
}

/// Register a device row + attach a root-signed renewal grant, exactly as the
/// production provision does. Returns the renewal signing seed.
async fn enroll(
    router: &RpcRouter,
    state: &Arc<AppState>,
    account: &ActorKeypair,
    device_id_hex: &str,
) -> [u8; 32] {
    let _: fauna_protocol::sync::SyncRegisterReply = dispatch(
        router,
        state,
        account.actor_id().0,
        "fauna.sync.register",
        fauna_protocol::sync::SyncRegisterRequest {
            device_id: device_id_hex.to_string(),
            label: "test agent".to_string(),
            ..Default::default()
        },
    )
    .await
    .expect("device registers");
    let (grant, seed) = common::fresh_device_grant(account);
    let reply: fauna_protocol::sync::DeviceGrantRegisterReply = dispatch(
        router,
        state,
        account.actor_id().0,
        "fauna.sync.device_grant.register",
        fauna_protocol::sync::DeviceGrantRegisterRequest {
            device_id: device_id_hex.to_string(),
            authorization: grant,
            extra: Default::default(),
        },
    )
    .await
    .expect("grant registers");
    assert!(reply.registered);
    seed
}

/// Mint a bearer over the production handshake path with the renewal seed —
/// the app-dead self-renewal every sync agent runs.
async fn mint_via_handshake(
    state: &Arc<AppState>,
    account: &ActorKeypair,
    renewal_seed: &[u8; 32],
    nonce: &[u8],
) -> Result<auth_core::TokenMint, auth_core::AuthError> {
    let signing = ed25519_dalek::SigningKey::from_bytes(renewal_seed);
    let device_key = signing.verifying_key().to_bytes();
    let now_ms = fauna_core::data::Timestamp::now_millis();
    let nest_id = state.bound_identity();
    let msg = fauna_protocol::auth::device_handshake_signed_message(
        &account.actor_id().0,
        &device_key,
        now_ms,
        &nest_id,
        nonce,
    );
    let sig = signing.sign(&msg);
    auth_core::device_auth_core(
        state,
        &account.actor_id_hex(),
        &fauna_core::hex32::encode(&device_key),
        now_ms,
        &hex_encode64(&sig.to_bytes()),
        nonce,
        &fauna_core::hex32::encode(&nest_id),
    )
    .await
}

fn hex_encode64(bytes: &[u8; 64]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `fauna.sync.device_grant.revoke` over the **app/user arm** — an
/// authenticated session of the account, no proof of possession.
async fn revoke_grant_by_session(
    router: &RpcRouter,
    state: &Arc<AppState>,
    account: &ActorKeypair,
    device_key_hex: &str,
) -> Result<fauna_protocol::sync::DeviceGrantRevokeReply, fauna_protocol::RpcError> {
    dispatch(
        router,
        state,
        account.actor_id().0,
        "fauna.sync.device_grant.revoke",
        fauna_protocol::sync::DeviceGrantRevokeRequest {
            device_key: device_key_hex.to_string(),
            timestamp_ms: None,
            nonce: None,
            signature: None,
            extra: Default::default(),
        },
    )
    .await
}

/// `fauna.sync.device_grant.revoke` over the **self arm** — the agent's own
/// retirement: a proof-of-possession signature by the very key being retired.
/// `sign_domain` selects which domain-tagged message is signed, so a test can
/// present a genuine `device_handshake` signature here and prove the domain
/// separation refuses it.
async fn revoke_grant_by_self(
    router: &RpcRouter,
    state: &Arc<AppState>,
    account: &ActorKeypair,
    renewal_seed: &[u8; 32],
    nonce: &[u8],
    sign_as_handshake_instead: bool,
) -> Result<fauna_protocol::sync::DeviceGrantRevokeReply, fauna_protocol::RpcError> {
    let signing = ed25519_dalek::SigningKey::from_bytes(renewal_seed);
    let device_key = signing.verifying_key().to_bytes();
    let now_ms = fauna_core::data::Timestamp::now_millis();
    let msg = if sign_as_handshake_instead {
        fauna_protocol::auth::device_handshake_signed_message(
            &account.actor_id().0,
            &device_key,
            now_ms,
            &state.bound_identity(),
            nonce,
        )
    } else {
        fauna_protocol::auth::device_grant_revoke_signed_message(
            &account.actor_id().0,
            &device_key,
            now_ms,
            nonce,
        )
    };
    let sig = signing.sign(&msg);
    dispatch(
        router,
        state,
        account.actor_id().0,
        "fauna.sync.device_grant.revoke",
        fauna_protocol::sync::DeviceGrantRevokeRequest {
            device_key: fauna_core::hex32::encode(&device_key),
            timestamp_ms: Some(now_ms),
            nonce: Some(hex_encode_bytes(nonce)),
            signature: Some(hex_encode64(&sig.to_bytes())),
            extra: Default::default(),
        },
    )
    .await
}

fn hex_encode_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The device ids the account's `sync_devices` rows carry right now — the
/// devices-list projection every app's devices UI renders.
async fn listed_device_ids(
    router: &RpcRouter,
    state: &Arc<AppState>,
    account: &ActorKeypair,
) -> Vec<String> {
    let reply: fauna_protocol::sync::SyncDevicesListReply = dispatch(
        router,
        state,
        account.actor_id().0,
        "fauna.sync.devices.list",
        fauna_protocol::sync::SyncDevicesListRequest {
            extra: Default::default(),
        },
    )
    .await
    .expect("devices list");
    reply.devices.into_iter().map(|d| d.device_id).collect()
}

async fn delete_device(
    router: &RpcRouter,
    state: &Arc<AppState>,
    account: &ActorKeypair,
    device_id_hex: &str,
) {
    let reply: fauna_protocol::sync::SyncDeviceDeleteReply = dispatch(
        router,
        state,
        account.actor_id().0,
        "fauna.sync.devices.delete",
        fauna_protocol::sync::SyncDeviceDeleteRequest {
            device_id: device_id_hex.to_string(),
            extra: Default::default(),
        },
    )
    .await
    .expect("device deletes");
    assert!(reply.deleted);
}

/// The finding's core: the user's revocation gesture must end the sessions the
/// grant already minted — not only stop future mints.
#[tokio::test(flavor = "multi_thread")]
async fn deleting_a_device_revokes_the_sessions_its_grant_minted() {
    let (router, state, account) = nest().await;
    let device_id_hex = fauna_core::hex32::encode(&[0x11; 32]);
    let seed = enroll(&router, &state, &account, &device_id_hex).await;

    let minted = mint_via_handshake(&state, &account, &seed, b"nonce-mint-1")
        .await
        .expect("the enrolled grant mints");
    assert_eq!(
        state.auth.token_store.validate(&minted.token).await,
        Some(account.actor_id()),
        "the minted bearer authenticates before the delete"
    );

    // A direct app sign-in session of the same actor, minted by no device —
    // the delete must NOT touch it (deleting a sync device is not a sign-out).
    let app_session = state
        .auth
        .token_store
        .insert(account.actor_id(), auth_core::TOKEN_TTL_SECS)
        .await;

    delete_device(&router, &state, &account, &device_id_hex).await;

    assert_eq!(
        state.auth.token_store.validate(&minted.token).await,
        None,
        "the device-minted bearer is dead on its very next authenticated call \
         — a revoked device must not keep a live session for up to an hour"
    );
    assert_eq!(
        state.auth.token_store.validate(&app_session).await,
        Some(account.actor_id()),
        "an app session the device did not mint survives the device delete"
    );
}

/// The finding's steps 4–5: with any still-live full-actor session, replaying
/// the SAME root-signed grant after the delete must not restore renewal — the
/// nest must remember the revocation.
#[tokio::test(flavor = "multi_thread")]
async fn a_post_delete_replay_of_the_same_grant_does_not_restore_renewal() {
    let (router, state, account) = nest().await;
    let device_id_hex = fauna_core::hex32::encode(&[0x22; 32]);

    // Enroll, capturing the exact signed grant wire the thief would replay.
    let _: fauna_protocol::sync::SyncRegisterReply = dispatch(
        &router,
        &state,
        account.actor_id().0,
        "fauna.sync.register",
        fauna_protocol::sync::SyncRegisterRequest {
            device_id: device_id_hex.clone(),
            label: "test agent".to_string(),
            ..Default::default()
        },
    )
    .await
    .expect("device registers");
    let (grant, seed) = common::fresh_device_grant(&account);
    let reply: fauna_protocol::sync::DeviceGrantRegisterReply = dispatch(
        &router,
        &state,
        account.actor_id().0,
        "fauna.sync.device_grant.register",
        fauna_protocol::sync::DeviceGrantRegisterRequest {
            device_id: device_id_hex.clone(),
            authorization: grant.clone(),
            extra: Default::default(),
        },
    )
    .await
    .expect("grant registers");
    assert!(reply.registered);

    delete_device(&router, &state, &account, &device_id_hex).await;

    // The replay: re-create the row (fauna.sync.register is an ordinary
    // authenticated kind with a client-chosen device_id), then re-attach the
    // SAME genuinely root-signed grant. Every one of the register handler's
    // four checks passes on the grant itself — the refusal must come from the
    // nest's memory of the revocation.
    let _: fauna_protocol::sync::SyncRegisterReply = dispatch(
        &router,
        &state,
        account.actor_id().0,
        "fauna.sync.register",
        fauna_protocol::sync::SyncRegisterRequest {
            device_id: device_id_hex.clone(),
            label: "re-registered".to_string(),
            ..Default::default()
        },
    )
    .await
    .expect("re-register itself is an ordinary row create");
    let replay: Result<fauna_protocol::sync::DeviceGrantRegisterReply, _> = dispatch(
        &router,
        &state,
        account.actor_id().0,
        "fauna.sync.device_grant.register",
        fauna_protocol::sync::DeviceGrantRegisterRequest {
            device_id: device_id_hex.clone(),
            authorization: grant,
            extra: Default::default(),
        },
    )
    .await;
    let err = replay.expect_err(
        "re-attaching the revoked grant must be refused — the nest keeps a \
         memory of the revocation, so the credential cannot heal itself",
    );
    // The refusal's CODE is load-bearing: it is the succession
    // trigger's evidence — a ceremony-capable sign-in seeing exactly this
    // code mints a successor principal, and anything else (a malformed
    // grant, an older nest) must NOT fire it. The two mutation tests below
    // pin the never-fires direction.
    assert_eq!(
        err.code,
        fauna_protocol::RpcError::CODE_SYNC_DEVICE_GRANT_REVOKED,
        "the tombstone refusal must carry the DISTINCT typed code"
    );

    // And the handshake with the revoked key mints nothing, whichever way the
    // row ended up.
    let mint = mint_via_handshake(&state, &account, &seed, b"nonce-replay").await;
    assert!(
        mint.is_err(),
        "the revoked renewal key must never mint again"
    );
}

/// The succession trigger must be UNFORGEABLE by a merely-bad grant: neither
/// a capability-less grant nor a signature-tampered one may answer the
/// `grant_revoked` code — those stay generic `invalid_grant`, or a rotation
/// would fire on a bug (mutation direction; the positive
/// direction is the code assertion in the replay test above).
#[tokio::test(flavor = "multi_thread")]
async fn a_bad_grant_never_answers_the_succession_trigger_code() {
    let (router, state, account) = nest().await;
    let device_id_hex = fauna_core::hex32::encode(&[0x44; 32]);
    let _: fauna_protocol::sync::SyncRegisterReply = dispatch(
        &router,
        &state,
        account.actor_id().0,
        "fauna.sync.register",
        fauna_protocol::sync::SyncRegisterRequest {
            device_id: device_id_hex.clone(),
            label: "test agent".to_string(),
            ..Default::default()
        },
    )
    .await
    .expect("device registers");

    // Arm 1: a root-signed grant WITHOUT the RenewBearer capability.
    let bare = fauna_core::data::DeviceAuthorization {
        actor_id: account.actor_id(),
        device_key: [0x55; 32],
        capabilities: vec![],
        created_at: fauna_core::data::Timestamp(0),
        expires_at: None,
    };
    let (bare_bytes, bare_env) =
        fauna_core::encoding::sign_envelope(&account, &bare).expect("sign");
    let bare_wire = fauna_core::encoding::EmbedAsBytes::from_signed(bare_bytes, bare_env);
    let no_capability: Result<fauna_protocol::sync::DeviceGrantRegisterReply, _> = dispatch(
        &router,
        &state,
        account.actor_id().0,
        "fauna.sync.device_grant.register",
        fauna_protocol::sync::DeviceGrantRegisterRequest {
            device_id: device_id_hex.clone(),
            authorization: bare_wire,
            extra: Default::default(),
        },
    )
    .await;
    let err = no_capability.expect_err("a capability-less grant refuses");
    assert_ne!(
        err.code,
        fauna_protocol::RpcError::CODE_SYNC_DEVICE_GRANT_REVOKED,
        "a capability refusal must never read as the succession trigger"
    );

    // Arm 2: a signature-tampered grant (another identity's signature over
    // this account's payload — verification fails).
    let intruder = fauna_core::identity::ActorKeypair::from_secret([0x66; 32]);
    let forged = fauna_core::data::DeviceAuthorization {
        actor_id: account.actor_id(),
        device_key: [0x77; 32],
        capabilities: vec![fauna_core::data::Capability::RenewBearer],
        created_at: fauna_core::data::Timestamp(0),
        expires_at: None,
    };
    let (forged_bytes, forged_env) =
        fauna_core::encoding::sign_envelope(&intruder, &forged).expect("sign");
    let forged_wire = fauna_core::encoding::EmbedAsBytes::from_signed(forged_bytes, forged_env);
    let bad_sig: Result<fauna_protocol::sync::DeviceGrantRegisterReply, _> = dispatch(
        &router,
        &state,
        account.actor_id().0,
        "fauna.sync.device_grant.register",
        fauna_protocol::sync::DeviceGrantRegisterRequest {
            device_id: device_id_hex,
            authorization: forged_wire,
            extra: Default::default(),
        },
    )
    .await;
    let err = bad_sig.expect_err("a signature-tampered grant refuses");
    assert_ne!(
        err.code,
        fauna_protocol::RpcError::CODE_SYNC_DEVICE_GRANT_REVOKED,
        "a signature refusal must never read as the succession trigger"
    );
}

/// The over-blocking guard: revocation is per-grant, never per-machine. A
/// legitimate re-enrollment of the same device id with a FRESH device keypair
/// (a successor principal is exactly this) registers and mints exactly as a
/// first enrollment does.
#[tokio::test(flavor = "multi_thread")]
async fn a_fresh_grant_re_enrolls_the_same_device_after_a_delete() {
    let (router, state, account) = nest().await;
    let device_id_hex = fauna_core::hex32::encode(&[0x33; 32]);

    let _old_seed = enroll(&router, &state, &account, &device_id_hex).await;
    delete_device(&router, &state, &account, &device_id_hex).await;

    let fresh_seed = enroll(&router, &state, &account, &device_id_hex).await;
    let minted = mint_via_handshake(&state, &account, &fresh_seed, b"nonce-fresh")
        .await
        .expect("a fresh root-signed grant on the same device id mints");
    assert_eq!(
        state.auth.token_store.validate(&minted.token).await,
        Some(account.actor_id()),
        "the re-enrolled device's bearer authenticates"
    );
}

/// Remedy (3) — the audit surface: `fauna.sessions.list` distinguishes a
/// device-grant renewal from an app sign-in by carrying the minting device
/// key, so the surface `sync-agent.md` points at for these mints can answer
/// the question it is pointed at for.
#[tokio::test(flavor = "multi_thread")]
async fn sessions_list_names_the_minting_device() {
    let (router, state, account) = nest().await;
    let device_id_hex = fauna_core::hex32::encode(&[0x44; 32]);
    let seed = enroll(&router, &state, &account, &device_id_hex).await;

    let minted = mint_via_handshake(&state, &account, &seed, b"nonce-list")
        .await
        .expect("the enrolled grant mints");
    let _app_session = state
        .auth
        .token_store
        .insert(account.actor_id(), auth_core::TOKEN_TTL_SECS)
        .await;

    let signing = ed25519_dalek::SigningKey::from_bytes(&seed);
    let device_key_hex = fauna_core::hex32::encode(&signing.verifying_key().to_bytes());

    let sessions: fauna_protocol::sessions::SessionsListReply = dispatch(
        &router,
        &state,
        account.actor_id().0,
        "fauna.sessions.list",
        fauna_protocol::sessions::SessionsListRequest {
            extra: Default::default(),
        },
    )
    .await
    .expect("sessions list");
    assert_eq!(sessions.sessions.len(), 2);
    let device_minted = sessions
        .sessions
        .iter()
        .find(|s| s.token_id == minted.token_id)
        .expect("the device-minted session is listed");
    assert_eq!(
        device_minted.minted_by_device.as_deref(),
        Some(device_key_hex.as_str()),
        "the device-grant mint is labeled with its minting device key"
    );
    let app_minted = sessions
        .sessions
        .iter()
        .find(|s| s.token_id != minted.token_id)
        .expect("the app session is listed");
    assert_eq!(
        app_minted.minted_by_device, None,
        "a direct sign-in carries no minting device"
    );
}

// ── fauna.sync.device_grant.revoke — the ruled grant retirement ──────────────
//
// `sync-agent.md` § Credential model → the RULED 2026-08-15 block. The kind
// factors the triple above out of the device delete so a *credential* can be
// retired without deleting the *device* that carries it. Since the RULED
// 2026-09-28 block (one credential per machine) its live caller is the
// sign-out's retirement of the machine's store principal.

/// The self arm — the agent's own retirement. Proof of possession of the very
/// key being retired is the whole authorization story: it destroys only the
/// caller's own credential, so it confers no authority over anything else.
#[tokio::test(flavor = "multi_thread")]
async fn an_agent_retires_its_own_grant_by_proving_possession_of_the_key() {
    let (router, state, account) = nest().await;
    let device_id_hex = fauna_core::hex32::encode(&[0x55; 32]);
    let seed = enroll(&router, &state, &account, &device_id_hex).await;

    let minted = mint_via_handshake(&state, &account, &seed, b"nonce-self-1")
        .await
        .expect("the enrolled grant mints");
    assert_eq!(
        state.auth.token_store.validate(&minted.token).await,
        Some(account.actor_id()),
        "the enrolled grant's bearer authenticates before the retirement"
    );

    let reply = revoke_grant_by_self(&router, &state, &account, &seed, b"nonce-self-2", false)
        .await
        .expect("the self arm authorizes");
    assert!(reply.revoked, "a stored grant was cleared");

    // The three halves of the triple, each asserted on its own observable.
    assert!(
        mint_via_handshake(&state, &account, &seed, b"nonce-self-3")
            .await
            .is_err(),
        "the retired key must never mint again"
    );
    assert_eq!(
        state.auth.token_store.validate(&minted.token).await,
        None,
        "the sessions the retired key already minted die with it, rather than \
         living out their own expiry"
    );
    assert!(
        listed_device_ids(&router, &state, &account)
            .await
            .contains(&device_id_hex),
        "the DEVICE survives its grant's retirement — this kind is not \
         fauna.sync.devices.delete, and the row it leaves alone is the \
         machine's real sync device (label, folder memberships, the id every \
         engine presents)"
    );
}

/// The app/user arm — an authenticated session of the owning account, no proof
/// of possession. This is the arm the signed-out reconcile's nest-side revoke
/// has been waiting for (§ Credential model, *Not in scope, deliberately*).
#[tokio::test(flavor = "multi_thread")]
async fn an_account_session_retires_a_grant_without_proving_possession() {
    let (router, state, account) = nest().await;
    let device_id_hex = fauna_core::hex32::encode(&[0x66; 32]);
    let seed = enroll(&router, &state, &account, &device_id_hex).await;
    let device_key_hex = fauna_core::hex32::encode(
        &ed25519_dalek::SigningKey::from_bytes(&seed)
            .verifying_key()
            .to_bytes(),
    );

    let reply = revoke_grant_by_session(&router, &state, &account, &device_key_hex)
        .await
        .expect("an account session authorizes");
    assert!(reply.revoked);
    assert!(
        mint_via_handshake(&state, &account, &seed, b"nonce-session")
            .await
            .is_err(),
        "the retired key must never mint again"
    );
}

/// Not-found is SUCCESS, not an error — the ruling's evidence bound. The agent
/// retries after every principal renewal, and the user may have deleted the
/// device row first; a not-found answer is what lets that loop terminate
/// instead of spinning forever on a grant nobody will ever return.
#[tokio::test(flavor = "multi_thread")]
async fn retiring_a_grant_that_is_already_gone_answers_success() {
    let (router, state, account) = nest().await;
    let device_id_hex = fauna_core::hex32::encode(&[0x77; 32]);
    let seed = enroll(&router, &state, &account, &device_id_hex).await;

    let first = revoke_grant_by_self(&router, &state, &account, &seed, b"nonce-again-1", false)
        .await
        .expect("the first retirement authorizes");
    assert!(first.revoked);

    let second = revoke_grant_by_self(&router, &state, &account, &seed, b"nonce-again-2", false)
        .await
        .expect("a repeat retirement is success, not an error");
    assert!(
        !second.revoked,
        "the second answer reports nothing left to clear — and is still Ok, \
         which is what terminates the agent's retry loop"
    );

    // A key that was never registered at all answers the same way.
    let never = revoke_grant_by_session(
        &router,
        &state,
        &account,
        &fauna_core::hex32::encode(
            &ed25519_dalek::SigningKey::from_bytes(&[0x99; 32])
                .verifying_key()
                .to_bytes(),
        ),
    )
    .await
    .expect("an unknown key is not an error either");
    assert!(!never.revoked);
}

/// The tombstone's own purpose, from the retirement side: after the agent
/// retires its enrolled grant, replaying the SAME root-signed authorization must
/// not restore renewal. Without this, retirement would be undoable by anyone
/// holding the (still genuinely root-signed) old grant wire — the hardening's hole, re-entered through the narrower kind.
#[tokio::test(flavor = "multi_thread")]
async fn a_post_retirement_replay_of_the_same_grant_does_not_restore_renewal() {
    let (router, state, account) = nest().await;
    let device_id_hex = fauna_core::hex32::encode(&[0xdd; 32]);

    let _: fauna_protocol::sync::SyncRegisterReply = dispatch(
        &router,
        &state,
        account.actor_id().0,
        "fauna.sync.register",
        fauna_protocol::sync::SyncRegisterRequest {
            device_id: device_id_hex.clone(),
            label: "test agent".to_string(),
            ..Default::default()
        },
    )
    .await
    .expect("device registers");
    let (grant, seed) = common::fresh_device_grant(&account);
    let _: fauna_protocol::sync::DeviceGrantRegisterReply = dispatch(
        &router,
        &state,
        account.actor_id().0,
        "fauna.sync.device_grant.register",
        fauna_protocol::sync::DeviceGrantRegisterRequest {
            device_id: device_id_hex.clone(),
            authorization: grant.clone(),
            extra: Default::default(),
        },
    )
    .await
    .expect("grant registers");

    revoke_grant_by_self(&router, &state, &account, &seed, b"nonce-replay-1", false)
        .await
        .expect("retirement authorizes");

    let replay: Result<fauna_protocol::sync::DeviceGrantRegisterReply, _> = dispatch(
        &router,
        &state,
        account.actor_id().0,
        "fauna.sync.device_grant.register",
        fauna_protocol::sync::DeviceGrantRegisterRequest {
            device_id: device_id_hex.clone(),
            authorization: grant,
            extra: Default::default(),
        },
    )
    .await;
    assert!(
        replay.is_err(),
        "re-attaching the retired grant must be refused — the retirement \
         writes the same revocation memory the device delete does"
    );
    assert!(
        mint_via_handshake(&state, &account, &seed, b"nonce-replay-2")
            .await
            .is_err(),
        "and the retired key mints nothing however its row ended up"
    );
}

/// The tombstone is written even when there was nothing to clear — and that
/// is not defensive tidiness, it is the terminal state the caller was promised.
/// A grant can be root-signed and in the wild while stored nowhere (a provision
/// whose `sync.register` landed and whose `device_grant.register` did not; a
/// row a device delete already took). Tombstoning only on a successful clear
/// would answer such a caller "revoked" while leaving the credential
/// re-attachable by anyone holding its wire.
#[tokio::test(flavor = "multi_thread")]
async fn retiring_an_unstored_grant_still_forecloses_it() {
    let (router, state, account) = nest().await;
    let device_id_hex = fauna_core::hex32::encode(&[0xee; 32]);

    let _: fauna_protocol::sync::SyncRegisterReply = dispatch(
        &router,
        &state,
        account.actor_id().0,
        "fauna.sync.register",
        fauna_protocol::sync::SyncRegisterRequest {
            device_id: device_id_hex.clone(),
            label: "test agent".to_string(),
            ..Default::default()
        },
    )
    .await
    .expect("device registers");
    // The grant exists and is genuinely root-signed — it was simply never
    // stored (the second leg of the provision never landed).
    let (grant, seed) = common::fresh_device_grant(&account);
    let device_key_hex = fauna_core::hex32::encode(
        &ed25519_dalek::SigningKey::from_bytes(&seed)
            .verifying_key()
            .to_bytes(),
    );

    let reply = revoke_grant_by_session(&router, &state, &account, &device_key_hex)
        .await
        .expect("retiring a key the nest never stored is success");
    assert!(!reply.revoked, "there was nothing stored to clear");

    let after: Result<fauna_protocol::sync::DeviceGrantRegisterReply, _> = dispatch(
        &router,
        &state,
        account.actor_id().0,
        "fauna.sync.device_grant.register",
        fauna_protocol::sync::DeviceGrantRegisterRequest {
            device_id: device_id_hex,
            authorization: grant,
            extra: Default::default(),
        },
    )
    .await;
    assert!(
        after.is_err(),
        "the retirement is durable even though it cleared nothing — otherwise \
         'this key never mints again' would hold only for keys that happened \
         to be stored at the moment it was asked for"
    );
}

/// The over-blocking guard, from the retirement side: a retired grant leaves
/// re-enrollment untouched, exactly as a device delete does. Every production
/// provision mints a FRESH keypair, and the tombstone keys on the revoked
/// public key alone — which is what lets the next sign-in after a sign-out's
/// retirement enroll a fresh principal on the same row.
#[tokio::test(flavor = "multi_thread")]
async fn a_retired_grant_does_not_block_re_enrollment() {
    let (router, state, account) = nest().await;
    let device_id_hex = fauna_core::hex32::encode(&[0x88; 32]);

    let old_seed = enroll(&router, &state, &account, &device_id_hex).await;
    revoke_grant_by_self(
        &router,
        &state,
        &account,
        &old_seed,
        b"nonce-reenroll",
        false,
    )
    .await
    .expect("retirement authorizes");

    let fresh_seed = enroll(&router, &state, &account, &device_id_hex).await;
    let minted = mint_via_handshake(&state, &account, &fresh_seed, b"nonce-reenroll-mint")
        .await
        .expect("a fresh grant on the same device row mints");
    assert_eq!(
        state.auth.token_store.validate(&minted.token).await,
        Some(account.actor_id()),
        "the re-provisioned device's bearer authenticates"
    );
    assert!(
        mint_via_handshake(&state, &account, &old_seed, b"nonce-reenroll-old")
            .await
            .is_err(),
        "while the retired key stays dead — the tombstone is per key, not per device"
    );
}

/// Domain separation, asserted rather than assumed: the retirement payload and
/// the mint payload are the same key over the same fields, so only the domain
/// tag tells them apart. Without separate tags, an eavesdropper could replay a
/// captured handshake signature as a retirement of the signer's own grant — a
/// denial of app-dead renewal for anyone who could observe one mint.
#[tokio::test(flavor = "multi_thread")]
async fn a_handshake_signature_cannot_be_replayed_as_a_retirement() {
    let (router, state, account) = nest().await;
    let device_id_hex = fauna_core::hex32::encode(&[0xaa; 32]);
    let seed = enroll(&router, &state, &account, &device_id_hex).await;

    let wrong_domain =
        revoke_grant_by_self(&router, &state, &account, &seed, b"nonce-domain", true).await;
    assert!(
        wrong_domain.is_err(),
        "a genuine signature by the right key over the MINT payload must not \
         authorize a retirement"
    );
    assert!(
        mint_via_handshake(&state, &account, &seed, b"nonce-domain-survives")
            .await
            .is_ok(),
        "and the grant it failed to retire is untouched"
    );
}

/// A present-but-invalid proof of possession REFUSES; it must never fall
/// through to the account-session arm that would have authorized the same call
/// with no proof at all. Otherwise the self arm would be decorative: a caller
/// could send garbage and still be let in by the other arm.
#[tokio::test(flavor = "multi_thread")]
async fn a_broken_proof_of_possession_refuses_rather_than_falling_through() {
    let (router, state, account) = nest().await;
    let device_id_hex = fauna_core::hex32::encode(&[0xbb; 32]);
    let seed = enroll(&router, &state, &account, &device_id_hex).await;
    let device_key = ed25519_dalek::SigningKey::from_bytes(&seed)
        .verifying_key()
        .to_bytes();

    // A signature by a DIFFERENT key over the right payload.
    let impostor = ed25519_dalek::SigningKey::from_bytes(&[0xcc; 32]);
    let now_ms = fauna_core::data::Timestamp::now_millis();
    let msg = fauna_protocol::auth::device_grant_revoke_signed_message(
        &account.actor_id().0,
        &device_key,
        now_ms,
        b"nonce-impostor",
    );
    let sig = impostor.sign(&msg);
    let refused: Result<fauna_protocol::sync::DeviceGrantRevokeReply, _> = dispatch(
        &router,
        &state,
        account.actor_id().0,
        "fauna.sync.device_grant.revoke",
        fauna_protocol::sync::DeviceGrantRevokeRequest {
            device_key: fauna_core::hex32::encode(&device_key),
            timestamp_ms: Some(now_ms),
            nonce: Some(hex_encode_bytes(b"nonce-impostor")),
            signature: Some(hex_encode64(&sig.to_bytes())),
            extra: Default::default(),
        },
    )
    .await;
    assert!(
        refused.is_err(),
        "a proof of possession that does not verify is a refusal — never a \
         silent downgrade to the session arm"
    );
    assert!(
        mint_via_handshake(&state, &account, &seed, b"nonce-impostor-survives")
            .await
            .is_ok(),
        "and the grant is untouched"
    );
}

// ── The one-row topology (`sync-agent-credentials.md` § Credential model, ────
//    the RULED 2026-09-28 block) ─────────────────────────────────────────────
//
// One credential per machine, on the machine's named row. Each test below
// asserts on production-observable state — what the devices list projects, and
// whether the key still mints — never on a column read, because the claim being
// made is about the user's gesture, not about SQL.

/// The gesture the whole topology exists for: with one row, the user's delete
/// on the row they recognise structurally ends the machine — the principal is
/// tombstoned and every session it minted dies with it.
#[tokio::test(flavor = "multi_thread")]
async fn deleting_the_named_row_ends_the_machine() {
    let (router, state, account) = nest().await;
    let named_hex = fauna_core::hex32::encode(&[0x42; 32]);
    let seed = enroll(&router, &state, &account, &named_hex).await;

    let mint = mint_via_handshake(&state, &account, &seed, b"nonce-pre-delete")
        .await
        .expect("renewal mints while the machine is enrolled");
    assert!(
        state.auth.token_store.validate(&mint.token).await.is_some(),
        "the bearer is live before the delete"
    );

    delete_device(&router, &state, &account, &named_hex).await;

    assert!(
        state.auth.token_store.validate(&mint.token).await.is_none(),
        "the user's delete ends the sessions the principal already minted"
    );
    assert!(
        mint_via_handshake(&state, &account, &seed, b"nonce-post-delete")
            .await
            .is_err(),
        "and app-dead renewal is over — one gesture ends the machine"
    );
    assert!(
        listed_device_ids(&router, &state, &account)
            .await
            .is_empty(),
        "no row survives the machine it named"
    );
}

/// A retirement clears the named row's grant columns and keeps the row, full
/// stop — decision 2's original guarantee, and since the RULED 2026-09-28
/// block's decision 4 the revoke's only shape (the placeholder-vacate arm went
/// with the placeholder row).
#[tokio::test(flavor = "multi_thread")]
async fn retiring_a_grant_on_the_named_row_keeps_the_row() {
    let (router, state, account) = nest().await;
    let named_hex = fauna_core::hex32::encode(&[0x45; 32]);
    let seed = enroll(&router, &state, &account, &named_hex).await;
    let key_hex = fauna_core::hex32::encode(
        &ed25519_dalek::SigningKey::from_bytes(&seed)
            .verifying_key()
            .to_bytes(),
    );

    let reply = revoke_grant_by_session(&router, &state, &account, &key_hex)
        .await
        .expect("retires");
    assert!(reply.revoked);
    assert_eq!(
        listed_device_ids(&router, &state, &account).await,
        vec![named_hex],
        "a grant retired off the user's own device row leaves the row, its \
         label and its memberships in place"
    );
}
