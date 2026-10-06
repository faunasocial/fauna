//! The DNS management record's persistence seam — the account's
//! `fauna.state.dns` row (`docs/goal/architecture/config-dissolution.md`
//! kinds table; the door is `fauna_account_plane::dns_rows`, reached through
//! [`AccountStoreHandle::{dns, write_dns}`](AccountStoreHandle::dns)).
//!
//! The record is plane-only. It is one whole-record latest-wins row, so a
//! write REPLACES what is stored. That is why every writer goes through [`update_dns`], which re-reads the
//! record immediately before it writes: a record read before a provider
//! round-trip (seconds) or an ACME order (up to 45 minutes) is a stale copy,
//! and writing it back would silently undo another device's change made in
//! between.

#[cfg(any(test, feature = "test-helpers"))]
use std::sync::Arc;

use async_trait::async_trait;
use fauna_account_plane::account_driver::{AccountStoreHandle, wait_for_account_handle};
use fauna_core::data::DnsConfig;
use fauna_protocol::MaybeSendSync;

use crate::{DnsDispatchError, StoreError};

/// Load/save the account's DNS management record. The machine's only
/// persistence dependency; the production impl is [`AccountDnsStore`].
///
/// Native boxes `Send` futures; the wasm arm is `?Send` (the browser handle is
/// single-threaded) — the split every seam in this crate uses.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait DnsStore: MaybeSendSync {
    /// The stored record — the default (no credentials, nothing managed) one
    /// when none is stored.
    async fn load(&self) -> Result<DnsConfig, StoreError>;
    /// Replace the stored record with `next`.
    async fn save(&self, next: DnsConfig) -> Result<(), StoreError>;
}

/// Read the record, apply `edit`, write it back, and return what was written
/// together with `edit`'s own result. The one write path every
/// [`DnsManagementMachine`](crate::DnsManagementMachine) mutation takes, so
/// the read the edit applies to is always the fresh one (the module docs'
/// stale-copy rule). An `edit` that fails writes nothing.
pub(crate) async fn update_dns<T>(
    store: &dyn DnsStore,
    edit: impl FnOnce(&mut DnsConfig) -> Result<T, DnsDispatchError>,
) -> Result<(DnsConfig, T), DnsDispatchError> {
    let mut dns = store.load().await?;
    let out = edit(&mut dns)?;
    store.save(dns.clone()).await?;
    Ok((dns, out))
}

pub use fauna_account_plane::account_driver::AccountHandleSource;

/// [`DnsStore`] over the seat's account runtime — the production impl.
pub struct AccountDnsStore {
    source: AccountHandleSource,
}

impl AccountDnsStore {
    pub fn new(source: AccountHandleSource) -> Self {
        Self { source }
    }

    async fn handle(&self) -> Option<AccountStoreHandle> {
        wait_for_account_handle(&self.source, "the DNS record's store").await
    }
}

/// What a store with no account runtime answers.
const RUNTIME_ABSENT: &str = "the account runtime is not running";

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl DnsStore for AccountDnsStore {
    async fn load(&self) -> Result<DnsConfig, StoreError> {
        let handle = self
            .handle()
            .await
            .ok_or_else(|| StoreError::Load(RUNTIME_ABSENT.into()))?;
        handle
            .dns()
            .await
            .map_err(|e| StoreError::Load(format!("{e:#}")))
    }

    async fn save(&self, next: DnsConfig) -> Result<(), StoreError> {
        let handle = self
            .handle()
            .await
            .ok_or_else(|| StoreError::Save(RUNTIME_ABSENT.into()))?;
        handle
            .write_dns(next)
            .await
            .map(|_| ())
            .map_err(|e| StoreError::Save(format!("{e:#}")))
    }
}

/// An in-memory [`DnsStore`] — the test double. `Clone` shares the record, so
/// two machines built over clones of one store behave like two devices of one
/// account (or one device across a page navigation).
#[cfg(any(test, feature = "test-helpers"))]
#[derive(Clone, Default)]
pub struct FakeDnsStore {
    record: Arc<std::sync::Mutex<DnsConfig>>,
    /// A write another device lands right after the next load returns — the
    /// load→save window [`update_dns`]'s re-read exists for.
    after_next_load: Arc<std::sync::Mutex<Option<InjectedWrite>>>,
}

#[cfg(any(test, feature = "test-helpers"))]
type InjectedWrite = Box<dyn FnOnce(&mut DnsConfig) + Send>;

#[cfg(any(test, feature = "test-helpers"))]
impl FakeDnsStore {
    pub fn with(dns: DnsConfig) -> Self {
        Self {
            record: Arc::new(std::sync::Mutex::new(dns)),
            ..Self::default()
        }
    }

    /// The stored record.
    pub fn current(&self) -> DnsConfig {
        self.record.lock().expect("fake dns store").clone()
    }

    /// Change the stored record in place — another device's write.
    pub fn mutate(&self, f: impl FnOnce(&mut DnsConfig)) {
        f(&mut self.record.lock().expect("fake dns store"));
    }

    /// Land `f` on the stored record right after the next load returns, as if
    /// another device wrote while this one was working from what it loaded.
    pub fn inject_after_next_load(&self, f: impl FnOnce(&mut DnsConfig) + Send + 'static) {
        *self.after_next_load.lock().expect("fake dns store") = Some(Box::new(f));
    }
}

#[cfg(any(test, feature = "test-helpers"))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl DnsStore for FakeDnsStore {
    async fn load(&self) -> Result<DnsConfig, StoreError> {
        let loaded = self.current();
        if let Some(f) = self.after_next_load.lock().expect("fake dns store").take() {
            self.mutate(f);
        }
        Ok(loaded)
    }

    async fn save(&self, next: DnsConfig) -> Result<(), StoreError> {
        *self.record.lock().expect("fake dns store") = next;
        Ok(())
    }
}
