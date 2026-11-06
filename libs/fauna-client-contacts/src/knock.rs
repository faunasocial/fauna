//! The knock send, written once: `build_knock_payload` → `fauna.inbox.send`,
//! classified. tui's contacts/profile pages and linux's find/profile views all
//! render from [`KnockSend`], so the guardian gate is told apart from every
//! other failure in exactly one place. Native-only (`native` feature).

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_core::identity::ActorKeypair;

/// How a knock send ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KnockSend {
    Sent,
    /// The nest's typed guardian-approval refusal
    /// (`RpcError::is_guardian_approval_required`) — the one failure that
    /// reveals `contact-request-guardian-button`.
    RefusedByGuardian,
    Failed(String),
}

/// `build_knock_payload` → `fauna.inbox.send`. `recipient_nest_url` `None`
/// delivers on this nest; `Some(peer)` makes the home nest originate
/// `fauna.federation.inbox.deliver` to that peer. The guardian gate is
/// separated from every other failure on the shared typed predicate rather
/// than a per-app string match — which handler refused is the nest's business.
pub async fn send_knock(
    nest: Arc<NestClient>,
    node_url: &str,
    secret: [u8; 32],
    recipient: String,
    recipient_nest_url: Option<String>,
) -> KnockSend {
    let kp = ActorKeypair::from_secret(secret);
    let recipient_bytes = match fauna_core::hex32::decode(&recipient) {
        Ok(b) => b,
        Err(e) => return KnockSend::Failed(format!("recipient id: {e}")),
    };
    let payload =
        match fauna_client_core::email::build_knock_payload(&kp, &recipient_bytes, node_url) {
            Ok(p) => p,
            Err(e) => return KnockSend::Failed(format!("knock payload: {e}")),
        };
    match fauna_client_inbox::InboxClient::new(nest)
        .send(recipient, recipient_nest_url, payload)
        .await
    {
        Ok(_) => KnockSend::Sent,
        Err(fauna_client::NestClientError::Rpc(err)) if err.is_guardian_approval_required() => {
            KnockSend::RefusedByGuardian
        }
        Err(e) => KnockSend::Failed(format!("inbox send: {e}")),
    }
}
