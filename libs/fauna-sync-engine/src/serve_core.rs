//! The engine's **one serve core** — answer a stored chunk from the body this
//! seat holds on disk (`file-sync.md` § Relay serving → *The seat serves from
//! the file, through one serve core*).
//!
//! The engine keeps no chunk cache. A chunk is re-derived on demand: read the
//! plaintext range a held body's manifest names, check it still hashes to the
//! manifest's plaintext anchor, seal it under the folder's seal root through
//! the one per-chunk pipeline ([`crate::seal::seal_chunk_body`]), and serve it
//! only when the result is the store key asked for. Every honest refusal — no
//! body here (a placeholder), a body that changed, a reseal that does not
//! reproduce the key — is `None`, never an error, and nothing is ever fetched
//! on an asker's behalf: a [`crate::share_body::BodySource`] never fetches.
//!
//! Three consumers, one core: the cross-user share leg
//! (`peer_share_store::chunk_body_from_hit`, behind `p2p-share`), the
//! nest relay ([`crate::engine::SyncEngine::serve_chunk`], through the durable
//! store-key index `held_chunks`), and later the same-account peer leg. Not an
//! excisable feature, so this module and the body source compile without
//! `p2p-share`.

use fauna_core::chunk::ChunkManifest;
use fauna_core::data::ContentHash;

pub use crate::db::HeldChunk;
use crate::share_body::BodySource;

/// Every chunk of `manifest` as a [`HeldChunk`] — its store key (the
/// ciphertext hash for a sealed manifest, the plaintext hash for an unsealed
/// one: [`ChunkManifest::store_keys`]) and its plaintext range, the prefix sums
/// of `chunk_sizes` (chunks are contiguous — the chunker's own walk).
///
/// `None` for a manifest whose parallel lists disagree in length, or whose
/// offsets overflow: nothing in it can be trusted to address a range.
pub fn held_chunks_of(manifest: &ChunkManifest) -> Option<Vec<HeldChunk>> {
    let keys = manifest.store_keys();
    if keys.len() != manifest.chunk_hashes.len() || keys.len() != manifest.chunk_sizes.len() {
        return None;
    }
    let mut out = Vec::with_capacity(keys.len());
    let mut offset = 0u64;
    for ((store_key, plain_hash), len) in keys
        .iter()
        .zip(manifest.chunk_hashes.iter())
        .zip(manifest.chunk_sizes.iter())
    {
        out.push(HeldChunk {
            store_key: store_key.digest(),
            offset,
            len: *len,
            plain_hash: *plain_hash,
        });
        offset = offset.checked_add(*len)?;
    }
    Some(out)
}

/// Re-derive one stored chunk from `path`'s body read through `body`.
///
/// `seal_root` is the root the chunk sealed under, or `None` for a public
/// folder's unsealed chunk (whose store key is its plaintext hash and whose
/// body is the framed plaintext). `Ok(None)` for every honest refusal: no
/// body here, a range that is short or no longer hashes to the manifest's
/// anchor, or a result that is not `chunk.store_key`.
pub async fn serve_held_chunk(
    body: &dyn BodySource,
    path: &str,
    chunk: &HeldChunk,
    seal_root: Option<&[u8; 32]>,
) -> anyhow::Result<Option<Vec<u8>>> {
    let Some(plain) = body.read_range(path, chunk.offset, chunk.len).await else {
        return Ok(None); // no body here, or the file shrank — drifted
    };
    if ContentHash::of_raw(&plain) != chunk.plain_hash {
        return Ok(None); // the body changed since it was indexed
    }
    let (derived_key, sealed) = match seal_root {
        Some(root) => crate::seal::seal_chunk_body(&chunk.plain_hash, &plain, root)?,
        None => (
            chunk.plain_hash,
            fauna_core::chunk_seal::FramedChunk::frame(&chunk.plain_hash, &plain)?.into_body(),
        ),
    };
    if derived_key.digest() != chunk.store_key {
        // A different build's seal, or the wrong root for this row: check,
        // never trust (`blob_seal::seal_blob` is deterministic only for the
        // same bytes, root and build).
        tracing::debug!(
            path = %fauna_core::log_redact::log_path(path),
            "re-derived chunk body does not reproduce the requested store key"
        );
        return Ok(None);
    }
    Ok(Some(sealed))
}
