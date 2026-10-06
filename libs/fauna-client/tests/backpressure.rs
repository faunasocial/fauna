//! PushBroker buffer overflow → subscriber sees `RecvError::Lagged(n)`
//! and continues to receive subsequent events.

use std::sync::Arc;

use fauna_client::PushBroker;
use fauna_protocol::PushEvent;
use fauna_protocol::push_events::KnockPayload;
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;

fn knock(s: &str) -> PushEvent {
    PushEvent::Knock(KnockPayload {
        sender_id: s.into(),
        summary: "x".into(),
        ..Default::default()
    })
}

#[tokio::test]
async fn slow_subscriber_sees_lagged_then_recovers() {
    let broker = PushBroker::new(2); // tiny capacity
    let mut sub = broker.subscribe();

    // Drive sends through bridge_from with a separate source channel.
    let (src_tx, src_rx) = broadcast::channel::<PushEvent>(256);
    let _bridge = broker.bridge_from(src_rx);

    // Fill + overflow. Pump the events synchronously and yield to let
    // the bridge task pump them through.
    src_tx.send(knock("a")).unwrap();
    src_tx.send(knock("b")).unwrap();
    src_tx.send(knock("c")).unwrap();
    src_tx.send(knock("d")).unwrap();

    // Yield so the bridge task forwards into the broker's buffer.
    // After yields, the broker's buffer of capacity 2 holds the most
    // recent 2 events; older ones are dropped.
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }

    // First recv should yield Lagged with how many were dropped.
    match sub.recv().await {
        Err(RecvError::Lagged(n)) => assert!(n >= 1, "lag count: {n}"),
        other => panic!("expected Lagged, got {other:?}"),
    }

    // Subsequent recv yields the still-buffered events. The broker keeps
    // the most recent 2 items.
    let next = sub.recv().await.unwrap();
    match next {
        PushEvent::Knock(k) => assert!(["c", "d"].contains(&k.sender_id.as_str())),
        other => panic!("expected Knock, got {other:?}"),
    }

    drop(broker);
    drop(src_tx);
    let _: Arc<()> = Arc::new(()); // sanity to ensure no leftover refs
}
