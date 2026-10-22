//! Leave a shared folder set: nest roster self-drop (durable — same-nest, or
//! relayed to the set's home nest for a foreign/federated one) followed by the
//! local MLS-group forget (idempotent). The ONE sequence every app runs —
//! native (`fauna-ffi`, linux, tui) and web (`fauna-wasm`'s `foldersLeave`)
//! alike. `mls`-gated: it drives the conversations rail's
//! [`FaunaMlsBackend`], which every app holds (native via
//! `ConversationsSession::backend`).

use fauna_conversations::backends::fauna_mls::FaunaMlsBackend;
use fauna_protocol::RpcRequester;

use crate::FoldersClient;

/// (1) Nest roster self-drop — relayed to the set's recorded home nest when
/// foreign (federated), same-nest otherwise ([`FoldersClient::leave_with_home`]
/// resolves the home URL's `None`/`Some` split from the caller). (2) Locally
/// forget the MLS group and the foreign-set record (idempotent). Both steps are
/// best-effort in sequence: a roster-drop failure stops before the local
/// forget, so a retry re-attempts the whole leave rather than leaving the
/// client's local state ahead of the nest's. An unreadable foreign-set custody
/// record (home UNKNOWN) is such a failure too: it stops before either step,
/// keeping the record for the retry. A `left: false` reply is NOT one — with
/// the home known it is the idempotent already-gone answer, and failing it
/// would strand a crash-resumed leave.
pub async fn leave_share<R: RpcRequester>(
    nest: R,
    backend: &FaunaMlsBackend,
    group_id: String,
) -> Result<(), String> {
    // Fail closed: an unreadable custody record leaves the set's home UNKNOWN,
    // and a same-nest leave of a foreign set reports success while the home
    // nest keeps the member — then the forget below would delete the only
    // record a retry needs. Stop before any mutation.
    let home_url = backend.folder_home_url(&group_id).await?;
    FoldersClient::new(nest)
        .leave_with_home(group_id.clone(), home_url)
        .await
        .map_err(|e| e.to_string())?;
    backend
        .leave_folder(&group_id)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}
