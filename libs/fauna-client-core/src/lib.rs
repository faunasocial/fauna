//! Platform-agnostic client operations for Fauna apps.
//!
//! This crate contains all shared logic between fauna-wasm (web) and
//! fauna-ffi (iOS/Android). Both binding crates are thin type-conversion
//! wrappers that delegate to functions here.

pub mod auth;
pub mod chunking;
mod cr_post;
pub mod email;
pub mod find_user;
pub mod identity;
pub mod linked_nests;
pub mod nest_trust;
pub mod post;
pub mod press;
pub mod recovery_chain;
pub mod recovery_pending;
pub mod scan;
pub mod succession_delivery;

/// Simple error type convertible to both JsValue (WASM) and FfiError (UniFFI).
#[derive(Debug, Clone)]
pub struct ClientError(pub String);

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ClientError {}

impl From<String> for ClientError {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for ClientError {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// Truncate a string to `max` bytes, appending "..." if truncated.
pub fn truncate(s: &str, max: usize) -> String {
    fauna_core::encoding::truncate_to_char_boundary_with_ellipsis(s, max)
}
