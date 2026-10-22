//! UniFFI façade for the tier-1 **muted-keywords** word-list (frame Q3).
//!
//! Two surfaces the native apps consume, both over one shared definition so
//! nest and every app agree (priority #2/#3):
//!
//! * **Read / set** the user's sealed muted-keywords list — the `muted-words`
//!   Settings sub-page's CRUD. A thin pass-through to the shared
//!   `fauna_sync_engine::preference_surfaces` — the same calls tui and linux
//!   make — which read and write the account store of this process's runtime
//!   (`crate::account_runtime::handle_source()`), waiting for it when a call
//!   arrives before the assembly has landed (`config-dissolution.md` § The
//!   `__config` dissolution schedule → *The closure order*, steps (1) and
//!   (5)). The normalize-on-write is one shared function. The list rests
//!   sealed on the account-state plane; the nest never receives or can
//!   decrypt it.
//! * **Match** a decrypted conversation body against a muted list —
//!   [`matches_muted_keywords`], a thin sync wrapper over the canonical shared
//!   collapse decision `fauna_core::scoring::muted_keywords_collapse` (the
//!   shared matcher `fauna_core::keyword::body_excludes_matches` per term, then
//!   the weights: only a keyword muted at the full penalty collapses). The
//!   conversation view calls it post-decrypt to decide whether to collapse a
//!   message behind the muted-keyword reveal affordance. Sharing the one
//!   decision is why nest and client can never fork the "does this body match"
//!   semantics.
//!
//! The exported fns take built-in types or the shared `MutedKeyword` record
//! (term + weight, registered under `fauna_core`'s uniffi namespace) and return
//! either a `bool` or the shared `MutedWordsSnapshot` record (`Vec<MutedKeyword>`
//! / `bool`). The snapshot is registered under
//! `fauna_client_config`'s uniffi namespace, forwarded by **this crate's
//! `muted-keywords` feature** — which the Go mail-bridge's
//! `--no-default-features` build drops along with this whole module, so the
//! checked-in Go bindings stay byte-identical (the same gating rationale the
//! feature already carried).
//!
//! Authority for behavior: `docs/goal/architecture/content-moderation-and-ranking.md`
//! § Resolved design decisions Q3 + `docs/goal/behavior/moderation.md`
//! § Muted keywords; at-rest shape: the delegable
//! `fauna.state.moderation` kind (`docs/goal/architecture/config-dissolution.md`).

use fauna_client_config::{MutedKeyword, MutedWordsSnapshot};
use fauna_core::scoring::MutedKeywordLevel;
use fauna_sync_engine::preference_surfaces;

use crate::{FfiError, general_err};

/// Whether a preference page's load-failure text is the first-listing gate's
/// refusal (`common.needs_nest` / `common.needs_other_device`) — the one
/// shared predicate behind every seat that logs a background read's failure
/// but must say why a page has not loaded when the replica is not ready
/// (`account-client-lifecycle.md` § The client-side lifecycle → *The first
/// listing*, clauses (4) and (5)). A failure it does not recognise stays the
/// seat's ordinary background-read handling.
#[uniffi::export]
pub fn refused_as_not_ready(reason: String) -> bool {
    preference_surfaces::refused_as_not_ready(&reason)
}

/// Read the owner's muted-keywords list from the sealed
/// `fauna.state.moderation` record, as the page record
/// [`MutedWordsSnapshot`] — the rows **and** the `loaded` bit that gates
/// `muted-word-empty` (`docs/goal/ui/README.md` § *List pages: loading is not
/// empty*). A caller that only needs the list (the conversation collapse) reads
/// `.keywords`; a caller rendering the page renders each entry's `keyword` and
/// must gate its empty state on `.loaded`, because an empty `keywords` here
/// means "no terms", never "not read yet".
#[fauna_uniffi_async::export]
pub async fn load_muted_words() -> Result<MutedWordsSnapshot, FfiError> {
    preference_surfaces::load_muted_words(&crate::account_runtime::handle_source())
        .await
        .map_err(preference_surfaces::plane_failure)
        .map_err(general_err)
}

/// Replace the owner's muted-keywords list (terms and weights) and persist. The
/// input is normalized (trim, drop blanks, case-insensitive dedupe keeping the
/// first-seen entry, weights clamped) by the shared seam; the normalized stored
/// list is returned so the UI shows exactly what was saved. Whole-record
/// latest-wins on `updated_at` resolves concurrent edits across the device
/// fleet.
#[fauna_uniffi_async::export]
pub async fn save_muted_words(keywords: Vec<MutedKeyword>) -> Result<MutedWordsSnapshot, FfiError> {
    preference_surfaces::save_muted_words(&crate::account_runtime::handle_source(), keywords)
        .await
        .map_err(preference_surfaces::plane_failure)
        .map_err(general_err)
}

/// Add one term to the owner's muted-keywords list — the UniFFI face of
/// [`preference_surfaces::add_muted_word`], the production shape of the page's
/// add gesture.
///
/// A **delta**, never the page's list wholesale: the seam re-reads the stored
/// list inside its own CAS update, so a term another device stored since this
/// page loaded survives this click — the exact clobber every app's
/// send-my-in-memory-list shape used to carry. Re-adding an existing term is a
/// no-op (case-insensitive dedupe keeps first-seen spelling); the returned
/// rows are the stored normalization.
#[fauna_uniffi_async::export]
pub async fn add_muted_word(word: String) -> Result<MutedWordsSnapshot, FfiError> {
    preference_surfaces::add_muted_word(&crate::account_runtime::handle_source(), &word)
        .await
        .map_err(preference_surfaces::plane_failure)
        .map_err(general_err)
}

/// Remove one term — [`add_muted_word`]'s inverse
/// ([`preference_surfaces::remove_muted_word`]): exact stored spelling, and
/// removing a term already gone is a success no-op (another device deleting it
/// first is convergence, not an error).
#[fauna_uniffi_async::export]
pub async fn remove_muted_word(word: String) -> Result<MutedWordsSnapshot, FfiError> {
    preference_surfaces::remove_muted_word(&crate::account_runtime::handle_source(), &word)
        .await
        .map_err(preference_surfaces::plane_failure)
        .map_err(general_err)
}

/// Set one term's level — the muted-words page's level picker, the UniFFI
/// face of [`preference_surfaces::set_muted_word_level`]. `Hide` is the full
/// penalty (collapses and sinks), `ShowLess` sinks a post in a ranked feed and
/// nothing else; the weights behind the two levels live in
/// `fauna_core::scoring::MutedKeywordLevel`, never in an app. A delta like
/// [`add_muted_word`]: exact stored spelling, and a term the list does not
/// hold is a success no-op; the stored page record is returned, each row's
/// level readable through [`muted_keyword_level`].
#[fauna_uniffi_async::export]
pub async fn set_muted_word_level(
    word: String,
    level: MutedKeywordLevel,
) -> Result<MutedWordsSnapshot, FfiError> {
    preference_surfaces::set_muted_word_level(
        &crate::account_runtime::handle_source(),
        &word,
        level,
    )
    .await
    .map_err(preference_surfaces::plane_failure)
    .map_err(general_err)
}

/// The level the muted-words page's level picker shows for a row's stored
/// `weight` — `fauna_core::scoring::MutedKeywordLevel::of`: `Hide` iff the
/// weight collapses (the full penalty), else `ShowLess`. The one shared
/// threshold, so no app classifies a row by comparing numbers of its own; the
/// native twin of the wasm `mutedKeywordLevel`.
#[uniffi::export]
pub fn muted_keyword_level(weight: i64) -> MutedKeywordLevel {
    MutedKeywordLevel::of(weight)
}

/// Both levels in the picker's order — `MutedKeywordLevel::ALL`, so a native
/// app builds the `muted-word-level-select` options from the shared catalog
/// rather than listing the two itself.
#[uniffi::export]
pub fn muted_keyword_levels() -> Vec<MutedKeywordLevel> {
    MutedKeywordLevel::ALL.to_vec()
}

/// A level's own spelling (`hide` | `show-less`) — the picker's option value,
/// which a driver selects and reads back. The native twin of
/// `MutedKeywordLevel::value`; no app spells it.
#[uniffi::export]
pub fn muted_keyword_level_value(level: MutedKeywordLevel) -> String {
    level.value().to_string()
}

/// The level a picker value names — the inverse of [`muted_keyword_level_value`];
/// `None` for a value no level carries.
#[uniffi::export]
pub fn muted_keyword_level_from_value(value: String) -> Option<MutedKeywordLevel> {
    MutedKeywordLevel::from_value(&value)
}

/// Does `body` collapse behind the `muted_keywords` list? A thin sync wrapper
/// over the canonical shared decision `fauna_core::scoring::muted_keywords_collapse`:
/// a case-insensitive substring match of a term muted at the full penalty (a
/// softer weight only demotes in a ranked feed, and a conversation is not
/// ranked). The conversation view calls this post-decrypt to decide whether to
/// collapse a message behind the muted-keyword reveal affordance. `false` for
/// an empty list — no mutes, nothing collapses.
#[uniffi::export]
pub fn matches_muted_keywords(body: String, muted_keywords: Vec<MutedKeyword>) -> bool {
    fauna_core::scoring::muted_keywords_collapse(&muted_keywords, &body)
}

// ── The notification collapse and the toast gate (`behavior/notifications.md`
// § Muted words) ──
//
// Pure functions over one row or push and the list the app already holds for
// its conversation collapse (`load_muted_words`' `.keywords`) — never a second
// read. The decision is `fauna_client_notifications`'; the app paints the
// collapse and withholds the toast.

/// Does a muted word collapse this notification row behind
/// `notification-muted` and its reveal? The UniFFI face of
/// `fauna_client_notifications::notification_muted`: the match runs over the
/// row's relayed text (its `body` args, or its `summary` when it has no body),
/// only a word muted at Hide collapses, and a `security.notice` never does.
#[uniffi::export]
pub fn notification_muted_for(
    item: crate::FfiNotifItem,
    muted_keywords: Vec<MutedKeyword>,
) -> bool {
    fauna_client_notifications::notification_muted(&muted_keywords, &item.into())
}

/// Should the in-app toast for this `fauna.notification` push be withheld?
/// The same decision as [`notification_muted_for`] over the push (rule 6: a
/// toast cannot be revealed, so a row that would collapse raises none).
#[cfg(feature = "push-subscription")]
#[uniffi::export]
pub fn notification_push_muted_for(
    notification: crate::FfiNotification,
    muted_keywords: Vec<MutedKeyword>,
) -> bool {
    fauna_client_notifications::notification_push_muted(&muted_keywords, &notification.into())
}

/// Should the knock toast for this `fauna.knock` push be withheld? The knock's
/// message is an arg of its row, so the toast follows the row (rule 6).
#[cfg(feature = "conversations-session")]
#[uniffi::export]
pub fn knock_push_muted_for(knock: crate::FfiKnock, muted_keywords: Vec<MutedKeyword>) -> bool {
    fauna_client_notifications::knock_push_muted(&muted_keywords, &knock.into())
}

// ── The reporter-side hide (`moderation.md` § Corollary — block also hides) ──
//
// The same sealed record as the muted words, so the same store: the
// `preference_surfaces` hide deltas. The list feeds `content_render_for_item`.

/// The ids the owner hid by reporting them.
#[fauna_uniffi_async::export]
pub async fn load_hidden_content() -> Result<Vec<String>, FfiError> {
    preference_surfaces::load_hidden_content(&crate::account_runtime::handle_source())
        .await
        .map_err(preference_surfaces::plane_failure)
        .map_err(general_err)
}

/// Hide a reported subject (a post or message id, or an account's actor id)
/// for the owner; returns the stored list.
#[fauna_uniffi_async::export]
pub async fn hide_reported(id: String) -> Result<Vec<String>, FfiError> {
    preference_surfaces::hide_reported(&crate::account_runtime::handle_source(), &id)
        .await
        .map_err(preference_surfaces::plane_failure)
        .map_err(general_err)
}

/// Show a hidden subject again; returns the stored list.
#[fauna_uniffi_async::export]
pub async fn unhide_reported(id: String) -> Result<Vec<String>, FfiError> {
    preference_surfaces::unhide_reported(&crate::account_runtime::handle_source(), &id)
        .await
        .map_err(preference_surfaces::plane_failure)
        .map_err(general_err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_protocol::LocalizedText;

    fn knock_body(message: &str) -> LocalizedText {
        LocalizedText::new("notifications.row_knock")
            .with_arg("sender", "abcd1234")
            .with_arg("message", message)
    }

    /// The façades carry the row and the push whole across the boundary: the
    /// relayed arg decides, through the mirror types, exactly as the wire
    /// types decide in `fauna_client_notifications`.
    #[test]
    fn the_facades_decide_on_the_relayed_args_through_the_mirrors() {
        let muted = vec![MutedKeyword::new("spoiler")];
        let item = |message: &str| fauna_protocol::notifications::NotifItem {
            notif_type: "knock".into(),
            source: "fauna".into(),
            summary: "abcd1234 wants to connect".into(),
            body: Some(knock_body(message)),
            ..Default::default()
        };
        assert!(notification_muted_for(
            item("a SPOILER").into(),
            muted.clone()
        ));
        assert!(!notification_muted_for(item("hello").into(), muted.clone()));

        #[cfg(feature = "push-subscription")]
        for (message, want) in [("a SPOILER", true), ("hello", false)] {
            let ev = crate::FfiPushEvent::from(fauna_protocol::PushEvent::Notification(
                fauna_protocol::push_events::NotificationPayload {
                    notification_id: 1,
                    notif_type: "knock".into(),
                    source: "fauna".into(),
                    sender_id: None,
                    content_id: None,
                    summary: "abcd1234 wants to connect".into(),
                    body: Some(knock_body(message)),
                    timestamp: 1,
                    extra: Default::default(),
                },
            ));
            let crate::FfiPushEvent::Notification { notification } = ev else {
                panic!("expected a notification push");
            };
            assert_eq!(
                notification_push_muted_for(notification, muted.clone()),
                want
            );
        }

        #[cfg(feature = "conversations-session")]
        for (message, want) in [("a SPOILER", true), ("hello", false)] {
            let knock = crate::FfiKnock {
                sender_id: "abcd1234".into(),
                summary: message.into(),
                body: Some(fauna_core::localized::LocalizedText::key_args(
                    "notifications.row_knock",
                    [("message".to_string(), message.to_string())],
                )),
            };
            assert_eq!(knock_push_muted_for(knock, muted.clone()), want);
        }
    }
}
