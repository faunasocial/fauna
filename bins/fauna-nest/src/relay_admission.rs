//! Who may use the nest's peer relay — the nest's answer to the relay sidecar's
//! `fauna.relay.admit` question.
//!
//! Authority: `docs/goal/behavior/p2p.md` § The relay — *the devices of the
//! nest's own members, and nobody else*. The relay (`bins/fauna-iroh-relay`)
//! holds no list: it asks about every connecting endpoint over its sidecar
//! channel (`crate::sidecar_channel`), and serves the endpoint only on a yes.
//!
//! **The answer is a lookup over a set of sources** ([`AdmissionSource`]): a
//! key is known when any one source knows it. Today there are two. A later
//! ruling that widens admission adds a variant and its arm in
//! [`AdmissionSource::knows`]; nothing else changes, and the relay never learns
//! which source said yes. **Across nests the answer is no** (§ The relay →
//! *Across nests*): a key that belongs to a contact, a co-member of a shared
//! set or a custodian whose account lives on another nest is in neither source.

use crate::db::CacheDb;

/// One place the nest may know an endpoint key from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AdmissionSource {
    /// The store device principal of a device enrolled for one of this nest's
    /// accounts and not since removed — the peer leg's node identity
    /// (`account-sync-plane.md` § The peer leg).
    EnrolledDevice,
    /// The actor key of an account on this nest — the contact plane's node
    /// identity (`p2p.md`, PT-1b).
    MemberActor,
}

impl AdmissionSource {
    /// Every source, in the order they are asked.
    pub(crate) const ALL: [AdmissionSource; 2] = [
        AdmissionSource::EnrolledDevice,
        AdmissionSource::MemberActor,
    ];

    /// Whether this source knows `key`. A database error is a no: the relay
    /// fails closed, and the endpoint's own reconnect asks again.
    async fn knows(self, db: &CacheDb, key: &[u8; 32]) -> bool {
        let found = match self {
            AdmissionSource::EnrolledDevice => db.is_enrolled_device_principal(key).await,
            AdmissionSource::MemberActor => db.is_actor_registered(key).await,
        };
        found.unwrap_or_else(|e| {
            tracing::warn!(source = ?self, error = %format!("{e:#}"), "relay admission lookup failed; refusing");
            false
        })
    }
}

/// Whether the relay may serve the endpoint whose key is `key`.
pub(crate) async fn key_is_known(db: &CacheDb, key: &[u8; 32]) -> bool {
    for source in AdmissionSource::ALL {
        if source.knows(db, key).await {
            return true;
        }
    }
    false
}
