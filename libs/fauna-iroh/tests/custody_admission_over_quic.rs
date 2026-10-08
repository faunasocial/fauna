//! W8 (account-data-plane.md § Workstreams) × iroh: the **custody grant** — the third admission arm (W8.1) — over
//! a real QUIC handshake; the custody twin of `peer_leg_assembly_over_quic.rs`
//! (charter § The custody grant + ceremony; W8-wide residual "the custody
//! QUIC transport twin"). The custody witness had only ever been exercised
//! over `MemTransport` (`fauna-sync-engine/tests/custody_convergence.rs` —
//! "no nest anywhere", but not QUIC), and the sibling assembly test proves
//! the `DeviceAuthorization` arm only. This file proves, against the SAME
//! runtime-assembled listener the sibling uses: an owner-signed custody
//! grant naming the channel-proven dialer key ADMITS; a stranger-signed
//! grant and an expired grant are REFUSED. (Row transfer over real QUIC is
//! `peer_sync_over_quic.rs`'s existing proof, and custody row transfer is
//! `custody_convergence.rs`'s — this file deliberately stops at the
//! admission verdicts, the same line its sibling draws.)
//!
//! The nest requester is the sibling's fake answering ONLY `fauna.nest.info`
//! (the brake gate's evidence) — the runtime's pump absorbs every other
//! step, which doubles as a proof the custody serve does not depend on any
//! other nest leg.
//!
//! Latency-independent throughout (convention 14): the bind is read off the
//! pump report (a command round-trip is the causal barrier), the dial rides
//! QUIC's own handshake, no sleeps.

#![cfg(feature = "quic")]

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ed25519_dalek::SigningKey;
use fauna_core::custody_grant::{
    CUSTODY_GRANT_ID_LEN, CustodyGrant, CustodyScopeSet, sign_custody_grant,
};
use fauna_core::data::Timestamp;
use fauna_core::encoding::EmbedAsBytes;
use fauna_core::identity::ActorKeypair;
use fauna_credential_store::CredentialStore;
use fauna_iroh::{DEFAULT_ALPN, IrohTransport};
use fauna_peer_channel::PeerChannel;
use fauna_peer_sync::admit_over_as;
use fauna_protocol::discovery::NestInfoReply;
use fauna_protocol::peer_sync::WITNESS_CUSTODY_GRANT;
use fauna_protocol::{RpcErrorClass, RpcRequester, decode_strict, encode_canonical};
use fauna_sync_engine::account_runtime::{
    AccountRuntimeParams, AccountStoreRuntime, CRED_NAMESPACE, PeerLegBinding, PeerLegPass,
    RuntimePrincipal, StoreRoot,
};
use fauna_transport::{EndpointKey, PathKind};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMode, SecretKey, endpoint::presets};

/// The custodied account — the runtime under test serves ITS OWN planes, so
/// the owner signing the grant is the runtime's own identity.
fn owner() -> ActorKeypair {
    ActorKeypair::from_secret([0x21; 32])
}

/// A transport fault the pump absorbs (never a rejection — nothing reached a
/// nest, because there is none).
#[derive(Debug)]
struct NoNest(&'static str);

impl std::fmt::Display for NoNest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "no nest in this test: {}", self.0)
    }
}

impl RpcErrorClass for NoNest {
    fn is_rejection(&self) -> bool {
        false
    }
}

/// Answers `fauna.nest.info` with the `peer-sync` advertisement (the brake
/// gate's evidence) and fails every other kind as a transport fault.
#[derive(Clone)]
struct NestInfoOnly;

impl RpcRequester for NestInfoOnly {
    type Error = NoNest;

    async fn request<Req, Reply>(&self, kind: &'static str, _payload: Req) -> Result<Reply, NoNest>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        if kind == "fauna.nest.info" {
            let reply = NestInfoReply {
                capabilities: vec!["peer-sync".to_string()],
                ..Default::default()
            };
            return Ok(decode_strict(&encode_canonical(&reply).expect("encode"))
                .expect("node-info reply decodes"));
        }
        Err(NoNest(kind))
    }
}

impl fauna_protocol::KeyedRpcRequester for NestInfoOnly {
    async fn request_keyed<Req, Reply>(
        &self,
        kind: &'static str,
        _idempotency_key: [u8; 16],
        payload: Req,
    ) -> Result<Reply, NoNest>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        self.request(kind, payload).await
    }
}

/// An `Account`-form custody witness signed by `sign_owner`, naming
/// `custodian_key` as the admitted device principal.
///
/// `expires_at` is a raw value on purpose: the serve side's clock is real
/// epoch seconds, so `1` is always in the past and the far-future constant
/// always ahead, whichever unit a reader assumes.
fn custody_witness_for(
    sign_owner: &ActorKeypair,
    custodian_key: [u8; 32],
    expires_at: u64,
) -> EmbedAsBytes {
    sign_custody_grant(
        sign_owner,
        &CustodyGrant {
            grant_id: vec![0x1D; CUSTODY_GRANT_ID_LEN],
            owner: sign_owner.actor_id(),
            custodian_key,
            scopes: CustodyScopeSet::Account,
            minted_at: Timestamp(1_000),
            expires_at: Timestamp(expires_at),
            removed_devices: Vec::new(),
        },
    )
    .expect("sign custody witness")
}

const FAR_FUTURE: u64 = 9_000_000_000_000_000;

fn epoch_secs() -> u64 {
    fauna_core::data::Timestamp::now_secs_or_zero() as u64
}

#[tokio::test(flavor = "multi_thread")]
async fn the_custody_grant_admits_and_refuses_over_real_quic() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().to_path_buf();

    // The factory: a REAL IrohTransport from the runtime's own resolved
    // writer key, loopback-bound (the raw-dial convention of
    // `peer_sync_over_quic.rs` — the PT-4 candidate filter rejects loopback
    // by design, so tests dial raw).
    let bound_out: Arc<Mutex<Option<Vec<SocketAddr>>>> = Arc::default();
    let factory: fauna_sync_engine::account_runtime::PeerTransportFactory = {
        let bound_out = Arc::clone(&bound_out);
        Arc::new(
            move |inputs: fauna_sync_engine::account_runtime::PeerLegFactoryInputs| {
                let bound_out = Arc::clone(&bound_out);
                Box::pin(async move {
                    let transport = IrohTransport::builder(inputs.writer_key.to_bytes())
                        .bind_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
                        .build()
                        .await?;
                    let bound_addrs = transport.bound_addrs();
                    *bound_out.lock().unwrap() = Some(bound_addrs.clone());
                    Ok(PeerLegBinding {
                        transport: Arc::new(transport),
                        bound_addrs,
                        file_sync: None,
                    })
                })
            },
        )
    };

    let runtime = AccountStoreRuntime::start(AccountRuntimeParams {
        store_backup_exclusion:
            fauna_sync_engine::account_runtime::CloudBackupExclusion::NotApplicable {
                platform: "test".into(),
            },
        store_root: StoreRoot::at(base.join("state")),
        actor_id_hex: owner().actor_id_hex(),
        rpc: NestInfoOnly,
        process_rpc: None,
        principal: RuntimePrincipal::SeedHolding(owner().into()),
        credentials: CredentialStore::with_file_backend(CRED_NAMESPACE, base.join("creds")),
        reconnects: None,
        pushes: None,
        backstop_interval: Duration::from_secs(3600),
        memberships: None,
        trusted_escrow_holders: fauna_sync_engine::account_runtime::fixed_holders(Vec::new()),
        attested_predecessors: Default::default(),
        linked_nests: None,
        owed_nests: None,
        peer_transport: Some(factory),
        // The machine's named row — never read here (no nest answers enrollment).
        enrollment_target_device_id: "ab".repeat(32),
    })
    .await
    .expect("assembly is offline-safe");

    // A pump pass binds (every other step fails against the no-nest requester
    // and is absorbed — the report is the causal barrier).
    let report = runtime.reconcile_now().await.expect("pass");
    assert!(
        matches!(
            report.peer_leg,
            Some(PeerLegPass::Bound | PeerLegPass::AlreadyBound)
        ),
        "the factory-assembled listener came up: {:?} (errors: {:?})",
        report.peer_leg,
        report.errors
    );
    let server_key = runtime
        .principal_bundle_status()
        .await
        .expect("status")
        .device_authorization
        .expect("the assembly minted the enrollment witness")
        .device_key;
    let server_addrs = bound_out
        .lock()
        .unwrap()
        .clone()
        .expect("the factory reported where it bound");
    let server_addrs: Vec<SocketAddr> = server_addrs
        .into_iter()
        .map(|a| SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), a.port()))
        .collect();

    // The custodian: a DIFFERENT account's device — it holds no read keys
    // and no DeviceAuthorization from the owner; the custody grant is its
    // only door.
    let custodian_secret = [0x44u8; 32];
    let custodian_pub = SigningKey::from_bytes(&custodian_secret)
        .verifying_key()
        .to_bytes();
    let endpoint = Endpoint::builder(presets::Empty)
        .secret_key(SecretKey::from_bytes(&custodian_secret))
        .alpns(vec![DEFAULT_ALPN.to_vec()])
        .relay_mode(RelayMode::Disabled)
        .crypto_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .bind_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
        .expect("bind_addr")
        .bind()
        .await
        .expect("custodian endpoint");
    let server_id = EndpointKey::from_bytes(server_key);
    let target = server_addrs.iter().fold(
        EndpointAddr::new(EndpointId::from_bytes(server_id.as_bytes()).unwrap()),
        |a, sa| a.with_ip_addr(*sa),
    );

    let connect = |target: EndpointAddr| {
        let endpoint = endpoint.clone();
        async move {
            let conn = endpoint
                .connect(target, DEFAULT_ALPN)
                .await
                .expect("QUIC connect to the runtime-hosted listener");
            assert_eq!(
                conn.remote_id().as_bytes(),
                server_id.as_bytes(),
                "the QUIC handshake proves the runtime's device principal"
            );
            let (send, recv) = conn.open_bi().await.expect("open_bi");
            let stream: fauna_transport::ByteStream = Box::pin(tokio::io::join(recv, send));
            PeerChannel::over_stream(stream, server_id, PathKind::Lan)
        }
    };

    // Admitting: the owner-signed custody grant naming the channel-proven
    // custodian key passes the third admission arm over real QUIC. The reply
    // witness is the owner device's `DeviceAuthorization` (`None` revocation
    // view is correct for that kind — `custody_leg::dial_one_owner`'s rule).
    let channel = connect(target.clone()).await;
    admit_over_as(
        &channel,
        WITNESS_CUSTODY_GRANT,
        custody_witness_for(&owner(), custodian_pub, FAR_FUTURE),
        &owner().actor_id().0,
        epoch_secs(),
        fauna_peer_sync::AdmissionViews::default(),
        None,
    )
    .await
    .expect("an owner-signed custody grant admits over real QUIC");

    // Refusing, stranger: a grant signed by a key that is NOT the served
    // account's identity is refused at the same exchange.
    let channel = connect(target.clone()).await;
    let stranger = ActorKeypair::from_secret([0x99; 32]);
    admit_over_as(
        &channel,
        WITNESS_CUSTODY_GRANT,
        custody_witness_for(&stranger, custodian_pub, FAR_FUTURE),
        &stranger.actor_id().0,
        epoch_secs(),
        fauna_peer_sync::AdmissionViews::default(),
        None,
    )
    .await
    .expect_err("a stranger-signed custody grant is refused over real QUIC");

    // Refusing, expired: the validity bound is part of the arm — an
    // owner-signed grant whose expiry has passed is refused too.
    let channel = connect(target).await;
    admit_over_as(
        &channel,
        WITNESS_CUSTODY_GRANT,
        custody_witness_for(&owner(), custodian_pub, 1),
        &owner().actor_id().0,
        epoch_secs(),
        fauna_peer_sync::AdmissionViews::default(),
        None,
    )
    .await
    .expect_err("an expired custody grant is refused over real QUIC");

    runtime.shutdown().await;
}
