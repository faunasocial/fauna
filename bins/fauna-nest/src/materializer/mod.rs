//! The nest's **materializer** — the in-process holder of materialization
//! grants and the worker that keeps a granted actor's readable views of their
//! own mail, calendar and cards (`encryption-at-rest.md` § Capability tiering
//! → *The materialization grant*, parts (b)–(d); class 4 of § Readable
//! classes owns the seven bounds every line here keeps).
//!
//! - [`holder`] — the holder identity: seed, keys, the self-approved
//!   enrollment row, the grant registry;
//! - [`area`] — the per-actor `__materialized/<actor_hex>/<kind>/` views
//!   outside `nest.db` and every self-backup, one per [`MatKind`];
//! - [`worker`] — the per-record open, doc shape and snippet.
//!
//! **One view per (actor, kind), each bound to its own grant.** The user
//! chooses per kind what the nest keeps readable, so each kind's view rides
//! the grant that declared its `materialize{kind}` tuple and dies with it: a
//! calendar revoke deletes the calendar view and leaves mail's standing.
//!
//! **Cadence** — event-driven with a resumable catch-up, every constant a
//! Rust constant: *arm* (the first time a standing materialization grant is
//! seen for an (actor, kind) — a mint, or boot with one standing — the view is
//! created, bound to the grant id, and the catch-up walk starts, oldest first,
//! [`worker::CATCH_UP_BATCH`] records a batch, yielding between batches);
//! *trickle* (the content append seams' observer,
//! [`crate::db::ContentAppendHook`], fired by every mail, calendar and card
//! segment insert, queues a wake; the worker indexes what changed, debounced
//! by [`WAKE_DEBOUNCE`]); *retire* (mail: a change in the live mail count
//! sends the view through a sweep that deletes every doc whose record is gone;
//! a DAV kind: every walk diffs the whole corpus — `uid_hash` → `record_cid`
//! over every collection — so an edit replaces its doc and a delete drops
//! it); *revoke and lapse* (the revoke handler awaits
//! [`Materializer::on_grant_revoked`], which deletes the view before the
//! reply; the [`RECONCILE_PERIOD`] tick treats a lapsed window exactly as a
//! vanished grant, and wakes every view, so a delete no insert announced is
//! retired within it); *boot reconcile* (before the nest serves,
//! [`Materializer::init`] deletes every view whose bound grant no longer
//! stands — revoked, lapsed, the holder row revoked, the actor gone — and an
//! area of the version-1 layout whole).
//!
//! A record the held wraps cannot open is skipped and counted
//! ([`area::Area::skipped`]); the next renew of the grant (the top-up,
//! part (e)) restarts the walk so it is retried.

pub mod area;
pub mod holder;
pub mod worker;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use fauna_index::TimeRange;
use fauna_mls::wrapped_blob::ScopeTuple;
use fauna_mls::wrapped_blob::format::GrantBlob;
use fauna_segment_store::SegmentManager;
use tokio::sync::{Mutex, mpsc};

use crate::db::CacheDb;
use crate::db::bridge_service_users::BridgeStatus;
use area::Area;
pub use area::MatKind;
use holder::MaterializerHolder;

/// How long the worker gathers wakes before it walks — the trickle's flush
/// debounce.
pub const WAKE_DEBOUNCE: Duration = Duration::from_millis(500);

/// The reconcile tick: grant lapse, an admin's revocation of the holder row,
/// and retire sweeps for deletes no append announced all bite within it.
pub const RECONCILE_PERIOD: Duration = Duration::from_secs(60);

type Slot = Arc<Mutex<Option<Area>>>;
type ViewKey = ([u8; 32], MatKind);

/// The segment stores the materializer reads canon from — one per kind.
#[derive(Clone)]
pub struct ContentStores {
    /// The `__mail` store.
    pub mail: Arc<SegmentManager>,
    /// The `__calendar` store.
    pub calendar: Arc<SegmentManager>,
    /// The `__card` store.
    pub card: Arc<SegmentManager>,
}

/// One hit of an actor's readable view, for the search merge.
#[derive(Debug, Clone)]
pub struct MaterializedHit {
    /// The view it came from — the hit's `content_type` is
    /// [`MatKind::content_type`].
    pub kind: MatKind,
    /// The doc's content id: the RFC `Message-ID` for mail, the hex
    /// `uid_hash` for an event or a card.
    pub content_id: String,
    /// Mail: the nest message id (the record digest) — the local doc's
    /// `secondary_id`. `None` for a DAV kind, whose local doc carries none.
    pub nest_message_id: Option<Vec<u8>>,
    /// Epoch micros.
    pub created_at: i64,
    /// BM25 score (higher is more relevant).
    pub score: f32,
    /// Rendered at query time from the sealed record.
    pub snippet: String,
}

/// The materializer: holder, views, worker.
pub struct Materializer {
    holder: MaterializerHolder,
    data_dir: PathBuf,
    db: Arc<CacheDb>,
    stores: ContentStores,
    slots: std::sync::Mutex<HashMap<ViewKey, Slot>>,
    wake_tx: mpsc::UnboundedSender<[u8; 32]>,
    wake_rx: std::sync::Mutex<Option<mpsc::UnboundedReceiver<[u8; 32]>>>,
}

/// A grant blob's materialization verdict: `(owner, grant_id, kinds)` when it
/// is a well-formed materialization grant whose window holds `now` — `kinds`
/// every own-actor kind it declares a `materialize` tuple for.
fn standing_materialization(blob: &[u8], now: u64) -> Option<([u8; 32], Vec<u8>, Vec<MatKind>)> {
    let blob = GrantBlob::from_canonical_bytes(blob).ok()?;
    let kinds: Vec<MatKind> = blob
        .scope
        .iter()
        .filter(|t| t.is_materialize())
        .filter_map(|t| MatKind::from_scope_kind(t.kind.as_deref()?))
        .collect();
    if kinds.is_empty()
        || fauna_mls::wrapped_blob::materialization_shape_refusal(&blob.scope, &blob.wrapped_keys)
            .is_some()
        // The one authorizing window check (both bounds, fail closed): a clock
        // past i64 converts to -1, which opens no window.
        || !fauna_mls::wrapped_blob::grant_window_is_open(&blob, i64::try_from(now).unwrap_or(-1))
    {
        return None;
    }
    let owner: [u8; 32] = blob.index.0.as_slice().try_into().ok()?;
    Some((owner, blob.index.1.clone(), kinds))
}

fn now_secs() -> u64 {
    u64::try_from(crate::db::now_epoch_secs()).unwrap_or(0)
}

impl Materializer {
    /// Bring the materializer up and run the **boot reconcile** before the
    /// nest serves: an area of the version-1 layout is deleted whole, and
    /// every view whose bound grant no longer stands is deleted. `Ok(None)`
    /// when an admin revoked the holder row — every area is then deleted too,
    /// and nothing is kept.
    pub async fn init(
        data_dir: &Path,
        db: Arc<CacheDb>,
        stores: ContentStores,
    ) -> Result<Option<Arc<Self>>> {
        let Some(holder) = MaterializerHolder::init(data_dir, db.clone()).await? else {
            area::destroy_all(data_dir)?;
            return Ok(None);
        };
        // The version-1 layout (one mail view at the actor's root) is derived
        // state like every view: deleted and rebuilt from canon, never
        // migrated.
        for actor in area::list_actors(data_dir) {
            if area::is_v1_layout(data_dir, &actor) {
                area::destroy_actor(data_dir, &actor)?;
            }
        }
        let (wake_tx, wake_rx) = mpsc::unbounded_channel();
        let this = Arc::new(Self {
            holder,
            data_dir: data_dir.to_path_buf(),
            db: db.clone(),
            stores,
            slots: std::sync::Mutex::new(HashMap::new()),
            wake_tx,
            wake_rx: std::sync::Mutex::new(Some(wake_rx)),
        });
        this.reconcile().await;
        // A record skipped before the restart is retried once after it.
        for (actor, _) in this.armed() {
            this.restart_skipped_walks(&actor).await;
        }
        let tx = this.wake_tx.clone();
        db.set_content_append_hook(Arc::new(move |actor: &[u8; 32]| {
            let _ = tx.send(*actor);
        }));
        Ok(Some(this))
    }

    /// The X25519 key grants to this materializer are sealed to — the one
    /// holder a `materialize` tuple may name (the mint door's check).
    pub fn holder_x25519(&self) -> &[u8; 32] {
        &self.holder.x25519_pubkey
    }

    /// The holder's published ML-KEM-768 encapsulation key (X-Wing mints).
    pub fn holder_mlkem_ek(&self) -> &[u8] {
        &self.holder.mlkem_ek
    }

    /// The data dir the areas live under.
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    fn slot(&self, key: ViewKey) -> Slot {
        self.slots
            .lock()
            .expect("materializer slots lock poisoned")
            .entry(key)
            .or_default()
            .clone()
    }

    fn armed_slot(&self, key: &ViewKey) -> Option<Slot> {
        self.slots
            .lock()
            .expect("materializer slots lock poisoned")
            .get(key)
            .cloned()
    }

    fn armed(&self) -> Vec<ViewKey> {
        self.slots
            .lock()
            .expect("materializer slots lock poisoned")
            .keys()
            .copied()
            .collect()
    }

    /// The standing materialization grants, (actor, kind) → grant id, or
    /// `None` on a transient read failure (nothing is deleted on a
    /// transient). Empty when the holder row is no longer approved (an admin
    /// revoked it at runtime).
    async fn standing(&self) -> Option<HashMap<ViewKey, Vec<u8>>> {
        match self
            .db
            .lookup_bridge_service_user(&self.holder.ed25519_pubkey())
            .await
        {
            Ok(Some(row)) if row.status == BridgeStatus::Approved => {}
            Ok(_) => return Some(HashMap::new()),
            Err(e) => {
                tracing::warn!(target: "materializer", "holder row read failed: {e:#}");
                return None;
            }
        }
        let now = crate::db::now_epoch_secs();
        let blobs = match self
            .db
            .fetch_capability_grants_for_holder(&self.holder.x25519_pubkey, now)
            .await
        {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(target: "materializer", "grant read failed: {e:#}");
                return None;
            }
        };
        let now = u64::try_from(now).unwrap_or(0);
        let mut out: HashMap<ViewKey, Vec<u8>> = HashMap::new();
        for blob in &blobs {
            let Some((owner, grant_id, kinds)) = standing_materialization(blob, now) else {
                continue;
            };
            // An actor with no account here is "the actor gone".
            if !matches!(self.db.get_user(&owner).await, Ok(Some(_))) {
                continue;
            }
            for kind in kinds {
                // Two standing grants of one kind: bind the lowest id,
                // deterministically.
                out.entry((owner, kind))
                    .and_modify(|g| {
                        if grant_id < *g {
                            *g = grant_id.clone();
                        }
                    })
                    .or_insert_with(|| grant_id.clone());
            }
        }
        Some(out)
    }

    /// Reconcile every view against the standing grants: delete the unbound,
    /// arm the newly granted, wake every armed actor (the tick's retire
    /// probe). The registry is refreshed first so the worker wields the
    /// current keys.
    pub async fn reconcile(&self) {
        if let Err(e) = self.holder.registry.refresh().await {
            tracing::warn!(target: "materializer", %e, "grant refresh failed");
        }
        let Some(standing) = self.standing().await else {
            return;
        };
        let mut on_disk: HashSet<ViewKey> = area::list_areas(&self.data_dir).into_iter().collect();
        on_disk.extend(self.armed());
        for key in on_disk {
            let bound = standing.get(&key);
            if bound.is_none() || area::bound_grant(&self.data_dir, &key.0, key.1).as_ref() != bound
            {
                self.delete_view(key).await;
            }
        }
        let mut woken: HashSet<[u8; 32]> = HashSet::new();
        for (key, grant_id) in &standing {
            let slot = self.slot(*key);
            let mut guard = slot.lock().await;
            let rebind = guard
                .as_ref()
                .is_none_or(|a| a.grant_id() != grant_id.as_slice());
            if rebind {
                match Area::open_bound(&self.data_dir, &key.0, key.1, grant_id) {
                    Ok(a) => *guard = Some(a),
                    Err(e) => {
                        tracing::warn!(target: "materializer", "arm failed: {e:#}");
                        continue;
                    }
                }
            }
            drop(guard);
            if woken.insert(key.0) {
                let _ = self.wake_tx.send(key.0);
            }
        }
    }

    async fn delete_view(&self, key: ViewKey) {
        let slot = self.slot(key);
        let mut guard = slot.lock().await;
        *guard = None;
        if let Err(e) = area::destroy(&self.data_dir, &key.0, key.1) {
            tracing::warn!(target: "materializer", "view delete failed: {e:#}");
        }
        drop(guard);
        self.slots
            .lock()
            .expect("materializer slots lock poisoned")
            .remove(&key);
    }

    /// Restart every walk of `actor`'s that skipped a record, so a wrap a
    /// renew added (or a restart) retries it.
    async fn restart_skipped_walks(&self, actor: &[u8; 32]) {
        for kind in MatKind::ALL {
            let Some(slot) = self.armed_slot(&(*actor, kind)) else {
                continue;
            };
            if let Some(area) = slot.lock().await.as_mut()
                && area.skipped > 0
            {
                area.cursor = 0;
                area.skipped = 0;
                area.unopened.clear();
            }
        }
    }

    /// A grant of `actor`'s was minted or renewed: refresh, arm, and — when a
    /// renew topped the grant up with a wrap the walk lacked — restart the
    /// walk so every skipped record is retried.
    pub async fn on_grant_changed(&self, actor: &[u8; 32]) {
        self.reconcile().await;
        self.restart_skipped_walks(actor).await;
        let _ = self.wake_tx.send(*actor);
    }

    /// A grant of `actor`'s was revoked: every view of theirs the revoke
    /// unbound is deleted before this returns (and so before the revoke
    /// replies); a view another materialization grant of theirs still binds
    /// stays.
    pub async fn on_grant_revoked(&self, actor: &[u8; 32]) {
        if let Err(e) = self.holder.registry.refresh().await {
            tracing::warn!(target: "materializer", %e, "grant refresh failed");
        }
        // Delete first whatever the standing read says: a transient failure
        // must not leave a revoked view on disk. A grant that still stands
        // re-arms below and the walk rebuilds it from canon.
        let standing = self.standing().await;
        let mut any_still = false;
        for kind in MatKind::ALL {
            let key = (*actor, kind);
            let still = standing.as_ref().and_then(|s| s.get(&key).cloned());
            let bound = area::bound_grant(&self.data_dir, actor, kind);
            if still.is_none() || bound != still {
                self.delete_view(key).await;
            }
            any_still |= still.is_some();
        }
        if any_still {
            self.reconcile().await;
        }
    }

    /// Account deletion purges the actor's whole area with their tables.
    pub async fn purge_actor(&self, actor: &[u8; 32]) {
        for kind in MatKind::ALL {
            self.delete_view((*actor, kind)).await;
        }
        if let Err(e) = area::destroy_actor(&self.data_dir, actor) {
            tracing::warn!(target: "materializer", "area delete failed: {e:#}");
        }
    }

    /// Spawn the worker: the wake loop (trickle + catch-up) and the reconcile
    /// tick. Call once.
    pub async fn run(self: Arc<Self>) {
        let Some(mut rx) = self
            .wake_rx
            .lock()
            .expect("materializer wake lock poisoned")
            .take()
        else {
            return;
        };
        let mut tick = tokio::time::interval(RECONCILE_PERIOD);
        tick.tick().await;
        loop {
            tokio::select! {
                first = rx.recv() => {
                    let Some(first) = first else { return };
                    let mut due: HashSet<[u8; 32]> = HashSet::from([first]);
                    tokio::time::sleep(WAKE_DEBOUNCE).await;
                    while let Ok(a) = rx.try_recv() {
                        due.insert(a);
                    }
                    for actor in due {
                        if let Err(e) = self.process(&actor).await {
                            tracing::warn!(target: "materializer", "walk failed: {e:#}");
                        }
                    }
                }
                _ = tick.tick() => self.reconcile().await,
            }
        }
    }

    /// Walk every armed view of `actor`'s, each to its canon's current state,
    /// then retire what canon no longer holds. A no-op for an actor with no
    /// armed view. Public for the tests, which drive the walk without the
    /// wake loop.
    pub async fn process(&self, actor: &[u8; 32]) -> Result<()> {
        for kind in MatKind::ALL {
            let Some(slot) = self.armed_slot(&(*actor, kind)) else {
                continue;
            };
            match kind {
                MatKind::Mail => self.process_mail(&slot, actor).await?,
                MatKind::Calendar | MatKind::Card => self.process_dav(&slot, actor, kind).await?,
            }
        }
        Ok(())
    }

    /// Walk `actor`'s mail past the view's cursor, then run the retire probe.
    async fn process_mail(&self, slot: &Slot, actor: &[u8; 32]) -> Result<()> {
        loop {
            let mut guard = slot.lock().await;
            let Some(area) = guard.as_mut() else {
                return Ok(());
            };
            let set = self.holder.registry.current();
            let Some(key) = set.key_for(
                actor,
                ScopeTuple::CLASS_CONTENT_READ,
                ScopeTuple::KIND_MAIL,
                None,
                now_secs(),
            ) else {
                // Dark: the registry holds no key yet (a transient fetch). The
                // cursor stays put; the next tick retries.
                return Ok(());
            };
            let rows = self
                .db
                .segment_records_list_mail_heads_after_seq(
                    actor,
                    area.cursor,
                    worker::CATCH_UP_BATCH,
                )
                .await?;
            let full = rows.len() as i64 == worker::CATCH_UP_BATCH;
            for (seq, cid) in rows {
                let digest = cid.digest();
                match crate::segments::mail::read_sealed_body_with_floor(
                    &self.stores.mail,
                    &self.db,
                    actor,
                    &digest,
                )
                .await
                {
                    Ok(Some((body, _hint, floor))) => match worker::open_mail_body(&body, key) {
                        Some(pt) => {
                            if let Some(doc) = worker::mail_doc(&pt, &digest, floor.received_at) {
                                area.add_doc(doc)?;
                            }
                        }
                        None => area.skipped += 1,
                    },
                    // Gone between the list and the read (a racing tombstone),
                    // or a continuation whose part is still in flight: the
                    // retire sweep / a later walk settles it.
                    Ok(None) => {}
                    Err(e) => {
                        tracing::debug!(target: "materializer", "record read failed: {e:#}");
                        area.skipped += 1;
                    }
                }
                area.cursor = seq;
            }
            area.commit()?;
            area.flush()?;
            if !full {
                break;
            }
            drop(guard);
            tokio::task::yield_now().await;
        }
        self.retire_mail(slot, actor).await
    }

    /// The mail retire probe: when the live mail count moved since the last
    /// sweep, delete every doc whose record no longer lives.
    async fn retire_mail(&self, slot: &Slot, actor: &[u8; 32]) -> Result<()> {
        let live = self.db.segment_records_count_live_mail_heads(actor).await?;
        let mut guard = slot.lock().await;
        let Some(area) = guard.as_mut() else {
            return Ok(());
        };
        if area.live_at_last_sweep == Some(live) {
            return Ok(());
        }
        let alive: HashSet<[u8; 32]> = self
            .db
            .segment_records_list_mail_heads_after_seq(actor, 0, i64::MAX)
            .await?
            .into_iter()
            .map(|(_, cid)| cid.digest())
            .collect();
        let dead: Vec<Vec<u8>> = area
            .ids()
            .filter(|(_, nest)| {
                <[u8; 32]>::try_from(nest.as_slice()).is_ok_and(|d| !alive.contains(&d))
            })
            .map(|(cid, _)| cid.clone())
            .collect();
        for content_id in &dead {
            area.delete_doc(content_id);
        }
        area.live_at_last_sweep = Some(live);
        if !dead.is_empty() {
            area.commit()?;
            area.flush()?;
        }
        Ok(())
    }

    /// Read and open one DAV record of `kind` under `key`: `Ok(Some(Some(pt,
    /// created_at)))` opened, `Ok(Some(None))` the key does not open it,
    /// `Ok(None)` gone since the corpus read.
    async fn open_dav_record(
        &self,
        actor: &[u8; 32],
        kind: MatKind,
        cid: &fauna_cbor::Cid,
        key: &[u8],
    ) -> Result<Option<Option<(Vec<u8>, i64)>>> {
        let sealed = match kind {
            MatKind::Calendar => {
                crate::segments::cal::read_record(&self.stores.calendar, &self.db, actor, cid)
                    .await?
                    .map(|(env, floor)| (env.encrypted_body, floor.created_at))
            }
            MatKind::Card => {
                crate::segments::card::read_record(&self.stores.card, &self.db, actor, cid)
                    .await?
                    .map(|(env, floor)| (env.encrypted_body, floor.created_at))
            }
            MatKind::Mail => return Ok(None),
        };
        Ok(sealed.map(|(body, created_at)| {
            worker::open_dav_body(kind.record_kind(), &body, key).map(|pt| (pt, created_at))
        }))
    }

    /// Walk `actor`'s calendar or card corpus: every record the view does not
    /// already hold at its current `record_cid` is opened and indexed (an
    /// edit replaces its doc), in batches of [`worker::CATCH_UP_BATCH`]; then
    /// every doc whose `uid_hash` left the corpus is deleted.
    async fn process_dav(&self, slot: &Slot, actor: &[u8; 32], kind: MatKind) -> Result<()> {
        let set = self.holder.registry.current();
        let Some(key) = set.key_for(
            actor,
            ScopeTuple::CLASS_CONTENT_READ,
            kind.scope_kind(),
            None,
            now_secs(),
        ) else {
            // Dark: no key yet (a transient fetch); the next tick retries.
            return Ok(());
        };
        let corpus = self
            .db
            .materializer_dav_corpus(actor, kind == MatKind::Calendar)
            .await?;
        // One doc per UID, as the client's own corpus keys it: a UID in two
        // collections is one entry (the lowest record id, deterministically).
        let mut live: BTreeMap<Vec<u8>, ([u8; 32], fauna_cbor::Cid)> = BTreeMap::new();
        for (uid, cid) in corpus {
            let content_id = worker::dav_content_id(&uid).into_bytes();
            live.entry(content_id)
                .and_modify(|e| {
                    if cid.as_bytes() < e.1.as_bytes() {
                        *e = (uid, cid);
                    }
                })
                .or_insert((uid, cid));
        }
        let pending: Vec<([u8; 32], fauna_cbor::Cid)> = {
            let guard = slot.lock().await;
            let Some(area) = guard.as_ref() else {
                return Ok(());
            };
            live.iter()
                .filter(|(content_id, (_, cid))| {
                    area.nest_id_of(content_id) != Some(&cid.as_bytes()[..])
                        && !area.unopened.contains(&cid.as_bytes()[..])
                })
                .map(|(_, v)| *v)
                .collect()
        };
        for batch in pending.chunks(usize::try_from(worker::CATCH_UP_BATCH).unwrap_or(200)) {
            let mut docs = Vec::with_capacity(batch.len());
            let mut unopened = Vec::new();
            for (uid, cid) in batch {
                match self.open_dav_record(actor, kind, cid, key).await {
                    Ok(Some(Some((pt, created_at)))) => {
                        let doc = match kind {
                            MatKind::Calendar => {
                                worker::event_doc(&pt, uid, &cid.as_bytes()[..], created_at)
                            }
                            _ => worker::card_doc(&pt, uid, &cid.as_bytes()[..], created_at),
                        };
                        docs.extend(doc);
                    }
                    Ok(Some(None)) => unopened.push(cid.as_bytes().to_vec()),
                    // Gone since the corpus read: the next walk settles it.
                    Ok(None) => {}
                    Err(e) => {
                        tracing::debug!(target: "materializer", "record read failed: {e:#}");
                        unopened.push(cid.as_bytes().to_vec());
                    }
                }
            }
            let mut guard = slot.lock().await;
            let Some(area) = guard.as_mut() else {
                return Ok(());
            };
            for doc in docs {
                area.add_doc(doc)?;
            }
            for cid in unopened {
                if area.unopened.insert(cid) {
                    area.skipped += 1;
                }
            }
            area.commit()?;
            area.flush()?;
            drop(guard);
            tokio::task::yield_now().await;
        }
        let mut guard = slot.lock().await;
        let Some(area) = guard.as_mut() else {
            return Ok(());
        };
        let dead: Vec<Vec<u8>> = area
            .ids()
            .filter(|(content_id, _)| !live.contains_key(*content_id))
            .map(|(content_id, _)| content_id.clone())
            .collect();
        for content_id in &dead {
            area.delete_doc(content_id);
        }
        if !dead.is_empty() {
            area.commit()?;
            area.flush()?;
        }
        Ok(())
    }

    /// The query-time snippet of one hit, from its sealed record opened
    /// under the held key of its kind; empty when it cannot be opened.
    async fn snippet(
        &self,
        actor: &[u8; 32],
        kind: MatKind,
        nest_id: &[u8],
        key: &[u8],
        query: &str,
    ) -> String {
        match kind {
            MatKind::Mail => {
                if let Ok(Some((body, _, _))) = crate::segments::mail::read_sealed_body_with_floor(
                    &self.stores.mail,
                    &self.db,
                    actor,
                    nest_id,
                )
                .await
                    && let Some(pt) = worker::open_mail_body(&body, key)
                {
                    return worker::mail_snippet(&pt, query);
                }
                String::new()
            }
            MatKind::Calendar | MatKind::Card => {
                let Some(cid) = <[u8; 36]>::try_from(nest_id)
                    .ok()
                    .and_then(|b| fauna_cbor::Cid::from_bytes(b).ok())
                else {
                    return String::new();
                };
                match self.open_dav_record(actor, kind, &cid, key).await {
                    Ok(Some(Some((pt, _)))) if kind == MatKind::Calendar => {
                        worker::event_snippet(&pt, query)
                    }
                    Ok(Some(Some((pt, _)))) => worker::card_snippet(&pt, query),
                    _ => String::new(),
                }
            }
        }
    }

    /// Query `actor`'s readable views of `kinds` — `None` when no
    /// materialization grant of theirs for any of them stands here (the
    /// caller then answers from the floor corpus alone). Never for another
    /// actor: the caller passes the connection actor, which is the ACL.
    /// Snippets are rendered from the sealed records opened under the held
    /// key of each kind, and each kind is asked for at most `limit` hits.
    pub async fn search(
        &self,
        actor: &[u8; 32],
        kinds: &[MatKind],
        query: &str,
        range: Option<TimeRange>,
        limit: usize,
    ) -> Option<Vec<MaterializedHit>> {
        let mut out = Vec::new();
        let mut any = false;
        for &kind in kinds {
            let Some(slot) = self.armed_slot(&(*actor, kind)) else {
                continue;
            };
            let hits: Vec<(String, Option<Vec<u8>>, i64, f32)> = {
                let guard = slot.lock().await;
                let Some(area) = guard.as_ref() else {
                    continue;
                };
                any = true;
                match area
                    .index()
                    .query(query, &[kind.content_kind()], range, limit)
                {
                    Ok(hits) => hits
                        .into_iter()
                        .map(|h| {
                            let nest = area.nest_id_of(&h.content_id.0).map(<[u8]>::to_vec);
                            (
                                String::from_utf8_lossy(&h.content_id.0).into_owned(),
                                nest,
                                h.timestamp_ns / 1_000,
                                h.score,
                            )
                        })
                        .collect(),
                    Err(e) => {
                        tracing::debug!(target: "materializer", "readable index query failed: {e}");
                        Vec::new()
                    }
                }
            };
            let set = self.holder.registry.current();
            let key = set.key_for(
                actor,
                ScopeTuple::CLASS_CONTENT_READ,
                kind.scope_kind(),
                None,
                now_secs(),
            );
            for (content_id, nest, created_at, score) in hits {
                let snippet = match (key, nest.as_deref()) {
                    (Some(key), Some(nest)) => self.snippet(actor, kind, nest, key, query).await,
                    _ => String::new(),
                };
                out.push(MaterializedHit {
                    kind,
                    content_id,
                    // The local mail doc's `secondary_id`; a DAV doc's local
                    // twin carries none, so the hit carries none either.
                    nest_message_id: nest.filter(|_| kind == MatKind::Mail),
                    created_at,
                    score,
                    snippet,
                });
            }
        }
        any.then_some(out)
    }
}
