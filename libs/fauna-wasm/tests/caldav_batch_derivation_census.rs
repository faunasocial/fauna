//! Census, ENUMERATED rather than re-audited by hand : no
//! production caller in this crate may reach for the single-shot
//! `caldav::unseal_event_body` / `unseal_card_body` / `unseal_dav_body`
//! convenience wrapper — each call re-derives `DavRecipientKeys` from
//! scratch (one ML-KEM-768 keygen plus one classical HPKE derive). A batch
//! caller must derive the keys ONCE and call `keys.unseal(...)` per entry
//! instead (the `fauna-ffi` native twins' own pattern).
//!
//! Why this lives here rather than beside the leaf-level count pin
//! (`fauna_mls::wrapped_blob::dav_body::tests::
//! n_opens_under_one_derivation_cost_one_keygen`): that pin counts
//! `DavRecipientKeys::derive` calls, so it can only observe a caller that
//! reaches the leaf THROUGH the type — it is structurally blind to a caller
//! that keeps calling the single-shot wrapper directly, which is exactly how
//! `caldav_export_calendar` went unnoticed after the batch-derivation
//! cascade converted its sibling call sites in this same file (`rpc.rs`).
//! This census inspects the CALLER's own source instead, closing that blind
//! spot for every present and future call site in this crate.
//!
//! `fauna_wasm`'s modules are `#[cfg(target_arch = "wasm32")]`-gated and the
//! crate does not compile natively (`fauna-wasm-not-checkable-on-native`), so
//! this runs as a `#[wasm_bindgen_test]` under `wasm-pack test`, same as
//! `subscription.rs` — `run_in_browser`, not the wasm-bindgen-test default of
//! Node (this project's toolchain has no Node.js).

use wasm_bindgen_test::wasm_bindgen_test;

wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

#[wasm_bindgen_test]
fn no_production_caller_uses_the_single_shot_dav_unseal() {
    const SRC: &str = include_str!("../src/rpc.rs");

    for banned in [
        "unseal_event_body(",
        "unseal_card_body(",
        "unseal_dav_body(",
    ] {
        assert!(
            !SRC.contains(banned),
            "found `{banned}` in fauna-wasm's rpc.rs — a batch caller must derive \
             DavRecipientKeys ONCE (`caldav::DavRecipientKeys::derive(&msek)`) and \
             call `keys.unseal(...)` per entry instead of the single-shot \
             convenience wrapper, which re-derives on every call"
        );
    }
}
