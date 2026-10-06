//! Shared **staged-removal lifecycle**: stage (upsert-by-identity), find, and
//! clear a sentinel in a `Vec<T>`.
//!
//! [`fauna_client_folders::custody`]'s `FolderPendingRemoval` (keyed by
//! `(channel_id, removed_member)`, upserted by fresh key) and
//! [`fauna_client_subscriptions::custody`]'s `PendingRemoval` (keyed by
//! `(tier_name, subscriber_id)`, same upsert-by-fresh-key rule) each
//! hand-copied the identical "retain-then-push" / "find-first-match" /
//! "retain, report whether anything dropped" shape over their own sentinel
//! type — their module docs already cross-reference each other as mirrors
//! (`custody.rs`: "exactly mirroring how `fauna-client-subscriptions` splits
//! its pure `custody` transitions..."). This owns the mechanical shape once;
//! the (id, key) comparison itself stays crate-local, passed in as a
//! predicate, so each caller's own identity/upsert rule is untouched.

/// Stage (or re-stage) `removal`: every existing entry `is_same_staging`
/// calls a match for is dropped before `removal` is pushed — an upsert on
/// whatever identity the caller's predicate encodes. A predicate scoped to
/// `(id, fresh key)` leaves a *different* fresh key for the same id alone
/// (a concurrent device's differently-keyed staging must survive).
pub fn stage<T>(items: &mut Vec<T>, removal: T, is_same_staging: impl Fn(&T) -> bool) {
    items.retain(|r| !is_same_staging(r));
    items.push(removal);
}

/// The first entry `matches` calls a hit for, if any.
pub fn find<T>(items: &[T], matches: impl Fn(&T) -> bool) -> Option<&T> {
    items.iter().find(|r| matches(r))
}

/// Drop every entry `matches` calls a hit for. Returns whether anything was
/// dropped (`false` means the caller's identity no longer matches anything
/// staged — a stale or duplicate clear).
pub fn clear<T>(items: &mut Vec<T>, matches: impl Fn(&T) -> bool) -> bool {
    let before = items.len();
    items.retain(|r| !matches(r));
    items.len() != before
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Entry {
        id: u8,
        key: u8,
    }

    fn same_staging(r: &Entry, id: u8, key: u8) -> bool {
        r.id == id && r.key == key
    }

    #[test]
    fn stage_upserts_by_id_and_key_leaves_other_keys_for_the_same_id() {
        let mut items = Vec::new();
        stage(&mut items, Entry { id: 1, key: 0xAA }, |r| {
            same_staging(r, 1, 0xAA)
        });
        assert_eq!(items, vec![Entry { id: 1, key: 0xAA }]);

        // Re-stage with the SAME (id, key): replaced in place.
        stage(&mut items, Entry { id: 1, key: 0xAA }, |r| {
            same_staging(r, 1, 0xAA)
        });
        assert_eq!(items.len(), 1);

        // A DIFFERENT key for the same id: both survive (concurrent staging).
        stage(&mut items, Entry { id: 1, key: 0xBB }, |r| {
            same_staging(r, 1, 0xBB)
        });
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn find_returns_first_match_none_otherwise() {
        let items = vec![Entry { id: 1, key: 0xAA }, Entry { id: 2, key: 0xBB }];
        assert_eq!(
            find(&items, |r| r.id == 2),
            Some(&Entry { id: 2, key: 0xBB })
        );
        assert_eq!(find(&items, |r| r.id == 9), None);
    }

    #[test]
    fn clear_reports_whether_anything_was_dropped() {
        let mut items = vec![Entry { id: 1, key: 0xAA }];
        assert!(
            !clear(&mut items, |r| same_staging(r, 1, 0xBB)),
            "wrong key is a no-op"
        );
        assert!(clear(&mut items, |r| same_staging(r, 1, 0xAA)));
        assert!(items.is_empty());
    }
}
