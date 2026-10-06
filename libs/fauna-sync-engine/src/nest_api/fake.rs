//! In-memory double for the [`SyncControlApi`] seam — the piece whose absence
//! made `ResolvedApply::KeepLocal` unreachable in this crate's tests.
//!
//! Records every call so a test can assert *what* was reported (the sealed
//! request the nest would have seen, not a paraphrase), and can be scripted to
//! fail so the fail-closed degradation to `Unresolved` stays testable too — both
//! sides of the branch, from one double.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fauna_protocol::folders::{
    ConflictReportRequest, ConflictResolveRequest, FolderDepositsListReply,
    FolderDepositsListRequest, FolderDepositsRetireRequest, FolderUpdateRequest, ParkedDeposit,
};
use fauna_protocol::web::WebFilesPruneSealedRequest;

use super::{SyncControlApi, SyncControlError};

/// One control-plane call the fake observed, in call order.
#[derive(Debug, Clone)]
pub enum RecordedControlCall {
    /// `fauna.sync.conflicts.report` — the full wire request, so a test can
    /// assert the resolution kind, the winning manifest, and that the path
    /// arrived **sealed** exactly as the engine sealed it.
    ReportConflict(Box<ConflictReportRequest>),
    /// `fauna.sync.conflicts.resolve` — the full wire request.
    ResolveConflict(Box<ConflictResolveRequest>),
    /// `fauna.folders.update` — the sealed set-name stamp.
    UpdateFolder(Box<FolderUpdateRequest>),
    /// `fauna.web.files.prune_sealed` — the complete-set declaration, so a test
    /// can assert **which paths** were declared live (the whole point: a
    /// truncated set would delete live content) and that it was sent only after
    /// the walk.
    PruneSealedWebFiles(Box<WebFilesPruneSealedRequest>),
    /// `fauna.folders.deposits.list` — one page of the parked inbox.
    ListDeposits(FolderDepositsListRequest),
    /// `fauna.folders.deposits.retire` — the drain after adoption.
    RetireDeposit(FolderDepositsRetireRequest),
}

#[derive(Debug, Default)]
struct FakeState {
    calls: Vec<RecordedControlCall>,
    /// Next conflict-row id to hand back; increments per successful report so
    /// two conflicts in one test are distinguishable.
    next_conflict_id: i64,
    /// When set, every call fails with this message — the fail-closed arm.
    fail_with: Option<String>,
    /// Reply flag for `update_folder`.
    update_ok: bool,
    /// The parked third-party deposits `list_deposits` serves, by folder id,
    /// oldest first; `retire_deposit` drains them.
    inbox: Vec<(i64, ParkedDeposit)>,
}

/// Recording, scriptable [`SyncControlApi`]. Cheap-clone via `Arc<Mutex<..>>`,
/// so a test can hold a handle for assertions while the engine holds it as
/// `Arc<dyn SyncControlApi>`.
#[derive(Debug, Clone)]
pub struct FakeSyncControl {
    inner: Arc<Mutex<FakeState>>,
}

impl Default for FakeSyncControl {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(FakeState {
                calls: Vec::new(),
                next_conflict_id: 1,
                fail_with: None,
                update_ok: true,
                inbox: Vec::new(),
            })),
        }
    }
}

impl FakeSyncControl {
    /// A fake that accepts every call — the shape that lets a resolved report
    /// succeed and so drives the engine to `KeepLocal`/`ApplyIncoming` rather
    /// than the `Unresolved` fallback.
    pub fn accepting() -> Self {
        Self::default()
    }

    /// A fake whose every call fails — pins the fail-closed arm
    /// (`fallback_unresolved!`): local bytes kept, nothing recorded as resolved.
    pub fn failing(message: impl Into<String>) -> Self {
        let fake = Self::default();
        fake.inner.lock().unwrap().fail_with = Some(message.into());
        fake
    }

    /// Every call the engine made, in order.
    pub fn calls(&self) -> Vec<RecordedControlCall> {
        self.inner.lock().unwrap().calls.clone()
    }

    /// Just the sealed-prune declarations, in order.
    pub fn prune_declarations(&self) -> Vec<WebFilesPruneSealedRequest> {
        self.inner
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter_map(|c| match c {
                RecordedControlCall::PruneSealedWebFiles(r) => Some((**r).clone()),
                _ => None,
            })
            .collect()
    }

    /// Make every later call fail (`Some`) or succeed again (`None`) — a lost
    /// reply followed by the pass that re-sends it.
    pub fn set_failing(&self, message: Option<String>) {
        self.inner.lock().unwrap().fail_with = message;
    }

    /// Park a sealed deposit in `folder_id`'s inbox, as the nest's deposit
    /// door would.
    pub fn park_deposit(&self, folder_id: i64, item: ParkedDeposit) {
        self.inner.lock().unwrap().inbox.push((folder_id, item));
    }

    /// The ids still parked in `folder_id`'s inbox.
    pub fn parked_ids(&self, folder_id: i64) -> Vec<i64> {
        self.inner
            .lock()
            .unwrap()
            .inbox
            .iter()
            .filter(|(f, _)| *f == folder_id)
            .map(|(_, d)| d.id)
            .collect()
    }

    /// Just the conflict resolves, in order.
    pub fn conflict_resolves(&self) -> Vec<ConflictResolveRequest> {
        self.inner
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter_map(|c| match c {
                RecordedControlCall::ResolveConflict(r) => Some((**r).clone()),
                _ => None,
            })
            .collect()
    }

    /// Just the conflict reports, in order — the common assertion.
    pub fn conflict_reports(&self) -> Vec<ConflictReportRequest> {
        self.inner
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter_map(|c| match c {
                RecordedControlCall::ReportConflict(r) => Some((**r).clone()),
                _ => None,
            })
            .collect()
    }
}

#[async_trait]
impl SyncControlApi for FakeSyncControl {
    async fn report_conflict(&self, req: ConflictReportRequest) -> Result<i64, SyncControlError> {
        let mut st = self.inner.lock().unwrap();
        st.calls
            .push(RecordedControlCall::ReportConflict(Box::new(req)));
        if let Some(msg) = &st.fail_with {
            return Err(SyncControlError(msg.clone()));
        }
        let id = st.next_conflict_id;
        st.next_conflict_id += 1;
        Ok(id)
    }

    async fn resolve_conflict(
        &self,
        req: ConflictResolveRequest,
    ) -> Result<bool, SyncControlError> {
        let mut st = self.inner.lock().unwrap();
        st.calls
            .push(RecordedControlCall::ResolveConflict(Box::new(req)));
        if let Some(msg) = &st.fail_with {
            return Err(SyncControlError(msg.clone()));
        }
        Ok(true)
    }

    async fn update_folder(&self, req: FolderUpdateRequest) -> Result<bool, SyncControlError> {
        let mut st = self.inner.lock().unwrap();
        st.calls
            .push(RecordedControlCall::UpdateFolder(Box::new(req)));
        if let Some(msg) = &st.fail_with {
            return Err(SyncControlError(msg.clone()));
        }
        Ok(st.update_ok)
    }

    async fn prune_sealed_web_files(
        &self,
        req: WebFilesPruneSealedRequest,
    ) -> Result<u32, SyncControlError> {
        let mut st = self.inner.lock().unwrap();
        st.calls
            .push(RecordedControlCall::PruneSealedWebFiles(Box::new(req)));
        if let Some(msg) = &st.fail_with {
            return Err(SyncControlError(msg.clone()));
        }
        Ok(0)
    }

    async fn list_deposits(
        &self,
        req: FolderDepositsListRequest,
    ) -> Result<FolderDepositsListReply, SyncControlError> {
        let mut st = self.inner.lock().unwrap();
        st.calls
            .push(RecordedControlCall::ListDeposits(req.clone()));
        if let Some(msg) = &st.fail_with {
            return Err(SyncControlError(msg.clone()));
        }
        Ok(FolderDepositsListReply {
            items: st
                .inbox
                .iter()
                .filter(|(f, d)| *f == req.folder_id && d.id > req.after)
                .map(|(_, d)| d.clone())
                .collect(),
            more: false,
            extra: Default::default(),
        })
    }

    async fn retire_deposit(
        &self,
        req: FolderDepositsRetireRequest,
    ) -> Result<bool, SyncControlError> {
        let mut st = self.inner.lock().unwrap();
        st.calls
            .push(RecordedControlCall::RetireDeposit(req.clone()));
        if let Some(msg) = &st.fail_with {
            return Err(SyncControlError(msg.clone()));
        }
        let before = st.inbox.len();
        st.inbox
            .retain(|(f, d)| !(*f == req.folder_id && d.id == req.deposit_id));
        Ok(st.inbox.len() < before)
    }
}
