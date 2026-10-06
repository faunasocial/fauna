//! Golden CID vectors — the **cross-language** half of the content-address
//! contract.
//!
//! `Cid::of_raw(..).to_base32()` is not only an internal encoding: the Go mail
//! bridge computes the identical string for the same bytes when it PUTs a
//! sealed content-index segment to `/api/v1/blob/{cid}`
//! (`bins/fauna-bridges/internal/byteplane`, `blobCID`). The nest recomputes
//! `blake3(body)` and answers **400 `cid_mismatch`** if the two disagree, so a
//! drift between the encoders is a hard production failure on the publish path.
//!
//! These vectors are what make that a *test* failure instead. The identical
//! strings are pinned on the Go side in `byteplane_test.go`; changing one
//! without the other is the bug this file exists to catch.

use fauna_cbor::Cid;

/// The bytes both languages hash. Any string works; what matters is that both
/// sides use the same one.
const VECTOR: &[u8] = b"fauna index segment golden vector";

/// Raw codec (0x55) — what an opaque sealed blob (a content-index segment)
/// addresses under.
#[test]
fn raw_codec_base32_golden() {
    assert_eq!(
        Cid::of_raw(VECTOR).to_base32(),
        "bafkr4igwtls73dmy7tjwwwugc6homoskipdjpcdy3fzqbtyvi3pcnpq5zm"
    );
}

/// The codec byte is part of the address: the same bytes under the dag-cbor
/// codec name a *different* CID. Pinned so a caller that reached for the wrong
/// constructor produces a visibly different string rather than a subtly wrong
/// one.
#[test]
fn the_codec_byte_changes_the_address() {
    assert_ne!(
        Cid::of_raw(VECTOR).to_base32(),
        Cid::of_dag_cbor(VECTOR).to_base32()
    );
}
