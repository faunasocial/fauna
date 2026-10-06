//! Shared orchestration for the flat **`admin-calendar`** page — the admin-tier
//! deployment-wide **CalDAV-enable** toggle plus the admin-set **CalDAV port**
//! (`admin.md` § 8 Calendar).
//!
//! Authority for the enablement model: [`caldav-server.md`](../../../docs/goal/behavior/caldav-server.md)
//! § Independent enablement (email + calendar are two independently enableable
//! features of one MDA bridge). Authority for UX/IDs: `docs/goal/behavior/admin.md`
//! § 8 Calendar + `tests/e2e-unified/ui.yaml` `admin-calendar`.
//!
//! This is the **Admin-class** calendar twin of [`super::admin_policy`]'s
//! mail-enable slice — it wraps `MailAdminClient` (`libs/fauna-client-bridges`),
//! mirroring `local_domains.rs` / `forwarders.rs`. The UI renders
//! [`CaldavPolicySnapshot`] and dispatches [`CaldavPolicyAction`]; the per-app
//! glue implements one WS-RPC seam ([`CaldavPolicyNest`]). It shares its
//! one-toggle skeleton with [`super::carddav_policy`] / [`super::webdav_policy`]
//! (generated from [`crate::bool_toggle_policy::bool_toggle_policy_machine`]),
//! but keeps its own hand-written form here for the extra port knob below —
//! round 68 of the shared-Rust lift sweep judged forking the macro for one
//! extra field not worth the added arm complexity.
//!
//! **Read + write are both live.** The toggle *hydrates* `caldav_enabled` from the
//! admin read twin `fauna.bridges.get_mail_config` (`FetchConfigReply.caldav_enabled`,
//! which the nest reports as `mail_enabled` while the CalDAV toggle is unset — the
//! legacy "enabling email also enables CalDAV" fallback; `Refresh`), and *saves*
//! via the Admin-class write kind `fauna.bridges.set_caldav_enabled`. The MDA bridge
//! runs iff `mail_enabled || caldav_enabled` (`bins/fauna-nest/src/mail_enable.rs`),
//! so flipping this toggle (or the `admin-mail` mail-enable toggle) is what brings
//! the one MDA bridge up/down — "enabled if either".
//!
//! **The CalDAV port** (`caldav-server.md` § Network exposure — *the admin-settable
//! CalDAV port*) is the second knob: a port a human *picks* is client UI per the
//! iron-clad config-surface invariant, so the page exposes it. It hydrates from the
//! same `get_mail_config` read (`FetchConfigReply.caldav_port`, serde default 8443)
//! and saves via the Admin-class `fauna.bridges.set_caldav_port`. It governs only
//! the **router-less direct listener** (desktop-native / bare-IP / domainless box);
//! on a domain deployment CalDAV is served at `mail.<domain>:443` via the SNI router
//! and the operator-hatch IPC wins, so the admin port is inert there.
//!
//! A focused machine (not folded into `MailPolicyMachine`) so the `admin-calendar`
//! page stays a dumb renderer of a small snapshot, mirroring the other focused
//! machines (`ForwarderMachine` / `LocalDomainMachine`). The other 5 clients lift it
//! over the UniFFI/wasm `build_caldav_policy_machine` export.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fauna_protocol::MaybeSendSync;
use fauna_protocol::bridge_routing::FetchConfigReply;
use serde::{Deserialize, Serialize};

use crate::error::{DispatchError, NestError};

/// Coarse machine status for spinner / disabled-control rendering. Mirrors
/// [`super::admin_policy::MailPolicyStatus`] / `ForwarderStatus`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum CaldavPolicyStatus {
    Idle,
    Loading,
    Working,
}

/// Read-only snapshot the per-app `admin-calendar` UI renders. Projected from
/// the admin read twin `get_mail_config` (its `caldav_enabled` field).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct CaldavPolicySnapshot {
    /// Deployment-wide CalDAV-enable toggle (`caldav.enabled` / `set_caldav_enabled`).
    /// Hydrated from `FetchConfigReply.caldav_enabled` — which the nest reports as
    /// `mail_enabled` until the toggle is explicitly set (legacy unified fallback).
    pub caldav_enabled: bool,
    /// Admin-set CalDAV listener port (`caldav-server.md` § Network exposure), the
    /// `admin-calendar-caldav-port-input` field. Hydrated from
    /// `FetchConfigReply.caldav_port` (serde default 8443); written via
    /// `set_caldav_port`. Governs the router-less direct listener only.
    pub caldav_port: u16,
    pub status: CaldavPolicyStatus,
    /// Last action's error, surfaced via `admin-calendar`'s `error-message`.
    pub error: Option<String>,
}

impl CaldavPolicySnapshot {
    /// The pre-hydrate placeholder (catalog default: disabled) so the snapshot is
    /// never in an invalid state before the first `get_mail_config`.
    fn defaults() -> Self {
        Self::from_config(&FetchConfigReply::default())
    }

    /// Build the rendered snapshot from the admin read (`get_mail_config`).
    fn from_config(reply: &FetchConfigReply) -> Self {
        Self {
            caldav_enabled: reply.caldav_enabled,
            caldav_port: reply.caldav_port,
            status: CaldavPolicyStatus::Idle,
            error: None,
        }
    }
}

/// Actions the per-app `admin-calendar` UI dispatches.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum CaldavPolicyAction {
    /// Re-read the effective config (page load / after a save).
    Refresh,
    /// Flip the deployment-wide CalDAV-enable toggle.
    SetCaldavEnabled { enabled: bool },
    /// Set the admin-chosen CalDAV listener port (`admin-calendar-caldav-port-input`
    /// and its save button). Re-reads after the write so the field reflects persisted
    /// state, exactly like the enable toggle.
    SetCaldavPort { port: u16 },
}

/// WS-RPC seam to nest. Per-app glue implements this over `MailAdminClient`
/// (`libs/fauna-client-bridges`) — each method a 1:1 forward. Dual `async_trait`
/// arm + `MaybeSendSync` supertrait so one seam serves native + wasm.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait CaldavPolicyNest: MaybeSendSync {
    /// `fauna.bridges.get_mail_config` — the overlaid effective config; this
    /// machine reads only its `caldav_enabled` field (the admin read twin shared
    /// with `admin-mail`).
    async fn get_mail_config(&self) -> Result<FetchConfigReply, NestError>;
    /// `fauna.bridges.set_caldav_enabled`.
    async fn set_caldav_enabled(&self, enabled: bool) -> Result<(), NestError>;
    /// `fauna.bridges.set_caldav_port`.
    async fn set_caldav_port(&self, port: u16) -> Result<(), NestError>;
}

/// One instance per admin client. Holds the rendered snapshot; drives the seam.
/// Mirrors `MailPolicyMachine` / `ForwarderMachine`.
#[cfg_attr(feature = "uniffi", derive(uniffi::Object))]
pub struct CaldavPolicyMachine {
    nest: Arc<dyn CaldavPolicyNest>,
    inner: Mutex<CaldavPolicySnapshot>,
}

impl CaldavPolicyMachine {
    pub fn new(nest: Arc<dyn CaldavPolicyNest>) -> Self {
        Self {
            nest,
            inner: Mutex::new(CaldavPolicySnapshot::defaults()),
        }
    }

    fn set_status(&self, status: CaldavPolicyStatus) {
        self.inner.lock().expect("snapshot mutex").status = status;
    }

    async fn refresh(&self) -> Result<(), DispatchError> {
        self.set_status(CaldavPolicyStatus::Loading);
        let reply = self.nest.get_mail_config().await?;
        let mut snap = self.inner.lock().expect("snapshot mutex");
        *snap = CaldavPolicySnapshot::from_config(&reply);
        snap.status = CaldavPolicyStatus::Idle;
        Ok(())
    }

    async fn set_caldav_enabled(&self, enabled: bool) -> Result<(), DispatchError> {
        self.set_status(CaldavPolicyStatus::Working);
        self.nest.set_caldav_enabled(enabled).await?;
        // Re-read so the toggle reflects persisted state.
        self.refresh().await
    }

    async fn set_caldav_port(&self, port: u16) -> Result<(), DispatchError> {
        self.set_status(CaldavPolicyStatus::Working);
        self.nest.set_caldav_port(port).await?;
        // Re-read so the field reflects persisted state.
        self.refresh().await
    }
}

#[cfg_attr(feature = "uniffi", uniffi::export)]
impl CaldavPolicyMachine {
    pub fn snapshot(&self) -> CaldavPolicySnapshot {
        fauna_core::clone_locked(&self.inner, |s| s)
    }
}

#[cfg_attr(feature = "uniffi", fauna_uniffi_async::export)]
impl CaldavPolicyMachine {
    /// Initial page load — hydrate from `get_mail_config`.
    pub async fn hydrate(&self) -> Result<(), DispatchError> {
        self.refresh().await
    }

    pub async fn dispatch(&self, action: CaldavPolicyAction) -> Result<(), DispatchError> {
        // Clear any prior error before the new action runs.
        self.inner.lock().expect("snapshot mutex").error = None;
        crate::dispatch_capturing_error!(
            self,
            CaldavPolicyStatus,
            match action {
                CaldavPolicyAction::Refresh => self.refresh().await,
                CaldavPolicyAction::SetCaldavEnabled { enabled } => {
                    self.set_caldav_enabled(enabled).await
                }
                CaldavPolicyAction::SetCaldavPort { port } => self.set_caldav_port(port).await,
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    /// In-memory nest modelling the `caldav_enabled` / `mail_enabled` toggles
    /// overlaid on `get_mail_config`. `set_caldav_enabled` flips the persisted
    /// flag; a config holds the `FetchConfigReply` the admin read returns.
    struct FakeNest {
        cfg: StdMutex<FetchConfigReply>,
        /// If set, `set_caldav_enabled` returns this error instead of recording —
        /// for the error-surfacing test.
        fail_set: StdMutex<Option<NestError>>,
    }

    impl FakeNest {
        fn new() -> Self {
            // The wire `FetchConfigReply::default()` reports `caldav_enabled: true`
            // (catalog default), but for a clean test baseline we start disabled and
            // let each test flip it explicitly.
            let cfg = FetchConfigReply {
                caldav_enabled: false,
                ..Default::default()
            };
            Self {
                cfg: StdMutex::new(cfg),
                fail_set: StdMutex::new(None),
            }
        }
    }

    #[async_trait]
    impl CaldavPolicyNest for FakeNest {
        async fn get_mail_config(&self) -> Result<FetchConfigReply, NestError> {
            Ok(self.cfg.lock().unwrap().clone())
        }

        async fn set_caldav_enabled(&self, enabled: bool) -> Result<(), NestError> {
            if let Some(err) = self.fail_set.lock().unwrap().clone() {
                return Err(err);
            }
            self.cfg.lock().unwrap().caldav_enabled = enabled;
            Ok(())
        }

        async fn set_caldav_port(&self, port: u16) -> Result<(), NestError> {
            if let Some(err) = self.fail_set.lock().unwrap().clone() {
                return Err(err);
            }
            self.cfg.lock().unwrap().caldav_port = port;
            Ok(())
        }
    }

    #[tokio::test]
    async fn hydrate_reads_caldav_enabled() {
        let nest = Arc::new(FakeNest::new());
        nest.cfg.lock().unwrap().caldav_enabled = true;
        let m = CaldavPolicyMachine::new(nest);
        m.hydrate().await.unwrap();
        assert!(m.snapshot().caldav_enabled);
        assert_eq!(m.snapshot().status, CaldavPolicyStatus::Idle);
    }

    #[tokio::test]
    async fn hydrate_reads_caldav_port() {
        let nest = Arc::new(FakeNest::new());
        // FetchConfigReply::default() seeds the catalog default (8443).
        assert_eq!(nest.cfg.lock().unwrap().caldav_port, 8443);
        nest.cfg.lock().unwrap().caldav_port = 9443;
        let m = CaldavPolicyMachine::new(nest);
        m.hydrate().await.unwrap();
        assert_eq!(m.snapshot().caldav_port, 9443);
        assert_eq!(m.snapshot().status, CaldavPolicyStatus::Idle);
    }

    #[tokio::test]
    async fn set_caldav_port_persists_and_rereads() {
        let nest = Arc::new(FakeNest::new());
        let m = CaldavPolicyMachine::new(nest);
        m.hydrate().await.unwrap();
        assert_eq!(m.snapshot().caldav_port, 8443);

        m.dispatch(CaldavPolicyAction::SetCaldavPort { port: 8444 })
            .await
            .unwrap();
        // Reflects the persisted post-write state (re-read), not the local set.
        assert_eq!(m.snapshot().caldav_port, 8444);
        assert!(m.snapshot().error.is_none());
    }

    #[tokio::test]
    async fn set_port_failure_surfaces_error() {
        let nest = Arc::new(FakeNest::new());
        let m = CaldavPolicyMachine::new(nest.clone());
        m.hydrate().await.unwrap();
        assert_eq!(m.snapshot().caldav_port, 8443);

        *nest.fail_set.lock().unwrap() = Some(NestError::Rejected("nope".into()));
        let result = m
            .dispatch(CaldavPolicyAction::SetCaldavPort { port: 8444 })
            .await;
        assert!(result.is_err());
        let snap = m.snapshot();
        assert!(snap.error.is_some());
        // The write was rejected, so the persisted (and re-read) port is unchanged.
        assert_eq!(snap.caldav_port, 8443);
        assert_eq!(snap.status, CaldavPolicyStatus::Idle);
    }

    #[tokio::test]
    async fn set_caldav_enabled_persists_and_rereads() {
        let nest = Arc::new(FakeNest::new());
        let m = CaldavPolicyMachine::new(nest);
        m.hydrate().await.unwrap();
        assert!(!m.snapshot().caldav_enabled);

        m.dispatch(CaldavPolicyAction::SetCaldavEnabled { enabled: true })
            .await
            .unwrap();
        // Reflects the persisted post-write state (re-read), not the local set.
        assert!(m.snapshot().caldav_enabled);
        assert!(m.snapshot().error.is_none());
    }

    /// A nest rejection surfaces on the snapshot's `error` (never faked green) and
    /// leaves the status back at Idle.
    #[tokio::test]
    async fn set_failure_surfaces_error() {
        let nest = Arc::new(FakeNest::new());
        let m = CaldavPolicyMachine::new(nest.clone());
        // Hydrate to a known baseline (disabled) before the failing write.
        m.hydrate().await.unwrap();
        assert!(!m.snapshot().caldav_enabled);

        *nest.fail_set.lock().unwrap() = Some(NestError::Rejected("nope".into()));
        let result = m
            .dispatch(CaldavPolicyAction::SetCaldavEnabled { enabled: true })
            .await;
        assert!(result.is_err());
        let snap = m.snapshot();
        assert!(snap.error.is_some());
        // The write was rejected, so the persisted (and re-read) state is unchanged.
        assert!(!snap.caldav_enabled);
        assert_eq!(snap.status, CaldavPolicyStatus::Idle);
    }
}
