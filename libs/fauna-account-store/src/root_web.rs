//! The account-store root — **web's leg** (mounted as `crate::root` on wasm32;
//! the native leg is `root.rs`).
//!
//! A browser origin has no filesystem to root a store in, so where the native
//! leg resolves `<platform state base>/<actor-id-hex>/account-store/`, web
//! resolves a **store name**: `<root>/<actor-id-hex>`, per origin (the browser
//! partitions IndexedDB and OPFS by origin, as the OS partitions a home dir by
//! login). That one name is the IndexedDB database, the OPFS segment
//! directory ([`crate::indexeddb::IndexedDbBackend::open`]) and the engine
//! lock's key ([`crate::locks::engine_lock_name`]). The actor id goes through
//! the same actor-scope floor as the native state dir, so junk hex is refused
//! rather than minted into a stray store.
//!
//! What stays the same across the legs is the charter's point: one store per
//! (user, account), shared by every instance of the app that user runs —
//! tabs here, processes natively (`account-data-plane.md` § The account store).

use anyhow::Result;

use crate::physical::normalize_actor_hex;

/// The production root every store name sits under.
pub const PLATFORM_ROOT: &str = "fauna-account-store";

/// The resolved account-store root: the prefix the per-actor store names live
/// directly under. [`Self::platform`] in production; [`Self::at`] for tests
/// that need an isolated namespace.
#[derive(Debug, Clone)]
pub struct StoreRoot(String);

impl StoreRoot {
    /// The production root — [`PLATFORM_ROOT`].
    pub fn platform() -> Self {
        Self(PLATFORM_ROOT.to_owned())
    }

    /// An explicit root (tests).
    pub fn at(base: impl Into<String>) -> Self {
        Self(base.into())
    }

    /// The root itself.
    pub fn base(&self) -> &str {
        &self.0
    }

    /// The store name for one actor: `<root>/<actor-id-hex>`, the hex
    /// normalized to its one lowercase spelling (two spellings of one actor
    /// are one store) and refused when it is not an actor id.
    pub fn store_name(&self, actor_id_hex: &str) -> Result<String> {
        Ok(format!("{}/{}", self.0, normalize_actor_hex(actor_id_hex)?))
    }
}
