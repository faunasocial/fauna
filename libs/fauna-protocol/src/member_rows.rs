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

use crate::folders::FolderSummary;
use fauna_core::identity::ActorId;

/// The local MLS-membership oracle the join-filter consults — implemented by
/// each client over the engine it already holds and injected into the page
/// machines post-construction (never over UniFFI: a trait object has no FFI
/// ABI). It is also the host's [`FolderChannelOwners`]: the one object a page
/// is handed for member rows answers both whether the client joined the group
/// and whom the channel's MLS record names as the folder's owner, so no app
/// wires the join-filter without the declassification anchor.
pub trait MlsQuery: FolderChannelOwners + Send + Sync {
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

/// Where a host keeps the **MLS-recorded folder owner** of a channel — the
/// `folder_channel_owner` marker `fauna-mls` stamps at folder-group mint and
/// folder-Welcome join and re-points on the owner's verified succession
/// (`federation-shared-folders.md` § Cross-nest shared folders + channel append). Abstract
/// here because this crate holds no MLS engine: an MLS-holding host wraps its
/// engine, a bearer-only host has none.
pub trait FolderChannelOwners: Sync {
    /// The recorded owner of the channel with this id, `None` when unstamped.
    fn folder_channel_owner(&self, channel_id: &[u8; 32]) -> Option<ActorId>;
}

/// The identity a seat verifies the owner-attested declassification against —
/// [`FolderSummary::judge_declassification`]'s `trusted_owner`, resolved from
/// sources **the nest cannot forge** (`encryption-at-rest.md` § Readable
/// classes → *The declassification is owner-ATTESTED*). One resolver for every
/// reader, so the live seat resolver, the engine binding decision, the pushed
/// agent capability and Media's seam cannot disagree about whose signature
/// arms a folder. It lives here, beside `FolderSummary`, because Media's seam
/// cannot depend on `fauna-client-folders` (that crate's `mls` feature depends
/// on the media machine), which re-exports it.
pub struct DeclassificationAnchor<'a> {
    /// This seat's own actor id — the trusted owner of every folder its
    /// account owns (an app derives it from the identity seed; the bearer-only
    /// agent holds it in its pushed capability).
    pub own: ActorId,
    /// The host's MLS state, for a **member** row: the trusted owner is the
    /// folder channel's recorded owner — never the `owner_actor_id` /
    /// `owner_handle` the nest fills on the row. `None` on a host that holds
    /// no MLS state (the sync agent's bearer-only engines, the File Provider
    /// helper, a Media page whose MLS query is not wired yet): its member
    /// seats have no anchor and **seal**.
    pub channel_owners: Option<&'a dyn FolderChannelOwners>,
}

impl<'a> DeclassificationAnchor<'a> {
    /// A host with no MLS state: own folders verify, member seats seal.
    #[must_use]
    pub fn own_only(own: ActorId) -> Self {
        Self {
            own,
            channel_owners: None,
        }
    }

    /// The trusted owner for `summary`: the seat's own actor id for a row its
    /// account owns (any `role` but `member` — an owner row, or a row with no
    /// role, which reads as owner), the channel's MLS-recorded owner for a
    /// member row. `None` seals: a member row on an MLS-less host, a member
    /// row whose channel this host never stamped, or a member row whose group
    /// id does not resolve to a channel at all.
    #[must_use]
    pub fn trusted_owner_for(&self, summary: &FolderSummary) -> Option<ActorId> {
        if summary.role.as_deref() != Some("member") {
            return Some(self.own);
        }
        let group = hex::decode(summary.mls_group_id.as_deref()?).ok()?;
        let channel = fauna_core::folder_keys::channel_id_for_group(&group);
        self.channel_owners?.folder_channel_owner(&channel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Joined(&'static str);
    impl FolderChannelOwners for Joined {
        fn folder_channel_owner(&self, _: &[u8; 32]) -> Option<ActorId> {
            None
        }
    }
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
