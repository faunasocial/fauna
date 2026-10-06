//! In-memory [`BackupsNestApi`] for tier_1 tests — the page machine's whole
//! nest side without a nest. Mirrors `fauna_devices_machine::nest_api::fake`.
//!
//! Records every call so a test can assert *what the machine asked the nest*,
//! not merely what it rendered — the difference that catches a page which
//! renders a plausible default it never fetched.

#![cfg(any(test, feature = "test-helpers"))]

use std::sync::Mutex;

use async_trait::async_trait;

use fauna_protocol::filesync::{
    SnapshotCheckReply, SnapshotGetReply, SnapshotPruneSetPolicyReply, SnapshotSummaryRow,
};
use fauna_protocol::folders::FolderSummary as WireFolderSummary;

use super::{BackupsApiError, BackupsNestApi};

/// A dependency-free "yield to the executor once" future — `tokio::task::yield_now`
/// without the tokio dependency, since this module ships under a plain feature
/// flag rather than only in dev builds.
struct YieldOnce(bool);

impl std::future::Future for YieldOnce {
    type Output = ();
    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<()> {
        if self.0 {
            std::task::Poll::Ready(())
        } else {
            self.0 = true;
            cx.waker().wake_by_ref();
            std::task::Poll::Pending
        }
    }
}

/// One recorded call, for call-shape assertions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FakeCall {
    ListFolders,
    ListSnapshots {
        folder: String,
    },
    CreateSnapshot {
        folder: String,
        device_id: Option<Vec<u8>>,
    },
    DeleteSnapshot {
        snapshot_id: i64,
    },
    UndeleteSnapshot {
        snapshot_id: i64,
    },
    DeleteSnapshotImmediate {
        snapshot_id: i64,
        confirm_id: String,
        acknowledge: String,
    },
    PruneSetPolicy {
        folder: String,
        dry_run: bool,
    },
    GetSnapshot {
        snapshot_id: i64,
    },
    Check {
        folder: String,
        verify_content: bool,
    },
}

/// Scripted responses + a call log.
#[derive(Debug, Default)]
pub struct FakeBackupsNestApi {
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    folders: Vec<WireFolderSummary>,
    /// Snapshots per folder name.
    snapshots: std::collections::BTreeMap<String, Vec<SnapshotSummaryRow>>,
    check_reply: SnapshotCheckReply,
    prune_reply: SnapshotPruneSetPolicyReply,
    /// Per-snapshot-id `get` replies. A snapshot with no entry answers the
    /// default reply, which is an empty file list — the same thing a keyless
    /// custody wiring produces, so a test can tell the two apart only by what it
    /// seeded.
    get_replies: std::collections::BTreeMap<i64, SnapshotGetReply>,
    calls: Vec<FakeCall>,
    /// When set, the next seam call yields once before proceeding — see
    /// [`FakeBackupsNestApi::pause_next_call`].
    pause_next_call: bool,
    /// When set, the next `list_snapshots` call — and only that one — yields
    /// once. See [`FakeBackupsNestApi::pause_next_list_snapshots`].
    pause_next_list_snapshots: bool,
    /// When set, the next call of that shape fails with this error.
    fail_list_folders: Option<BackupsApiError>,
    fail_list_snapshots: Option<BackupsApiError>,
    fail_create: Option<BackupsApiError>,
    fail_delete: Option<BackupsApiError>,
    fail_undelete: Option<BackupsApiError>,
    fail_delete_immediate: Option<BackupsApiError>,
    fail_prune: Option<BackupsApiError>,
    fail_check: Option<BackupsApiError>,
    fail_get: Option<BackupsApiError>,
}

impl FakeBackupsNestApi {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_folders(&self, sets: Vec<WireFolderSummary>) {
        self.inner.lock().unwrap().folders = sets;
    }

    pub fn set_snapshots(&self, folder: &str, rows: Vec<SnapshotSummaryRow>) {
        self.inner
            .lock()
            .unwrap()
            .snapshots
            .insert(folder.to_string(), rows);
    }

    pub fn set_check_reply(&self, reply: SnapshotCheckReply) {
        self.inner.lock().unwrap().check_reply = reply;
    }

    pub fn set_prune_reply(&self, reply: SnapshotPruneSetPolicyReply) {
        self.inner.lock().unwrap().prune_reply = reply;
    }

    pub fn set_get_reply(&self, snapshot_id: i64, reply: SnapshotGetReply) {
        self.inner
            .lock()
            .unwrap()
            .get_replies
            .insert(snapshot_id, reply);
    }

    pub fn fail_list_folders(&self, err: BackupsApiError) {
        self.inner.lock().unwrap().fail_list_folders = Some(err);
    }
    pub fn fail_list_snapshots(&self, err: BackupsApiError) {
        self.inner.lock().unwrap().fail_list_snapshots = Some(err);
    }
    pub fn fail_create(&self, err: BackupsApiError) {
        self.inner.lock().unwrap().fail_create = Some(err);
    }
    pub fn fail_delete(&self, err: BackupsApiError) {
        self.inner.lock().unwrap().fail_delete = Some(err);
    }
    pub fn fail_undelete(&self, err: BackupsApiError) {
        self.inner.lock().unwrap().fail_undelete = Some(err);
    }
    pub fn fail_delete_immediate(&self, err: BackupsApiError) {
        self.inner.lock().unwrap().fail_delete_immediate = Some(err);
    }
    pub fn fail_prune(&self, err: BackupsApiError) {
        self.inner.lock().unwrap().fail_prune = Some(err);
    }
    pub fn fail_check(&self, err: BackupsApiError) {
        self.inner.lock().unwrap().fail_check = Some(err);
    }
    pub fn fail_get(&self, err: BackupsApiError) {
        self.inner.lock().unwrap().fail_get = Some(err);
    }

    /// Make the next seam call yield to the executor once before proceeding.
    ///
    /// This is what lets a single-flight test be *honest*: without a real
    /// suspension point the machine's op completes between two sequential
    /// `await`s and nothing is ever concurrent, so the test would pass against a
    /// machine that has no single-flight gate at all. With it, a `join!` of two
    /// gestures polls the second while the first is genuinely in flight.
    pub fn pause_next_call(&self) {
        self.inner.lock().unwrap().pause_next_call = true;
    }

    /// Make the next `list_snapshots` call yield once before answering — a load
    /// that has already read which folder is selected and is now fetching that
    /// folder's rows. [`Self::pause_next_call`] would stop at the folder list
    /// instead, before the load has chosen a folder.
    pub fn pause_next_list_snapshots(&self) {
        self.inner.lock().unwrap().pause_next_list_snapshots = true;
    }

    /// Take the pause flag, returning whether this call should yield.
    fn take_pause(&self) -> bool {
        std::mem::take(&mut self.inner.lock().unwrap().pause_next_call)
    }

    /// Every call the machine made, in order.
    pub fn calls(&self) -> Vec<FakeCall> {
        self.inner.lock().unwrap().calls.clone()
    }

    pub fn clear_calls(&self) {
        self.inner.lock().unwrap().calls.clear();
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl BackupsNestApi for FakeBackupsNestApi {
    async fn list_folders(&self) -> Result<Vec<WireFolderSummary>, BackupsApiError> {
        if self.take_pause() {
            YieldOnce(false).await;
        }
        let mut inner = self.inner.lock().unwrap();
        inner.calls.push(FakeCall::ListFolders);
        if let Some(err) = inner.fail_list_folders.take() {
            return Err(err);
        }
        Ok(inner.folders.clone())
    }

    async fn list_snapshots(
        &self,
        folder: &str,
    ) -> Result<Vec<SnapshotSummaryRow>, BackupsApiError> {
        let pause_here = std::mem::take(&mut self.inner.lock().unwrap().pause_next_list_snapshots);
        if self.take_pause() || pause_here {
            YieldOnce(false).await;
        }
        let mut inner = self.inner.lock().unwrap();
        inner.calls.push(FakeCall::ListSnapshots {
            folder: folder.to_string(),
        });
        if let Some(err) = inner.fail_list_snapshots.take() {
            return Err(err);
        }
        Ok(inner.snapshots.get(folder).cloned().unwrap_or_default())
    }

    async fn create_snapshot(
        &self,
        folder: &str,
        device_id: Option<Vec<u8>>,
    ) -> Result<(), BackupsApiError> {
        if self.take_pause() {
            YieldOnce(false).await;
        }
        let mut inner = self.inner.lock().unwrap();
        inner.calls.push(FakeCall::CreateSnapshot {
            folder: folder.to_string(),
            device_id,
        });
        if let Some(err) = inner.fail_create.take() {
            return Err(err);
        }
        Ok(())
    }

    async fn delete_snapshot(&self, snapshot_id: i64) -> Result<(), BackupsApiError> {
        if self.take_pause() {
            YieldOnce(false).await;
        }
        let mut inner = self.inner.lock().unwrap();
        inner.calls.push(FakeCall::DeleteSnapshot { snapshot_id });
        if let Some(err) = inner.fail_delete.take() {
            return Err(err);
        }
        Ok(())
    }

    async fn undelete_snapshot(&self, snapshot_id: i64) -> Result<(), BackupsApiError> {
        if self.take_pause() {
            YieldOnce(false).await;
        }
        let mut inner = self.inner.lock().unwrap();
        inner.calls.push(FakeCall::UndeleteSnapshot { snapshot_id });
        if let Some(err) = inner.fail_undelete.take() {
            return Err(err);
        }
        Ok(())
    }

    async fn delete_snapshot_immediate(
        &self,
        snapshot_id: i64,
        confirm_id: &str,
        acknowledge: &str,
    ) -> Result<(), BackupsApiError> {
        if self.take_pause() {
            YieldOnce(false).await;
        }
        let mut inner = self.inner.lock().unwrap();
        inner.calls.push(FakeCall::DeleteSnapshotImmediate {
            snapshot_id,
            confirm_id: confirm_id.to_string(),
            acknowledge: acknowledge.to_string(),
        });
        if let Some(err) = inner.fail_delete_immediate.take() {
            return Err(err);
        }
        Ok(())
    }

    async fn prune_set_policy(
        &self,
        folder: &str,
        dry_run: bool,
    ) -> Result<SnapshotPruneSetPolicyReply, BackupsApiError> {
        if self.take_pause() {
            YieldOnce(false).await;
        }
        let mut inner = self.inner.lock().unwrap();
        inner.calls.push(FakeCall::PruneSetPolicy {
            folder: folder.to_string(),
            dry_run,
        });
        if let Some(err) = inner.fail_prune.take() {
            return Err(err);
        }
        let mut reply = inner.prune_reply.clone();
        reply.dry_run = dry_run;
        Ok(reply)
    }

    async fn check(
        &self,
        folder: &str,
        verify_content: bool,
    ) -> Result<SnapshotCheckReply, BackupsApiError> {
        if self.take_pause() {
            YieldOnce(false).await;
        }
        let mut inner = self.inner.lock().unwrap();
        inner.calls.push(FakeCall::Check {
            folder: folder.to_string(),
            verify_content,
        });
        if let Some(err) = inner.fail_check.take() {
            return Err(err);
        }
        Ok(inner.check_reply.clone())
    }

    async fn get_snapshot(&self, snapshot_id: i64) -> Result<SnapshotGetReply, BackupsApiError> {
        if self.take_pause() {
            YieldOnce(false).await;
        }
        let mut inner = self.inner.lock().unwrap();
        inner.calls.push(FakeCall::GetSnapshot { snapshot_id });
        if let Some(err) = inner.fail_get.take() {
            return Err(err);
        }
        Ok(inner
            .get_replies
            .get(&snapshot_id)
            .cloned()
            .unwrap_or_default())
    }
}
