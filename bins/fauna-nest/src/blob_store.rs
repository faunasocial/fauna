use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Result;
use async_trait::async_trait;
use fauna_core::data::ContentHash;

/// Async trait for content-addressed blob storage.
#[async_trait]
pub trait BlobStoreBackend: Send + Sync {
    async fn put(&self, hash: &ContentHash, data: &[u8]) -> Result<()>;
    async fn get(&self, hash: &ContentHash) -> Result<Option<Vec<u8>>>;
    async fn exists(&self, hash: &ContentHash) -> Result<bool>;
    /// Byte length of a stored blob, or `None` when it is not held.
    ///
    /// The strict form of [`Self::exists`], for a caller that must check a
    /// *declared* size against the real one -- the `__index` rail's `record`
    /// door does this, because the declared figure is what `fauna.index.list`
    /// reports and what the shared fold planner budgets on
    /// (`content-index.md` Where the index is built).
    ///
    /// The default reads the bytes, which is correct for every backend; one
    /// that can ask for the length alone overrides it.
    async fn len(&self, hash: &ContentHash) -> Result<Option<u64>> {
        Ok(self.get(hash).await?.map(|b| b.len() as u64))
    }
    /// The bytes `start..=end_inclusive` of a stored blob, or `None` when it is
    /// not held — the read behind a `Range` request on the blob route
    /// (`render-model.md` § D6c → *Inline playback*), so a seek into a large
    /// video never reads the whole blob. An end past the last byte is clamped;
    /// a start past it yields an empty vector (the route answers 416 before
    /// asking).
    ///
    /// The default slices [`Self::get`], which is correct for every backend; one
    /// that can read a range natively overrides it.
    async fn get_range(
        &self,
        hash: &ContentHash,
        start: u64,
        end_inclusive: u64,
    ) -> Result<Option<Vec<u8>>> {
        Ok(self.get(hash).await?.map(|b| {
            let len = b.len() as u64;
            if start >= len || end_inclusive < start {
                return Vec::new();
            }
            let end = end_inclusive.min(len - 1);
            b[start as usize..=end as usize].to_vec()
        }))
    }
    async fn exists_batch(&self, hashes: &[ContentHash]) -> Result<Vec<bool>>;
    async fn delete(&self, hash: &ContentHash) -> Result<()>;
    async fn usage_bytes(&self) -> Result<u64>;
    async fn list_all_hashes(&self) -> Result<Vec<ContentHash>> {
        anyhow::bail!("list_all_hashes not implemented for this backend")
    }
}

/// The default free-space floor below which the blob store refuses NEW writes
/// (a NEW blob, not an idempotent re-put of one already on disk). This is the
/// hard guard that makes a disk-fill outage impossible regardless of which
/// writer leaks: with a 2 GiB floor the nest's SQLite DB + WAL always have
/// headroom to write, so the box can never be bricked by ENOSPC at boot. See
/// `docs/goal/behavior/backup-restore.md` § Blob-store disk guard.
pub const DEFAULT_MIN_FREE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Per-process counter that makes each `put`'s temp filename unique, so two
/// concurrent writers of the same blob never share one temp path. See the
/// comment at its use site in [`DiskBlobStore::put`].
static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Disk-backed blob store using a hash-sharded directory layout.
///
/// Blobs are stored at `{root}/{hex[0..2]}/{hex[2..4]}/{hex}`.
pub struct DiskBlobStore {
    root: PathBuf,
    usage: AtomicU64,
    /// Refuse a NEW-blob write when the filesystem's available bytes would drop
    /// below this floor (`DEFAULT_MIN_FREE_BYTES` unless overridden). The guard
    /// that makes the disk-fill outage class unrepresentable.
    min_free_bytes: u64,
}

impl DiskBlobStore {
    /// Create a new `DiskBlobStore` rooted at the given directory, with the
    /// default free-space floor ([`DEFAULT_MIN_FREE_BYTES`]).
    ///
    /// Scans existing files to compute initial usage.
    pub fn new(root: &Path) -> Result<Self> {
        Self::new_with_min_free(root, DEFAULT_MIN_FREE_BYTES)
    }

    /// Create a `DiskBlobStore` with an explicit free-space floor (bytes). A
    /// floor of `0` disables the guard; `u64::MAX` refuses every new write (used
    /// by tests to exercise the refusal path without a real full disk).
    pub fn new_with_min_free(root: &Path, min_free_bytes: u64) -> Result<Self> {
        std::fs::create_dir_all(root)?;

        let mut total_bytes: u64 = 0;

        // Scan existing files to compute initial usage.
        // Walk {root}/{xx}/{yy}/{hash_hex}
        for shard1 in std::fs::read_dir(root)? {
            let shard1 = shard1?;
            if !shard1.file_type()?.is_dir() {
                continue;
            }
            for shard2 in std::fs::read_dir(shard1.path())? {
                let shard2 = shard2?;
                if !shard2.file_type()?.is_dir() {
                    continue;
                }
                for entry in std::fs::read_dir(shard2.path())? {
                    let entry = entry?;
                    if entry.file_type()?.is_file() {
                        total_bytes += entry.metadata()?.len();
                    }
                }
            }
        }

        Ok(Self {
            root: root.to_path_buf(),
            usage: AtomicU64::new(total_bytes),
            min_free_bytes,
        })
    }

    /// Compute the on-disk path for a given content hash.
    fn blob_path(&self, hash: &ContentHash) -> PathBuf {
        let hex = hex::encode(hash.digest());
        self.root.join(&hex[0..2]).join(&hex[2..4]).join(&hex)
    }

    /// Bytes available to an unprivileged writer on the filesystem holding the
    /// blob root (`statvfs.f_bavail * f_frsize`). Fail-open: if the stat fails
    /// (unsupported FS, transient error) we return `u64::MAX` so the guard never
    /// wrongly blocks writes on a healthy box — the guard's job is to catch the
    /// disk-fill, and `statvfs` is reliable on the Linux ext4 deploy target.
    ///
    /// On non-Unix (Windows is dev/test only — the nest deploys on Linux Docker
    /// /VPS, never Windows) the POSIX `statvfs` is unavailable, so we fail-open
    /// here as well: the guard exists to protect the Linux deploy target, and a
    /// Windows nest only ever runs in the tier_3 e2e suite where it must build
    /// and let writes through.
    fn available_bytes(&self) -> u64 {
        #[cfg(unix)]
        {
            match rustix::fs::statvfs(&self.root) {
                Ok(s) => s.f_bavail.saturating_mul(s.f_frsize),
                Err(e) => {
                    tracing::warn!(error = %e, "blob-store statvfs failed; disk guard fail-open");
                    u64::MAX
                }
            }
        }
        #[cfg(not(unix))]
        {
            u64::MAX
        }
    }
}

#[async_trait]
impl BlobStoreBackend for DiskBlobStore {
    async fn put(&self, hash: &ContentHash, data: &[u8]) -> Result<()> {
        let path = self.blob_path(hash);

        // Idempotent: skip if file already exists. This MUST stay above the disk
        // guard so reads/dedup/idempotent re-puts keep working on a full disk —
        // only a genuinely NEW write is gated.
        if path.exists() {
            return Ok(());
        }

        // Hard disk guard: refuse a NEW write that would drop free space below
        // the floor, so no writer (the backup scheduler, file-sync chunks,
        // anything) can ever drive the filesystem to ENOSPC and brick the box at
        // boot. (`docs/goal/behavior/backup-restore.md` § Blob-store disk guard.)
        if self.min_free_bytes > 0 {
            let available = self.available_bytes();
            if available < self.min_free_bytes.saturating_add(data.len() as u64) {
                anyhow::bail!(
                    "blob store refused write: {} bytes free < {} floor + {} blob (disk guard)",
                    available,
                    self.min_free_bytes,
                    data.len()
                );
            }
        }

        // Ensure parent directories exist.
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        // Write to a temp file in the same directory, then atomic rename.
        //
        // The temp name must be unique PER CALL and never derived from the hash
        // alone: concurrent puts of the *same* chunk are the normal case, not a
        // corner — N devices converging on one merged file each upload the
        // identical chunk at the same moment. A shared `.tmp.<hash>` makes them
        // collide, and the collision is silent-then-fatal: both write the one
        // path (a torn blob if the encodings differ), the first rename
        // publishes, and the loser's rename fails ENOENT because its temp file
        // was renamed out from under it. That surfaces as a 500 on an upload
        // the caller had every reason to expect to succeed — on 2026-08-02 it
        // cost a three-seat sync run its resolution publish, and the seat whose
        // conflict report never landed then lost its own edit.
        let parent = path.parent().expect("blob path always has a parent");
        let temp_path = parent.join(format!(
            ".tmp.{}.{}.{}",
            hex::encode(hash.digest()),
            std::process::id(),
            TEMP_SEQ.fetch_add(1, Ordering::Relaxed)
        ));

        // Don't leak the temp file on either failure — `list_all_hashes` skips
        // `.tmp.*`, so GC would never reclaim what we left behind.
        if let Err(e) = tokio::fs::write(&temp_path, data).await {
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(e.into());
        }
        if let Err(e) = tokio::fs::rename(&temp_path, &path).await {
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(e.into());
        }

        self.usage.fetch_add(data.len() as u64, Ordering::Relaxed);

        Ok(())
    }

    async fn get(&self, hash: &ContentHash) -> Result<Option<Vec<u8>>> {
        let path = self.blob_path(hash);
        match tokio::fs::read(&path).await {
            Ok(data) => Ok(Some(data)),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    async fn exists(&self, hash: &ContentHash) -> Result<bool> {
        let path = self.blob_path(hash);
        match tokio::fs::metadata(&path).await {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    async fn len(&self, hash: &ContentHash) -> Result<Option<u64>> {
        let path = self.blob_path(hash);
        match tokio::fs::metadata(&path).await {
            Ok(meta) => Ok(Some(meta.len())),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Seek + bounded read: a 99 MB video's seek costs the bytes asked for, not
    /// the file.
    async fn get_range(
        &self,
        hash: &ContentHash,
        start: u64,
        end_inclusive: u64,
    ) -> Result<Option<Vec<u8>>> {
        use tokio::io::{AsyncReadExt, AsyncSeekExt};
        let path = self.blob_path(hash);
        let mut file = match tokio::fs::File::open(&path).await {
            Ok(f) => f,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        if end_inclusive < start {
            return Ok(Some(Vec::new()));
        }
        file.seek(std::io::SeekFrom::Start(start)).await?;
        let want = end_inclusive - start + 1;
        let mut buf = Vec::with_capacity(want.min(16 * 1024 * 1024) as usize);
        file.take(want).read_to_end(&mut buf).await?;
        Ok(Some(buf))
    }

    async fn exists_batch(&self, hashes: &[ContentHash]) -> Result<Vec<bool>> {
        let mut results = Vec::with_capacity(hashes.len());
        for hash in hashes {
            results.push(self.exists(hash).await?);
        }
        Ok(results)
    }

    async fn delete(&self, hash: &ContentHash) -> Result<()> {
        let path = self.blob_path(hash);
        match tokio::fs::metadata(&path).await {
            Ok(meta) => {
                let size = meta.len();
                tokio::fs::remove_file(&path).await?;
                self.usage.fetch_sub(size, Ordering::Relaxed);
                Ok(())
            }
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    async fn usage_bytes(&self) -> Result<u64> {
        Ok(self.usage.load(Ordering::Relaxed))
    }

    async fn list_all_hashes(&self) -> Result<Vec<ContentHash>> {
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || {
            let mut hashes = Vec::new();

            let shard1_iter = match std::fs::read_dir(&root) {
                Ok(iter) => iter,
                Err(e) if e.kind() == ErrorKind::NotFound => return Ok(hashes),
                Err(e) => return Err(e.into()),
            };

            for shard1 in shard1_iter {
                let shard1 = shard1?;
                if !shard1.file_type()?.is_dir() {
                    continue;
                }

                for shard2 in std::fs::read_dir(shard1.path())? {
                    let shard2 = shard2?;
                    if !shard2.file_type()?.is_dir() {
                        continue;
                    }

                    for entry in std::fs::read_dir(shard2.path())? {
                        let entry = entry?;
                        if !entry.file_type()?.is_file() {
                            continue;
                        }

                        let name = entry.file_name();
                        let name = name.to_string_lossy();

                        // Skip temp files written during put().
                        if name.starts_with(".tmp.") {
                            continue;
                        }

                        // Parse hex filename into ContentHash.
                        if let Ok(arr) = fauna_core::hex32::decode(name.as_ref()) {
                            hashes.push(ContentHash::from_digest_raw(arr));
                        }
                    }
                }
            }

            Ok(hashes)
        })
        .await?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn disk_blob_store_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = DiskBlobStore::new(dir.path()).unwrap();

        let data = b"hello world";
        let hash = ContentHash::of_raw(data);

        assert!(!store.exists(&hash).await.unwrap());
        assert!(store.get(&hash).await.unwrap().is_none());

        store.put(&hash, data).await.unwrap();
        assert!(store.exists(&hash).await.unwrap());
        let got = store.get(&hash).await.unwrap().unwrap();
        assert_eq!(got, data);

        assert_eq!(store.usage_bytes().await.unwrap(), data.len() as u64);

        store.delete(&hash).await.unwrap();
        assert!(!store.exists(&hash).await.unwrap());
        assert_eq!(store.usage_bytes().await.unwrap(), 0);
    }

    /// The disk override seeks: it returns exactly `[start, end]`, clamps an end
    /// past the last byte, answers `None` for an unheld blob — and agrees with
    /// the trait's slice-of-`get` default byte for byte.
    #[tokio::test]
    async fn disk_get_range_reads_exactly_the_requested_bytes() {
        struct ViaDefault<'a>(&'a DiskBlobStore);
        #[async_trait]
        impl BlobStoreBackend for ViaDefault<'_> {
            async fn put(&self, h: &ContentHash, d: &[u8]) -> Result<()> {
                self.0.put(h, d).await
            }
            async fn get(&self, h: &ContentHash) -> Result<Option<Vec<u8>>> {
                self.0.get(h).await
            }
            async fn exists(&self, h: &ContentHash) -> Result<bool> {
                self.0.exists(h).await
            }
            async fn exists_batch(&self, h: &[ContentHash]) -> Result<Vec<bool>> {
                self.0.exists_batch(h).await
            }
            async fn delete(&self, h: &ContentHash) -> Result<()> {
                self.0.delete(h).await
            }
            async fn usage_bytes(&self) -> Result<u64> {
                self.0.usage_bytes().await
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let store = DiskBlobStore::new(dir.path()).unwrap();
        let data: Vec<u8> = (0u8..=199).collect();
        let hash = ContentHash::of_raw(&data);
        assert!(store.get_range(&hash, 0, 3).await.unwrap().is_none());
        store.put(&hash, &data).await.unwrap();

        let fallback = ViaDefault(&store);
        for (start, end, want) in [
            (0u64, 3u64, &data[0..=3]),
            (10, 10, &data[10..=10]),
            (150, 199, &data[150..]),
            (150, 10_000, &data[150..]),
        ] {
            let got = store.get_range(&hash, start, end).await.unwrap().unwrap();
            assert_eq!(got, want, "disk [{start}, {end}]");
            let via = fallback
                .get_range(&hash, start, end)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(via, want, "default [{start}, {end}]");
        }
    }

    /// Concurrent puts of the SAME blob all succeed and publish intact content.
    ///
    /// This is the everyday shape, not a corner: N devices converging on one
    /// merged file upload the identical chunk simultaneously. While `put` derived
    /// its temp filename from the hash alone, every writer but one failed
    /// `rename` with ENOENT (its temp file renamed away by the winner) — a 500
    /// to the uploader, which on 2026-08-02 cost a three-seat sync run its
    /// resolution publish. Asserts state, not timing: every task must report
    /// `Ok`, and the published blob must be exactly the bytes written.
    #[tokio::test]
    async fn disk_blob_store_concurrent_put_of_same_blob_all_succeed() {
        let dir = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(DiskBlobStore::new(dir.path()).unwrap());

        // Large enough that a torn interleave would be visible if two writers
        // ever shared one temp path again.
        let data = std::sync::Arc::new(vec![0xABu8; 256 * 1024]);
        let hash = ContentHash::of_raw(&data);

        let mut tasks = Vec::new();
        for _ in 0..16 {
            let (store, data) = (store.clone(), data.clone());
            // spawn-ok(test)
            tasks.push(tokio::spawn(async move { store.put(&hash, &data).await }));
        }
        for (i, task) in tasks.into_iter().enumerate() {
            task.await
                .expect("put task panicked")
                .unwrap_or_else(|e| panic!("concurrent put {i} failed: {e}"));
        }

        assert_eq!(
            store.get(&hash).await.unwrap().as_deref(),
            Some(&data[..]),
            "the published blob is not the bytes that were written"
        );

        // No `.tmp.*` leftovers: every writer cleaned up or renamed its own file.
        let hex_hash = hex::encode(hash.digest());
        let shard = dir.path().join(&hex_hash[0..2]).join(&hex_hash[2..4]);
        let mut entries = tokio::fs::read_dir(&shard).await.unwrap();
        while let Some(entry) = entries.next_entry().await.unwrap() {
            let name = entry.file_name().to_string_lossy().into_owned();
            assert!(!name.starts_with(".tmp."), "leaked temp file: {name}");
        }
    }

    #[tokio::test]
    async fn disk_blob_store_exists_batch() {
        let dir = tempfile::tempdir().unwrap();
        let store = DiskBlobStore::new(dir.path()).unwrap();

        let data1 = b"aaa";
        let data2 = b"bbb";
        let hash1 = ContentHash::of_raw(data1);
        let hash2 = ContentHash::of_raw(data2);
        let hash3 = ContentHash::of_raw(b"ccc");

        store.put(&hash1, data1).await.unwrap();
        store.put(&hash2, data2).await.unwrap();

        let results = store.exists_batch(&[hash1, hash2, hash3]).await.unwrap();
        assert_eq!(results, vec![true, true, false]);
    }

    #[tokio::test]
    async fn disk_blob_store_idempotent_put() {
        let dir = tempfile::tempdir().unwrap();
        let store = DiskBlobStore::new(dir.path()).unwrap();

        let data = b"duplicate";
        let hash = ContentHash::of_raw(data);

        store.put(&hash, data).await.unwrap();
        store.put(&hash, data).await.unwrap(); // idempotent
        assert_eq!(store.usage_bytes().await.unwrap(), data.len() as u64);
    }

    #[tokio::test]
    async fn disk_blob_store_list_all_hashes() {
        let dir = tempfile::tempdir().unwrap();
        let store = DiskBlobStore::new(dir.path()).unwrap();

        let data1 = b"blob-one";
        let data2 = b"blob-two";
        let hash1 = ContentHash::of_raw(data1);
        let hash2 = ContentHash::of_raw(data2);

        store.put(&hash1, data1).await.unwrap();
        store.put(&hash2, data2).await.unwrap();

        let mut hashes = store.list_all_hashes().await.unwrap();
        hashes.sort_by_key(|h| h.digest());

        let mut expected = vec![hash1, hash2];
        expected.sort_by_key(|h| h.digest());

        assert_eq!(hashes, expected);
    }

    #[tokio::test]
    async fn disk_blob_store_list_all_hashes_empty() {
        let dir = tempfile::tempdir().unwrap();
        let store = DiskBlobStore::new(dir.path()).unwrap();

        let hashes = store.list_all_hashes().await.unwrap();
        assert!(hashes.is_empty());
    }

    /// The disk guard refuses a NEW write when free space would fall below the
    /// floor (modeled with an impossible `u64::MAX` floor). This is the backstop
    /// that makes the disk-fill outage class unrepresentable.
    ///
    /// Unix-only: `available_bytes` fails open to `u64::MAX` on non-Unix (the
    /// nest deploys on Linux; Windows is dev/test), so the guard is compiled out
    /// there and there is no refusal to assert. Without this gate the test fails
    /// unconditionally on Windows.
    #[cfg(unix)]
    #[tokio::test]
    async fn disk_guard_refuses_new_write_below_floor() {
        let dir = tempfile::tempdir().unwrap();
        let store = DiskBlobStore::new_with_min_free(dir.path(), u64::MAX).unwrap();
        let data = b"x";
        let hash = ContentHash::of_raw(data);
        assert!(
            store.put(&hash, data).await.is_err(),
            "put must be refused when free space is below the floor"
        );
        assert!(
            !store.exists(&hash).await.unwrap(),
            "a refused blob must not be written to disk"
        );
    }

    /// A zero floor disables the guard — normal writes proceed.
    #[tokio::test]
    async fn disk_guard_zero_floor_allows_writes() {
        let dir = tempfile::tempdir().unwrap();
        let store = DiskBlobStore::new_with_min_free(dir.path(), 0).unwrap();
        let data = b"y";
        let hash = ContentHash::of_raw(data);
        store.put(&hash, data).await.unwrap();
        assert!(store.exists(&hash).await.unwrap());
    }

    /// Idempotent re-put of an ALREADY-stored blob must succeed even below the
    /// floor — it writes nothing, and reads/dedup/deletes must keep working on a
    /// full disk so the box stays recoverable.
    #[tokio::test]
    async fn disk_guard_allows_reput_of_existing_blob_when_full() {
        let dir = tempfile::tempdir().unwrap();
        let data = b"z";
        let hash = ContentHash::of_raw(data);
        // Write it first with the guard disabled.
        DiskBlobStore::new_with_min_free(dir.path(), 0)
            .unwrap()
            .put(&hash, data)
            .await
            .unwrap();
        // Now an impossible floor must NOT block the idempotent re-put.
        let full = DiskBlobStore::new_with_min_free(dir.path(), u64::MAX).unwrap();
        full.put(&hash, data)
            .await
            .expect("idempotent re-put of an existing blob must not be refused");
    }
}
