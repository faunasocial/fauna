//! Rule 5 at the socket: an iroh endpoint's UDP port is free again once every
//! handle to the transport is dropped (`docs/goal/behavior/p2p.md`
//! § Per-device participation — "off ⇒ no socket"). Both shared bind doors
//! take a listener down by dropping their handles (`peer_leg::drop_listener`,
//! `SessionSeat::unbind`), so this is the one fact the whole control rests
//! on, asserted here on the real substrate rather than the seam double.
#![cfg(feature = "quic")]

use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

/// A generous ceiling — convention 14: a budget a green run pays one tick of,
/// never an expected latency.
const FREE_BUDGET: Duration = Duration::from_secs(20);

fn port_is_bindable(addr: SocketAddr) -> bool {
    // Bind the same family + port the endpoint held; a socket iroh still
    // holds makes this fail with EADDRINUSE.
    let probe: SocketAddr = match addr {
        SocketAddr::V4(_) => SocketAddr::new("0.0.0.0".parse().unwrap(), addr.port()),
        SocketAddr::V6(_) => SocketAddr::new("::".parse().unwrap(), addr.port()),
    };
    UdpSocket::bind(probe).is_ok()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bound_endpoints_port_is_free_again_once_every_handle_drops() {
    let (transport, bound) = fauna_iroh::peer_leg_transport([0x51u8; 32], None)
        .await
        .expect("an endpoint binds");
    let addr = *bound.first().expect("the endpoint reports where it bound");
    assert!(
        !port_is_bindable(addr),
        "while the transport lives, its port is held: {addr}"
    );

    drop(transport);

    let deadline = Instant::now() + FREE_BUDGET;
    loop {
        if port_is_bindable(addr) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the endpoint's port {addr} was still held {FREE_BUDGET:?} after the last handle dropped"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
