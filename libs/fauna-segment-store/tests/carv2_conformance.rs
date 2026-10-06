//! Cross-language CARv2 conformance fixture + check.
//!
//! Produces the bytes of a deterministic 3-block CARv2 file with
//! `fauna_carv2::Writer`, asserts those bytes equal the committed
//! `tests/fixtures/carv2/sample.car` (guards against accidental fixture
//! drift across rebuilds), and re-reads them with `fauna_carv2::Reader` to
//! confirm every block's bytes survive a writer→reader round trip.
//!
//! The Layer 3 Track C Go conformance test (Task 3.6,
//! `bins/fauna-bridges/internal/dagcbor/carv2_conformance_test.go`)
//! reads the same vendored `sample.car` and parses it through
//! `github.com/ipld/go-car/v2`. Together these two checks pin Fauna's CARv2
//! output as standard CARv2: any Rust-side regression that changes the file
//! bytes fails *this* test; any Rust↔Go disagreement on framing fails the
//! Go-side test.

use fauna_carv2::{Reader, Writer};
use fauna_cbor::Cid;
use std::io::Cursor;
use std::path::Path;

/// Deterministic input set. The bytes intentionally don't need to be valid
/// dag-cbor — Layer 3 Task 3.6 conformance is about CARv2 *framing*, not
/// block-content semantics. (The seal layer guarantees inner payload
/// well-formedness at the bytes level upstream of the segment store.)
fn sample_blocks() -> Vec<(Cid, &'static [u8])> {
    let bodies: [&'static [u8]; 3] = [b"record-1", b"record-2", b"record-3"];
    bodies.iter().map(|b| (Cid::of_dag_cbor(b), *b)).collect()
}

/// Build the canonical sample CARv2 bytes — `Writer::new(.., &[])` (no
/// roots, matching segment-store usage) + three `write_block` calls in
/// fixed order + `finalize`. All inputs are constant, so the bytes are
/// deterministic across runs.
fn build_sample_bytes() -> Vec<u8> {
    let mut buf = Cursor::new(Vec::<u8>::new());
    {
        let mut writer = Writer::new(&mut buf, &[]).expect("Writer::new");
        for (cid, bytes) in sample_blocks() {
            writer.write_block(&cid, bytes).expect("write_block");
        }
        writer.finalize().expect("finalize");
    }
    buf.into_inner()
}

#[test]
fn sample_car_bytes_match_committed_fixture() {
    let fixture_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/carv2/sample.car");
    let on_disk = std::fs::read(&fixture_path).expect(
        "tests/fixtures/carv2/sample.car must exist (committed alongside this test); if it's \
         missing or stale, regenerate it with: \
         `cargo test -p fauna-segment-store --test carv2_conformance -- --ignored regen_fixture`",
    );
    let regenerated = build_sample_bytes();
    if on_disk != regenerated {
        panic!(
            "fixture drift: tests/fixtures/carv2/sample.car ({} bytes) does not match \
             the bytes produced by today's fauna_carv2::Writer ({} bytes). Either the writer's \
             on-the-wire bytes changed (intentional? update the fixture: \
             `cargo test -p fauna-segment-store --test carv2_conformance -- --ignored \
             regen_fixture` then commit the new file) or there's a non-determinism bug.",
            on_disk.len(),
            regenerated.len()
        );
    }
}

#[test]
fn sample_car_round_trips_through_reader() {
    let bytes = build_sample_bytes();
    let mut cursor = Cursor::new(bytes);
    let mut reader = Reader::new(&mut cursor).expect("Reader::new on sample bytes");
    assert_eq!(reader.len(), 3, "sample has 3 blocks");

    for (cid, expected) in sample_blocks() {
        let got = reader.get(&cid).expect("reader.get on sample block");
        assert_eq!(got.as_slice(), expected, "block bytes round-trip");
    }
}

/// Regenerate the committed fixture from the deterministic input. Marked
/// `#[ignore]` so it doesn't run by default — invoke explicitly when the
/// `fauna_carv2::Writer` byte format changes intentionally.
#[test]
#[ignore = "explicit run only (regenerates committed fixture)"]
fn regen_fixture() {
    let fixture_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/carv2/sample.car");
    let bytes = build_sample_bytes();
    if let Some(parent) = fixture_path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir fixture parent");
    }
    std::fs::write(&fixture_path, &bytes).expect("write fixture");
    eprintln!(
        "regenerated {} ({} bytes) — commit the change",
        fixture_path.display(),
        bytes.len()
    );
}
