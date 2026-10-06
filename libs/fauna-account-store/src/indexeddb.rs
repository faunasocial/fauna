//! Web's physical backend: the store's logical schema over **IndexedDB**
//! object stores, adopted segment bytes in the **Origin Private File System**
//! (charter: `account-data-plane.md` § Store logical schema, *Physical
//! realization* — "a parallel physical backend behind the same store API").
//!
//! One IndexedDB database per store, named by the caller (a per-origin,
//! per-actor name stands where the native arm has `<state base>/<actor-id-hex>/`
//! — a browser origin has no filesystem to root it in), and one OPFS
//! directory beside it for the segment area. The web arm owes the logical
//! schema, never the native file layout (§ The account store → *Physical
//! form*: same-on-disk is a desktop property by construction).
//!
//! **Transactions.** Every [`StoreBackend`] method is ONE IndexedDB
//! transaction over every object store it names — the multi-table methods
//! (`state_put_with_row`, `group_state_put_with_row`, `record_added_with_row`,
//! the `meta_*_all*` family, `compact_retired_rows`, `drop_scope`'s rows)
//! included. An IndexedDB transaction auto-commits the moment it has no
//! pending request at the end of a task, which binds a body twice over:
//!
//! - **It awaits nothing but that transaction's own requests.** A request's
//!   success resolves a promise whose continuation runs in the same microtask
//!   checkpoint, while the transaction is still active.
//! - **It runs in a task of its own** ([`IndexedDbBackend::transact`], the
//!   only door to a transaction; [`own_task`]). "The continuation runs in the
//!   same checkpoint" is true of the task the browser wakes, and a caller's
//!   task does not always poll what it is woken for: the account driver
//!   serves a local command inside a pass's yield point and leaves the pass
//!   unpolled meanwhile, so a read-then-write the pass had open saw its read
//!   answered, nobody ran the continuation, the request-less transaction
//!   committed whatever had landed, and the write met
//!   `TransactionInactiveError` — every pass of a busy tab, until
//!   2026-10-01. A body therefore owns everything it touches (`'static`),
//!   and a method clones what it was lent before it opens one.
//!
//! A body that fails aborts the transaction — IndexedDB would otherwise
//! commit whatever requests had already landed — so a refusal (the writer
//! guard's `StaleWriter`, the compaction's current-writer refusal) leaves
//! nothing behind.
//!
//! **Integers.** A key's number is an IEEE double, exact only to 2^53; every
//! `u64` this store keeps (sequence numbers, sizes, versions) rests as 8
//! big-endian bytes instead — IndexedDB orders binary keys bytewise, which for
//! big-endian is numeric order, so the ranges below sort exactly as the SQLite
//! arm's integer columns do. The segment id (a `u32`) is the one plain number.
//!
//! **Segments.** Files first, rows second on adoption; rows first, files
//! second on departure, guarded by the pending-drop mark the next open
//! replays — the SQLite arm's crash discipline, over OPFS files. OPFS is
//! reached by property lookup, not typed `web_sys` bindings (its types sit
//! behind `web_sys`'s unstable-APIs cfg, the Web Locks leg's reason —
//! `fauna_client_accounts::web_mutation_lock`).
//!
//! **Cross-connection notice:** `data_version` answers `None` — IndexedDB has
//! no cross-connection change counter; web's poke is the platform's
//! BroadcastChannel (`account-runtime.md` § Multi-instance concurrency, the
//! web leg).

#![cfg(target_arch = "wasm32")]

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::rc::Rc;
use std::task::{Poll, Waker};

use anyhow::{Context, Result, anyhow, bail};
use js_sys::{Array, Object, Promise, Reflect, Uint8Array};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    DomException, IdbCursorDirection, IdbCursorWithValue, IdbDatabase, IdbFactory, IdbIndex,
    IdbIndexParameters, IdbKeyRange, IdbObjectStore, IdbObjectStoreParameters, IdbOpenDbRequest,
    IdbRequest, IdbTransaction, IdbTransactionMode,
};

use fauna_core::data::ContentHash;

use crate::backend::{
    ReadSeek, RelayEvicted, RelayScopeMeter, RetiredCompaction, ScopeDropCounts, SegmentScopeMeter,
    SegmentStaging, StoreBackend,
};
use crate::physical::{
    META_SCOPE_DROPS_PENDING, STAGING_PREFIX, ascii_u64, entry_moved, fits_i64, nest_watermark_key,
    parse_pending_drops, rising_meta_integer, sanitize_component, scope_file_prefix, segment_stem,
    staging_stem,
};
use crate::segments::{AdoptedBlock, SegmentHalf, SegmentKey, SegmentSink};
use crate::types::{
    InsertOutcome, IntentDrainer, IntentStatus, IssuedRetire, ItemRef, JournalOp, JournalRow,
    NewOutboxIntent, OutboxIntent, RecordIndexEntry, RelayRow, StateEntry, WriterId,
};

/// The IndexedDB schema version. Version 1 is the genesis: every object store
/// and index at its current shape. A later additive shape (a new store, a new
/// index) is a version bump whose `upgradeneeded` step adds exactly that; a
/// nullable value field needs no bump at all (an absent property reads as
/// `None`, the SQLite arm's nullable column).
///
/// **2 (2026-10-01):** the retire record's object store ([`RETIRES`]) —
/// additive: the upgrade creates it and touches nothing else.
const DB_VERSION: u32 = 2;

// ── Object stores (the SQLite arm's tables, one to one) ──────────────────────
const META: &str = "store_meta";
const JOURNAL: &str = "journal";
const STATE: &str = "state_entries";
const GROUP: &str = "group_entries";
const FRONTIERS: &str = "frontiers";
const RECORDS: &str = "record_index";
const BLOCKS: &str = "blocks";
const SEGMENTS: &str = "segments";
const SEGMENT_BLOCKS: &str = "segment_blocks";
const RELAY: &str = "relay_rows";
const OUTBOX: &str = "outbox";
/// The retire record (`account-sync-plane.md` § The bind leg, ruling 5).
/// Recreatable: derived from retires already sent; a lost entry delays its
/// retire at a secondary or leaves a redundant row there (ruling 6).
const RETIRES: &str = "issued_retires";

/// Every store a scope departure reaches (the group plane is not one — the
/// SQLite arm's departure leaves `group_entries` alone, and the conformance
/// suite grades both against one statement).
const SCOPE_PLANES: &[&str] = &[
    META,
    JOURNAL,
    STATE,
    FRONTIERS,
    RECORDS,
    BLOCKS,
    SEGMENTS,
    SEGMENT_BLOCKS,
    RELAY,
];

/// `(store, keyPath, [(index, keyPath, unique)])` — the genesis schema. Key
/// paths name value properties; a missing property leaves the record out of
/// that index (how `relay_rows.gen` stays a partial index, as in SQLite).
type IndexSpec = (&'static str, &'static [&'static str], bool);
const SCHEMA: &[(&str, &[&str], &[IndexSpec])] = &[
    (META, &["k"], &[]),
    (
        JOURNAL,
        &["scope", "w", "seq"],
        &[
            // The per-writer append counter's read path (`max_writer_seq`).
            ("writer", &["w", "seq"], false),
            // `coordinate_of_item`: item → its introducing (seq, writer).
            ("item", &["scope", "item", "seq", "w"], false),
        ],
    ),
    (STATE, &["kind", "key"], &[("scope", &["scope"], false)]),
    (GROUP, &["scope", "kind", "key"], &[]),
    (FRONTIERS, &["scope", "w"], &[]),
    (RECORDS, &["cid"], &[("scope", &["scope", "cid"], false)]),
    (BLOCKS, &["cid"], &[]),
    (SEGMENTS, &["scope", "kind", "id"], &[]),
    (
        SEGMENT_BLOCKS,
        &["cid", "scope", "kind", "id"],
        &[("scope", &["scope"], false)],
    ),
    (
        RELAY,
        &["scope", "w", "item"],
        &[
            ("walk", &["scope", "cls", "w", "seq"], false),
            ("item", &["scope", "cls", "item"], false),
            ("gen", &["scope", "gen"], false),
            ("coord", &["scope", "w", "seq"], false),
        ],
    ),
    (OUTBOX, &["id"], &[("fifo", &["scope", "cseq"], true)]),
    (
        RETIRES,
        &["scope", "w", "item", "seq"],
        // The order token: rising with every put, so the newest is last.
        &[("ord", &["ord"], true)],
    ),
];

/// Web's store backend: one IndexedDB database + one OPFS segment directory.
pub struct IndexedDbBackend {
    db: IdbDatabase,
    /// The database name — also, folded to one path component, the OPFS
    /// segment directory's name.
    name: String,
}

impl std::fmt::Debug for IndexedDbBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IndexedDbBackend")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl IndexedDbBackend {
    /// Open (or create) the store named `name` in this origin.
    ///
    /// The schema upgrade is IndexedDB's own `versionchange` transaction,
    /// which the browser runs exclusively across every connection of the
    /// origin — the web arm's migration critical section needs no separate
    /// lock (the native arm's `MigrationLock` exists because SQLite has no
    /// such thing). A connection closes itself when a later schema version
    /// asks (`onversionchange`), so a newer tab's upgrade never waits forever
    /// on an older one; the older handle's next call then fails loudly.
    ///
    /// Replays any scope departure whose file sweep a closed tab left owed,
    /// and sweeps any segment staging files one left. Unlike the native arm,
    /// the sweep cannot tell a live tab's transfer from a dead one's (OPFS
    /// files carry no lock this arm can probe), so a tab opening while
    /// another stages a segment removes that slot: the stager's adoption then
    /// fails at its rename and its next pass re-fetches — lossless, as the
    /// row half is only written after the rename.
    pub async fn open(name: &str) -> Result<Self> {
        Self::open_with(name, false)
            .await?
            .ok_or_else(|| anyhow!("account store {name:?}: open refused a fresh database"))
    }

    /// [`Self::open`] for a store that may not exist: `None` when this origin
    /// holds no database named `name`, and none is created. The reader that
    /// must never mint a store is the pre-login local read
    /// (`nest/box-recovery.md` § The plane-era recovery floor, *(b)*).
    ///
    /// Atomic, not check-then-open: the open's own `versionchange` sees the
    /// stored version, and a database that did not exist (version 0) has its
    /// genesis transaction aborted, which fails the open and leaves no
    /// database behind. An existing store opens exactly as [`Self::open`]
    /// opens it.
    pub async fn open_existing(name: &str) -> Result<Option<Self>> {
        Self::open_with(name, true).await
    }

    /// The open both doors share; `must_exist` aborts a genesis.
    async fn open_with(name: &str, must_exist: bool) -> Result<Option<Self>> {
        let factory = idb_factory()?;
        let request: IdbOpenDbRequest = factory
            .open_with_u32(name, DB_VERSION)
            .map_err(|e| js_error("indexedDB.open", e))?;
        let refused_genesis = std::rc::Rc::new(std::cell::Cell::new(false));
        let upgrade = Closure::once({
            let refused_genesis = refused_genesis.clone();
            move |event: web_sys::IdbVersionChangeEvent| {
                let Some(request) = event
                    .target()
                    .and_then(|t| t.dyn_into::<IdbOpenDbRequest>().ok())
                else {
                    return;
                };
                // Version 0 is "no such database": the open is about to create
                // it. Aborting the versionchange transaction fails the open and
                // discards the database it would have created.
                if must_exist && event.old_version() == 0.0 {
                    refused_genesis.set(true);
                    if let Some(tx) = request.transaction() {
                        let _ = tx.abort();
                    }
                    return;
                }
                upgrade_or_abort(&request);
            }
        });
        request.set_onupgradeneeded(Some(upgrade.as_ref().unchecked_ref()));
        let opened = settle(&request).await;
        drop(upgrade);
        if refused_genesis.get() {
            return Ok(None);
        }
        let db: IdbDatabase = opened
            .with_context(|| format!("open account store {name:?}"))?
            .dyn_into()
            .map_err(|e| js_error("open result", e))?;
        let on_version_change = Closure::<dyn FnMut()>::new({
            let db = db.clone();
            move || db.close()
        });
        db.set_onversionchange(Some(on_version_change.as_ref().unchecked_ref()));
        // Lives as long as the page: the handler must outlive every later
        // version change, and a connection is opened once per store handle.
        on_version_change.forget();

        let backend = Self {
            db,
            name: name.to_owned(),
        };
        backend
            .resume_pending_scope_drops()
            .await
            .context("account store: resume pending scope drops")?;
        // A segment transfer a closed tab left mid-way leaves only staging
        // files (adoption renames before it writes a row).
        opfs::sweep_prefixed(&backend.segment_dir(), STAGING_PREFIX).await;
        Ok(Some(backend))
    }

    /// The name this store was opened under.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Delete the store named `name` — its database and its segment
    /// directory. Idempotent: a store that is not there deletes cleanly.
    ///
    /// **A delete another connection blocks is an error, not a wait.** A
    /// connection this module opened closes itself when the delete asks
    /// (`onversionchange`, [`Self::open`]); one that does not — another
    /// build's, a tab that is not answering — makes the browser fire `blocked`
    /// and leave the request waiting for as long as that connection lives.
    /// This answers `Err` on `blocked`, so the caller can keep the store on
    /// its books and tell the user (`apps/account-scoping.md` § The scoping
    /// taxonomy → *Erasure follows scope*, the web paragraph's decision 4).
    /// ⚠ The browser offers no way to withdraw the request: it stays queued
    /// and runs when the last holder closes, and an open of the same name
    /// queues behind it until then.
    ///
    /// **The directory goes only once the database has.** A store the delete
    /// left behind is left whole — rows and the segment files they point at —
    /// because a later sign-in may adopt it again.
    pub async fn delete(name: &str) -> Result<()> {
        let request = idb_factory()?
            .delete_database(name)
            .map_err(|e| js_error("indexedDB.deleteDatabase", e))?;
        settle_unblocked(&request)
            .await
            .with_context(|| format!("delete account store {name:?}"))?;
        opfs::remove_dir(&sanitize_component(name)).await
    }

    /// Run `body` inside ONE transaction over `stores`, in a task of its own
    /// (module docs, *Transactions*) — the only door to a transaction here.
    /// Commits and waits for the commit on `Ok`; on `Err`, ABORTS — IndexedDB
    /// would otherwise commit the requests that had already landed, and
    /// every method here is all-or-nothing.
    async fn transact<T, Body, Fut>(
        &self,
        stores: &'static [&'static str],
        mode: IdbTransactionMode,
        body: Body,
    ) -> Result<T>
    where
        T: 'static,
        Body: FnOnce(Tx) -> Fut + 'static,
        Fut: Future<Output = Result<T>> + 'static,
    {
        let db = self.db.clone();
        own_task(async move {
            let names = Array::new();
            for s in stores {
                names.push(&JsValue::from_str(s));
            }
            let tx = db
                .transaction_with_str_sequence_and_mode(&names, mode)
                .map_err(|e| js_error("begin transaction", e))?;
            let done = JsFuture::from(Promise::new(&mut |resolve, reject| {
                tx.set_oncomplete(Some(&resolve));
                // A failed request's error event bubbles here before the abort.
                tx.set_onerror(Some(&reject));
                tx.set_onabort(Some(&reject));
            }));
            match body(Tx(tx.clone())).await {
                Ok(v) => {
                    done.await
                        .map_err(|e| js_error("transaction did not commit", e))?;
                    Ok(v)
                }
                Err(e) => {
                    // Already finished (the failing request aborted it) is fine.
                    let _ = tx.abort();
                    let _ = done.await;
                    Err(e)
                }
            }
        })
        .await
    }

    async fn read<T, Body, Fut>(&self, stores: &'static [&'static str], body: Body) -> Result<T>
    where
        T: 'static,
        Body: FnOnce(Tx) -> Fut + 'static,
        Fut: Future<Output = Result<T>> + 'static,
    {
        self.transact(stores, IdbTransactionMode::Readonly, body)
            .await
    }

    async fn write<T, Body, Fut>(&self, stores: &'static [&'static str], body: Body) -> Result<T>
    where
        T: 'static,
        Body: FnOnce(Tx) -> Fut + 'static,
        Fut: Future<Output = Result<T>> + 'static,
    {
        self.transact(stores, IdbTransactionMode::Readwrite, body)
            .await
    }

    fn segment_dir(&self) -> String {
        sanitize_component(&self.name)
    }

    async fn resume_pending_scope_drops(&self) -> Result<()> {
        let pending = self
            .read(&[META], |t| async move {
                meta_get(&t.store(META)?, META_SCOPE_DROPS_PENDING).await
            })
            .await?;
        let Some(raw) = pending else {
            return Ok(());
        };
        for scope in parse_pending_drops(&raw) {
            opfs::remove_prefixed(&self.segment_dir(), &scope_file_prefix(&scope)).await?;
            self.clear_pending_scope_drop(&scope).await?;
        }
        Ok(())
    }

    async fn clear_pending_scope_drop(&self, scope: &str) -> Result<()> {
        let scope = scope.to_owned();
        self.write(&[META], move |t| async move {
            let scope = scope.as_str();
            let meta = t.store(META)?;
            let rest: Vec<String> = match meta_get(&meta, META_SCOPE_DROPS_PENDING).await? {
                Some(raw) => parse_pending_drops(&raw)
                    .into_iter()
                    .filter(|s| s != scope)
                    .collect(),
                None => return Ok(()),
            };
            if rest.is_empty() {
                delete(&meta, &JsValue::from_str(META_SCOPE_DROPS_PENDING)).await
            } else {
                meta_put(&meta, META_SCOPE_DROPS_PENDING, rest.join("\n").as_bytes()).await
            }
        })
        .await
    }

    /// The row half of a scope departure: one transaction over every
    /// scope-keyed store, the pending-drop mark written INSIDE it, so "rows
    /// gone" and "files owed" commit together — the property the resume at
    /// [`Self::open`] leans on.
    async fn drop_scope_rows(&self, scope: &str) -> Result<ScopeDropCounts> {
        let scope = scope.to_owned();
        self.write(SCOPE_PLANES, move |t| async move {
            let scope = scope.as_str();
            let s = JsValue::from_str(scope);
            let mut counts = ScopeDropCounts::default();

            // Loose block bytes go with their index rows; segment-resident
            // bytes leave with the segment files after the commit.
            let (records, blocks) = (t.store(RECORDS)?, t.store(BLOCKS)?);
            let held = get_all(
                &t.index(RECORDS, "scope")?,
                &prefix(std::slice::from_ref(&s)),
                None,
            )
            .await?;
            for o in &held {
                let cid = req(o, "cid")?;
                if get(&blocks, &cid).await?.is_some() {
                    delete(&blocks, &cid).await?;
                    counts.blocks += 1;
                }
                delete(&records, &cid).await?;
            }
            counts.record_index_rows = held.len() as u64;

            let whole = prefix(std::slice::from_ref(&s));
            for (store, count) in [
                (JOURNAL, &mut counts.journal_rows),
                (FRONTIERS, &mut counts.frontier_rows),
                (RELAY, &mut counts.relay_rows),
                (SEGMENTS, &mut counts.segments),
            ] {
                let store = t.store(store)?;
                *count = get_all_keys(&store, &whole).await?.len() as u64;
                delete(&store, &whole).await?;
            }
            // Entries *of* the departed scope — the seen-set entry FOR it
            // lives in the account-state scope and is untouched.
            let state = t.store(STATE)?;
            let entries = get_all_keys(
                &t.index(STATE, "scope")?,
                &IdbKeyRange::only(&s)
                    .map_err(|e| js_error("range", e))?
                    .into(),
            )
            .await?;
            for k in &entries {
                delete(&state, k).await?;
            }
            counts.state_entries = entries.len() as u64;
            // The routing mirror carries no count of its own.
            let routes = t.store(SEGMENT_BLOCKS)?;
            for k in get_all_keys(
                &t.index(SEGMENT_BLOCKS, "scope")?,
                &IdbKeyRange::only(&s)
                    .map_err(|e| js_error("range", e))?
                    .into(),
            )
            .await?
            {
                delete(&routes, &k).await?;
            }

            let meta = t.store(META)?;
            delete(&meta, &JsValue::from_str(&nest_watermark_key(scope))).await?;
            if counts.segments > 0 {
                let mut pending = meta_get(&meta, META_SCOPE_DROPS_PENDING)
                    .await?
                    .map(|raw| parse_pending_drops(&raw))
                    .unwrap_or_default();
                if !pending.iter().any(|p| p == scope) {
                    pending.push(scope.to_owned());
                }
                meta_put(
                    &meta,
                    META_SCOPE_DROPS_PENDING,
                    pending.join("\n").as_bytes(),
                )
                .await?;
            }
            Ok(counts)
        })
        .await
    }

    /// The row half of a departure **alone** — the exact state a tab closed
    /// between the commit and the file sweep leaves behind. Test-only:
    /// production always continues into the sweep, and the only way to prove
    /// the resume works is to stop there deliberately (the SQLite arm's
    /// `drop_scope_rows_for_test`).
    #[cfg(feature = "test-helpers")]
    pub async fn drop_scope_rows_for_test(&self, scope: &str) -> Result<ScopeDropCounts> {
        self.drop_scope_rows(scope).await
    }

    /// [`Self::segment_files_for_test`] for the store named `name`, WITHOUT
    /// opening it — an open sweeps staging files, so observing a crash's
    /// leftovers through an opened handle would erase what it looks for.
    #[cfg(feature = "test-helpers")]
    pub async fn segment_files_of_unopened_for_test(name: &str) -> Result<Vec<String>> {
        opfs::list(&sanitize_component(name)).await
    }

    /// The file names in this store's OPFS segment directory (none when it
    /// does not exist) — what a test observes the segment area through.
    #[cfg(feature = "test-helpers")]
    pub async fn segment_files_for_test(&self) -> Result<Vec<String>> {
        opfs::list(&self.segment_dir()).await
    }
}

/// Create every object store and index (the `versionchange` transaction's
/// body). Idempotent per store, so a later additive version can run it whole.
fn genesis(request: &IdbOpenDbRequest) -> Result<()> {
    let db: IdbDatabase = request
        .result()
        .map_err(|e| js_error("upgrade result", e))?
        .dyn_into()
        .map_err(|e| js_error("upgrade result", e))?;
    let tx = request
        .transaction()
        .context("an upgrade runs inside a versionchange transaction")?;
    let existing = db.object_store_names();
    for (name, key_path, indexes) in SCHEMA {
        let store = if existing.contains(name) {
            tx.object_store(name)
                .map_err(|e| js_error("upgrade object store", e))?
        } else {
            let params = IdbObjectStoreParameters::new();
            params.set_key_path(&key_path_value(key_path));
            db.create_object_store_with_optional_parameters(name, &params)
                .map_err(|e| js_error("create object store", e))?
        };
        let held = store.index_names();
        for (index, path, unique) in *indexes {
            if held.contains(index) {
                continue;
            }
            let params = IdbIndexParameters::new();
            params.set_unique(*unique);
            store
                .create_index_with_str_sequence_and_optional_parameters(
                    index,
                    &key_path_value(path),
                    &params,
                )
                .map_err(|e| js_error("create index", e))?;
        }
    }
    Ok(())
}

/// A one-element key path is the property name itself (a scalar key); a
/// longer one is an array key path (an array key).
fn key_path_value(path: &[&str]) -> JsValue {
    match path {
        [one] => JsValue::from_str(one),
        many => many
            .iter()
            .map(|p| JsValue::from_str(p))
            .collect::<Array>()
            .into(),
    }
}

// ── Transactions and requests ─────────────────────────────────────────────────

/// One open IndexedDB transaction, as its body sees it
/// ([`IndexedDbBackend::transact`] opens and closes it).
struct Tx(IdbTransaction);

impl Tx {
    fn store(&self, name: &str) -> Result<IdbObjectStore> {
        self.0
            .object_store(name)
            .map_err(|e| js_error("object store", e))
    }

    fn index(&self, store: &str, index: &str) -> Result<IdbIndex> {
        self.store(store)?
            .index(index)
            .map_err(|e| js_error("index", e))
    }
}

/// Run `work` to completion as a `spawn_local` task of its own and hand back
/// its output (module docs, *Transactions*). The task is polled by the
/// browser's microtask queue the moment one of its requests answers, whatever
/// the caller is doing; a caller that is dropped while it waits leaves the
/// task running to its commit, so a cancelled caller never leaves half a
/// method behind either.
async fn own_task<T: 'static>(work: impl Future<Output = T> + 'static) -> T {
    struct Handoff<T> {
        output: Option<T>,
        waiter: Option<Waker>,
    }
    let handoff = Rc::new(RefCell::new(Handoff {
        output: None,
        waiter: None,
    }));
    wasm_bindgen_futures::spawn_local({
        let handoff = Rc::clone(&handoff);
        async move {
            let output = work.await;
            let waiter = {
                let mut handoff = handoff.borrow_mut();
                handoff.output = Some(output);
                handoff.waiter.take()
            };
            if let Some(waiter) = waiter {
                waiter.wake();
            }
        }
    });
    std::future::poll_fn(move |cx| {
        let mut handoff = handoff.borrow_mut();
        match handoff.output.take() {
            Some(output) => Poll::Ready(output),
            None => {
                handoff.waiter = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    })
    .await
}

/// Await one request's success; its result, or its error as an `anyhow`.
async fn settle(request: &IdbRequest) -> Result<JsValue> {
    let outcome = JsFuture::from(Promise::new(&mut |resolve, reject| {
        request.set_onsuccess(Some(&resolve));
        request.set_onerror(Some(&reject));
    }))
    .await;
    if outcome.is_err() {
        let err = request
            .error()
            .ok()
            .flatten()
            .map(|e| format!("{}: {}", e.name(), e.message()))
            .unwrap_or_else(|| "request failed".to_owned());
        bail!("IndexedDB: {err}");
    }
    request.result().map_err(|e| js_error("request result", e))
}

/// [`settle`] for a delete request, which can also be answered `blocked`: a
/// connection that did not close for it is holding the database. `blocked` is
/// an `Err` here; the request itself stays queued in the browser.
async fn settle_unblocked(request: &IdbOpenDbRequest) -> Result<JsValue> {
    let outcome = JsFuture::from(Promise::new(&mut |resolve, reject| {
        request.set_onsuccess(Some(&resolve));
        request.set_onerror(Some(&reject));
        request.set_onblocked(Some(&reject));
    }))
    .await;
    match outcome {
        Ok(_) => request.result().map_err(|e| js_error("request result", e)),
        Err(event) => {
            let blocked = event
                .dyn_ref::<web_sys::Event>()
                .is_some_and(|e| e.type_() == "blocked");
            if blocked {
                bail!("IndexedDB: blocked — another connection holds the database open");
            }
            let err = request
                .error()
                .ok()
                .flatten()
                .map(|e| format!("{}: {}", e.name(), e.message()))
                .unwrap_or_else(|| "request failed".to_owned());
            bail!("IndexedDB: {err}");
        }
    }
}

fn js_error(what: &str, e: JsValue) -> anyhow::Error {
    match e.dyn_ref::<DomException>() {
        Some(d) => anyhow!("IndexedDB {what}: {}: {}", d.name(), d.message()),
        None => anyhow!("IndexedDB {what}: {e:?}"),
    }
}

/// The schema upgrade an open's `versionchange` runs. Aborting the
/// versionchange transaction fails the open request, which is how a genesis
/// error reaches the opener.
fn upgrade_or_abort(request: &IdbOpenDbRequest) {
    if let Err(e) = genesis(request) {
        tracing::error!(error = %e, "account store: IndexedDB genesis failed");
        if let Some(tx) = request.transaction() {
            let _ = tx.abort();
        }
    }
}

fn idb_factory() -> Result<IdbFactory> {
    Reflect::get(&js_sys::global(), &JsValue::from_str("indexedDB"))
        .ok()
        .filter(|f| !f.is_undefined() && !f.is_null())
        .context("IndexedDB is unavailable in this context")?
        .dyn_into()
        .map_err(|e| js_error("indexedDB", e))
}

/// The read side both a store and an index offer.
trait Source {
    fn get_req(&self, key: &JsValue) -> Result<IdbRequest, JsValue>;
    fn get_all_req(&self, range: &JsValue, limit: Option<u32>) -> Result<IdbRequest, JsValue>;
    fn get_all_keys_req(&self, range: &JsValue) -> Result<IdbRequest, JsValue>;
    fn cursor_req(&self, range: &JsValue, dir: IdbCursorDirection) -> Result<IdbRequest, JsValue>;
}

impl Source for IdbObjectStore {
    fn get_req(&self, key: &JsValue) -> Result<IdbRequest, JsValue> {
        self.get(key)
    }
    fn get_all_req(&self, range: &JsValue, limit: Option<u32>) -> Result<IdbRequest, JsValue> {
        match limit {
            Some(n) => self.get_all_with_key_and_limit(range, n),
            None => self.get_all_with_key(range),
        }
    }
    fn get_all_keys_req(&self, range: &JsValue) -> Result<IdbRequest, JsValue> {
        self.get_all_keys_with_key(range)
    }
    fn cursor_req(&self, range: &JsValue, dir: IdbCursorDirection) -> Result<IdbRequest, JsValue> {
        self.open_cursor_with_range_and_direction(range, dir)
    }
}

impl Source for IdbIndex {
    fn get_req(&self, key: &JsValue) -> Result<IdbRequest, JsValue> {
        self.get(key)
    }
    fn get_all_req(&self, range: &JsValue, limit: Option<u32>) -> Result<IdbRequest, JsValue> {
        match limit {
            Some(n) => self.get_all_with_key_and_limit(range, n),
            None => self.get_all_with_key(range),
        }
    }
    fn get_all_keys_req(&self, range: &JsValue) -> Result<IdbRequest, JsValue> {
        self.get_all_keys_with_key(range)
    }
    fn cursor_req(&self, range: &JsValue, dir: IdbCursorDirection) -> Result<IdbRequest, JsValue> {
        self.open_cursor_with_range_and_direction(range, dir)
    }
}

/// The record at `key` (an exact key; on an index, the first match).
async fn get(src: &impl Source, key: &JsValue) -> Result<Option<JsValue>> {
    let v = settle(&src.get_req(key).map_err(|e| js_error("get", e))?).await?;
    Ok((!v.is_undefined()).then_some(v))
}

/// Every record in `range` (a key or an `IDBKeyRange`), in key order.
async fn get_all(src: &impl Source, range: &JsValue, limit: Option<u32>) -> Result<Vec<JsValue>> {
    let v = settle(
        &src.get_all_req(range, limit)
            .map_err(|e| js_error("getAll", e))?,
    )
    .await?;
    Ok(v.unchecked_into::<Array>().to_vec())
}

/// Every primary key in `range`.
async fn get_all_keys(src: &impl Source, range: &JsValue) -> Result<Vec<JsValue>> {
    let v = settle(
        &src.get_all_keys_req(range)
            .map_err(|e| js_error("getAllKeys", e))?,
    )
    .await?;
    Ok(v.unchecked_into::<Array>().to_vec())
}

/// The first record in `range` walking `dir` — `Prev` for a maximum.
async fn first(
    src: &impl Source,
    range: &JsValue,
    dir: IdbCursorDirection,
) -> Result<Option<JsValue>> {
    let cursor = settle(
        &src.cursor_req(range, dir)
            .map_err(|e| js_error("openCursor", e))?,
    )
    .await?;
    if cursor.is_null() {
        return Ok(None);
    }
    let cursor: IdbCursorWithValue = cursor.unchecked_into();
    Ok(Some(
        cursor.value().map_err(|e| js_error("cursor value", e))?,
    ))
}

async fn put(store: &IdbObjectStore, value: &Object) -> Result<()> {
    settle(&store.put(value).map_err(|e| js_error("put", e))?).await?;
    Ok(())
}

/// Delete one key, or every key in an `IDBKeyRange`.
async fn delete(store: &IdbObjectStore, key: &JsValue) -> Result<()> {
    settle(&store.delete(key).map_err(|e| js_error("delete", e))?).await?;
    Ok(())
}

// ── Keys and values ──────────────────────────────────────────────────────────

fn bin(bytes: &[u8]) -> JsValue {
    Uint8Array::from(bytes).into()
}

/// A `u64` as an order-preserving binary key component (module docs).
fn be(v: u64) -> JsValue {
    bin(&v.to_be_bytes())
}

fn key(parts: &[JsValue]) -> JsValue {
    parts.iter().collect::<Array>().into()
}

/// Every array key that starts with `prefix`: `[prefix…]` sorts below any
/// longer key sharing it, and `[prefix…, []]` above every one whose next
/// component is a scalar (an array sorts after every non-array key) — and no
/// key here carries an array component.
fn prefix(parts: &[JsValue]) -> JsValue {
    let upper: Array = parts.iter().collect();
    upper.push(&Array::new());
    IdbKeyRange::bound(&key(parts), &upper)
        .expect("a prefix's lower key sorts below its upper key")
        .into()
}

/// Keys strictly after `after` that still start with `parts`.
fn prefix_after(parts: &[JsValue], after: &JsValue) -> JsValue {
    let lower: Array = parts.iter().collect();
    lower.push(after);
    let upper: Array = parts.iter().collect();
    upper.push(&Array::new());
    IdbKeyRange::bound_with_lower_open_and_upper_open(&lower, &upper, true, false)
        .expect("a lower key inside a prefix sorts below its upper key")
        .into()
}

fn only(parts: &[JsValue]) -> JsValue {
    IdbKeyRange::only(&key(parts))
        .expect("an array key is a valid key")
        .into()
}

/// A value object from `(property, value)` pairs; `None` values are left out,
/// the absent property reading back as `None` (the nullable column).
fn record(fields: &[(&str, Option<JsValue>)]) -> Object {
    let o = Object::new();
    for (name, value) in fields {
        if let Some(v) = value {
            Reflect::set(&o, &JsValue::from_str(name), v).expect("setting a plain object's field");
        }
    }
    o
}

fn field(o: &JsValue, name: &str) -> Result<Option<JsValue>> {
    let v = Reflect::get(o, &JsValue::from_str(name)).map_err(|e| js_error("read field", e))?;
    Ok((!v.is_undefined() && !v.is_null()).then_some(v))
}

fn req(o: &JsValue, name: &str) -> Result<JsValue> {
    field(o, name)?.with_context(|| format!("stored record lacks {name:?}"))
}

fn str_of(o: &JsValue, name: &str) -> Result<String> {
    req(o, name)?
        .as_string()
        .with_context(|| format!("stored {name:?} is not a string"))
}

fn bytes_of(v: &JsValue) -> Vec<u8> {
    Uint8Array::new(v).to_vec()
}

fn bin_of(o: &JsValue, name: &str) -> Result<Vec<u8>> {
    Ok(bytes_of(&req(o, name)?))
}

fn opt_bin_of(o: &JsValue, name: &str) -> Result<Option<Vec<u8>>> {
    Ok(field(o, name)?.map(|v| bytes_of(&v)))
}

fn arr_of<const N: usize>(o: &JsValue, name: &str) -> Result<[u8; N]> {
    bin_of(o, name)?
        .try_into()
        .map_err(|v: Vec<u8>| anyhow!("stored {name:?} is {} bytes, not {N}", v.len()))
}

fn be_of(o: &JsValue, name: &str) -> Result<u64> {
    Ok(u64::from_be_bytes(arr_of::<8>(o, name)?))
}

fn opt_be_of(o: &JsValue, name: &str) -> Result<Option<u64>> {
    opt_bin_of(o, name)?
        .map(|b| {
            <[u8; 8]>::try_from(b.as_slice())
                .map(u64::from_be_bytes)
                .map_err(|_| anyhow!("stored {name:?} is not 8 bytes"))
        })
        .transpose()
}

fn num_of(o: &JsValue, name: &str) -> Result<f64> {
    req(o, name)?
        .as_f64()
        .with_context(|| format!("stored {name:?} is not a number"))
}

fn writer_of(o: &JsValue) -> Result<WriterId> {
    Ok(WriterId(arr_of::<32>(o, "w")?))
}

fn cid_of(v: &JsValue) -> Result<ContentHash> {
    let bytes: [u8; 36] = bytes_of(v)
        .try_into()
        .map_err(|_| anyhow!("stored cid is not 36 bytes"))?;
    ContentHash::from_bytes(bytes).context("stored cid")
}

// ── Per-table codecs ─────────────────────────────────────────────────────────

async fn meta_get(meta: &IdbObjectStore, k: &str) -> Result<Option<Vec<u8>>> {
    get(meta, &JsValue::from_str(k))
        .await?
        .map(|o| bin_of(&o, "v"))
        .transpose()
}

async fn meta_put(meta: &IdbObjectStore, k: &str, v: &[u8]) -> Result<()> {
    put(
        meta,
        &record(&[("k", Some(JsValue::from_str(k))), ("v", Some(bin(v)))]),
    )
    .await
}

/// The rising-only upsert (`StoreBackend::meta_put_pair_max`'s per-key law).
async fn meta_put_max(meta: &IdbObjectStore, k: &str, value: u64) -> Result<()> {
    fits_i64(value, "rising-only meta value")?;
    let rises = meta_get(meta, k)
        .await?
        .is_none_or(|held| rising_meta_integer(&held) < value);
    if rises {
        meta_put(meta, k, value.to_string().as_bytes()).await?;
    }
    Ok(())
}

/// The append-time writer guard, inside the caller's transaction (which must
/// span [`META`]).
async fn writer_guard(meta: &IdbObjectStore, local_writer: Option<&WriterId>) -> Result<()> {
    let Some(held) = local_writer else {
        return Ok(());
    };
    match meta_get(meta, crate::store::META_WRITER_ID).await? {
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

fn journal_key(scope: &str, writer: &WriterId, seq: u64) -> JsValue {
    key(&[JsValue::from_str(scope), bin(&writer.0), be(seq)])
}

/// Insert one journal row unless its slot is taken; report the occupant.
async fn insert_row(journal: &IdbObjectStore, row: &JournalRow) -> Result<InsertOutcome> {
    fits_i64(row.seq, "writer_seq")?;
    let item = row.item.encode();
    if let Some(held) = get(journal, &journal_key(&row.scope, &row.writer, row.seq)).await? {
        let identical = str_of(&held, "op")? == row.op.as_str() && bin_of(&held, "item")? == item;
        return Ok(if identical {
            InsertOutcome::IdenticalPresent
        } else {
            InsertOutcome::OccupiedByDifferent
        });
    }
    put(
        journal,
        &record(&[
            ("scope", Some(JsValue::from_str(&row.scope))),
            ("w", Some(bin(&row.writer.0))),
            ("seq", Some(be(row.seq))),
            ("op", Some(JsValue::from_str(row.op.as_str()))),
            ("item", Some(bin(&item))),
        ]),
    )
    .await?;
    Ok(InsertOutcome::Inserted)
}

fn journal_row(o: &JsValue) -> Result<JournalRow> {
    Ok(JournalRow {
        writer: writer_of(o)?,
        seq: be_of(o, "seq")?,
        scope: str_of(o, "scope")?,
        op: JournalOp::parse(&str_of(o, "op")?)?,
        item: ItemRef::decode(&bin_of(o, "item")?)?,
    })
}

fn state_record(entry: &StateEntry) -> Result<Object> {
    fits_i64(entry.entry_version, "entry_version")?;
    Ok(record(&[
        ("kind", Some(JsValue::from_str(&entry.kind))),
        ("key", Some(JsValue::from_str(&entry.key))),
        ("scope", Some(JsValue::from_str(&entry.scope))),
        ("value", Some(bin(&entry.value))),
        ("mm", entry.merge_meta.as_deref().map(bin)),
        ("ver", Some(be(entry.entry_version))),
        ("tomb", Some(JsValue::from_bool(entry.tombstone))),
    ]))
}

fn state_entry(o: &JsValue) -> Result<StateEntry> {
    Ok(StateEntry {
        kind: str_of(o, "kind")?,
        key: str_of(o, "key")?,
        scope: str_of(o, "scope")?,
        value: bin_of(o, "value")?,
        merge_meta: opt_bin_of(o, "mm")?,
        entry_version: be_of(o, "ver")?,
        tombstone: req(o, "tomb")?.as_bool().unwrap_or(false),
    })
}

fn relay_record(row: &RelayRow) -> Result<Object> {
    fits_i64(row.writer_seq, "relay writer_seq")?;
    if let Some(f) = row.feed_seq {
        fits_i64(f, "relay feed_seq")?;
    }
    let generation = row
        .entry
        .as_deref()
        .and_then(fauna_core::account_entry_crypto::peek_generation_id);
    Ok(record(&[
        ("scope", Some(JsValue::from_str(&row.scope))),
        ("cls", Some(JsValue::from_str(&row.item_class))),
        ("w", Some(bin(&row.writer.0))),
        ("seq", Some(be(row.writer_seq))),
        ("item", Some(bin(&row.item_key))),
        ("op", Some(JsValue::from_str(&row.op))),
        ("entry", row.entry.as_deref().map(bin)),
        ("feed", row.feed_seq.map(be)),
        ("gen", generation.as_ref().map(|g| bin(g))),
    ]))
}

fn relay_row(o: &JsValue) -> Result<RelayRow> {
    Ok(RelayRow {
        scope: str_of(o, "scope")?,
        item_class: str_of(o, "cls")?,
        writer: writer_of(o)?,
        writer_seq: be_of(o, "seq")?,
        item_key: bin_of(o, "item")?,
        op: str_of(o, "op")?,
        entry: opt_bin_of(o, "entry")?,
        feed_seq: opt_be_of(o, "feed")?,
    })
}

fn relay_key(scope: &str, writer: &WriterId, item_key: &[u8]) -> JsValue {
    key(&[JsValue::from_str(scope), bin(&writer.0), bin(item_key)])
}

fn issued_retire_record(retire: &IssuedRetire, ord: u64) -> Result<Object> {
    fits_i64(retire.writer_seq, "retire writer_seq")?;
    Ok(record(&[
        ("scope", Some(JsValue::from_str(&retire.scope))),
        ("w", Some(bin(&retire.writer.0))),
        ("item", Some(bin(&retire.item_key))),
        ("seq", Some(be(retire.writer_seq))),
        ("ord", Some(be(ord))),
        ("belt", retire.no_rows_sealed_under.as_ref().map(|g| bin(g))),
        (
            "sweep",
            Some(JsValue::from_bool(retire.delete_escrow_wraps)),
        ),
        ("settled", Some(JsValue::from_bool(retire.settled))),
    ]))
}

fn issued_retire(o: &JsValue) -> Result<(u64, IssuedRetire)> {
    let flag = |name: &str| -> Result<bool> {
        req(o, name)?
            .as_bool()
            .with_context(|| format!("stored {name:?} is not a boolean"))
    };
    Ok((
        be_of(o, "ord")?,
        IssuedRetire {
            scope: str_of(o, "scope")?,
            writer: writer_of(o)?,
            item_key: arr_of::<32>(o, "item")?,
            writer_seq: be_of(o, "seq")?,
            no_rows_sealed_under: match field(o, "belt")? {
                Some(_) => Some(arr_of::<32>(o, "belt")?),
                None => None,
            },
            delete_escrow_wraps: flag("sweep")?,
            settled: flag("settled")?,
        },
    ))
}

fn issued_retire_key(retire: &IssuedRetire) -> JsValue {
    key(&[
        JsValue::from_str(&retire.scope),
        bin(&retire.writer.0),
        bin(&retire.item_key),
        be(retire.writer_seq),
    ])
}

/// Every key of the retire record's `ord` index at or below `through`.
fn ord_through(through: u64) -> Result<JsValue> {
    Ok(IdbKeyRange::upper_bound(&be(through))
        .map_err(|e| js_error("range", e))?
        .into())
}

/// Every relay row at `(scope, writer, writer_seq)`, optionally sparing one
/// item — the SQLite arm's `delete_relay_rows_at`. Returns the rows deleted.
async fn delete_relay_rows_at(
    relay: &IdbObjectStore,
    scope: &str,
    writer: &WriterId,
    writer_seq: u64,
    keep_item_key: Option<&[u8]>,
) -> Result<u64> {
    let index = relay.index("coord").map_err(|e| js_error("index", e))?;
    let rows = get_all(
        &index,
        &only(&[JsValue::from_str(scope), bin(&writer.0), be(writer_seq)]),
        None,
    )
    .await?;
    let mut deleted = 0;
    for row in rows {
        let item = bin_of(&row, "item")?;
        if keep_item_key.is_some_and(|keep| keep == item.as_slice()) {
            continue;
        }
        delete(relay, &relay_key(scope, writer, &item)).await?;
        deleted += 1;
    }
    Ok(deleted)
}

fn record_index_record(entry: &RecordIndexEntry, size: Option<u64>) -> Object {
    record(&[
        ("cid", Some(bin(entry.cid.as_bytes()))),
        ("scope", Some(JsValue::from_str(&entry.scope))),
        ("kind", Some(JsValue::from_str(&entry.kind))),
        ("size", size.map(be)),
    ])
}

fn record_index_entry(o: &JsValue) -> Result<RecordIndexEntry> {
    Ok(RecordIndexEntry {
        cid: cid_of(&req(o, "cid")?)?,
        scope: str_of(o, "scope")?,
        kind: str_of(o, "kind")?,
        size: opt_be_of(o, "size")?,
    })
}

/// Upsert an index row, keeping a known size the new row does not carry
/// (`COALESCE(excluded.size, size)`).
async fn record_index_put(records: &IdbObjectStore, entry: &RecordIndexEntry) -> Result<()> {
    if let Some(size) = entry.size {
        fits_i64(size, "record size")?;
    }
    let size = match entry.size {
        Some(s) => Some(s),
        None => match get(records, &bin(entry.cid.as_bytes())).await? {
            Some(held) => opt_be_of(&held, "size")?,
            None => None,
        },
    };
    put(records, &record_index_record(entry, size)).await
}

/// Idempotent content-addressed insert (the key IS the hash of the value).
async fn block_put(blocks: &IdbObjectStore, cid: &ContentHash, bytes: &[u8]) -> Result<()> {
    let k = bin(cid.as_bytes());
    if get(blocks, &k).await?.is_none() {
        put(
            blocks,
            &record(&[("cid", Some(k)), ("bytes", Some(bin(bytes)))]),
        )
        .await?;
    }
    Ok(())
}

/// Which adopted segment holds `cid`, and the length it recorded.
async fn segment_route(
    segment_blocks: &IdbObjectStore,
    cid: &ContentHash,
) -> Result<Option<(SegmentKey, u64)>> {
    first(
        segment_blocks,
        &prefix(&[bin(cid.as_bytes())]),
        IdbCursorDirection::Next,
    )
    .await?
    .map(|o| {
        Ok((
            SegmentKey {
                scope: str_of(&o, "scope")?,
                kind: str_of(&o, "kind")?,
                segment_id: num_of(&o, "id")? as u32,
            },
            be_of(&o, "len")?,
        ))
    })
    .transpose()
}

fn segment_key_value(key: &SegmentKey) -> JsValue {
    self::key(&[
        JsValue::from_str(&key.scope),
        JsValue::from_str(&key.kind),
        JsValue::from_f64(f64::from(key.segment_id)),
    ])
}

fn outbox_intent(o: &JsValue) -> Result<OutboxIntent> {
    Ok(OutboxIntent {
        intent_id: arr_of::<16>(o, "id")?,
        kind: str_of(o, "kind")?,
        scope: str_of(o, "scope")?,
        payload: bin_of(o, "payload")?,
        drainer: IntentDrainer::parse(&str_of(o, "drainer")?)?,
        channel_seq: be_of(o, "cseq")?,
        status: IntentStatus::parse(&str_of(o, "status")?)?,
        retry_count: num_of(o, "retries")? as u32,
        created_at: be_of(o, "created")?,
        last_attempt_at: opt_be_of(o, "last")?,
    })
}

fn set_field(o: &JsValue, name: &str, v: &JsValue) -> Result<()> {
    Reflect::set(o, &JsValue::from_str(name), v).map_err(|e| js_error("set field", e))?;
    Ok(())
}

fn now_epoch_secs() -> u64 {
    u64::try_from(fauna_core::data::Timestamp::now_secs_or_zero()).unwrap_or(0)
}

/// Scopes named by a set of array primary keys whose first component is the
/// scope, distinct and ascending.
fn distinct_first_strings(keys: &[JsValue]) -> Vec<String> {
    let held: BTreeSet<String> = keys
        .iter()
        .filter_map(|k| k.dyn_ref::<Array>().and_then(|a| a.get(0).as_string()))
        .collect();
    held.into_iter().collect()
}

impl StoreBackend for IndexedDbBackend {
    type Staging = IndexedDbStaging;

    // `data_version`: the trait's default `None` (module docs).

    async fn meta_get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let key = key.to_owned();
        self.read(&[META], move |t| async move {
            meta_get(&t.store(META)?, &key).await
        })
        .await
    }

    async fn meta_get_all(&self, keys: &[&str]) -> Result<Vec<Option<Vec<u8>>>> {
        let keys: Vec<String> = keys.iter().map(|k| (*k).to_owned()).collect();
        self.read(&[META], move |t| async move {
            let meta = t.store(META)?;
            let mut out = Vec::with_capacity(keys.len());
            for k in &keys {
                out.push(meta_get(&meta, k).await?);
            }
            Ok(out)
        })
        .await
    }

    async fn meta_put(&self, key: &str, value: &[u8]) -> Result<()> {
        let (key, value) = (key.to_owned(), value.to_vec());
        self.write(&[META], move |t| async move {
            meta_put(&t.store(META)?, &key, &value).await
        })
        .await
    }

    async fn meta_put_all(&self, pairs: &[(&str, &[u8])]) -> Result<()> {
        let pairs: Vec<(String, Vec<u8>)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.to_vec()))
            .collect();
        self.write(&[META], move |t| async move {
            let meta = t.store(META)?;
            for (k, v) in &pairs {
                meta_put(&meta, k, v).await?;
            }
            Ok(())
        })
        .await
    }

    async fn meta_delete(&self, key: &str) -> Result<()> {
        let key = JsValue::from_str(key);
        self.write(&[META], move |t| async move {
            delete(&t.store(META)?, &key).await
        })
        .await
    }

    /// The compare and the deletes share one readwrite transaction — which
    /// IndexedDB serializes against every other readwrite transaction on
    /// `store_meta`, so no fence can land between them.
    async fn meta_delete_all_if_unchanged(
        &self,
        expected: &[(&str, Option<&[u8]>)],
    ) -> Result<bool> {
        let expected: Vec<(String, Option<Vec<u8>>)> = expected
            .iter()
            .map(|(k, want)| ((*k).to_owned(), want.map(<[u8]>::to_vec)))
            .collect();
        self.write(&[META], move |t| async move {
            let meta = t.store(META)?;
            for (k, want) in &expected {
                if meta_get(&meta, k).await? != *want {
                    return Ok(false);
                }
            }
            for (k, _) in &expected {
                delete(&meta, &JsValue::from_str(k)).await?;
            }
            Ok(true)
        })
        .await
    }

    async fn meta_put_pair_max(&self, first: (&str, u16), second: (&str, u16)) -> Result<()> {
        let (first, second) = (
            (first.0.to_owned(), first.1),
            (second.0.to_owned(), second.1),
        );
        self.write(&[META], move |t| async move {
            let meta = t.store(META)?;
            meta_put_max(&meta, &first.0, u64::from(first.1)).await?;
            meta_put_max(&meta, &second.0, u64::from(second.1)).await
        })
        .await
    }

    async fn insert_row(
        &self,
        row: &JournalRow,
        local_writer: Option<&WriterId>,
    ) -> Result<InsertOutcome> {
        let row = row.clone();
        let local_writer = local_writer.copied();
        self.write(&[META, JOURNAL], move |t| async move {
            let row = &row;
            let local_writer = local_writer.as_ref();
            writer_guard(&t.store(META)?, local_writer).await?;
            insert_row(&t.store(JOURNAL)?, row).await
        })
        .await
    }

    async fn state_put_with_row(
        &self,
        entry: &StateEntry,
        row: &JournalRow,
        local_writer: Option<&WriterId>,
    ) -> Result<InsertOutcome> {
        let entry = entry.clone();
        let row = row.clone();
        let local_writer = local_writer.copied();
        self.write(&[META, JOURNAL, STATE], move |t| async move {
            let entry = &entry;
            let row = &row;
            let local_writer = local_writer.as_ref();
            let value = state_record(entry)?;
            writer_guard(&t.store(META)?, local_writer).await?;
            let stored = get(
                &t.store(STATE)?,
                &self::key(&[
                    JsValue::from_str(&entry.kind),
                    JsValue::from_str(&entry.key),
                ]),
            )
            .await?
            .map(|o| state_entry(&o))
            .transpose()?;
            if entry_moved(stored.map(|e| e.entry_version), entry.entry_version) {
                return Ok(InsertOutcome::EntryMoved);
            }
            let outcome = insert_row(&t.store(JOURNAL)?, row).await?;
            if outcome == InsertOutcome::Inserted {
                put(&t.store(STATE)?, &value).await?;
            }
            Ok(outcome)
        })
        .await
    }

    async fn state_get(&self, kind: &str, key: &str) -> Result<Option<StateEntry>> {
        let kind = kind.to_owned();
        let key = key.to_owned();
        self.read(&[STATE], move |t| async move {
            let kind = kind.as_str();
            let key = key.as_str();
            get(
                &t.store(STATE)?,
                &self::key(&[JsValue::from_str(kind), JsValue::from_str(key)]),
            )
            .await?
            .map(|o| state_entry(&o))
            .transpose()
        })
        .await
    }

    async fn group_state_put_with_row(
        &self,
        entry: &StateEntry,
        row: &JournalRow,
        local_writer: Option<&WriterId>,
    ) -> Result<InsertOutcome> {
        let entry = entry.clone();
        let row = row.clone();
        let local_writer = local_writer.copied();
        self.write(&[META, JOURNAL, GROUP], move |t| async move {
            let entry = &entry;
            let row = &row;
            let local_writer = local_writer.as_ref();
            let value = state_record(entry)?;
            writer_guard(&t.store(META)?, local_writer).await?;
            let stored = get(
                &t.store(GROUP)?,
                &self::key(&[
                    JsValue::from_str(&entry.scope),
                    JsValue::from_str(&entry.kind),
                    JsValue::from_str(&entry.key),
                ]),
            )
            .await?
            .map(|o| state_entry(&o))
            .transpose()?;
            if entry_moved(stored.map(|e| e.entry_version), entry.entry_version) {
                return Ok(InsertOutcome::EntryMoved);
            }
            let outcome = insert_row(&t.store(JOURNAL)?, row).await?;
            if outcome == InsertOutcome::Inserted {
                put(&t.store(GROUP)?, &value).await?;
            }
            Ok(outcome)
        })
        .await
    }

    async fn group_state_get(
        &self,
        scope: &str,
        kind: &str,
        key: &str,
    ) -> Result<Option<StateEntry>> {
        let scope = scope.to_owned();
        let kind = kind.to_owned();
        let key = key.to_owned();
        self.read(&[GROUP], move |t| async move {
            let scope = scope.as_str();
            let kind = kind.as_str();
            let key = key.as_str();
            get(
                &t.store(GROUP)?,
                &self::key(&[
                    JsValue::from_str(scope),
                    JsValue::from_str(kind),
                    JsValue::from_str(key),
                ]),
            )
            .await?
            .map(|o| state_entry(&o))
            .transpose()
        })
        .await
    }

    async fn group_states_for_scope(&self, scope: &str) -> Result<Vec<StateEntry>> {
        let scope = scope.to_owned();
        self.read(&[GROUP], move |t| async move {
            let scope = scope.as_str();
            get_all(&t.store(GROUP)?, &prefix(&[JsValue::from_str(scope)]), None)
                .await?
                .iter()
                .map(state_entry)
                .collect()
        })
        .await
    }

    async fn max_writer_seq(&self, writer: &WriterId) -> Result<Option<u64>> {
        let writer = *writer;
        self.read(&[JOURNAL], move |t| async move {
            let writer = &writer;
            first(
                &t.index(JOURNAL, "writer")?,
                &prefix(&[bin(&writer.0)]),
                IdbCursorDirection::Prev,
            )
            .await?
            .map(|o| be_of(&o, "seq"))
            .transpose()
        })
        .await
    }

    async fn max_scope_writer_seq(&self, scope: &str, writer: &WriterId) -> Result<Option<u64>> {
        let scope = scope.to_owned();
        let writer = *writer;
        self.read(&[JOURNAL], move |t| async move {
            let scope = scope.as_str();
            let writer = &writer;
            first(
                &t.store(JOURNAL)?,
                &prefix(&[JsValue::from_str(scope), bin(&writer.0)]),
                IdbCursorDirection::Prev,
            )
            .await?
            .map(|o| be_of(&o, "seq"))
            .transpose()
        })
        .await
    }

    async fn rows_for_scope(
        &self,
        scope: &str,
        writer: &WriterId,
        after: u64,
        limit: u32,
    ) -> Result<Vec<JournalRow>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let scope = scope.to_owned();
        let writer = *writer;
        self.read(&[JOURNAL], move |t| async move {
            let scope = scope.as_str();
            let writer = &writer;
            get_all(
                &t.store(JOURNAL)?,
                &prefix_after(&[JsValue::from_str(scope), bin(&writer.0)], &be(after)),
                Some(limit),
            )
            .await?
            .iter()
            .map(journal_row)
            .collect()
        })
        .await
    }

    async fn coordinate_of_item(
        &self,
        scope: &str,
        item: &ItemRef,
    ) -> Result<Option<(WriterId, u64)>> {
        let scope = scope.to_owned();
        let item = item.clone();
        self.read(&[JOURNAL], move |t| async move {
            let scope = scope.as_str();
            let item = &item;
            // The index orders (scope, item, seq, writer): the first match is
            // the lowest seq, ties broken by writer — the SQLite arm's
            // `ORDER BY writer_seq, writer_id LIMIT 1`.
            first(
                &t.index(JOURNAL, "item")?,
                &prefix(&[JsValue::from_str(scope), bin(&item.encode())]),
                IdbCursorDirection::Next,
            )
            .await?
            .map(|o| Ok((writer_of(&o)?, be_of(&o, "seq")?)))
            .transpose()
        })
        .await
    }

    async fn frontier(&self, scope: &str) -> Result<Vec<(WriterId, u64)>> {
        let scope = scope.to_owned();
        self.read(&[FRONTIERS], move |t| async move {
            let scope = scope.as_str();
            get_all(
                &t.store(FRONTIERS)?,
                &prefix(&[JsValue::from_str(scope)]),
                None,
            )
            .await?
            .iter()
            .map(|o| Ok((writer_of(o)?, be_of(o, "high")?)))
            .collect()
        })
        .await
    }

    async fn frontier_raise(&self, scope: &str, writer: &WriterId, seq: u64) -> Result<u64> {
        fits_i64(seq, "high_seq")?;
        let scope = scope.to_owned();
        let writer = *writer;
        self.write(&[FRONTIERS], move |t| async move {
            let scope = scope.as_str();
            let writer = &writer;
            let frontiers = t.store(FRONTIERS)?;
            let held = match get(
                &frontiers,
                &key(&[JsValue::from_str(scope), bin(&writer.0)]),
            )
            .await?
            {
                Some(o) => Some(be_of(&o, "high")?),
                None => None,
            };
            let now = held.map_or(seq, |h| h.max(seq));
            if held != Some(now) {
                put(
                    &frontiers,
                    &record(&[
                        ("scope", Some(JsValue::from_str(scope))),
                        ("w", Some(bin(&writer.0))),
                        ("high", Some(be(now))),
                    ]),
                )
                .await?;
            }
            Ok(now)
        })
        .await
    }

    async fn nest_watermark(&self, scope: &str) -> Result<Option<u64>> {
        self.meta_get(&nest_watermark_key(scope))
            .await?
            .map(|raw| ascii_u64(&raw).context("nest watermark"))
            .transpose()
    }

    async fn nest_watermark_raise(&self, scope: &str, seq: u64) -> Result<u64> {
        let k = nest_watermark_key(scope);
        self.write(&[META], move |t| async move {
            let meta = t.store(META)?;
            meta_put_max(&meta, &k, seq).await?;
            let now = meta_get(&meta, &k)
                .await?
                .context("nest watermark absent inside its own raise")?;
            ascii_u64(&now).context("nest watermark")
        })
        .await
    }

    async fn nest_watermark_clear(&self, scope: &str) -> Result<()> {
        self.meta_delete(&nest_watermark_key(scope)).await
    }

    async fn relay_put(&self, row: &RelayRow) -> Result<()> {
        let row = row.clone();
        self.write(&[RELAY], move |t| async move {
            let row = &row;
            let relay = t.store(RELAY)?;
            let fresh = relay_record(row)?;
            let at = relay_key(&row.scope, &row.writer, &row.item_key);
            let current: JsValue = match get(&relay, &at).await? {
                Some(held) if be_of(&held, "seq")? >= row.writer_seq => held,
                _ => {
                    put(&relay, &fresh).await?;
                    fresh.into()
                }
            };
            // The one column a replay at the held coordinates may fill in.
            if let Some(feed_seq) = row.feed_seq
                && be_of(&current, "seq")? == row.writer_seq
                && opt_be_of(&current, "feed")? != Some(feed_seq)
            {
                set_field(&current, "feed", &be(feed_seq))?;
                put(&relay, current.unchecked_ref()).await?;
            }
            Ok(())
        })
        .await
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
        let scope = scope.to_owned();
        let writer = *writer;
        let item_key = item_key.to_vec();
        self.write(&[RELAY], move |t| async move {
            let scope = scope.as_str();
            let writer = &writer;
            let item_key = item_key.as_slice();
            let relay = t.store(RELAY)?;
            if let Some(held) = get(&relay, &relay_key(scope, writer, item_key)).await?
                && be_of(&held, "seq")? == writer_seq
            {
                set_field(&held, "feed", &be(feed_seq))?;
                put(&relay, held.unchecked_ref()).await?;
            }
            Ok(())
        })
        .await
    }

    async fn relay_clear_feed_seqs(&self, scope: &str) -> Result<()> {
        let scope = scope.to_owned();
        self.write(&[RELAY], move |t| async move {
            let scope = scope.as_str();
            let relay = t.store(RELAY)?;
            for o in get_all(&relay, &prefix(&[JsValue::from_str(scope)]), None).await? {
                if field(&o, "feed")?.is_some() {
                    Reflect::delete_property(
                        o.unchecked_ref::<Object>(),
                        &JsValue::from_str("feed"),
                    )
                    .map_err(|e| js_error("clear feed", e))?;
                    put(&relay, o.unchecked_ref()).await?;
                }
            }
            Ok(())
        })
        .await
    }

    async fn issued_retire_put(&self, retire: &IssuedRetire, cap: u32) -> Result<()> {
        let retire = retire.clone();
        self.write(&[RETIRES], move |t| async move {
            let retires = t.store(RETIRES)?;
            let by_ord = t.index(RETIRES, "ord")?;
            let everything = ord_through(u64::MAX)?;
            let newest = match first(&by_ord, &everything, IdbCursorDirection::Prev).await? {
                Some(held) => be_of(&held, "ord")?,
                None => 0,
            };
            fits_i64(newest + 1, "retire order token")?;
            // A put at held coordinates replaces the entry (the key path), so
            // its old order token leaves the index with it.
            put(&retires, &issued_retire_record(&retire, newest + 1)?).await?;
            let held = get_all(&by_ord, &everything, None).await?;
            for stale in held.iter().rev().skip(cap as usize) {
                delete(&retires, &issued_retire_key(&issued_retire(stale)?.1)).await?;
            }
            Ok(())
        })
        .await
    }

    async fn issued_retires(&self) -> Result<Vec<(u64, IssuedRetire)>> {
        self.read(&[RETIRES], |t| async move {
            get_all(&t.index(RETIRES, "ord")?, &ord_through(u64::MAX)?, None)
                .await?
                .iter()
                .map(issued_retire)
                .collect()
        })
        .await
    }

    async fn issued_retires_clear_through(&self, through: u64) -> Result<()> {
        fits_i64(through, "retire order token")?;
        self.write(&[RETIRES], move |t| async move {
            let retires = t.store(RETIRES)?;
            for held in get_all(&t.index(RETIRES, "ord")?, &ord_through(through)?, None).await? {
                delete(&retires, &issued_retire_key(&issued_retire(&held)?.1)).await?;
            }
            Ok(())
        })
        .await
    }

    async fn relay_retire_shadowed(
        &self,
        scope: &str,
        writer: &WriterId,
        writer_seq: u64,
        keep_item_key: &[u8],
    ) -> Result<u64> {
        fits_i64(writer_seq, "relay writer_seq")?;
        let scope = scope.to_owned();
        let writer = *writer;
        let keep_item_key = keep_item_key.to_vec();
        self.write(&[RELAY], move |t| async move {
            let scope = scope.as_str();
            let writer = &writer;
            let keep_item_key = keep_item_key.as_slice();
            delete_relay_rows_at(
                &t.store(RELAY)?,
                scope,
                writer,
                writer_seq,
                Some(keep_item_key),
            )
            .await
        })
        .await
    }

    async fn relay_retire_at(
        &self,
        scope: &str,
        writer: &WriterId,
        writer_seq: u64,
    ) -> Result<u64> {
        fits_i64(writer_seq, "relay writer_seq")?;
        let (scope, writer) = (scope.to_owned(), *writer);
        self.write(&[RELAY], move |t| async move {
            delete_relay_rows_at(&t.store(RELAY)?, &scope, &writer, writer_seq, None).await
        })
        .await
    }

    async fn relay_rows(
        &self,
        scope: &str,
        item_class: &str,
        frontier: &[(WriterId, u64)],
        limit: u32,
    ) -> Result<Vec<RelayRow>> {
        let scope = scope.to_owned();
        let item_class = item_class.to_owned();
        let frontier = frontier.to_vec();
        self.read(&[RELAY], move |t| async move {
            let scope = scope.as_str();
            let item_class = item_class.as_str();
            let frontier = frontier.as_slice();
            // The walk index orders (writer, seq) within the family; the
            // frontier gate is applied here, as the SQLite arm applies it in
            // its walk loop rather than in a generated query.
            let mut out = Vec::new();
            for o in get_all(
                &t.index(RELAY, "walk")?,
                &prefix(&[JsValue::from_str(scope), JsValue::from_str(item_class)]),
                None,
            )
            .await?
            {
                let row = relay_row(&o)?;
                let high = frontier
                    .iter()
                    .find(|(w, _)| *w == row.writer)
                    .map_or(0, |(_, s)| *s);
                if row.writer_seq <= high {
                    continue;
                }
                out.push(row);
                if out.len() as u32 >= limit {
                    break;
                }
            }
            Ok(out)
        })
        .await
    }

    async fn state_forget(&self, kind: &str, key: &str) -> Result<()> {
        let kind = kind.to_owned();
        let key = key.to_owned();
        self.write(&[STATE], move |t| async move {
            let kind = kind.as_str();
            let key = key.as_str();
            delete(
                &t.store(STATE)?,
                &self::key(&[JsValue::from_str(kind), JsValue::from_str(key)]),
            )
            .await
        })
        .await
    }

    async fn relay_forget(&self, scope: &str, writer: &WriterId, item_key: &[u8]) -> Result<()> {
        let at = relay_key(scope, writer, item_key);
        self.write(&[RELAY], move |t| async move {
            delete(&t.store(RELAY)?, &at).await
        })
        .await
    }

    async fn relay_generations(&self, scope: &str) -> Result<Vec<[u8; 32]>> {
        let scope = scope.to_owned();
        self.read(&[RELAY], move |t| async move {
            let scope = scope.as_str();
            let mut held = BTreeSet::new();
            for o in get_all(
                &t.index(RELAY, "gen")?,
                &prefix(&[JsValue::from_str(scope)]),
                None,
            )
            .await?
            {
                held.insert(arr_of::<32>(&o, "gen")?);
            }
            Ok(held.into_iter().collect())
        })
        .await
    }

    async fn relay_forget_sealed_under(&self, scope: &str, generation: &[u8; 32]) -> Result<u64> {
        let scope = scope.to_owned();
        let generation = *generation;
        self.write(&[RELAY], move |t| async move {
            let scope = scope.as_str();
            let generation = &generation;
            let relay = t.store(RELAY)?;
            let keys = get_all_keys(
                &t.index(RELAY, "gen")?,
                &only(&[JsValue::from_str(scope), bin(generation)]),
            )
            .await?;
            for k in &keys {
                delete(&relay, k).await?;
            }
            Ok(keys.len() as u64)
        })
        .await
    }

    async fn relay_rows_sealed_under(
        &self,
        scope: &str,
        generation: &[u8; 32],
    ) -> Result<Vec<RelayRow>> {
        let scope = scope.to_owned();
        let generation = *generation;
        self.read(&[RELAY], move |t| async move {
            let scope = scope.as_str();
            let generation = &generation;
            get_all(
                &t.index(RELAY, "gen")?,
                &only(&[JsValue::from_str(scope), bin(generation)]),
                None,
            )
            .await?
            .iter()
            .map(relay_row)
            .collect()
        })
        .await
    }

    async fn relay_rows_at(
        &self,
        scope: &str,
        item_class: &str,
        item_key: &[u8],
    ) -> Result<Vec<RelayRow>> {
        let scope = scope.to_owned();
        let item_class = item_class.to_owned();
        let item_key = item_key.to_vec();
        self.read(&[RELAY], move |t| async move {
            let scope = scope.as_str();
            let item_class = item_class.as_str();
            let item_key = item_key.as_slice();
            // Equal index keys order by primary key: (scope, writer, item) —
            // one row per writer, writer ascending.
            get_all(
                &t.index(RELAY, "item")?,
                &only(&[
                    JsValue::from_str(scope),
                    JsValue::from_str(item_class),
                    bin(item_key),
                ]),
                None,
            )
            .await?
            .iter()
            .map(relay_row)
            .collect()
        })
        .await
    }

    async fn relay_rows_of_writer(
        &self,
        scope: &str,
        item_class: &str,
        writer: &WriterId,
    ) -> Result<Vec<RelayRow>> {
        let scope = scope.to_owned();
        let item_class = item_class.to_owned();
        let writer = *writer;
        self.read(&[RELAY], move |t| async move {
            let scope = scope.as_str();
            let item_class = item_class.as_str();
            let writer = &writer;
            get_all(
                &t.index(RELAY, "walk")?,
                &prefix(&[
                    JsValue::from_str(scope),
                    JsValue::from_str(item_class),
                    bin(&writer.0),
                ]),
                None,
            )
            .await?
            .iter()
            .map(relay_row)
            .collect()
        })
        .await
    }

    async fn relay_high_waters(
        &self,
        scope: &str,
        item_class: &str,
    ) -> Result<Vec<(WriterId, u64)>> {
        let scope = scope.to_owned();
        let item_class = item_class.to_owned();
        self.read(&[RELAY], move |t| async move {
            let scope = scope.as_str();
            let item_class = item_class.as_str();
            let mut high: BTreeMap<WriterId, u64> = BTreeMap::new();
            for o in get_all(
                &t.index(RELAY, "walk")?,
                &prefix(&[JsValue::from_str(scope), JsValue::from_str(item_class)]),
                None,
            )
            .await?
            {
                let seq = be_of(&o, "seq")?;
                let slot = high.entry(writer_of(&o)?).or_insert(seq);
                *slot = (*slot).max(seq);
            }
            Ok(high.into_iter().collect())
        })
        .await
    }

    async fn relay_meter(&self, floor_ops: &[&str]) -> Result<Vec<RelayScopeMeter>> {
        let floor_ops: Vec<String> = floor_ops.iter().map(|op| (*op).to_owned()).collect();
        self.read(&[RELAY], move |t| async move {
            let mut families: BTreeMap<(String, String), RelayScopeMeter> = BTreeMap::new();
            for o in get_all(&t.store(RELAY)?, &JsValue::UNDEFINED, None).await? {
                let (scope, class, op) =
                    (str_of(&o, "scope")?, str_of(&o, "cls")?, str_of(&o, "op")?);
                let bytes = opt_bin_of(&o, "entry")?.map_or(0, |e| e.len() as u64);
                let evictable = !floor_ops.contains(&op) && bytes > 0;
                let m = families
                    .entry((scope.clone(), class.clone()))
                    .or_insert_with(|| RelayScopeMeter {
                        scope,
                        item_class: class,
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
        })
        .await
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
        let scope = scope.to_owned();
        let item_class = item_class.to_owned();
        let floor_ops: Vec<String> = floor_ops.iter().map(|op| (*op).to_owned()).collect();
        self.write(&[RELAY], move |t| async move {
            let scope = scope.as_str();
            let item_class = item_class.as_str();
            let relay = t.store(RELAY)?;
            let mut victims: Vec<(u64, WriterId, JsValue, u64)> = Vec::new();
            for o in get_all(
                &t.index(RELAY, "walk")?,
                &prefix(&[JsValue::from_str(scope), JsValue::from_str(item_class)]),
                None,
            )
            .await?
            {
                let Some(entry) = opt_bin_of(&o, "entry")? else {
                    continue;
                };
                if floor_ops.contains(&str_of(&o, "op")?) {
                    continue;
                }
                victims.push((be_of(&o, "seq")?, writer_of(&o)?, o, entry.len() as u64));
            }
            // Oldest first, as the SQLite arm orders it.
            victims.sort_by_key(|v| (v.0, v.1));
            let mut freed = RelayEvicted::default();
            for (_, _, o, bytes) in victims {
                // Coordinates untouched — only the payload goes (dehydration,
                // never deletion).
                Reflect::delete_property(o.unchecked_ref::<Object>(), &JsValue::from_str("entry"))
                    .map_err(|e| js_error("clear payload", e))?;
                put(&relay, o.unchecked_ref()).await?;
                freed.rows += 1;
                freed.bytes = freed.bytes.saturating_add(bytes);
                if freed.bytes >= target_bytes {
                    break;
                }
            }
            Ok(freed)
        })
        .await
    }

    async fn state_entries_of_kind(&self, kind: &str) -> Result<Vec<StateEntry>> {
        let kind = kind.to_owned();
        self.read(&[STATE], move |t| async move {
            let kind = kind.as_str();
            let mut out = Vec::new();
            for o in get_all(&t.store(STATE)?, &prefix(&[JsValue::from_str(kind)]), None).await? {
                let e = state_entry(&o)?;
                if !e.tombstone {
                    out.push(e);
                }
            }
            Ok(out)
        })
        .await
    }

    async fn block_put(&self, cid: &ContentHash, bytes: &[u8]) -> Result<()> {
        let (cid, bytes) = (*cid, bytes.to_vec());
        self.write(&[BLOCKS], move |t| async move {
            block_put(&t.store(BLOCKS)?, &cid, &bytes).await
        })
        .await
    }

    async fn block_get(&self, cid: &ContentHash) -> Result<Option<Vec<u8>>> {
        let held = *cid;
        let (loose, route) = self
            .read(&[BLOCKS, SEGMENT_BLOCKS], move |t| async move {
                let cid = &held;
                if let Some(o) = get(&t.store(BLOCKS)?, &bin(cid.as_bytes())).await? {
                    return Ok((Some(bin_of(&o, "bytes")?), None));
                }
                Ok((None, segment_route(&t.store(SEGMENT_BLOCKS)?, cid).await?))
            })
            .await?;
        if loose.is_some() {
            return Ok(loose);
        }
        let Some((key, _)) = route else {
            return Ok(None);
        };
        // The file read is outside the transaction: an OPFS await would
        // otherwise let IndexedDB auto-commit under us.
        let dat = opfs::read(&self.segment_dir(), &format!("{}.dat", segment_stem(&key)))
            .await
            .with_context(|| format!("read adopted segment {key:?}"))?;
        let mut reader = fauna_carv2::Reader::new(std::io::Cursor::new(dat.as_slice()))
            .map_err(|e| anyhow!("adopted segment {key:?}: {e}"))?;
        reader.get(cid).map(Some).map_err(|e| {
            anyhow!("adopted segment {key:?} does not yield the block its routing row claims: {e}")
        })
    }

    async fn block_has(&self, cid: &ContentHash) -> Result<bool> {
        let cid = *cid;
        self.read(&[BLOCKS, SEGMENT_BLOCKS], move |t| async move {
            let cid = &cid;
            Ok(get(&t.store(BLOCKS)?, &bin(cid.as_bytes()))
                .await?
                .is_some()
                || segment_route(&t.store(SEGMENT_BLOCKS)?, cid)
                    .await?
                    .is_some())
        })
        .await
    }

    async fn block_delete(&self, cid: &ContentHash) -> Result<bool> {
        let cid = *cid;
        self.write(&[BLOCKS], move |t| async move {
            let cid = &cid;
            let blocks = t.store(BLOCKS)?;
            let k = bin(cid.as_bytes());
            if get(&blocks, &k).await?.is_none() {
                return Ok(false);
            }
            delete(&blocks, &k).await?;
            Ok(true)
        })
        .await
    }

    async fn record_index_put(&self, entry: &RecordIndexEntry) -> Result<()> {
        let entry = entry.clone();
        self.write(&[RECORDS], move |t| async move {
            record_index_put(&t.store(RECORDS)?, &entry).await
        })
        .await
    }

    async fn record_index_get(&self, cid: &ContentHash) -> Result<Option<RecordIndexEntry>> {
        let cid = *cid;
        self.read(&[RECORDS], move |t| async move {
            let cid = &cid;
            get(&t.store(RECORDS)?, &bin(cid.as_bytes()))
                .await?
                .map(|o| record_index_entry(&o))
                .transpose()
        })
        .await
    }

    async fn record_index_delete(&self, cid: &ContentHash) -> Result<bool> {
        let cid = *cid;
        self.write(&[RECORDS], move |t| async move {
            let cid = &cid;
            let records = t.store(RECORDS)?;
            let k = bin(cid.as_bytes());
            if get(&records, &k).await?.is_none() {
                return Ok(false);
            }
            delete(&records, &k).await?;
            Ok(true)
        })
        .await
    }

    async fn records_in_scope(
        &self,
        scope: &str,
        after: Option<&ContentHash>,
        limit: u32,
    ) -> Result<Vec<RecordIndexEntry>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let scope = scope.to_owned();
        let after = after.cloned();
        self.read(&[RECORDS], move |t| async move {
            let scope = scope.as_str();
            let after = after.as_ref();
            let parts = [JsValue::from_str(scope)];
            let range = match after {
                Some(c) => prefix_after(&parts, &bin(c.as_bytes())),
                None => prefix(&parts),
            };
            get_all(&t.index(RECORDS, "scope")?, &range, Some(limit))
                .await?
                .iter()
                .map(record_index_entry)
                .collect()
        })
        .await
    }

    async fn record_added_with_row(
        &self,
        entry: &RecordIndexEntry,
        bytes: Option<&[u8]>,
        row: &JournalRow,
        local_writer: Option<&WriterId>,
    ) -> Result<InsertOutcome> {
        let entry = entry.clone();
        let bytes = bytes.map(<[u8]>::to_vec);
        let row = row.clone();
        let local_writer = local_writer.copied();
        self.write(&[META, JOURNAL, RECORDS, BLOCKS], move |t| async move {
            let entry = &entry;
            let bytes = bytes.as_deref();
            let row = &row;
            let local_writer = local_writer.as_ref();
            writer_guard(&t.store(META)?, local_writer).await?;
            let outcome = insert_row(&t.store(JOURNAL)?, row).await?;
            if outcome == InsertOutcome::Inserted {
                record_index_put(&t.store(RECORDS)?, entry).await?;
                if let Some(bytes) = bytes {
                    block_put(&t.store(BLOCKS)?, &entry.cid, bytes).await?;
                }
            }
            Ok(outcome)
        })
        .await
    }

    async fn segment_stage(&self) -> Result<IndexedDbStaging> {
        IndexedDbStaging::open(self.segment_dir()).await
    }

    /// Files first, rows second (the SQLite arm's order and reason): a file
    /// no row names is invisible and the next adoption overwrites it, while a
    /// row naming no file is the failure `block_get` cannot recover from.
    async fn segment_adopt(
        &self,
        key: &SegmentKey,
        mut staged: IndexedDbStaging,
        blocks: &[AdoptedBlock],
    ) -> Result<bool> {
        for block in blocks {
            fits_i64(block.len, "block len")?;
        }
        if self.segment_meta(key).await?.is_some() {
            return Ok(false);
        }
        let meta = staged.meta().await?;
        let dat_len = staged.len(SegmentHalf::Dat);
        let stem = segment_stem(key);
        staged
            .install(&format!("{stem}.dat"), &format!("{stem}.meta"))
            .await?;

        let key = key.clone();
        let blocks = blocks.to_vec();
        self.write(&[SEGMENTS, SEGMENT_BLOCKS], move |t| async move {
            let key = &key;
            let blocks = blocks.as_slice();
            let segments = t.store(SEGMENTS)?;
            // A racing adopter of the same key committed first: its files are
            // the same verified bytes, and its rows already route to them.
            if get(&segments, &segment_key_value(key)).await?.is_some() {
                return Ok(false);
            }
            put(
                &segments,
                &record(&[
                    ("scope", Some(JsValue::from_str(&key.scope))),
                    ("kind", Some(JsValue::from_str(&key.kind))),
                    ("id", Some(JsValue::from_f64(f64::from(key.segment_id)))),
                    ("meta", Some(bin(&meta))),
                    // The custody meter's segment half reads this rather than
                    // the OPFS file; custody eviction swaps it for `evicted`.
                    ("dat_len", Some(be(dat_len))),
                ]),
            )
            .await?;
            let routes = t.store(SEGMENT_BLOCKS)?;
            for block in blocks {
                let route = record(&[
                    ("cid", Some(bin(block.cid.as_bytes()))),
                    ("scope", Some(JsValue::from_str(&key.scope))),
                    ("kind", Some(JsValue::from_str(&key.kind))),
                    ("id", Some(JsValue::from_f64(f64::from(key.segment_id)))),
                    ("len", Some(be(block.len))),
                ]);
                // INSERT OR IGNORE: a route already held is left as it is.
                let at = self::key(&[
                    bin(block.cid.as_bytes()),
                    JsValue::from_str(&key.scope),
                    JsValue::from_str(&key.kind),
                    JsValue::from_f64(f64::from(key.segment_id)),
                ]);
                if get(&routes, &at).await?.is_none() {
                    put(&routes, &route).await?;
                }
            }
            Ok(true)
        })
        .await
    }

    async fn segment_meter(&self) -> Result<Vec<SegmentScopeMeter>> {
        let held = self
            .read(&[SEGMENTS], |t| async move {
                let mut held = Vec::new();
                for o in get_all(&t.store(SEGMENTS)?, &JsValue::UNDEFINED, None).await? {
                    let key = SegmentKey {
                        scope: str_of(&o, "scope")?,
                        kind: str_of(&o, "kind")?,
                        segment_id: num_of(&o, "id")? as u32,
                    };
                    let evicted = field(&o, "evicted")?.and_then(|v| v.as_bool()) == Some(true);
                    let dat_len = if evicted {
                        Some(None)
                    } else {
                        opt_be_of(&o, "dat_len")?.map(Some)
                    };
                    held.push((key, bin_of(&o, "meta")?.len() as u64, dat_len));
                }
                Ok(held)
            })
            .await?;
        let mut scopes: BTreeMap<String, SegmentScopeMeter> = BTreeMap::new();
        for (key, meta_len, dat_len) in held {
            // A record adopted before `dat_len` was written carries neither
            // field: its length is the file's, read once here outside the
            // transaction (an OPFS await would let IndexedDB auto-commit).
            let dat_len = match dat_len {
                Some(known) => known,
                None => {
                    opfs::size(&self.segment_dir(), &format!("{}.dat", segment_stem(&key))).await?
                }
            };
            let m = scopes
                .entry(key.scope.clone())
                .or_insert_with(|| SegmentScopeMeter {
                    scope: key.scope.clone(),
                    ..SegmentScopeMeter::default()
                });
            m.segments += 1;
            m.meta_bytes = m.meta_bytes.saturating_add(meta_len);
            if let Some(len) = dat_len {
                m.held_segments += 1;
                m.dat_bytes = m.dat_bytes.saturating_add(len);
            }
        }
        Ok(scopes.into_values().collect())
    }

    /// Rows first — the routes and the `evicted` mark in one transaction —
    /// then the OPFS file: a tab closed between leaves an unrouted file no
    /// reader reaches (the SQLite arm's order and reason).
    async fn segment_evict_dat(&self, scope: &str, target_bytes: u64) -> Result<RelayEvicted> {
        if target_bytes == 0 {
            return Ok(RelayEvicted::default());
        }
        let mut candidates: Vec<(u32, String, SegmentKey)> = Vec::new();
        for key in self.segments_in_scope(scope).await? {
            candidates.push((key.segment_id, key.kind.clone(), key));
        }
        // Oldest first: ascending segment_id, then kind.
        candidates.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
        let mut freed = RelayEvicted::default();
        for (_, _, key) in candidates {
            if freed.bytes >= target_bytes {
                break;
            }
            let marked = self
                .write(&[SEGMENTS, SEGMENT_BLOCKS], {
                    let key = key.clone();
                    move |t| async move {
                        let segments = t.store(SEGMENTS)?;
                        let Some(o) = get(&segments, &segment_key_value(&key)).await? else {
                            return Ok(None);
                        };
                        if field(&o, "evicted")?.and_then(|v| v.as_bool()) == Some(true) {
                            return Ok(None);
                        }
                        let known = opt_be_of(&o, "dat_len")?;
                        put(
                            &segments,
                            &record(&[
                                ("scope", Some(JsValue::from_str(&key.scope))),
                                ("kind", Some(JsValue::from_str(&key.kind))),
                                ("id", Some(JsValue::from_f64(f64::from(key.segment_id)))),
                                ("meta", Some(bin(&bin_of(&o, "meta")?))),
                                ("evicted", Some(JsValue::TRUE)),
                            ]),
                        )
                        .await?;
                        let routes = t.store(SEGMENT_BLOCKS)?;
                        for route in get_all(
                            &t.index(SEGMENT_BLOCKS, "scope")?,
                            &IdbKeyRange::only(&JsValue::from_str(&key.scope))
                                .map_err(|e| js_error("range", e))?
                                .into(),
                            None,
                        )
                        .await?
                        {
                            if str_of(&route, "kind")? == key.kind
                                && num_of(&route, "id")? as u32 == key.segment_id
                            {
                                delete(
                                    &routes,
                                    &self::key(&[
                                        bin(&bin_of(&route, "cid")?),
                                        JsValue::from_str(&key.scope),
                                        JsValue::from_str(&key.kind),
                                        JsValue::from_f64(f64::from(key.segment_id)),
                                    ]),
                                )
                                .await?;
                            }
                        }
                        Ok(Some(known))
                    }
                })
                .await?;
            let Some(known) = marked else {
                continue;
            };
            let file = format!("{}.dat", segment_stem(&key));
            let len = match known {
                Some(len) => len,
                None => opfs::size(&self.segment_dir(), &file).await?.unwrap_or(0),
            };
            opfs::remove(&self.segment_dir(), &file).await?;
            freed.rows += 1;
            freed.bytes = freed.bytes.saturating_add(len);
        }
        Ok(freed)
    }

    async fn segments_in_scope(&self, scope: &str) -> Result<Vec<SegmentKey>> {
        let scope = scope.to_owned();
        self.read(&[SEGMENTS], move |t| async move {
            get_all(
                &t.store(SEGMENTS)?,
                &prefix(&[JsValue::from_str(&scope)]),
                None,
            )
            .await?
            .iter()
            .map(|o| {
                Ok(SegmentKey {
                    scope: str_of(o, "scope")?,
                    kind: str_of(o, "kind")?,
                    segment_id: num_of(o, "id")? as u32,
                })
            })
            .collect()
        })
        .await
    }

    async fn segment_meta(&self, key: &SegmentKey) -> Result<Option<Vec<u8>>> {
        let key = key.clone();
        self.read(&[SEGMENTS], move |t| async move {
            let key = &key;
            get(&t.store(SEGMENTS)?, &segment_key_value(key))
                .await?
                .map(|o| bin_of(&o, "meta"))
                .transpose()
        })
        .await
    }

    async fn segment_of_block(&self, cid: &ContentHash) -> Result<Option<SegmentKey>> {
        let cid = *cid;
        self.read(&[SEGMENT_BLOCKS], move |t| async move {
            let cid = &cid;
            Ok(segment_route(&t.store(SEGMENT_BLOCKS)?, cid)
                .await?
                .map(|(k, _)| k))
        })
        .await
    }

    async fn segment_block_len(&self, cid: &ContentHash) -> Result<Option<u64>> {
        let cid = *cid;
        self.read(&[SEGMENT_BLOCKS], move |t| async move {
            let cid = &cid;
            Ok(segment_route(&t.store(SEGMENT_BLOCKS)?, cid)
                .await?
                .map(|(_, len)| len))
        })
        .await
    }

    /// Rows first — one transaction over every scope-keyed store, the pending
    /// mark written inside it — then the OPFS files, then the mark cleared.
    /// A tab closed between leaves "dropped, files pending", which the next
    /// open replays (`nest/common.md` § Client-state recoverability).
    async fn drop_scope(&self, scope: &str) -> Result<ScopeDropCounts> {
        let counts = self.drop_scope_rows(scope).await?;
        if counts.segments > 0 {
            opfs::remove_prefixed(&self.segment_dir(), &scope_file_prefix(scope)).await?;
            self.clear_pending_scope_drop(scope).await?;
        }
        Ok(counts)
    }

    async fn scopes_of_writer(&self, writer: &WriterId) -> Result<Vec<String>> {
        let writer = *writer;
        self.read(&[JOURNAL], move |t| async move {
            let writer = &writer;
            let keys =
                get_all_keys(&t.index(JOURNAL, "writer")?, &prefix(&[bin(&writer.0)])).await?;
            Ok(distinct_first_strings(&keys))
        })
        .await
    }

    async fn compact_retired_rows(&self, rows: &[JournalRow]) -> Result<RetiredCompaction> {
        let rows = rows.to_vec();
        self.write(&[META, JOURNAL, RELAY], move |t| async move {
            let rows = rows.as_slice();
            let stamped = meta_get(&t.store(META)?, crate::store::META_WRITER_ID).await?;
            let (journal, relay) = (t.store(JOURNAL)?, t.store(RELAY)?);
            let mut done = RetiredCompaction::default();
            for row in rows {
                fits_i64(row.seq, "writer_seq")?;
                if stamped.as_deref() == Some(row.writer.0.as_slice()) {
                    // The abort in `transact` undoes every delete made so far.
                    bail!(
                        "retired compaction: writer {} is the store's current writer — its log \
                         is never compacted (its append counter would re-issue the freed seqs)",
                        row.writer.to_hex()
                    );
                }
                let at = journal_key(&row.scope, &row.writer, row.seq);
                let matches = match get(&journal, &at).await? {
                    Some(held) => {
                        str_of(&held, "op")? == row.op.as_str()
                            && bin_of(&held, "item")? == row.item.encode()
                    }
                    None => false,
                };
                if !matches {
                    continue;
                }
                delete(&journal, &at).await?;
                done.journal_rows += 1;
                done.relay_rows +=
                    delete_relay_rows_at(&relay, &row.scope, &row.writer, row.seq, None).await?
                        as usize;
            }
            Ok(done)
        })
        .await
    }

    async fn scopes_with_frontiers(&self) -> Result<Vec<String>> {
        self.read(&[FRONTIERS], move |t| async move {
            let keys = get_all_keys(&t.store(FRONTIERS)?, &JsValue::UNDEFINED).await?;
            Ok(distinct_first_strings(&keys))
        })
        .await
    }

    async fn outbox_append(
        &self,
        intent: &NewOutboxIntent,
        local_writer: Option<&WriterId>,
    ) -> Result<bool> {
        let intent = intent.clone();
        let local_writer = local_writer.copied();
        self.write(&[META, OUTBOX], move |t| async move {
            let intent = &intent;
            let local_writer = local_writer.as_ref();
            writer_guard(&t.store(META)?, local_writer).await?;
            let outbox = t.store(OUTBOX)?;
            if get(&outbox, &bin(&intent.intent_id)).await?.is_some() {
                return Ok(false);
            }
            // One readwrite transaction on `outbox` excludes every other, so
            // the max-plus-one cannot race (the SQLite arm's retry loop has
            // nothing to retry here).
            let channel_seq = match first(
                &t.index(OUTBOX, "fifo")?,
                &prefix(&[JsValue::from_str(&intent.scope)]),
                IdbCursorDirection::Prev,
            )
            .await?
            {
                Some(o) => be_of(&o, "cseq")? + 1,
                None => 1,
            };
            put(
                &outbox,
                &record(&[
                    ("id", Some(bin(&intent.intent_id))),
                    ("kind", Some(JsValue::from_str(&intent.kind))),
                    ("scope", Some(JsValue::from_str(&intent.scope))),
                    ("payload", Some(bin(&intent.payload))),
                    ("drainer", Some(JsValue::from_str(intent.drainer.as_str()))),
                    ("cseq", Some(be(channel_seq))),
                    (
                        "status",
                        Some(JsValue::from_str(IntentStatus::Pending.as_str())),
                    ),
                    ("retries", Some(JsValue::from_f64(0.0))),
                    ("created", Some(be(now_epoch_secs()))),
                ]),
            )
            .await?;
            Ok(true)
        })
        .await
    }

    async fn outbox_undrained(&self) -> Result<Vec<OutboxIntent>> {
        self.read(&[OUTBOX], move |t| async move {
            get_all(&t.index(OUTBOX, "fifo")?, &JsValue::UNDEFINED, None)
                .await?
                .iter()
                .map(outbox_intent)
                .collect()
        })
        .await
    }

    async fn outbox_ack(&self, intent_id: &[u8; 16]) -> Result<bool> {
        let intent_id = *intent_id;
        self.write(&[OUTBOX], move |t| async move {
            let intent_id = &intent_id;
            let outbox = t.store(OUTBOX)?;
            let k = bin(intent_id);
            if get(&outbox, &k).await?.is_none() {
                return Ok(false);
            }
            delete(&outbox, &k).await?;
            Ok(true)
        })
        .await
    }

    async fn outbox_mark_failed(&self, intent_id: &[u8; 16]) -> Result<bool> {
        let intent_id = *intent_id;
        self.write(&[OUTBOX], move |t| async move {
            let intent_id = &intent_id;
            let outbox = t.store(OUTBOX)?;
            let Some(o) = get(&outbox, &bin(intent_id)).await? else {
                return Ok(false);
            };
            set_field(
                &o,
                "status",
                &JsValue::from_str(IntentStatus::Failed.as_str()),
            )?;
            put(&outbox, o.unchecked_ref()).await?;
            Ok(true)
        })
        .await
    }

    async fn outbox_record_attempt(&self, intent_id: &[u8; 16]) -> Result<bool> {
        let intent_id = *intent_id;
        self.write(&[OUTBOX], move |t| async move {
            let intent_id = &intent_id;
            let outbox = t.store(OUTBOX)?;
            let Some(o) = get(&outbox, &bin(intent_id)).await? else {
                return Ok(false);
            };
            let retries = num_of(&o, "retries")?;
            set_field(&o, "retries", &JsValue::from_f64(retries + 1.0))?;
            set_field(&o, "last", &be(now_epoch_secs()))?;
            put(&outbox, o.unchecked_ref()).await?;
            Ok(true)
        })
        .await
    }
}

/// The web arm's [`SegmentStaging`] slot: the pair's two files in the store's
/// OPFS directory under [`STAGING_PREFIX`], each written through one open
/// writable stream a chunk at a time, committed at `close()`, and adopted by
/// an OPFS rename.
///
/// **One gap against the native arm:** [`SegmentStaging::dat_reader`] reads
/// the staged `.dat` back whole — OPFS offers this arm no synchronous
/// random-access read outside a worker, and admission's CARv2 reader is
/// synchronous. The web has no network path into adoption today (the
/// transfer lives in the native-only sync engine), so the transfer half is
/// bounded here and the admission half is its own tracked gap
/// (`message-segment-store.md` § Segment size).
pub struct IndexedDbStaging {
    dir: String,
    dat: StagedOpfsFile,
    meta: StagedOpfsFile,
    /// Set once renamed into place (or abandoned as a crash).
    settled: bool,
}

struct StagedOpfsFile {
    name: String,
    /// `None` once committed.
    writable: Option<JsValue>,
    len: u64,
}

impl StagedOpfsFile {
    async fn create(dir: &str, name: String) -> Result<Self> {
        let writable = opfs::create_writable(dir, &name).await?;
        Ok(Self {
            name,
            writable: Some(writable),
            len: 0,
        })
    }

    async fn write(&mut self, chunk: &[u8]) -> Result<()> {
        let Some(writable) = self.writable.as_ref() else {
            bail!("staging file {} is already closed", self.name);
        };
        opfs::write_chunk(writable, chunk).await?;
        self.len += chunk.len() as u64;
        Ok(())
    }

    /// Commit the writable. Idempotent.
    async fn close(&mut self) -> Result<()> {
        if let Some(writable) = self.writable.take() {
            opfs::close_writable(&writable).await?;
        }
        Ok(())
    }
}

impl IndexedDbStaging {
    async fn open(dir: String) -> Result<Self> {
        // Unique among this origin's live slots: a random draw (no process id
        // on the web, and tabs share the directory).
        let unique = format!(
            "{:013x}{:013x}",
            (js_sys::Math::random() * (1u64 << 52) as f64) as u64,
            (js_sys::Math::random() * (1u64 << 52) as f64) as u64
        );
        let stem = staging_stem(&unique);
        let dat = StagedOpfsFile::create(&dir, format!("{stem}.dat")).await?;
        let meta = StagedOpfsFile::create(&dir, format!("{stem}.meta")).await?;
        Ok(Self {
            dir,
            dat,
            meta,
            settled: false,
        })
    }

    /// Commit both files and rename them onto the adopted pair's names —
    /// `.dat` first, the native arm's order.
    async fn install(&mut self, dat_name: &str, meta_name: &str) -> Result<()> {
        self.dat.close().await?;
        self.meta.close().await?;
        opfs::rename(&self.dir, &self.dat.name, dat_name).await?;
        opfs::rename(&self.dir, &self.meta.name, meta_name).await?;
        self.settled = true;
        Ok(())
    }
}

impl SegmentSink for IndexedDbStaging {
    async fn write(&mut self, half: SegmentHalf, chunk: &[u8]) -> Result<()> {
        match half {
            SegmentHalf::Dat => self.dat.write(chunk).await,
            SegmentHalf::Meta => self.meta.write(chunk).await,
        }
    }
}

impl SegmentStaging for IndexedDbStaging {
    fn len(&self, half: SegmentHalf) -> u64 {
        match half {
            SegmentHalf::Dat => self.dat.len,
            SegmentHalf::Meta => self.meta.len,
        }
    }

    async fn meta(&mut self) -> Result<Vec<u8>> {
        self.meta.close().await?;
        opfs::read(&self.dir, &self.meta.name).await
    }

    async fn dat_reader(&mut self) -> Result<Box<dyn ReadSeek + '_>> {
        self.dat.close().await?;
        let dat = opfs::read(&self.dir, &self.dat.name).await?;
        Ok(Box::new(std::io::Cursor::new(dat)))
    }

    #[cfg(feature = "test-helpers")]
    async fn abandon_as_crash(mut self) {
        self.dat.close().await.expect("commit staged .dat");
        self.meta.close().await.expect("commit staged .meta");
        self.settled = true;
    }
}

impl Drop for IndexedDbStaging {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        // Discarded un-adopted: abort what is uncommitted and remove the
        // files, after the fact — a drop cannot await. Whatever this misses,
        // the next open sweeps.
        let dir = self.dir.clone();
        let halves: Vec<(String, Option<JsValue>)> = [&mut self.dat, &mut self.meta]
            .into_iter()
            .map(|f| (f.name.clone(), f.writable.take()))
            .collect();
        wasm_bindgen_futures::spawn_local(async move {
            for (name, writable) in halves {
                if let Some(writable) = writable {
                    opfs::abort_writable(&writable).await;
                }
                let _ = opfs::remove(&dir, &name).await;
            }
        });
    }
}

/// The segment area over the Origin Private File System, reached by property
/// lookup (module docs). One directory per store under the OPFS root.
mod opfs {
    use anyhow::{Context, Result, anyhow};
    use js_sys::{Object, Promise, Reflect, Uint8Array};
    use wasm_bindgen::JsCast;
    use wasm_bindgen::prelude::*;
    use wasm_bindgen_futures::JsFuture;

    fn get(o: &JsValue, name: &str) -> Result<JsValue> {
        Reflect::get(o, &JsValue::from_str(name)).map_err(|e| anyhow!("OPFS {name}: {e:?}"))
    }

    /// `o.name(args…)`, awaited.
    async fn call(o: &JsValue, name: &str, args: &[&JsValue]) -> Result<JsValue> {
        let f: js_sys::Function = get(o, name)?
            .dyn_into()
            .map_err(|_| anyhow!("OPFS: {name} is not a function"))?;
        let returned = match args {
            [] => f.call0(o),
            [a] => f.call1(o, a),
            [a, b] => f.call2(o, a, b),
            _ => unreachable!("no OPFS call here takes more than two arguments"),
        }
        .map_err(|e| anyhow!("OPFS {name}: {e:?}"))?;
        JsFuture::from(Promise::resolve(&returned))
            .await
            .map_err(|e| anyhow!("OPFS {name}: {e:?}"))
    }

    fn create(yes: bool) -> JsValue {
        let o = Object::new();
        let _ = Reflect::set(&o, &JsValue::from_str("create"), &JsValue::from_bool(yes));
        o.into()
    }

    async fn root() -> Result<JsValue> {
        let navigator = get(&js_sys::global(), "navigator")?;
        let storage = get(&navigator, "storage")?;
        if storage.is_undefined() {
            anyhow::bail!("the Origin Private File System is unavailable in this context");
        }
        call(&storage, "getDirectory", &[]).await
    }

    /// The store's directory; `None` when it does not exist and `make` is off.
    async fn dir(name: &str, make: bool) -> Result<Option<JsValue>> {
        let root = root().await?;
        match call(
            &root,
            "getDirectoryHandle",
            &[&JsValue::from_str(name), &create(make)],
        )
        .await
        {
            Ok(d) => Ok(Some(d)),
            Err(_) if !make => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// `file`'s length, or `None` when it (or the store's directory) does not
    /// exist.
    pub(super) async fn size(dir_name: &str, file: &str) -> Result<Option<u64>> {
        let Some(dir) = dir(dir_name, false).await? else {
            return Ok(None);
        };
        let Ok(handle) = call(&dir, "getFileHandle", &[&JsValue::from_str(file)]).await else {
            return Ok(None);
        };
        let blob = call(&handle, "getFile", &[]).await?;
        Ok(get(&blob, "size")?.as_f64().map(|n| n as u64))
    }

    /// Remove `file`; absent is not an error (a replayed eviction).
    pub(super) async fn remove(dir_name: &str, file: &str) -> Result<()> {
        let Some(dir) = dir(dir_name, false).await? else {
            return Ok(());
        };
        match call(&dir, "removeEntry", &[&JsValue::from_str(file)]).await {
            Ok(_) => Ok(()),
            Err(e) if format!("{e}").contains("NotFound") => Ok(()),
            Err(e) => Err(e),
        }
    }

    pub(super) async fn read(dir_name: &str, file: &str) -> Result<Vec<u8>> {
        let dir = dir(dir_name, false)
            .await?
            .context("the store has no segment area")?;
        let handle = call(&dir, "getFileHandle", &[&JsValue::from_str(file)]).await?;
        let blob = call(&handle, "getFile", &[]).await?;
        let buffer = call(&blob, "arrayBuffer", &[]).await?;
        Ok(Uint8Array::new(&buffer).to_vec())
    }

    /// Remove every file in the store's directory whose name starts with
    /// `prefix`; how many went.
    pub(super) async fn remove_prefixed(dir_name: &str, prefix: &str) -> Result<u64> {
        let Some(dir) = dir(dir_name, false).await? else {
            return Ok(0); // nothing was ever adopted
        };
        // Collect first, remove after: removing while iterating the async
        // iterator is unspecified.
        let names: Vec<String> = names_in(&dir)
            .await?
            .into_iter()
            .filter(|n| n.starts_with(prefix))
            .collect();
        for name in &names {
            call(&dir, "removeEntry", &[&JsValue::from_str(name)]).await?;
        }
        Ok(names.len() as u64)
    }

    /// [`remove_prefixed`], best effort: an entry that will not go (a live
    /// tab's open writable holds it) is skipped, never an error — the caller
    /// is a store's open, which a sibling tab's transfer must not fail.
    pub(super) async fn sweep_prefixed(dir_name: &str, prefix: &str) {
        let Ok(Some(dir)) = dir(dir_name, false).await else {
            return;
        };
        let Ok(names) = names_in(&dir).await else {
            return;
        };
        for name in names.iter().filter(|n| n.starts_with(prefix)) {
            let _ = call(&dir, "removeEntry", &[&JsValue::from_str(name)]).await;
        }
    }

    /// A writable stream on a new, empty `file` — the staging half's sink. Its
    /// content swaps in whole at `close()`, the web's write-then-rename.
    pub(super) async fn create_writable(dir_name: &str, file: &str) -> Result<JsValue> {
        let dir = dir(dir_name, true).await?.context("created above")?;
        let handle = call(
            &dir,
            "getFileHandle",
            &[&JsValue::from_str(file), &create(true)],
        )
        .await?;
        call(&handle, "createWritable", &[]).await
    }

    /// Append `bytes` to an open writable.
    pub(super) async fn write_chunk(writable: &JsValue, bytes: &[u8]) -> Result<()> {
        call(writable, "write", &[&Uint8Array::from(bytes).into()]).await?;
        Ok(())
    }

    /// Commit a writable: its content becomes the file's at `close()`.
    pub(super) async fn close_writable(writable: &JsValue) -> Result<()> {
        call(writable, "close", &[]).await?;
        Ok(())
    }

    /// Discard a writable's uncommitted content.
    pub(super) async fn abort_writable(writable: &JsValue) {
        let _ = call(writable, "abort", &[]).await;
    }

    /// Rename `from` to `to` in the store's directory, replacing `to`.
    pub(super) async fn rename(dir_name: &str, from: &str, to: &str) -> Result<()> {
        let dir = dir(dir_name, false)
            .await?
            .context("the store has no segment area")?;
        let handle = call(&dir, "getFileHandle", &[&JsValue::from_str(from)]).await?;
        call(&handle, "move", &[&JsValue::from_str(to)])
            .await
            .with_context(|| format!("OPFS rename {from} -> {to}"))?;
        Ok(())
    }

    /// Every file name in the store's directory; empty when it does not exist.
    #[cfg(feature = "test-helpers")]
    pub(super) async fn list(dir_name: &str) -> Result<Vec<String>> {
        match dir(dir_name, false).await? {
            Some(dir) => names_in(&dir).await,
            None => Ok(Vec::new()),
        }
    }

    /// Drain a directory handle's `keys()` async iterator.
    async fn names_in(dir: &JsValue) -> Result<Vec<String>> {
        let iter = call(dir, "keys", &[]).await?;
        let mut names = Vec::new();
        loop {
            let step = call(&iter, "next", &[]).await?;
            if get(&step, "done")?.as_bool().unwrap_or(true) {
                break;
            }
            if let Some(name) = get(&step, "value")?.as_string() {
                names.push(name);
            }
        }
        Ok(names)
    }

    /// Remove the store's whole directory (store deletion).
    pub(super) async fn remove_dir(dir_name: &str) -> Result<()> {
        let root = root().await?;
        let recursive = Object::new();
        let _ = Reflect::set(&recursive, &JsValue::from_str("recursive"), &JsValue::TRUE);
        match call(
            &root,
            "removeEntry",
            &[&JsValue::from_str(dir_name), &recursive.into()],
        )
        .await
        {
            Ok(_) => Ok(()),
            // NotFoundError: no segment was ever adopted.
            Err(e) if format!("{e}").contains("NotFound") => Ok(()),
            Err(e) => Err(e),
        }
    }
}
