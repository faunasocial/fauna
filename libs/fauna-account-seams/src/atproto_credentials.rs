//! The ATProto settings page's credential seam over a seat's account runtime
//! ([`fauna_atproto_settings_machine::AtprotoCredentialStore`];
//! `atproto-pds-full.md` § D3 owns the custody rule) — the one implementation
//! every runtime-hosting app wires into its `AtprotoSettingsMachine`, and the
//! one web's core chunk serves the ATProto chunk's port forwarder through
//! (`account-client-lifecycle.md` § The client-side lifecycle → *The account
//! port*, decisions (d) and (h)).
//!
//! The secrets are the account plane's `fauna.state.atproto` kind, one row per
//! credential (`config-dissolution.md` — the kinds table's row;
//! `fauna_account_plane::atproto_rows` owns the door). The kind was cut
//! plane-only: there is no second rail behind this seam, so an absent
//! runtime is a refusal, never a fallback.
//!
//! **An absent runtime** — the blessing door's posture
//! ([`crate::blessed_nests`]). A read answers `Err` at once: the page's
//! refresh must not stall behind it, and a web tab that hosts no runtime (only
//! the MLS-writing tab does) is a normal state — the machine hides every
//! reveal. A write — the mint's persist, which lands after the nest already
//! holds the verifier, or a revoke's local drop — waits for the handle for up
//! to [`WAIT_POLLS`] × [`WAIT_POLL`], the seconds a seat's runtime takes to
//! assemble after sign-in, then refuses; the machine then hands the minted
//! secret back once, flagged as not kept.

use std::time::Duration;

use fauna_account_plane::account_driver::AccountStoreHandle;
use fauna_atproto_settings_machine::AtprotoCredentialStore;
use fauna_client_config::StoreError;
use fauna_core::data::{AtprotoAppCredential, AtprotoConfig};

/// What an absent runtime answers.
const RUNTIME_ABSENT: &str = "the account runtime is not running";

/// How often a write re-reads the seat's handle while the runtime assembles.
pub const WAIT_POLL: Duration = Duration::from_millis(250);

/// How many polls a write waits for the runtime before it refuses (~30 s, the
/// blessing door's bound — `blessed_nests::WRITE_WAIT_POLLS`).
pub const WAIT_POLLS: u32 = 120;

/// [`AtprotoCredentialStore`] over a seat's account runtime. `source` is the
/// seat's own fresh read of its live handle — `AccountRuntimeHost::handle` on
/// linux and the `fauna-ffi` seat, the `App`-owned slot on tui, and on web the
/// core chunk's `account_runtime::handle_for` the port's account (so a machine
/// that outlived an account switch is refused, decision (e)).
pub struct RuntimeAtprotoCredentials<S> {
    source: S,
    polls: u32,
}

impl<S> RuntimeAtprotoCredentials<S>
where
    S: Fn() -> Option<AccountStoreHandle> + Send + Sync,
{
    pub fn new(source: S) -> Self {
        Self {
            source,
            polls: WAIT_POLLS,
        }
    }

    /// The handle, waiting out an assembly still in flight (writes only).
    async fn handle_for_write(&self) -> Option<AccountStoreHandle> {
        for _ in 0..self.polls {
            if let Some(handle) = (self.source)() {
                return Some(handle);
            }
            fauna_sleep::sleep(WAIT_POLL).await;
        }
        (self.source)()
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl<S> AtprotoCredentialStore for RuntimeAtprotoCredentials<S>
where
    S: Fn() -> Option<AccountStoreHandle> + Send + Sync,
{
    async fn atproto(&self) -> Result<AtprotoConfig, StoreError> {
        let handle = (self.source)().ok_or_else(|| StoreError::Load(RUNTIME_ABSENT.to_string()))?;
        handle
            .atproto()
            .await
            .map_err(|e| StoreError::Load(format!("{e:#}")))
    }

    async fn put_app_credential(
        &self,
        credential: AtprotoAppCredential,
    ) -> Result<bool, StoreError> {
        let handle = self
            .handle_for_write()
            .await
            .ok_or_else(|| StoreError::Save(RUNTIME_ABSENT.to_string()))?;
        handle
            .put_app_credential(credential)
            .await
            .map_err(|e| StoreError::Save(format!("{e:#}")))
    }

    async fn revoke_app_credential(&self, credential_id: String) -> Result<bool, StoreError> {
        let handle = self
            .handle_for_write()
            .await
            .ok_or_else(|| StoreError::Save(RUNTIME_ABSENT.to_string()))?;
        handle
            .revoke_app_credential(credential_id)
            .await
            .map_err(|e| StoreError::Save(format!("{e:#}")))
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    /// With no runtime, every call is refused — the read as a load failure,
    /// the writes as save failures — after the wait, never answered as an
    /// empty success (which would read as "no secret held" and let a mint go
    /// live with nowhere to keep it).
    #[tokio::test]
    async fn an_absent_runtime_refuses_every_call() {
        let seam = RuntimeAtprotoCredentials {
            source: || None,
            polls: 1,
        };
        assert!(matches!(seam.atproto().await, Err(StoreError::Load(w)) if w == RUNTIME_ABSENT));
        let credential = AtprotoAppCredential {
            credential_id: "ivory".into(),
            label: "Ivory".into(),
            secret: fauna_core::secret::SecretByteBuf::new(b"s".to_vec()),
            dm_allowed: false,
            created_at: 1,
        };
        assert!(matches!(
            seam.put_app_credential(credential).await,
            Err(StoreError::Save(w)) if w == RUNTIME_ABSENT
        ));
        assert!(matches!(
            seam.revoke_app_credential("ivory".into()).await,
            Err(StoreError::Save(w)) if w == RUNTIME_ABSENT
        ));
    }
}
