//! UniFFI façade for shared MIME-type detection
//! (`fauna_core::share::content_type_for_filename`).
//!
//! One free fn over built-in types (`String` → `String`), so uniffi-bindgen-go
//! *could* emit it — but it is gated behind the default-on `mime` feature so the
//! Go mail-bridge `--no-default-features` build drops it, keeping it off the
//! checked-in Go bindings and avoiding an off-win Go regen (memory
//! reference_ffi_gate_conversations_session_excludes_go; same rationale as
//! `nest-trust`). The bridge derives content types server-side from the wire, not
//! from client filenames.
//!
//! Replaces per-app hand-rolled extension→MIME maps (e.g. the windows
//! `MimeDetect` 5-entry image-only map) with the one shared catalog every native
//! app resolves through this export (priority #2/#4 — one shape across all 6
//! apps).

/// Return the MIME content-type for `filename` from its extension
/// (case-insensitive). `"application/octet-stream"` for unknown or missing
/// extensions. Thin façade over `fauna_core::share::content_type_for_filename`
/// (the canonical text / application / image / audio / video / font catalog).
#[uniffi::export]
pub fn content_type_for_filename(filename: String) -> String {
    fauna_core::share::content_type_for_filename(&filename).to_string()
}
