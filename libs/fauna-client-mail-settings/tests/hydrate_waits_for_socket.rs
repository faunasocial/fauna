//! A machine's `hydrate()` issued **before the WS is up** parks until the
//! socket lands and then succeeds — it does not fail fast.
//!
//! # Why this test exists
//!
//! This is the property that makes an app-side "retry `hydrate()` N times,
//! sleeping between" wrapper unnecessary, and ~40 of those had accumulated
//! across five app platforms before anyone checked whether the transport
//! already handled it. It does, and has since
//! 2026-05-25: `NestClient::connect` returns once the WS handshake has been
//! *initiated*, so a page mounting immediately after login issues its first
//! read while the dispatcher slot is still `None` — and `request_inner` parks
//! that read for the kind's deadline budget rather than answering a spurious
//! `RpcDisconnected`.
//!
//! `fauna-client`'s own `request_issued_while_disconnected_waits_for_reconnect`
//! pins that at the transport layer, over a raw echo kind. This one pins it one
//! layer up, where the apps actually consume it: a real `CaldavPolicyMachine`
//! built by the production `rpc_glue` constructor over a real `NestClient`,
//! hydrating a real `fauna.bridges.get_mail_config` read. If this goes red, the
//! deleted app-side wrappers were load-bearing after all and the deletions must
//! be revisited — that is exactly the signal it is here to give.
//!
//! Latency-independent per `e2e-conventions.md` convention 14: nothing asserts
//! *how long* the park lasts. The test drives the connection up on its own
//! schedule and asserts the hydrate resolved `Ok` with the served config, so a
//! slow machine changes nothing.

use std::sync::Arc;
use std::time::Duration;

use fauna_client::testing::{MockClientChannel, connect_queue, mpsc_pair};
use fauna_client::{AuthClient, NestClient, PushBroker};
use fauna_client_mail_settings::rpc_glue::build_caldav_policy_machine;
use fauna_core::identity::ActorKeypair;
use fauna_protocol::bridge_routing::FetchConfigReply;
use fauna_protocol::{Frame, Reply, decode_frame, encode_frame};
use fauna_ws_substrate::{Supervisor, run_supervisor};

/// `hydrate()` on a machine whose client has not finished connecting must wait
/// for the socket and then return the served config — not `Err`.
#[tokio::test]
async fn hydrate_issued_before_socket_is_up_waits_and_succeeds() {
    let auth = Arc::new(AuthClient::new(
        "http://127.0.0.1:0".into(),
        ActorKeypair::from_secret([7u8; 32]),
    ));
    // A client in the exact post-login state: constructed, supervisor not yet
    // landed, so the dispatcher slot is `None` and the state `Disconnected`.
    let client = NestClient::with_auth(Arc::clone(&auth));
    let (slot, state_tx) = client.supervisor_channels_for_test();

    // The machine is built by the SAME production constructor the apps call
    // through `fauna-ffi`/linux, over this not-yet-connected client.
    let machine = Arc::new(build_caldav_policy_machine(Arc::clone(&client)));

    // The served config: CalDAV on, on a non-default port, so a pass cannot be
    // confused with the `CaldavPolicySnapshot::default()` a failed hydrate
    // would leave behind.
    let served = FetchConfigReply {
        caldav_enabled: true,
        caldav_port: 8443,
        ..FetchConfigReply::default()
    };

    let (adapter, mut server) = mpsc_pair();
    let served_for_server = served.clone();
    let server_task = tokio::spawn(async move {
        let bytes = server.rx_from_client.recv().await.expect("a request frame");
        let Frame::Request(req) = decode_frame(&bytes).expect("decodable frame") else {
            panic!("expected a Request frame");
        };
        assert_eq!(
            req.kind, "fauna.bridges.get_mail_config",
            "hydrate() should read the admin config twin"
        );
        let payload_bytes = fauna_cbor::encode_canonical(&served_for_server).unwrap();
        let reply = Frame::Reply(Reply {
            ty: Reply::TYPE,
            correlation_id: req.correlation_id,
            payload: fauna_cbor::decode_strict(&payload_bytes).unwrap(),
            ok: true,
        });
        server
            .tx_to_client
            .send(encode_frame(&reply).unwrap())
            .await
            .unwrap();
    });

    // Issue the hydrate FIRST, while the slot is still `None`. Before the
    // transport's wait-for-reconnect this returned `RpcDisconnected`
    // immediately — which is what every app-side retry wrapper was built to
    // paper over.
    let hydrating = tokio::spawn({
        let machine = Arc::clone(&machine);
        async move { machine.hydrate().await }
    });

    // Let the hydrate reach its wait point with the connection still down.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !hydrating.is_finished(),
        "hydrate() must still be parked while the dispatcher slot is None — a \
         finished future here means it failed fast, and the app-side retry \
         wrappers this test licenses deleting were load-bearing"
    );

    // Now land the connection, exactly as the supervisor does post-login.
    let channel = Arc::new(MockClientChannel::new(
        connect_queue([Ok(adapter)]),
        Arc::clone(&auth),
        PushBroker::new(16),
    ));
    let _sup = tokio::spawn(run_supervisor(Supervisor {
        channel,
        dispatcher_slot: slot,
        connection_state_tx: state_tx,
        initial_backoff: Duration::from_millis(10),
        max_backoff: Duration::from_millis(50),
    }));

    hydrating
        .await
        .expect("hydrate task panicked")
        .expect("hydrate issued before the socket was up should succeed once it lands");

    let snap = machine.snapshot();
    assert!(
        snap.caldav_enabled,
        "the parked hydrate should have applied the served config"
    );
    assert_eq!(snap.caldav_port, 8443);

    server_task.await.unwrap();
}
