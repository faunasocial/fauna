//! The in-memory physical backend — a **test double, a wasm-compilation
//! proof, and a throwaway replica; never a shipped one** (charter:
//! `account-client-lifecycle.md` ruling (3)). It has three jobs: to grade
//! [`crate::conformance`] itself (a suite that one arm passes might be testing
//! that arm's accidents; two independent arms passing it is evidence the suite
//! states the trait), to give target-agnostic tests above this crate a store
//! with no medium to stand up, and — its production uses — to hold the
//! throwaway replicas: the box-recovery **cold read**'s (`nest/box-recovery.md`
//! § The plane-era recovery floor, *(b)*), which walks a nest's fleet scope
//! into memory, folds one kind and drops the whole store, and the capability
//! host's fleet-scope read (`on-demand-files.md` § Shared sets on a capability
//! host, decision 1′), which holds the same replica in memory for the host
//! process's life, keyed by the machine principal's wraps, and publishes
//! nothing. Both are `fauna_account_plane::cold_replica::ColdFleetReplica`,
//! one per key source. It is ungated for those reads, on native and wasm alike; a replica
//! an account runtime keeps is never this one (the SQLite and IndexedDB arms
//! are the replicas).
//!
//! **One medium, many handles.** A [`MemoryBackend`] is a handle on a shared
//! table set; [`MemoryBackend::handle`] opens a second handle on the SAME
//! tables — the "second process" / "reopen after restart" the SQLite arm gets
//! from opening one directory twice, so the persistence cases of the
//! conformance suite run here unchanged.
//!
//! **Transactions are the lock.** Every trait method takes the medium's one
//! mutex for its whole body and never awaits inside it, so each method is one
//! serializable transaction by construction — the [`StoreBackend`] contract's
//! "dumb and atomic", with no rollback machinery: a method that can refuse
//! decides before it writes.
//!
//! **Fidelity over convenience.** Where the SQLite arm's behaviour has become
//! contract (a `u64` above `i64::MAX` refused rather than wrapped; the
//! rising-only meta compare read as a number; `drop_scope` leaving the group
//! plane alone), this arm reproduces it, because the conformance suite grades
//! all arms against one statement.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::{Context, Result, bail};

use fauna_core::data::ContentHash;

use crate::backend::{
    ReadSeek, RelayEvicted, RelayScopeMeter, RetiredCompaction, ScopeDropCounts, SegmentScopeMeter,
    SegmentStaging, StoreBackend,
};
use crate::physical::{ascii_u64, entry_moved, fits_i64, nest_watermark_key, rising_meta_integer};
use crate::segments::{AdoptedBlock, SegmentHalf, SegmentKey, SegmentSink};

/// The memory arm's [`SegmentStaging`] slot. This medium's "segment area" is
/// memory, so a staged pair rests in two buffers and adoption moves them in —
/// the arm's contract is the logical one (stage, read back, adopt or drop),
/// never the native arm's bounded-memory property, which a medium that IS
/// memory cannot have. A slot is never leftover: dropping it frees it.
#[derive(Default)]
pub struct MemoryStaging {
    dat: Vec<u8>,
    meta: Vec<u8>,
}

impl SegmentSink for MemoryStaging {
    async fn write(&mut self, half: SegmentHalf, chunk: &[u8]) -> Result<()> {
        match half {
            SegmentHalf::Dat => self.dat.extend_from_slice(chunk),
            SegmentHalf::Meta => self.meta.extend_from_slice(chunk),
        }
        Ok(())
    }
}

impl SegmentStaging for MemoryStaging {
    fn len(&self, half: SegmentHalf) -> u64 {
        match half {
            SegmentHalf::Dat => self.dat.len() as u64,
            SegmentHalf::Meta => self.meta.len() as u64,
        }
    }

    async fn meta(&mut self) -> Result<Vec<u8>> {
        Ok(self.meta.clone())
    }

    async fn dat_reader(&mut self) -> Result<Box<dyn ReadSeek + '_>> {
        Ok(Box::new(std::io::Cursor::new(self.dat.as_slice())))
    }

    #[cfg(any(test, feature = "test-helpers"))]
    async fn abandon_as_crash(self) {}
}
use crate::types::{
    InsertOutcome, IntentDrainer, IntentStatus, IssuedRetire, ItemRef, JournalOp, JournalRow,
    NewOutboxIntent, OutboxIntent, RecordIndexEntry, RelayRow, StateEntry, WriterId,
};

/// A journal slot's occupant: `(op, encoded item)`, compared bytewise exactly
/// as the SQLite arm compares its `item_ref` column.
type JournalKey = (String, [u8; 32], u64);

#[derive(Debug, Clone)]
struct RelayStored {
    item_class: String,
    writer_seq: u64,
    op: String,
    entry: Option<Vec<u8>>,
    feed_seq: Option<u64>,
    generation: Option<[u8; 32]>,
}

#[derive(Debug, Clone)]
struct SegmentStored {
    /// `None` once custody eviction dropped the `.dat` — the row and its
    /// `.meta` stay (the SQLite arm's `dat_len IS NULL`).
    dat: Option<Vec<u8>>,
    meta: Vec<u8>,
}

#[derive(Debug, Clone)]
struct OutboxStored {
    kind: String,
    scope: String,
    payload: Vec<u8>,
    drainer: IntentDrainer,
    channel_seq: u64,
    status: IntentStatus,
    retry_count: u32,
    created_at: u64,
    last_attempt_at: Option<u64>,
}

/// The medium: every table the trait names, keyed exactly as the SQLite arm's
/// primary keys so iteration order is the index order its queries sort by.
#[derive(Debug, Default)]
struct Tables {
    meta: BTreeMap<String, Vec<u8>>,
    journal: BTreeMap<JournalKey, (JournalOp, Vec<u8>)>,
    state: BTreeMap<(String, String), StateEntry>,
    group: BTreeMap<(String, String, String), StateEntry>,
    frontiers: BTreeMap<(String, [u8; 32]), u64>,
    record_index: BTreeMap<[u8; 36], RecordIndexEntry>,
    blocks: BTreeMap<[u8; 36], Vec<u8>>,
    segments: BTreeMap<(String, String, u32), SegmentStored>,
    /// `(cid, scope, kind, segment_id) → len` — the routing mirror.
    segment_blocks: BTreeMap<([u8; 36], String, String, u32), u64>,
    relay: BTreeMap<(String, [u8; 32], Vec<u8>), RelayStored>,
    outbox: BTreeMap<[u8; 16], OutboxStored>,
    /// The retire record, keyed by its order token (the SQLite arm's `ord`).
    issued_retires: BTreeMap<u64, IssuedRetire>,
}

/// A handle on one in-memory store medium (see the module doc).
#[derive(Debug, Clone, Default)]
pub struct MemoryBackend {
    tables: Arc<Mutex<Tables>>,
}

impl MemoryBackend {
    /// A handle on a fresh, empty medium.
    pub fn new() -> Self {
        Self::default()
    }

    /// A second handle on THIS medium — what reopening the same store (or a
    /// second process opening it) is for a medium with no name to reopen by.
    pub fn handle(&self) -> Self {
        Self {
            tables: Arc::clone(&self.tables),
        }
    }

    /// The transaction: the medium's one lock, held for a method's body.
    fn tx(&self) -> Result<MutexGuard<'_, Tables>> {
        self.tables
            .lock()
            .map_err(|_| anyhow::anyhow!("memory store medium poisoned by a panicking writer"))
    }
}

fn now_epoch_secs() -> u64 {
    u64::try_from(fauna_core::data::Timestamp::now_secs_or_zero()).unwrap_or(0)
}

impl Tables {
    /// The append-time writer guard — `SqliteBackend::writer_guard_sync`'s
    /// contract, read inside the caller's transaction (the held lock).
    fn writer_guard(&self, local_writer: Option<&WriterId>) -> Result<()> {
        let Some(held) = local_writer else {
            return Ok(());
        };
        match self.meta.get(crate::store::META_WRITER_ID) {
            None => Ok(()),
            Some(v) if v.as_slice() == held.0 => Ok(()),
            Some(v) => {
                let current: [u8; 32] = v
                    .as_slice()
                    .try_into()
                    .context("writer guard: stored writer identity is not 32 bytes")?;
                Err(crate::store::StaleWriter {
                    held: *held,
                    current: WriterId(current),
                }
                .into())
            }
        }
    }

    fn insert_row(&mut self, row: &JournalRow) -> Result<InsertOutcome> {
        fits_i64(row.seq, "writer_seq")?;
        let key = (row.scope.clone(), row.writer.0, row.seq);
        let item = row.item.encode();
        if let Some((op, held)) = self.journal.get(&key) {
            return Ok(if *op == row.op && *held == item {
                InsertOutcome::IdenticalPresent
            } else {
                InsertOutcome::OccupiedByDifferent
            });
        }
        self.journal.insert(key, (row.op, item));
        Ok(InsertOutcome::Inserted)
    }

    fn meta_put_max(&mut self, key: &str, value: u64) -> Result<()> {
        fits_i64(value, "rising-only meta value")?;
        let rises = self
            .meta
            .get(key)
            .is_none_or(|held| rising_meta_integer(held) < value);
        if rises {
            self.meta
                .insert(key.to_owned(), value.to_string().into_bytes());
        }
        Ok(())
    }

    fn record_index_put(&mut self, entry: &RecordIndexEntry) -> Result<()> {
        if let Some(size) = entry.size {
            fits_i64(size, "record size")?;
        }
        let cid = *entry.cid.as_bytes();
        let size = entry
            .size
            .or_else(|| self.record_index.get(&cid).and_then(|held| held.size));
        self.record_index.insert(
            cid,
            RecordIndexEntry {
                size,
                ..entry.clone()
            },
        );
        Ok(())
    }

    fn block_put(&mut self, cid: &ContentHash, bytes: &[u8]) {
        self.blocks
            .entry(*cid.as_bytes())
            .or_insert_with(|| bytes.to_vec());
    }

    fn segment_of_block(&self, cid: &ContentHash) -> Option<SegmentKey> {
        let cid = *cid.as_bytes();
        self.segment_blocks
            .range((cid, String::new(), String::new(), 0)..)
            .next()
            .filter(|((c, ..), _)| *c == cid)
            .map(|((_, scope, kind, segment_id), _)| SegmentKey {
                scope: scope.clone(),
                kind: kind.clone(),
                segment_id: *segment_id,
            })
    }

    fn relay_row(scope: &str, writer: [u8; 32], item_key: &[u8], held: &RelayStored) -> RelayRow {
        RelayRow {
            scope: scope.to_owned(),
            item_class: held.item_class.clone(),
            writer: WriterId(writer),
            writer_seq: held.writer_seq,
            item_key: item_key.to_vec(),
            op: held.op.clone(),
            entry: held.entry.clone(),
            feed_seq: held.feed_seq,
        }
    }

    /// Every relay row at `(scope, writer, writer_seq)`, optionally sparing
    /// one item — the SQLite arm's `delete_relay_rows_at`.
    fn delete_relay_rows_at(
        &mut self,
        scope: &str,
        writer: &WriterId,
        writer_seq: u64,
        keep_item_key: Option<&[u8]>,
    ) -> u64 {
        let before = self.relay.len();
        self.relay.retain(|(s, w, item), held| {
            !(s == scope
                && *w == writer.0
                && held.writer_seq == writer_seq
                && keep_item_key.is_none_or(|keep| item.as_slice() != keep))
        });
        (before - self.relay.len()) as u64
    }
}

impl StoreBackend for MemoryBackend {
    type Staging = MemoryStaging;

    // `data_version`: the trait's default `None` — this medium has no
    // cross-connection counter, the posture the web arm (whose medium has
    // none either) takes; the memory arm stands in for it.

    async fn meta_get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        Ok(self.tx()?.meta.get(key).cloned())
    }

    async fn meta_get_all(&self, keys: &[&str]) -> Result<Vec<Option<Vec<u8>>>> {
        let t = self.tx()?;
        Ok(keys.iter().map(|k| t.meta.get(*k).cloned()).collect())
    }

    async fn meta_put(&self, key: &str, value: &[u8]) -> Result<()> {
        self.tx()?.meta.insert(key.to_owned(), value.to_vec());
        Ok(())
    }

    async fn meta_put_all(&self, pairs: &[(&str, &[u8])]) -> Result<()> {
        let mut t = self.tx()?;
        for (key, value) in pairs {
            t.meta.insert((*key).to_owned(), value.to_vec());
        }
        Ok(())
    }

    async fn meta_delete(&self, key: &str) -> Result<()> {
        self.tx()?.meta.remove(key);
        Ok(())
    }

    async fn meta_delete_all_if_unchanged(
        &self,
        expected: &[(&str, Option<&[u8]>)],
    ) -> Result<bool> {
        let mut t = self.tx()?;
        if expected
            .iter()
            .any(|(key, want)| t.meta.get(*key).map(Vec::as_slice) != *want)
        {
            return Ok(false);
        }
        for (key, _) in expected {
            t.meta.remove(*key);
        }
        Ok(true)
    }

    async fn meta_put_pair_max(&self, first: (&str, u16), second: (&str, u16)) -> Result<()> {
        let mut t = self.tx()?;
        t.meta_put_max(first.0, u64::from(first.1))?;
        t.meta_put_max(second.0, u64::from(second.1))
    }

    async fn insert_row(
        &self,
        row: &JournalRow,
        local_writer: Option<&WriterId>,
    ) -> Result<InsertOutcome> {
        let mut t = self.tx()?;
        t.writer_guard(local_writer)?;
        t.insert_row(row)
    }

    async fn state_put_with_row(
        &self,
        entry: &StateEntry,
        row: &JournalRow,
        local_writer: Option<&WriterId>,
    ) -> Result<InsertOutcome> {
        fits_i64(entry.entry_version, "entry_version")?;
        let mut t = self.tx()?;
        t.writer_guard(local_writer)?;
        let stored = t.state.get(&(entry.kind.clone(), entry.key.clone()));
        if entry_moved(stored.map(|e| e.entry_version), entry.entry_version) {
            return Ok(InsertOutcome::EntryMoved);
        }
        let outcome = t.insert_row(row)?;
        if outcome == InsertOutcome::Inserted {
            t.state
                .insert((entry.kind.clone(), entry.key.clone()), entry.clone());
        }
        Ok(outcome)
    }

    async fn state_get(&self, kind: &str, key: &str) -> Result<Option<StateEntry>> {
        Ok(self
            .tx()?
            .state
            .get(&(kind.to_owned(), key.to_owned()))
            .cloned())
    }

    async fn group_state_put_with_row(
        &self,
        entry: &StateEntry,
        row: &JournalRow,
        local_writer: Option<&WriterId>,
    ) -> Result<InsertOutcome> {
        fits_i64(entry.entry_version, "entry_version")?;
        let mut t = self.tx()?;
        t.writer_guard(local_writer)?;
        let stored = t
            .group
            .get(&(entry.scope.clone(), entry.kind.clone(), entry.key.clone()));
        if entry_moved(stored.map(|e| e.entry_version), entry.entry_version) {
            return Ok(InsertOutcome::EntryMoved);
        }
        let outcome = t.insert_row(row)?;
        if outcome == InsertOutcome::Inserted {
            t.group.insert(
                (entry.scope.clone(), entry.kind.clone(), entry.key.clone()),
                entry.clone(),
            );
        }
        Ok(outcome)
    }

    async fn group_state_get(
        &self,
        scope: &str,
        kind: &str,
        key: &str,
    ) -> Result<Option<StateEntry>> {
        Ok(self
            .tx()?
            .group
            .get(&(scope.to_owned(), kind.to_owned(), key.to_owned()))
            .cloned())
    }

    async fn group_states_for_scope(&self, scope: &str) -> Result<Vec<StateEntry>> {
        Ok(self
            .tx()?
            .group
            .iter()
            .filter(|((s, ..), _)| s == scope)
            .map(|(_, e)| e.clone())
            .collect())
    }

    async fn max_writer_seq(&self, writer: &WriterId) -> Result<Option<u64>> {
        Ok(self
            .tx()?
            .journal
            .keys()
            .filter(|(_, w, _)| *w == writer.0)
            .map(|(_, _, seq)| *seq)
            .max())
    }

    async fn max_scope_writer_seq(&self, scope: &str, writer: &WriterId) -> Result<Option<u64>> {
        Ok(self
            .tx()?
            .journal
            .range((scope.to_owned(), writer.0, 0)..=(scope.to_owned(), writer.0, u64::MAX))
            .next_back()
            .map(|((_, _, seq), _)| *seq))
    }

    async fn rows_for_scope(
        &self,
        scope: &str,
        writer: &WriterId,
        after: u64,
        limit: u32,
    ) -> Result<Vec<JournalRow>> {
        let t = self.tx()?;
        let Some(from) = after.checked_add(1) else {
            return Ok(Vec::new());
        };
        t.journal
            .range((scope.to_owned(), writer.0, from)..=(scope.to_owned(), writer.0, u64::MAX))
            .take(limit as usize)
            .map(|((scope, w, seq), (op, item))| {
                Ok(JournalRow {
                    writer: WriterId(*w),
                    seq: *seq,
                    scope: scope.clone(),
                    op: *op,
                    item: ItemRef::decode(item)?,
                })
            })
            .collect()
    }

    async fn coordinate_of_item(
        &self,
        scope: &str,
        item: &ItemRef,
    ) -> Result<Option<(WriterId, u64)>> {
        let encoded = item.encode();
        Ok(self
            .tx()?
            .journal
            .iter()
            .filter(|((s, ..), (_, held))| s == scope && *held == encoded)
            .map(|((_, w, seq), _)| (*seq, *w))
            .min()
            .map(|(seq, w)| (WriterId(w), seq)))
    }

    async fn frontier(&self, scope: &str) -> Result<Vec<(WriterId, u64)>> {
        Ok(self
            .tx()?
            .frontiers
            .iter()
            .filter(|((s, _), _)| s == scope)
            .map(|((_, w), seq)| (WriterId(*w), *seq))
            .collect())
    }

    async fn frontier_raise(&self, scope: &str, writer: &WriterId, seq: u64) -> Result<u64> {
        fits_i64(seq, "high_seq")?;
        let mut t = self.tx()?;
        let slot = t
            .frontiers
            .entry((scope.to_owned(), writer.0))
            .or_insert(seq);
        *slot = (*slot).max(seq);
        Ok(*slot)
    }

    async fn nest_watermark(&self, scope: &str) -> Result<Option<u64>> {
        self.tx()?
            .meta
            .get(&nest_watermark_key(scope))
            .map(|raw| ascii_u64(raw).context("nest watermark"))
            .transpose()
    }

    async fn nest_watermark_raise(&self, scope: &str, seq: u64) -> Result<u64> {
        let key = nest_watermark_key(scope);
        let mut t = self.tx()?;
        t.meta_put_max(&key, seq)?;
        let now = t
            .meta
            .get(&key)
            .context("nest watermark absent inside its own raise")?;
        ascii_u64(now).context("nest watermark")
    }

    async fn nest_watermark_clear(&self, scope: &str) -> Result<()> {
        self.tx()?.meta.remove(&nest_watermark_key(scope));
        Ok(())
    }

    async fn relay_put(&self, row: &RelayRow) -> Result<()> {
        fits_i64(row.writer_seq, "relay writer_seq")?;
        if let Some(feed_seq) = row.feed_seq {
            fits_i64(feed_seq, "relay feed_seq")?;
        }
        let mut t = self.tx()?;
        let key = (row.scope.clone(), row.writer.0, row.item_key.clone());
        let supersedes = t
            .relay
            .get(&key)
            .is_none_or(|held| row.writer_seq > held.writer_seq);
        if supersedes {
            t.relay.insert(
                key.clone(),
                RelayStored {
                    item_class: row.item_class.clone(),
                    writer_seq: row.writer_seq,
                    op: row.op.clone(),
                    entry: row.entry.clone(),
                    feed_seq: row.feed_seq,
                    generation: row
                        .entry
                        .as_deref()
                        .and_then(fauna_core::account_entry_crypto::peek_generation_id),
                },
            );
        }
        // The one column a replay at the held coordinates may fill in (the
        // SQLite arm's trailing `relay_stamp_feed_seq`).
        if let Some(feed_seq) = row.feed_seq
            && let Some(held) = t.relay.get_mut(&key)
            && held.writer_seq == row.writer_seq
        {
            held.feed_seq = Some(feed_seq);
        }
        Ok(())
    }

    async fn relay_stamp_feed_seq(
        &self,
        scope: &str,
        writer: &WriterId,
        item_key: &[u8],
        writer_seq: u64,
        feed_seq: u64,
    ) -> Result<()> {
        fits_i64(writer_seq, "relay writer_seq")?;
        fits_i64(feed_seq, "relay feed_seq")?;
        let mut t = self.tx()?;
        if let Some(held) = t
            .relay
            .get_mut(&(scope.to_owned(), writer.0, item_key.to_vec()))
            && held.writer_seq == writer_seq
        {
            held.feed_seq = Some(feed_seq);
        }
        Ok(())
    }

    async fn relay_clear_feed_seqs(&self, scope: &str) -> Result<()> {
        let mut t = self.tx()?;
        for ((s, ..), held) in &mut t.relay {
            if s == scope {
                held.feed_seq = None;
            }
        }
        Ok(())
    }

    async fn issued_retire_put(&self, retire: &IssuedRetire, cap: u32) -> Result<()> {
        fits_i64(retire.writer_seq, "retire writer_seq")?;
        let mut t = self.tx()?;
        let ord = t.issued_retires.keys().next_back().map_or(1, |o| o + 1);
        let at = |r: &IssuedRetire| (r.scope.clone(), r.writer, r.item_key, r.writer_seq);
        t.issued_retires.retain(|_, held| at(held) != at(retire));
        t.issued_retires.insert(ord, retire.clone());
        while t.issued_retires.len() > cap as usize {
            t.issued_retires.pop_first();
        }
        Ok(())
    }

    async fn issued_retires(&self) -> Result<Vec<(u64, IssuedRetire)>> {
        Ok(self
            .tx()?
            .issued_retires
            .iter()
            .map(|(ord, r)| (*ord, r.clone()))
            .collect())
    }

    async fn issued_retires_clear_through(&self, through: u64) -> Result<()> {
        fits_i64(through, "retire order token")?;
        self.tx()?.issued_retires.retain(|ord, _| *ord > through);
        Ok(())
    }

    async fn relay_retire_shadowed(
        &self,
        scope: &str,
        writer: &WriterId,
        writer_seq: u64,
        keep_item_key: &[u8],
    ) -> Result<u64> {
        fits_i64(writer_seq, "relay writer_seq")?;
        Ok(self
            .tx()?
            .delete_relay_rows_at(scope, writer, writer_seq, Some(keep_item_key)))
    }

    async fn relay_retire_at(
        &self,
        scope: &str,
        writer: &WriterId,
        writer_seq: u64,
    ) -> Result<u64> {
        fits_i64(writer_seq, "relay writer_seq")?;
        Ok(self
            .tx()?
            .delete_relay_rows_at(scope, writer, writer_seq, None))
    }

    async fn relay_rows(
        &self,
        scope: &str,
        item_class: &str,
        frontier: &[(WriterId, u64)],
        limit: u32,
    ) -> Result<Vec<RelayRow>> {
        let t = self.tx()?;
        let mut due: Vec<RelayRow> = t
            .relay
            .iter()
            .filter(|((s, w, _), held)| {
                let high = frontier
                    .iter()
                    .find(|(fw, _)| fw.0 == *w)
                    .map_or(0, |(_, seq)| *seq);
                s == scope && held.item_class == item_class && held.writer_seq > high
            })
            .map(|((s, w, item), held)| Tables::relay_row(s, *w, item, held))
            .collect();
        due.sort_by_key(|r| (r.writer, r.writer_seq));
        due.truncate(limit as usize);
        Ok(due)
    }

    async fn state_forget(&self, kind: &str, key: &str) -> Result<()> {
        self.tx()?.state.remove(&(kind.to_owned(), key.to_owned()));
        Ok(())
    }

    async fn relay_forget(&self, scope: &str, writer: &WriterId, item_key: &[u8]) -> Result<()> {
        self.tx()?
            .relay
            .remove(&(scope.to_owned(), writer.0, item_key.to_vec()));
        Ok(())
    }

    async fn relay_generations(&self, scope: &str) -> Result<Vec<[u8; 32]>> {
        let t = self.tx()?;
        let held: BTreeSet<[u8; 32]> = t
            .relay
            .iter()
            .filter(|((s, ..), _)| s == scope)
            .filter_map(|(_, held)| held.generation)
            .collect();
        Ok(held.into_iter().collect())
    }

    async fn relay_forget_sealed_under(&self, scope: &str, generation: &[u8; 32]) -> Result<u64> {
        let mut t = self.tx()?;
        let before = t.relay.len();
        t.relay
            .retain(|(s, ..), held| !(s == scope && held.generation.as_ref() == Some(generation)));
        Ok((before - t.relay.len()) as u64)
    }

    async fn relay_rows_sealed_under(
        &self,
        scope: &str,
        generation: &[u8; 32],
    ) -> Result<Vec<RelayRow>> {
        Ok(self
            .tx()?
            .relay
            .iter()
            .filter(|((s, ..), held)| s == scope && held.generation.as_ref() == Some(generation))
            .map(|((s, w, item), held)| Tables::relay_row(s, *w, item, held))
            .collect())
    }

    async fn relay_rows_at(
        &self,
        scope: &str,
        item_class: &str,
        item_key: &[u8],
    ) -> Result<Vec<RelayRow>> {
        Ok(self
            .tx()?
            .relay
            .iter()
            .filter(|((s, _, item), held)| {
                s == scope && item.as_slice() == item_key && held.item_class == item_class
            })
            .map(|((s, w, item), held)| Tables::relay_row(s, *w, item, held))
            .collect())
    }

    async fn relay_rows_of_writer(
        &self,
        scope: &str,
        item_class: &str,
        writer: &WriterId,
    ) -> Result<Vec<RelayRow>> {
        let t = self.tx()?;
        let mut rows: Vec<RelayRow> = t
            .relay
            .iter()
            .filter(|((s, w, _), held)| {
                s == scope && *w == writer.0 && held.item_class == item_class
            })
            .map(|((s, w, item), held)| Tables::relay_row(s, *w, item, held))
            .collect();
        rows.sort_by_key(|r| r.writer_seq);
        Ok(rows)
    }

    async fn relay_high_waters(
        &self,
        scope: &str,
        item_class: &str,
    ) -> Result<Vec<(WriterId, u64)>> {
        let t = self.tx()?;
        let mut high: BTreeMap<[u8; 32], u64> = BTreeMap::new();
        for ((s, w, _), held) in &t.relay {
            if s == scope && held.item_class == item_class {
                let slot = high.entry(*w).or_insert(held.writer_seq);
                *slot = (*slot).max(held.writer_seq);
            }
        }
        Ok(high.into_iter().map(|(w, s)| (WriterId(w), s)).collect())
    }

    async fn relay_meter(&self, floor_ops: &[&str]) -> Result<Vec<RelayScopeMeter>> {
        let t = self.tx()?;
        let mut families: BTreeMap<(String, String), RelayScopeMeter> = BTreeMap::new();
        for ((scope, ..), held) in &t.relay {
            let bytes = held.entry.as_ref().map_or(0, |e| e.len() as u64);
            let evictable = !floor_ops.contains(&held.op.as_str()) && bytes > 0;
            let m = families
                .entry((scope.clone(), held.item_class.clone()))
                .or_insert_with(|| RelayScopeMeter {
                    scope: scope.clone(),
                    item_class: held.item_class.clone(),
                    ..RelayScopeMeter::default()
                });
            m.rows += 1;
            m.payload_bytes = m.payload_bytes.saturating_add(bytes);
            if evictable {
                m.evictable_rows += 1;
                m.evictable_bytes = m.evictable_bytes.saturating_add(bytes);
            }
        }
        Ok(families.into_values().collect())
    }

    async fn relay_evict_payload(
        &self,
        scope: &str,
        item_class: &str,
        target_bytes: u64,
        floor_ops: &[&str],
    ) -> Result<RelayEvicted> {
        if target_bytes == 0 {
            return Ok(RelayEvicted::default());
        }
        let mut t = self.tx()?;
        let mut candidates: Vec<(u64, [u8; 32], Vec<u8>, u64)> = t
            .relay
            .iter()
            .filter(|((s, ..), held)| {
                s == scope
                    && held.item_class == item_class
                    && held.entry.is_some()
                    && !floor_ops.contains(&held.op.as_str())
            })
            .map(|((_, w, item), held)| {
                let bytes = held.entry.as_ref().map_or(0, |e| e.len() as u64);
                (held.writer_seq, *w, item.clone(), bytes)
            })
            .collect();
        // Oldest first, as the SQLite arm orders it.
        candidates.sort_by_key(|c| (c.0, c.1));
        let mut freed = RelayEvicted::default();
        for (_, w, item, bytes) in candidates {
            if let Some(held) = t.relay.get_mut(&(scope.to_owned(), w, item))
                && held.entry.take().is_some()
            {
                freed.rows += 1;
                freed.bytes = freed.bytes.saturating_add(bytes);
            }
            if freed.bytes >= target_bytes {
                break;
            }
        }
        Ok(freed)
    }

    async fn state_entries_of_kind(&self, kind: &str) -> Result<Vec<StateEntry>> {
        Ok(self
            .tx()?
            .state
            .iter()
            .filter(|((k, _), e)| k == kind && !e.tombstone)
            .map(|(_, e)| e.clone())
            .collect())
    }

    async fn block_put(&self, cid: &ContentHash, bytes: &[u8]) -> Result<()> {
        self.tx()?.block_put(cid, bytes);
        Ok(())
    }

    async fn block_get(&self, cid: &ContentHash) -> Result<Option<Vec<u8>>> {
        let t = self.tx()?;
        if let Some(loose) = t.blocks.get(cid.as_bytes()) {
            return Ok(Some(loose.clone()));
        }
        let Some(key) = t.segment_of_block(cid) else {
            return Ok(None);
        };
        let seg = t
            .segments
            .get(&(key.scope.clone(), key.kind.clone(), key.segment_id))
            .context("a routing row names a segment this store does not hold")?;
        let dat = seg
            .dat
            .as_deref()
            .context("a routing row names an evicted segment")?;
        let mut reader = fauna_carv2::Reader::new(std::io::Cursor::new(dat))
            .map_err(|e| anyhow::anyhow!("adopted segment {key:?}: {e}"))?;
        reader.get(cid).map(Some).map_err(|e| {
            anyhow::anyhow!(
                "adopted segment {key:?} does not yield the block its routing row claims: {e}"
            )
        })
    }

    async fn block_has(&self, cid: &ContentHash) -> Result<bool> {
        let t = self.tx()?;
        Ok(t.blocks.contains_key(cid.as_bytes()) || t.segment_of_block(cid).is_some())
    }

    async fn block_delete(&self, cid: &ContentHash) -> Result<bool> {
        Ok(self.tx()?.blocks.remove(cid.as_bytes()).is_some())
    }

    async fn record_index_put(&self, entry: &RecordIndexEntry) -> Result<()> {
        self.tx()?.record_index_put(entry)
    }

    async fn record_index_get(&self, cid: &ContentHash) -> Result<Option<RecordIndexEntry>> {
        Ok(self.tx()?.record_index.get(cid.as_bytes()).cloned())
    }

    async fn record_index_delete(&self, cid: &ContentHash) -> Result<bool> {
        Ok(self.tx()?.record_index.remove(cid.as_bytes()).is_some())
    }

    async fn records_in_scope(
        &self,
        scope: &str,
        after: Option<&ContentHash>,
        limit: u32,
    ) -> Result<Vec<RecordIndexEntry>> {
        let t = self.tx()?;
        let rows = t.record_index.iter().filter(|(_, e)| e.scope == scope);
        Ok(match after {
            Some(after) => rows
                .filter(|(cid, _)| *cid > after.as_bytes())
                .take(limit as usize)
                .map(|(_, e)| e.clone())
                .collect(),
            None => rows.take(limit as usize).map(|(_, e)| e.clone()).collect(),
        })
    }

    async fn record_added_with_row(
        &self,
        entry: &RecordIndexEntry,
        bytes: Option<&[u8]>,
        row: &JournalRow,
        local_writer: Option<&WriterId>,
    ) -> Result<InsertOutcome> {
        if let Some(size) = entry.size {
            fits_i64(size, "record size")?;
        }
        let mut t = self.tx()?;
        t.writer_guard(local_writer)?;
        let outcome = t.insert_row(row)?;
        if outcome == InsertOutcome::Inserted {
            t.record_index_put(entry)?;
            if let Some(bytes) = bytes {
                t.block_put(&entry.cid, bytes);
            }
        }
        Ok(outcome)
    }

    async fn segment_stage(&self) -> Result<MemoryStaging> {
        Ok(MemoryStaging::default())
    }

    async fn segment_adopt(
        &self,
        key: &SegmentKey,
        staged: MemoryStaging,
        blocks: &[AdoptedBlock],
    ) -> Result<bool> {
        let MemoryStaging { dat, meta } = staged;
        for block in blocks {
            fits_i64(block.len, "block len")?;
        }
        let mut t = self.tx()?;
        let seg_key = (key.scope.clone(), key.kind.clone(), key.segment_id);
        if t.segments.contains_key(&seg_key) {
            return Ok(false);
        }
        t.segments.insert(
            seg_key,
            SegmentStored {
                dat: Some(dat),
                meta,
            },
        );
        for block in blocks {
            t.segment_blocks
                .entry((
                    *block.cid.as_bytes(),
                    key.scope.clone(),
                    key.kind.clone(),
                    key.segment_id,
                ))
                .or_insert(block.len);
        }
        Ok(true)
    }

    async fn segment_meter(&self) -> Result<Vec<SegmentScopeMeter>> {
        let t = self.tx()?;
        let mut scopes: BTreeMap<String, SegmentScopeMeter> = BTreeMap::new();
        for ((scope, ..), seg) in &t.segments {
            let m = scopes
                .entry(scope.clone())
                .or_insert_with(|| SegmentScopeMeter {
                    scope: scope.clone(),
                    ..SegmentScopeMeter::default()
                });
            m.segments += 1;
            m.meta_bytes = m.meta_bytes.saturating_add(seg.meta.len() as u64);
            if let Some(dat) = &seg.dat {
                m.held_segments += 1;
                m.dat_bytes = m.dat_bytes.saturating_add(dat.len() as u64);
            }
        }
        Ok(scopes.into_values().collect())
    }

    async fn segment_evict_dat(&self, scope: &str, target_bytes: u64) -> Result<RelayEvicted> {
        if target_bytes == 0 {
            return Ok(RelayEvicted::default());
        }
        let mut t = self.tx()?;
        // Oldest first: ascending segment_id, then kind (the SQLite arm's order).
        let mut candidates: Vec<(u32, String)> = t
            .segments
            .iter()
            .filter(|((s, ..), seg)| s == scope && seg.dat.is_some())
            .map(|((_, kind, id), _)| (*id, kind.clone()))
            .collect();
        candidates.sort();
        let mut freed = RelayEvicted::default();
        for (segment_id, kind) in candidates {
            if freed.bytes >= target_bytes {
                break;
            }
            let seg_key = (scope.to_string(), kind.clone(), segment_id);
            let Some(dat) = t.segments.get_mut(&seg_key).and_then(|seg| seg.dat.take()) else {
                continue;
            };
            t.segment_blocks
                .retain(|(_, s, k, id), _| !(s == scope && *k == kind && *id == segment_id));
            freed.rows += 1;
            freed.bytes = freed.bytes.saturating_add(dat.len() as u64);
        }
        Ok(freed)
    }

    async fn segments_in_scope(&self, scope: &str) -> Result<Vec<SegmentKey>> {
        Ok(self
            .tx()?
            .segments
            .keys()
            .filter(|(s, ..)| s == scope)
            .map(|(scope, kind, segment_id)| SegmentKey {
                scope: scope.clone(),
                kind: kind.clone(),
                segment_id: *segment_id,
            })
            .collect())
    }

    async fn segment_meta(&self, key: &SegmentKey) -> Result<Option<Vec<u8>>> {
        Ok(self
            .tx()?
            .segments
            .get(&(key.scope.clone(), key.kind.clone(), key.segment_id))
            .map(|s| s.meta.clone()))
    }

    async fn segment_of_block(&self, cid: &ContentHash) -> Result<Option<SegmentKey>> {
        Ok(self.tx()?.segment_of_block(cid))
    }

    async fn segment_block_len(&self, cid: &ContentHash) -> Result<Option<u64>> {
        let cid = *cid.as_bytes();
        Ok(self
            .tx()?
            .segment_blocks
            .range((cid, String::new(), String::new(), 0)..)
            .next()
            .filter(|((c, ..), _)| *c == cid)
            .map(|(_, len)| *len))
    }

    /// One transaction for every plane, files included — this medium's
    /// segment bytes are rows like any other, so there is no after-commit
    /// sweep and no pending mark to replay.
    async fn drop_scope(&self, scope: &str) -> Result<ScopeDropCounts> {
        let mut t = self.tx()?;
        let t = &mut *t;
        let held: Vec<[u8; 36]> = t
            .record_index
            .iter()
            .filter(|(_, e)| e.scope == scope)
            .map(|(cid, _)| *cid)
            .collect();
        let mut counts = ScopeDropCounts::default();
        for cid in &held {
            if t.blocks.remove(cid).is_some() {
                counts.blocks += 1;
            }
            t.record_index.remove(cid);
        }
        counts.record_index_rows = held.len() as u64;
        let count_retain = |n: &mut u64, before: usize, after: usize| *n = (before - after) as u64;

        let before = t.journal.len();
        t.journal.retain(|(s, ..), _| s != scope);
        count_retain(&mut counts.journal_rows, before, t.journal.len());

        let before = t.state.len();
        t.state.retain(|_, e| e.scope != scope);
        count_retain(&mut counts.state_entries, before, t.state.len());

        let before = t.frontiers.len();
        t.frontiers.retain(|(s, _), _| s != scope);
        count_retain(&mut counts.frontier_rows, before, t.frontiers.len());

        let before = t.relay.len();
        t.relay.retain(|(s, ..), _| s != scope);
        count_retain(&mut counts.relay_rows, before, t.relay.len());

        t.segment_blocks.retain(|(_, s, ..), _| s != scope);
        let before = t.segments.len();
        t.segments.retain(|(s, ..), _| s != scope);
        count_retain(&mut counts.segments, before, t.segments.len());

        t.meta.remove(&nest_watermark_key(scope));
        Ok(counts)
    }

    async fn scopes_of_writer(&self, writer: &WriterId) -> Result<Vec<String>> {
        let t = self.tx()?;
        let scopes: BTreeSet<&String> = t
            .journal
            .keys()
            .filter(|(_, w, _)| *w == writer.0)
            .map(|(s, ..)| s)
            .collect();
        Ok(scopes.into_iter().cloned().collect())
    }

    async fn compact_retired_rows(&self, rows: &[JournalRow]) -> Result<RetiredCompaction> {
        let mut t = self.tx()?;
        // The refusal is decided before any delete, so a refused call leaves
        // everything — the SQLite arm's roll-back-by-drop, without a rollback.
        let stamped = t.meta.get(crate::store::META_WRITER_ID).cloned();
        for row in rows {
            fits_i64(row.seq, "writer_seq")?;
            if stamped.as_deref() == Some(row.writer.0.as_slice()) {
                bail!(
                    "retired compaction: writer {} is the store's current writer — its log is \
                     never compacted (its append counter would re-issue the freed seqs)",
                    row.writer.to_hex()
                );
            }
        }
        let mut done = RetiredCompaction::default();
        for row in rows {
            let key = (row.scope.clone(), row.writer.0, row.seq);
            let matches = t
                .journal
                .get(&key)
                .is_some_and(|(op, item)| *op == row.op && *item == row.item.encode());
            if !matches {
                continue;
            }
            t.journal.remove(&key);
            done.journal_rows += 1;
            done.relay_rows +=
                t.delete_relay_rows_at(&row.scope, &row.writer, row.seq, None) as usize;
        }
        Ok(done)
    }

    async fn scopes_with_frontiers(&self) -> Result<Vec<String>> {
        let t = self.tx()?;
        let scopes: BTreeSet<&String> = t.frontiers.keys().map(|(s, _)| s).collect();
        Ok(scopes.into_iter().cloned().collect())
    }

    async fn outbox_append(
        &self,
        intent: &NewOutboxIntent,
        local_writer: Option<&WriterId>,
    ) -> Result<bool> {
        let mut t = self.tx()?;
        t.writer_guard(local_writer)?;
        if t.outbox.contains_key(&intent.intent_id) {
            return Ok(false);
        }
        let channel_seq = t
            .outbox
            .values()
            .filter(|o| o.scope == intent.scope)
            .map(|o| o.channel_seq)
            .max()
            .unwrap_or(0)
            + 1;
        t.outbox.insert(
            intent.intent_id,
            OutboxStored {
                kind: intent.kind.clone(),
                scope: intent.scope.clone(),
                payload: intent.payload.clone(),
                drainer: intent.drainer,
                channel_seq,
                status: IntentStatus::Pending,
                retry_count: 0,
                created_at: now_epoch_secs(),
                last_attempt_at: None,
            },
        );
        Ok(true)
    }

    async fn outbox_undrained(&self) -> Result<Vec<OutboxIntent>> {
        let t = self.tx()?;
        let mut out: Vec<OutboxIntent> = t
            .outbox
            .iter()
            .map(|(id, o)| OutboxIntent {
                intent_id: *id,
                kind: o.kind.clone(),
                scope: o.scope.clone(),
                payload: o.payload.clone(),
                drainer: o.drainer,
                channel_seq: o.channel_seq,
                status: o.status,
                retry_count: o.retry_count,
                created_at: o.created_at,
                last_attempt_at: o.last_attempt_at,
            })
            .collect();
        out.sort_by(|a, b| (&a.scope, a.channel_seq).cmp(&(&b.scope, b.channel_seq)));
        Ok(out)
    }

    async fn outbox_ack(&self, intent_id: &[u8; 16]) -> Result<bool> {
        Ok(self.tx()?.outbox.remove(intent_id).is_some())
    }

    async fn outbox_mark_failed(&self, intent_id: &[u8; 16]) -> Result<bool> {
        Ok(match self.tx()?.outbox.get_mut(intent_id) {
            Some(o) => {
                o.status = IntentStatus::Failed;
                true
            }
            None => false,
        })
    }

    async fn outbox_record_attempt(&self, intent_id: &[u8; 16]) -> Result<bool> {
        Ok(match self.tx()?.outbox.get_mut(intent_id) {
            Some(o) => {
                o.retry_count += 1;
                o.last_attempt_at = Some(now_epoch_secs());
                true
            }
            None => false,
        })
    }
}
