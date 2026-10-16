//! An iroh connection opened early in a dial pass survives the pass's long
//! idle gaps — the premise of connecting every dial target at once
//! (`account-sync-plane.md` § The peer leg → *Discovery*): the sibling and
//! custody dial passes connect and admit every target concurrently, then walk
//! them one after another, so a channel opened at the start of a pass idles
//! for as long as the earlier targets' walks take — up to the per-target
//! walk budget (120 s) each.
//!
//! One round trip, a real-clock idle gap (`IDLE_SECS`, default 300 s), a
//! second round trip on the SAME stream. The listener is our own
//! `IrohTransport` endpoint; the dialer is raw iroh on the same presets
//! (`IrohTransport::dial` refuses loopback candidates by design — see
//! `peer_sync_over_quic.rs`). Measured 2026-10-14: survives 45 s and 300 s
//! with no keep-alive or idle timeout configured in this crate.
//!
//! `#[ignore]`d — a real-clock wait has no place in the suite (convention 14).
//! Re-run by hand after any iroh bump or endpoint-config change:
//! `cargo test -p fauna-iroh --features quic --test idle_channel_survives -- --ignored --nocapture`.

#![cfg(feature = "quic")]

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use fauna_iroh::{DEFAULT_ALPN, IrohTransport};
use fauna_transport::PeerTransport;
use futures_util::StreamExt;
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMode, SecretKey, endpoint::presets};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const SERVER: [u8; 32] = [0x0B; 32];
const CLIENT: [u8; 32] = [0x0A; 32];

#[tokio::test(flavor = "multi_thread")]
#[ignore = "real-clock idle gap (default 300 s) — run by hand, see the module docs"]
async fn an_early_opened_connection_survives_a_long_idle_gap() {
    let idle = Duration::from_secs(
        std::env::var("IDLE_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(300),
    );

    // The listener: our endpoint, echoing every byte back.
    let server = IrohTransport::builder(SERVER)
        .bind_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
        .build()
        .await
        .expect("server transport");
    let addrs: Vec<SocketAddr> = server
        .bound_addrs()
        .into_iter()
        .map(|a| SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), a.port()))
        .collect();
    let mut incoming = server.listen().await.expect("listen");
    let _echo = tokio::spawn(async move {
        let conn = incoming.next().await.expect("one dial").expect("accepted");
        let mut stream = conn.accept_stream().await.expect("accept stream");
        let mut byte = [0u8; 1];
        while stream.read_exact(&mut byte).await.is_ok() {
            if stream.write_all(&byte).await.is_err() {
                break;
            }
        }
    });

    // The dialer.
    let client = Endpoint::builder(presets::Empty)
        .secret_key(SecretKey::from_bytes(&CLIENT))
        .alpns(vec![DEFAULT_ALPN.to_vec()])
        .relay_mode(RelayMode::Disabled)
        .crypto_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .bind_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
        .expect("bind_addr")
        .bind()
        .await
        .expect("client endpoint");
    let server_id = EndpointId::from_bytes(
        ed25519_dalek::SigningKey::from_bytes(&SERVER)
            .verifying_key()
            .as_bytes(),
    )
    .expect("server id");
    let target = addrs
        .into_iter()
        .fold(EndpointAddr::new(server_id), |a, sa| a.with_ip_addr(sa));
    let conn = client
        .connect(target, DEFAULT_ALPN)
        .await
        .expect("QUIC connect");
    let (mut send, mut recv) = conn.open_bi().await.expect("open_bi");
    let mut byte = [0u8; 1];
    send.write_all(&[1]).await.expect("first write");
    recv.read_exact(&mut byte).await.expect("first echo");
    assert_eq!(byte, [1]);

    tokio::time::sleep(idle).await;

    let after = async {
        send.write_all(&[2]).await?;
        recv.read_exact(&mut byte).await?;
        anyhow::Ok(byte)
    };
    let echoed = tokio::time::timeout(Duration::from_secs(5), after)
        .await
        .unwrap_or_else(|_| panic!("no echo within 5 s after {idle:?} idle"))
        .unwrap_or_else(|e| {
            panic!(
                "the connection died during {idle:?} idle: {e:#} (close reason: {:?})",
                conn.close_reason()
            )
        });
    assert_eq!(echoed, [2], "the same stream answers after the idle gap");
}
