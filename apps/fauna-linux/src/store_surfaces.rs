//! This app's consumer of the **store-change notice** — the one shared watch
//! (`fauna_client_account_runtime::store_change`;
//! `docs/goal/architecture/account-runtime.md` § Multi-instance concurrency →
//! *A runtime's own pump is a source of the notice too*, part 4): an OPEN
//! surface whose render source is read through the account store shows what a
//! fresh visit would show, whichever process applied the change — this
//! runtime's own pump, a sibling same-account instance, or the sync agent.
//!
//! [`watch`] posts a payload-free `DataMessage::AccountStoreChanged`;
//! `app.rs`'s handler asks [`open_surface`] which store-backed surface is
//! showing and re-drives that surface's own load — reload semantics, the same
//! refetch a same-page nav fires, no new render path (tui's
//! `settings::store_resync_op` is the reference). **A new store-backed surface
//! joins [`StoreSurface`] in the change that adds it**: the enum is matched
//! exhaustively where the re-drives are wired
//! (`views::settings_shell`), so a new variant cannot be forgotten there.
//!
//! The notice is a level, not an event: each re-driven load paints only what
//! differs and never discards an edit in progress — those halves are the
//! load's own (e.g. `settings::muted_words::apply_load`). A gesture's own
//! write is not a source (part 2): the page that made it repaints from its
//! own answer.

use fauna_sync_engine::account_runtime::AccountStoreHandle;

use crate::app::{DataMessage, UiMessage};
use crate::client::UiSender;

/// A surface whose load reads through the account store.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreSurface {
    /// The feed's sealed scorers (muted keywords, trained factors) load only
    /// inside the manager's reload.
    Feed,
    /// Settings → Muted words.
    MutedWords,
    /// Settings → Task delegation (the pins).
    TaskDelegation,
    /// Settings → Folders (followed folders, foreign sets).
    Folders,
    /// Settings → Devices (the custody facet, the enrolled row, the keyed
    /// set).
    Devices,
    /// Settings → Nests (the custody facet, the escrow holders).
    Nests,
}

/// Which store-backed surface is open, from the content stack's visible child
/// and the Settings sub-stack's. `None` when the open page reads nothing
/// through the store: the next visit's own load serves fresh state anyway.
pub fn open_surface(content: Option<&str>, settings_sub: Option<&str>) -> Option<StoreSurface> {
    match content? {
        "feed" => Some(StoreSurface::Feed),
        "settings" => match settings_sub? {
            "muted-words" => Some(StoreSurface::MutedWords),
            "task-delegation" => Some(StoreSurface::TaskDelegation),
            "folders" => Some(StoreSurface::Folders),
            "devices" => Some(StoreSurface::Devices),
            "nests" => Some(StoreSurface::Nests),
            _ => None,
        },
        _ => None,
    }
}

/// Post a payload-free [`DataMessage::AccountStoreChanged`] whenever the
/// account store may have changed. Ends when the runtime is gone — sign-out's
/// deterministic shutdown makes the floor's read err — so the handle clone
/// held here never keeps alive a runtime the app has let go of.
pub async fn watch(handle: AccountStoreHandle, tx: UiSender) {
    let mut watch = fauna_client_account_runtime::store_change::StoreChangeWatch::new(handle).await;
    while watch.changed().await {
        tx.send(UiMessage::Data(DataMessage::AccountStoreChanged));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The handler re-drives the open store-backed surface and nothing else.
    #[test]
    fn only_the_open_store_backed_surface_is_named() {
        use StoreSurface::*;
        assert_eq!(open_surface(Some("feed"), Some("muted-words")), Some(Feed));
        for (sub, surface) in [
            ("muted-words", MutedWords),
            ("task-delegation", TaskDelegation),
            ("folders", Folders),
            ("devices", Devices),
            ("nests", Nests),
        ] {
            assert_eq!(open_surface(Some("settings"), Some(sub)), Some(surface));
            // The sub-stack keeps its child while another top-level page shows.
            assert_eq!(open_surface(Some("conversations"), Some(sub)), None);
        }
        // A sub-page that reads nothing through the store, and no page at all.
        assert_eq!(open_surface(Some("settings"), Some("status")), None);
        assert_eq!(open_surface(Some("settings"), None), None);
        assert_eq!(open_surface(None, Some("muted-words")), None);
    }
}
