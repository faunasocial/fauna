//! Y.3 RPC round-trip over in-memory mpsc transport.
//!
//! Drives `RpcDispatcher.request_raw()` against a mock server that
//! decodes the Request, validates kind/idempotency_key/payload, and
//! emits a Reply. Verifies the protocol glue: kind, correlation_id,
//! and typed payload encode/decode through the canonical dag-cbor codec
//! round-trip the way `NestClient.request<Req, Reply>` will use them.
//!
//! End-to-end through the supervisor is covered in tests/reconnect_resume.rs.

mod common;

use std::time::Duration;

use fauna_protocol::{Frame, Reply, RpcDispatcher, Value, decode_frame, encode_frame};
use serde::{Deserialize, Serialize};

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

#[tokio::test]
async fn round_trip_dispatcher_direct() {
    let (adapter, mut server) = common::mpsc_pair();
    let (dispatcher, driver) = RpcDispatcher::new(adapter);
    tokio::spawn(driver);

    // Server: decode one Request, send Reply with prefixed echo.
    let server_task = tokio::spawn(async move {
        let bytes = server.rx_from_client.recv().await.unwrap();
        let frame = decode_frame(&bytes).unwrap();
        match frame {
            Frame::Request(req) => {
                assert_eq!(req.kind, "fauna.protocol.echo");
                assert_eq!(req.idempotency_key, [1u8; 16]);

                // Decode the Req payload (Value → bytes → EchoIn).
                let payload_bytes = fauna_cbor::encode_canonical(&req.payload).unwrap();
                let echo: EchoIn = fauna_cbor::decode_strict(&payload_bytes).unwrap();
                assert_eq!(echo.msg, "hello");

                let reply_payload = encode_value(&EchoOut {
                    msg: format!("echoed: {}", echo.msg),
                });
                let reply = Frame::Reply(Reply {
                    ty: Reply::TYPE,
                    correlation_id: req.correlation_id,
                    payload: reply_payload,
                    ok: true,
                });
                let bytes = encode_frame(&reply).unwrap();
                server.tx_to_client.send(bytes).await.unwrap();
            }
            other => panic!("unexpected frame: {:?}", other),
        }
    });

    let echo_in_value = encode_value(&EchoIn {
        msg: "hello".into(),
    });
    let call = dispatcher
        .request_raw(
            "fauna.protocol.echo",
            [1u8; 16],
            echo_in_value,
            Some(Duration::from_secs(2)),
        )
        .await
        .expect("request_raw");
    let reply_value = call.await_reply().await.expect("reply");

    let bytes = fauna_cbor::encode_canonical(&reply_value).unwrap();
    let reply: EchoOut = fauna_cbor::decode_strict(&bytes).unwrap();
    assert_eq!(
        reply,
        EchoOut {
            msg: "echoed: hello".into(),
        }
    );

    server_task.await.unwrap();
}
