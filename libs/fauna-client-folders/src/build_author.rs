//! Assemble the owner-side shared-folder author from a live nest connection +
//! resolved actor keypair: the thin `fauna.folders.*` client, the owner's
//! identity, the account's folder-key custody (the seat's
//! [`FolderKeyStore`]), its mail custody (the MSEK a served set's
//! `WebdavKeysBlob` seals under), and the conversations rail's shared
//! per-actor `MlsEngine` as the group-crypto seam, wired with its
//! `FolderCommitGate` (so a member removal rides the device-owned-epoch rebase
//! loop when the multi-device plane is wired, the ungated staged discipline
//! otherwise — see [`FoldersAuthor::with_commit_gate`]).
//!
//! Every native call site hand-copied this exact four-line ceremony before
//! this — `fauna-ffi::folders_author::author`, and tui's and linux's own
//! `build_folders_author` — each one's own doc comment already naming at
//! least one sibling as the thing it mirrors. `mls`-gated (needs
//! [`ConversationsSession`]) + native-gated, like [`crate::leave`].

use std::sync::Arc;

use fauna_conversations::ConversationsSession;
use fauna_core::identity::ActorKeypair;
use fauna_mls::engine::MlsEngine;
use fauna_protocol::{RpcErrorClass, RpcRequester};

use crate::FoldersClient;
use crate::key_reader::FolderKeyStore;
use crate::orchestration::FoldersAuthor;

/// See module doc.
pub fn build_folders_author<R: RpcRequester + Clone>(
    nest: R,
    keypair: ActorKeypair,
    custody: Arc<dyn FolderKeyStore>,
    mail: Arc<dyn fauna_client_config::MailStore>,
    session: &ConversationsSession,
) -> FoldersAuthor<R, Arc<MlsEngine>>
where
    R::Error: RpcErrorClass,
{
    let files = FoldersClient::new(nest);
    FoldersAuthor::new(files, keypair, custody, mail, session.engine())
        .with_commit_gate(Arc::new(session.backend()))
}
