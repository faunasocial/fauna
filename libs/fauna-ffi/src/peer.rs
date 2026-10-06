use std::sync::Mutex;

use fauna_peer::contact::{PeerContact, PeerDb};

// ── Error type ──

#[derive(uniffi::Error, Debug, thiserror::Error)]
pub enum FfiPeerError {
    #[error("Database error: {0}")]
    Database(String),
    #[error("Invalid input: {0}")]
    InvalidInput(String),
}

// ── Record types ──

#[derive(uniffi::Record)]
pub struct FfiPeerContact {
    pub actor_id: Vec<u8>,
    pub display_name: String,
    pub p2p_enabled: bool,
    pub met_in_person: bool,
}

// ── Conversion helpers ──

impl From<PeerContact> for FfiPeerContact {
    fn from(c: PeerContact) -> Self {
        Self {
            actor_id: c.actor_id.to_vec(),
            display_name: c.display_name,
            p2p_enabled: c.p2p_enabled,
            met_in_person: c.met_in_person,
        }
    }
}

impl FfiPeerContact {
    /// Convert back to a `PeerContact`, validating that Vec lengths are 32.
    fn to_peer_contact(&self) -> Result<PeerContact, FfiPeerError> {
        let actor_id: [u8; 32] = self
            .actor_id
            .as_slice()
            .try_into()
            .map_err(|_| FfiPeerError::InvalidInput("actor_id must be 32 bytes".into()))?;
        Ok(PeerContact {
            actor_id,
            display_name: self.display_name.clone(),
            p2p_enabled: self.p2p_enabled,
            met_in_person: self.met_in_person,
            // Defaults for fields not exposed to mobile clients
            last_endpoint: None,
            last_connected: None,
            success_rate: 0.0,
            backoff_level: 0,
            tunnel_ip: None,
            lan_endpoints: vec![],
            stun_endpoint: None,
            feed_sync_enabled: false,
        })
    }
}

// ── Object: FfiPeerDb ──

#[derive(uniffi::Object)]
pub struct FfiPeerDb {
    inner: Mutex<PeerDb>,
}

#[uniffi::export]
impl FfiPeerDb {
    #[uniffi::constructor]
    pub fn open(path: String) -> Result<Self, FfiPeerError> {
        let db = PeerDb::open(&path).map_err(|e| FfiPeerError::Database(e.to_string()))?;
        Ok(Self {
            inner: Mutex::new(db),
        })
    }

    pub fn upsert_contact(&self, contact: FfiPeerContact) -> Result<(), FfiPeerError> {
        let pc = contact.to_peer_contact()?;
        let db = self
            .inner
            .lock()
            .map_err(|e| FfiPeerError::Database(format!("lock poisoned: {e}")))?;
        db.upsert_contact(&pc)
            .map_err(|e| FfiPeerError::Database(e.to_string()))
    }

    pub fn get_contact(&self, actor_id: Vec<u8>) -> Result<Option<FfiPeerContact>, FfiPeerError> {
        let arr: [u8; 32] = actor_id
            .as_slice()
            .try_into()
            .map_err(|_| FfiPeerError::InvalidInput("actor_id must be 32 bytes".into()))?;
        let db = self
            .inner
            .lock()
            .map_err(|e| FfiPeerError::Database(format!("lock poisoned: {e}")))?;
        let result = db
            .get_contact(&arr)
            .map_err(|e| FfiPeerError::Database(e.to_string()))?;
        Ok(result.map(FfiPeerContact::from))
    }

    pub fn list_p2p_enabled(&self) -> Result<Vec<FfiPeerContact>, FfiPeerError> {
        let db = self
            .inner
            .lock()
            .map_err(|e| FfiPeerError::Database(format!("lock poisoned: {e}")))?;
        let contacts = db
            .list_p2p_enabled()
            .map_err(|e| FfiPeerError::Database(e.to_string()))?;
        Ok(contacts.into_iter().map(FfiPeerContact::from).collect())
    }

    /// Remove a P2P contact from this device.
    ///
    /// **This is the survivor of the three removal spellings** (`p2p.md`
    /// § Implementation status today): local-only removal is what
    /// `file-sync.md` § P2P peer state ratified — peer state is device-local.
    /// The cross-device `revoke_contact`/tombstone rail (superseded, same
    /// disposition as the `SyncableContact` twins below) was deleted
    /// 2026-08-25 — build the app affordance on this method instead.
    /// Uncalled as of 2026-08-02; no app renders a removal affordance yet.
    pub fn delete_contact(&self, actor_id: Vec<u8>) -> Result<(), FfiPeerError> {
        let arr: [u8; 32] = actor_id
            .as_slice()
            .try_into()
            .map_err(|_| FfiPeerError::InvalidInput("actor_id must be 32 bytes".into()))?;
        let db = self
            .inner
            .lock()
            .map_err(|e| FfiPeerError::Database(format!("lock poisoned: {e}")))?;
        db.delete_contact(&arr)
            .map_err(|e| FfiPeerError::Database(e.to_string()))
    }

    pub fn set_feed_sync(&self, actor_id: Vec<u8>, enabled: bool) -> Result<(), FfiPeerError> {
        let id: [u8; 32] = actor_id
            .try_into()
            .map_err(|_| FfiPeerError::InvalidInput("actor_id must be 32 bytes".into()))?;
        let db = self.inner.lock().unwrap();
        let mut contact = db
            .get_contact(&id)
            .map_err(|e| FfiPeerError::Database(e.to_string()))?
            .ok_or(FfiPeerError::Database("contact not found".into()))?;
        contact.feed_sync_enabled = enabled;
        db.upsert_contact(&contact)
            .map_err(|e| FfiPeerError::Database(e.to_string()))
    }
}

// ── Free functions ──
//
// The QR/URI invite faces (`peer_generate_invite`/`peer_parse_invite`) were
// deleted 2026-07-15: consumerless fleet-wide after the apple WireGuard-era
// P2P UI retired (docs/goal/behavior/p2p.md § invite faces).
//
// `peer_contact_to_sync_json` / `peer_contact_from_sync_json` /
// `peer_contact_sync_namespace` (the `SyncableContact`-based namespace-sync
// trio) were deleted 2026-08-10 (Y.1 reframe, disposition executed): zero
// production callers on any client, and `file-sync.md` § P2P peer state
// rules this state device-local — the bespoke sync namespace never gets
// wired (`docs/goal/behavior/p2p.md` § Implementation status today).
//
// `FfiPeerDb::revoke_contact` (+ the backing `fauna_peer::revocation` module,
// `apply_tombstone` included) carried the same dark-rail note and was
// deleted 2026-08-25, same Y.1 reframe disposition and reason: zero callers
// on any client, and the tombstone it minted reached nothing. The
// `fauna_peer::contact_sync` module those two rails shared
// (`SyncableContact`, `SyncTombstone`) followed them 2026-10-02.
//
// The P2P node exports (`peer_tunnel_{start,stop,is_active}` and their
// `FfiPeerTunnel*` records, gated on the `tunnel` feature) were deleted
// 2026-10-02: no build had enabled `tunnel` since android's dead
// `P2PTunnelService` went (2026-08-25), and linux's Settings → P2P is the
// only peer-node host there will be (`docs/goal/behavior/p2p.md` § Element
// IDs). linux drives `fauna-peer-channel`'s `PeerNode` directly.

// The WireGuard signaling FFI (`FfiSignalMessage`, `signal_build_*`,
// `signal_parse`, `signal_is_schema`) was deleted 2026-08-23 with the whole
// WireGuard stack (user-directed; `docs/goal/behavior/p2p.md` — iroh-QUIC is
// the only substrate). It brokered a WG tunnel through the inbox: every
// payload carried a `wg_public_key` and the accept carried a `tunnel_ip`.
// iroh dials from the peer leg's own discovery entries
// (`fauna_peer_sync::discovery`), so there is nothing left to broker.

// `nestless_validate` / `nestless_limitations` (+ their `FfiNestlessConfig` /
// `FfiValidationIssue` records) were deleted 2026-08-10 (Y.1 reframe,
// disposition executed): "nestless mode" is dissolved as a concept
// (`docs/goal/architecture/account-data-plane.md` § The peer leg — a nest
// outage is not a special mode) and the linux switch that was their only
// caller is gone. `docs/goal/behavior/p2p.md` § Implementation status
// today owns the disposition.
