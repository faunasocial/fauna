//! The **ATProto credential seam** — where the page machine keeps the secrets
//! it mints: the account plane's `fauna.state.atproto` kind, one row per
//! credential (`docs/goal/architecture/config-dissolution.md` § The
//! `__config` dissolution schedule — the kinds table's row;
//! `atproto-pds-full.md` § D3 owns the custody rule: the nest holds only the
//! verifier, so the recoverable copy is client-held and fleet-synced).
//!
//! The kind is plane-only: there is no other copy behind this
//! seam, so an absent account runtime is a refusal, never a fallback. The
//! machine stays wasm-clean and cannot name the plane, so the seam is
//! injected ([`crate::AtprotoSettingsMachine::set_credential_store`]):
//!
//! - the six native seats wire `fauna_account_seams::atproto_credentials`'s
//!   one implementation over their account-store handle;
//! - web's ATProto chunk wires [`crate::port::PortAtprotoCredentials`], the
//!   account port's forwarder, whose calls the core chunk answers through that
//!   same implementation (`account-client-lifecycle.md` § The client-side
//!   lifecycle → *The account port*, decision (h): `atproto`,
//!   `put_app_credential`, `revoke_app_credential`).

use fauna_client_config::StoreError;
use fauna_core::data::{AtprotoAppCredential, AtprotoConfig};

/// The account's minted ATProto app credentials, as the page machine reads
/// and writes them. Each method mirrors the account-store handle's door of the
/// same name (`AccountStoreHandle::{atproto, put_app_credential,
/// revoke_app_credential}`); a read failure is [`StoreError::Load`], a write
/// failure — the no-tip refusal included — [`StoreError::Save`].
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait AtprotoCredentialStore: fauna_core::MaybeSendSync {
    /// Every live credential this account holds a secret for, oldest first.
    async fn atproto(&self) -> Result<AtprotoConfig, StoreError>;
    /// Put one credential at its own `credential_id` (latest-wins per
    /// credential). Whether anything was written.
    async fn put_app_credential(
        &self,
        credential: AtprotoAppCredential,
    ) -> Result<bool, StoreError>;
    /// Revoke the credential at `credential_id` (a stamped tombstone). Whether
    /// anything was written — `false` when no live row was there.
    async fn revoke_app_credential(&self, credential_id: String) -> Result<bool, StoreError>;
}

/// What the seam answers before a host wires it: every call refused. A
/// machine the host never wired must not mint a credential whose secret it
/// then has nowhere to keep.
pub(crate) const NOT_WIRED: &str = "the ATProto credential store is not wired";

/// An in-memory [`AtprotoCredentialStore`] for the machine's tests: the plane's
/// shape (one row per credential, read oldest first) with armable faults.
#[cfg(any(test, feature = "test-helpers"))]
#[derive(Clone, Default)]
pub struct FakeCredentialStore {
    inner: std::sync::Arc<std::sync::Mutex<FakeInner>>,
}

#[cfg(any(test, feature = "test-helpers"))]
#[derive(Default)]
struct FakeInner {
    rows: Vec<AtprotoAppCredential>,
    fail_load: bool,
    fail_save: bool,
}

#[cfg(any(test, feature = "test-helpers"))]
impl FakeCredentialStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// The credentials as the store now holds them.
    pub fn current(&self) -> AtprotoConfig {
        AtprotoConfig {
            app_credentials: self.inner.lock().unwrap().rows.clone(),
        }
    }

    /// Seed a credential, as a sibling device's (or an earlier mint's) write.
    pub fn seed(&self, credential: AtprotoAppCredential) {
        let mut inner = self.inner.lock().unwrap();
        inner
            .rows
            .retain(|c| c.credential_id != credential.credential_id);
        inner.rows.push(credential);
    }

    /// Make every read fail (`true`) or succeed again (`false`).
    pub fn set_load_failure(&self, fail: bool) {
        self.inner.lock().unwrap().fail_load = fail;
    }

    /// Make every write fail from now on.
    pub fn arm_save_failure(&self) {
        self.inner.lock().unwrap().fail_save = true;
    }
}

#[cfg(any(test, feature = "test-helpers"))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl AtprotoCredentialStore for FakeCredentialStore {
    async fn atproto(&self) -> Result<AtprotoConfig, StoreError> {
        if self.inner.lock().unwrap().fail_load {
            return Err(StoreError::Load("fake: load failure".into()));
        }
        Ok(self.current())
    }

    async fn put_app_credential(
        &self,
        credential: AtprotoAppCredential,
    ) -> Result<bool, StoreError> {
        let mut inner = self.inner.lock().unwrap();
        if inner.fail_save {
            return Err(StoreError::Save("fake: save failure".into()));
        }
        if inner.rows.contains(&credential) {
            return Ok(false);
        }
        inner
            .rows
            .retain(|c| c.credential_id != credential.credential_id);
        inner.rows.push(credential);
        Ok(true)
    }

    async fn revoke_app_credential(&self, credential_id: String) -> Result<bool, StoreError> {
        let mut inner = self.inner.lock().unwrap();
        if inner.fail_save {
            return Err(StoreError::Save("fake: save failure".into()));
        }
        let before = inner.rows.len();
        inner.rows.retain(|c| c.credential_id != credential_id);
        Ok(inner.rows.len() != before)
    }
}
