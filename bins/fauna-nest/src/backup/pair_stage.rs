//! The segment pairs a restore keeps — staged on disk, never held in memory.
//!
//! Owner: `docs/goal/architecture/message-segment-store.md` § Segment size: a
//! segment has no size ceiling, and a transfer that keeps one is file-backed,
//! its memory a constant piece, never the segment. Both restore verbs keep
//! pairs — [`super::materialize`] (the re-seed flip of a fresh target) and
//! [`super::recover`] (the lived-in recovery) — and both take them through
//! here: each half is reassembled from its custody generation straight into a
//! file ([`fauna_core::file_download::download_file_to_path_by_manifest`], a
//! window of chunks at a time), or copied off the target's own open handle,
//! then verified by a streaming hash and adopted by a streaming copy.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use fauna_protocol::segments::SegmentRef;

pub(super) use crate::segments::copy_new_synced;

/// One segment pair, verified against the mirror that named it and staged on
/// disk as `seg-<id>.dat` beside `seg-<id>.meta` — the layout
/// `FramedSegment::open` reads.
#[derive(Debug, Clone)]
pub(super) struct VerifiedPair {
    pub(super) id: u32,
    pub(super) dat: PathBuf,
    pub(super) meta: PathBuf,
}

/// The two staged paths of segment `id` in `dir`.
pub(super) fn pair_paths(dir: &Path, id: u32) -> (PathBuf, PathBuf) {
    (
        dir.join(format!("seg-{id:08}.dat")),
        dir.join(format!("seg-{id:08}.meta")),
    )
}

/// Where every restore run stages, under the segment store's own data dir so a
/// staged half crosses no filesystem on its way into the segment area. Every
/// store keeps its segments under `__<kind>/`, so no store's replay sees it.
const STAGE_ROOT: &str = ".pair-stage";

/// One run's staging directory, removed on drop. A crash leaves it behind for
/// the next run's sweep, once it is a day old
/// (`fauna_sync_engine::segment_backup::STALE_STAGING_SECS`, the backup
/// passes' own staging rule).
pub(super) struct PairStage {
    dir: PathBuf,
    next: usize,
}

impl PairStage {
    pub(super) async fn new(under: &Path) -> anyhow::Result<Self> {
        let root = under.join(STAGE_ROOT);
        fauna_sync_engine::segment_backup::sweep_stale_staging(&root).await;
        let mut token = [0u8; 8];
        getrandom::fill(&mut token).map_err(|e| anyhow::anyhow!("staging token: {e}"))?;
        let dir = root.join(hex::encode(token));
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("create the restore staging dir {}", dir.display()))?;
        Ok(Self { dir, next: 0 })
    }

    /// A fresh directory of its own — for a pair whose segment id another pair
    /// in hand may share (two generations of one id).
    pub(super) fn fresh_dir(&mut self) -> anyhow::Result<PathBuf> {
        self.next += 1;
        self.named_dir(&format!("{:06}", self.next))
    }

    /// A directory named `name`, created if absent — for pairs that must sit
    /// together (one journal generation, which its replay folds as one dir).
    pub(super) fn named_dir(&self, name: &str) -> anyhow::Result<PathBuf> {
        let dir = self.dir.join(name);
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("create the staging dir {}", dir.display()))?;
        Ok(dir)
    }
}

impl Drop for PairStage {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The BLAKE3 hex of a file, streamed.
pub(super) fn file_blake3_hex(path: &Path) -> anyhow::Result<String> {
    let file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let digest = fauna_core::chunker_stream::blake3_of_reader(file)
        .with_context(|| format!("hash {}", path.display()))?;
    Ok(hex::encode(digest))
}

/// Both staged halves against what the corpus's own `manifest.<kind>` mirror
/// advertised, over hashes streamed off the files — each unconditionally: the
/// mirror's refs carry the sidecar hash as a required field, so the mirror is
/// always the sidecar's anchor
/// (`docs/goal/architecture/segment-backup-protocol.md` § *A sidecar in the
/// corpus is anchored, or it is not restored*).
///
/// Nothing weaker stands in for the `.meta` check: `FramedSegment::open`
/// asserts only that the block count equals `record_order.len()` and that
/// `floor_metadata.len()` matches it — two lengths, nothing about content. Two
/// sidecars of the same owner with equal record counts are interchangeable to
/// it, and the convergent seal carries no path or scope binding to tell them
/// apart either.
pub(super) fn verify_pair(pair: &VerifiedPair, advertised: &SegmentRef) -> anyhow::Result<()> {
    for (half, path, want) in [
        (".dat", &pair.dat, &advertised.blake3_hex),
        (".meta", &pair.meta, &advertised.meta_blake3_hex),
    ] {
        let got = file_blake3_hex(path)?;
        anyhow::ensure!(
            got == *want,
            "segment {}: the custody `{half}` opens to bytes hashing {got}, but the source's \
             own mirror advertised {want}",
            advertised.segment_id,
        );
    }
    Ok(())
}

/// Is `found` a prefix of `intended` — and the whole of it? `None` when it is
/// not a prefix (or cannot be read). Compared a buffer at a time.
pub(super) fn prefix_of(found: &Path, intended: &Path) -> std::io::Result<Option<bool>> {
    use std::io::Read as _;
    const PIECE: usize = 64 * 1024;
    let found_len = std::fs::metadata(found)?.len();
    let intended_len = std::fs::metadata(intended)?.len();
    if found_len > intended_len {
        return Ok(None);
    }
    let (mut a, mut b) = (std::fs::File::open(found)?, std::fs::File::open(intended)?);
    let (mut x, mut y) = (vec![0u8; PIECE], vec![0u8; PIECE]);
    let mut left = found_len;
    while left > 0 {
        let n = PIECE.min(left as usize);
        a.read_exact(&mut x[..n])?;
        b.read_exact(&mut y[..n])?;
        if x[..n] != y[..n] {
            return Ok(None);
        }
        left -= n as u64;
    }
    Ok(Some(found_len == intended_len))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_prefix_is_told_from_a_whole_copy_and_from_a_stranger() {
        let tmp = tempfile::tempdir().unwrap();
        let intended = tmp.path().join("intended");
        let body: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&intended, &body).unwrap();
        let found = tmp.path().join("found");

        std::fs::write(&found, &body[..150_000]).unwrap();
        assert_eq!(prefix_of(&found, &intended).unwrap(), Some(false));
        std::fs::write(&found, &body).unwrap();
        assert_eq!(prefix_of(&found, &intended).unwrap(), Some(true));
        let mut other = body[..150_000].to_vec();
        other[100_000] ^= 1;
        std::fs::write(&found, &other).unwrap();
        assert_eq!(prefix_of(&found, &intended).unwrap(), None);
        let mut longer = body.clone();
        longer.push(0);
        std::fs::write(&found, &longer).unwrap();
        assert_eq!(prefix_of(&found, &intended).unwrap(), None);
    }

    /// A staged pair and the ref the corpus's own `manifest.<kind>` mirror
    /// carries for it.
    fn staged(dir: &Path, dat: &[u8], meta: &[u8]) -> (VerifiedPair, SegmentRef) {
        let (dat_path, meta_path) = pair_paths(dir, 7);
        std::fs::write(&dat_path, dat).unwrap();
        std::fs::write(&meta_path, meta).unwrap();
        let advertised = SegmentRef {
            segment_id: 7,
            blake3_hex: hex::encode(blake3::hash(dat).as_bytes()),
            meta_blake3_hex: hex::encode(blake3::hash(meta).as_bytes()),
            ..Default::default()
        };
        let pair = VerifiedPair {
            id: 7,
            dat: dat_path,
            meta: meta_path,
        };
        (pair, advertised)
    }

    #[test]
    fn an_anchored_pair_that_matches_the_mirror_verifies() {
        let tmp = tempfile::tempdir().unwrap();
        let (pair, advertised) = staged(tmp.path(), b"the dat", b"the meta");
        verify_pair(&pair, &advertised).expect("both halves match");
    }

    #[test]
    fn an_anchored_sidecar_that_contradicts_the_mirror_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let (pair, advertised) = staged(tmp.path(), b"the dat", b"the meta");
        std::fs::write(&pair.meta, b"another segment's sidecar").unwrap();
        let err = verify_pair(&pair, &advertised)
            .expect_err("the mirror's own hash catches the substitution");
        assert!(format!("{err:#}").contains(".meta"), "{err:#}");
    }

    #[test]
    fn a_copy_never_replaces_what_is_there() {
        let tmp = tempfile::tempdir().unwrap();
        let (src, dst) = (tmp.path().join("src"), tmp.path().join("dst"));
        std::fs::write(&src, b"the half").unwrap();
        copy_new_synced(&src, &dst).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), b"the half");
        let err = copy_new_synced(&src, &dst).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    }
}
