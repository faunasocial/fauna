//! Re-exports the page-level Media state machine so its UniFFI exports surface in
//! the generated Swift / Kotlin / C# bindings, plus a free-fn constructor that
//! builds the machine over an [`FfiNestClient`]'s WS-RPC connection. The machine
//! itself lives in libs/fauna-media-machine; this file is a thin glue layer
//! (mirrors src/devices.rs).

use std::sync::Arc;

pub use fauna_media_machine::{
    FileVersionSummary, MediaApiError, MediaItemSummary, MediaMachine, MediaObserver,
    MediaPageSnapshot,
};

use crate::FfiNestClient;

/// Build a [`MediaMachine`] for the Media page over `nest`'s authenticated WS-RPC
/// connection. `observer` ticks on every snapshot change. The machine owns the
/// cross-set all-media read (`refresh()`) and the client-held view-state gestures
/// (`set_sort` / `set_descending` / `set_filter` / `set_view_grid`) — the content
/// plane (`media.md` rule 4: Media reads folders, never configures them).
/// Render the page off `snapshot()`.
///
/// The machine is built with the shared folder custody resolver
/// (`NestFolderKeyResolver` over the connection's own identity) — the same
/// `build_media_machine_with_folder_keys` linux, tui and web build through — so
/// a **shared** set's delete/restore paths seal resolver-first and fail closed
/// (`path-sealing.md` § S8 D2, never the owner-root fallback), a member's
/// `download_file` opens the owner's files under the content keys in its own
/// custody, and change records go out signed. A bearer-only connection (no
/// identity key) has nothing to resolve with and builds owner-only.
#[uniffi::export]
pub fn build_media_machine(
    nest: Arc<FfiNestClient>,
    observer: Arc<dyn MediaObserver>,
) -> Arc<MediaMachine> {
    let nest_arc = nest.nest_arc();
    #[cfg(all(feature = "folders-author", not(target_arch = "wasm32")))]
    let folder_keys = nest_arc.auth().keypair().map(|_| {
        Arc::new(fauna_client_folders::NestFolderKeyResolver::new(
            Arc::clone(&nest_arc),
            crate::account_runtime::folder_key_store(),
        )) as Arc<dyn fauna_core::folder_keys::FolderKeyResolver>
    });
    #[cfg(not(all(feature = "folders-author", not(target_arch = "wasm32"))))]
    let folder_keys = None;
    fauna_media_machine::build_media_machine_with_folder_keys(nest_arc, observer, folder_keys)
}

/// Wire the Media page's **followed public folders** source — the
/// UniFFI-reachable equivalent of tui's and linux's direct
/// `MediaMachine::set_followed_media_source(StoreFollowedFoldersSource::new(…))`
/// call, and the native twin of `fauna-wasm-media`'s `setFollowedMediaSource`
/// (`docs/goal/ui/media.md` § Followed public folders).
///
/// **This one is not free, unlike the gestures.** `MediaMachine`'s gesture impl
/// block is `#[uniffi::export]`ed wholesale, so `select_followed_scope` /
/// `download_followed` reach the native apps on a binding regen alone — but
/// `set_followed_media_source` takes an `Arc<dyn FollowedMediaSource>`, which is
/// not a UniFFI-expressible argument, so it sits outside that block and needs
/// this hand-written seam. Web learned the same lesson the expensive way: its
/// face shipped the two gestures with no source, so `snapshot.followed` was
/// permanently empty and both gestures had nothing to act on — a face that
/// compiles and is unreachable.
///
/// Unwired, the page carries no followed scopes: `media-folder-filter` offers
/// only the user's own sets and `select_followed_scope` has nothing to select.
/// That is the correct render for a page that has not built the surface, not an
/// error. Call once right after [`build_media_machine`], before the first
/// `refresh()`, beside the owner-`BackupKey` wiring.
///
/// The SAME type backs the Devices page's followed rows
/// (`wire_devices_followed_folders`), so the availability verdicts a
/// browse fetch writes and the ones the staleness-budgeted probe writes are one
/// mechanism per page rather than two racing ones — the shared source owns that
/// cache. The follows come from the account store (`fauna.state.follows` —
/// the home nest keeps no follower state); `owner_secret` is the caller's own
/// 32-byte actor secret, still checked though it derives nothing.
#[uniffi::export]
pub fn wire_media_followed_folders(
    media: Arc<MediaMachine>,
    nest: Arc<FfiNestClient>,
    owner_secret: Vec<u8>,
) -> Result<(), crate::FfiError> {
    media.set_followed_media_source(crate::build_followed_folders_source(&nest, owner_secret)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NullObserver;
    impl MediaObserver for NullObserver {
        fn on_changed(&self) {}
    }

    /// Until 2026-09-29 `build_media_machine` handed every FFI app (apple/
    /// android/windows) a resolver-less `MediaMachine`, so a delete or restore
    /// in a SHARED set sealed its path under the owner root no roster member
    /// can open — the owner-root fallback `path-sealing.md` § S8 D2 forbids a
    /// bound set — and a member's `download_file` had no content keys to open
    /// the owner's files with. linux/tui/web build through
    /// `build_media_machine_with_folder_keys` themselves; the three FFI apps
    /// all funnel through this one free fn, so the resolver is wired here — the
    /// same call-site pin `devices.rs` holds for the Devices builder.
    ///
    /// Mutation: pass `None` for the resolver above → `has_folder_keys()` is
    /// false → this pin reds (`cargo test -p fauna-ffi --lib media::`).
    #[cfg(all(feature = "folders-author", not(target_arch = "wasm32")))]
    #[test]
    fn the_builder_hands_the_machine_the_folder_key_resolver() {
        let nest = FfiNestClient::new("wss://unreachable.invalid".into(), vec![7u8; 32]).unwrap();
        let observer: Arc<dyn MediaObserver> = Arc::new(NullObserver);
        let machine = build_media_machine(nest, observer);
        assert!(
            machine.has_folder_keys(),
            "a shared set's gesture paths must seal resolver-first and its files open \
             under the reader's content keys — never the owner-root fallback",
        );
    }
}
