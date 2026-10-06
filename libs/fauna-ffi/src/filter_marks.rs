//! UniFFI façade for the email-filter succession review surface
//!.
//!
//! The twin plane of [`crate::member_review`] — same shared-Rust seam
//! shape (`fauna_client_config`'s `SuccessionLedgerStore`-backed marks, read
//! through the account-store handle), same FFI treatment. What this module adds is the piece the row names explicitly:
//! `load_filter_marks`/`decide_filter_mark` are plain shared Rust today,
//! reached by tui/linux directly; Swift/Kotlin/C#/TS need a UniFFI export.
//!
//! **No `filter_mark_removed` bundles a deletion — on purpose.** Unlike the
//! member-review plane (whose Remove must evict before it can earn a
//! verdict), a filter's removal mechanism already exists
//! (`fauna.email.filters.delete`) and there is deliberately no second one.
//! [`filter_mark_removed`] only RECORDS the verdict; the caller's existing
//! delete flow owns the deletion itself, called first, this function called
//! after — a failure here must leave a re-asked question, never a silenced
//! armed rule (its own trap note).
//!
//! **The post-succession raise (`raise_succession_filter_marks`) is not
//! exported here** — it runs in Rust, in the post-store-ready pass
//! (`crate::succession_aftermath::spawn_ledger_pass`), off the ceremony the
//! fold parked in the account registry; no app drives it.
//!
//! **No export takes an identity.** The marks rest on the succession ledger,
//! reached through this process's account-store handle
//! (`crate::account_runtime::handle`), which knows its own identity.
//!
//! Gated behind its own `filter-marks` feature (default-on, dropped from the
//! Go mail-bridge `--no-default-features` build — same shape as
//! `member-review`/`muted-keywords`/`sync-prefs`): the bridge has no settings
//! UI, and every export here either returns a bare primitive or calls the
//! gated `FfiNestClient::nest_arc()` accessor.

use fauna_client_config::{decide_filter_mark, load_filter_marks};
use fauna_core::data::UnattestedVerdict;

use crate::{FfiError, general_err};

/// The succession-ledger seam — this process's account-store handle — or the
/// refusal a review surface shows before the store is up.
pub(crate) fn ledger_store()
-> Result<fauna_sync_engine::account_runtime::AccountStoreHandle, FfiError> {
    crate::account_runtime::handle().ok_or_else(|| FfiError::General {
        msg: "the account store is not ready yet".into(),
    })
}

/// Read the ids of every filter rule still awaiting the owner's verdict —
/// what the Privacy filter list marks and what the aftermath review item
/// counts. Apps **cache** what this returns and answer per-row questions
/// against it: a filter list paints far more often than the ledger changes.
#[fauna_uniffi_async::export]
pub async fn filter_marks_list() -> Result<Vec<i64>, FfiError> {
    let store = ledger_store()?;
    load_filter_marks(&store).await.map_err(general_err)
}

/// Record **Keep** — the owner recognises this rule; it stays, and the mark
/// clears. Returns whether anything was actually open, so a caller can tell a
/// real adjudication from a no-op (a concurrent device may have already
/// answered, and that is a success no-op, never an error).
#[fauna_uniffi_async::export]
pub async fn filter_mark_keep(filter_id: i64) -> Result<bool, FfiError> {
    let store = ledger_store()?;
    decide_filter_mark(&store, filter_id, UnattestedVerdict::Kept)
        .await
        .map_err(general_err)
}

/// Record **Removed** — call ONLY after the caller's own `filter-delete`
/// gesture has already deleted the rule (module docs: this plane has no
/// second removal mechanism, and `Removed` here is recorded, never
/// enforced). Returns whether anything was actually open.
#[fauna_uniffi_async::export]
pub async fn filter_mark_removed(filter_id: i64) -> Result<bool, FfiError> {
    let store = ledger_store()?;
    decide_filter_mark(&store, filter_id, UnattestedVerdict::Removed)
        .await
        .map_err(general_err)
}
