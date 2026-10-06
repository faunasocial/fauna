//! Conformance test against the ipld/codec-fixtures corpus.
//!
//! Pinned commit: the commit in `bins/fauna-bridges/internal/dagcbor/CORPUS_COMMIT`
//! (matches the Go bridge's existing gate; today: a312b720a4f8302c60f075aa3d33149967a4aa45).
//!
//! For each fixture's *.dag-cbor file this test:
//!   1. Decodes the dag-cbor bytes to an untyped IPLD value via serde_ipld_dagcbor +
//!      ipld_core::ipld::Ipld (the canonical untyped value type for this codec).
//!   2. Re-encodes via fauna_cbor::encode_canonical.
//!   3. Asserts the re-encoded bytes are byte-identical to the source dag-cbor bytes.
//!
//! Any failure means our encoder diverges from canonical IPLD dag-cbor.
//!
//! Skip classes (mirroring the Go bridge — DAG-CBOR strictness forbids both):
//!   - contains-float — major type 7 with 25/26/27 immediate.
//!   - contains-tag   — major type 6.
//!
//! Tracked as skipped rather than failed.
//!
//! Set DAGCBOR_FIXTURES_DIR to point at a clone of ipld/codec-fixtures.
//! The just recipe `dagcbor-fixtures-rust` (justfile) sets this for you.

use std::fs;
use std::path::{Path, PathBuf};

fn fixtures_root() -> Option<PathBuf> {
    std::env::var_os("DAGCBOR_FIXTURES_DIR").map(|s| PathBuf::from(s).join("fixtures"))
}

#[derive(Clone, Copy)]
enum CborClass {
    Neither,
    Float,
    Tag,
}

fn classify_cbor(b: &[u8]) -> CborClass {
    // Structural classifier (mirrors Go bridge fixtures_test.go). Walks
    // major types and reports the first float or tag hit; not a full parser.
    let mut i = 0usize;
    while i < b.len() {
        let ib = b[i];
        let major = ib >> 5;
        let info = ib & 0x1F;
        i += 1;
        match major {
            6 => return CborClass::Tag,
            7 => {
                if info == 25 || info == 26 || info == 27 {
                    return CborClass::Float;
                }
                // info 20/21/22/23 are false/true/null/undefined (1 byte total).
                // info 24 means one more byte (simple value). Other infos are
                // reserved; treat as 1-byte simple.
                if info == 24 {
                    i += 1;
                }
            }
            0 | 1 => i += arg_len(info),
            2 | 3 => {
                let (arg_l, length) = read_arg(b, i, info);
                i += arg_l + length;
            }
            4 | 5 => i += arg_len(info),
            _ => unreachable!(),
        }
    }
    CborClass::Neither
}

fn arg_len(info: u8) -> usize {
    match info {
        24 => 1,
        25 => 2,
        26 => 4,
        27 => 8,
        _ => 0,
    }
}

fn read_arg(b: &[u8], off: usize, info: u8) -> (usize, usize) {
    match info {
        24 if off < b.len() => (1, b[off] as usize),
        25 if off + 2 <= b.len() => (2, ((b[off] as usize) << 8) | (b[off + 1] as usize)),
        26 if off + 4 <= b.len() => (
            4,
            ((b[off] as usize) << 24)
                | ((b[off + 1] as usize) << 16)
                | ((b[off + 2] as usize) << 8)
                | (b[off + 3] as usize),
        ),
        27 if off + 8 <= b.len() => {
            let mut v = 0usize;
            for k in 0..8 {
                v = (v << 8) | (b[off + k] as usize);
            }
            (8, v)
        }
        24..=27 => (0, 0),
        _ => (0, info as usize),
    }
}

fn find_dag_cbor_file(dir: &Path) -> Option<PathBuf> {
    let entries = fs::read_dir(dir).ok()?;
    for e in entries.flatten() {
        if e.file_type().map(|t| t.is_file()).unwrap_or(false) {
            let path = e.path();
            if path.extension().and_then(|s| s.to_str()) == Some("dag-cbor") {
                return Some(path);
            }
        }
    }
    None
}

#[test]
fn dag_cbor_fixtures_roundtrip_byte_identical() {
    let Some(root) = fixtures_root() else {
        eprintln!(
            "skipped: DAGCBOR_FIXTURES_DIR not set. Run `just dagcbor-fixtures-rust` \
             or set DAGCBOR_FIXTURES_DIR to a clone of github.com/ipld/codec-fixtures \
             pinned to a312b720a4f8302c60f075aa3d33149967a4aa45."
        );
        return;
    };
    assert!(
        root.exists(),
        "fixtures dir missing at {:?}; run `just dagcbor-fixtures-rust`",
        root
    );

    let mut entries: Vec<_> = fs::read_dir(&root)
        .expect("read fixtures dir")
        .filter_map(|e| e.ok())
        .collect();
    entries.sort_by_key(|e| e.file_name());

    let (mut pass, mut skip_float, mut skip_tag, mut fail) = (0u32, 0u32, 0u32, 0u32);
    let mut failures: Vec<String> = Vec::new();

    for entry in &entries {
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let Some(path) = find_dag_cbor_file(&entry.path()) else {
            continue;
        };
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                failures.push(format!("{:?}: read: {}", path, e));
                fail += 1;
                continue;
            }
        };
        match classify_cbor(&bytes) {
            CborClass::Float => {
                skip_float += 1;
                continue;
            }
            CborClass::Tag => {
                skip_tag += 1;
                continue;
            }
            CborClass::Neither => {}
        }

        // Decode to an untyped IPLD value via serde_ipld_dagcbor.
        // ipld_core::ipld::Ipld is the canonical untyped value type; serde_ipld_dagcbor
        // depends on ipld-core and its Serialize/Deserialize impls handle the CID tag (42)
        // and all IPLD primitive types correctly.
        let value: ipld_core::ipld::Ipld = match serde_ipld_dagcbor::from_slice(&bytes) {
            Ok(v) => v,
            Err(e) => {
                failures.push(format!(
                    "{}: decode: {}",
                    entry.file_name().to_string_lossy(),
                    e
                ));
                fail += 1;
                continue;
            }
        };
        let re = match fauna_cbor::encode_canonical(&value) {
            Ok(b) => b,
            Err(e) => {
                failures.push(format!(
                    "{}: encode: {}",
                    entry.file_name().to_string_lossy(),
                    e
                ));
                fail += 1;
                continue;
            }
        };
        if re != bytes {
            failures.push(format!(
                "{}: re-encode not byte-identical (src={} bytes, re={} bytes)",
                entry.file_name().to_string_lossy(),
                bytes.len(),
                re.len()
            ));
            fail += 1;
            continue;
        }
        pass += 1;
    }

    eprintln!(
        "codec-fixtures: pass={} skip-float={} skip-tag={} fail={} (total dirs={})",
        pass,
        skip_float,
        skip_tag,
        fail,
        entries.len()
    );
    assert!(
        fail == 0,
        "{} fixtures failed:\n{}",
        fail,
        failures.join("\n")
    );
    assert!(
        pass > 0,
        "no fixtures passed — corpus at {:?} produced 0 actionable fixtures",
        root
    );
}
