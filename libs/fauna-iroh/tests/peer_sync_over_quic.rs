//! W2.6 (account-data-plane.md § Workstreams) × iroh: the peer-sync engine served and walked over a **real QUIC
//! connection** — the engine⇄substrate composition proof
//! (`account-data-plane.md` § The peer leg: "the same sync contract over a
//! different transport", here over the adopted production substrate).
//!
//! The serving replica runs the full stack through the seam: a real
//! `IrohTransport` listener → `start_peer_sync_node` (the rule-7-gated bind)
//! → admission → relay-plane serve. The pulling replica dials **raw iroh**
//! and wraps the QUIC bidi stream in a `PeerChannel` — deliberately, because
//! `IrohTransport::dial`'s PT-4 candidate filter rejects loopback by design
//! (the same reason the crate's own loopback tests drive dialing raw; the
//! dial path's cascade + filter have their own tests). Identity proof is
//! still the real thing: the listener's verdict binds to the NodeId the QUIC
//! handshake proved for the dialer's secret key.
//!
//! Latency-independent throughout (convention 14): handshakes and walks are
//! awaited events, the admission clock is injected, no sleeps.

#![cfg(feature = "quic")]

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use ed25519_dalek::SigningKey;
use fauna_account_store::sqlite::SqliteBackend;
use fauna_account_store::store::AccountStore;
use fauna_account_store::types::WriterId;
use fauna_core::crypto::{AccountStateKeySchedule, BackupKey};
use fauna_core::data::{Capability, DeviceAuthorization, ModerationConfig, Timestamp};
use fauna_core::encoding::{EmbedAsBytes, sign_envelope};
use fauna_core::identity::ActorKeypair;
use fauna_iroh::{DEFAULT_ALPN, IrohTransport};
use fauna_peer_channel::PeerChannel;
use fauna_peer_sync::quota::QuotaConfig;
use fauna_peer_sync::server::{
    NowFn, PeerSyncServer, PeerSyncServerConfig, ServeStoreHandle, start_peer_sync_node,
};
use fauna_peer_sync::{PeerRequester, admit_over};
use fauna_protocol::account_state::ACCOUNT_STATE_SCOPE;
use fauna_protocol::merge_policy::{KIND_MODERATION, LwwStamp, MODERATION_KEY};
use fauna_protocol::{decode_strict, encode_canonical};
use fauna_sync_engine::account_state_plane::{AccountStatePlane, ItemId};
use fauna_transport::{EndpointKey, PathKind};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMode, SecretKey, endpoint::presets};

fn root() -> ActorKeypair {
    ActorKeypair::from_secret([9u8; 32])
}

fn signing_key(device: u8) -> SigningKey {
    SigningKey::from_bytes(&[device; 32])
}

fn writer_id(device: u8) -> WriterId {
    WriterId(signing_key(device).verifying_key().to_bytes())
}

fn witness(device: u8) -> EmbedAsBytes {
    let cert = DeviceAuthorization {
        actor_id: root().actor_id(),
        device_key: writer_id(device).0,
        capabilities: vec![Capability::RenewBearer],
        created_at: Timestamp(1_000),
        expires_at: None,
    };
    let (bytes, env) = sign_envelope(&root(), &cert).expect("sign witness");
    EmbedAsBytes::from_signed(bytes, env)
}

const SERVER_DEVICE: u8 = 0x0B;
const CLIENT_DEVICE: u8 = 0x0A;

/// One replica writes class-2 state; the other admits over a real QUIC
/// connection and walks it into its own store through the seam-served relay
/// plane. iroh's NodeId derivation IS ed25519 (`SecretKey::from_bytes` →
/// public key), so `writer_id(device)` and the QUIC-proven identity agree by
/// construction — the same device-principal identity the charter rules (R5 (account-data-plane.md § The ratified decisions)).
#[tokio::test(flavor = "multi_thread")]
async fn a_replica_walks_its_sibling_over_real_quic() {
    let account = root().actor_id().0;
    let schedule = AccountStateKeySchedule::derive(&BackupKey::derive(&[7u8; 32]));
    // The plane's R14 writer-door trust: nothing here seals a `GenerationTip`
    // kind, so no priors and no escrow holders.
    let trust = fauna_sync_engine::generation_tip::GenerationTrust {
        root: root().actor_id(),
        prior: Vec::new(),
        trusted_holders: Default::default(),
    };
    let now = Arc::new(AtomicU64::new(5_000));

    // ── The serving replica: store + local write + peer-sync node over the seam.
    let server_dir = tempfile::tempdir().unwrap();
    let server_store = AccountStore::open(
        SqliteBackend::open(server_dir.path()).unwrap(),
        &hex::encode(account),
        writer_id(SERVER_DEVICE),
    )
    .await
    .unwrap();

    struct NoNest;
    impl fauna_protocol::RpcRequester for NoNest {
        type Error = anyhow::Error;
        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            _payload: Req,
        ) -> anyhow::Result<Reply>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            anyhow::bail!("pull-only put must be local-only (reached the wire: {kind})")
        }
    }
    let server_sk = signing_key(SERVER_DEVICE);
    let value = encode_canonical(&ModerationConfig {
        muted_keywords: vec!["over-quic".into()],
        ..Default::default()
    })
    .unwrap()
    .to_vec();
    {
        let no_nest = NoNest;
        let plane = AccountStatePlane::new_pull_only(
            &server_store,
            &no_nest,
            &schedule,
            &server_sk,
            &trust,
            ACCOUNT_STATE_SCOPE,
        )
        .unwrap();
        plane
            .put(
                &ItemId {
                    kind: KIND_MODERATION.into(),
                    key: MODERATION_KEY.into(),
                },
                value.clone(),
                Some(
                    LwwStamp {
                        at_ms: 2_000,
                        writer: writer_id(SERVER_DEVICE).0,
                    }
                    .encode()
                    .unwrap(),
                ),
            )
            .await
            .unwrap();
    }

    // A second connection to the same WAL store dir for the serve side (the
    // store's own multi-connection posture).
    let serve_store = AccountStore::open(
        SqliteBackend::open(server_dir.path()).unwrap(),
        &hex::encode(account),
        writer_id(SERVER_DEVICE),
    )
    .await
    .unwrap();
    let now_fn: NowFn = {
        let now = Arc::clone(&now);
        Arc::new(move || now.load(Ordering::SeqCst))
    };
    let server = Arc::new(PeerSyncServer::new(
        ServeStoreHandle::spawn(serve_store),
        account,
        PeerSyncServerConfig {
            display_name: "quic-server".into(),
            own_witness: witness(SERVER_DEVICE),
            own_witness_kind: fauna_protocol::peer_sync::WITNESS_DEVICE_AUTHORIZATION.to_string(),
            custody_revoked: None,
            device_removed: None,
            quotas: QuotaConfig::default(),
            now: now_fn,
        },
    ));
    // The transport's secret IS the device secret, so the QUIC-proven NodeId
    // equals the witness's device_key (R5 — NodeId = device principal).
    let transport = Arc::new(
        IrohTransport::builder([SERVER_DEVICE; 32])
            .bind_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
            .build()
            .await
            .expect("server transport"),
    );
    let server_addrs: Vec<SocketAddr> = transport
        .bound_addrs()
        .into_iter()
        .map(|a| SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), a.port()))
        .collect();
    let _node = start_peer_sync_node(transport.clone(), server, &["peer-sync".to_string()])
        .await
        .expect("the capability gate is open");

    // ── The pulling replica: raw iroh dial (loopback — see module docs), the
    // Y.1 channel over the real QUIC bidi stream, admission, walk.
    let client_dir = tempfile::tempdir().unwrap();
    let client_store = AccountStore::open(
        SqliteBackend::open(client_dir.path()).unwrap(),
        &hex::encode(account),
        writer_id(CLIENT_DEVICE),
    )
    .await
    .unwrap();

    let client_endpoint = Endpoint::builder(presets::Empty)
        .secret_key(SecretKey::from_bytes(&[CLIENT_DEVICE; 32]))
        .alpns(vec![DEFAULT_ALPN.to_vec()])
        .relay_mode(RelayMode::Disabled)
        .crypto_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .bind_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
        .expect("bind_addr")
        .bind()
        .await
        .expect("client endpoint");
    let server_id = EndpointKey::from_bytes(writer_id(SERVER_DEVICE).0);
    let target = server_addrs.into_iter().fold(
        EndpointAddr::new(EndpointId::from_bytes(server_id.as_bytes()).unwrap()),
        |a, sa| a.with_ip_addr(sa),
    );
    let conn = client_endpoint
        .connect(target, DEFAULT_ALPN)
        .await
        .expect("QUIC connect");
    assert_eq!(
        conn.remote_id().as_bytes(),
        server_id.as_bytes(),
        "the QUIC handshake proves the server's device identity"
    );
    let (send, recv) = conn.open_bi().await.expect("open_bi");
    let stream: fauna_transport::ByteStream = Box::pin(tokio::io::join(recv, send));
    let channel = Arc::new(PeerChannel::over_stream(stream, server_id, PathKind::Lan));

    // Mutual admission over the real channel: we present ours; the server's
    // reply witness verifies against ITS QUIC-proven key.
    admit_over(
        &channel,
        witness(CLIENT_DEVICE),
        &account,
        now.load(Ordering::SeqCst),
    )
    .await
    .expect("mutual admission over QUIC");

    // The same W2.4 walk, pull-only, over the real connection.
    let requester = PeerRequester::new(Arc::clone(&channel));
    let client_sk = signing_key(CLIENT_DEVICE);
    let plane = AccountStatePlane::new_pull_only(
        &client_store,
        &requester,
        &schedule,
        &client_sk,
        &trust,
        ACCOUNT_STATE_SCOPE,
    )
    .unwrap();
    let report = plane.walk().await.expect("walk over QUIC");
    assert_eq!(report.applied, 1, "one sibling row adopted: {report:?}");

    let got = client_store
        .state(KIND_MODERATION, MODERATION_KEY)
        .await
        .unwrap()
        .expect("the sibling's entry landed");
    let decoded: ModerationConfig = decode_strict(&got.value).unwrap();
    assert_eq!(
        decoded.muted_keywords,
        vec![fauna_core::data::MutedKeyword::from("over-quic")]
    );
}
