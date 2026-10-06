//! Protocol-level test handler — `fauna.protocol.echo`.
//!
//! Used by the e2e API test in tests/e2e-unified/tests/test_api_helpers.py
//! and by the integration test in tests/echo_round_trip.rs. Returns the
//! incoming `data` bytes verbatim. No auth restrictions beyond the WS
//! handshake's actor-bound bearer.

use std::time::Duration;

use bytes::Bytes;

use fauna_protocol::{EchoReply, EchoRequest, RpcError, decode_strict as decode, encode_canonical};

use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

/// Register the protocol-level test kinds.
pub fn register_protocol_handlers(b: &mut RpcRouterBuilder) {
    b.add(
        "fauna.protocol.echo",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: echo_handler(),
        },
    );
}

fn echo_handler() -> RpcHandler {
    Box::new(|_state, _actor, payload| {
        Box::pin(async move {
            // Decode EchoRequest from the raw payload bytes.
            let req: EchoRequest = match decode(&payload) {
                Ok(r) => r,
                Err(e) => {
                    return Err(RpcError::new(
                        "fauna.protocol.malformed",
                        "error.protocol.malformed",
                    )
                    .with_details_text(format!("decode: {e}")));
                }
            };
            let reply = EchoReply {
                data: req.data,
                extra: req.extra,
            };
            let bytes = encode_canonical(&reply).map_err(|e| {
                RpcError::new("fauna.protocol.encode_failed", "error.protocol.encode")
                    .with_details_text(format!("{e}"))
            })?;
            Ok(Bytes::from(bytes.to_vec()))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::AppState;
    use std::sync::Arc;

    #[tokio::test]
    async fn echo_handler_round_trips_data_bytes() {
        let db = Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        let state = Arc::new(AppState::for_test(db));
        let actor = [42u8; 32];

        let req = EchoRequest {
            data: vec![1, 2, 3, 4, 5],
            extra: std::collections::BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let payload = Bytes::from(bytes.to_vec());

        let h = echo_handler();
        let reply_bytes = h(state, actor, payload).await.expect("echo ok");
        let reply: EchoReply = decode(&reply_bytes).unwrap();
        assert_eq!(reply.data, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn register_adds_echo_kind() {
        let mut b = crate::rpc_router::RpcRouter::builder();
        register_protocol_handlers(&mut b);
        let r = b.build();
        let m = r.kind_meta("fauna.protocol.echo").expect("registered");
        assert!(!m.forbid_replay);
        assert_eq!(m.default_deadline, Duration::from_secs(5));
    }
}
