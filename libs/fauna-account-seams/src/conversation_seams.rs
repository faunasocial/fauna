//! The **one** place an app registers the conversations seams that rest on
//! the account store — called at the account-store-ready edge (never at
//! login: the store resolves after it), identically by every app that hosts
//! a runtime, so no app can wire one seam and forget its sibling.
//!
//! Today that is five seams: the community class's group-reception keys
//! ([`crate::group_reception`]), the fauna-native rail's read positions
//! ([`crate::read_positions`]), the private contact overlay projection
//! ([`crate::contact_overlays`]), the succession witness's peer anchors
//! ([`crate::peer_anchors`]) and the refused inbound scheduling changes
//! ([`crate::refused_changes`]). A future account-plane seam joins here, not
//! at the call sites.

use fauna_account_plane::account_driver::AccountStoreHandle;
use std::sync::Arc;

use fauna_conversations::backends::fauna_mls::FaunaMlsBackend;
use fauna_conversations::{ConversationsManager, ConversationsSession};

use crate::spawner::TaskSpawner;

/// Register every account-plane conversations seam on `session` over
/// `store`; the read-position and overlay seams' tasks start on `spawner`
/// (a `tokio::runtime::Handle` natively, `LocalSpawner` on web).
///
/// Wiring the same pair twice is harmless — a later read-position or overlay
/// registration retires the earlier one (its deliveries are refused, its
/// writer ends with the dropped seam), and the group-reception keys keep the
/// first — so a host whose session and store land in either order may call
/// this from both edges without a lock between them.
pub fn wire(session: &ConversationsSession, store: AccountStoreHandle, spawner: &impl TaskSpawner) {
    wire_parts(&session.manager(), &session.backend(), store, spawner);
}

/// [`wire`] over the two objects a session holds, for a host that builds them
/// without one — web, whose receive loop is JS-owned, holds the manager and
/// its FaunaMls backend directly (`fauna-wasm`'s `WasmConversationsManager`).
/// The one body both edges run, so neither can register a seam the other
/// forgets.
pub fn wire_parts(
    manager: &Arc<ConversationsManager>,
    fauna_mls: &FaunaMlsBackend,
    store: AccountStoreHandle,
    spawner: &impl TaskSpawner,
) {
    fauna_mls.set_group_reception_keys(crate::group_reception::AccountGroupReceptionKeys::new(
        store.clone(),
    ));
    manager.register_peer_anchor_store(Some(Arc::new(
        crate::peer_anchors::AccountPeerAnchorStore::new(store.clone()),
    )));
    crate::contact_overlays::register(manager, store.clone(), spawner);
    crate::refused_changes::register(manager, store.clone(), spawner);
    crate::read_positions::register(manager, store, spawner);
}
