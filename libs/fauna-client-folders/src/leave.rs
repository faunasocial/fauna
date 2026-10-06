//! Leave a shared folder set: nest roster self-drop (durable — same-nest, or
//! relayed to the set's home nest for a foreign/federated one) followed by the
//! local MLS-group forget (idempotent). The two-step sequence every native app
//! re-implemented per-app identically. `mls`-gated: forgetting the local group
//! needs [`fauna_conversations::ConversationsSession`].

use std::sync::Arc;

use fauna_conversations::ConversationsSession;
use fauna_protocol::RpcRequester;

use crate::FoldersClient;

/// (1) Nest roster self-drop — relayed to the set's recorded home nest when
/// foreign (federated), same-nest otherwise ([`FoldersClient::leave_with_home`]
/// resolves the home URL's `None`/`Some` split from the caller). (2) Locally
/// forget the MLS group (idempotent). Both steps are best-effort in sequence:
/// a roster-drop failure stops before the local forget, so a retry re-attempts
/// the whole leave rather than leaving the client's local state ahead of the
/// nest's.
pub async fn leave_share<R: RpcRequester>(
    nest: R,
    session: &Arc<ConversationsSession>,
    group_id: String,
) -> Result<(), String> {
    let home_url = session.folder_home_url(&group_id).await;
    FoldersClient::new(nest)
        .leave_with_home(group_id.clone(), home_url)
        .await
        .map_err(|e| e.to_string())?;
    session
        .leave_folder(group_id)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}
