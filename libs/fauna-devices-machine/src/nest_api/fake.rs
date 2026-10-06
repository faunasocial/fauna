//! In-memory fakes for the page seams, for `DevicesMachine` lifecycle tests.
//!
//! [`FakeDevicesNestApi`] fixtures the list responses + records every write
//! gesture (method + args) so tests can assert the exact wire shape each gesture
//! produces and fixture failures per method. [`FakeWizardFactory`] builds the
//! embedded wizard over the wizard crate's `FakeFolderNestApi` so `open_wizard`
//! works without a transport.

#![cfg(any(test, feature = "test-helpers"))]

use fauna_protocol::sync::SyncDevice;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fauna_folders_machine::nest_api::FakeFolderNestApi;
use fauna_folders_machine::{DeviceOption, FolderWizardMachine, FolderWizardObserver};

use fauna_protocol::folders::SyncConflict;

use super::{CandidateVerdict, ChosenWinner, DevicesApiError, DevicesNestApi, WizardFactory};
use fauna_protocol::folders::FolderSummary as WireFolderSummary;

/// One recorded write gesture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FakeCall {
    RemoveDevice {
        device_id: String,
    },
    /// The owner arm of `fauna.sync.devices.p2p_participation.set` — the
    /// only value it can carry is off, so the call records the row alone.
    RequestP2pOff {
        device_id: String,
    },
    DeleteFolder {
        name: String,
    },
    ResolveConflict {
        id: i64,
        winning_manifest_hash: Option<String>,
    },
    SetFolderPaths {
        name: String,
        include_paths: Option<Vec<String>>,
        exclude_paths: Option<Vec<String>>,
        /// The owner-sealed lists the machine minted for this save, if any
        /// (path-sealing S6-c). Recorded so a test can assert the seal was
        /// actually produced — a writer nothing observes is how a sealed column
        /// silently stops being written.
        include_sealed: Option<Vec<u8>>,
        exclude_sealed: Option<Vec<u8>>,
    },
    SetFolderConflictPolicy {
        name: String,
        conflict_policy: String,
    },
    SetFolderAudience {
        name: String,
        audience: String,
        /// Whether the machine handed the seam its wired identity key — the
        /// difference between an attested `→public` flip and one no verifying
        /// seat unseals.
        attested: bool,
    },
    SetFolderWebsiteEnabled {
        name: String,
        enabled: bool,
    },
    SetFolderResidency {
        name: String,
        residency: String,
    },
    /// The whole flag triple, because the point applies whole — a test asserts
    /// all three, including the two the user did not touch.
    SetFolderPlace {
        name: String,
        device_id: String,
        originates: bool,
        accepts: bool,
        applies_deletes: bool,
    },
    SetFolderNestPlace {
        name: String,
        snapshots: Option<bool>,
        quiet_secs: Option<i64>,
        retention: Option<String>,
        version_retention: Option<fauna_folders_machine::VersionRetentionWrite>,
    },
    RestoreFileVersion {
        folder: String,
        device_id: String,
        path: String,
        manifest_hash: String,
        size_bytes: i64,
        content_key_version: Option<u64>,
        path_sealed: Option<Vec<u8>>,
    },
}

/// `(set name, path hash, manifest)` — what a judged version is looked up by.
type JudgedKey = (String, [u8; 32], String);

#[derive(Debug, Default)]
pub struct FakeDevicesNestApi {
    /// Wire rows, matching the seam (same reason as `conflicts` below): the
    /// machine renders the sealed label before transcribing, so a test can drive
    /// a real device-label seal through it.
    devices: Mutex<Vec<SyncDevice>>,
    folders: Mutex<Vec<WireFolderSummary>>,
    /// The names `webdav_served` answers served (by rendered name) — the
    /// stand-in for the owner's custody's serve window.
    custody_served: Mutex<Vec<String>>,
    /// Wire rows, matching the seam: the machine renders their sealed paths
    /// before transcribing, so a test can drive a real seal through it.
    conflicts: Mutex<Vec<SyncConflict>>,
    /// `Some` overrides the next write of the matching kind with an error.
    remove_device_response: Mutex<Option<Result<(), DevicesApiError>>>,
    request_p2p_off_response: Mutex<Option<Result<(), DevicesApiError>>>,
    delete_folder_response: Mutex<Option<Result<(), DevicesApiError>>>,
    resolve_conflict_response: Mutex<Option<Result<(), DevicesApiError>>>,
    set_paths_response: Mutex<Option<Result<(), DevicesApiError>>>,
    set_conflict_policy_response: Mutex<Option<Result<(), DevicesApiError>>>,
    set_audience_response: Mutex<Option<Result<(), DevicesApiError>>>,
    set_website_response: Mutex<Option<Result<(), DevicesApiError>>>,
    set_residency_response: Mutex<Option<Result<(), DevicesApiError>>>,
    set_place_response: Mutex<Option<Result<(), DevicesApiError>>>,
    set_nest_place_response: Mutex<Option<Result<(), DevicesApiError>>>,
    restore_version_response: Mutex<Option<Result<(), DevicesApiError>>>,
    /// The judged version history, as `judge_candidate` answers it: keyed by
    /// `(folder, path_hash(path), manifest)` — by HASH, as the real seam looks
    /// a version up — so a test can pin that the lookup follows the rendered
    /// path. A candidate no test seeded is [`CandidateVerdict::NotAVersion`].
    judged_versions: Mutex<std::collections::HashMap<JudgedKey, CandidateVerdict>>,
    /// `Some` makes `judge_candidate` fail (a seam that cannot judge).
    judge_response: Mutex<Option<DevicesApiError>>,
    /// `Some` makes the *reads* (`list_*`) fail — for refresh-error tests.
    list_response: Mutex<Option<DevicesApiError>>,
    calls: Mutex<Vec<FakeCall>>,
}

impl FakeDevicesNestApi {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_devices(&self, devices: Vec<SyncDevice>) {
        *self.devices.lock().unwrap() = devices;
    }
    pub fn set_folders(&self, folders: Vec<WireFolderSummary>) {
        *self.folders.lock().unwrap() = folders;
    }
    /// The set names the fake's "custody" calls served — what
    /// `webdav_served` answers, whatever the rows' nest flag says.
    pub fn set_custody_served(&self, names: &[&str]) {
        *self.custody_served.lock().unwrap() = names.iter().map(|n| n.to_string()).collect();
    }
    pub fn set_conflicts(&self, conflicts: Vec<SyncConflict>) {
        *self.conflicts.lock().unwrap() = conflicts;
    }

    /// Make every `list_*` read fail with `err` (refresh-error path).
    pub fn fail_lists(&self, err: DevicesApiError) {
        *self.list_response.lock().unwrap() = Some(err);
    }
    pub fn set_request_p2p_off_response(&self, r: Result<(), DevicesApiError>) {
        *self.request_p2p_off_response.lock().unwrap() = Some(r);
    }
    pub fn set_remove_device_response(&self, r: Result<(), DevicesApiError>) {
        *self.remove_device_response.lock().unwrap() = Some(r);
    }
    pub fn set_delete_folder_response(&self, r: Result<(), DevicesApiError>) {
        *self.delete_folder_response.lock().unwrap() = Some(r);
    }
    /// Seed the judged history: the version `manifest_hash` names at `path`
    /// in `folder` reads as `verdict`.
    pub fn set_judged_version(
        &self,
        folder: &str,
        path: &str,
        manifest_hash: &str,
        verdict: CandidateVerdict,
    ) {
        self.judged_versions.lock().unwrap().insert(
            (
                folder.to_string(),
                fauna_core::sync::path_hash(path),
                manifest_hash.to_string(),
            ),
            verdict,
        );
    }
    /// Make `judge_candidate` fail with `err`.
    pub fn fail_judge(&self, err: DevicesApiError) {
        *self.judge_response.lock().unwrap() = Some(err);
    }
    pub fn set_resolve_conflict_response(&self, r: Result<(), DevicesApiError>) {
        *self.resolve_conflict_response.lock().unwrap() = Some(r);
    }
    pub fn set_paths_response(&self, r: Result<(), DevicesApiError>) {
        *self.set_paths_response.lock().unwrap() = Some(r);
    }
    pub fn set_conflict_policy_response(&self, r: Result<(), DevicesApiError>) {
        *self.set_conflict_policy_response.lock().unwrap() = Some(r);
    }
    pub fn set_audience_response(&self, r: Result<(), DevicesApiError>) {
        *self.set_audience_response.lock().unwrap() = Some(r);
    }
    pub fn set_website_response(&self, r: Result<(), DevicesApiError>) {
        *self.set_website_response.lock().unwrap() = Some(r);
    }
    pub fn set_residency_response(&self, r: Result<(), DevicesApiError>) {
        *self.set_residency_response.lock().unwrap() = Some(r);
    }
    pub fn set_place_response(&self, r: Result<(), DevicesApiError>) {
        *self.set_place_response.lock().unwrap() = Some(r);
    }
    pub fn set_nest_place_response(&self, r: Result<(), DevicesApiError>) {
        *self.set_nest_place_response.lock().unwrap() = Some(r);
    }
    pub fn set_restore_version_response(&self, r: Result<(), DevicesApiError>) {
        *self.restore_version_response.lock().unwrap() = Some(r);
    }

    /// All recorded write gestures, in order.
    pub fn calls(&self) -> Vec<FakeCall> {
        self.calls.lock().unwrap().clone()
    }

    fn list_err(&self) -> Option<DevicesApiError> {
        self.list_response.lock().unwrap().clone()
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl DevicesNestApi for FakeDevicesNestApi {
    async fn list_devices(&self) -> Result<Vec<SyncDevice>, DevicesApiError> {
        match self.list_err() {
            Some(e) => Err(e),
            None => Ok(self.devices.lock().unwrap().clone()),
        }
    }

    async fn list_folders(&self) -> Result<Vec<WireFolderSummary>, DevicesApiError> {
        match self.list_err() {
            Some(e) => Err(e),
            None => Ok(self.folders.lock().unwrap().clone()),
        }
    }

    async fn webdav_served(&self, rows: &[WireFolderSummary]) -> Vec<bool> {
        let served = self.custody_served.lock().unwrap();
        rows.iter().map(|r| served.contains(&r.name)).collect()
    }

    async fn list_conflicts(&self) -> Result<Vec<SyncConflict>, DevicesApiError> {
        match self.list_err() {
            Some(e) => Err(e),
            None => Ok(self.conflicts.lock().unwrap().clone()),
        }
    }

    async fn request_p2p_off(&self, device_id: &str) -> Result<(), DevicesApiError> {
        self.calls.lock().unwrap().push(FakeCall::RequestP2pOff {
            device_id: device_id.to_string(),
        });
        self.request_p2p_off_response
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(Ok(()))
    }

    async fn remove_device(&self, device_id: &str) -> Result<(), DevicesApiError> {
        self.calls.lock().unwrap().push(FakeCall::RemoveDevice {
            device_id: device_id.to_string(),
        });
        self.remove_device_response
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(Ok(()))
    }

    async fn delete_folder(&self, name: &str) -> Result<(), DevicesApiError> {
        self.calls.lock().unwrap().push(FakeCall::DeleteFolder {
            name: name.to_string(),
        });
        self.delete_folder_response
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(Ok(()))
    }

    async fn resolve_conflict(
        &self,
        id: i64,
        winner: Option<ChosenWinner>,
    ) -> Result<(), DevicesApiError> {
        self.calls.lock().unwrap().push(FakeCall::ResolveConflict {
            id,
            winning_manifest_hash: winner.map(|w| w.manifest_hash),
        });
        self.resolve_conflict_response
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(Ok(()))
    }

    async fn judge_candidate(
        &self,
        folder: &str,
        path: &str,
        manifest_hash: &str,
    ) -> Result<CandidateVerdict, DevicesApiError> {
        if let Some(err) = self.judge_response.lock().unwrap().clone() {
            return Err(err);
        }
        Ok(self
            .judged_versions
            .lock()
            .unwrap()
            .get(&(
                folder.to_string(),
                fauna_core::sync::path_hash(path),
                manifest_hash.to_string(),
            ))
            .cloned()
            .unwrap_or(CandidateVerdict::NotAVersion))
    }

    async fn set_folder_paths(
        &self,
        name: &str,
        include_paths: Option<Vec<String>>,
        exclude_paths: Option<Vec<String>>,
        include_sealed: Option<Vec<u8>>,
        exclude_sealed: Option<Vec<u8>>,
    ) -> Result<(), DevicesApiError> {
        self.calls.lock().unwrap().push(FakeCall::SetFolderPaths {
            name: name.to_string(),
            include_paths,
            exclude_paths,
            include_sealed,
            exclude_sealed,
        });
        self.set_paths_response
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(Ok(()))
    }

    async fn set_folder_conflict_policy(
        &self,
        name: &str,
        conflict_policy: &str,
    ) -> Result<(), DevicesApiError> {
        self.calls
            .lock()
            .unwrap()
            .push(FakeCall::SetFolderConflictPolicy {
                name: name.to_string(),
                conflict_policy: conflict_policy.to_string(),
            });
        self.set_conflict_policy_response
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(Ok(()))
    }

    async fn set_folder_audience(
        &self,
        name: &str,
        audience: &str,
        attestor: Option<Arc<fauna_core::identity::ActorKeypair>>,
    ) -> Result<(), DevicesApiError> {
        self.calls
            .lock()
            .unwrap()
            .push(FakeCall::SetFolderAudience {
                name: name.to_string(),
                audience: audience.to_string(),
                attested: attestor.is_some(),
            });
        self.set_audience_response
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(Ok(()))
    }

    async fn set_folder_website_enabled(
        &self,
        name: &str,
        enabled: bool,
    ) -> Result<(), DevicesApiError> {
        self.calls
            .lock()
            .unwrap()
            .push(FakeCall::SetFolderWebsiteEnabled {
                name: name.to_string(),
                enabled,
            });
        self.set_website_response
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(Ok(()))
    }

    async fn set_folder_residency(
        &self,
        name: &str,
        residency: &str,
    ) -> Result<(), DevicesApiError> {
        self.calls
            .lock()
            .unwrap()
            .push(FakeCall::SetFolderResidency {
                name: name.to_string(),
                residency: residency.to_string(),
            });
        self.set_residency_response
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(Ok(()))
    }

    async fn set_folder_place(
        &self,
        name: &str,
        device_id: &str,
        originates: bool,
        accepts: bool,
        applies_deletes: bool,
    ) -> Result<(), DevicesApiError> {
        self.calls.lock().unwrap().push(FakeCall::SetFolderPlace {
            name: name.to_string(),
            device_id: device_id.to_string(),
            originates,
            accepts,
            applies_deletes,
        });
        self.set_place_response
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(Ok(()))
    }

    async fn set_folder_nest_place(
        &self,
        name: &str,
        snapshots: Option<bool>,
        quiet_secs: Option<i64>,
        retention: Option<String>,
        version_retention: Option<fauna_folders_machine::VersionRetentionWrite>,
    ) -> Result<(), DevicesApiError> {
        self.calls
            .lock()
            .unwrap()
            .push(FakeCall::SetFolderNestPlace {
                name: name.to_string(),
                snapshots,
                quiet_secs,
                retention,
                version_retention,
            });
        self.set_nest_place_response
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(Ok(()))
    }

    async fn restore_file_version(
        &self,
        folder: &str,
        device_id: &str,
        path: &str,
        manifest_hash: String,
        size_bytes: i64,
        content_key_version: Option<u64>,
        path_sealed: Option<Vec<u8>>,
    ) -> Result<(), DevicesApiError> {
        self.calls
            .lock()
            .unwrap()
            .push(FakeCall::RestoreFileVersion {
                folder: folder.to_string(),
                device_id: device_id.to_string(),
                path: path.to_string(),
                manifest_hash,
                size_bytes,
                content_key_version,
                path_sealed,
            });
        self.restore_version_response
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(Ok(()))
    }
}

/// Builds the embedded wizard over a `FakeFolderNestApi` so `open_wizard` works
/// without a transport. Exposes the underlying fake (`wizard_fake`) so tests can
/// assert the wizard's `submit()` wire shape.
#[derive(Debug, Default)]
pub struct FakeWizardFactory {
    pub wizard_fake: Arc<FakeFolderNestApi>,
}

impl FakeWizardFactory {
    pub fn new() -> Self {
        Self {
            wizard_fake: Arc::new(FakeFolderNestApi::new()),
        }
    }
}

impl WizardFactory for FakeWizardFactory {
    fn build_wizard(
        &self,
        observer: Arc<dyn FolderWizardObserver>,
        available_devices: Vec<DeviceOption>,
    ) -> Arc<FolderWizardMachine> {
        FolderWizardMachine::new(
            observer,
            available_devices,
            Arc::clone(&self.wizard_fake) as _,
        )
    }
}
