//! Server emits Push frames → broker delivers typed `PushEvent` values
//! via the long-lived broadcast. Verifies the dispatcher.push_subscriber
//! → broker.bridge_from path, and the kind-filtering subscribe_kind helper.

mod common;

use fauna_protocol::{
    Frame, Push, PushEvent, RpcDispatcher, encode_frame, push_events::KnockPayload,
};

#[tokio::test]
async fn dispatcher_routes_push_to_broker_via_bridge() {
    use fauna_client::PushBroker;

    let (adapter, server) = common::mpsc_pair();
    let (dispatcher, driver) = RpcDispatcher::new(adapter);
    tokio::spawn(driver);

    let broker = PushBroker::new(16);
    let mut sub = broker.subscribe();
    let _bridge = broker.bridge_from(dispatcher.push_subscriber());

    // Server emits a Knock push.
    let payload = encode_payload(&KnockPayload {
        sender_id: "alice".into(),
        summary: "knock knock".into(),
        ..Default::default()
    });
    let push = Frame::Push(Push {
        ty: Push::TYPE,
        kind: "fauna.knock".into(),
        payload,
        seq: 1,
    });
    let bytes = encode_frame(&push).unwrap();
    server.tx_to_client.send(bytes).await.unwrap();

    let event = sub.recv().await.unwrap();
    match event {
        PushEvent::Knock(k) => {
            assert_eq!(k.sender_id, "alice");
            assert_eq!(k.summary, "knock knock");
        }
        other => panic!("expected Knock, got {other:?}"),
    }
}

fn encode_payload<T: serde::Serialize>(v: &T) -> fauna_protocol::Value {
    let bytes = fauna_cbor::encode_canonical(v).unwrap();
    fauna_cbor::decode_strict(&bytes).unwrap()
}

#[tokio::test]
async fn subscribe_kind_filters_other_kinds() {
    use fauna_client::PushBroker;
    use fauna_protocol::push_events::ResyncRequiredPayload;

    let (adapter, server) = common::mpsc_pair();
    let (dispatcher, driver) = RpcDispatcher::new(adapter);
    tokio::spawn(driver);

    let broker = PushBroker::new(16);
    let mut knock_sub = broker.subscribe_kind("fauna.knock");
    let _bridge = broker.bridge_from(dispatcher.push_subscriber());

    // Emit ResyncRequired first (should be filtered out).
    let resync_payload = encode_payload(&ResyncRequiredPayload {
        dropped_count: 7,
        extra: Default::default(),
    });
    let resync = Frame::Push(Push {
        ty: Push::TYPE,
        kind: "fauna.protocol.resync_required".into(),
        payload: resync_payload,
        seq: 1,
    });
    server
        .tx_to_client
        .send(encode_frame(&resync).unwrap())
        .await
        .unwrap();

    // Then a Knock (should pass the filter).
    let knock_payload = encode_payload(&KnockPayload {
        sender_id: "bob".into(),
        summary: "later".into(),
        ..Default::default()
    });
    let knock = Frame::Push(Push {
        ty: Push::TYPE,
        kind: "fauna.knock".into(),
        payload: knock_payload,
        seq: 2,
    });
    server
        .tx_to_client
        .send(encode_frame(&knock).unwrap())
        .await
        .unwrap();

    let ev = knock_sub.recv().await.unwrap();
    match ev {
        PushEvent::Knock(k) => assert_eq!(k.sender_id, "bob"),
        other => panic!("expected Knock, got {other:?}"),
    }
}
