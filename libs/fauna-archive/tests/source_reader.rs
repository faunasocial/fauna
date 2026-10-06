//! The streaming seam: a parser reads zip members by offset through
//! `ArchiveSource` and never needs the whole zip in memory
//! (`archive-import.md` § Parser contract rule 8).

use std::io::{Cursor, Read, Seek, SeekFrom, Write};

use fauna_archive::ArchiveError;
use fauna_archive::reader::{ArchiveReader, MAX_MEMBER_BYTES};
use fauna_archive::source::{ArchiveSource, SourceCursor, VecSource};
use zip::CompressionMethod;
use zip::write::SimpleFileOptions;

const SMALL: &[u8] = b"hello archive";
const BIG_LEN: usize = 200_000;

/// A zip with one stored member, one deflated member (exercises the
/// pure-Rust inflate path the workspace `zip` entry enables), a nested
/// path, and an explicit directory entry (which the directory must skip).
fn sample_zip() -> Vec<u8> {
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    w.add_directory("nested/", stored).unwrap();
    w.start_file("nested/small.txt", stored).unwrap();
    w.write_all(SMALL).unwrap();
    w.start_file("big.bin", deflated).unwrap();
    w.write_all(&big_bytes()).unwrap();
    w.start_file("a.json", stored).unwrap();
    w.write_all(b"[]").unwrap();
    w.finish().unwrap().into_inner()
}

fn big_bytes() -> Vec<u8> {
    (0..BIG_LEN).map(|i| (i % 251) as u8).collect()
}

#[test]
fn cursor_reads_and_seeks_over_a_source() {
    let src = VecSource(b"0123456789".to_vec());
    let mut cur = SourceCursor::new(&src);
    let mut buf = [0u8; 4];
    assert_eq!(cur.read(&mut buf).unwrap(), 4);
    assert_eq!(&buf, b"0123");
    assert_eq!(cur.seek(SeekFrom::End(-2)).unwrap(), 8);
    let mut tail = Vec::new();
    cur.read_to_end(&mut tail).unwrap();
    assert_eq!(tail, b"89");
    assert_eq!(cur.seek(SeekFrom::Current(-3)).unwrap(), 7);
    assert_eq!(cur.seek(SeekFrom::Start(100)).unwrap(), 100);
    assert_eq!(cur.read(&mut buf).unwrap(), 0, "past the end reads nothing");
    assert!(
        cur.seek(SeekFrom::Current(-1000)).is_err(),
        "before the start is an error"
    );
    assert_eq!(src.len(), 10);
    assert!(!src.is_empty());
}

#[test]
fn directory_lists_files_sorted_with_sizes_and_skips_directories() {
    let src = VecSource(sample_zip());
    let reader = ArchiveReader::open(&src).unwrap();
    let names: Vec<&str> = reader
        .directory()
        .entries()
        .iter()
        .map(|e| e.name.as_str())
        .collect();
    assert_eq!(names, ["a.json", "big.bin", "nested/small.txt"]);
    assert_eq!(
        reader.directory().get("big.bin").unwrap().size,
        BIG_LEN as u64
    );
    assert_eq!(
        reader.directory().get("nested/small.txt").unwrap().size,
        SMALL.len() as u64
    );
    assert!(reader.directory().get("nested/").is_none());
    assert_eq!(reader.directory().len(), 3);
    let json: Vec<&str> = reader
        .directory()
        .find(|n| n.ends_with(".json"))
        .into_iter()
        .map(|e| e.name.as_str())
        .collect();
    assert_eq!(json, ["a.json"]);
}

#[test]
fn reads_stored_and_deflated_members() {
    let src = VecSource(sample_zip());
    let mut reader = ArchiveReader::open(&src).unwrap();
    assert_eq!(reader.read_member("nested/small.txt").unwrap(), SMALL);
    assert_eq!(reader.read_member("big.bin").unwrap(), big_bytes());
}

#[test]
fn hashes_a_member_without_materializing_it() {
    let src = VecSource(sample_zip());
    let mut reader = ArchiveReader::open(&src).unwrap();
    let (hex, size) = reader.blake3_member("big.bin").unwrap();
    assert_eq!(size, BIG_LEN as u64);
    assert_eq!(hex, blake3::hash(&big_bytes()).to_hex().to_string());
}

#[test]
#[allow(clippy::assertions_on_constants)]
fn missing_member_and_non_zip_are_typed_errors() {
    let src = VecSource(sample_zip());
    let mut reader = ArchiveReader::open(&src).unwrap();
    assert!(matches!(
        reader.read_member("nope.json"),
        Err(ArchiveError::Member { .. })
    ));
    assert!(matches!(
        reader.blake3_member("nope.json"),
        Err(ArchiveError::Member { .. })
    ));
    let junk = VecSource(b"this is not a zip file at all".to_vec());
    assert!(matches!(
        ArchiveReader::open(&junk),
        Err(ArchiveError::Zip(_))
    ));
    assert!(MAX_MEMBER_BYTES >= 64 * 1024 * 1024);
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn file_source_reads_by_offset() {
    use fauna_archive::source::FileSource;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sample.zip");
    std::fs::write(&path, sample_zip()).unwrap();
    let src = FileSource::open(&path).unwrap();
    assert_eq!(src.len(), sample_zip().len() as u64);
    let mut reader = ArchiveReader::open(&src).unwrap();
    assert_eq!(reader.read_member("nested/small.txt").unwrap(), SMALL);
    let mut two = [0u8; 2];
    assert_eq!(src.read_at(0, &mut two).unwrap(), 2);
    assert_eq!(&two, b"PK");
}
