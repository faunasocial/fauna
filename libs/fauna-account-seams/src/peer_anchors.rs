//! The **peer-anchor store** over the account store — the succession
//! witness's held chain heads and harvested home domains for other identities
//! (`fauna.state.peer-anchors`; `identity-succession.md` § The succession
//! statement → *the peer-profile harvest* owns the anchors,
//! `config-dissolution.md` § The `__config` dissolution schedule the kind).
//! One implementation for every app that hosts the account runtime
//! (priority #2), registered on the manager by
//! [`crate::conversation_seams::wire_parts`] beside the read positions and the
//! contact overlay.
//!
//! A thin adapter and nothing more: the fold, the ceiling and the
//! read-join-put all live behind the handle's typed door
//! (`fauna_account_plane::peer_anchor_rows`), and the anchor rules on
//! `fauna_core::data::PeerAnchors`. Born plane-only — nothing on this path
//! reads or writes any other store.

use fauna_account_plane::account_driver::AccountStoreHandle;
use fauna_core::data::PeerAnchors;

/// `fauna_conversations::backend::PeerAnchorStore` over this account's store.
pub struct AccountPeerAnchorStore {
    store: AccountStoreHandle,
}

impl AccountPeerAnchorStore {
    /// The anchors of the account `store` serves.
    pub fn new(store: AccountStoreHandle) -> Self {
        Self { store }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl fauna_conversations::backend::PeerAnchorStore for AccountPeerAnchorStore {
    async fn peer_anchors(&self) -> Result<PeerAnchors, String> {
        self.store
            .peer_anchors()
            .await
            .map_err(|e| format!("{e:#}"))
    }

    async fn merge_peer_anchors(&self, replica: PeerAnchors) -> Result<PeerAnchors, String> {
        self.store
            .merge_peer_anchors(replica)
            .await
            .map_err(|e| format!("{e:#}"))
    }
}
