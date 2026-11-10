//! The `OfflineQueued` composer — send now, or queue durably while there is no
//! nest (W4 (account-data-plane.md § Workstreams), the first composer).
//!
//! Charter: `account-offline-mutation.md` § The offline-mutation contract. An
//! `OfflineQueued` kind is one whose effect the nest assigns (an id, a counter,
//! a delivery), so the UI keeps its control live with no nest
//! ([`crate::offline_class::affordance`], ruling 1) and its gesture must not
//! fail there either: it becomes a durable intent in the device's outbox, which
//! the account runtime's full pump pass drains with the intent id as the
//! envelope idempotency key once the nest is back
//! (`fauna_account_plane::outbox::drain_outbox`).
//!
//! [`send_or_queue`] is the ONE decision point every app's gesture goes through
//! — shared Rust, never a per-app "am I offline?" branch (priority #2). Its two
//! inputs come from the app through [`OfflineIntentQueue`]: the connection word
//! (read with [`crate::offline_class::is_online`], the same polarity the offline
//! gate uses, so a control the gate keeps live is exactly a control whose
//! gesture this queues) and the outbox door.
//!
//! **Queue only when the app already knows it has no nest.** A request that
//! fails in flight is NOT re-queued here: it may have reached the nest under a
//! key the composer never saw, and a fresh intent key would apply it twice. The
//! error goes back to the caller as it always did.

pub use fauna_core::offline_intent::OfflineIntentQueue;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::RpcRequester;
use crate::offline_class::{OfflineClass, is_online, offline_class};

/// What [`send_or_queue`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Composed<Reply> {
    /// The nest answered now.
    Sent(Reply),
    /// No nest: the intent rests durably in the outbox under this id (its
    /// idempotency key) and drains when the nest is back.
    Queued([u8; 16]),
}

/// Why [`send_or_queue`] failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposeError<E> {
    /// The online send failed — the transport's own error, unchanged.
    Rpc(E),
    /// The app has no nest and the intent could not be queued (the outbox
    /// refused it, the payload would not encode, or `kind` is not
    /// `OfflineQueued`).
    Queue(String),
}

impl<E: core::fmt::Display> core::fmt::Display for ComposeError<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ComposeError::Rpc(e) => write!(f, "{e}"),
            ComposeError::Queue(why) => write!(f, "could not queue the change: {why}"),
        }
    }
}

/// Send `payload` as `kind` now, or — when `queue` is wired and its connection
/// word is a known offline word — append it to the outbox under `scope` and
/// answer [`Composed::Queued`].
///
/// `queue = None` (an app that has not wired its outbox yet) is the old
/// behaviour exactly: always send. `kind` must be
/// [`OfflineClass::OfflineQueued`] for the queue arm; asking to queue any
/// other class is refused before the outbox sees it (the outbox enforces the
/// same boundary at its own door).
pub async fn send_or_queue<R, Req, Reply>(
    rpc: &R,
    queue: Option<&dyn OfflineIntentQueue>,
    kind: &'static str,
    scope: &str,
    payload: Req,
) -> Result<Composed<Reply>, ComposeError<R::Error>>
where
    R: RpcRequester,
    Req: Serialize,
    Reply: DeserializeOwned,
{
    if let Some(queue) = queue
        && !is_online(&queue.connection_state())
    {
        if offline_class(kind) != Some(OfflineClass::OfflineQueued) {
            return Err(ComposeError::Queue(format!(
                "{kind} is not an OfflineQueued kind"
            )));
        }
        let bytes = crate::encode_canonical(&payload)
            .map_err(|e| ComposeError::Queue(format!("payload does not encode: {e}")))?;
        let id = queue
            .enqueue(kind, scope.to_string(), bytes.to_vec())
            .await
            .map_err(ComposeError::Queue)?;
        return Ok(Composed::Queued(id));
    }
    rpc.request(kind, payload)
        .await
        .map(Composed::Sent)
        .map_err(ComposeError::Rpc)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const QUEUED: &str = "fauna.posts.interact";

    /// A transport that counts the requests it was asked to send and answers
    /// `ok` with an empty map.
    #[derive(Default)]
    struct CountingRpc {
        sent: AtomicUsize,
    }

    impl RpcRequester for CountingRpc {
        type Error = String;
        async fn request<Req, Reply>(&self, _kind: &'static str, _p: Req) -> Result<Reply, String>
        where
            Req: Serialize,
            Reply: DeserializeOwned,
        {
            self.sent.fetch_add(1, Ordering::SeqCst);
            let empty =
                crate::encode_canonical(&std::collections::BTreeMap::<String, u8>::new()).unwrap();
            crate::decode_strict(&empty).map_err(|e| e.to_string())
        }
    }

    struct RecordingQueue {
        word: &'static str,
        queued: Mutex<Vec<(String, String, Vec<u8>)>>,
    }

    impl RecordingQueue {
        fn new(word: &'static str) -> Self {
            Self {
                word,
                queued: Mutex::new(Vec::new()),
            }
        }
    }

    // Written in `async_trait`'s desugared form: this crate carries no
    // `async-trait` dependency of its own, and the trait (in `fauna-core`) is
    // the `async_trait` shape, so a hand impl names the boxed future.
    impl OfflineIntentQueue for RecordingQueue {
        fn connection_state(&self) -> String {
            self.word.to_string()
        }
        fn enqueue<'life0, 'async_trait>(
            &'life0 self,
            kind: &'static str,
            scope: String,
            payload: Vec<u8>,
        ) -> core::pin::Pin<
            Box<dyn core::future::Future<Output = Result<[u8; 16], String>> + Send + 'async_trait>,
        >
        where
            'life0: 'async_trait,
            Self: 'async_trait,
        {
            self.queued
                .lock()
                .unwrap()
                .push((kind.to_string(), scope, payload));
            Box::pin(async { Ok([7; 16]) })
        }
    }

    #[derive(Serialize, serde::Deserialize, Debug, PartialEq)]
    struct Req {
        post_id: String,
    }

    type Reply = std::collections::BTreeMap<String, u8>;

    fn block<F: core::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(f)
    }

    #[test]
    fn offline_queues_the_canonical_payload_and_sends_nothing() {
        let rpc = CountingRpc::default();
        let queue = RecordingQueue::new("disconnected");
        let out = block(send_or_queue::<_, _, Reply>(
            &rpc,
            Some(&queue),
            QUEUED,
            "posts.interact:ab",
            Req {
                post_id: "ab".into(),
            },
        ))
        .unwrap();
        assert_eq!(out, Composed::Queued([7; 16]));
        assert_eq!(
            rpc.sent.load(Ordering::SeqCst),
            0,
            "no nest: nothing may be sent"
        );
        let queued = queue.queued.lock().unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].0, QUEUED);
        assert_eq!(queued[0].1, "posts.interact:ab");
        // The drain decodes these bytes and re-sends them verbatim.
        let back: Req = crate::decode_strict(&queued[0].2).unwrap();
        assert_eq!(
            back,
            Req {
                post_id: "ab".into()
            }
        );
    }

    #[test]
    fn online_or_unwired_sends_and_queues_nothing() {
        for word in ["connected", "some-future-word"] {
            let rpc = CountingRpc::default();
            let queue = RecordingQueue::new(word);
            let out = block(send_or_queue::<_, _, Reply>(
                &rpc,
                Some(&queue),
                QUEUED,
                "s",
                Req {
                    post_id: "x".into(),
                },
            ))
            .unwrap();
            assert!(matches!(out, Composed::Sent(_)), "{word}: must send");
            assert_eq!(rpc.sent.load(Ordering::SeqCst), 1);
            assert!(
                queue.queued.lock().unwrap().is_empty(),
                "{word}: nothing queued"
            );
        }
        let rpc = CountingRpc::default();
        let out = block(send_or_queue::<_, _, Reply>(
            &rpc,
            None,
            QUEUED,
            "s",
            Req {
                post_id: "x".into(),
            },
        ))
        .unwrap();
        assert!(
            matches!(out, Composed::Sent(_)),
            "no queue wired: the old path"
        );
    }

    #[test]
    fn a_kind_that_is_not_offline_queued_is_refused_offline() {
        let rpc = CountingRpc::default();
        let queue = RecordingQueue::new("unreachable");
        let err = block(send_or_queue::<_, _, Reply>(
            &rpc,
            Some(&queue),
            "fauna.family.policy.update",
            "s",
            Req {
                post_id: "x".into(),
            },
        ))
        .unwrap_err();
        assert!(matches!(err, ComposeError::Queue(_)));
        assert!(queue.queued.lock().unwrap().is_empty());
        assert_eq!(rpc.sent.load(Ordering::SeqCst), 0);
    }
}
