//! The generated fixture corpus as zips, for this crate's tests and
//! downstream machines' — never a real export (`archive-import.md`
//! § Parser contract rule 7: "Fixtures are generated, never real. No real
//! export ever enters the repo.").
//!
//! JSON members are Deflated (the real exports are), everything else Stored.
//! Paths are sorted so the archive is byte-deterministic.
//!
//! Compiled only under `cfg(test)` or the `test-helpers` feature, so a
//! release build of the crate carries none of it.

use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};

use crate::source::VecSource;
use zip::CompressionMethod;
use zip::write::SimpleFileOptions;

fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("fixture dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            walk(root, &path, out);
        } else {
            out.push(path.strip_prefix(root).expect("under root").to_path_buf());
        }
    }
}

/// Zips the named fixture tree under `tests/fixtures/` in memory. The path
/// is resolved from this crate's own `CARGO_MANIFEST_DIR`, so it stays
/// correct when a downstream crate calls it.
pub fn fixture_zip(name: &str) -> Vec<u8> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let mut files = Vec::new();
    walk(&root, &root, &mut files);
    files.sort();
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for rel in files {
        let name: String = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        let method = if name.ends_with(".json") {
            CompressionMethod::Deflated
        } else {
            CompressionMethod::Stored
        };
        w.start_file(
            name,
            SimpleFileOptions::default().compression_method(method),
        )
        .unwrap();
        w.write_all(&std::fs::read(root.join(&rel)).unwrap())
            .unwrap();
    }
    w.finish().unwrap().into_inner()
}

/// [`fixture_zip`] wrapped in the in-memory [`VecSource`] every parser reads
/// through.
pub fn fixture_source(name: &str) -> VecSource {
    VecSource(fixture_zip(name))
}

/// A zip from literal members (Stored) — for shape-specific edge cases.
pub fn zip_of(members: &[(&str, &[u8])]) -> VecSource {
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    for (name, bytes) in members {
        w.start_file(*name, stored).unwrap();
        w.write_all(bytes).unwrap();
    }
    VecSource(w.finish().unwrap().into_inner())
}
