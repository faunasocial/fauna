//! Content-defined chunking and reassembly.

use fauna_core::chunk::ChunkManifest;
use fauna_core::chunker;
use fauna_core::data::ContentHash;
use fauna_core::encoding::{canonical_decode, canonical_encode, content_hash};

use crate::ClientError;

/// Chunk data using FastCDC. Returns a `ChunkManifest` describing the chunks.
pub fn chunk_data(data: &[u8]) -> ChunkManifest {
    chunker::chunk_file(data)
}

/// Extract individual chunk data from raw bytes + manifest.
/// Returns a list of `(ContentHash, chunk_bytes)` pairs in order.
pub fn extract_chunks(data: &[u8], manifest: &ChunkManifest) -> Vec<(ContentHash, Vec<u8>)> {
    chunker::extract_chunks(data, manifest)
}

/// Reassemble file bytes from ordered chunk byte-vectors.
pub fn reassemble_chunks(chunk_data: &[Vec<u8>]) -> Vec<u8> {
    chunker::reassemble_chunks(chunk_data)
}

/// BLAKE3 content hash of arbitrary data. Returns 32 bytes.
pub fn content_hash_bytes(data: &[u8]) -> [u8; 32] {
    content_hash(data).digest()
}

/// Generate a random 32-byte device ID using the platform CSPRNG.
pub fn generate_device_id() -> [u8; 32] {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("getrandom failed");
    bytes
}

/// Serialize a `ChunkManifest` to canonical dag-cbor bytes (the at-rest /
/// content-addressed encoding — Layer 6).
pub fn serialize_manifest(manifest: &ChunkManifest) -> Result<Vec<u8>, ClientError> {
    canonical_encode(manifest).map_err(|e| ClientError(e.to_string()))
}

/// Deserialize canonical dag-cbor bytes back to a `ChunkManifest`.
pub fn deserialize_manifest(data: &[u8]) -> Result<ChunkManifest, ClientError> {
    canonical_decode(data).map_err(|e| ClientError(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_chunk_extract_reassemble() {
        // Use 1 KB of test data — stays below the 64 MB single-chunk threshold,
        // so we get exactly one chunk; still exercises the full pipeline.
        let original: Vec<u8> = (0u8..=255).cycle().take(1024).collect();

        let manifest = chunk_data(&original);
        assert!(!manifest.chunk_hashes.is_empty());
        assert_eq!(manifest.total_size, original.len() as u64);

        let chunks = extract_chunks(&original, &manifest);
        assert_eq!(chunks.len(), manifest.chunk_hashes.len());

        let chunk_bytes: Vec<Vec<u8>> = chunks.into_iter().map(|(_, b)| b).collect();
        let reassembled = reassemble_chunks(&chunk_bytes);
        assert_eq!(reassembled, original);
    }

    #[test]
    fn content_hash_is_deterministic() {
        let data = b"hello fauna";
        let h1 = content_hash_bytes(data);
        let h2 = content_hash_bytes(data);
        assert_eq!(h1, h2);
        assert_ne!(h1, [0u8; 32]);
    }

    #[test]
    fn generate_device_id_non_zero() {
        let id = generate_device_id();
        // Astronomically unlikely to be all zeros
        assert_ne!(id, [0u8; 32]);
    }

    #[test]
    fn manifest_serialize_roundtrip() {
        let data: Vec<u8> = (0u8..100).collect();
        let manifest = chunk_data(&data);
        let encoded = serialize_manifest(&manifest).unwrap();
        let decoded = deserialize_manifest(&encoded).unwrap();
        assert_eq!(decoded.total_size, manifest.total_size);
        assert_eq!(decoded.chunk_hashes, manifest.chunk_hashes);
    }

    /// Layer 6: the at-rest `ChunkManifest` encoding must be canonical
    /// dag-cbor, not BARE. This is the discriminating assertion — BARE bytes
    /// fail strict dag-cbor decode (`NotCanonical { indefinite-length item }`),
    /// so this test is RED while `serialize_manifest` still BARE-encodes and
    /// GREEN once it produces canonical dag-cbor. A plain
    /// `serialize`→`deserialize` round-trip can't discriminate the flip.
    #[test]
    fn manifest_at_rest_is_canonical_dagcbor() {
        let data: Vec<u8> = (0u8..100).collect();
        let manifest = chunk_data(&data);
        let bytes = serialize_manifest(&manifest).unwrap();
        let decoded: ChunkManifest = fauna_core::encoding::canonical_decode(&bytes)
            .expect("at-rest manifest bytes must be canonical dag-cbor");
        assert_eq!(decoded.total_size, manifest.total_size);
        assert_eq!(decoded.chunk_hashes, manifest.chunk_hashes);
    }
}
