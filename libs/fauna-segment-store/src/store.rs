//! `FramedSegmentStore` — directory-scoped wrapper around FramedSegment.
//!
//! Manages segments under a root path (e.g. for mail:
//! `<data_dir>/__mail/<actor_id_hex>/` — there is no `segments/` parent
//! directory; `manager.rs::scope_dir` builds the real path, and
//! `message-segment-store.md` § At-rest vs transport owns it). One segment per
//! (kind, actor, segment_id); the store handles file naming and
//! bucket-driven rotation. The *outer* manifest (live/tombstoned
//! lists, atomic save) is the caller's responsibility.

use crate::{FramedSegment, SegmentHeader, SegmentStoreError};
use fauna_cbor::Cid;
use std::path::PathBuf;

pub struct FramedSegmentStore {
    root: PathBuf,
    kind: String,
    actor_id: [u8; 32],
    open: Option<OpenContext>,
}

struct OpenContext {
    bucket: String,
    segment: FramedSegment,
}

impl FramedSegmentStore {
    pub fn new(
        root: PathBuf,
        kind: impl Into<String>,
        actor_id: [u8; 32],
    ) -> Result<Self, SegmentStoreError> {
        std::fs::create_dir_all(&root)?;
        Ok(Self {
            root,
            kind: kind.into(),
            actor_id,
            open: None,
        })
    }

    pub fn segment_path(&self, segment_id: u32) -> PathBuf {
        self.root.join(format!("seg-{segment_id:08}.dat"))
    }

    /// Bucket of the currently-open segment, if any. Plan 5 T6 callers
    /// peek at this before invoking `append` to detect bucket-driven
    /// rotation and capture the just-closed segment id for the
    /// `fauna.segments.changed { Finalized }` push.
    pub fn open_bucket(&self) -> Option<&str> {
        self.open.as_ref().map(|ctx| ctx.bucket.as_str())
    }

    /// Append a record to the open segment for `bucket`. If no segment
    /// is open, or if the open segment's bucket differs, finalize the
    /// current one and open a new segment using `next_segment_id`.
    /// Returns the segment_id the record landed in.
    pub fn append(
        &mut self,
        bucket: &str,
        next_segment_id: u32,
        cid: Cid,
        record_bytes: &[u8],
        floor_metadata: &[u8],
    ) -> Result<u32, SegmentStoreError> {
        let rotate = match &self.open {
            None => true,
            Some(ctx) => ctx.bucket != bucket,
        };
        if rotate {
            if let Some(mut ctx) = self.open.take() {
                ctx.segment.finalize()?;
            }
            let header = SegmentHeader {
                kind: self.kind.clone(),
                actor_id: self.actor_id,
                segment_id: next_segment_id,
                bucket: bucket.to_string(),
                created_at_secs: now_secs(),
                record_count: 0,
            };
            let path = self.segment_path(next_segment_id);
            // Crash-recovery: a `.dat` already sitting at `next_segment_id` is an
            // orphan from a rotation that created the segment file but crashed
            // before the caller's manifest `save_atomic` advanced `next_seg_id`.
            // A *committed* segment can never sit at `next_segment_id` (the id is
            // only advanced after a record lands), so such a file is always an
            // uncommitted half-state — its record (if any) was never added to the
            // manifest or the DB mirror, and the writer was error-returned (the
            // sender tempfailed → will redeliver). `FramedSegment::create` uses
            // `create_new`, so without reclaiming it every future append for this
            // scope fails `AlreadyExists` forever (the example.com 2026-06-13
            // mail-append outage: a 0-byte `seg-00000040.dat` permanently 451'd
            // all mail for the actor). Reclaim the `.dat` (+ any stale `.meta`).
            if path.exists() {
                std::fs::remove_file(&path)?;
                let meta = path.with_extension("meta");
                if let Err(e) = std::fs::remove_file(&meta)
                    && e.kind() != std::io::ErrorKind::NotFound
                {
                    return Err(SegmentStoreError::Io(e));
                }
            }
            let segment = FramedSegment::create(&path, header)?;
            self.open = Some(OpenContext {
                bucket: bucket.to_string(),
                segment,
            });
        }
        let ctx = self.open.as_mut().expect("just set");
        ctx.segment
            .append_record(cid, record_bytes, floor_metadata)?;
        Ok(ctx.segment.header.segment_id)
    }

    /// Finalize the currently-open segment (if any). Idempotent.
    pub fn finalize_open(&mut self) -> Result<Option<u32>, SegmentStoreError> {
        let Some(mut ctx) = self.open.take() else {
            return Ok(None);
        };
        let id = ctx.segment.header.segment_id;
        ctx.segment.finalize()?;
        Ok(Some(id))
    }

    /// Open a finalized segment for reading.
    pub fn open_segment(&self, segment_id: u32) -> Result<FramedSegment, SegmentStoreError> {
        FramedSegment::open(&self.segment_path(segment_id))
    }

    /// Size lookups against the currently-OPEN segment, served from its
    /// in-memory per-record length map — `None` when `segment_id` is not the
    /// open segment (read a finalized one via [`Self::open_segment`] +
    /// `record_block_lens`). Lets the sizing path (quota / SEARCH /
    /// RFC822.SIZE) answer for just-appended records WITHOUT the
    /// flush-on-read finalize, which would rotate the segment early (see
    /// `FramedSegment::open_record_block_lens`).
    pub fn open_segment_record_lens(
        &self,
        segment_id: u32,
        cids: &[&Cid],
    ) -> Option<Vec<Option<u64>>> {
        let ctx = self.open.as_ref()?;
        if ctx.segment.header.segment_id != segment_id {
            return None;
        }
        ctx.segment.open_record_block_lens(cids)
    }

    /// Delete a finalized segment file (and its `.meta` sidecar) from
    /// disk (used by GC after retention window). Errors if the `.dat` is
    /// currently open; a missing `.meta` is tolerated (best-effort sidecar
    /// cleanup, in case a prior partial delete left only the `.dat`).
    pub fn delete_segment(&self, segment_id: u32) -> Result<(), SegmentStoreError> {
        let dat_path = self.segment_path(segment_id);
        let meta_path = dat_path.with_extension("meta");
        std::fs::remove_file(&dat_path)?;
        if let Err(e) = std::fs::remove_file(&meta_path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            return Err(SegmentStoreError::Io(e));
        }
        Ok(())
    }
}

fn now_secs() -> u64 {
    fauna_core::data::Timestamp::now_secs() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn store_in(tmp: &TempDir) -> FramedSegmentStore {
        FramedSegmentStore::new(tmp.path().join("store"), "mail", [7u8; 32]).expect("new")
    }

    #[test]
    fn append_creates_first_segment() {
        let tmp = TempDir::new().expect("tmp");
        let mut s = store_in(&tmp);
        let body = b"hello";
        let seg_id = s
            .append("2026-05", 1, Cid::of_dag_cbor(body), body, b"")
            .expect("append");
        assert_eq!(seg_id, 1);
    }

    #[test]
    fn bucket_change_rotates() {
        let tmp = TempDir::new().expect("tmp");
        let mut s = store_in(&tmp);
        let a = s
            .append("2026-05", 1, Cid::of_dag_cbor(b"a"), b"a", b"")
            .expect("append 1");
        let b = s
            .append("2026-05", 1, Cid::of_dag_cbor(b"b"), b"b", b"")
            .expect("append 2");
        let c = s
            .append("2026-06", 2, Cid::of_dag_cbor(b"c"), b"c", b"")
            .expect("append 3");
        assert_eq!(a, 1);
        assert_eq!(b, 1, "same bucket → same segment");
        assert_eq!(c, 2, "new bucket → next segment");
        s.finalize_open().expect("finalize");

        let seg1 = s.open_segment(1).expect("open 1");
        assert_eq!(seg1.header.bucket, "2026-05");
        assert_eq!(seg1.header.record_count, 2);
        let seg2 = s.open_segment(2).expect("open 2");
        assert_eq!(seg2.header.bucket, "2026-06");
        assert_eq!(seg2.header.record_count, 1);
    }

    #[test]
    fn finalize_open_is_idempotent() {
        let tmp = TempDir::new().expect("tmp");
        let mut s = store_in(&tmp);
        assert_eq!(s.finalize_open().expect("first"), None);
        let body = b"x";
        s.append("2026-05", 1, Cid::of_dag_cbor(body), body, b"")
            .expect("append");
        assert_eq!(s.finalize_open().expect("second"), Some(1));
        assert_eq!(s.finalize_open().expect("third"), None);
    }

    #[test]
    fn delete_removes_file_and_sidecar() {
        let tmp = TempDir::new().expect("tmp");
        let mut s = store_in(&tmp);
        let body = b"x";
        s.append("2026-05", 1, Cid::of_dag_cbor(body), body, b"")
            .expect("append");
        s.finalize_open().expect("finalize");
        let dat = s.segment_path(1);
        let meta = dat.with_extension("meta");
        assert!(dat.exists());
        assert!(meta.exists(), "finalize must write the sidecar");
        s.delete_segment(1).expect("delete");
        assert!(!dat.exists());
        assert!(!meta.exists(), "delete must also remove the sidecar");
    }

    /// Regression: a rotation that created `seg-{next_segment_id}.dat` but
    /// crashed before the caller advanced the manifest (`save_atomic`) leaves
    /// an orphan `.dat` at `next_segment_id`. Because `FramedSegment::create`
    /// uses `create_new`, every subsequent append for that scope would then
    /// fail `AlreadyExists` *forever* — the example.com 2026-06-13 mail-append
    /// outage (a 0-byte `seg-00000040.dat` permanently 451'd all mail for the
    /// actor). The rotation must reclaim the orphan, since a committed segment
    /// never sits at `next_segment_id`.
    #[test]
    fn append_reclaims_orphan_segment_from_crashed_rotation() {
        let tmp = TempDir::new().expect("tmp");
        let mut s = store_in(&tmp);
        // Simulate the crash: a 0-byte orphan parked at the next segment id.
        std::fs::write(s.segment_path(1), b"").expect("write orphan");
        // A leftover `.meta` from a crash mid-finalize must also be reclaimed.
        std::fs::write(s.segment_path(1).with_extension("meta"), b"stale").expect("write meta");

        let body = b"hello";
        let seg_id = s
            .append("2026-05", 1, Cid::of_dag_cbor(body), body, b"floor")
            .expect("append must reclaim the orphan, not fail AlreadyExists");
        assert_eq!(seg_id, 1);
        s.finalize_open().expect("finalize");

        let seg = s.open_segment(1).expect("open reclaimed segment");
        assert_eq!(
            seg.header.record_count, 1,
            "reclaimed segment holds the record"
        );
    }
}
