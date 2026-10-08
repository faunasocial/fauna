//! Principal succession's **un-pushed-tail re-author** — decision 3 of
//! `account-replica-posture.md` § The store device principal → *Principal
//! succession after a device delete*, run as a step of every full pump pass.
//!
//! The ceremony-time probe and the in-place writer rotation that ARM this
//! pass — by stamping the store's pending-re-author marker in the same
//! transaction as the writer fence — are native
//! (`fauna_sync_engine::principal_succession`: the nest evidence, the
//! credential slot and the migration section are the native host's). The
//! pass itself is store-only, which is why it lives here, in the wasm-capable
//! driver's crate: it reads the marker, re-journals every not-provably-pushed
//! predecessor row under the current writer, compacts a burnt predecessor's
//! carried rows, and clears the marker with a compare-and-delete against the
//! snapshot it decided on. A host that never fences (web today) runs it as
//! one `meta_get` per pass.

use anyhow::{Context, Result};
use fauna_account_store::backend::StoreBackend;
use fauna_account_store::store::{
    AccountStore, ReauthorMarker, clear_writer_reauthor_if_unchanged, pending_reauthor_snapshot,
};
use fauna_account_store::types::{ItemRef, JournalRow, StateEntry, WriterId};

/// Journal pages per re-author read (the `publish_pending` PAGE idiom).
const REAUTHOR_PAGE: u32 = 256;
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ReauthorPass {
    /// Distinct class-2 `(kind, key)` entries re-journaled under the
    /// successor this pass.
    pub reauthored: usize,
    /// Predecessor journal rows compacted this pass: a BURNT predecessor's
    /// carried class-2 rows, deleted once their entries re-put under the
    /// successor (refinement 11's compaction). Always 0 for a rotation off a
    /// writer no walk found burnt — those rows stay as local-only history.
    pub compacted: usize,
    /// Relay-plane rows deleted with them: the burnt life's own publish-time
    /// records at the compacted coordinates, which no peer may be served.
    pub relay_compacted: usize,
    /// Predecessor class-1 (`record-added`/CID) rows found un-pushed and NOT
    /// re-authored: local class-1 authorship has no production path yet
    /// (`stage_local_record` is unwired), so this is provably 0 in production
    /// today — a non-zero count is a build-order signal, warned loudly on the
    /// pass that walked them. When class-1 authorship gets wired, its
    /// re-author leg must land in this function BEFORE the marker clear at
    /// the bottom, or a rotation would clear the marker with class-1 rows
    /// still stranded.
    pub class1_unhandled: usize,
}

/// Decision 3 — the conservative tail re-author, run as a pump step (before
/// `publish_pending`, so re-authored rows publish on the same pass). Gated on
/// the durable marker the fence stamped, so it is crash-resumable and runs in
/// whichever process holds the pump — the rotating app or the app-dead agent
/// alike; no seed is needed to re-journal.
///
/// The bound: every predecessor journal row with
/// `writer_seq > frontier(scope, predecessor)` — the own-slot high-water IS
/// the push evidence ("our own slot in the scope's frontier is the published
/// high-water"). Rows at or below it are fleet-held history and stay
/// byte-identical. For each distinct `(kind, key)` above the bound, the
/// CURRENT entry re-puts under the successor — value, `merge_meta` (the LWW
/// stamp — preserved, never re-stamped: a re-author must not outrank fleet
/// writes the original never beat) and tombstone flag verbatim; the store
/// assigns fresh coordinates and the publish leg seals fresh under them (the
/// T14 AAD binds writer+seq, so envelopes are never copied). The predecessor
/// rows stay in the journal as local-only history — `publish_pending` walks
/// only the current writer's log, so nothing ever ships them — **unless the
/// walk found the predecessor's journal burnt** (the snapshot's
/// [`ReauthorMarker::burnt`] names it; `account-replica-posture.md` § The
/// store device principal, refinement 11). Then the rows the walk carried are
/// compacted right after their entries re-put: a burnt row can hold a
/// coordinate the previous life used for a different item, and once its
/// writer is retired the state walk drops the fleet's row there as an echo
/// for as long as the burnt row stays, because the retired arm's bound is
/// what the journal holds. The authority is `account-data-plane.md` § Store
/// logical schema: a writer may compact its own log's superseded class-2
/// rows, and a carried row is superseded by its re-put. The compaction
/// commits BEFORE the marker clear, so a crash anywhere before the clear
/// leaves the marker for a re-run, which is idempotent — a compacted row is
/// simply not walked again.
///
/// The outbox needs no walk, verified at build time (2026-08-15): intents
/// carry no writer column, `enqueue_intent` has NO production caller yet, and
/// the drain replays payloads verbatim — so no payload can embed the dead
/// writer today. The first production intent kind whose payload names the
/// writer must register a re-author hook here; this comment is that duty's
/// anchor.
pub async fn tail_reauthor_pass<B: StoreBackend>(
    store: &AccountStore<B>,
) -> Result<Option<ReauthorPass>> {
    // The stamp and BOTH marker keys in ONE transaction — the pair the fence
    // writes in one transaction, read back as one picture.
    //
    // ⚠ Filtered against the store's STAMPED writer, read live — never
    // `store.writer()`, which is a snapshot taken at `open` and never
    // reassigned (`rotate_writer_identity` takes `&B`, not the store, so no
    // open handle ever learns of a fence). The "unrepresentable" premise below
    // is true of the stamp and FALSE of a stale handle's cache: after a
    // co-located sibling fences `A -> B`, a still-open `A`-handle would filter
    // the marker's own `A` out, fall into the `is_empty` arm, and delete both
    // marker keys — losing `A`'s un-pushed tail for good, since the successor's
    // pump then finds nothing pending. Pinned by
    // `tests::a_siblings_fence_does_not_let_a_stale_handle_delete_the_reauthor_marker`.
    //
    // ⚠ And it is one SNAPSHOT for the same reason: taking the stamp and the
    // marker in two reads reintroduces exactly that loss as a race — a fence
    // landing between them serves a pre-fence stamp beside a post-fence
    // marker, and the filter drops the marker's own predecessor as if it were
    // our own writer. Reordering the two reads does not help; marker-first
    // reads empty pre-fence and the empty arm then clears the marker the fence
    // has just written. Pinned by
    // `tests::a_fence_between_the_snapshot_and_the_clear_leaves_the_marker_alone`.
    let snapshot = pending_reauthor_snapshot(store.backend()).await?;
    let priors: Vec<WriterId> = snapshot
        .priors
        .iter()
        .copied()
        // Unrepresentable via the fence (from != to by construction); skip a
        // hand-damaged marker rather than walking our own log as "prior".
        .filter(|prior| Some(*prior) != snapshot.stamped)
        .collect();
    if priors.is_empty() {
        clear_marker(store, &snapshot).await?;
        return Ok(None);
    }
    let mut pass = ReauthorPass::default();
    for prior in priors {
        let burnt = snapshot.burnt == Some(prior);
        reauthor_tail_of(store, &prior, burnt, &mut pass).await?;
    }
    if pass.class1_unhandled > 0 {
        tracing::warn!(
            count = pass.class1_unhandled,
            "tail re-author: predecessor class-1 rows have no re-author path yet \
             (local class-1 authorship is unwired) — counted, not dropped"
        );
    }
    clear_marker(store, &snapshot).await?;
    Ok(Some(pass))
}

/// The pass's only marker delete — a compare-and-delete against the snapshot
/// it decided on, so a fence landing anywhere between that decision and this
/// call leaves the marker for the next pass instead of erasing a tail nobody
/// walked.
///
/// Both of the pass's arms clear through here, because both are exposed. The
/// empty arm is the wide window (the whole filter runs in it). The post-walk
/// arm is the narrow one: a fence landing *during* the walk is caught by the
/// append-time writer guard — `reauthor_tail_of` re-puts through the guarded
/// path, so it refuses `StaleWriter` before ever reaching a clear — but a
/// fence landing after the last append and before this call is not, and the
/// marker it wrote names a predecessor this pass never saw.
async fn clear_marker<B: StoreBackend>(
    store: &AccountStore<B>,
    snapshot: &ReauthorMarker,
) -> Result<()> {
    #[cfg(test)]
    reauthor_window::fire();
    if !clear_writer_reauthor_if_unchanged(store.backend(), snapshot).await? {
        tracing::info!(
            "tail re-author: the pending marker changed under a co-located fence \
             between this pass's snapshot and its clear — leaving it for the next \
             pass, which walks the predecessor this one never saw (re-walking one \
             it did is idempotent: same value, same merge_meta)"
        );
    }
    Ok(())
}

/// The interleaving seam the race probes need: a fence must be able to land
/// **between** the pass's snapshot and its clear, and a probe that cannot
/// show its interleaving took is vacuous however it goes. Compiled out
/// entirely of anything but a test build — the same reason
/// (`e2e-conventions.md` convention 15: the automation surface is not in the
/// artifact). The mechanism itself is `fauna_core::process_hook::ProcessHook`
/// (round 116 lift — this module and `sqlite::pair_window` hand-copied it
/// byte-for-byte before then).
#[cfg(test)]
mod reauthor_window {
    use fauna_core::process_hook::{Hook, Installed, ProcessHook};

    static HOOK: ProcessHook = ProcessHook::new();

    pub(super) fn install(hook: Hook) -> Installed {
        HOOK.install(hook)
    }

    pub(super) fn fire() {
        HOOK.fire();
    }
}

/// One predecessor's walk — every scope it wrote, the distinct entries above
/// its published high-water re-put under the current writer, and for a
/// `burnt` predecessor the rows it carried compacted once those re-puts land.
async fn reauthor_tail_of<B: StoreBackend>(
    store: &AccountStore<B>,
    prior: &WriterId,
    burnt: bool,
    pass: &mut ReauthorPass,
) -> Result<()> {
    for scope in store.backend().scopes_of_writer(prior).await? {
        let high = store
            .frontier(&scope)
            .await?
            .into_iter()
            .find(|(w, _)| w == prior)
            .map(|(_, s)| s)
            .unwrap_or(0);
        // Distinct entries above the bound. Several un-pushed rows on one key
        // collapse to one re-put: the entry is the reconcile unit, journal
        // rows are its transport.
        let mut keys: std::collections::BTreeSet<(String, String)> = Default::default();
        // The class-2 rows the bound selected — the compaction's candidates.
        let mut carried: Vec<JournalRow> = Vec::new();
        // The predecessor's parked rows sit at or below its slot, which moved
        // past them as past an acked row, and no nest holds them: they are
        // part of the un-pushed tail (`account-replica-posture.md` § The
        // store device principal, refinement 11 → *A row refused for room is
        // parked*), carried and compacted exactly like the rows above it.
        let parked = store.parked(&scope, prior).await?;
        for seq in parked.iter().copied().filter(|seq| *seq <= high) {
            let Some(row) = store
                .scope_rows(&scope, prior, seq.saturating_sub(1), 1)
                .await?
                .into_iter()
                .find(|row| row.seq == seq)
            else {
                continue;
            };
            if let ItemRef::StateKey { kind, key, .. } = &row.item {
                keys.insert((kind.clone(), key.clone()));
                carried.push(row);
            }
        }
        let mut after = high;
        loop {
            let rows = store
                .scope_rows(&scope, prior, after, REAUTHOR_PAGE)
                .await?;
            if rows.is_empty() {
                break;
            }
            for row in rows {
                after = row.seq;
                if let ItemRef::StateKey { kind, key, .. } = &row.item {
                    keys.insert((kind.clone(), key.clone()));
                } else {
                    pass.class1_unhandled += 1;
                    continue;
                }
                carried.push(row);
            }
        }
        let mut reput: std::collections::BTreeSet<(String, String)> = Default::default();
        for (kind, key) in keys {
            let Some(current) = store.state(&kind, &key).await? else {
                // A journal row whose entry vanished has nothing to carry.
                continue;
            };
            store
                .put_state(StateEntry {
                    kind: kind.clone(),
                    key: key.clone(),
                    scope: current.scope,
                    value: current.value,
                    merge_meta: current.merge_meta,
                    entry_version: 0, // store-assigned
                    tombstone: current.tombstone,
                })
                .await
                .context("tail re-author: re-journal under the successor")?;
            pass.reauthored += 1;
            reput.insert((kind, key));
        }
        if burnt {
            // Refinement 11's compaction, over exactly the rows whose entries
            // re-put above. Each sits above the published high-water or is parked, so no
            // nest accepted it, and its value lives on under the successor
            // with its `merge_meta` verbatim: deleting it loses nothing, and
            // frees its coordinate for the fleet's own row there
            // (why-recreatable: superseded by the re-put). A class-1 row (no
            // re-author path) and a row whose entry vanished never reach
            // here. The backend deletes by exact coordinate AND item inside
            // one transaction, and refuses the store's current writer.
            carried.retain(|row| {
                matches!(&row.item, ItemRef::StateKey { kind, key, .. }
                    if reput.contains(&(kind.clone(), key.clone())))
            });
            let done = store
                .backend()
                .compact_retired_rows(&carried)
                .await
                .context("tail re-author: compact the burnt predecessor's carried rows")?;
            pass.compacted += done.journal_rows;
            pass.relay_compacted += done.relay_rows;
        }
        if !parked.is_empty() {
            // Their values now ride the successor's rows; before the marker
            // clear, so a crash leaves both for an idempotent re-run.
            store
                .clear_parked(&scope, prior)
                .await
                .context("tail re-author: clearing the predecessor's parked list")?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use fauna_account_store::sqlite::SqliteBackend;
    use fauna_account_store::store::{
        pending_writer_reauthors, rotate_writer_identity, stamped_writer,
    };

    fn writer(n: u8) -> WriterId {
        WriterId([n; 32])
    }

    async fn store_as(w: WriterId) -> AccountStore<SqliteBackend> {
        AccountStore::open(SqliteBackend::open_in_memory().unwrap(), "aa11", w)
            .await
            .unwrap()
    }

    /// A store on DISK, so a second `SqliteBackend` can open the same store —
    /// the co-located sibling *process* the succession machinery is written
    /// for (decision 4 exists because live co-located processes do). The
    /// in-memory fixture cannot host one: `SqliteBackend` is not `Clone` and
    /// an in-memory DB is private to its connection.
    async fn store_on_disk(w: WriterId) -> (tempfile::TempDir, AccountStore<SqliteBackend>) {
        let dir = tempfile::tempdir().unwrap();
        let backend = SqliteBackend::open(dir.path()).unwrap();
        let store = AccountStore::open(backend, "aa11", w).await.unwrap();
        (dir, store)
    }

    /// The sibling's fence, run from inside the pass's own window hook — a
    /// **synchronous** context on a tokio worker, where `block_on` would
    /// panic. `SqliteBackend`'s async methods resolve from synchronous bodies
    /// (the seam is async only so an IndexedDB backend can exist), so one
    /// poll completes them; a park here would be a real finding, hence the
    /// `expect` rather than a silent retry.
    fn fence_now(backend: &SqliteBackend, from: WriterId, to: WriterId) {
        use futures_util::FutureExt;
        rotate_writer_identity(backend, &from, &to)
            .now_or_never()
            .expect("the backend future parked — SqliteBackend must resolve synchronously")
            .expect("the sibling's fence");
    }

    /// An un-pushed local row under the store's current writer — the tail the
    /// marker exists to protect.
    async fn put_a_row(store: &AccountStore<SqliteBackend>, value: &[u8]) {
        store
            .put_state(StateEntry {
                kind: "moderation".into(),
                key: "muted".into(),
                scope: "state".into(),
                value: value.to_vec(),
                merge_meta: None,
                entry_version: 0,
                tombstone: false,
            })
            .await
            .unwrap();
    }

    /// One life of `w` on `state`: a published row, then two carried
    /// (un-pushed) rows on two keys, each beside the relay-plane row its
    /// publish recorded. Returns the published seq and the carried seqs.
    async fn a_life_with_a_carried_tail(
        store: &AccountStore<SqliteBackend>,
        w: WriterId,
    ) -> (u64, Vec<u64>) {
        let mut seqs = Vec::new();
        for (key, value) in [
            ("muted", &b"published"[..]),
            ("muted", &b"carried"[..]),
            ("other", &b"carried-too"[..]),
        ] {
            let (_, seq) = store
                .put_state(StateEntry {
                    kind: "moderation".into(),
                    key: key.into(),
                    scope: "state".into(),
                    value: value.to_vec(),
                    merge_meta: Some(b"a-stamp".to_vec()),
                    entry_version: 0,
                    tombstone: false,
                })
                .await
                .unwrap();
            store
                .record_relay_row(&fauna_account_store::types::RelayRow {
                    scope: "state".into(),
                    item_class: "state-entry".into(),
                    writer: w,
                    writer_seq: seq,
                    item_key: format!("{key}-{seq}").into_bytes(),
                    op: "state-put".into(),
                    entry: Some(value.to_vec()),
                    feed_seq: None,
                })
                .await
                .unwrap();
            seqs.push(seq);
        }
        store.advance_frontier("state", &w, seqs[0]).await.unwrap();
        (seqs[0], seqs[1..].to_vec())
    }

    /// The fence `from -> to` over `dir`, then the store reopened as the
    /// successor: the shape the pump's pass meets after a rotation.
    async fn fenced_onto(
        dir: &tempfile::TempDir,
        from: WriterId,
        to: WriterId,
    ) -> AccountStore<SqliteBackend> {
        rotate_writer_identity(&SqliteBackend::open(dir.path()).unwrap(), &from, &to)
            .await
            .unwrap();
        AccountStore::open(SqliteBackend::open(dir.path()).unwrap(), "aa11", to)
            .await
            .unwrap()
    }

    /// `w`'s journal seqs on `state`, ascending.
    async fn seqs_of(store: &AccountStore<SqliteBackend>, w: &WriterId) -> Vec<u64> {
        store
            .scope_rows("state", w, 0, 100)
            .await
            .unwrap()
            .iter()
            .map(|r| r.seq)
            .collect()
    }

    /// **Refinement 11's compaction.** A predecessor the walk found burnt: once
    /// its carried entries re-put under the successor, the rows that carried
    /// them go, journal and relay plane alike, so its held seq falls back to
    /// its published high-water (the fleet's rows above it then walk in as
    /// retired history), and the published row stays.
    #[tokio::test]
    async fn a_burnt_predecessors_carried_rows_are_compacted_once_their_entries_re_put() {
        let (a, b) = (writer(0xaa), writer(0xbb));
        let (dir, store) = store_on_disk(a).await;
        let (published, _) = a_life_with_a_carried_tail(&store, a).await;
        store.mark_writer_burnt(&a).await.unwrap();
        drop(store);
        let store = fenced_onto(&dir, a, b).await;

        let pass = tail_reauthor_pass(&store)
            .await
            .unwrap()
            .expect("a predecessor was owed");

        assert_eq!(pass.reauthored, 2, "both carried keys re-put: {pass:?}");
        assert_eq!(
            (pass.compacted, pass.relay_compacted),
            (2, 2),
            "the carried rows and their relay rows are compacted: {pass:?}"
        );
        assert_eq!(
            seqs_of(&store, &a).await,
            vec![published],
            "only the burnt writer's published row stays"
        );
        assert_eq!(
            store.max_held_seq("state", &a).await.unwrap(),
            Some(published),
            "held falls back to the published high-water"
        );
        let relayed: Vec<u64> = store
            .relay_rows("state", "state-entry", &[], 100)
            .await
            .unwrap()
            .iter()
            .filter(|r| r.writer == a)
            .map(|r| r.writer_seq)
            .collect();
        assert_eq!(
            relayed,
            vec![published],
            "no peer is served a compacted row"
        );
        assert_eq!(
            seqs_of(&store, &b).await.len(),
            2,
            "the successor journals both"
        );
        let muted = store.state("moderation", "muted").await.unwrap().unwrap();
        assert_eq!(
            (muted.value.as_slice(), muted.merge_meta.as_deref()),
            (&b"carried"[..], Some(&b"a-stamp"[..])),
            "the carried value lives on with its merge_meta verbatim"
        );
        assert!(
            pending_writer_reauthors(store.backend())
                .await
                .unwrap()
                .is_empty(),
            "the marker cleared after the compaction"
        );
    }

    /// Refinement 5 stands for every other rotation: a predecessor no walk
    /// found burnt keeps its carried rows as local-only history — even while
    /// the store's burnt verdict names some other writer.
    #[tokio::test]
    async fn a_rotation_off_a_writer_never_found_burnt_keeps_its_carried_rows() {
        let (a, b) = (writer(0xaa), writer(0xbb));
        let (dir, store) = store_on_disk(a).await;
        let (published, carried) = a_life_with_a_carried_tail(&store, a).await;
        store.mark_writer_burnt(&writer(0xcc)).await.unwrap();
        drop(store);
        let store = fenced_onto(&dir, a, b).await;

        let pass = tail_reauthor_pass(&store)
            .await
            .unwrap()
            .expect("a predecessor was owed");

        assert_eq!(pass.reauthored, 2, "{pass:?}");
        assert_eq!(
            (pass.compacted, pass.relay_compacted),
            (0, 0),
            "nothing compacted: {pass:?}"
        );
        let mut kept = vec![published];
        kept.extend(&carried);
        assert_eq!(seqs_of(&store, &a).await, kept);
    }

    /// A class-1 row above the bound has no re-author path (refinement 5), so
    /// the compaction never takes it, even for a burnt predecessor: it stays,
    /// counted.
    #[tokio::test]
    async fn a_burnt_predecessors_class1_rows_are_never_compacted() {
        let (a, b) = (writer(0xaa), writer(0xbb));
        let (dir, store) = store_on_disk(a).await;
        let (published, _) = a_life_with_a_carried_tail(&store, a).await;
        let record = store
            .append_record_added(
                "state",
                fauna_core::data::ContentHash::of_raw(b"a-local-record"),
            )
            .await
            .unwrap();
        store.mark_writer_burnt(&a).await.unwrap();
        drop(store);
        let store = fenced_onto(&dir, a, b).await;

        let pass = tail_reauthor_pass(&store)
            .await
            .unwrap()
            .expect("a predecessor was owed");

        assert_eq!(pass.class1_unhandled, 1, "{pass:?}");
        assert_eq!(pass.compacted, 2, "{pass:?}");
        assert_eq!(
            seqs_of(&store, &a).await,
            vec![published, record],
            "the class-1 row stays"
        );
    }

    /// **The compaction commits before the marker clear**, so a crash between
    /// the two leaves the marker, and the compaction owed, rather than a
    /// cleared marker over rows still shadowing the fleet's. Observed from a
    /// second connection inside the clear's own window.
    #[tokio::test]
    async fn the_compaction_commits_before_the_marker_clear() {
        let (a, b) = (writer(0xaa), writer(0xbb));
        let (dir, store) = store_on_disk(a).await;
        let (published, _) = a_life_with_a_carried_tail(&store, a).await;
        store.mark_writer_burnt(&a).await.unwrap();
        drop(store);
        let store = fenced_onto(&dir, a, b).await;

        let sibling = Mutex::new(SqliteBackend::open(dir.path()).unwrap());
        /// The predecessor's journal seqs and the owed marker, as the window saw them.
        type AtTheClear = Option<(Vec<u64>, Vec<WriterId>)>;
        let seen: Arc<Mutex<AtTheClear>> = Arc::default();
        let record = Arc::clone(&seen);
        let installed = reauthor_window::install(Arc::new(move || {
            use futures_util::FutureExt;
            let backend = sibling.lock().unwrap();
            let rows = backend
                .rows_for_scope("state", &a, 0, 100)
                .now_or_never()
                .expect("the backend future parked")
                .expect("rows");
            let owed = pending_writer_reauthors(&*backend)
                .now_or_never()
                .expect("the backend future parked")
                .expect("marker");
            *record.lock().unwrap() = Some((rows.iter().map(|r| r.seq).collect(), owed));
        }));

        let outcome = tail_reauthor_pass(&store).await;
        drop(installed);

        let (rows, owed) = seen
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| panic!("the clear's window never fired (pass: {outcome:?})"));
        assert_eq!(owed, vec![a], "sanity: the window sits before the clear");
        assert_eq!(
            rows,
            vec![published],
            "the marker clear ran with the carried rows still journaled — a crash there \
             would leave the compaction owed to nobody (pass: {outcome:?})"
        );
    }

    /// **A live sibling's fence must not make this handle delete the marker.**
    ///
    /// `tail_reauthor_pass` filters the pending predecessors — a **live** meta
    /// read — against the store's writer. Reading that from `store.writer()`
    /// was the defect: it is a snapshot taken at `open` and never reassigned
    /// (`rotate_writer_identity` takes `&B`, not the store, so no open handle
    /// ever learns of a fence). The filter's comment justified itself on the
    /// fence's `from != to`, which is true of the store's *stamped* writer and
    /// false of a *handle's cached* one.
    ///
    /// So after a co-located process fenced `A -> B`, a still-open `A`-handle
    /// filtered `A` out as if the marker were hand-damaged, fell into the
    /// `priors.is_empty()` arm, and **deleted both marker keys** — silently, no
    /// error, `Ok(None)`. `A`'s un-pushed tail was then re-authored by nobody:
    /// the successor's own pump finds nothing pending. That is the product's
    /// **No user-data loss** invariant (`principles.md`), which
    /// `account-data-plane.md` § The store device principal → succession
    /// decision 3 names as the reason the tail is re-authored rather than wiped.
    ///
    /// Two processes over one store dir is the documented deployment — decision
    /// 4 exists because live co-located processes do — and the lost-slot heal's
    /// own flagship scenario is the trigger: the agent serves with un-pushed
    /// rows while the app's assembly finds the slot reset and fences onto a
    /// fresh writer.
    ///
    /// What must happen instead: the stale handle keeps the real prior, and its
    /// attempt to re-author refuses at the append guard (decision 4) — so the
    /// marker SURVIVES for the successor's pump, which is the only handle that
    /// can legitimately do the walk.
    #[tokio::test]
    async fn a_siblings_fence_does_not_let_a_stale_handle_delete_the_reauthor_marker() {
        let (a, b) = (writer(0xaa), writer(0xbb));
        let store = store_as(a).await;

        // An un-pushed row under A — the tail the marker exists to protect.
        store
            .put_state(StateEntry {
                kind: "moderation".into(),
                key: "muted".into(),
                scope: "state".into(),
                value: b"survives-the-sibling-fence".to_vec(),
                merge_meta: None,
                entry_version: 0,
                tombstone: false,
            })
            .await
            .unwrap();

        // The sibling fences A -> B on the shared backend. Our handle is still
        // open and still caches A.
        rotate_writer_identity(store.backend(), &a, &b)
            .await
            .unwrap();
        assert_eq!(
            pending_writer_reauthors(store.backend()).await.unwrap(),
            vec![a],
            "sanity: the fence recorded A's tail as owed"
        );

        // Our (now stale) handle's next pump tick runs the pass. Whether it
        // errors at the append guard or returns is not the subject — what must
        // never happen is the marker going away with the tail un-re-authored.
        let outcome = tail_reauthor_pass(&store).await;

        assert_eq!(
            pending_writer_reauthors(store.backend()).await.unwrap(),
            vec![a],
            "a stale handle DELETED the pending-re-author marker: A's un-pushed \
             tail will never be re-authored by anyone, because the successor's \
             own pump will find nothing pending (pass returned {outcome:?})"
        );
    }

    /// The guard the filter was actually written for still works, and the fix
    /// must not cost it: a marker naming the store's **stamped** writer is
    /// unrepresentable through the fence (`from != to` by construction), so it
    /// can only be hand damage — skip it rather than walk our own log as
    /// "prior", and clearing it is then correct, because it really was read as
    /// nothing to do.
    ///
    /// Written through the raw meta key on purpose: no public API can produce
    /// this state, which is exactly what makes it the damage case.
    #[tokio::test]
    async fn a_marker_naming_the_stores_own_stamped_writer_is_still_skipped_and_cleared() {
        let a = writer(0xaa);
        let store = store_as(a).await;
        store
            .backend()
            .meta_put("prior_writer_id", &a.0)
            .await
            .unwrap();
        assert_eq!(
            pending_writer_reauthors(store.backend()).await.unwrap(),
            vec![a],
            "sanity: the damaged marker names the stamped writer"
        );

        assert!(
            tail_reauthor_pass(&store).await.unwrap().is_none(),
            "a marker naming only the stamped writer is nothing to do"
        );
        assert!(
            pending_writer_reauthors(store.backend())
                .await
                .unwrap()
                .is_empty(),
            "…and clearing it is correct: it really was read as empty"
        );
    }

    // The ordinary case — a successor handle walks a real predecessor's tail
    // and only then clears the marker — is covered end to end by
    // `account_runtime::tests::
    // a_lost_slot_over_a_stamped_store_self_heals_into_a_successor_with_its_unpushed_tail`,
    // which drives it through a real assembly. It is not re-pinned here: two
    // handles over one store need a shared backend, and `SqliteBackend` is not
    // `Clone`, so the fixture would be invented rather than real.

    /// **A fence landing between the pass's snapshot and its clear must not
    /// erase the marker**, the serialization half of the
    /// loss closed for a stale handle's *cached* writer.
    ///
    /// The pass filters the pending predecessors against the store's stamped
    /// writer, so the two must come from one instant. Read separately, a
    /// co-located sibling's fence in the gap serves a **pre-fence stamp beside
    /// a post-fence marker**: the filter drops the marker's own predecessor as
    /// if it were our own writer, the empty arm clears, and the predecessor's
    /// un-pushed tail is re-authored by nobody — the successor's pump finds
    /// nothing pending. That is the **No user-data loss** invariant
    /// (`principles.md`), which `account-data-plane.md` § The store device
    /// principal → succession decision 3 names as the reason the tail is
    /// re-authored rather than wiped.
    ///
    /// Reordering the two reads does not fix it and this test does not ask for
    /// it: marker-first reads empty pre-fence, and the `is_empty()` arm then
    /// deletes the marker the fence has just written. The fix is that the pass
    /// decides on ONE snapshot **and** re-asserts it in the delete.
    ///
    /// **The probe shows its own interleaving took** (the vacuous-race lesson):
    /// the fence is fired by [`reauthor_window`], which sits *between* the
    /// snapshot and the clear by construction, and the test asserts the hook
    /// ran. The sibling is a second `SqliteBackend` over the same store dir — a
    /// real second connection, the co-located process decision 4 exists for.
    #[tokio::test]
    async fn a_fence_between_the_snapshot_and_the_clear_leaves_the_marker_alone() {
        let (dir, store) = store_on_disk(writer(0xaa)).await;
        let (a, b) = (writer(0xaa), writer(0xbb));
        put_a_row(&store, b"survives-the-mid-pass-fence").await;

        // Nothing pending yet: this pass reads an EMPTY marker and takes the
        // empty arm — the wide window, where the whole filter runs.
        assert!(
            pending_writer_reauthors(store.backend())
                .await
                .unwrap()
                .is_empty(),
            "sanity: the pass starts with nothing owed"
        );

        // `Connection` is Send but not Sync and the hook is `Fn + Send + Sync`;
        // the mutex is only what carries the sibling across that bound.
        let sibling = Mutex::new(SqliteBackend::open(dir.path()).unwrap());
        let fired = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&fired);
        let installed = reauthor_window::install(Arc::new(move || {
            fence_now(&sibling.lock().unwrap(), a, b);
            flag.store(true, Ordering::SeqCst);
        }));

        let outcome = tail_reauthor_pass(&store).await;
        drop(installed);

        assert!(
            fired.load(Ordering::SeqCst),
            "the window never fired — the probe proves nothing about a race it \
             never staged (pass returned {outcome:?})"
        );
        assert_eq!(
            stamped_writer(store.backend()).await.unwrap(),
            Some(b),
            "sanity: the sibling's fence really did land, inside the window"
        );
        assert_eq!(
            pending_writer_reauthors(store.backend()).await.unwrap(),
            vec![a],
            "the pass DELETED a marker a fence wrote under it: A's un-pushed tail \
             will never be re-authored by anyone, because the successor's own pump \
             finds nothing pending (pass returned {outcome:?})"
        );
    }

    /// The same window at the pass's **other** clear: a fence landing after the
    /// last re-authored append and before the post-walk clear.
    ///
    /// A fence landing *during* the walk is caught — `reauthor_tail_of` re-puts
    /// through the guarded append path, which refuses `StaleWriter` before any
    /// clear runs — but nothing guards the gap after the final append, and the
    /// marker that fence wrote names a predecessor THIS pass never walked. So
    /// deleting it strands that tail exactly as the empty arm did.
    ///
    /// Both arms clear through the one compare-and-delete, so this is the same
    /// mechanism observed at the second site. It is pinned separately because a
    /// fix covering only the empty arm would pass the probe above and still
    /// lose data here.
    #[tokio::test]
    async fn a_fence_after_the_walk_and_before_the_clear_leaves_the_new_marker_alone() {
        let (dir, store) = store_on_disk(writer(0xaa)).await;
        let (a, b, c) = (writer(0xaa), writer(0xbb), writer(0xcc));
        put_a_row(&store, b"the-tail-this-pass-walks").await;
        drop(store);

        // A real fence A -> B, then a handle stamped B: this pass has a genuine
        // predecessor to walk, so it reaches the POST-WALK clear.
        {
            let sibling = SqliteBackend::open(dir.path()).unwrap();
            rotate_writer_identity(&sibling, &a, &b).await.unwrap();
        }
        let store = AccountStore::open(SqliteBackend::open(dir.path()).unwrap(), "aa11", b)
            .await
            .unwrap();
        assert_eq!(
            pending_writer_reauthors(store.backend()).await.unwrap(),
            vec![a],
            "sanity: A's tail is owed and B is the walker"
        );

        let sibling = Mutex::new(SqliteBackend::open(dir.path()).unwrap());
        let fired = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&fired);
        let installed = reauthor_window::install(Arc::new(move || {
            fence_now(&sibling.lock().unwrap(), b, c);
            flag.store(true, Ordering::SeqCst);
        }));

        let outcome = tail_reauthor_pass(&store).await;
        drop(installed);

        assert!(
            fired.load(Ordering::SeqCst),
            "the window never fired — the probe staged no race (pass: {outcome:?})"
        );
        assert!(
            matches!(&outcome, Ok(Some(p)) if p.reauthored > 0),
            "the probe must reach the POST-WALK clear, not the empty arm — a pass \
             that walked nothing is testing the other window: {outcome:?}"
        );
        let owed = pending_writer_reauthors(store.backend()).await.unwrap();
        assert!(
            owed.contains(&b),
            "the post-walk clear DELETED the marker a fence wrote after our last \
             append: B's un-pushed tail is owed to nobody, and B is not this pass's \
             predecessor — it never walked it (owed: {owed:?}, pass: {outcome:?})"
        );
    }

    /// The primitive both probes rest on, stated on its own: the clear refuses
    /// when the marker is not what its caller read.
    ///
    /// A consistent *read* alone is not enough — the decision to clear is taken
    /// on a snapshot and the fence can land after it — so the delete has to
    /// re-assert the picture it was decided on. Pinned separately because the
    /// two probes above would both still pass against a compare-and-delete that
    /// always answered "deleted".
    #[tokio::test]
    async fn the_marker_clear_refuses_a_picture_that_changed_under_it() {
        let (dir, store) = store_on_disk(writer(0xaa)).await;
        let (a, b) = (writer(0xaa), writer(0xbb));

        let snapshot = pending_reauthor_snapshot(store.backend()).await.unwrap();
        assert!(
            snapshot.priors.is_empty(),
            "sanity: nothing owed at the read"
        );

        let sibling = SqliteBackend::open(dir.path()).unwrap();
        rotate_writer_identity(&sibling, &a, &b).await.unwrap();

        assert!(
            !clear_writer_reauthor_if_unchanged(store.backend(), &snapshot)
                .await
                .unwrap(),
            "the clear deleted against a picture that had changed under it"
        );
        assert_eq!(
            pending_writer_reauthors(store.backend()).await.unwrap(),
            vec![a],
            "…and the marker it refused to delete is still there"
        );

        // Unchanged since the read: it clears, and reports that it did.
        let fresh = pending_reauthor_snapshot(store.backend()).await.unwrap();
        assert_eq!(fresh.priors, vec![a]);
        assert_eq!(fresh.stamped, Some(b));
        assert!(
            clear_writer_reauthor_if_unchanged(store.backend(), &fresh)
                .await
                .unwrap()
        );
        assert!(
            pending_writer_reauthors(store.backend())
                .await
                .unwrap()
                .is_empty()
        );
    }
}
