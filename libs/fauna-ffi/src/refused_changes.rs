//! UniFFI face of the **refused-change list** on the Events page — the
//! windows, macOS, iOS and android twin of what tui calls directly
//! (`docs/goal/ui/events.md` § Refused scheduling changes; the record and its
//! no-"apply anyway" ruling are `docs/goal/behavior/inbound-scheduling-authority.md`
//! § Surfacing).
//!
//! One call answers the whole page's reading, so no app resolves a handle,
//! words a security notice or filters a dismissed row:
//!
//! - [`refused_changes_open`] — the open rows over this process's account-store
//!   handle (`fauna_client_account_runtime::refused_changes::load_views`), each
//!   composed by `fauna_client_caldav::refused_change_view` with the sender
//!   handle `manager` resolves for the row's author. The handle is re-derived
//!   at every read, never cached into the record, and is only a *candidate*:
//!   the shared view decides whether the nest that attested the author may
//!   lend it.
//! - [`refused_change_dismiss`] — the row's only control, a local store write.
//!
//! A store that is not up yet reads empty (the surface's next load fills it
//! in), so the list stays absent rather than erroring.

use std::sync::Arc;

use fauna_client_account_runtime::refused_changes::{dismiss, load_views};
use fauna_conversations::ConversationsManager;
use fauna_core::localized::LocalizedText;

use crate::{FfiError, general_err};

/// One refused-change row as the Events page paints it: the four texts of
/// `refused-change-item` plus the key `refused-change-dismiss` hands back.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiRefusedChange {
    /// What `refused-change-dismiss` passes to [`refused_change_dismiss`].
    pub key: String,
    /// `refused-change-title` — what was tried, and to which event.
    pub headline: LocalizedText,
    /// `refused-change-author` — who tried it, as the shared view words it.
    pub author: LocalizedText,
    /// `refused-change-reason` — why it was refused.
    pub reason: LocalizedText,
    /// The repeat count, present only past one attempt. It rides the reason
    /// line (`reason`, a space, `attempts`) — ui.yaml gives the row four
    /// texts, not five.
    pub attempts: Option<LocalizedText>,
    /// `refused-change-time` — epoch seconds of the latest attempt; the app
    /// formats the date.
    pub last_refused_at: i64,
}

/// The open refused changes, most recent attempt first. Empty when there is
/// nothing to report — the list paints only when this is non-empty.
#[fauna_uniffi_async::export]
pub async fn refused_changes_open(manager: Arc<ConversationsManager>) -> Vec<FfiRefusedChange> {
    let store = crate::account_runtime::handle();
    load_views(store.as_ref(), &manager)
        .await
        .into_iter()
        .map(|view| FfiRefusedChange {
            key: view.key,
            headline: view.headline,
            author: view.author,
            reason: view.reason,
            attempts: view.attempts,
            last_refused_at: view.last_refused_at,
        })
        .collect()
}

/// Dismiss the row `key` names — the owner's *I have seen this*. Answers
/// whether it was open (a sibling device may have dismissed it first, which is
/// a success no-op).
///
/// # Errors
/// The account store is not up yet, or its door refused the write.
#[fauna_uniffi_async::export]
pub async fn refused_change_dismiss(key: String) -> Result<bool, FfiError> {
    let store = crate::account_runtime::handle();
    dismiss(store.as_ref(), &key).await.map_err(general_err)
}
