//! The login wiring's two halves (`conversations-sink` feature): the
//! [`ArchiveLinks`] module as the conversations session's
//! [`ArchiveSignalSink`] (ruling 2 — the `CustodyCeremonySink` pattern), and
//! the session as the module's [`ArchiveSignalPoster`] over the session's
//! two archive-signal doors — or, for an app with no session (web), the
//! backend's own doors ([`BackendPoster`]). Behind a feature so the base crate
//! never links the MLS graph.

use std::sync::{Arc, Weak};

use fauna_conversations::ConversationsManager;
use fauna_conversations::backend::ArchiveSignalSink;
use fauna_conversations::backends::fauna_mls::{FaunaMlsBackend, archive_channel_for};
use fauna_conversations::session::ConversationsSession;
use fauna_core::identity::ActorId;

use super::{ArchiveLinks, ArchiveSignalPoster};
use crate::nest::ArchiveNestError;

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl ArchiveSignalSink for ArchiveLinks {
    async fn archive_signal(&self, _channel_hex: &str, sender: ActorId, bytes: &[u8]) -> bool {
        self.ingest(&sender.to_hex(), bytes).await
    }
}

/// The session's pair-channel doors as the module's poster: the existing
/// two-member channel with the contact, or one bootstrapped with nothing
/// posted (`ConversationsSession::archive_channel_for`), then the signal.
///
/// Holds the session **weakly**. The same [`ArchiveLinks`] is the session's
/// sink, parked on the session's backend, so a strong handle here would be
/// the cycle session → backend → sink → poster → session: a re-login would
/// never free the old session, and the receive loop's liveness `Weak` would
/// never retire. Once the session is gone a send fails as a transport fault
/// and stays pending in the folder for the next session's drive.
pub struct SessionPoster(Weak<ConversationsSession>);

impl SessionPoster {
    pub fn new(session: &Arc<ConversationsSession>) -> Self {
        Self(Arc::downgrade(session))
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl ArchiveSignalPoster for SessionPoster {
    async fn post(&self, contact_hex: &str, bytes: Vec<u8>) -> Result<(), ArchiveNestError> {
        let contact = parse_contact(contact_hex)?;
        let session = self
            .0
            .upgrade()
            .ok_or_else(|| ArchiveNestError::Transport("the conversations session ended".into()))?;
        let channel = session
            .archive_channel_for(contact, None)
            .await
            .map_err(|e| ArchiveNestError::Transport(e.to_string()))?;
        session
            .send_archive_signal(&channel, bytes)
            .await
            .map_err(|e| ArchiveNestError::Transport(e.to_string()))
    }
}

/// The backend's pair-channel doors as the module's poster — for an app that
/// drives a [`FaunaMlsBackend`] with no [`ConversationsSession`] around it
/// (web: `fauna-wasm`'s conversations manager). The same two calls
/// [`SessionPoster`] reaches through the session, which only forwards to
/// these: [`archive_channel_for`] (the existing two-member channel with the
/// contact, or one bootstrapped with nothing posted, its thread materialized
/// on the manager) then [`FaunaMlsBackend::send_archive_signal`].
///
/// Holds both **weakly**, for [`SessionPoster`]'s reason: the same
/// [`ArchiveLinks`] is parked on the backend as its sink, and the manager
/// holds the backend as its rail, so a strong handle to either would keep a
/// replaced account's backend alive. Once either is gone a send fails as a
/// transport fault and stays pending in the folder for the next drive.
pub struct BackendPoster {
    backend: Weak<FaunaMlsBackend>,
    manager: Weak<ConversationsManager>,
}

impl BackendPoster {
    pub fn new(backend: &Arc<FaunaMlsBackend>, manager: &Arc<ConversationsManager>) -> Self {
        Self {
            backend: Arc::downgrade(backend),
            manager: Arc::downgrade(manager),
        }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl ArchiveSignalPoster for BackendPoster {
    async fn post(&self, contact_hex: &str, bytes: Vec<u8>) -> Result<(), ArchiveNestError> {
        let contact = parse_contact(contact_hex)?;
        let ended = || ArchiveNestError::Transport("the conversations backend ended".into());
        let backend = self.backend.upgrade().ok_or_else(ended)?;
        let manager = self.manager.upgrade().ok_or_else(ended)?;
        let channel = archive_channel_for(&backend, &manager, contact, None)
            .await
            .map_err(|e| ArchiveNestError::Transport(e.to_string()))?;
        backend
            .send_archive_signal(&channel, bytes)
            .await
            .map_err(|e| ArchiveNestError::Transport(e.to_string()))
    }
}

/// A poster's contact hex back to the actor id the doors take.
fn parse_contact(contact_hex: &str) -> Result<ActorId, ArchiveNestError> {
    ActorId::from_hex(contact_hex).map_err(|e| ArchiveNestError::Rejected {
        code: "fauna.archive_link.bad_contact".to_string(),
        detail: e.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ended_poster() -> BackendPoster {
        BackendPoster {
            backend: Weak::new(),
            manager: Weak::new(),
        }
    }

    /// Once the account's backend is gone (a re-login replaced it), a send
    /// fails as a transport fault — which `ArchiveLinks` keeps pending for
    /// the next drive — never as a rejection.
    #[tokio::test]
    async fn a_backend_poster_whose_backend_ended_fails_as_transport() {
        let err = ended_poster()
            .post(&"07".repeat(32), vec![1])
            .await
            .unwrap_err();
        assert!(matches!(err, ArchiveNestError::Transport(_)), "{err:?}");
    }

    #[tokio::test]
    async fn a_backend_poster_rejects_a_malformed_contact_before_any_door() {
        let err = ended_poster().post("not hex", vec![1]).await.unwrap_err();
        assert!(matches!(err, ArchiveNestError::Rejected { .. }), "{err:?}");
    }
}
