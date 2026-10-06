//! `fauna.sync.devices.p2p_participation.set` — the nest half of the
//! per-device p2p participation control (`docs/goal/behavior/p2p.md`
//! § Per-device participation): the **owner arm** can only brake a device of
//! the caller's own account and is refused on enable; the **self arm** (a
//! proof of possession by the row's own principal) records the device's
//! report and clears a pending brake only on an `off` report; `devices.list`
//! carries both columns. Handler-level, over the production router and an
//! in-memory nest, exactly as `conformance_device_revocation.rs` drives its
//! siblings.

mod common;

use std::sync::Arc;

use bytes::Bytes;
use fauna_core::identity::ActorKeypair;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest::{db::CacheDb, sync_handlers};
use fauna_protocol::encode_canonical;
use fauna_protocol::sync::{
    KIND_SYNC_DEVICES_P2P_PARTICIPATION_SET, SyncDeviceP2pParticipationSetReply,
    SyncDeviceP2pParticipationSetRequest, SyncDevicesListReply, SyncDevicesListRequest,
};

async fn nest() -> (RpcRouter, Arc<AppState>, ActorKeypair) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    sync_handlers::register_sync_handlers(&mut b);
    let account = ActorKeypair::generate();
    common::seed_dispatch_actor(&state.db, &account.actor_id().0).await;
    (b.build(), state, account)
}

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

/// Register a device row + attach a root-signed renewal grant — the row then
/// carries a principal, which is what the self arm proves possession of.
/// Returns that principal's signing key.
async fn enroll(
    router: &RpcRouter,
    state: &Arc<AppState>,
    account: &ActorKeypair,
    device_id_hex: &str,
) -> ed25519_dalek::SigningKey {
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
    ed25519_dalek::SigningKey::from_bytes(&seed)
}

async fn row(
    router: &RpcRouter,
    state: &Arc<AppState>,
    account: &ActorKeypair,
    device_id_hex: &str,
) -> fauna_protocol::sync::SyncDevice {
    let reply: SyncDevicesListReply = dispatch(
        router,
        state,
        account.actor_id().0,
        "fauna.sync.devices.list",
        SyncDevicesListRequest {
            extra: Default::default(),
        },
    )
    .await
    .expect("list");
    reply
        .devices
        .into_iter()
        .find(|d| d.device_id == device_id_hex)
        .expect("the row is listed")
}

fn owner_arm(device_id_hex: &str, participating: bool) -> SyncDeviceP2pParticipationSetRequest {
    SyncDeviceP2pParticipationSetRequest {
        device_id: device_id_hex.to_string(),
        participating,
        timestamp_ms: None,
        nonce: None,
        signature: None,
        extra: Default::default(),
    }
}

#[tokio::test]
async fn a_fresh_row_lists_no_report_and_no_pending_brake() {
    let (router, state, account) = nest().await;
    let device = fauna_core::hex32::encode(&[0x11u8; 32]);
    enroll(&router, &state, &account, &device).await;
    let listed = row(&router, &state, &account, &device).await;
    assert_eq!(listed.p2p_participation, None, "never reported");
    assert!(!listed.p2p_off_requested);
}

#[tokio::test]
async fn the_owner_arm_only_brakes_and_is_refused_on_enable() {
    let (router, state, account) = nest().await;
    let device = fauna_core::hex32::encode(&[0x22u8; 32]);
    enroll(&router, &state, &account, &device).await;

    let reply: SyncDeviceP2pParticipationSetReply = dispatch(
        &router,
        &state,
        account.actor_id().0,
        KIND_SYNC_DEVICES_P2P_PARTICIPATION_SET,
        owner_arm(&device, false),
    )
    .await
    .expect("the owner arm may ask a device to turn off");
    assert!(reply.off_requested, "the brake is pending");
    assert_eq!(
        reply.participating, None,
        "a brake is not a report: the device's own word stays untouched"
    );
    let listed = row(&router, &state, &account, &device).await;
    assert!(listed.p2p_off_requested);

    let err = dispatch::<_, SyncDeviceP2pParticipationSetReply>(
        &router,
        &state,
        account.actor_id().0,
        KIND_SYNC_DEVICES_P2P_PARTICIPATION_SET,
        owner_arm(&device, true),
    )
    .await
    .expect_err("no nest state may bring a listener up");
    assert_eq!(err.code, "fauna.sync.permission_denied");
    let listed = row(&router, &state, &account, &device).await;
    assert!(
        listed.p2p_off_requested,
        "the refused enable changed nothing"
    );
}

#[tokio::test]
async fn the_self_arm_reports_and_an_off_report_clears_the_brake() {
    let (router, state, account) = nest().await;
    let device = fauna_core::hex32::encode(&[0x33u8; 32]);
    let device_id = [0x33u8; 32];
    let principal = enroll(&router, &state, &account, &device).await;

    // A sibling brakes it.
    let _: SyncDeviceP2pParticipationSetReply = dispatch(
        &router,
        &state,
        account.actor_id().0,
        KIND_SYNC_DEVICES_P2P_PARTICIPATION_SET,
        owner_arm(&device, false),
    )
    .await
    .unwrap();

    // The device reports `on` — a report that raced the brake. The brake
    // stays pending for the device's next pass to fold.
    let reply: SyncDeviceP2pParticipationSetReply = dispatch(
        &router,
        &state,
        account.actor_id().0,
        KIND_SYNC_DEVICES_P2P_PARTICIPATION_SET,
        fauna_client_sync::build_p2p_participation_report(
            &account.actor_id().0,
            &principal,
            &device_id,
            true,
        ),
    )
    .await
    .expect("the row's own principal reports");
    assert_eq!(reply.participating, Some(true));
    assert!(
        reply.off_requested,
        "an `on` report never clears a pending brake"
    );

    // The device folds it and reports `off` — that clears the brake.
    let reply: SyncDeviceP2pParticipationSetReply = dispatch(
        &router,
        &state,
        account.actor_id().0,
        KIND_SYNC_DEVICES_P2P_PARTICIPATION_SET,
        fauna_client_sync::build_p2p_participation_report(
            &account.actor_id().0,
            &principal,
            &device_id,
            false,
        ),
    )
    .await
    .expect("the row's own principal reports");
    assert_eq!(reply.participating, Some(false));
    assert!(!reply.off_requested, "an `off` report clears the brake");
    let listed = row(&router, &state, &account, &device).await;
    assert_eq!(listed.p2p_participation, Some(false));
    assert!(!listed.p2p_off_requested);
}

#[tokio::test]
async fn a_report_signed_by_another_key_is_refused_and_changes_nothing() {
    let (router, state, account) = nest().await;
    let device = fauna_core::hex32::encode(&[0x44u8; 32]);
    let device_id = [0x44u8; 32];
    let _principal = enroll(&router, &state, &account, &device).await;
    let stranger = ed25519_dalek::SigningKey::from_bytes(&[0x77u8; 32]);

    let err = dispatch::<_, SyncDeviceP2pParticipationSetReply>(
        &router,
        &state,
        account.actor_id().0,
        KIND_SYNC_DEVICES_P2P_PARTICIPATION_SET,
        fauna_client_sync::build_p2p_participation_report(
            &account.actor_id().0,
            &stranger,
            &device_id,
            false,
        ),
    )
    .await
    .expect_err("only the row's own principal may report");
    assert_eq!(err.code, "fauna.sync.permission_denied");
    let listed = row(&router, &state, &account, &device).await;
    assert_eq!(listed.p2p_participation, None, "nothing was recorded");
}

#[tokio::test]
async fn a_captured_report_cannot_be_replayed_with_its_verdict_flipped() {
    let (router, state, account) = nest().await;
    let device = fauna_core::hex32::encode(&[0x55u8; 32]);
    let device_id = [0x55u8; 32];
    let principal = enroll(&router, &state, &account, &device).await;

    let mut captured = fauna_client_sync::build_p2p_participation_report(
        &account.actor_id().0,
        &principal,
        &device_id,
        true,
    );
    // The verdict is inside the signed bytes: flipping the field alone
    // invalidates the signature.
    captured.participating = false;
    let err = dispatch::<_, SyncDeviceP2pParticipationSetReply>(
        &router,
        &state,
        account.actor_id().0,
        KIND_SYNC_DEVICES_P2P_PARTICIPATION_SET,
        captured,
    )
    .await
    .expect_err("a flipped verdict fails the signature");
    assert_eq!(err.code, "fauna.sync.permission_denied");
}

#[tokio::test]
async fn a_stranger_learns_only_not_found_and_a_principal_less_row_cannot_self_report() {
    let (router, state, account) = nest().await;
    let device = fauna_core::hex32::encode(&[0x66u8; 32]);
    enroll(&router, &state, &account, &device).await;

    // Another account, probing this account's device id.
    let other = ActorKeypair::generate();
    common::seed_dispatch_actor(&state.db, &other.actor_id().0).await;
    let err = dispatch::<_, SyncDeviceP2pParticipationSetReply>(
        &router,
        &state,
        other.actor_id().0,
        KIND_SYNC_DEVICES_P2P_PARTICIPATION_SET,
        owner_arm(&device, false),
    )
    .await
    .expect_err("not this account's device");
    assert_eq!(err.code, "fauna.sync.not_found");

    // A row registered with no grant carries no principal: a self report
    // has nothing to verify against and is refused, whatever it signs with.
    let bare = fauna_core::hex32::encode(&[0x67u8; 32]);
    let _: fauna_protocol::sync::SyncRegisterReply = dispatch(
        &router,
        &state,
        account.actor_id().0,
        "fauna.sync.register",
        fauna_protocol::sync::SyncRegisterRequest {
            device_id: bare.clone(),
            label: "bare".to_string(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let key = ed25519_dalek::SigningKey::from_bytes(&[0x68u8; 32]);
    let err = dispatch::<_, SyncDeviceP2pParticipationSetReply>(
        &router,
        &state,
        account.actor_id().0,
        KIND_SYNC_DEVICES_P2P_PARTICIPATION_SET,
        fauna_client_sync::build_p2p_participation_report(
            &account.actor_id().0,
            &key,
            &[0x67u8; 32],
            false,
        ),
    )
    .await
    .expect_err("no principal to self-report with");
    assert_eq!(err.code, "fauna.sync.permission_denied");
    // The owner arm still brakes it.
    let reply: SyncDeviceP2pParticipationSetReply = dispatch(
        &router,
        &state,
        account.actor_id().0,
        KIND_SYNC_DEVICES_P2P_PARTICIPATION_SET,
        owner_arm(&bare, false),
    )
    .await
    .unwrap();
    assert!(reply.off_requested);
}
