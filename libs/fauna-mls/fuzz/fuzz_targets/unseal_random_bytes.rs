#![no_main]

//! Feed arbitrary bytes to each `WrappedBlob::from_canonical_bytes`.
//! Verifies no panics: only structured `UnwrapError` variants.

use fauna_mls::wrapped_blob::{
    MlsSnapshotBlob, TlsCertBlob, WrappedMsekBlob, WrappedSubmissionTokenBlob,
};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = WrappedMsekBlob::from_canonical_bytes(data);
    let _ = MlsSnapshotBlob::from_canonical_bytes(data);
    let _ = WrappedSubmissionTokenBlob::from_canonical_bytes(data);
    let _ = TlsCertBlob::from_canonical_bytes(data);
});
