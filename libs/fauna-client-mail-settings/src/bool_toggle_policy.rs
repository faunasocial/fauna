//! The shape shared by every "hydrate one boolean from
//! `fauna.bridges.get_mail_config`, save it via one Admin-class write kind,
//! re-read" admin-tier toggle page — [`crate::carddav_policy`] and
//! [`crate::webdav_policy`] are its two instantiations, byte-for-byte
//! identical modulo CardDAV/WebDAV naming before this lift (round 68 of the
//! shared-Rust lift sweep).
//!
//! [`crate::caldav_policy`] shares the same skeleton but adds a second knob
//! (the admin-set port) and is left hand-written rather than forced through
//! this macro's one-field shape — a fork for one extra field would cost more
//! macro-arm complexity than the ~40 lines it would save.

/// Generates the `{Status, Snapshot, Action, Nest, Machine}` quintet for a
/// single boolean admin-tier toggle. Invoke once per protocol; write the
/// module's own `//!` doc above the invocation with the page-specific detail
/// (which siblings it mirrors, the MDA "enabled if any" clause, the UniFFI
/// export name) — this macro only carries the parts that were byte-identical
/// across every instantiation.
macro_rules! bool_toggle_policy_machine {
    (
        protocol: $protocol:literal,
        status: $Status:ident,
        snapshot: $Snapshot:ident,
        action: $Action:ident,
        nest: $Nest:ident,
        machine: $Machine:ident,
        field: $field:ident,
        set_variant: $SetVariant:ident,
        set_method: $set_method:ident,
    ) => {
        use std::sync::{Arc, Mutex};

        use async_trait::async_trait;
        use fauna_protocol::MaybeSendSync;
        use fauna_protocol::bridge_routing::FetchConfigReply;
        use serde::{Deserialize, Serialize};

        use crate::error::{DispatchError, NestError};

        /// Coarse machine status for spinner / disabled-control rendering. See
        /// the module doc for which protocol this instantiation serves.
        #[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
        #[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
        pub enum $Status {
            Idle,
            Loading,
            Working,
        }

        /// Read-only snapshot the per-app UI renders. Projected from the admin
        /// read twin `get_mail_config`. See the module doc for which protocol
        /// this instantiation serves.
        #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
        #[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
        pub struct $Snapshot {
            /// Deployment-wide enable toggle for this protocol.
            pub $field: bool,
            pub status: $Status,
            /// Last action's error, surfaced via the page's `error-message`.
            pub error: Option<String>,
        }

        impl $Snapshot {
            /// The pre-hydrate placeholder so the snapshot is never in an
            /// invalid state before the first `get_mail_config`.
            fn defaults() -> Self {
                Self::from_config(&FetchConfigReply::default())
            }

            /// Build the rendered snapshot from the admin read (`get_mail_config`).
            fn from_config(reply: &FetchConfigReply) -> Self {
                Self {
                    $field: reply.$field,
                    status: $Status::Idle,
                    error: None,
                }
            }
        }

        /// Actions the per-app UI dispatches. See the module doc for which
        /// protocol this instantiation serves.
        #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
        #[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
        pub enum $Action {
            /// Re-read the effective config (page load / after a save).
            Refresh,
            /// Flip the deployment-wide enable toggle.
            $SetVariant { enabled: bool },
        }

        /// WS-RPC seam to nest. Per-app glue implements this over
        /// `MailAdminClient` (`libs/fauna-client-bridges`) — each method a 1:1
        /// forward. Dual `async_trait` arm + `MaybeSendSync` supertrait so one
        /// seam serves native + wasm.
        #[cfg_attr(not(target_arch = "wasm32"), async_trait)]
        #[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
        pub trait $Nest: MaybeSendSync {
            /// `fauna.bridges.get_mail_config` — the overlaid effective config;
            /// this machine reads only its own toggle field.
            async fn get_mail_config(&self) -> Result<FetchConfigReply, NestError>;
            /// The Admin-class write kind for this toggle.
            async fn $set_method(&self, enabled: bool) -> Result<(), NestError>;
        }

        /// One instance per admin client. Holds the rendered snapshot; drives
        /// the seam. See the module doc for which protocol this instantiation
        /// serves.
        #[cfg_attr(feature = "uniffi", derive(uniffi::Object))]
        pub struct $Machine {
            nest: Arc<dyn $Nest>,
            inner: Mutex<$Snapshot>,
        }

        impl $Machine {
            pub fn new(nest: Arc<dyn $Nest>) -> Self {
                Self {
                    nest,
                    inner: Mutex::new($Snapshot::defaults()),
                }
            }

            fn set_status(&self, status: $Status) {
                self.inner.lock().expect("snapshot mutex").status = status;
            }

            async fn refresh(&self) -> Result<(), DispatchError> {
                self.set_status($Status::Loading);
                let reply = self.nest.get_mail_config().await?;
                let mut snap = self.inner.lock().expect("snapshot mutex");
                *snap = $Snapshot::from_config(&reply);
                snap.status = $Status::Idle;
                Ok(())
            }

            async fn $set_method(&self, enabled: bool) -> Result<(), DispatchError> {
                self.set_status($Status::Working);
                self.nest.$set_method(enabled).await?;
                // Re-read so the toggle reflects persisted state.
                self.refresh().await
            }
        }

        #[cfg_attr(feature = "uniffi", uniffi::export)]
        impl $Machine {
            pub fn snapshot(&self) -> $Snapshot {
                fauna_core::clone_locked(&self.inner, |s| s)
            }
        }

        #[cfg_attr(feature = "uniffi", fauna_uniffi_async::export)]
        impl $Machine {
            /// Initial page load — hydrate from `get_mail_config`.
            pub async fn hydrate(&self) -> Result<(), DispatchError> {
                self.refresh().await
            }

            pub async fn dispatch(&self, action: $Action) -> Result<(), DispatchError> {
                // Clear any prior error before the new action runs.
                self.inner.lock().expect("snapshot mutex").error = None;
                crate::dispatch_capturing_error!(
                    self,
                    $Status,
                    match action {
                        $Action::Refresh => self.refresh().await,
                        $Action::$SetVariant { enabled } => self.$set_method(enabled).await,
                    }
                )
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;
            use std::sync::Mutex as StdMutex;

            /// In-memory nest modelling the toggle overlaid on `get_mail_config`.
            /// `$set_method` flips the persisted flag; a config holds the
            /// `FetchConfigReply` the admin read returns.
            struct FakeNest {
                cfg: StdMutex<FetchConfigReply>,
                /// If set, `$set_method` returns this error instead of
                /// recording — for the error-surfacing test.
                fail_set: StdMutex<Option<NestError>>,
            }

            impl FakeNest {
                fn new() -> Self {
                    // The wire `FetchConfigReply::default()` reports the toggle
                    // true (catalog default), but for a clean test baseline we
                    // start disabled and let each test flip it explicitly.
                    let cfg = FetchConfigReply {
                        $field: false,
                        ..Default::default()
                    };
                    Self {
                        cfg: StdMutex::new(cfg),
                        fail_set: StdMutex::new(None),
                    }
                }
            }

            #[async_trait]
            impl $Nest for FakeNest {
                async fn get_mail_config(&self) -> Result<FetchConfigReply, NestError> {
                    Ok(self.cfg.lock().unwrap().clone())
                }

                async fn $set_method(&self, enabled: bool) -> Result<(), NestError> {
                    if let Some(err) = self.fail_set.lock().unwrap().clone() {
                        return Err(err);
                    }
                    self.cfg.lock().unwrap().$field = enabled;
                    Ok(())
                }
            }

            #[tokio::test]
            async fn hydrate_reads_enabled() {
                let nest = Arc::new(FakeNest::new());
                nest.cfg.lock().unwrap().$field = true;
                let m = $Machine::new(nest);
                m.hydrate().await.unwrap();
                assert!(m.snapshot().$field);
                assert_eq!(m.snapshot().status, $Status::Idle);
            }

            #[tokio::test]
            async fn set_enabled_persists_and_rereads() {
                let nest = Arc::new(FakeNest::new());
                let m = $Machine::new(nest);
                m.hydrate().await.unwrap();
                assert!(!m.snapshot().$field);

                m.dispatch($Action::$SetVariant { enabled: true })
                    .await
                    .unwrap();
                // Reflects the persisted post-write state (re-read), not the local set.
                assert!(m.snapshot().$field);
                assert!(m.snapshot().error.is_none());
            }

            /// A nest rejection surfaces on the snapshot's `error` (never faked
            /// green) and leaves the status back at Idle.
            #[tokio::test]
            async fn set_failure_surfaces_error() {
                let nest = Arc::new(FakeNest::new());
                let m = $Machine::new(nest.clone());
                // Hydrate to a known baseline (disabled) before the failing write.
                m.hydrate().await.unwrap();
                assert!(!m.snapshot().$field);

                *nest.fail_set.lock().unwrap() = Some(NestError::Rejected("nope".into()));
                let result = m.dispatch($Action::$SetVariant { enabled: true }).await;
                assert!(result.is_err());
                let snap = m.snapshot();
                assert!(snap.error.is_some());
                // The write was rejected, so the persisted (and re-read) state is unchanged.
                assert!(!snap.$field);
                assert_eq!(snap.status, $Status::Idle);
            }
        }
    };
}

pub(crate) use bool_toggle_policy_machine;
