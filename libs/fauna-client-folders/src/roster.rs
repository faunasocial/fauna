//! The owner-side "Shared with" roster derivation — the `role == "member"`
//! filter every app applies to an `actor_members_list` reply before
//! rendering the "Shared with" list and computing the `folder-shared-badge`
//! "Shared · N" count (`.len()` of the filtered slice). One derivation site
//! for both — `docs/goal/ui/folders.md` § Sharing a folder (cross-user).
//!
//! Ungated (no `mls`): the filter is pure data shaping over an already-fetched
//! roster, needed by every UI surface that renders `actor_members_list`.

use fauna_protocol::folders::FolderActorMember;

/// The `role == "member"` subset of a folder actor roster (the owner
/// excluded) — the owner-side "Shared with" list, and the source of the
/// `folder-shared-badge` count (`.len()` of the result). Order preserved.
pub fn member_actors(actors: &[FolderActorMember]) -> Vec<&FolderActorMember> {
    actors.iter().filter(|m| m.role == "member").collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actor(actor_id: &str, handle: &str, role: &str) -> FolderActorMember {
        FolderActorMember {
            actor_id: actor_id.to_string(),
            handle: handle.to_string(),
            role: role.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn filters_out_the_owner_row_keeping_member_order() {
        let roster = vec![
            actor("aa", "alice", "owner"),
            actor("bb", "bob", "member"),
            actor("cc", "carol", "member"),
        ];
        let members = member_actors(&roster);
        assert_eq!(
            members
                .iter()
                .map(|m| m.actor_id.as_str())
                .collect::<Vec<_>>(),
            vec!["bb", "cc"]
        );
    }

    #[test]
    fn an_owner_only_unshared_roster_yields_no_members() {
        let roster = vec![actor("aa", "alice", "owner")];
        assert!(member_actors(&roster).is_empty());
    }

    #[test]
    fn an_empty_roster_yields_an_empty_count() {
        assert!(member_actors(&[]).is_empty());
    }
}
