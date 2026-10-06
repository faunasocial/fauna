//! The one builder every serve face wires into [`crate::orchestration::FoldersAuthor::serve_set`]:
//! the flipping client's served-set walk (`webdav-server.md` § Key model, the
//! app-side seams bullet, part (c)) over a seat's own session — the seat's
//! custody resolver ([`crate::NestFolderKeyResolver`] over the account's
//! folder-key custody) plus the Media seam's
//! byte plane and signed record (`fauna_media_machine::served_reseal`).
//!
//! One function per transport, so the FFI face, the wasm face and tui build the
//! walk identically — the faces differ only in the transport they hold and the
//! recording device the app names (the one its Media gestures record under).

use std::sync::Arc;

use fauna_core::folder_keys::FolderKeyResolver;
use fauna_core::identity::ActorKeypair;
use fauna_core::nest_reseal::ServedSetConverge;

use crate::{FolderKeyReader, NestFolderKeyResolver};

/// The walk over a native session (`Arc<NestClient>`). `predecessors` is the
/// account's attested predecessor ids
/// (`AccountRegistry::attested_predecessor_actor_ids`; ruling (8)(b) source
/// (ii)): handed in, a head a retired identity signed is admitted with no
/// `fauna.recovery.succession.lookup` round trip; empty, the seat proves the
/// link by the statement walk.
#[cfg(not(target_arch = "wasm32"))]
pub fn served_set_converge(
    nest: Arc<fauna_client::NestClient>,
    keypair: &ActorKeypair,
    custody: Arc<dyn FolderKeyReader>,
    device_id: String,
    predecessors: Vec<[u8; 32]>,
) -> Arc<dyn ServedSetConverge> {
    Arc::new(native_walk(nest, keypair, custody, device_id, predecessors))
}

#[cfg(not(target_arch = "wasm32"))]
fn native_walk(
    nest: Arc<fauna_client::NestClient>,
    keypair: &ActorKeypair,
    custody: Arc<dyn FolderKeyReader>,
    device_id: String,
    predecessors: Vec<[u8; 32]>,
) -> fauna_media_machine::served_reseal::ServedSetWalk<Arc<fauna_client::NestClient>> {
    let resolver: Arc<dyn FolderKeyResolver> =
        Arc::new(NestFolderKeyResolver::new(nest.clone(), custody));
    fauna_media_machine::served_reseal::served_set_walk(nest, keypair, resolver, device_id)
        .with_predecessors(predecessors)
}

/// The walk over the SPA's browser session (`WsRpcClient`).
#[cfg(target_arch = "wasm32")]
pub fn served_set_converge(
    nest: fauna_rpc_wasm::WsRpcClient,
    keypair: &ActorKeypair,
    custody: Arc<dyn FolderKeyReader>,
    device_id: String,
    predecessors: Vec<[u8; 32]>,
) -> Arc<dyn ServedSetConverge> {
    let resolver: Arc<dyn FolderKeyResolver> =
        Arc::new(NestFolderKeyResolver::new(nest.clone(), custody));
    Arc::new(
        fauna_media_machine::served_reseal::served_set_walk(nest, keypair, resolver, device_id)
            .with_predecessors(predecessors),
    )
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::key_reader::MemoryFolderKeyStore;

    /// The walk's reader seat carries the attested predecessor ids handed to
    /// [`served_set_converge`] — the input that lets it admit a retired
    /// identity's head with no `fauna.recovery.succession.lookup` (the judge's
    /// own pin: `row_judge::tests::a_successors_inherited_media_lists_and_carries_who_signed_it`).
    /// Mutation: drop `.with_predecessors(..)` in [`native_walk`] and this reds.
    #[test]
    fn the_served_set_walk_carries_the_attested_predecessor_ids() {
        let kp = ActorKeypair::generate();
        let nest = fauna_client::NestClient::new(
            "ws://127.0.0.1:0/ws".into(),
            ActorKeypair::from_secret(*kp.secret_bytes()),
        );
        let custody: Arc<dyn FolderKeyReader> = Arc::new(MemoryFolderKeyStore::default());
        let ids = vec![[7u8; 32], [8u8; 32]];
        let walk = native_walk(
            nest.clone(),
            &kp,
            custody.clone(),
            "dev".into(),
            ids.clone(),
        );
        assert_eq!(walk.predecessors(), &ids[..]);
        let bare = native_walk(nest, &kp, custody, "dev".into(), Vec::new());
        assert!(
            bare.predecessors().is_empty(),
            "no ids → the statement walk"
        );
    }
}
