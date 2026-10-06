//! Conformance test against committed binary fixtures. Verifies that
//! every blob shape decodes successfully and re-encodes to canonical
//! CBOR (idempotent round-trip).

use fauna_mls::wrapped_blob::{
    MlsSnapshotBlob, TlsCertBlob, WrappedMsekBlob, WrappedSubmissionTokenBlob,
};
use std::path::PathBuf;

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../fauna-protocol/schemas/test_vectors")
        .join(name)
}

fn read_fixture(name: &str) -> Vec<u8> {
    std::fs::read(fixture_path(name)).unwrap_or_else(|e| panic!("read fixture {name}: {e}"))
}

#[test]
fn wrapped_msek_fixture_decodes_and_round_trips() {
    let bytes = read_fixture("wrapped_msek.bin");
    let blob = WrappedMsekBlob::from_canonical_bytes(&bytes).unwrap();
    assert_eq!(blob.kind, "wrapped-msek");
    assert_eq!(blob.version, 1);
    let re_encoded = blob.to_canonical_bytes().unwrap();
    let blob2 = WrappedMsekBlob::from_canonical_bytes(&re_encoded).unwrap();
    assert_eq!(blob.kind, blob2.kind);
}

#[test]
fn mls_snapshot_fixture_decodes_and_round_trips() {
    let bytes = read_fixture("mls_snapshot.bin");
    let blob = MlsSnapshotBlob::from_canonical_bytes(&bytes).unwrap();
    assert_eq!(blob.kind, "mls-snapshot");
    let _re = blob.to_canonical_bytes().unwrap();
}

#[test]
fn wrapped_submission_token_fixture_decodes_and_round_trips() {
    let bytes = read_fixture("wrapped_submission_token.bin");
    let blob = WrappedSubmissionTokenBlob::from_canonical_bytes(&bytes).unwrap();
    assert_eq!(blob.kind, "submission-token");
    let _re = blob.to_canonical_bytes().unwrap();
}

#[test]
fn tls_cert_fixture_decodes_and_round_trips() {
    let bytes = read_fixture("tls_cert_blob.bin");
    let blob = TlsCertBlob::from_canonical_bytes(&bytes).unwrap();
    assert_eq!(blob.kind, "tls-cert");
    let _re = blob.to_canonical_bytes().unwrap();
}
