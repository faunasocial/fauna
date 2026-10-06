//! The **shared** in-memory doubles of the account-state seams
//! ([`crate::store_seam`]) — one home per seam's test semantics, so no consumer
//! crate hand-rolls a vacuous one. Each models its seam's *contract*, not the
//! account store: a write joins through the kind's real merge, and the faults a
//! caller must survive are injected by name.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use fauna_core::custody_ceremony::CustodyConfig;
use fauna_core::data::{
    DeploymentSeedEntry, FollowedFolder, FollowsConfig, MailConfig, MailCredential,
    MsekFingerprint, Timestamp,
};
use fauna_core::identity::ActorId;

use fauna_core::backup_state::{
    BackupDestinationsRow, BackupRecord, BackupState, decode_backup_row, destination_lists,
    destination_marks, destinations_key,
};
use fauna_core::data::{BackupConfig, BackupDestination, DestinationUnattestedMark};

use crate::store_seam::{
    BackupStateStore, CustodyCeremonyStore, DeploymentSeedStore, FollowsStore, MailStore,
    StoreError, SuccessionLedgerStore,
};
use fauna_core::mail_rows::{MailRows, MailStateRow};
use fauna_core::succession_ledger::SuccessionLedger;

/// The shared in-memory [`SuccessionLedgerStore`] double — one home for its
/// semantics, so no consumer hand-rolls a vacuous one.
///
/// A write joins through the real [`SuccessionLedger::merge`] (the per-row
/// arms' composite), so a replica that moves nothing leaves the stored value
/// unchanged, exactly as the handle's door puts nothing. The one fault it
/// injects is the door's transient **no-tip refusal**
/// ([`Self::refuse_next_merges`]) — the refusal every post-store-ready leg
/// must survive by staying owed.
#[derive(Clone)]
pub struct FakeSuccessionLedgerStore {
    inner: Arc<Mutex<FakeLedgerInner>>,
}

struct FakeLedgerInner {
    self_actor: ActorId,
    ledger: SuccessionLedger,
    refuse: usize,
    accept_budget: Option<usize>,
    merges: usize,
    not_ready: bool,
}

impl FakeLedgerInner {
    /// Whether this write meets the door's refusal (either knob).
    fn refused(&mut self) -> bool {
        if self.refuse > 0 {
            self.refuse -= 1;
            return true;
        }
        match self.accept_budget.as_mut() {
            Some(0) => true,
            Some(n) => {
                *n -= 1;
                false
            }
            None => false,
        }
    }
}

impl FakeSuccessionLedgerStore {
    /// A store holding no row yet, read as `actor`.
    pub fn empty(actor: ActorId) -> Self {
        Self::with(SuccessionLedger::empty(actor))
    }

    /// A store holding `ledger`, read as the identity its chain names.
    pub fn with(ledger: SuccessionLedger) -> Self {
        Self::serving(ledger.actor_id, ledger)
    }

    /// A store holding `ledger`, read as `self_actor` — a successor's runtime
    /// over rows a predecessor's chain still names (the state before the
    /// post-store-ready pass's re-point).
    pub fn serving(self_actor: ActorId, ledger: SuccessionLedger) -> Self {
        Self {
            inner: Arc::new(Mutex::new(FakeLedgerInner {
                self_actor,
                ledger,
                refuse: 0,
                accept_budget: None,
                merges: 0,
                not_ready: false,
            })),
        }
    }

    /// What is stored right now.
    pub fn current(&self) -> SuccessionLedger {
        self.inner.lock().unwrap().ledger.clone()
    }

    /// Overwrite what is stored — a test seeding marks a sibling wrote.
    pub fn replace(&self, ledger: SuccessionLedger) {
        self.inner.lock().unwrap().ledger = ledger;
    }

    /// Edit what is stored in place — a test seeding rows before the code
    /// under test runs.
    pub fn mutate(&self, f: impl FnOnce(&mut SuccessionLedger)) {
        f(&mut self.inner.lock().unwrap().ledger);
    }

    /// Refuse the next `n` merges the way the writer door refuses a put while
    /// no generation tip resolves.
    pub fn refuse_next_merges(&self, n: usize) {
        self.inner.lock().unwrap().refuse = n;
    }

    /// Accept `n` more writes, then refuse every further one until
    /// [`Self::stop_refusing`] — the crash window between two of a leg's
    /// writes, as a knob.
    pub fn refuse_after(&self, n: usize) {
        self.inner.lock().unwrap().accept_budget = Some(n);
    }

    /// Clear [`Self::refuse_after`] and [`Self::refuse_next_merges`].
    pub fn stop_refusing(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.accept_budget = None;
        inner.refuse = 0;
    }

    /// Answer every call [`crate::LEDGER_NOT_READY`] (or stop) — a host whose
    /// account store has not assembled yet, as `ResolvingLedgerStore` answers
    /// once its wait runs out.
    pub fn set_not_ready(&self, not_ready: bool) {
        self.inner.lock().unwrap().not_ready = not_ready;
    }

    /// How many merges were accepted.
    pub fn merges(&self) -> usize {
        self.inner.lock().unwrap().merges
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl SuccessionLedgerStore for FakeSuccessionLedgerStore {
    fn self_actor(&self) -> Result<ActorId, StoreError> {
        Ok(self.inner.lock().unwrap().self_actor)
    }

    async fn load(&self) -> Result<SuccessionLedger, StoreError> {
        if self.inner.lock().unwrap().not_ready {
            return Err(StoreError::Load(crate::LEDGER_NOT_READY.into()));
        }
        Ok(self.current())
    }

    async fn merge(&self, replica: SuccessionLedger) -> Result<SuccessionLedger, StoreError> {
        let mut inner = self.inner.lock().unwrap();
        if inner.not_ready {
            return Err(StoreError::Save(crate::LEDGER_NOT_READY.into()));
        }
        if inner.refused() {
            return Err(StoreError::Save(
                "fake door: no generation tip resolves yet".into(),
            ));
        }
        inner.ledger = inner.ledger.merge(&replica);
        inner.merges += 1;
        Ok(inner.ledger.clone())
    }

    async fn repoint(&self, retired: ActorId) -> Result<bool, StoreError> {
        let mut inner = self.inner.lock().unwrap();
        if inner.refused() {
            return Err(StoreError::Save(
                "fake door: no generation tip resolves yet".into(),
            ));
        }
        let self_actor = inner.self_actor;
        let mut chain = inner.ledger.chain();
        chain.repoint_to_successor(retired, self_actor);
        if chain == inner.ledger.chain() {
            return Ok(false);
        }
        inner.ledger.actor_id = chain.actor_id;
        inner.ledger.prior_actor_ids = chain.prior_actor_ids;
        inner.merges += 1;
        Ok(true)
    }

    async fn raise_grant_marks(&self, predecessor: ActorId) -> Result<bool, StoreError> {
        let mut inner = self.inner.lock().unwrap();
        if inner.refused() {
            return Err(StoreError::Save(
                "fake door: no generation tip resolves yet".into(),
            ));
        }
        // The door's rule: live grants whose latest event `predecessor` signed.
        let marks: Vec<fauna_core::data::GrantUnattestedMark> =
            fauna_core::grant_event::latest_live_events(&inner.ledger)
                .into_iter()
                .filter(|e| e.verify(&predecessor).is_ok())
                .map(|e| fauna_core::data::GrantUnattestedMark {
                    grant_id: e.grant_id.clone(),
                    predecessor,
                    verdict: fauna_core::data::UnattestedVerdict::Open,
                })
                .collect();
        let replica = SuccessionLedger {
            unattested_grant_marks: marks,
            ..SuccessionLedger::empty(inner.ledger.actor_id)
        };
        let joined = inner.ledger.merge(&replica);
        if joined == inner.ledger {
            return Ok(false);
        }
        inner.ledger = joined;
        inner.merges += 1;
        Ok(true)
    }
}

/// When a [`FakeDeploymentSeedStore`] merge reaches "the bound nest" — the
/// account runtime's publish step, as a knob.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeedPublish {
    /// Every moved row is published as the merge lands.
    OnMerge,
    /// A moved row is published once [`DeploymentSeedStore::seed_published`]
    /// has been asked about it this many times — a pump that ships the write
    /// a little after it lands, so a caller that never waits is caught.
    AfterPolls(usize),
    /// Nothing is ever published — the pump is wedged or offline.
    Never,
}

/// The shared in-memory [`DeploymentSeedStore`] double — the plane custody
/// map with the door's refusals and the publish step as knobs.
///
/// A merge joins through the shipped rule
/// ([`DeploymentSeedEntry::merge_seed_map`]) and refuses, as the plane's
/// strict decode does, an entry whose seed is not its id's preimage. Every
/// row a merge moved is **unpublished** until [`SeedPublish`] says
/// otherwise; [`Self::published`] is what the bound nest holds — the view a
/// fake nest snapshots when the rotate ceremony reaches it.
#[derive(Clone)]
pub struct FakeDeploymentSeedStore {
    inner: Arc<Mutex<FakeSeedInner>>,
}

struct FakeSeedInner {
    seeds: Vec<DeploymentSeedEntry>,
    /// Rows the publish step has not shipped yet, with the polls each has
    /// seen.
    pending: BTreeMap<[u8; 32], usize>,
    publish: SeedPublish,
    refuse: usize,
    accept_budget: Option<usize>,
    merges: usize,
    /// Every read refused — the store not up, or its read failing.
    refuse_reads: bool,
}

impl FakeSeedInner {
    /// Whether this merge meets the door's refusal (either knob).
    fn refused(&mut self) -> bool {
        if self.refuse > 0 {
            self.refuse -= 1;
            return true;
        }
        match self.accept_budget.as_mut() {
            Some(0) => true,
            Some(n) => {
                *n -= 1;
                false
            }
            None => false,
        }
    }
}

impl FakeDeploymentSeedStore {
    /// A store holding no row yet.
    pub fn empty() -> Self {
        Self::with(Vec::new())
    }

    /// A store holding `seeds`, every row already published.
    pub fn with(seeds: Vec<DeploymentSeedEntry>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(FakeSeedInner {
                seeds,
                pending: BTreeMap::new(),
                publish: SeedPublish::OnMerge,
                refuse: 0,
                accept_budget: None,
                merges: 0,
                refuse_reads: false,
            })),
        }
    }

    /// Refuse every read from now on, as a store that is not up does.
    pub fn refuse_reads(&self) {
        self.inner.lock().unwrap().refuse_reads = true;
    }

    /// What is stored right now.
    pub fn current(&self) -> Vec<DeploymentSeedEntry> {
        self.inner.lock().unwrap().seeds.clone()
    }

    /// The rows the bound nest holds — every stored row the publish step has
    /// shipped, as it stands.
    pub fn published(&self) -> Vec<DeploymentSeedEntry> {
        let inner = self.inner.lock().unwrap();
        inner
            .seeds
            .iter()
            .filter(|e| !inner.pending.contains_key(&e.nest_actor_id))
            .cloned()
            .collect()
    }

    /// Set when a moved row is published.
    pub fn set_publish(&self, publish: SeedPublish) {
        self.inner.lock().unwrap().publish = publish;
    }

    /// Ship every pending row now — the pump catching up.
    pub fn publish_all(&self) {
        self.inner.lock().unwrap().pending.clear();
    }

    /// Refuse the next `n` merges the way the writer door refuses a put while
    /// no generation tip resolves.
    pub fn refuse_next_merges(&self, n: usize) {
        self.inner.lock().unwrap().refuse = n;
    }

    /// Accept `n` more merges, then refuse every further one until
    /// [`Self::stop_refusing`] — the crash window between two of a drive's
    /// writes, as a knob.
    pub fn refuse_after(&self, n: usize) {
        self.inner.lock().unwrap().accept_budget = Some(n);
    }

    /// Clear [`Self::refuse_after`] and [`Self::refuse_next_merges`].
    pub fn stop_refusing(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.accept_budget = None;
        inner.refuse = 0;
    }

    /// How many merges were accepted.
    pub fn merges(&self) -> usize {
        self.inner.lock().unwrap().merges
    }
}

/// The shared in-memory [`MailStore`] double — the account-store door's
/// semantics (`fauna_account_plane::mail_rows`) over [`MailRows`], so a test
/// sees exactly what production writes do: every write a read-join-put
/// through the rows' own joins (`fauna_core::mail_rows`), stamped strictly
/// above the stored row, answering whether anything moved, never deleting.
///
/// `Clone` shares the rows, so two machines built over clones behave like two
/// devices whose writes the plane has already converged. A device whose
/// replica lags is modelled by the peer hooks instead: [`Self::join_peer_state`]
/// joins a sibling's state row the way a walk would, and [`Self::on_write`]
/// lands one right after one of this device's writes — the window the rotation
/// finalize's re-drive exists for (`mail-credentials.md` § Cross-device
/// finalize race).
#[derive(Clone, Default)]
pub struct FakeMailStore {
    inner: Arc<Mutex<FakeMailInner>>,
}

/// A write hook: runs after each accepted write until it answers `true`.
type InjectedRowsWrite = Box<dyn FnMut(&mut MailRows) -> bool + Send>;

#[derive(Default)]
struct FakeMailInner {
    rows: MailRows,
    /// The last stamp handed out — writes stamp strictly above it (and above
    /// the stored row), so ordering never depends on the wall clock's grain.
    clock: u64,
    refuse: usize,
    writes: usize,
    on_write: Option<InjectedRowsWrite>,
}

impl FakeMailInner {
    fn stamp_above(&mut self, stored: Option<Timestamp>) -> Timestamp {
        let floor = stored.map(|t| t.0).unwrap_or(0);
        self.clock = self.clock.max(Timestamp::now().0).max(floor) + 1;
        Timestamp(self.clock)
    }

    fn refused(&mut self) -> Result<(), StoreError> {
        if self.refuse > 0 {
            self.refuse -= 1;
            return Err(StoreError::Save(
                "fake door: no generation tip resolves yet".into(),
            ));
        }
        Ok(())
    }

    fn landed(&mut self) {
        self.writes += 1;
        if let Some(mut f) = self.on_write.take()
            && !f(&mut self.rows)
        {
            self.on_write = Some(f);
        }
    }

    fn join_credential(&mut self, intent: MailCredential) -> bool {
        let joined = match self.rows.credentials.get(&intent.credential_id) {
            Some(stored) => stored.merge(&intent),
            None => intent.merge(&intent),
        };
        if self.rows.credentials.get(&joined.credential_id) == Some(&joined) {
            return false;
        }
        self.rows
            .credentials
            .insert(joined.credential_id.clone(), joined);
        true
    }
}

impl FakeMailStore {
    /// A store holding no mail row — mail never enabled.
    pub fn empty() -> Self {
        Self::default()
    }

    /// A store holding `mail`'s rows, as if a device had written them.
    pub fn with(mail: &MailConfig) -> Self {
        let store = Self::default();
        store.seed(mail);
        store
    }

    /// Overwrite what is stored with `mail`'s rows (the state row stamped
    /// now, each credential at its own stamp).
    pub fn seed(&self, mail: &MailConfig) {
        let mut inner = self.inner.lock().unwrap();
        let at = inner.stamp_above(None);
        let mut rows = MailRows {
            state: Some(MailStateRow::from_config(mail, at)),
            ..MailRows::default()
        };
        for c in &mail.credentials {
            rows.credentials.insert(c.credential_id.clone(), c.clone());
        }
        inner.rows = rows;
    }

    /// The READ fold of what is stored.
    pub fn current(&self) -> MailConfig {
        self.inner.lock().unwrap().rows.config()
    }

    /// The rows themselves, revoked included.
    pub fn rows(&self) -> MailRows {
        self.inner.lock().unwrap().rows.clone()
    }

    /// Edit the stored rows in place — a test seeding what a sibling wrote.
    pub fn mutate(&self, f: impl FnOnce(&mut MailRows)) {
        f(&mut self.inner.lock().unwrap().rows);
    }

    /// Join a sibling's state row into what is stored — the plane walk's arm.
    pub fn join_peer_state(&self, peer: &MailStateRow) {
        Self::join_state_into(&mut self.inner.lock().unwrap().rows, peer);
    }

    /// Run `f` on the rows right after each of this store's accepted writes
    /// until it answers `true` (it fired) — a sibling's row the walk merges in
    /// between this device's write and its read back. `f` sees the rows the
    /// write left, so it can wait for the write it means (a swap, a clear).
    pub fn on_write(&self, f: impl FnMut(&mut MailRows) -> bool + Send + 'static) {
        self.inner.lock().unwrap().on_write = Some(Box::new(f));
    }

    /// Join `peer`'s state row into `rows` — the walk's arm, for an
    /// [`Self::on_write`] hook landing a sibling's row.
    pub fn join_state_into(rows: &mut MailRows, peer: &MailStateRow) {
        rows.state = Some(match &rows.state {
            Some(stored) => stored.merge(peer),
            None => peer.clone(),
        });
    }

    /// Refuse the next `n` writes the way the writer door refuses a put while
    /// no generation tip resolves.
    pub fn refuse_next_writes(&self, n: usize) {
        self.inner.lock().unwrap().refuse = n;
    }

    /// How many writes moved a row.
    pub fn writes(&self) -> usize {
        self.inner.lock().unwrap().writes
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl DeploymentSeedStore for FakeDeploymentSeedStore {
    async fn seeds(&self) -> Result<Vec<DeploymentSeedEntry>, StoreError> {
        if self.inner.lock().unwrap().refuse_reads {
            return Err(StoreError::Load("the store is not up".into()));
        }
        Ok(self.current())
    }

    async fn merge_seeds(
        &self,
        replica: Vec<DeploymentSeedEntry>,
    ) -> Result<Vec<DeploymentSeedEntry>, StoreError> {
        let mut inner = self.inner.lock().unwrap();
        if inner.refused() {
            return Err(StoreError::Save(
                "fake door: no generation tip resolves yet".into(),
            ));
        }
        // The plane's strict decode: a row certifies itself.
        if let Some(bad) = replica.iter().find(|e| {
            fauna_core::data::DeploymentSeedEntry::nest_actor_id_for_seed(e.seed.to_array())
                != e.nest_actor_id
        }) {
            return Err(StoreError::Save(format!(
                "fake door: the seed for {} is not its id's preimage",
                fauna_core::hex32::encode(&bad.nest_actor_id)
            )));
        }
        let joined = DeploymentSeedEntry::merge_seed_map(&inner.seeds, &replica);
        let moved: Vec<[u8; 32]> = joined
            .iter()
            .filter(|j| !inner.seeds.contains(j))
            .map(|j| j.nest_actor_id)
            .collect();
        inner.seeds = joined;
        inner.merges += 1;
        if inner.publish != SeedPublish::OnMerge {
            for id in moved {
                inner.pending.insert(id, 0);
            }
        }
        Ok(inner.seeds.clone())
    }

    async fn seed_published(&self, nest_actor_id: [u8; 32]) -> Result<bool, StoreError> {
        let mut inner = self.inner.lock().unwrap();
        if !inner.seeds.iter().any(|e| e.nest_actor_id == nest_actor_id) {
            return Ok(false);
        }
        let publish = inner.publish;
        let Some(polls) = inner.pending.get_mut(&nest_actor_id) else {
            return Ok(true);
        };
        *polls += 1;
        if matches!(publish, SeedPublish::AfterPolls(n) if *polls >= n) {
            inner.pending.remove(&nest_actor_id);
            return Ok(true);
        }
        Ok(false)
    }
}

/// The shared in-memory [`CustodyCeremonyStore`] double — the
/// custody-ceremony twin of [`FakeSuccessionLedgerStore`], for the same
/// reason: one home for its semantics.
///
/// A write joins through the real [`CustodyConfig::merge`] (the plane arm's
/// per-record halves, composed), so it keeps exactly what the handle's door
/// keeps: nothing is removed, a monotone mark is never un-set, and a replica
/// that moves nothing leaves the stored value unchanged. The one fault it
/// injects is the door's transient **no-tip refusal**
/// ([`Self::refuse_next_merges`]).
#[derive(Clone, Default)]
pub struct FakeCustodyCeremonyStore {
    inner: Arc<Mutex<FakeCustodyInner>>,
}

#[derive(Default)]
struct FakeCustodyInner {
    custody: CustodyConfig,
    refuse: usize,
    merges: usize,
}

impl FakeCustodyCeremonyStore {
    /// A store holding no row yet.
    pub fn empty() -> Self {
        Self::default()
    }

    /// What is stored right now.
    pub fn current(&self) -> CustodyConfig {
        self.inner.lock().unwrap().custody.clone()
    }

    /// Edit what is stored in place, bypassing the join — a test seeding a
    /// state another device's write could leave behind.
    pub fn mutate<T>(&self, f: impl FnOnce(&mut CustodyConfig) -> T) -> T {
        f(&mut self.inner.lock().unwrap().custody)
    }

    /// Refuse the next `n` merges the way the writer door refuses a put while
    /// no generation tip resolves.
    pub fn refuse_next_merges(&self, n: usize) {
        self.inner.lock().unwrap().refuse = n;
    }

    /// How many merges were accepted.
    pub fn merges(&self) -> usize {
        self.inner.lock().unwrap().merges
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl CustodyCeremonyStore for FakeCustodyCeremonyStore {
    async fn custody(&self) -> Result<CustodyConfig, StoreError> {
        Ok(self.current())
    }

    async fn merge_custody(&self, replica: CustodyConfig) -> Result<CustodyConfig, StoreError> {
        let mut inner = self.inner.lock().unwrap();
        if inner.refuse > 0 {
            inner.refuse -= 1;
            return Err(StoreError::Save(
                "fake door: no generation tip resolves yet".into(),
            ));
        }
        inner.custody = inner.custody.merge(&replica);
        inner.merges += 1;
        Ok(inner.custody.clone())
    }
}

/// The shared in-memory [`FollowsStore`] double — the followed-folders twin
/// of [`FakeCustodyCeremonyStore`], for the same reason: one home for its
/// semantics.
///
/// It keeps exactly what the handle's door keeps: one entry per folder at
/// [`FollowedFolder::plane_key`], a put replacing that one entry (and
/// answering `false` when it already equals the value), an unfollow removing
/// it (and answering `false` when it was absent), the read folded in
/// canonical order. The one fault it injects is the door's transient
/// **no-tip refusal** ([`Self::refuse_next_writes`]).
#[derive(Clone, Default)]
pub struct FakeFollowsStore {
    inner: Arc<Mutex<FakeFollowsInner>>,
}

#[derive(Default)]
struct FakeFollowsInner {
    rows: BTreeMap<String, FollowedFolder>,
    refuse: usize,
    writes: usize,
}

impl FakeFollowsStore {
    /// A store holding no follow yet.
    pub fn empty() -> Self {
        Self::default()
    }

    /// What is stored right now, as the read folds it.
    pub fn current(&self) -> FollowsConfig {
        let mut follows = FollowsConfig {
            followed: self.inner.lock().unwrap().rows.values().cloned().collect(),
        };
        follows.sort_canonically();
        follows
    }

    /// Refuse the next `n` writes the way the writer door refuses one while
    /// no generation tip resolves.
    pub fn refuse_next_writes(&self, n: usize) {
        self.inner.lock().unwrap().refuse = n;
    }

    /// How many writes changed a row.
    pub fn writes(&self) -> usize {
        self.inner.lock().unwrap().writes
    }
}

impl FakeFollowsInner {
    fn refused(&mut self) -> Result<(), StoreError> {
        if self.refuse > 0 {
            self.refuse -= 1;
            return Err(StoreError::Save(
                "fake door: no generation tip resolves yet".into(),
            ));
        }
        Ok(())
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl FollowsStore for FakeFollowsStore {
    async fn follows(&self) -> Result<FollowsConfig, StoreError> {
        Ok(self.current())
    }

    async fn put_follow(&self, follow: FollowedFolder) -> Result<bool, StoreError> {
        let mut inner = self.inner.lock().unwrap();
        inner.refused()?;
        let key = follow.plane_key();
        if inner.rows.get(&key) == Some(&follow) {
            return Ok(false);
        }
        inner.rows.insert(key, follow);
        inner.writes += 1;
        Ok(true)
    }

    async fn unfollow(&self, home_nest_url: String, folder_id: i64) -> Result<bool, StoreError> {
        let mut inner = self.inner.lock().unwrap();
        inner.refused()?;
        let key = FollowedFolder::plane_key_of(&home_nest_url, folder_id);
        if inner.rows.remove(&key).is_none() {
            return Ok(false);
        }
        inner.writes += 1;
        Ok(true)
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl MailStore for FakeMailStore {
    async fn load(&self) -> Result<MailConfig, StoreError> {
        Ok(self.current())
    }

    async fn load_rows(&self) -> Result<MailRows, StoreError> {
        Ok(self.rows())
    }

    async fn write_state(&self, state: MailStateRow) -> Result<bool, StoreError> {
        let mut inner = self.inner.lock().unwrap();
        inner.refused()?;
        let stored = inner.rows.state.clone();
        if stored.as_ref().is_some_and(|s| s.same_content(&state)) {
            return Ok(false);
        }
        let intent = MailStateRow {
            updated_at: inner.stamp_above(stored.as_ref().map(|s| s.updated_at)),
            ..state
        };
        let joined = match &stored {
            Some(s) => s.merge(&intent),
            None => intent.merge(&intent),
        };
        if stored.as_ref() == Some(&joined) {
            return Ok(false);
        }
        inner.rows.state = Some(joined);
        inner.landed();
        Ok(true)
    }

    async fn put_credential(&self, credential: MailCredential) -> Result<bool, StoreError> {
        let mut inner = self.inner.lock().unwrap();
        inner.refused()?;
        if credential.credential_id.is_empty() {
            return Err(StoreError::Save(
                "a mail credential needs a credential_id to key its row".into(),
            ));
        }
        let stored = inner
            .rows
            .credentials
            .get(&credential.credential_id)
            .cloned();
        if let Some(c) = &stored
            && *c
                == (MailCredential {
                    updated_at: c.updated_at,
                    ..credential.clone()
                })
        {
            return Ok(false);
        }
        let intent = MailCredential {
            updated_at: inner.stamp_above(stored.as_ref().map(|c| c.updated_at)),
            ..credential
        };
        let moved = inner.join_credential(intent);
        if moved {
            inner.landed();
        }
        Ok(moved)
    }

    async fn mark_wrapped(
        &self,
        credential_id: String,
        fingerprint: MsekFingerprint,
    ) -> Result<bool, StoreError> {
        let mut inner = self.inner.lock().unwrap();
        inner.refused()?;
        let Some(stored) = inner.rows.credentials.get(&credential_id).cloned() else {
            return Ok(false);
        };
        if stored.is_marked() || stored.wrapped_under == Some(fingerprint) {
            return Ok(false);
        }
        let intent = MailCredential {
            wrapped_under: Some(fingerprint),
            updated_at: inner.stamp_above(Some(stored.updated_at)),
            ..stored
        };
        let moved = inner.join_credential(intent);
        if moved {
            inner.landed();
        }
        Ok(moved)
    }

    async fn revoke(&self, credential_id: String) -> Result<bool, StoreError> {
        let mut inner = self.inner.lock().unwrap();
        inner.refused()?;
        let Some(stored) = inner.rows.credentials.get(&credential_id).cloned() else {
            return Ok(false);
        };
        if stored.revoked_at_unix.is_some() {
            return Ok(false);
        }
        let at = inner.stamp_above(Some(stored.updated_at));
        let intent = MailCredential {
            revoked_at_unix: Some(at.0 / 1_000_000),
            wrapped_under: None,
            secret: Default::default(),
            updated_at: at,
            ..stored
        };
        let moved = inner.join_credential(intent);
        if moved {
            inner.landed();
        }
        Ok(moved)
    }
}

/// The call-log entry a [`FakeBackupStateStore`] pushes for every read
/// (`backup_state`, `backup_destination_lists`) when it logs
/// ([`FakeBackupStateStore::logging_into`]).
pub const BACKUP_STATE_READ: &str = "fauna.state.backup:read";
/// The call-log entry a [`FakeBackupStateStore`] pushes for every accepted
/// write (a list put or a mark merge).
pub const BACKUP_STATE_WRITE: &str = "fauna.state.backup:write";

/// The encoded `fauna.state.backup` rows a [`FakeBackupStateStore`] holds,
/// key → canonical value bytes.
pub type BackupRows = BTreeMap<String, Vec<u8>>;

/// The shared in-memory [`BackupStateStore`] double — the plane's
/// `fauna.state.backup` rows, kept **encoded** and read through the shipped
/// fold, so the double cannot drift from what the handle answers:
///
/// - a read is [`BackupState::from_rows`] for the named box (its own list
///   row only, every mark of the account, the list pruned of every
///   `Removed` destination) and [`destination_lists`] for the all-boxes read;
/// - a list write refuses a row over its bounds
///   ([`BackupDestinationsRow::check_bounds`]), stamps the row
///   `max(now, stored + 1)` and puts it under [`destinations_key`];
/// - a mark write joins each mark into its own row
///   ([`DestinationUnattestedMark::join`]) under its `plane_key`.
///
/// Knobs: the door's transient refusal ([`Self::refuse_next_writes`]), the
/// store not up yet ([`Self::set_not_ready`]), and a stale read
/// ([`Self::serve_next_read_from`]) — a device whose read predates a
/// sibling's write, the one race a door without CAS still has.
///
/// Cheap to [`Clone`]: every clone is the same store.
#[derive(Clone, Default)]
pub struct FakeBackupStateStore {
    inner: Arc<Mutex<FakeBackupInner>>,
    /// Per clone: where this handle logs its calls.
    log: Option<Arc<Mutex<Vec<&'static str>>>>,
}

#[derive(Default)]
struct FakeBackupInner {
    rows: BackupRows,
    stale_read: Option<BackupRows>,
    refuse: usize,
    not_ready: bool,
    writes: usize,
}

impl FakeBackupStateStore {
    /// A store holding no row yet.
    pub fn empty() -> Self {
        Self::default()
    }

    /// This same store, logging every call into `log` ([`BACKUP_STATE_READ`],
    /// [`BACKUP_STATE_WRITE`]) — the fake nest's call log, so an ordering
    /// assertion sees the nest calls and the store writes in one sequence.
    pub fn logging_into(&self, log: Arc<Mutex<Vec<&'static str>>>) -> Self {
        Self {
            inner: self.inner.clone(),
            log: Some(log),
        }
    }

    fn note(&self, entry: &'static str) {
        if let Some(log) = &self.log {
            log.lock().unwrap().push(entry);
        }
    }

    /// Seed `source_nest`'s list directly, bypassing every knob — a state a
    /// sibling device's write left behind. Not counted as a write.
    pub fn seed_list(&self, source_nest: [u8; 32], destinations: Vec<BackupDestination>) {
        let mut inner = self.inner.lock().unwrap();
        let backup = BackupConfig { destinations };
        put_list(&mut inner.rows, source_nest, backup).expect("seed a list within its bounds");
    }

    /// Seed marks directly (joined into their rows), bypassing every knob.
    pub fn seed_marks(&self, marks: &[DestinationUnattestedMark]) {
        let mut inner = self.inner.lock().unwrap();
        put_marks(&mut inner.rows, marks);
    }

    /// `source_nest`'s state as the fold reads it right now.
    pub fn state(&self, source_nest: [u8; 32]) -> BackupState {
        fold(&self.inner.lock().unwrap().rows, source_nest)
    }

    /// Every mark of the account right now.
    pub fn marks(&self) -> Vec<DestinationUnattestedMark> {
        let inner = self.inner.lock().unwrap();
        destination_marks(inner.rows.iter().map(|(k, v)| (k.as_str(), v.as_slice())))
            .expect("the fake's rows decode")
    }

    /// Every box's list row right now.
    pub fn lists(&self) -> Vec<BackupDestinationsRow> {
        lists(&self.inner.lock().unwrap().rows)
    }

    /// The encoded rows right now — a snapshot a test rewinds to or serves
    /// stale ([`Self::serve_next_read_from`]).
    pub fn snapshot(&self) -> BackupRows {
        self.inner.lock().unwrap().rows.clone()
    }

    /// Answer the next `backup_state` read from `rows` instead of the
    /// current rows: a device whose read landed before a sibling's write
    /// that has since rested. Writes still land on the current rows.
    pub fn serve_next_read_from(&self, rows: BackupRows) {
        self.inner.lock().unwrap().stale_read = Some(rows);
    }

    /// Refuse the next `n` writes the way the writer door refuses a put while
    /// no generation tip resolves.
    pub fn refuse_next_writes(&self, n: usize) {
        self.inner.lock().unwrap().refuse = n;
    }

    /// Answer every call [`crate::LEDGER_NOT_READY`] (or stop).
    pub fn set_not_ready(&self, not_ready: bool) {
        self.inner.lock().unwrap().not_ready = not_ready;
    }

    /// How many writes (list puts and mark merges that moved a row) were
    /// accepted.
    pub fn writes(&self) -> usize {
        self.inner.lock().unwrap().writes
    }
}

fn fold(rows: &BackupRows, source_nest: [u8; 32]) -> BackupState {
    BackupState::from_rows(
        rows.iter().map(|(k, v)| (k.as_str(), v.as_slice())),
        &source_nest,
    )
    .expect("the fake's rows decode")
}

fn lists(rows: &BackupRows) -> Vec<BackupDestinationsRow> {
    destination_lists(rows.iter().map(|(k, v)| (k.as_str(), v.as_slice())))
        .expect("the fake's rows decode")
}

/// The door's list put: bounds first, then the stamp rule. Returns whether a
/// row moved (the echo-stop: an equal list puts nothing).
fn put_list(
    rows: &mut BackupRows,
    source_nest: [u8; 32],
    backup: BackupConfig,
) -> Result<bool, StoreError> {
    let key = destinations_key(&source_nest);
    let stored =
        rows.get(&key).map(
            |v| match decode_backup_row(&key, v).expect("stored row decodes") {
                BackupRecord::Destinations(row) => row,
                BackupRecord::Mark(_) => unreachable!("the key names a list row"),
            },
        );
    if stored.as_ref().is_some_and(|row| row.backup == backup) {
        return Ok(false);
    }
    let now = Timestamp::now();
    let updated_at = match &stored {
        Some(row) => now.max(Timestamp(row.updated_at.0.saturating_add(1))),
        None => now,
    };
    let row = BackupDestinationsRow {
        source_nest,
        backup,
        updated_at,
    };
    row.check_bounds()
        .map_err(|e| StoreError::Save(format!("backup destinations: {e}")))?;
    let value = BackupRecord::Destinations(row)
        .encode()
        .expect("encode a list row");
    rows.insert(key, value);
    Ok(true)
}

/// The door's mark merge: each mark joined into its own row. Returns whether
/// any row moved.
fn put_marks(rows: &mut BackupRows, marks: &[DestinationUnattestedMark]) -> bool {
    let mut moved = false;
    for mark in marks {
        let key = mark.plane_key();
        let joined = match rows.get(&key) {
            Some(v) => match decode_backup_row(&key, v).expect("stored mark decodes") {
                BackupRecord::Mark(stored) => stored.join(mark),
                BackupRecord::Destinations(_) => unreachable!("the key names a mark row"),
            },
            None => mark.clone(),
        };
        let value = BackupRecord::Mark(joined)
            .encode()
            .expect("encode a mark row");
        if rows.get(&key) != Some(&value) {
            rows.insert(key, value);
            moved = true;
        }
    }
    moved
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl BackupStateStore for FakeBackupStateStore {
    async fn backup_state(&self, source_nest: [u8; 32]) -> Result<BackupState, StoreError> {
        self.note(BACKUP_STATE_READ);
        let mut inner = self.inner.lock().unwrap();
        if inner.not_ready {
            return Err(StoreError::Load(crate::LEDGER_NOT_READY.into()));
        }
        Ok(match inner.stale_read.take() {
            Some(stale) => fold(&stale, source_nest),
            None => fold(&inner.rows, source_nest),
        })
    }

    async fn backup_destination_lists(&self) -> Result<Vec<BackupDestinationsRow>, StoreError> {
        self.note(BACKUP_STATE_READ);
        let inner = self.inner.lock().unwrap();
        if inner.not_ready {
            return Err(StoreError::Load(crate::LEDGER_NOT_READY.into()));
        }
        Ok(lists(&inner.rows))
    }

    async fn write_backup_destinations(
        &self,
        source_nest: [u8; 32],
        backup: BackupConfig,
    ) -> Result<BackupState, StoreError> {
        let mut inner = self.inner.lock().unwrap();
        if inner.not_ready {
            return Err(StoreError::Save(crate::LEDGER_NOT_READY.into()));
        }
        if inner.refuse > 0 {
            inner.refuse -= 1;
            return Err(StoreError::Save(
                "fake door: no generation tip resolves yet".into(),
            ));
        }
        if put_list(&mut inner.rows, source_nest, backup)? {
            inner.writes += 1;
            self.note(BACKUP_STATE_WRITE);
        }
        Ok(fold(&inner.rows, source_nest))
    }

    async fn merge_destination_marks(
        &self,
        marks: Vec<DestinationUnattestedMark>,
    ) -> Result<Vec<DestinationUnattestedMark>, StoreError> {
        let mut inner = self.inner.lock().unwrap();
        if inner.not_ready {
            return Err(StoreError::Save(crate::LEDGER_NOT_READY.into()));
        }
        if inner.refuse > 0 {
            inner.refuse -= 1;
            return Err(StoreError::Save(
                "fake door: no generation tip resolves yet".into(),
            ));
        }
        if put_marks(&mut inner.rows, &marks) {
            inner.writes += 1;
            self.note(BACKUP_STATE_WRITE);
        }
        Ok(
            destination_marks(inner.rows.iter().map(|(k, v)| (k.as_str(), v.as_slice())))
                .expect("the fake's rows decode"),
        )
    }
}

/// The shared in-memory [`crate::KindManifestStore`] double: the rows by
/// `client_id`, the overlay folded from them through the real
/// `VerifiedManifest::admit_into`, a publish replacing the row it names
/// (whole-row latest-wins). The one fault it injects is the door's transient
/// no-tip refusal ([`Self::refuse_next_publishes`]).
#[derive(Clone, Default)]
pub struct FakeKindManifestStore {
    inner: Arc<Mutex<FakeKindManifestInner>>,
}

#[derive(Default)]
struct FakeKindManifestInner {
    rows: BTreeMap<String, fauna_protocol::kind_manifest::VerifiedManifest>,
    refuse: usize,
}

impl FakeKindManifestStore {
    /// An account with no manifest row.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Refuse the next `n` publishes as the door's no-tip refusal does.
    pub fn refuse_next_publishes(&self, n: usize) {
        self.inner.lock().unwrap().refuse = n;
    }

    /// The row stored for `client_id`, if any.
    pub fn row(&self, client_id: &str) -> Option<fauna_protocol::kind_manifest::VerifiedManifest> {
        self.inner.lock().unwrap().rows.get(client_id).cloned()
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl crate::KindManifestStore for FakeKindManifestStore {
    async fn admitted_kinds(
        &self,
    ) -> Result<fauna_protocol::merge_policy::AdmittedKinds, StoreError> {
        let mut overlay = fauna_protocol::merge_policy::AdmittedKinds::new();
        for manifest in self.inner.lock().unwrap().rows.values() {
            manifest
                .admit_into(&mut overlay)
                .map_err(|e| StoreError::Load(e.to_string()))?;
        }
        Ok(overlay)
    }

    async fn publish(
        &self,
        client_id: &str,
        manifest: &fauna_protocol::kind_manifest::VerifiedManifest,
        _admitted_at_ms: i64,
    ) -> Result<(), StoreError> {
        let mut inner = self.inner.lock().unwrap();
        if inner.refuse > 0 {
            inner.refuse -= 1;
            return Err(StoreError::Save("no generation tip resolves yet".into()));
        }
        let host = fauna_protocol::kind_manifest::client_id_host(client_id);
        if host.as_deref() != Some(manifest.publisher_domain.as_str()) {
            return Err(StoreError::Save(format!(
                "the manifest was verified for {:?}, not {client_id:?}",
                manifest.publisher_domain
            )));
        }
        inner.rows.insert(client_id.to_string(), manifest.clone());
        Ok(())
    }
}
