//! Opening an archive reads its central directory and nothing else, and
//! reading one member reads that member and nothing else.
//!
//! The importer opens the archive once per record, so an open that scans the
//! source costs one whole-archive read per post. zip 9's default offset
//! detection did exactly that (100.96 MB read opening a 100.66 MB archive);
//! `ArchiveReader::open` opens at a known offset of 0 to avoid it.

use std::sync::atomic::{AtomicU64, Ordering};

struct Counting {
    inner: fauna_archive::source::VecSource,
    read: AtomicU64,
}

impl fauna_archive::ArchiveSource for Counting {
    fn len(&self) -> u64 {
        fauna_archive::ArchiveSource::len(&self.inner)
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = fauna_archive::ArchiveSource::read_at(&self.inner, offset, buf)?;
        self.read.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

/// Headers and the central directory of a two-member archive fit well inside
/// this; a single member of the fixture's size does not.
const SLACK: u64 = 64 * 1024;

#[test]
fn open_reads_the_directory_and_a_member_read_reads_that_member() {
    const EACH: usize = 8 * 1024 * 1024;
    let blob = vec![0u8; EACH];
    let src = Counting {
        inner: fauna_archive::testing::zip_of(&[("a.jpg", &blob), ("b.jpg", &blob)]),
        read: AtomicU64::new(0),
    };
    let mut reader = fauna_archive::reader::ArchiveReader::open(&src).unwrap();
    let after_open = src.read.load(Ordering::Relaxed);
    assert!(
        after_open < SLACK,
        "open read {after_open} B of a {} B archive — it must read the central directory only",
        fauna_archive::ArchiveSource::len(&src)
    );
    assert_eq!(reader.member_size("b.jpg").unwrap(), EACH as u64);
    assert_eq!(
        src.read.load(Ordering::Relaxed),
        after_open,
        "a size lookup reads nothing"
    );
    assert_eq!(reader.read_member("a.jpg").unwrap().len(), EACH);
    let member_read = src.read.load(Ordering::Relaxed) - after_open;
    assert!(
        member_read < EACH as u64 + SLACK,
        "reading one {EACH} B member read {member_read} B"
    );
}
