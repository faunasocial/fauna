//! Long-lived push broadcast that survives reconnects.
//!
//! The L3 `RpcDispatcher` re-creates its `broadcast::Sender<PushEvent>`
//! on every `spawn` call. Application subscribers expect a stable
//! channel that survives reconnect cycles. `PushBroker` is that stable
//! channel; the reconnect supervisor parks a forward task per
//! dispatcher-lifetime that pumps the dispatcher's broadcast into the
//! broker's broadcast.

use std::sync::Arc;

use fauna_protocol::PushEvent;
use tokio::sync::broadcast;

/// Long-lived push broadcast. Owned by `NestClient`; survives reconnects.
pub struct PushBroker {
    tx: broadcast::Sender<PushEvent>,
}

impl PushBroker {
    /// Create a broker with the given channel capacity.
    /// 256 matches the per-actor server-side bound in spec § 1.6.
    pub fn new(capacity: usize) -> Arc<Self> {
        let (tx, _) = broadcast::channel(capacity);
        Arc::new(Self { tx })
    }

    /// Subscribe to all push events.
    pub fn subscribe(&self) -> broadcast::Receiver<PushEvent> {
        self.tx.subscribe()
    }

    /// Subscribe to one specific kind. Filters on the broker side; if you
    /// have many `subscribe_kind` consumers, prefer one `subscribe()` and
    /// match on `PushEvent::kind()` in the consumer for less overhead.
    pub fn subscribe_kind(&self, kind: &'static str) -> KindSubscriber {
        KindSubscriber {
            inner: self.tx.subscribe(),
            kind,
        }
    }

    /// Spawn a forward task that pumps `incoming` into this broker until
    /// `incoming` ends. Returns a `JoinHandle` so the supervisor can abort
    /// it on disconnect.
    pub fn bridge_from(
        self: &Arc<Self>,
        mut incoming: broadcast::Receiver<PushEvent>,
    ) -> tokio::task::JoinHandle<()> {
        let broker = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                match incoming.recv().await {
                    Ok(ev) => {
                        let _ = broker.tx.send(ev);
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(
                            "push broker bridge lagged by {n}; dispatcher push channel \
                             outpaced the bridge — should not happen with default capacity"
                        );
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        })
    }

    /// Number of currently-subscribed receivers (for diagnostics / tests).
    pub fn receiver_count(&self) -> usize {
        self.tx.receiver_count()
    }
}

/// Filtered subscriber that only yields a single push-kind.
pub struct KindSubscriber {
    inner: broadcast::Receiver<PushEvent>,
    kind: &'static str,
}

impl KindSubscriber {
    /// Receive the next event of the subscribed kind, skipping others.
    /// Returns `Err(broadcast::error::RecvError)` on close or fatal lag.
    pub async fn recv(&mut self) -> Result<PushEvent, broadcast::error::RecvError> {
        loop {
            let ev = self.inner.recv().await?;
            if ev.kind() == self.kind {
                return Ok(ev);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_protocol::push_events::KnockPayload;

    fn knock(sender: &str) -> PushEvent {
        PushEvent::Knock(KnockPayload {
            sender_id: sender.into(),
            summary: "hi".into(),
            ..Default::default()
        })
    }

    #[tokio::test]
    async fn subscribe_receives_pushed_events() {
        let broker = PushBroker::new(16);
        let mut rx = broker.subscribe();
        broker.tx.send(knock("alice")).unwrap();
        let ev = rx.recv().await.unwrap();
        match ev {
            PushEvent::Knock(k) => assert_eq!(k.sender_id, "alice"),
            other => panic!("got {other:?}"),
        }
    }

    #[tokio::test]
    async fn subscribe_kind_filters_other_kinds() {
        use fauna_protocol::push_events::ResyncRequiredPayload;
        let broker = PushBroker::new(16);
        let mut sub = broker.subscribe_kind("fauna.knock");
        broker
            .tx
            .send(PushEvent::ResyncRequired(ResyncRequiredPayload {
                dropped_count: 3,
                extra: Default::default(),
            }))
            .unwrap();
        broker.tx.send(knock("bob")).unwrap();
        let ev = sub.recv().await.unwrap();
        match ev {
            PushEvent::Knock(k) => assert_eq!(k.sender_id, "bob"),
            other => panic!("expected filtered Knock, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn bridge_forwards_until_source_closes() {
        let broker = PushBroker::new(16);
        let (src_tx, _) = broadcast::channel::<PushEvent>(16);
        let mut sink = broker.subscribe();

        let bridge = broker.bridge_from(src_tx.subscribe());
        src_tx.send(knock("carol")).unwrap();
        let got = sink.recv().await.unwrap();
        match got {
            PushEvent::Knock(k) => assert_eq!(k.sender_id, "carol"),
            other => panic!("got {other:?}"),
        }
        // Dropping all senders closes the source; bridge task should end.
        drop(src_tx);
        let _ = bridge.await;
    }
}
