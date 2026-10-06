//! Shared fixtures for the `fauna-conversations` integration tests.
//!
//! Each file under `tests/` is its own crate, so a helper used by one file is
//! dead code in the next — the module-wide allow below is what lets this
//! module be `mod common;`-ed into a test that needs only some of its
//! helpers (the fauna-nest and fauna-sync-agent integration tests' own
//! `tests/common/mod.rs` are the same pattern).
#![allow(dead_code)]

use async_trait::async_trait;
use fauna_conversations::backend::{ConvRpcError, ConversationsRpc, WelcomeChannelKind};

/// A nest with an empty MLS rail, for a test that drives the mail rail only.
///
/// Byte-identical across `index_launch_ordering_tests.rs`,
/// `index_catch_up_boundary_tests.rs` and (apart from three methods)
/// `receive_cycle_poke_tests.rs` (round 29). Every method errors by default —
/// an unexpectedly-reached call fails loudly rather than silently no-opping —
/// except `channel_fetch`, which the receive loop's channel sweep
/// unconditionally calls and so always returns empty. `permissive` switches
/// the three methods `receive_cycle_poke_tests.rs` needs to succeed silently
/// (its flow legitimately reaches them, unlike the other two callers').
pub struct SilentNest {
    /// Named in every error, so a wrongly-reached method's panic/reject
    /// names which test's "mail rail only" nest was hit.
    pub label: &'static str,
    /// `false` (the default): `keypackage_count`, `keypackage_upload` and
    /// `blob_get` also error like every other method. `true`:
    /// `receive_cycle_poke_tests.rs`'s shape — those three answer a silent
    /// `Ok` instead.
    pub permissive: bool,
}

impl SilentNest {
    fn unused(&self, what: &str) -> ConvRpcError {
        ConvRpcError::Rejected {
            message: format!("{} drives the mail rail only ({what})", self.label),
        }
    }
}

#[async_trait]
impl ConversationsRpc for SilentNest {
    async fn channel_send(
        &self,
        _c: String,
        _e: Vec<u8>,
        _expect_no_commit_since: Option<i64>,
        _attachment_refs: Vec<String>,
    ) -> Result<i64, ConvRpcError> {
        Err(self.unused("channel_send"))
    }
    async fn channel_send_remote(
        &self,
        _c: String,
        _u: String,
        _e: Vec<u8>,
        _expect_no_commit_since: Option<i64>,
        _attachment_refs: Vec<String>,
    ) -> Result<i64, ConvRpcError> {
        Err(self.unused("channel_send_remote"))
    }
    /// The one method the receive loop's channel sweep actually calls: no
    /// channels, so no MLS traffic.
    async fn channel_fetch(
        &self,
        _c: String,
        _a: i64,
        _l: i64,
        _h: Option<String>,
    ) -> Result<Vec<fauna_conversations::backend::FetchedRecord>, ConvRpcError> {
        Ok(vec![])
    }
    async fn keypackage_count(&self, _a: String) -> Result<u64, ConvRpcError> {
        if self.permissive {
            Ok(0)
        } else {
            Err(self.unused("keypackage_count"))
        }
    }
    async fn actor_by_handle(
        &self,
        _h: String,
    ) -> Result<Option<fauna_conversations::backend::ResolvedHandle>, ConvRpcError> {
        Err(self.unused("actor_by_handle"))
    }
    async fn actor_by_handle_remote(
        &self,
        _d: String,
        _l: String,
    ) -> Result<Option<fauna_conversations::backend::ResolvedHandle>, ConvRpcError> {
        Err(self.unused("actor_by_handle_remote"))
    }
    async fn keypackage_fetch(
        &self,
        _a: String,
        _p: Option<String>,
    ) -> Result<Option<Vec<u8>>, ConvRpcError> {
        Err(self.unused("keypackage_fetch"))
    }
    async fn keypackage_upload(&self, _p: Vec<Vec<u8>>, _l: bool) -> Result<u64, ConvRpcError> {
        if self.permissive {
            Ok(0)
        } else {
            Err(self.unused("keypackage_upload"))
        }
    }
    async fn welcome_deliver(
        &self,
        _r: String,
        _c: String,
        _w: Vec<u8>,
        _k: WelcomeChannelKind,
        _p: Option<String>,
    ) -> Result<(), ConvRpcError> {
        Err(self.unused("welcome_deliver"))
    }
    async fn blob_put(
        &self,
        _c: String,
        _h: Option<String>,
        _s: String,
        _b: Vec<u8>,
    ) -> Result<(), ConvRpcError> {
        Err(self.unused("blob_put"))
    }
    async fn blob_get(
        &self,
        _c: String,
        _h: Option<String>,
        _s: String,
    ) -> Result<Option<Vec<u8>>, ConvRpcError> {
        if self.permissive {
            Ok(None)
        } else {
            Err(self.unused("blob_get"))
        }
    }
}
