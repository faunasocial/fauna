//! An **on-disk working index** — the unsealed, readable index a key holder
//! keeps for its own queries, persisted in a directory it owns.
//!
//! The posture is a client's own working index (`content-index.md`
//! § Encryption posture → *On a client*): the holder already holds the keys,
//! so its working copy is protected by the disk it rests on, not by a seal.
//! The second user is the nest's in-process materializer
//! (`encryption-at-rest.md` § Capability tiering → *The materialization
//! grant*, part (c)), which keeps one per granted actor and kind under
//! `<data_dir>/__materialized/<actor_hex>/` — the same engine, schema and
//! tokenizer as a device's, so the nest's hits and a device's own hits are
//! the same documents (priority #2). The doc shape is [`IndexedDoc`]: for
//! mail the content id is the RFC `Message-ID` and `secondary_id` the nest
//! message id, exactly the client builder's. **No stored source text** —
//! the schema stores no body, and a snippet is rendered at query time from
//! the sealed record (that doc's Don't stands).
//!
//! **Persistence.** The index lives in memory and is written whole, as the
//! plaintext snapshot [`Index::seal_to_bytes`] produces, to one file in the
//! directory ([`WORKING_INDEX_FILE`]) by [`WorkingIndex::flush`] — atomically
//! (a temporary sibling, fsynced, renamed over, the directory fsynced), so a
//! crash leaves the previous snapshot or the new one, never a torn file. A
//! memory-mapped Tantivy directory would avoid holding the whole index in
//! memory; it needs Tantivy's `mmap` feature, a dependency change this crate
//! does not take on its own.
//!
//! Native targets only: a browser has no directory to own.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::index::Index;
use crate::types::{ContentId, ContentKind, IndexError, IndexedDoc, QueryHit, TimeRange};

/// The snapshot's file name inside a working-index directory.
pub const WORKING_INDEX_FILE: &str = "working.idx";

/// The temporary sibling a flush writes before renaming it over
/// [`WORKING_INDEX_FILE`]; a leftover one (a crash mid-flush) is ignored and
/// overwritten.
const WORKING_INDEX_TMP: &str = "working.idx.tmp";

/// One readable index persisted in a directory its holder owns. See the
/// module doc.
pub struct WorkingIndex {
    dir: PathBuf,
    index: Index,
    dirty: bool,
}

impl WorkingIndex {
    /// Open the working index in `dir`, or create an empty one there. The
    /// directory is created when missing (owner-only on unix: the contents
    /// are readable). Nothing is written until the first [`Self::flush`].
    ///
    /// # Errors
    ///
    /// [`IndexError::Io`] when the directory cannot be created or the
    /// snapshot read; the snapshot's own decode errors when it is corrupt —
    /// never healed by overwriting it here: the caller decides (a
    /// materializer rebuilds from the sealed corpus, which is canon).
    pub fn open_or_create(dir: &Path) -> Result<Self, IndexError> {
        create_private_dir(dir)?;
        let path = dir.join(WORKING_INDEX_FILE);
        let index = match fs::read(&path) {
            Ok(bytes) => Index::open_from_bytes(&bytes)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Index::create_in_ram()?,
            Err(e) => return Err(e.into()),
        };
        Ok(Self {
            dir: dir.to_path_buf(),
            index,
            dirty: false,
        })
    }

    /// Delete the working index in `dir` — the directory and everything in
    /// it. Absent is success: deletion is idempotent, as a revoke handler and
    /// a boot reconcile both need.
    ///
    /// # Errors
    ///
    /// [`IndexError::Io`] on any other removal failure.
    pub fn destroy(dir: &Path) -> Result<(), IndexError> {
        match fs::remove_dir_all(dir) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// The directory this index persists in.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Whether a change has been made since the last flush.
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Add or replace the document keyed by its content id
    /// ([`Index::add_doc`]); visible to [`Self::query`] after
    /// [`Self::commit`] or [`Self::flush`].
    ///
    /// # Errors
    ///
    /// The underlying writer's.
    pub fn add_doc(&mut self, doc: IndexedDoc) -> Result<(), IndexError> {
        self.index.add_doc(doc)?;
        self.dirty = true;
        Ok(())
    }

    /// Delete the document keyed by `content_id` ([`Index::delete_doc`]).
    pub fn delete_doc(&mut self, content_id: &ContentId) {
        self.index.delete_doc(content_id);
        self.dirty = true;
    }

    /// Make every change so far visible to [`Self::query`] without writing
    /// the snapshot.
    ///
    /// # Errors
    ///
    /// The underlying writer's.
    pub fn commit(&mut self) -> Result<(), IndexError> {
        self.index.commit()
    }

    /// Commit and write the snapshot atomically (see the module doc). A
    /// clean index writes nothing.
    ///
    /// # Errors
    ///
    /// [`IndexError::Io`] on a write, sync or rename failure — the previous
    /// snapshot is then still in place.
    pub fn flush(&mut self) -> Result<(), IndexError> {
        if !self.dirty {
            return Ok(());
        }
        let bytes = self.index.seal_to_bytes()?;
        let tmp = self.dir.join(WORKING_INDEX_TMP);
        {
            let mut file = create_private_file(&tmp)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        fs::rename(&tmp, self.dir.join(WORKING_INDEX_FILE))?;
        // The rename is durable only once the directory entry is.
        #[cfg(unix)]
        fs::File::open(&self.dir)?.sync_all()?;
        self.dirty = false;
        Ok(())
    }

    /// Query the committed documents ([`Index::query`]).
    ///
    /// # Errors
    ///
    /// The underlying query's.
    pub fn query(
        &self,
        query_str: &str,
        kinds: &[ContentKind],
        range: Option<TimeRange>,
        limit: usize,
    ) -> Result<Vec<QueryHit>, IndexError> {
        self.index.query(query_str, kinds, range, limit)
    }

    /// The underlying index, for the read paths this wrapper does not
    /// forward ([`Index::query_all_of`], [`Index::doc_identities`], …).
    #[must_use]
    pub fn index(&self) -> &Index {
        &self.index
    }
}

fn create_private_dir(dir: &Path) -> Result<(), IndexError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
    }
    #[cfg(not(unix))]
    fs::create_dir_all(dir)?;
    Ok(())
}

fn create_private_file(path: &Path) -> Result<fs::File, IndexError> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{FieldKind, IndexedField};

    /// A scratch directory under the system temp dir, removed on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            // Unique per process and per call; no wall clock in a crate that
            // reaches wasm.
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "fauna-index-working-{tag}-{}-{n}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&dir);
            Self(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn mail(message_id: &str, nest_id: &[u8], body: &str) -> IndexedDoc {
        IndexedDoc {
            kind: ContentKind::Mail,
            content_id: ContentId(message_id.as_bytes().to_vec()),
            timestamp_ns: 1,
            sender_actor_id: None,
            secondary_id: Some(nest_id.to_vec()),
            fields: vec![IndexedField {
                kind: FieldKind::Body,
                text: body.to_string(),
            }],
        }
    }

    fn ids(hits: &[QueryHit]) -> Vec<Vec<u8>> {
        hits.iter().map(|h| h.content_id.0.clone()).collect()
    }

    /// A flushed index reopens from its directory with every doc, the
    /// client builder's doc shape (`Message-ID` content id, nest id
    /// secondary) intact.
    #[test]
    fn a_flushed_working_index_reopens_with_its_docs() {
        let scratch = Scratch::new("reopen");
        let dir = scratch.0.join("actor").join("mail");
        let mut index = WorkingIndex::open_or_create(&dir).unwrap();
        index
            .add_doc(mail("<a@x>", b"n1", "quarterly invoice"))
            .unwrap();
        index.add_doc(mail("<b@x>", b"n2", "lunch plans")).unwrap();
        assert!(index.is_dirty());
        index.flush().unwrap();
        assert!(!index.is_dirty());
        assert!(dir.join(WORKING_INDEX_FILE).is_file());
        assert!(!dir.join(WORKING_INDEX_TMP).exists());
        drop(index);

        let reopened = WorkingIndex::open_or_create(&dir).unwrap();
        let hits = reopened.query("invoice", &[], None, 10).unwrap();
        assert_eq!(ids(&hits), vec![b"<a@x>".to_vec()]);
        let identities = reopened.index().doc_identities().unwrap();
        assert!(
            identities.iter().any(
                |d| d.content_id.0 == b"<b@x>" && d.secondary_id.as_deref() == Some(&b"n2"[..])
            )
        );
    }

    /// The retire arm: a deleted doc is gone from the query and from the
    /// reopened snapshot; an unflushed change never reaches the disk.
    #[test]
    fn delete_by_content_id_retires_a_doc() {
        let scratch = Scratch::new("delete");
        let mut index = WorkingIndex::open_or_create(&scratch.0).unwrap();
        index.add_doc(mail("<a@x>", b"n1", "invoice one")).unwrap();
        index.add_doc(mail("<b@x>", b"n2", "invoice two")).unwrap();
        index.flush().unwrap();
        index.delete_doc(&ContentId(b"<a@x>".to_vec()));
        index.commit().unwrap();
        assert_eq!(
            ids(&index.query("invoice", &[], None, 10).unwrap()),
            vec![b"<b@x>".to_vec()]
        );
        // Not flushed: the disk still has both.
        let on_disk = WorkingIndex::open_or_create(&scratch.0).unwrap();
        assert_eq!(on_disk.query("invoice", &[], None, 10).unwrap().len(), 2);
        index.flush().unwrap();
        let on_disk = WorkingIndex::open_or_create(&scratch.0).unwrap();
        assert_eq!(
            ids(&on_disk.query("invoice", &[], None, 10).unwrap()),
            vec![b"<b@x>".to_vec()]
        );
    }

    /// The snapshot holds no source text verbatim in a stored field: the
    /// schema stores no body, so a query hit carries ids only.
    #[test]
    fn a_hit_carries_no_source_text() {
        let scratch = Scratch::new("nostore");
        let mut index = WorkingIndex::open_or_create(&scratch.0).unwrap();
        index
            .add_doc(mail("<a@x>", b"n1", "a uniquely searchable sentence"))
            .unwrap();
        index.commit().unwrap();
        let hits = index.query("uniquely", &[], None, 10).unwrap();
        assert_eq!(hits.len(), 1);
        let debug = format!("{:?}", hits[0]);
        assert!(!debug.contains("searchable sentence"), "{debug}");
    }

    /// Destroy removes the directory and is idempotent; a fresh open after it
    /// is empty.
    #[test]
    fn destroy_is_idempotent_and_leaves_nothing() {
        let scratch = Scratch::new("destroy");
        let mut index = WorkingIndex::open_or_create(&scratch.0).unwrap();
        index.add_doc(mail("<a@x>", b"n1", "invoice")).unwrap();
        index.flush().unwrap();
        drop(index);
        WorkingIndex::destroy(&scratch.0).unwrap();
        assert!(!scratch.0.exists());
        WorkingIndex::destroy(&scratch.0).unwrap();
        let fresh = WorkingIndex::open_or_create(&scratch.0).unwrap();
        assert!(fresh.query("invoice", &[], None, 10).unwrap().is_empty());
    }

    /// The directory and snapshot are owner-only: their contents are
    /// readable.
    #[cfg(unix)]
    #[test]
    fn the_directory_and_snapshot_are_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let scratch = Scratch::new("perms");
        let mut index = WorkingIndex::open_or_create(&scratch.0).unwrap();
        index.add_doc(mail("<a@x>", b"n1", "invoice")).unwrap();
        index.flush().unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&scratch.0), 0o700);
        assert_eq!(mode(&scratch.0.join(WORKING_INDEX_FILE)), 0o600);
    }
}
