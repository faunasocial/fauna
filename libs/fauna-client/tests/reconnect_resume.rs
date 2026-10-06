//! `NestClient` ⇄ reconnect-supervisor integration: a request issued while the
//! WS is down must wait for the supervisor to reconnect and then succeed.
//!
//! The pure supervisor mechanics (backoff/retry, ConnectionState transitions,
//! the SubprotocolMismatch exit) now live as unit tests in `fauna-ws-substrate`
//! (`supervisor::tests`); this file keeps only what couples the supervisor to
//! `NestClient::request*`.

mod common;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fauna_client::{AuthClient, NestClient, NestClientError, PushBroker};
use fauna_core::identity::ActorKeypair;
use fauna_protocol::{Frame, Reply, Value, decode_frame, encode_frame};
use fauna_ws_substrate::{Supervisor, run_supervisor};
use serde::{Deserialize, Serialize};

fn kp() -> ActorKeypair {
    ActorKeypair::from_secret([5u8; 32])
}

#[derive(Debug, Serialize, Deserialize)]
struct EchoIn {
    msg: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct EchoOut {
    msg: String,
}

fn encode_value<T: Serialize>(v: &T) -> Value {
    let bytes = fauna_cbor::encode_canonical(v).unwrap();
    fauna_cbor::decode_strict(&bytes).unwrap()
}

/// A request issued while the WS is down (dispatcher slot `None`) must wait
/// for the reconnect supervisor to re-establish the connection and then
/// succeed — not fail fast with a spurious `RpcDisconnected{was_in_flight:
/// false}`. Regression for the cross-app idle-drop spurious-error bug
/// (the request was never sent, so waiting + sending fresh is safe).
#[tokio::test]
async fn request_issued_while_disconnected_waits_for_reconnect() {
    let auth = Arc::new(AuthClient::new("http://127.0.0.1:0".into(), kp()));
    let client = NestClient::with_auth(Arc::clone(&auth));
    // The client starts disconnected: slot None, state Disconnected.
    let (slot, state_tx) = client.supervisor_channels_for_test();

    // Mock channel yields one working adapter; its server end echoes.
    let (adapter, mut server) = common::mpsc_pair();
    let queue: Arc<Mutex<VecDeque<Result<common::MpscAdapter, NestClientError>>>> =
        Arc::new(Mutex::new(VecDeque::from([Ok(adapter)])));
    let channel = Arc::new(common::MockClientChannel::new(
        Arc::clone(&queue),
        Arc::clone(&auth),
        PushBroker::new(16),
    ));

    let server_task = tokio::spawn(async move {
        let bytes = server.rx_from_client.recv().await.unwrap();
        let Frame::Request(req) = decode_frame(&bytes).unwrap() else {
            panic!("expected a Request frame");
        };
        let payload_bytes = fauna_cbor::encode_canonical(&req.payload).unwrap();
        let echo: EchoIn = fauna_cbor::decode_strict(&payload_bytes).unwrap();
        let reply = Frame::Reply(Reply {
            ty: Reply::TYPE,
            correlation_id: req.correlation_id,
            payload: encode_value(&EchoOut { msg: echo.msg }),
            ok: true,
        });
        server
            .tx_to_client
            .send(encode_frame(&reply).unwrap())
            .await
            .unwrap();
    });

    // Issue the request *first*, while still disconnected. Pre-fix it fails
    // fast immediately; post-fix it parks until the supervisor connects.
    let req_client = Arc::clone(&client);
    let req_task = tokio::spawn(async move {
        req_client
            .request_with_deadline::<EchoIn, EchoOut>(
                "fauna.protocol.echo",
                EchoIn { msg: "hi".into() },
                Duration::from_secs(5),
            )
            .await
    });

    // Let the request reach its wait point while the slot is still None.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Now bring the connection up: the supervisor populates the slot and
    // flips connection_state to Connected.
    let cfg = Supervisor {
        channel,
        dispatcher_slot: slot,
        connection_state_tx: state_tx,
        initial_backoff: Duration::from_millis(10),
        max_backoff: Duration::from_millis(50),
    };
    let _sup = tokio::spawn(run_supervisor(cfg));

    let result = req_task.await.unwrap();
    assert_eq!(
        result.unwrap(),
        EchoOut { msg: "hi".into() },
        "request issued while disconnected should succeed after reconnect"
    );
    server_task.await.unwrap();
}
