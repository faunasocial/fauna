//! Linux-side glue for the shared `fauna-conversations` crate.
//!
//! Linux is in-process Rust — the `ConversationsManager` is a direct
//! dependency, no UniFFI hop. This module hosts the singleton and the
//! GTK-side observer that marshals snapshot-change notifications onto
//! the GTK main loop.
//!
//! Mirror of `apps/fauna-windows/FaunaApp/Conversations/` (host, observer,
//! commands). The view layer at `crate::views::conversations` consumes
//! the manager via these accessors.

pub mod conv_backend;
pub mod drafts;
pub mod host;
pub mod observer;
pub mod overlays;

pub use host::manager;

use std::cell::RefCell;
use std::collections::HashSet;

use fauna_conversations::message::MessageId;

// ---------------------------------------------------------------------------
// Muted-keyword cache + per-message reveal set (moderation.md § Muted
// keywords; content-moderation-and-ranking.md § Q3).
//
// The conversation bubble collapse (`views/conversations/message_bubble.rs`)
// matches a decrypted message's body against the user's muted-keyword list
// client-side, post-decrypt (the nest never sees plaintext or the list). GTK
// is single-threaded, so a thread-local cache is safe and avoids threading the
// list through every render call: it is populated once at auth
// (`FaunaClient::load_muted_keywords`, mirroring `refresh_mail_epoch_schedule`'s
// post-auth hook) and again on every Settings "Muted words" page load/save
// (`settings/muted_words.rs`). An edit made AFTER a thread rendered (a term's
// level flipped, a term added or removed) must reach that thread's reused
// bubbles, whose collapse is decided at build time: [`muted_keywords_epoch`]
// moves whenever the list does, and the thread view folds it into its rebuild
// check beside the content-policy epoch.
//
// `REVEALED_MUTED` is the session-local "show anyway" set: revealing a
// muted-collapsed message un-collapses that one message for the rest of the
// session (the mute itself is unaffected — un-muting the term is the only way
// to stop future messages collapsing).
thread_local! {
    static MUTED_KEYWORDS: RefCell<Vec<fauna_core::data::MutedKeyword>> = const { RefCell::new(Vec::new()) };
    /// See [`muted_keywords_epoch`].
    static MUTED_KEYWORDS_EPOCH: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static REVEALED_MUTED: RefCell<HashSet<MessageId>> = RefCell::new(HashSet::new());
    /// Session-local set of message ids whose content-policy `collapse` the viewer
    /// revealed (`family-safety.md` § Content policy — the `Collapse` verdict's
    /// reveal affordance; distinct from `REVEALED_MUTED`). The floor itself
    /// persists; the guardian relaxing it is what stops future collapse.
    static REVEALED_CONTENT: RefCell<HashSet<MessageId>> = RefCell::new(HashSet::new());
}

/// The current muted-keyword list (a clone of the cache), for
/// `fauna_core::scoring::muted_keywords_collapse` at bubble-render time.
pub fn muted_keywords_cache() -> Vec<fauna_core::data::MutedKeyword> {
    MUTED_KEYWORDS.with(|c| c.borrow().clone())
}

/// Replace the cached muted-keyword list — called after every load/save of the
/// Settings "Muted words" page and once at auth.
pub fn set_muted_keywords_cache(list: Vec<fauna_core::data::MutedKeyword>) {
    MUTED_KEYWORDS.with(|c| {
        let mut cached = c.borrow_mut();
        if *cached != list {
            MUTED_KEYWORDS_EPOCH.with(|e| e.set(e.get() + 1));
            *cached = list;
        }
    });
}

/// A counter that moves whenever the cached muted-keyword list changes (terms
/// or levels) — a bubble's muted collapse is decided when it is built, so the
/// thread view rebuilds every reused bubble when this moves.
pub fn muted_keywords_epoch() -> u64 {
    MUTED_KEYWORDS_EPOCH.with(|e| e.get())
}

/// Whether `id` has been revealed ("show anyway") this session.
pub fn is_muted_revealed(id: &MessageId) -> bool {
    REVEALED_MUTED.with(|s| s.borrow().contains(id))
}

/// Mark `id` as revealed for the rest of the session.
pub fn reveal_muted(id: MessageId) {
    REVEALED_MUTED.with(|s| {
        s.borrow_mut().insert(id);
    });
}

/// Whether `id`'s content-policy `collapse` has been revealed this session
/// (`family-safety.md` § Content policy).
pub fn is_content_revealed(id: &MessageId) -> bool {
    REVEALED_CONTENT.with(|s| s.borrow().contains(id))
}

/// Mark `id`'s content-policy collapse as revealed for the rest of the session.
pub fn reveal_content(id: MessageId) {
    REVEALED_CONTENT.with(|s| {
        s.borrow_mut().insert(id);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keyword(word: &str, weight: i64) -> fauna_core::data::MutedKeyword {
        fauna_core::data::MutedKeyword {
            keyword: word.to_string(),
            weight,
        }
    }

    /// An open thread's reused bubbles rebuild only when this moves
    /// (`views/conversations/detail.rs::bubble_policy_epoch`), so it must move
    /// when a term's LEVEL flips (same words, a different weight) and must not
    /// move when a reload hands back the list it already holds.
    #[test]
    fn the_muted_keywords_epoch_moves_exactly_when_the_list_does() {
        let start = muted_keywords_epoch();
        set_muted_keywords_cache(vec![keyword("lottery", -1000)]);
        let added = muted_keywords_epoch();
        assert_eq!(added, start + 1, "a new list moves the epoch");

        set_muted_keywords_cache(vec![keyword("lottery", -1000)]);
        assert_eq!(
            muted_keywords_epoch(),
            added,
            "an identical reload does not"
        );

        set_muted_keywords_cache(vec![keyword("lottery", -500)]);
        assert_eq!(
            muted_keywords_epoch(),
            added + 1,
            "a level flip on the same word moves it"
        );
    }
}
