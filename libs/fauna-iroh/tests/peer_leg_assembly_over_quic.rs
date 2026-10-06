//! W5.7 (account-data-plane.md § Workstreams) × iroh: the **runtime-assembled** peer leg over a real QUIC listener
//! — the assembly-seam twin of `peer_sync_over_quic.rs` (which drives the
//! engine composition with a hand-built server; here the ONLY hands on the
//! listener are `fauna_sync_engine::peer_leg`'s own).
//!
//! What this proves that neither W2.6 test can: the production factory path.
//! `AccountStoreRuntime::start` resolves the writer key, mints the enrollment
//! witness, elects itself, fetches the brake evidence, invokes the
//! app-supplied factory with **its own resolved key** — and the
//! `IrohTransport` that comes back therefore proves, over a real QUIC
//! handshake, exactly the NodeId the slot's witness names (R5 (account-data-plane.md § The ratified decisions): NodeId =
//! device principal = writer key). A root-signed sibling witness admits
//! against that listener; a stranger-signed one is refused. (Row transfer
//! over real QUIC is `peer_sync_over_quic.rs`'s existing proof; this file
//! deliberately stops at the admission verdicts.)
//!
//! The nest requester is a fake that answers ONLY `fauna.nest.info` (the
//! brake gate's evidence) and fails everything else as a transport fault —
//! the runtime's pump absorbs per-step failures by contract, which doubles as
//! a proof the bind does not depend on any other nest leg.
//!
//! Latency-independent throughout (convention 14): the bind is read off the
//! pump report (a command round-trip is the causal barrier), the dial rides
//! QUIC's own handshake, no sleeps.

#![cfg(feature = "quic")]

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ed25519_dalek::SigningKey;
use fauna_core::data::{Capability, DeviceAuthorization, Timestamp};
use fauna_core::encoding::{EmbedAsBytes, sign_envelope};
use fauna_core::identity::ActorKeypair;
use fauna_credential_store::CredentialStore;
use fauna_iroh::{DEFAULT_ALPN, IrohTransport};
use fauna_peer_channel::PeerChannel;
use fauna_peer_sync::admit_over;
use fauna_protocol::discovery::NestInfoReply;
use fauna_protocol::{RpcErrorClass, RpcRequester, decode_strict, encode_canonical};
use fauna_sync_engine::account_runtime::{
    AccountRuntimeParams, AccountStoreRuntime, CRED_NAMESPACE, PeerLegBinding, PeerLegPass,
    RuntimePrincipal, StoreRoot,
};
use fauna_transport::{EndpointKey, PathKind};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMode, SecretKey, endpoint::presets};

fn root() -> ActorKeypair {
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

fn witness_for(sign_root: &ActorKeypair, device_key: [u8; 32]) -> EmbedAsBytes {
    let cert = DeviceAuthorization {
        actor_id: sign_root.actor_id(),
        device_key,
        capabilities: vec![Capability::RenewBearer],
        created_at: Timestamp(1_000),
        expires_at: None,
    };
    let (bytes, env) = sign_envelope(sign_root, &cert).expect("sign witness");
    EmbedAsBytes::from_signed(bytes, env)
}

fn epoch_secs() -> u64 {
    fauna_core::data::Timestamp::now_secs_or_zero() as u64
}

#[tokio::test(flavor = "multi_thread")]
async fn the_runtime_assembled_listener_admits_and_refuses_over_real_quic() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().to_path_buf();

    // The factory: a REAL IrohTransport from the runtime's own resolved
    // writer key, loopback-bound (the raw-dial convention of
    // `peer_sync_over_quic.rs` — the PT-4 candidate filter rejects loopback
    // by design, so tests dial raw). The bound addresses flow out through
    // this slot for the dialer.
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
        actor_id_hex: root().actor_id_hex(),
        rpc: NestInfoOnly,
        process_rpc: None,
        principal: RuntimePrincipal::SeedHolding(root().into()),
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

    // Raw iroh dial: the QUIC handshake proves the listener's NodeId is the
    // writer key the slot's witness names — R5 end to end.
    let dialer_secret = [0x44u8; 32];
    let dialer_pub = SigningKey::from_bytes(&dialer_secret)
        .verifying_key()
        .to_bytes();
    let endpoint = Endpoint::builder(presets::Empty)
        .secret_key(SecretKey::from_bytes(&dialer_secret))
        .alpns(vec![DEFAULT_ALPN.to_vec()])
        .relay_mode(RelayMode::Disabled)
        .crypto_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .bind_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
        .expect("bind_addr")
        .bind()
        .await
        .expect("dialer endpoint");
    let server_id = EndpointKey::from_bytes(server_key);
    let target = server_addrs.iter().fold(
        EndpointAddr::new(EndpointId::from_bytes(server_id.as_bytes()).unwrap()),
        |a, sa| a.with_ip_addr(*sa),
    );
    let conn = endpoint
        .connect(target.clone(), DEFAULT_ALPN)
        .await
        .expect("QUIC connect to the runtime-hosted listener");
    assert_eq!(
        conn.remote_id().as_bytes(),
        server_id.as_bytes(),
        "the QUIC handshake proves the runtime's device principal"
    );

    // Admitting: the root-signed sibling witness passes the admission
    // exchange against the runtime-hosted server.
    let (send, recv) = conn.open_bi().await.expect("open_bi");
    let stream: fauna_transport::ByteStream = Box::pin(tokio::io::join(recv, send));
    let channel = PeerChannel::over_stream(stream, server_id, PathKind::Lan);
    admit_over(
        &channel,
        witness_for(&root(), dialer_pub),
        &root().actor_id().0,
        epoch_secs(),
    )
    .await
    .expect("a root-signed sibling witness admits over real QUIC");

    // Refusing: a stranger-signed witness is refused at the same exchange.
    let conn = endpoint
        .connect(target, DEFAULT_ALPN)
        .await
        .expect("second QUIC connect");
    let (send, recv) = conn.open_bi().await.expect("open_bi");
    let stream: fauna_transport::ByteStream = Box::pin(tokio::io::join(recv, send));
    let channel = PeerChannel::over_stream(stream, server_id, PathKind::Lan);
    let stranger = ActorKeypair::from_secret([0x99; 32]);
    admit_over(
        &channel,
        witness_for(&stranger, dialer_pub),
        &root().actor_id().0,
        epoch_secs(),
    )
    .await
    .expect_err("a stranger-signed witness is refused over real QUIC");

    runtime.shutdown().await;
}
