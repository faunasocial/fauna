use crate::chunk::ChunkManifest;
use crate::data::ContentHash;

/// Minimum chunk size: 512 KB
pub(crate) const MIN_CHUNK: u32 = 512 * 1024;
/// Average (target) chunk size: 2 MB
pub(crate) const AVG_CHUNK: u32 = 2 * 1024 * 1024;
/// Maximum chunk size: 8 MB.
///
/// This is the load-bearing **content-addressed-storage size invariant**: no
/// chunk the chunker ever produces exceeds `MAX_CHUNK`. It is deliberately held
/// ≤ the smallest tier's "max blob" limit (Free = 10 MB; see
/// `docs/goal/behavior/file-sync.md` § Content-Addressed Storage and
/// `backup-restore.md`), so every chunk is a storable blob on every tier and the
/// nest transport can be sized once for it (the `/chunks` HTTP body limit).
/// Chunk size is tier-*independent* — a tier-dependent max would fork the chunk
/// boundaries of the same file per tier and break content-addressed dedup.
pub(crate) const MAX_CHUNK: u32 = 8 * 1024 * 1024;
/// The largest body the content-addressed store ever holds under one chunk
/// store key: a [`MAX_CHUNK`] plaintext, framed uncompressed (one prefix byte —
/// `compress::compress_chunk_framed` stores raw whenever zstd would not shrink
/// the chunk, so the frame never adds more than that) and sealed by
/// `chunk_crypto::encrypt_chunk` (ChaCha20-Poly1305 with no nonce prefix: a
/// 16-byte tag). The other shapes the store holds sit under it: a
/// plaintext-manifest body is the framed chunk alone, and a raw-body seal (the
/// settled raw read) is the chunk plus the tag.
///
/// A puller receiving a body from a counterparty that declares its length
/// (the share leg's ranged `chunks.pull`) refuses any declared length above
/// this before buffering a byte of it — an honest body never exceeds it, so a
/// larger claim is a peer asking this side to hold memory it has no reason to
/// hold. `chunk_seal`'s tests pin the arithmetic against a real sealed
/// maximum-size chunk. A Rust constant — never a knob.
pub const MAX_STORED_CHUNK_BODY: u64 = MAX_CHUNK as u64 + 1 + 16;
/// Files smaller than this are stored as a single blob, not chunked. Held equal
/// to `MAX_CHUNK` so the single-chunk path can never produce a chunk larger than
/// `MAX_CHUNK` either — the size invariant holds across both chunking paths.
pub(crate) const SINGLE_CHUNK_THRESHOLD: u64 = MAX_CHUNK as u64;

/// Chunk a file using FastCDC and return a ChunkManifest.
///
/// Files below `SINGLE_CHUNK_THRESHOLD` (8 MB) return a manifest
/// with a single chunk equal to the entire file; larger files are split by
/// FastCDC into chunks each ≤ `MAX_CHUNK` (8 MB).
pub fn chunk_file(data: &[u8]) -> ChunkManifest {
    let file_hash = ContentHash::of_raw(data);

    if (data.len() as u64) < SINGLE_CHUNK_THRESHOLD {
        // Small file: single chunk = entire file
        let chunk_hash = file_hash;
        return ChunkManifest {
            file_hash,
            total_size: data.len() as u64,
            chunk_hashes: vec![chunk_hash],
            chunk_sizes: vec![data.len() as u64],
            stored_hashes: None, // plaintext: store key == plaintext hash
            sealed_hashes: None,
            min_reader: None,
        };
    }

    let chunker = fastcdc::v2020::FastCDC::new(data, MIN_CHUNK, AVG_CHUNK, MAX_CHUNK);
    let mut chunk_hashes = Vec::new();
    let mut chunk_sizes = Vec::new();

    for chunk in chunker {
        let chunk_data = &data[chunk.offset..chunk.offset + chunk.length];
        let hash = ContentHash::of_raw(chunk_data);
        chunk_hashes.push(hash);
        chunk_sizes.push(chunk.length as u64);
    }

    ChunkManifest {
        file_hash,
        total_size: data.len() as u64,
        chunk_hashes,
        chunk_sizes,
        stored_hashes: None, // plaintext: store key == plaintext hash
        sealed_hashes: None,
        min_reader: None,
    }
}

/// Extract chunk data from a file given a manifest.
/// Returns (hash, data) pairs for each chunk.
pub fn extract_chunks(data: &[u8], manifest: &ChunkManifest) -> Vec<(ContentHash, Vec<u8>)> {
    let mut result = Vec::new();
    let mut offset = 0usize;

    for (hash, &size) in manifest.chunk_hashes.iter().zip(&manifest.chunk_sizes) {
        let end = offset + size as usize;
        let chunk_data = data[offset..end].to_vec();
        result.push((*hash, chunk_data));
        offset = end;
    }

    result
}

/// Reassemble a file from its chunks (in manifest order).
pub fn reassemble_chunks(chunks: &[Vec<u8>]) -> Vec<u8> {
    let total: usize = chunks.iter().map(|c| c.len()).sum();
    let mut result = Vec::with_capacity(total);
    for chunk in chunks {
        result.extend_from_slice(chunk);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_file_single_chunk() {
        let data = b"hello world";
        let manifest = chunk_file(data);
        assert_eq!(manifest.chunk_hashes.len(), 1);
        assert_eq!(manifest.total_size, data.len() as u64);
        assert_eq!(manifest.file_hash, ContentHash::of_raw(data));
    }

    #[test]
    fn extract_and_reassemble_small() {
        let data = b"small file content here";
        let manifest = chunk_file(data);
        let chunks = extract_chunks(data, &manifest);
        assert_eq!(chunks.len(), 1);
        let assembled =
            reassemble_chunks(&chunks.iter().map(|(_, d)| d.clone()).collect::<Vec<_>>());
        assert_eq!(assembled, data);
    }

    #[test]
    fn large_file_multiple_chunks() {
        // 80 MB of data — should be chunked
        let data = vec![0xABu8; 80 * 1024 * 1024];
        let manifest = chunk_file(&data);
        assert!(manifest.chunk_hashes.len() > 1);
        assert_eq!(manifest.total_size, data.len() as u64);

        // All chunk sizes should sum to total
        let total: u64 = manifest.chunk_sizes.iter().sum();
        assert_eq!(total, data.len() as u64);

        // Extract and reassemble
        let chunks = extract_chunks(&data, &manifest);
        let assembled =
            reassemble_chunks(&chunks.iter().map(|(_, d)| d.clone()).collect::<Vec<_>>());
        assert_eq!(assembled.len(), data.len());
        assert_eq!(ContentHash::of_raw(&assembled), manifest.file_hash);
    }

    #[test]
    fn deterministic_chunking() {
        let data = vec![0xCDu8; 80 * 1024 * 1024];
        let m1 = chunk_file(&data);
        let m2 = chunk_file(&data);
        assert_eq!(m1.chunk_hashes, m2.chunk_hashes);
        assert_eq!(m1.chunk_sizes, m2.chunk_sizes);
    }

    #[test]
    fn no_chunk_exceeds_max_chunk() {
        // The CAS size invariant: every chunk the chunker emits is ≤ MAX_CHUNK,
        // so the nest transport (sized once for a base64'd MAX_CHUNK) can always
        // carry it and every chunk is a valid blob on every tier. Exercise both
        // paths: a file between SINGLE_CHUNK_THRESHOLD and the streaming
        // threshold (FastCDC in-memory) and a large file. Random-ish content so
        // FastCDC actually finds cut points rather than emitting MAX_CHUNK runs.
        for size in [
            20 * 1024 * 1024usize, // 20 MB — was a single 20 MB chunk before F4
            70 * 1024 * 1024usize, // 70 MB
        ] {
            let data: Vec<u8> = (0..size).map(|i| ((i * 2654435761) >> 13) as u8).collect();
            let manifest = chunk_file(&data);
            assert!(
                manifest.chunk_hashes.len() > 1,
                "{size} bytes should be split into multiple chunks now"
            );
            for &cs in &manifest.chunk_sizes {
                assert!(
                    cs <= MAX_CHUNK as u64,
                    "chunk of {cs} bytes exceeds MAX_CHUNK ({MAX_CHUNK})"
                );
            }
            assert_eq!(manifest.chunk_sizes.iter().sum::<u64>(), size as u64);
        }
    }

    #[test]
    fn file_at_single_chunk_threshold_minus_one_is_one_chunk() {
        // A file just under the threshold is still one chunk, and that chunk is
        // ≤ MAX_CHUNK (because SINGLE_CHUNK_THRESHOLD == MAX_CHUNK).
        let data = vec![0x11u8; (SINGLE_CHUNK_THRESHOLD - 1) as usize];
        let manifest = chunk_file(&data);
        assert_eq!(manifest.chunk_hashes.len(), 1);
        assert!(manifest.chunk_sizes[0] <= MAX_CHUNK as u64);
    }
}
