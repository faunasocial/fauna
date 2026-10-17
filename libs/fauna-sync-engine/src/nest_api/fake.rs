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
    FolderDepositsListRequest, FolderDepositsPartRequest, FolderDepositsRetireRequest,
    FolderUpdateRequest, ParkedDeposit,
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
    /// `fauna.folders.deposits.part` — one sealed part of a larger item.
    FetchDepositPart(FolderDepositsPartRequest),
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
    /// How many of the next conflict reports fail before reports land again —
    /// a resolved report refused, then the unresolved fallback accepted.
    fail_reports_remaining: usize,
    /// Reply flag for `update_folder`.
    update_ok: bool,
    /// The parked third-party deposits `list_deposits` serves, by folder id,
    /// oldest first; `retire_deposit` drains them.
    inbox: Vec<(i64, ParkedDeposit)>,
    /// The sealed parts of parked items larger than one message, by
    /// `(deposit id, index)`; they go with their item's retire.
    parts: Vec<((i64, u32), Vec<u8>)>,
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
                fail_reports_remaining: 0,
                update_ok: true,
                inbox: Vec::new(),
                parts: Vec::new(),
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

    /// A fake whose first `n` conflict reports fail and every later call
    /// succeeds — the ladder whose resolved rung is refused and whose
    /// unresolved (chooser) rung lands.
    pub fn failing_first_reports(n: usize) -> Self {
        let fake = Self::default();
        fake.inner.lock().unwrap().fail_reports_remaining = n;
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

    /// Park sealed part `index` of item `deposit_id`, as the nest's staged
    /// upload would.
    pub fn park_deposit_part(&self, deposit_id: i64, index: u32, sealed: Vec<u8>) {
        self.inner
            .lock()
            .unwrap()
            .parts
            .push(((deposit_id, index), sealed));
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
        if st.fail_reports_remaining > 0 {
            st.fail_reports_remaining -= 1;
            return Err(SyncControlError("scripted report refusal".to_string()));
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
        let retired = st.inbox.len() < before;
        if retired {
            st.parts.retain(|((d, _), _)| *d != req.deposit_id);
        }
        Ok(retired)
    }

    async fn fetch_deposit_part(
        &self,
        req: FolderDepositsPartRequest,
    ) -> Result<Vec<u8>, SyncControlError> {
        let mut st = self.inner.lock().unwrap();
        st.calls
            .push(RecordedControlCall::FetchDepositPart(req.clone()));
        if let Some(msg) = &st.fail_with {
            return Err(SyncControlError(msg.clone()));
        }
        let parked = st
            .inbox
            .iter()
            .any(|(f, d)| *f == req.folder_id && d.id == req.deposit_id);
        st.parts
            .iter()
            .find(|((d, i), _)| parked && *d == req.deposit_id && *i == req.index)
            .map(|(_, sealed)| sealed.clone())
            .ok_or_else(|| SyncControlError("no such deposit part".into()))
    }
}
