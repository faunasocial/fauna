//! The client-side **MLS join-filter** over member-visible folder rows — the
//! one filter every page that reads `fauna.folders.list` with
//! `include_shared_with_me` runs (`docs/goal/ui/folders.md` § Sharing; the Media
//! page's twin read, `docs/goal/ui/media.md` § Layout & flow → *Where the
//! offerable sets come from*).
//!
//! The nest returns *rostered* members — it cannot observe a client-side MLS
//! join — so a `role == "member"` row is shown **only if** the client has joined
//! the group, else a stranger's un-accepted knock would surface unbidden. A knock
//! surfaces only as a pending share, never as a row.
//!
//! Lives here, beside `FolderSummary`, because both the Folders page
//! (`fauna_devices_machine`) and the Media page (`fauna_media_machine`) read the
//! member projection and neither may own the other's copy: a page that forgot
//! the filter is exactly how the list-surface exposure arose. This crate holds no MLS
//! engine, so the derivation `ChannelId::from_group_id(mls_group_id)` +
//! `MlsEngine::has_group` lives in the client's [`MlsQuery`] impl. Each page
//! transcribes the row into its own type, so the filter takes the two facts it
//! decides on rather than a row.

/// The local MLS-membership oracle the join-filter consults — implemented by
/// each client over the engine it already holds and injected into the page
/// machines post-construction (never over UniFFI: a trait object has no FFI
/// ABI).
pub trait MlsQuery: Send + Sync {
    /// Whether the local MLS engine holds the group for the shared set with this raw
    /// hex `mls_group_id` — i.e. the client has joined it.
    fn is_joined_shared_set(&self, mls_group_id_hex: &str) -> bool;
}

/// Whether a row passes the join-filter: an owner (or unspecified-role) row
/// always does; a member row only when a wired `query` says its group is
/// joined. **Fail-safe:** no query wired, or a member row with no group id,
/// drops it.
pub fn admits(query: Option<&dyn MlsQuery>, member: bool, mls_group_id: Option<&str>) -> bool {
    if !member {
        return true;
    }
    match (query, mls_group_id) {
        (Some(q), Some(gid)) => q.is_joined_shared_set(gid),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Joined(&'static str);
    impl MlsQuery for Joined {
        fn is_joined_shared_set(&self, gid: &str) -> bool {
            gid == self.0
        }
    }

    #[test]
    fn owner_rows_pass_member_rows_only_when_joined_and_unwired_drops_them() {
        let q: &dyn MlsQuery = &Joined("aa");
        assert!(admits(None, false, None), "an own row always passes");
        assert!(admits(Some(q), true, Some("aa")));
        assert!(!admits(Some(q), true, Some("bb")), "not joined");
        assert!(!admits(Some(q), true, None), "a member row with no group");
        assert!(!admits(None, true, Some("aa")), "unwired is fail-safe");
    }
}
