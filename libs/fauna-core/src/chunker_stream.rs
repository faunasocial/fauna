//! Streaming chunker for large files.
//!
//! Unlike [`crate::chunker`] which requires the entire file to be loaded into memory,
//! these functions operate with O(MAX_CHUNK) memory — roughly 16 MB worst case
//! (a `2 * MAX_CHUNK` rolling buffer) — making them suitable for files of
//! arbitrary size.

use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result};

use crate::chunk::ChunkManifest;
use crate::chunker::{AVG_CHUNK, MAX_CHUNK, MIN_CHUNK};
use crate::data::ContentHash;

/// Files at or above this size should be processed with the streaming chunker.
pub const STREAMING_THRESHOLD: u64 = 64 * 1024 * 1024;

/// Chunk a file from disk using FastCDC and return a [`ChunkManifest`].
///
/// Individual chunk files are written to `chunk_dir/{hex_hash}`. The function
/// uses a rolling buffer of at most `2 * MAX_CHUNK` bytes, so memory usage is
/// O(MAX_CHUNK) regardless of file size.
///
/// Idempotent: if a chunk file already exists it is not overwritten.
pub fn chunk_file_streaming(path: &Path, chunk_dir: &Path) -> Result<ChunkManifest> {
    let mut file = std::fs::File::open(path)?;
    let file_size = file.metadata()?.len();
    let mut file_hasher = blake3::Hasher::new();

    // Rolling accumulation buffer — bounded to ~2 * MAX_CHUNK bytes.
    let buf_capacity = 2 * MAX_CHUNK as usize;
    let mut buf: Vec<u8> = Vec::with_capacity(buf_capacity);
    let mut chunk_hashes: Vec<ContentHash> = Vec::new();
    let mut chunk_sizes: Vec<u64> = Vec::new();
    let mut total_read: u64 = 0;

    std::fs::create_dir_all(chunk_dir)?;

    // Small read buffer to avoid large stack allocations.
    let mut read_buf = vec![0u8; 64 * 1024];

    loop {
        let n = file.read(&mut read_buf)?;
        if n == 0 {
            break;
        }

        file_hasher.update(&read_buf[..n]);
        buf.extend_from_slice(&read_buf[..n]);
        total_read += n as u64;

        let at_eof = total_read >= file_size;

        // Process when the buffer is full enough or we reached end-of-file.
        if buf.len() >= buf_capacity || at_eof {
            let chunks: Vec<_> =
                fastcdc::v2020::FastCDC::new(&buf, MIN_CHUNK, AVG_CHUNK, MAX_CHUNK).collect();

            let last_idx = chunks.len().saturating_sub(1);
            let mut consumed = 0usize;

            for (i, chunk) in chunks.iter().enumerate() {
                // Skip the last chunk unless we are at EOF — it may be incomplete.
                if i == last_idx && !at_eof {
                    break;
                }

                let chunk_data = &buf[chunk.offset..chunk.offset + chunk.length];
                let hash = ContentHash::of_raw(chunk_data);

                let chunk_path = chunk_dir.join(hex::encode(hash.digest()));
                if !chunk_path.exists() {
                    std::fs::write(&chunk_path, chunk_data)?;
                }

                chunk_hashes.push(hash);
                chunk_sizes.push(chunk.length as u64);
                consumed = chunk.offset + chunk.length;
            }

            // Retain unprocessed tail for the next iteration.
            buf = buf[consumed..].to_vec();
        }
    }

    // Any remaining data in the buffer forms the final (possibly partial) chunk.
    if !buf.is_empty() {
        let hash = ContentHash::of_raw(&buf);
        let chunk_path = chunk_dir.join(hex::encode(hash.digest()));
        if !chunk_path.exists() {
            std::fs::write(&chunk_path, &buf)?;
        }
        chunk_hashes.push(hash);
        chunk_sizes.push(buf.len() as u64);
    }

    let file_hash = ContentHash::from_digest_raw(*file_hasher.finalize().as_bytes());

    Ok(ChunkManifest {
        file_hash,
        total_size: total_read,
        chunk_hashes,
        chunk_sizes,
        stored_hashes: None, // plaintext: store key == plaintext hash
        sealed_hashes: None,
        min_reader: None,
    })
}

/// Compute the BLAKE3 hash of a file by reading it in 1 MB blocks.
///
/// Used for quick change-detection without chunking the file.
///
/// **The single source of the streamed file-hash** — every other surface that
/// needs one wraps this rather than re-rolling the read loop: the sync engine's
/// reconcile/delete paths call it directly, and the UniFFI door
/// `fauna_ffi::content_hash_at_path` is a thin byte-vec wrapper over it. Block
/// size does not affect the digest, so a second copy with its own buffer size
/// is invisible until it drifts on something that *does* matter (error shape,
/// symlink handling, a future `mmap` fast path) — which is why the copies were
/// consolidated here rather than left to agree by coincidence.
///
/// The `opening` context is load-bearing: a bare `io::Error` from the open does
/// not name the file, and the sync engine's remote-delete arm logs this error
/// verbatim when it decides whether to keep a file it cannot re-hash. It stays
/// on the *open* alone, which is why the shared loop below takes an already
/// opened reader rather than a path.
pub fn content_hash_streaming(path: &Path) -> Result<ContentHash> {
    let file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    Ok(ContentHash::from_digest_raw(blake3_of_reader(file)?))
}

/// Stream `reader` through BLAKE3 and return the raw 32-byte digest — the read
/// loop behind [`content_hash_streaming`], shared with callers that need the
/// bare digest, their own error type, or a reader that is not a path.
///
/// It takes a **reader, not a path**, on purpose: [`content_hash_streaming`]'s
/// `opening` context must stay on the open (see its doc), and a path-taking
/// helper would either lose that distinction or force every other caller into
/// `anyhow`.
///
/// ⚠ Its existence is the correction to a claim this module used to make about
/// itself. [`content_hash_streaming`] called itself *"the single source of the
/// streamed file-hash — every other surface that needs one wraps this rather
/// than re-rolling the read loop"*, and that was **false** when written:
/// `fauna_segment_store::Segment::file_blake3` re-rolled the loop with its own
/// 64 KiB buffer, in a crate that already depends on this one, because it wants
/// `[u8; 32]` and `SegmentStoreError` rather than a CID and `anyhow` (found
/// 2026-08-23). The block size is digest-irrelevant, so the copy was invisible
/// until it drifted on something that is not — which is the same argument the
/// consolidation was made on in the first place.
pub fn blake3_of_reader(mut reader: impl Read) -> std::io::Result<[u8; 32]> {
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; 1024 * 1024];

    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }

    Ok(*hasher.finalize().as_bytes())
}

/// Reassemble a file from chunk files on disk, writing the result to `output`.
///
/// `chunks` is a slice of `(hash, chunk_path)` pairs in manifest order.
/// After writing all chunks the final file hash is verified against `file_hash`.
///
/// At most one chunk (≤ MAX_CHUNK, 8 MB) is held in memory at a time.
// The `drop(out)` below is load-bearing off wasm: it closes the handle so the
// verification re-read sees a fully-flushed file. On wasm32 `std::fs::File` is a
// stub with no `Drop` impl, so `clippy::drop_non_drop` fires there — a false
// positive we silence per-target rather than deleting a drop the native path
// depends on. (Nothing gates wasm32 clippy today; this keeps a manual run clean.)
#[cfg_attr(target_arch = "wasm32", allow(clippy::drop_non_drop))]
pub fn reassemble_streaming(
    chunks: &[(ContentHash, &Path)],
    output: &Path,
    file_hash: &ContentHash,
) -> Result<()> {
    use std::io::Write;

    let mut out = std::fs::File::create(output)?;

    for (_hash, chunk_path) in chunks {
        let data = std::fs::read(chunk_path)?;
        out.write_all(&data)?;
        // `data` is dropped here — at most one chunk in memory.
    }

    drop(out);

    // Verify the assembled file matches the expected hash.
    let actual = content_hash_streaming(output)?;
    anyhow::ensure!(
        actual == *file_hash,
        "reassembled file hash mismatch: expected {}, got {}",
        hex::encode(file_hash.digest()),
        hex::encode(actual.digest()),
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Write `data` to a temporary file and return its path.
    fn write_temp_file(data: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("testfile.bin");
        std::fs::File::create(&path)
            .expect("create")
            .write_all(data)
            .expect("write");
        (dir, path)
    }

    // -----------------------------------------------------------------------
    // content_hash_streaming
    // -----------------------------------------------------------------------

    #[test]
    fn content_hash_streaming_matches_blake3() {
        // 2 MB — multiple 1 MB read blocks.
        let data = vec![0x55u8; 2 * 1024 * 1024];
        let (dir, path) = write_temp_file(&data);
        let expected = ContentHash::of_raw(&data);
        let got = content_hash_streaming(&path).expect("hash");
        assert_eq!(got, expected);
        drop(dir);
    }

    #[test]
    fn content_hash_streaming_small_file() {
        let data = b"hello fauna";
        let (dir, path) = write_temp_file(data);
        let expected = ContentHash::of_raw(data);
        let got = content_hash_streaming(&path).expect("hash");
        assert_eq!(got, expected);
        drop(dir);
    }

    #[test]
    fn content_hash_streaming_names_the_file_it_could_not_open() {
        // The sync engine's remote-delete arm logs this error verbatim when it
        // cannot re-hash a file, and a bare io::Error names no path — so the
        // operator would see "No such file or directory" about nothing.
        let missing = Path::new("/nonexistent-dir-for-fauna-test/absent-file.bin");
        let err = content_hash_streaming(missing).expect_err("a missing file cannot hash");
        assert!(
            format!("{err:#}").contains("absent-file.bin"),
            "error should name the path, got: {err:#}"
        );
    }

    // -----------------------------------------------------------------------
    // chunk_file_streaming
    // -----------------------------------------------------------------------

    #[test]
    fn streaming_manifest_matches_inmemory_large() {
        // 80 MB — well above STREAMING_THRESHOLD.
        // Use a simple pattern so both chunkers see identical bytes.
        let size = 80 * 1024 * 1024usize;
        let data: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();

        let chunk_dir = tempfile::tempdir().expect("chunk_dir");
        let (file_dir, path) = write_temp_file(&data);

        let streaming = chunk_file_streaming(&path, chunk_dir.path()).expect("streaming");
        let inmemory = crate::chunker::chunk_file(&data);

        assert_eq!(
            streaming.file_hash, inmemory.file_hash,
            "file_hash mismatch"
        );
        assert_eq!(
            streaming.total_size, inmemory.total_size,
            "total_size mismatch"
        );
        assert_eq!(
            streaming.chunk_hashes, inmemory.chunk_hashes,
            "chunk_hashes mismatch"
        );
        assert_eq!(
            streaming.chunk_sizes, inmemory.chunk_sizes,
            "chunk_sizes mismatch"
        );

        drop(chunk_dir);
        drop(file_dir);
    }

    #[test]
    fn streaming_chunk_sizes_sum_to_total() {
        let data = vec![0xAAu8; 80 * 1024 * 1024];
        let chunk_dir = tempfile::tempdir().expect("chunk_dir");
        let (file_dir, path) = write_temp_file(&data);

        let manifest = chunk_file_streaming(&path, chunk_dir.path()).expect("manifest");
        let sum: u64 = manifest.chunk_sizes.iter().sum();
        assert_eq!(sum, manifest.total_size);

        drop(chunk_dir);
        drop(file_dir);
    }

    #[test]
    fn streaming_small_file() {
        // Below STREAMING_THRESHOLD — still works correctly.
        let data = b"small file for streaming test";
        let chunk_dir = tempfile::tempdir().expect("chunk_dir");
        let (file_dir, path) = write_temp_file(data);

        let manifest = chunk_file_streaming(&path, chunk_dir.path()).expect("manifest");
        assert_eq!(manifest.total_size, data.len() as u64);
        assert_eq!(manifest.file_hash, ContentHash::of_raw(data));
        let sum: u64 = manifest.chunk_sizes.iter().sum();
        assert_eq!(sum, data.len() as u64);

        drop(chunk_dir);
        drop(file_dir);
    }

    #[test]
    fn streaming_idempotent_chunk_files() {
        let data = vec![0xBBu8; 80 * 1024 * 1024];
        let chunk_dir = tempfile::tempdir().expect("chunk_dir");
        let (file_dir, path) = write_temp_file(&data);

        // Run twice — second pass must not fail even though chunk files exist.
        let m1 = chunk_file_streaming(&path, chunk_dir.path()).expect("first");
        let m2 = chunk_file_streaming(&path, chunk_dir.path()).expect("second");
        assert_eq!(m1.chunk_hashes, m2.chunk_hashes);

        drop(chunk_dir);
        drop(file_dir);
    }

    // -----------------------------------------------------------------------
    // reassemble_streaming
    // -----------------------------------------------------------------------

    #[test]
    fn reassemble_streaming_roundtrip() {
        let size = 80 * 1024 * 1024usize;
        let data: Vec<u8> = (0..size).map(|i| (i % 199) as u8).collect();

        let chunk_dir = tempfile::tempdir().expect("chunk_dir");
        let (file_dir, path) = write_temp_file(&data);

        let manifest = chunk_file_streaming(&path, chunk_dir.path()).expect("manifest");

        // Build (hash, path) pairs for reassembly.
        let chunk_paths: Vec<std::path::PathBuf> = manifest
            .chunk_hashes
            .iter()
            .map(|h| chunk_dir.path().join(hex::encode(h.digest())))
            .collect();
        let chunks: Vec<(ContentHash, &Path)> = manifest
            .chunk_hashes
            .iter()
            .copied()
            .zip(chunk_paths.iter().map(|p| p.as_path()))
            .collect();

        let out_dir = tempfile::tempdir().expect("out_dir");
        let out_path = out_dir.path().join("reassembled.bin");

        reassemble_streaming(&chunks, &out_path, &manifest.file_hash).expect("reassemble");

        let reassembled = std::fs::read(&out_path).expect("read reassembled");
        assert_eq!(reassembled.len(), data.len());
        assert_eq!(ContentHash::of_raw(&reassembled), manifest.file_hash);

        drop(chunk_dir);
        drop(file_dir);
        drop(out_dir);
    }

    #[test]
    fn reassemble_streaming_hash_mismatch_fails() {
        let data = b"tiny file";
        let chunk_dir = tempfile::tempdir().expect("chunk_dir");
        let (file_dir, path) = write_temp_file(data);

        let manifest = chunk_file_streaming(&path, chunk_dir.path()).expect("manifest");

        let chunk_paths: Vec<std::path::PathBuf> = manifest
            .chunk_hashes
            .iter()
            .map(|h| chunk_dir.path().join(hex::encode(h.digest())))
            .collect();
        let chunks: Vec<(ContentHash, &Path)> = manifest
            .chunk_hashes
            .iter()
            .copied()
            .zip(chunk_paths.iter().map(|p| p.as_path()))
            .collect();

        let out_dir = tempfile::tempdir().expect("out_dir");
        let out_path = out_dir.path().join("out.bin");

        // Pass a deliberately wrong file hash.
        let wrong_hash = ContentHash::from_digest_raw([0u8; 32]);
        let result = reassemble_streaming(&chunks, &out_path, &wrong_hash);
        assert!(result.is_err(), "expected error for hash mismatch");

        drop(chunk_dir);
        drop(file_dir);
        drop(out_dir);
    }
}
