//! The knock send, written once: `build_knock_payload` → `fauna.inbox.send`,
//! classified. tui's contacts/profile pages and linux's find/profile views all
//! render from [`KnockSend`], so the guardian gate is told apart from every
//! other failure in exactly one place. Native-only (`native` feature).
//!
//! The send is identity-bound (`account-offline-mutation.md` § The
//! offline-mutation contract → *Identity-bound intents compose at
//! drain*): its tuple is signed by the actor key, and the home nest refuses
//! content authored by a superseded identity. So with no nest
//! [`send_knock_or_queue`] queues the knock's INPUTS
//! ([`KnockSendIntent`]) on the outbox's identity door, and the app's
//! [`KnockComposer`] — seated on the outbox as its identity sender — builds
//! and signs the tuple at drain, as the identity it holds then.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use fauna_client::NestClient;
use fauna_core::identity::ActorKeypair;
use fauna_core::offline_intent::{IdentityIntentOutcome, IdentityIntentSender, park_code};
use fauna_protocol::contacts::{KNOCK_SEND_KIND, KnockSendIntent, knock_send_scope};
use fauna_protocol::inbox::InboxSendRequest;
use fauna_protocol::offline_class::is_online;
use fauna_protocol::offline_intent::OfflineIntentQueue;
use fauna_protocol::{KeyedRpcRequester, RpcErrorClass, RpcRequester, Value};

/// How a knock send ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KnockSend {
    Sent,
    /// The nest's typed guardian-approval refusal
    /// (`RpcError::is_guardian_approval_required`) — the one failure that
    /// reveals `contact-request-guardian-button`.
    RefusedByGuardian,
    /// No nest: the knock's inputs rest in the device's outbox and are
    /// composed, signed and sent at drain ([`send_knock_or_queue`]). The
    /// guardian gate can be learned only then; a refusal parks the queued
    /// knock with the nest's code on the unsent-changes list.
    Queued,
    Failed(String),
}

/// `build_knock_payload` → `fauna.inbox.send`. `recipient_nest_url` `None`
/// delivers on this nest; `Some(peer)` makes the home nest originate
/// `fauna.federation.inbox.deliver` to that peer. The guardian gate is
/// separated from every other failure on the shared typed predicate rather
/// than a per-app string match — which handler refused is the nest's business.
pub async fn send_knock<R>(
    nest: R,
    node_url: &str,
    secret: [u8; 32],
    recipient: String,
    recipient_nest_url: Option<String>,
) -> KnockSend
where
    R: RpcRequester,
    R::Error: RpcErrorClass + std::fmt::Display,
{
    let kp = ActorKeypair::from_secret(secret);
    let payload = match knock_payload(&kp, &recipient, node_url) {
        Ok(p) => p,
        Err(e) => return KnockSend::Failed(e),
    };
    match fauna_client_inbox::InboxClient::new(nest)
        .send(recipient, recipient_nest_url, payload)
        .await
    {
        Ok(_) => KnockSend::Sent,
        Err(e) if is_guardian_refusal(&e) => KnockSend::RefusedByGuardian,
        Err(e) => KnockSend::Failed(format!("inbox send: {e}")),
    }
}

/// [`send_knock`] through the identity door: sent now while the app has a
/// nest, or — when `queue` is wired and its connection word is a known
/// offline word — queued as a [`KnockSendIntent`] (scope
/// `inbox.send:<recipient>`, [`knock_send_scope`]) on the outbox's identity
/// drainer ([`OfflineIntentQueue::enqueue_identity`]). The gate is
/// `fauna_protocol::offline_intent::send_or_queue`'s exactly: a request that
/// fails in flight is never re-queued. A recipient id that does not parse
/// fails now, never parks later.
pub async fn send_knock_or_queue<R>(
    nest: R,
    queue: Option<&dyn OfflineIntentQueue>,
    node_url: &str,
    secret: [u8; 32],
    recipient: String,
    recipient_nest_url: Option<String>,
) -> KnockSend
where
    R: RpcRequester,
    R::Error: RpcErrorClass + std::fmt::Display,
{
    if let Some(queue) = queue
        && !is_online(&queue.connection_state())
    {
        if let Err(e) = fauna_core::hex32::decode(&recipient) {
            return KnockSend::Failed(format!("recipient id: {e}"));
        }
        let scope = knock_send_scope(&recipient);
        let intent = KnockSendIntent {
            recipient_actor_id: recipient,
            recipient_nest_url,
            extra: Default::default(),
        };
        let bytes = match fauna_protocol::encode_canonical(&intent) {
            Ok(b) => b.to_vec(),
            Err(e) => return KnockSend::Failed(format!("knock intent: {e}")),
        };
        return match queue.enqueue_identity(KNOCK_SEND_KIND, scope, bytes).await {
            Ok(_) => KnockSend::Queued,
            Err(e) => KnockSend::Failed(e),
        };
    }
    send_knock(nest, node_url, secret, recipient, recipient_nest_url).await
}

/// Compose and send one queued knock: decode its inputs, build and sign the
/// tuple as `kp` (the identity the app holds now) for `node_url`, and send
/// `fauna.inbox.send` under `intent_id` as the idempotency key. A nest
/// refusal — the guardian gate included — parks with the nest's code; a
/// transport fault retries; inputs that do not decode park.
pub async fn compose_and_send_knock<R>(
    nest: &R,
    kp: &ActorKeypair,
    node_url: &str,
    intent_id: [u8; 16],
    payload: &[u8],
) -> IdentityIntentOutcome
where
    R: KeyedRpcRequester,
    R::Error: RpcErrorClass + std::fmt::Display,
{
    let intent: KnockSendIntent = match fauna_protocol::decode_strict(payload) {
        Ok(i) => i,
        Err(e) => {
            return IdentityIntentOutcome::Park {
                code: park_code::UNDECODABLE.to_string(),
                why: format!("knock intent does not decode: {e}"),
            };
        }
    };
    let payload_bytes = match knock_payload(kp, &intent.recipient_actor_id, node_url) {
        Ok(p) => p,
        Err(why) => {
            return IdentityIntentOutcome::Park {
                code: park_code::UNDECODABLE.to_string(),
                why,
            };
        }
    };
    let request = InboxSendRequest {
        recipient_actor_id: intent.recipient_actor_id,
        recipient_nest_url: intent.recipient_nest_url,
        payload_bytes,
        extra: Default::default(),
    };
    match nest
        .request_keyed::<_, Value>(KNOCK_SEND_KIND, intent_id, request)
        .await
    {
        Ok(_) => IdentityIntentOutcome::Sent,
        Err(e) if e.is_rejection() => IdentityIntentOutcome::Park {
            code: e
                .as_rpc_error()
                .map_or(park_code::REJECTED.to_string(), |r| r.code.clone()),
            why: format!("nest refused the knock: {e}"),
        },
        Err(e) => IdentityIntentOutcome::Retry(format!("inbox send: {e}")),
    }
}

/// The app's identity composer: the [`IdentityIntentSender`] it seats on its
/// outbox (`AccountOutbox::set_identity_sender`), holding the session's
/// identity, node url and nest — re-seated at every sign-in, so a drain
/// signs as the identity the app holds then. Only `fauna.inbox.send` has a
/// composer today; any other kind parks as unknown, loudly (a newer build's
/// identity-bound kind read by this one).
pub struct KnockComposer {
    nest: Arc<NestClient>,
    kp: ActorKeypair,
    node_url: String,
}

impl KnockComposer {
    pub fn new(nest: Arc<NestClient>, secret: [u8; 32], node_url: impl Into<String>) -> Self {
        Self {
            nest,
            kp: ActorKeypair::from_secret(secret),
            node_url: node_url.into(),
        }
    }

    async fn send(&self, intent_id: [u8; 16], kind: &str, payload: &[u8]) -> IdentityIntentOutcome {
        if kind != KNOCK_SEND_KIND {
            return IdentityIntentOutcome::Park {
                code: park_code::UNKNOWN_KIND.to_string(),
                why: format!("no identity composer for {kind}"),
            };
        }
        compose_and_send_knock(&self.nest, &self.kp, &self.node_url, intent_id, payload).await
    }
}

// The trait's `async_trait` shape written out by hand (its native arm — this
// module is native-only), so this crate takes no macro dependency for one impl.
impl IdentityIntentSender for KnockComposer {
    fn send_identity_intent<'life0, 'life1, 'life2, 'async_trait>(
        &'life0 self,
        intent_id: [u8; 16],
        kind: &'life1 str,
        payload: &'life2 [u8],
    ) -> Pin<Box<dyn Future<Output = IdentityIntentOutcome> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        'life1: 'async_trait,
        'life2: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(self.send(intent_id, kind, payload))
    }
}

/// The signed tuple for `recipient` (hex) as `kp`, or the reason in words.
fn knock_payload(kp: &ActorKeypair, recipient: &str, node_url: &str) -> Result<Vec<u8>, String> {
    let recipient_bytes =
        fauna_core::hex32::decode(recipient).map_err(|e| format!("recipient id: {e}"))?;
    fauna_client_core::email::build_knock_payload(kp, &recipient_bytes, node_url)
        .map_err(|e| format!("knock payload: {e}"))
}

fn is_guardian_refusal<E: RpcErrorClass>(e: &E) -> bool {
    e.as_rpc_error()
        .is_some_and(|r| r.is_guardian_approval_required())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::{ClassifiedError, block_on};
    use fauna_protocol::RpcError;
    use std::sync::Mutex;

    /// One recorded send: `(kind, idempotency key, request)`.
    type Sent = (&'static str, Option<[u8; 16]>, InboxSendRequest);

    /// A keyed transport recording `(kind, key, request)` and answering from
    /// one canned result.
    struct KeyedFake {
        sent: Mutex<Vec<Sent>>,
        answer: Result<(), ClassifiedError>,
    }

    impl KeyedFake {
        fn answering(answer: Result<(), ClassifiedError>) -> Self {
            Self {
                sent: Mutex::new(Vec::new()),
                answer,
            }
        }

        fn record<Req: serde::Serialize, Reply: serde::de::DeserializeOwned>(
            &self,
            kind: &'static str,
            key: Option<[u8; 16]>,
            payload: Req,
        ) -> Result<Reply, ClassifiedError> {
            let bytes = fauna_protocol::encode_canonical(&payload).unwrap();
            let req: InboxSendRequest = fauna_protocol::decode_strict(&bytes).unwrap();
            self.sent.lock().unwrap().push((kind, key, req));
            self.answer.clone()?;
            let reply = fauna_protocol::encode_canonical(&fauna_protocol::inbox::InboxSendReply {
                inbox_id: Some(1),
                extra: Default::default(),
            })
            .unwrap();
            Ok(fauna_protocol::decode_strict(&reply).unwrap())
        }
    }

    impl RpcRequester for KeyedFake {
        type Error = ClassifiedError;
        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, ClassifiedError>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            self.record(kind, None, payload)
        }
    }

    impl KeyedRpcRequester for KeyedFake {
        async fn request_keyed<Req, Reply>(
            &self,
            kind: &'static str,
            key: [u8; 16],
            payload: Req,
        ) -> Result<Reply, ClassifiedError>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            self.record(kind, Some(key), payload)
        }
    }

    use fauna_client_testkit::RecordingQueue;

    fn recipient() -> String {
        "aa".repeat(32)
    }

    #[test]
    fn a_knock_send_with_no_nest_is_queued_scoped_to_its_recipient() {
        let nest = KeyedFake::answering(Ok(()));
        let queue = RecordingQueue::new("disconnected");
        let out = block_on(send_knock_or_queue(
            &nest,
            Some(&queue),
            "https://home.test",
            [1; 32],
            recipient(),
            Some("https://peer.test".into()),
        ));
        assert_eq!(out, KnockSend::Queued);
        assert!(nest.sent.lock().unwrap().is_empty(), "nothing sent");
        assert!(queue.queued().is_empty(), "never the generic door");
        let queued = queue.identity_queued();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].0, "fauna.inbox.send");
        assert_eq!(queued[0].1, format!("inbox.send:{}", recipient()));
        let intent: KnockSendIntent = fauna_protocol::decode_strict(&queued[0].2).unwrap();
        assert_eq!(intent.recipient_actor_id, recipient());
        assert_eq!(
            intent.recipient_nest_url.as_deref(),
            Some("https://peer.test")
        );
    }

    #[test]
    fn a_knock_send_with_the_nest_connected_is_sent_not_queued() {
        let nest = KeyedFake::answering(Ok(()));
        let queue = RecordingQueue::new("connected");
        let out = block_on(send_knock_or_queue(
            &nest,
            Some(&queue),
            "https://home.test",
            [1; 32],
            recipient(),
            None,
        ));
        assert_eq!(out, KnockSend::Sent);
        assert!(queue.identity_queued().is_empty(), "nothing queued");
        assert_eq!(nest.sent.lock().unwrap().len(), 1);
    }

    /// Queued under one identity, drained under its successor: the tuple the
    /// drain sends is signed by the key the composer holds at drain, under the
    /// intent id.
    #[test]
    fn a_queued_knock_is_composed_and_signed_at_drain_by_the_then_current_key() {
        let queue = RecordingQueue::new("disconnected");
        let key_a = [1u8; 32];
        let out = block_on(send_knock_or_queue(
            &KeyedFake::answering(Ok(())),
            Some(&queue),
            "https://home.test",
            key_a,
            recipient(),
            None,
        ));
        assert_eq!(out, KnockSend::Queued);
        let inputs = queue.identity_queued()[0].2.clone();

        let key_b = ActorKeypair::from_secret([2u8; 32]);
        let nest = KeyedFake::answering(Ok(()));
        let id = [7u8; 16];
        let outcome = block_on(compose_and_send_knock(
            &nest,
            &key_b,
            "https://home.test",
            id,
            &inputs,
        ));
        assert_eq!(outcome, IdentityIntentOutcome::Sent);
        let sent = nest.sent.lock().unwrap();
        assert_eq!(sent[0].0, "fauna.inbox.send");
        assert_eq!(sent[0].1, Some(id), "the intent id is the idempotency key");
        assert_eq!(sent[0].2.recipient_actor_id, recipient());
        let decoded = fauna_client_core::email::decode_email(&sent[0].2.payload_bytes).unwrap();
        assert!(decoded.valid);
        assert_eq!(decoded.from, key_b.actor_id().0);
        assert_ne!(decoded.from, ActorKeypair::from_secret(key_a).actor_id().0);
    }

    #[test]
    fn a_refused_drain_parks_with_the_nests_code_and_a_fault_retries() {
        let inputs = fauna_protocol::encode_canonical(&KnockSendIntent {
            recipient_actor_id: recipient(),
            recipient_nest_url: None,
            extra: Default::default(),
        })
        .unwrap()
        .to_vec();
        let kp = ActorKeypair::from_secret([2u8; 32]);
        let guardian = RpcError::new(
            "fauna.family.guardian_approval_required",
            "family.guardian_approval_required",
        );
        let refused = KeyedFake::answering(Err(ClassifiedError::Rejected(guardian)));
        let parked = block_on(compose_and_send_knock(&refused, &kp, "u", [1; 16], &inputs));
        assert!(
            matches!(&parked, IdentityIntentOutcome::Park { code, .. }
                if code == "fauna.family.guardian_approval_required"),
            "{parked:?}"
        );
        let down = KeyedFake::answering(Err(ClassifiedError::Transport("gone".into())));
        let retried = block_on(compose_and_send_knock(&down, &kp, "u", [1; 16], &inputs));
        assert!(
            matches!(retried, IdentityIntentOutcome::Retry(_)),
            "{retried:?}"
        );
        let garbage = block_on(compose_and_send_knock(&down, &kp, "u", [1; 16], b"\x01"));
        assert!(
            matches!(&garbage, IdentityIntentOutcome::Park { code, .. }
                if code == park_code::UNDECODABLE),
            "{garbage:?}"
        );
    }
}
