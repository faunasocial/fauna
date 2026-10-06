//! UniFFI `IndexHandle` — `Arc<Self>` wrapper around `Mutex<Index>`.
//!
//! Exists because UniFFI calls Object methods on `&Arc<Self>` (i.e. through a
//! shared reference), but the underlying `Index` writer methods take `&mut
//! self`. Wrapping in `Mutex<Index>` keeps the bare-Rust `Index` API untouched
//! (nest still calls `&mut self` methods directly) while letting Apple,
//! Android, Windows apps call into the same Tantivy state through the
//! UniFFI calling convention.
//!
//! Lock-window discipline: every method acquires, does the Tantivy call,
//! releases. Tantivy operations on a RAM directory are microseconds; clients
//! never observe contention worth worrying about. If a future method needs to
//! hold the lock across an `await`, switch to `tokio::sync::Mutex` and gate
//! the surface as async.
//!
//! Plan 4 surface (foundation): create_in_ram, add_doc, commit, query.
//! Plan 5 will add open_encrypted_in_ram and any seal/manifest entry points
//! clients need then; this struct stays the single Object across both plans.

use crate::index::Index;
use crate::types::{ContentKind, IndexError, IndexedDoc, QueryHit, TimeRange};
use std::sync::{Arc, Mutex};

#[derive(uniffi::Object)]
pub struct IndexHandle {
    inner: Mutex<Index>,
}

#[uniffi::export]
impl IndexHandle {
    /// Build a fresh in-memory index. Equivalent to `Index::create_in_ram()`
    /// but returned as an `Arc<IndexHandle>` so it can travel across the FFI
    /// boundary.
    #[uniffi::constructor]
    pub fn create_in_ram() -> Result<Arc<Self>, IndexError> {
        let inner = Index::create_in_ram()?;
        Ok(Arc::new(Self {
            inner: Mutex::new(inner),
        }))
    }

    /// Add one document. Caller must call `commit` before the doc is visible
    /// to `query`.
    pub fn add_doc(&self, doc: IndexedDoc) -> Result<(), IndexError> {
        self.lock().add_doc(doc)
    }

    /// Commit pending writes. Required before `query` sees newly-added docs.
    pub fn commit(&self) -> Result<(), IndexError> {
        self.lock().commit()
    }

    /// Run a free-text query.
    ///
    /// `query` is parsed by Tantivy's `QueryParser` (so phrase queries with
    /// `"..."` work); `kinds` filters to those `ContentKind`s; `range`
    /// optionally constrains by `IndexedDoc::timestamp_ns`. `limit` caps the
    /// hit count (BM25-ranked).
    ///
    /// Empty `query` produces zero hits (no implicit "match all" — the caller
    /// must provide a non-empty query).
    pub fn query(
        &self,
        query: String,
        kinds: Vec<ContentKind>,
        range: Option<TimeRange>,
        limit: u32,
    ) -> Result<Vec<QueryHit>, IndexError> {
        self.lock().query(&query, &kinds, range, limit as usize)
    }
}

impl IndexHandle {
    /// Acquire the inner Mutex; panics only on poisoning, which means a prior
    /// method panicked while holding the lock — irrecoverable, so panicking
    /// is correct.
    fn lock(&self) -> std::sync::MutexGuard<'_, Index> {
        self.inner.lock().expect("IndexHandle mutex poisoned")
    }
}
