use fauna_client_core::chunking as core_chunking;
use fauna_core::chunk::ChunkManifest;
use fauna_core::data::ContentHash;

use crate::{FfiError, bytes_to_content_hash, general_err};

/// A file manifest describing chunks.
#[derive(uniffi::Record, Clone)]
pub struct FfiManifest {
    pub file_hash: Vec<u8>,
    pub total_size: u64,
    /// BLAKE3 digest of each **plaintext** chunk (the AEAD key/nonce salt +
    /// integrity anchor). **Empty** (and `file_hash` the all-zero digest) for a
    /// content-key-sealed manifest decoded by [`deserialize_manifest`]: those
    /// ride only in the manifest's sealed hashes, which this record does not
    /// carry — open a sealed file through `webdav_open_file` (the shared walk),
    /// never by these fields (`mls-group-key-material.md` § M2 *Sealed
    /// manifest hashes*).
    pub chunk_hashes: Vec<Vec<u8>>,
    pub chunk_sizes: Vec<u64>,
    /// For a **content-key-sealed** (M2) folder: BLAKE3 digest of each
    /// *ciphertext* chunk — the content-addressed blob-store key the byte
    /// routes use (`manifest.store_keys()`), parallel to `chunk_hashes`. `None`
    /// for the plaintext-chunking path (owner-only / no content key). The Go
    /// WebDAV MDA receives it populated from `webdav_seal_file` on PUT and
    /// reads it on GET to fetch each ciphertext chunk by store key before
    /// handing the bodies to `webdav_open_file`.
    pub stored_hashes: Option<Vec<Vec<u8>>>,
}

/// A chunk: hash + data bytes.
#[derive(uniffi::Record)]
pub struct FfiChunkItem {
    pub hash: Vec<u8>,
    pub data: Vec<u8>,
}

/// Result of chunking a file at a path: manifest + temporary chunk directory.
#[derive(uniffi::Record)]
pub struct FfiChunkResult {
    pub manifest: FfiManifest,
    pub chunk_dir: Option<String>,
}

impl From<&ChunkManifest> for FfiManifest {
    fn from(m: &ChunkManifest) -> Self {
        FfiManifest {
            file_hash: m.file_hash.digest().to_vec(),
            total_size: m.total_size,
            chunk_hashes: m.chunk_hashes.iter().map(|h| h.digest().to_vec()).collect(),
            chunk_sizes: m.chunk_sizes.clone(),
            stored_hashes: m
                .stored_hashes
                .as_ref()
                .map(|v| v.iter().map(|h| h.digest().to_vec()).collect()),
        }
    }
}

impl FfiManifest {
    pub(crate) fn to_manifest(&self) -> Result<ChunkManifest, FfiError> {
        let file_hash = bytes_to_content_hash(&self.file_hash)?;
        let chunk_hashes: Result<Vec<ContentHash>, FfiError> = self
            .chunk_hashes
            .iter()
            .map(|h| bytes_to_content_hash(h))
            .collect();
        let stored_hashes: Option<Vec<ContentHash>> = match &self.stored_hashes {
            Some(hs) => Some(
                hs.iter()
                    .map(|h| bytes_to_content_hash(h))
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            None => None,
        };
        Ok(ChunkManifest {
            file_hash,
            total_size: self.total_size,
            chunk_hashes: chunk_hashes?,
            chunk_sizes: self.chunk_sizes.clone(),
            // Content-key store keys (ciphertext hashes), threaded from the Go
            // WebDAV MDA's PUT path so the serialized manifest is byte-identical
            // to the sync engine's — `mls-group-key-material.md` § M2.
            stored_hashes,
            sealed_hashes: None,
            min_reader: None,
        })
    }
}

/// Chunk file data using FastCDC. Returns typed manifest.
#[uniffi::export]
pub fn chunk_file(data: Vec<u8>) -> FfiManifest {
    let manifest = core_chunking::chunk_data(&data);
    FfiManifest::from(&manifest)
}

/// Extract individual chunk data from file + manifest.
#[uniffi::export]
pub fn extract_chunks(data: Vec<u8>, manifest: FfiManifest) -> Result<Vec<FfiChunkItem>, FfiError> {
    let m = manifest.to_manifest()?;
    let chunks = core_chunking::extract_chunks(&data, &m);
    Ok(chunks
        .iter()
        .map(|(hash, chunk_data)| FfiChunkItem {
            hash: hash.digest().to_vec(),
            data: chunk_data.clone(),
        })
        .collect())
}

/// Reassemble file from chunk items (inverse of extract_chunks).
#[uniffi::export]
pub fn reassemble_chunks(chunks: Vec<FfiChunkItem>) -> Result<Vec<u8>, FfiError> {
    let chunk_data: Vec<Vec<u8>> = chunks.into_iter().map(|item| item.data).collect();
    Ok(core_chunking::reassemble_chunks(&chunk_data))
}

/// Chunk a file by path. Writes each chunk to a temp directory.
#[uniffi::export]
pub fn chunk_file_at_path(path: String) -> Result<FfiChunkResult, FfiError> {
    let data = std::fs::read(&path).map_err(|e| FfiError::General {
        msg: format!("read {path}: {e}"),
    })?;
    let manifest = core_chunking::chunk_data(&data);
    let chunks = core_chunking::extract_chunks(&data, &manifest);

    let chunk_dir = tempfile::tempdir().map_err(general_err)?;
    for (hash, chunk_data) in &chunks {
        let p = chunk_dir
            .path()
            .join(format!("{}.bin", hex::encode(hash.digest())));
        std::fs::write(&p, chunk_data).map_err(general_err)?;
    }

    let dir_str = chunk_dir.keep().to_string_lossy().to_string();
    Ok(FfiChunkResult {
        manifest: FfiManifest::from(&manifest),
        chunk_dir: Some(dir_str),
    })
}

/// BLAKE3 hash of a file by path. Returns 32 bytes.
///
/// A thin door over [`fauna_core::chunker_stream::content_hash_streaming`] — the
/// single source of the streamed file hash. This used to re-roll the same read
/// loop, which is invisible while both agree and a silent split the moment one
/// side gains a symlink rule or an `mmap` path; the digest is identical either
/// way, so nothing would have failed to warn us.
#[uniffi::export]
pub fn content_hash_at_path(path: String) -> Result<Vec<u8>, FfiError> {
    let hash = fauna_core::chunker_stream::content_hash_streaming(std::path::Path::new(&path))
        .map_err(|e| FfiError::General {
            msg: format!("{e:#}"),
        })?;
    // `digest()`, not `as_bytes()`: this door's contract is the RAW 32-byte
    // BLAKE3 digest, while `as_bytes()` is the 36-byte CID form (a 4-byte
    // codec/multihash prefix ahead of the same digest).
    Ok(hash.digest().to_vec())
}

/// Reassemble a file from chunks on disk and write to output_path.
#[uniffi::export]
pub fn reassemble_chunks_to_path(
    chunk_dir: String,
    manifest: FfiManifest,
    output_path: String,
) -> Result<(), FfiError> {
    use std::io::Write;
    let m = manifest.to_manifest()?;
    let mut out = std::fs::File::create(&output_path).map_err(general_err)?;
    for hash in &m.chunk_hashes {
        let p = format!("{}/{}.bin", chunk_dir, hex::encode(hash.digest()));
        let data = std::fs::read(&p).map_err(|e| FfiError::General {
            msg: format!("read chunk: {e}"),
        })?;
        out.write_all(&data).map_err(general_err)?;
    }
    Ok(())
}

/// BLAKE3 hash of arbitrary data, returned as 32 bytes.
#[uniffi::export]
pub fn ffi_content_hash(data: Vec<u8>) -> Vec<u8> {
    core_chunking::content_hash_bytes(&data).to_vec()
}

/// Generate a random 32-byte device ID.
#[uniffi::export]
pub fn generate_device_id() -> Vec<u8> {
    core_chunking::generate_device_id().to_vec()
}

/// Serialize a manifest to canonical dag-cbor bytes.
#[uniffi::export]
pub fn serialize_manifest(manifest: FfiManifest) -> Result<Vec<u8>, FfiError> {
    let m = manifest.to_manifest()?;
    core_chunking::serialize_manifest(&m).map_err(|e| FfiError::General { msg: e.0 })
}

/// Deserialize canonical dag-cbor manifest bytes to typed manifest.
#[uniffi::export]
pub fn deserialize_manifest(data: Vec<u8>) -> Result<FfiManifest, FfiError> {
    let manifest: ChunkManifest =
        core_chunking::deserialize_manifest(&data).map_err(|e| FfiError::General { msg: e.0 })?;
    Ok(FfiManifest::from(&manifest))
}

// NOTE: there is deliberately NO per-chunk `encrypt_chunk` / `decrypt_chunk`
// FFI export any more (removed 2026-09-03). The pair let the Go WebDAV MDA
// seal a RAW chunk while every Rust writer sealed the FRAMED one — two
// plaintexts under one deterministic (key, nonce), a two-time pad that
// recovers user content with no key (`fauna_core::chunk_seal`, module doc).
// The MDA now seals whole files through `mail::webdav_seal_file` (the sync
// engine's `seal_blob`) and opens them through `mail::webdav_open_file` (the
// apps' shared `download_file_bytes_by_manifest` walk); a bare AEAD door on
// the Go face is exactly the third implementation that drifts.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_hashes_round_trip_through_serialize() {
        // A content-key-sealed manifest carries both plaintext `chunk_hashes`
        // and ciphertext `stored_hashes`; the Go WebDAV MDA relies on both
        // surviving serialize→deserialize byte-identically to the sync engine.
        let plain = ffi_content_hash(b"plaintext-chunk".to_vec());
        let stored = ffi_content_hash(b"ciphertext-chunk".to_vec());
        let m = FfiManifest {
            file_hash: ffi_content_hash(b"whole-file".to_vec()),
            total_size: 15,
            chunk_hashes: vec![plain.clone()],
            chunk_sizes: vec![15],
            stored_hashes: Some(vec![stored.clone()]),
        };
        let bytes = serialize_manifest(m).unwrap();
        let back = deserialize_manifest(bytes).unwrap();
        assert_eq!(back.chunk_hashes, vec![plain]);
        assert_eq!(back.stored_hashes, Some(vec![stored]));
    }
}
