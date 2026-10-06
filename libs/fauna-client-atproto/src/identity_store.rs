//! The account-plane door every ATProto consumer reads and writes the
//! account's ATProto state through — `fauna.state.atproto-identity`, the
//! user-custodied senior rotation keys and their custody records
//! (`config-dissolution.md` § The `__config` dissolution schedule, the kind's
//! row: born plane-only).
//!
//! This crate must stay wasm-clean and cannot name the account plane, so the
//! door is injected: [`AtprotoIdentityStore`] is the seam, and
//! `fauna_account_seams::atproto_identity::RuntimeAtprotoIdentity` its one impl over
//! a seat's `AccountStoreHandle` — the same impl on all seven apps. A web
//! chunk other than the core chunk reaches it through the account port
//! ([`crate::port`]).
//!
//! **A write is a join, never a replacement.** The plane door unions the
//! caller's replica into the stored rows and removes nothing, so a caller
//! hands in only what it wants added (or the whole custody, a copy it read
//! and changed — the two are the same join) and gets the account's custody
//! as it now stands back. That is why a
//! concurrent device's rotation key can no longer be lost to a race, because
//! no write path can shrink the set.

use fauna_core::data::AtprotoIdentityConfig;
use fauna_protocol::MaybeSendSync;

/// What a seat without an account runtime answers — the kind is plane-only,
/// so there is nowhere else to keep it.
pub const NO_ACCOUNT_RUNTIME: &str = "this app hosts no account runtime";

/// The account's ATProto state on the account plane. Every error is the
/// door's own refusal as text (an absent runtime, a store fault, the writer
/// door refusing while no generation tip resolves).
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait AtprotoIdentityStore: MaybeSendSync {
    /// The account's ATProto identity custody — empty when none rests.
    async fn atproto_identity(&self) -> Result<AtprotoIdentityConfig, String>;

    /// Join `replica` into the custody and answer it as it now stands.
    async fn merge_atproto_identity(
        &self,
        replica: AtprotoIdentityConfig,
    ) -> Result<AtprotoIdentityConfig, String>;

    /// Resolve once a read can reach the custody — for the plane door, the
    /// moment the seat's account runtime has assembled and its first catch-up
    /// pass has settled, so the read answers the synced ring. A door that never
    /// will (the default: no runtime to wait for) never resolves, so a caller
    /// always races this against a clock of its own.
    ///
    /// It exists for the session-start alert sweep, which fires before the
    /// runtime assembles and re-runs the custody check at this edge rather
    /// than leaving it unrun until the next re-sweep
    /// (`critical-alerts.md` § Mechanism → *How often the detector runs*).
    async fn until_readable(&self) {
        core::future::pending::<()>().await
    }
}

/// The [`AtprotoIdentityStore`] of a seat that hosts no account runtime (a
/// build without one, a harness driving the nest alone, a page machine not
/// yet wired): every read and every write is refused — never kept anywhere
/// else, the kind being plane-only. A refused read is "cannot verify", which
/// every custody check already treats as quiet-and-retry.
pub struct NoAccountRuntime;

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl AtprotoIdentityStore for NoAccountRuntime {
    async fn atproto_identity(&self) -> Result<AtprotoIdentityConfig, String> {
        Err(NO_ACCOUNT_RUNTIME.into())
    }

    async fn merge_atproto_identity(
        &self,
        _replica: AtprotoIdentityConfig,
    ) -> Result<AtprotoIdentityConfig, String> {
        Err(NO_ACCOUNT_RUNTIME.into())
    }
}

/// An in-memory [`AtprotoIdentityStore`] with the plane door's semantics —
/// the stored custody joined through `AtprotoIdentityConfig::merge`, nothing
/// ever removed — for tests that drive a consumer without a runtime.
#[cfg(any(test, feature = "test-fixtures"))]
#[derive(Default, Clone)]
pub struct InMemoryAtprotoIdentityStore(std::sync::Arc<std::sync::Mutex<AtprotoIdentityConfig>>);

#[cfg(any(test, feature = "test-fixtures"))]
impl InMemoryAtprotoIdentityStore {
    /// A store already holding `custody`.
    pub fn holding(custody: AtprotoIdentityConfig) -> Self {
        Self(std::sync::Arc::new(std::sync::Mutex::new(custody)))
    }

    /// The custody as it now stands, synchronously.
    pub fn current(&self) -> AtprotoIdentityConfig {
        self.0.lock().unwrap().clone()
    }

    /// Overwrite the custody outright — never a door the plane has (its
    /// write is a join); it models a device whose rows for some element have
    /// not synced, which a test cannot reach through the join.
    pub fn replace(&self, custody: AtprotoIdentityConfig) {
        *self.0.lock().unwrap() = custody;
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl AtprotoIdentityStore for InMemoryAtprotoIdentityStore {
    async fn atproto_identity(&self) -> Result<AtprotoIdentityConfig, String> {
        Ok(self.current())
    }

    async fn merge_atproto_identity(
        &self,
        replica: AtprotoIdentityConfig,
    ) -> Result<AtprotoIdentityConfig, String> {
        let mut stored = self.0.lock().unwrap();
        *stored = stored.merge(&replica);
        Ok(stored.clone())
    }

    /// Always readable — there is no runtime to wait for.
    async fn until_readable(&self) {}
}
