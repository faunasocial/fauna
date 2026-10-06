//! Smoke test: wasm-bindgen-test that the fauna-media re-exports are
//! reachable from `fauna-wasm` and the UploadSidecar CBOR round-trip works.

#![cfg(target_arch = "wasm32")]

use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

#[wasm_bindgen_test]
fn sidecar_cbor_roundtrip() {
    // Library variant — uses BackupKey, which is a sealed key type. We don't
    // construct one here (it requires a backup seed); the smoke test exercises
    // serde shape, not seal dispatch.
    let sc = fauna_wasm::UploadSidecar {
        class: fauna_wasm::AudienceClass::PublicPost,
        mime: "image/png".to_string(),
        has_c2pa: true,
        thumbnail_hash: Some([7u8; 32]),
    };
    let buf = sc.to_dag_cbor();
    let round = fauna_wasm::UploadSidecar::from_dag_cbor(&buf).unwrap();
    assert_eq!(round, sc);
}

#[wasm_bindgen_test]
fn public_post_wrapper_round_trips_in_browser() {
    // Exercises the JS-callable wrapper end-to-end in the real wasm target:
    // seal (passthrough for PublicPost) + sidecar encode + getters. The native
    // unit tests in `lib.rs` cover the same logic; this proves it links and
    // runs under wasm32.
    let raw = b"\x89PNG\r\n\x1a\n fake png bytes";
    let payload = fauna_wasm::process_and_seal_public_post(raw, "image/png", false);
    assert_eq!(payload.bytes(), raw.to_vec());
    assert_eq!(payload.mime(), "image/png");
    let sidecar = fauna_wasm::UploadSidecar::from_dag_cbor(payload.sidecar().as_slice()).unwrap();
    assert_eq!(sidecar.class, fauna_wasm::AudienceClass::PublicPost);
    assert_eq!(sidecar.mime, "image/png");
}
