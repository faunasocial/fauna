//! The **followed-public-folders chokepoint** — the one shared place the
//! account's follows are read and written (`docs/goal/behavior/folders.md`
//! § Publicly-synced follow), over the [`FollowsStore`] seam
//! (`fauna.state.follows`, one row per followed folder —
//! `config-dissolution.md` § The `__config` dissolution schedule).
//!
//! Every app needs follow / unfollow / list, and each answers the page with
//! the whole stored list, so the write-then-read-back sequencing lives here
//! once rather than in each face (priority #4 — resolve drift, don't match
//! it).
//!
//! **Everything about a follow lives here.** The home nest holds no follower
//! state — no roster, no registration, no per-follower row — so the account's
//! row is not a cache of anything: it *is* the follow. Unfollow is its stamped
//! tombstone, and it propagates across the user's own devices because each
//! row is latest-wins on its own stamp (the door, `fauna_account_plane`'s
//! `follows_rows`, owns the mechanics).

use fauna_core::data::FollowedFolder;

use crate::store_seam::{FollowsStore, StoreError};

/// The user's followed public folders, in canonical order. Empty until they
/// follow one.
pub async fn load_followed_folders<S: FollowsStore + ?Sized>(
    store: &S,
) -> Result<Vec<FollowedFolder>, StoreError> {
    Ok(store.follows().await?.followed)
}

/// Record a follow — or refresh it in place: the display name and the nest
/// stamp come from the latest fetch — and answer the stored list.
///
/// One row per folder, so a concurrent follow of another folder from another
/// device is never touched by this write.
pub async fn save_follow<S: FollowsStore + ?Sized>(
    store: &S,
    record: FollowedFolder,
) -> Result<Vec<FollowedFolder>, StoreError> {
    store.put_follow(record).await?;
    load_followed_folders(store).await
}

/// Unfollow and answer the stored list. Unfollowing something that is already
/// gone is success (and writes nothing).
pub async fn save_unfollow<S: FollowsStore + ?Sized>(
    store: &S,
    home_nest_url: &str,
    folder_id: i64,
) -> Result<Vec<FollowedFolder>, StoreError> {
    store.unfollow(home_nest_url.to_string(), folder_id).await?;
    load_followed_folders(store).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::FakeFollowsStore;
    use fauna_client_testkit::block_on;

    fn record(home: &str, id: i64, name: &str) -> FollowedFolder {
        FollowedFolder {
            home_nest_url: home.to_string(),
            home_nest_actor_id: Some("ab".repeat(32)),
            owner_actor_id: "cd".repeat(32),
            owner_handle: Some("alice".to_string()),
            folder_id: id,
            display_name: name.to_string(),
        }
    }

    /// A follow reads back whole, and unfollow removes it and stays removed.
    #[test]
    fn follow_persists_and_unfollow_removes_it() {
        let store = FakeFollowsStore::empty();

        let stored = block_on(save_follow(
            &store,
            record("https://peer.example", 7, "site"),
        ))
        .expect("follow");
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].folder_id, 7);
        assert_eq!(stored[0].display_name, "site");

        let read = block_on(load_followed_folders(&store)).expect("read");
        assert_eq!(read, stored);
        assert_eq!(
            read[0].owner_handle.as_deref(),
            Some("alice"),
            "the handle the follow was made by is kept"
        );

        let after = block_on(save_unfollow(&store, "https://peer.example", 7)).expect("unfollow");
        assert!(after.is_empty(), "unfollow removes the record");
        assert!(
            block_on(load_followed_folders(&store))
                .expect("read again")
                .is_empty()
        );
    }

    /// Re-following refreshes in place rather than duplicating: the gesture is
    /// idempotent, and the display name tracks the latest fetch.
    #[test]
    fn re_following_refreshes_the_record_in_place() {
        let store = FakeFollowsStore::empty();
        block_on(save_follow(
            &store,
            record("https://peer.example", 7, "site"),
        ))
        .expect("follow");

        let stored = block_on(save_follow(
            &store,
            record("https://peer.example", 7, "portfolio"),
        ))
        .expect("re-follow");

        assert_eq!(stored.len(), 1, "no duplicate row for the same address");
        assert_eq!(
            stored[0].display_name, "portfolio",
            "the record tracks the latest fetch"
        );
    }

    /// Follows at different addresses coexist and come back in canonical
    /// order, whichever order they were added in.
    #[test]
    fn several_follows_coexist_in_canonical_order() {
        let store = FakeFollowsStore::empty();
        for rec in [
            record("https://z.example", 2, "zed"),
            record("https://a.example", 9, "alpha"),
            record("https://a.example", 3, "also-alpha"),
        ] {
            block_on(save_follow(&store, rec)).expect("follow");
        }

        let read = block_on(load_followed_folders(&store)).expect("read");
        let addresses: Vec<(&str, i64)> = read
            .iter()
            .map(|f| (f.home_nest_url.as_str(), f.folder_id))
            .collect();
        assert_eq!(
            addresses,
            vec![
                ("https://a.example", 3),
                ("https://a.example", 9),
                ("https://z.example", 2),
            ],
            "sorted by (home_nest_url, folder_id), independent of insertion order"
        );
    }

    /// Unfollowing something already gone is success, writes nothing, and
    /// does not disturb the rest of the list.
    #[test]
    fn unfollowing_an_absent_record_is_a_no_op_success() {
        let store = FakeFollowsStore::empty();
        block_on(save_follow(
            &store,
            record("https://peer.example", 7, "site"),
        ))
        .expect("follow");
        let writes = store.writes();

        let after = block_on(save_unfollow(&store, "https://peer.example", 999))
            .expect("unfollow an absent record succeeds");
        assert_eq!(after.len(), 1, "the real follow is untouched");
        assert_eq!(store.writes(), writes, "nothing was written");
    }

    /// The door's refusal (no generation tip resolves yet) is the caller's
    /// error — a follow is never reported stored when it was not.
    #[test]
    fn a_door_refusal_surfaces_as_a_save_error() {
        let store = FakeFollowsStore::empty();
        store.refuse_next_writes(1);
        let err = block_on(save_follow(
            &store,
            record("https://peer.example", 7, "site"),
        ))
        .expect_err("refused");
        assert!(matches!(err, StoreError::Save(_)), "got {err:?}");
        assert!(store.current().followed.is_empty());
    }
}
