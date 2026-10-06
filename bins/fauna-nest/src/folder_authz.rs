//! Shared **read** authorization for folders — the S2-P3 group-membership gate
//! for cross-user shared folders.
//!
//! Authority: `docs/goal/architecture/key-material-hierarchy.md` § *Audience: an
//! MLS group at a specific epoch* (the gate "derives the ChannelId via
//! `ChannelId::from_group_id(mls_group_id)` and admits a member through the same
//! `actor_channels`-shaped roster check conv uses (`is_actor_in_channel`),
//! alongside owner-equality") + the shared-folders design (tracked
//! internally), Q5 (the gate is
//! **discovery + defense-in-depth**; sealing the chunk bytes stays the
//! confidentiality boundary).
//!
//! ## What this module gates
//!
//! A folder is **readable** by:
//! - its **owner** (`folders.actor_id`, retained as the owner per S2-P2 — *not*
//!   repointed to the group id), always; **and**
//! - for a **group-bound** shared set (`mls_group_id IS NOT NULL`, bound by
//!   `fauna.folders.share`): any actor on the derived
//!   `ChannelId::from_group_id(mls_group_id)` roster (`is_actor_in_channel`), or an
//!   admin (discovery metadata only — the chunks stay sealed under the group key).
//!
//! An **owner-only** set (`mls_group_id IS NULL`) is strictly owner-scoped, with
//! **no admin override** — the N1/ST-1 encryption-at-rest property (a nest admin
//! must not read a user's private backups).
//!
//! ## Reads vs. writes
//!
//! [`resolve_readable_folder`] is the **read** boundary. The **write** plane
//! (multi-writer Phase 1, `file-sync.md` § Multi-writer shared sets) widens at
//! exactly three kinds — `fauna.sync.changes.record`, the upload leases, and
//! `fauna.sync.conflicts.report` — through [`resolve_writable_folder`]: the
//! owner, OR a roster member the owner granted `access == 'writer'`
//! (`folder_member_access`; absent row = reader). There is **no admin
//! override on writes** (the admin read grant is discovery-metadata only).
//! Every other write surface (share / evict / rotate / supersede / config /
//! snapshot / serve / paywall) keeps strict owner-equality and is *not* routed
//! through this module. All resolvers fold *absent* and *exists-but-not-yours*
//! to `None` per ST-RES-1 (the caller maps it to `not_found` — no
//! folder-name existence oracle).

use crate::db::{CacheDb, FolderRow};
use fauna_mls::types::ChannelId;

/// **Why** a caller may read a set — the same three arms
/// [`can_read_folder`] already walks, kept instead of collapsed to a `bool`.
///
/// The distinction is not an authorization one (every variant reads; the gate is
/// unchanged) — it is an **audience** one. A set's user-chosen name seals to the
/// people who can open the set's chunks, so [`Owner`](Self::Owner) and
/// [`Member`](Self::Member) are that seal's audience and
/// [`AdminDiscovery`](Self::AdminDiscovery) is not. A read surface that ships a
/// sealed label needs to know which, because handing the non-audience reader the
/// label's **salt** hands them the name back offline: the salt is an unkeyed
/// digest of a user-chosen, dictionary-shaped string (
/// `file-sync.md` § Sealed names & paths).
///
/// Ordinary permission checks want the `bool` and should keep asking for it —
/// `can_read_folder(..).await?.is_some()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FolderReadGrant {
    /// `folders.actor_id` equals the caller. Reads bound or not.
    Owner,
    /// A current roster member of a group-bound set's derived channel. Holds the
    /// M2 content key, so this reader opens the set's bytes *and* its labels.
    Member,
    /// An admin who is neither owner nor member, reading a group-bound set under
    /// the Q5 **discovery-metadata** grant. Holds no key for this set — the
    /// chunks stay sealed under the group key, and so do the labels.
    AdminDiscovery,
    /// **The world**, reading a `audience='public'` folder over the public read
    /// plane (`federation.md` § The public folder read plane; phase 4 slice
    /// 4f-i). A public folder rests unsealed by the ratified exception, so this
    /// reader opens the bytes — but holds no *key*, and is nobody's roster
    /// member.
    ///
    /// ⚠ **[`can_read_folder`] never returns this variant, deliberately.** The
    /// public plane authorizes independently, in `folder_public`, on the
    /// inverse-shaped gate (*serve iff `audience == 'public'`*). An OR-ed public
    /// arm inside the membership resolver would flow into `authorize_snapshot`,
    /// the folder listing, and every future call site that asks "may this caller
    /// read?" — one bug there opens member data. The variant exists so a surface
    /// that must *name* the grant it served under can say so; it is never a way
    /// to acquire one.
    Public,
}

impl FolderReadGrant {
    /// Whether this reader is the audience a set's sealed labels are sealed to —
    /// i.e. whether they hold (or can resolve) a key that opens them.
    ///
    /// Drives the per-reader projection of `name_sealed` + its `name_hash` salt:
    /// a reader who cannot open the seal is sent neither half.
    pub(crate) fn is_label_audience(self) -> bool {
        // `Public` is deliberately NOT here: a public folder's labels rest
        // *plaintext*, so there is no seal for this reader to be the audience
        // of — the public projection ships the plaintext name and strips
        // `path_sealed` outright (`folders.md` § Publicly-synced follow, the
        // stripped-projection bullet). Answering `true` would hand a sealed
        // label's salt to the world on any folder that still carries one from
        // before its declassification.
        matches!(self, Self::Owner | Self::Member)
    }
}

/// `Some(grant)` iff `caller` may **read** an already-loaded `fs`, naming which
/// arm admitted them.
///
/// Owner-equality always; for a group-bound set additionally a current roster
/// member of the derived channel, or an admin. For an owner-only set, owner only
/// (no admin override). Callers that loaded the row by a global id
/// (`authorize_snapshot` via `get_folder_by_id`) use this directly.
///
/// The arms are walked in audience order — owner, then member, then admin — so a
/// caller who is *both* on the roster and an admin classifies as
/// [`FolderReadGrant::Member`], the stronger (key-holding) grant. Getting that
/// order backwards would withhold labels from a reader who can open them.
pub(crate) async fn can_read_folder(
    db: &CacheDb,
    fs: &FolderRow,
    caller: &[u8; 32],
) -> anyhow::Result<Option<FolderReadGrant>> {
    // The owner reads their own set, bound or not.
    if fs.actor_id.as_slice() == caller.as_slice() {
        return Ok(Some(FolderReadGrant::Owner));
    }
    // A group-bound shared set additionally admits roster members + admin. The
    // ChannelId is derived at the gate (it is NOT stored in actor_id — S2-P2).
    if let Some(group_id) = &fs.mls_group_id {
        let channel_id = ChannelId::from_group_id(group_id).0;
        if db.is_actor_in_channel(caller, &channel_id).await? {
            return Ok(Some(FolderReadGrant::Member));
        }
        if db.is_admin(caller).await? {
            return Ok(Some(FolderReadGrant::AdminDiscovery));
        }
    }
    Ok(None)
}

/// Resolve the folder named `name` that `caller` may **read** — their own set
/// (the `(name, actor_id)`-unique owner row), else a group-bound shared set of
/// that name whose derived channel has `caller` on its roster.
///
/// `None` ⇒ "no set of this name is readable by you", folding *absent* and
/// *exists-but-not-yours* together so the caller's `not_found` closes the
/// **ST-RES-1** name-existence oracle.
///
/// **Addressing note (Slice 2):** the owner row wins first, so if `caller` *owns*
/// a set named `name` they always resolve to their own — even if a same-named
/// shared set also exists. That is safe (no cross-user leak), but means a member
/// cannot reach a shared set by a name that collides with one of their own; the
/// robust fix (addressing shared reads by the global `folder_id`) is a future
/// refinement and is unnecessary for the gate's security property (the membership
/// check is the boundary regardless of how the row is addressed).
///
/// **`name_hash` (S5b, `file-sync.md` § Sealed names & paths):** when present,
/// resolves hash-first on BOTH arms (the owner row via
/// `get_folder_for_actor_by_name_hash`, the group-bound candidates via
/// `get_group_bound_folders_by_name_hash`) — the address that survives once
/// the nest's plaintext `name` column scrubs post-flip. Callers validate the
/// hash is exactly 32 bytes before calling in (this module stays free of
/// `RpcError`); a malformed hash is the caller's problem, never silently
/// downgraded to the name arm here.
pub(crate) async fn resolve_readable_folder(
    db: &CacheDb,
    name: &str,
    name_hash: Option<&[u8; 32]>,
    caller: &[u8; 32],
) -> anyhow::Result<Option<FolderRow>> {
    // Owner fast-path: the `(name, actor_id)`-scoped unique row.
    let owned = match name_hash {
        Some(h) => db.get_folder_for_actor_by_name_hash(h, caller).await?,
        None => db.get_folder_for_actor(name, caller).await?,
    };
    if let Some(fs) = owned {
        return Ok(Some(fs));
    }
    // Member path: any group-bound set of this name whose derived channel admits
    // the caller. (Owner-only rows are excluded by the query.)
    let candidates = match name_hash {
        Some(h) => db.get_group_bound_folders_by_name_hash(h).await?,
        None => db.get_group_bound_folders_by_name(name).await?,
    };
    for fs in candidates {
        if let Some(group_id) = &fs.mls_group_id {
            let channel_id = ChannelId::from_group_id(group_id).0;
            if db.is_actor_in_channel(caller, &channel_id).await? {
                return Ok(Some(fs));
            }
        }
    }
    Ok(None)
}

/// Resolve the hinted folder for a caller named by a **federated byte-plane
/// token** — a member whose account lives on another nest (`file-sync.md`
/// § Relay serving → *A member on another nest*, step (5); `federation.md`
/// § Cross-nest shared folders + channel append → *Relay serving across
/// nests*).
///
/// The one roster read is `channel_foreign_members`, at every call: the row
/// this nest wrote at Welcome-relay time and deletes at the member's removal,
/// so a still-live token reads nothing once the member is gone. **Neither arm
/// of [`resolve_readable_folder`] is consulted** — no owner row, no
/// `actor_channels` — because the token's purpose says which roster names its
/// actor, and a federated token naming a same-nest account is not that
/// account's session. Reader access is enough; the `writer` grant is not read.
///
/// A candidate must also be the folder its channel's claimant owns — the row
/// the federated read plane serves for that channel (`claimed_folder_for_channel`)
/// — so a folder bound to a channel somebody else claimed is never reached
/// through that channel's cross-nest roster.
///
/// Same addressing as the same-nest resolver: `name_hash` wins when present,
/// and `None` folds *absent* and *not yours* together (ST-RES-1).
pub(crate) async fn resolve_foreign_readable_folder(
    db: &CacheDb,
    name: &str,
    name_hash: Option<&[u8; 32]>,
    caller: &[u8; 32],
) -> anyhow::Result<Option<FolderRow>> {
    let candidates = match name_hash {
        Some(h) => db.get_group_bound_folders_by_name_hash(h).await?,
        None => db.get_group_bound_folders_by_name(name).await?,
    };
    for fs in candidates {
        let Some(group_id) = &fs.mls_group_id else {
            continue;
        };
        let channel_id = ChannelId::from_group_id(group_id).0;
        if db
            .foreign_member_home_nest(&channel_id, caller)
            .await?
            .is_none()
        {
            continue;
        }
        if db
            .folder_channel_claimed_by(&channel_id)
            .await?
            .is_some_and(|claimant| claimant.as_slice() == fs.actor_id.as_slice())
        {
            return Ok(Some(fs));
        }
    }
    Ok(None)
}

/// Resolve the folder named `name` that `caller` may **write** — their own
/// set (any mode, exactly the old `owned_folder` behavior), else a
/// group-bound shared set of that name where `caller` is a current roster
/// member **and** holds an explicit `writer` grant on the derived channel
/// (`folder_member_access`; absent row = reader ⇒ `None`).
///
/// The write-plane twin of [`resolve_readable_folder`], with the same
/// ST-RES-1 `None` fold and the same owner-row-wins addressing note. Gates
/// exactly `fauna.sync.changes.record`, `fauna.folders.lease.{acquire,
/// release}`, and `fauna.sync.conflicts.report` (`file-sync.md` § Multi-writer
/// shared sets); no admin arm by design.
///
/// `name_hash`: see [`resolve_readable_folder`]'s doc — same hash-first
/// contract on both the owner and group-bound arms.
pub(crate) async fn resolve_writable_folder(
    db: &CacheDb,
    name: &str,
    name_hash: Option<&[u8; 32]>,
    caller: &[u8; 32],
) -> anyhow::Result<Option<FolderRow>> {
    // Owner fast-path: the `(name, actor_id)`-scoped unique row.
    let owned = match name_hash {
        Some(h) => db.get_folder_for_actor_by_name_hash(h, caller).await?,
        None => db.get_folder_for_actor(name, caller).await?,
    };
    if let Some(fs) = owned {
        return Ok(Some(fs));
    }
    // Writer-member path: a group-bound set of this name whose derived channel
    // has `caller` on the roster with an explicit `writer` role row. Both checks
    // are required — the roster admits readers too, and a stale role row without
    // roster membership (should not exist; evict deletes it) must not admit.
    let candidates = match name_hash {
        Some(h) => db.get_group_bound_folders_by_name_hash(h).await?,
        None => db.get_group_bound_folders_by_name(name).await?,
    };
    for fs in candidates {
        if let Some(group_id) = &fs.mls_group_id {
            let channel_id = ChannelId::from_group_id(group_id).0;
            if db.is_actor_in_channel(caller, &channel_id).await?
                && db
                    .get_folder_member_role(&channel_id, caller)
                    .await?
                    .is_some_and(|role| role.access == "writer")
            {
                return Ok(Some(fs));
            }
        }
    }
    Ok(None)
}

/// Enumerate **every** folder `caller` may read — their own sets (owner rows,
/// bound or not) plus any group-bound shared set whose derived channel admits
/// `caller` as a roster member (or admin, the discovery-metadata grant
/// [`can_read_folder`] already allows). Deduplicated by row id; reserved
/// `__*` sets are excluded (owner-infra, never browsable media).
///
/// This is the cross-set read surface backing `fauna.media.list`'s all-media
/// view. It composes [`CacheDb::get_folders_for_actor_full`] (owned) with a
/// [`can_read_folder`] filter over [`CacheDb::get_group_bound_folders`]
/// (member, plus the admin discovery-metadata grant noted above). For a
/// non-admin **User** this is the same S2-P3 membership boundary
/// [`resolve_readable_folder`] applies per name — the aggregate leaks no set
/// the per-name reads wouldn't. For a nest **admin** it is intentionally broader:
/// [`can_read_folder`]'s `is_admin` branch admits *every* group-bound set's
/// discovery metadata, which the admin-less per-name [`resolve_readable_folder`]
/// does not (spec Q5 — admin may see a shared set's discovery
/// metadata, bytes stay group-key-sealed in both paths).
///
/// Each row carries **why** it is readable ([`FolderReadGrant`]) so a caller
/// shipping sealed labels can project them per reader without re-running the
/// gate — the aggregate is the one surface where a single reply mixes rows the
/// caller is the label audience for with rows they are not.
pub(crate) async fn enumerate_readable_folders(
    db: &CacheDb,
    caller: &[u8; 32],
) -> anyhow::Result<Vec<(FolderRow, FolderReadGrant)>> {
    use std::collections::HashSet;

    let mut out: Vec<(FolderRow, FolderReadGrant)> = Vec::new();
    let mut seen: HashSet<i64> = HashSet::new();

    // Owned sets (includes the caller's own group-bound shares).
    for fs in db.get_folders_for_actor_full(caller).await? {
        if crate::db::snapshots::is_reserved_folder_name(&fs.name) {
            continue;
        }
        if seen.insert(fs.id) {
            out.push((fs, FolderReadGrant::Owner));
        }
    }
    // Group-bound sets the caller may read but does NOT own (member / admin).
    for fs in db.get_group_bound_folders().await? {
        if fs.actor_id.as_slice() == caller.as_slice() || seen.contains(&fs.id) {
            continue; // already covered by the owned pass
        }
        if crate::db::snapshots::is_reserved_folder_name(&fs.name) {
            continue;
        }
        if let Some(grant) = can_read_folder(db, &fs, caller).await? {
            seen.insert(fs.id);
            out.push((fs, grant));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bind `name` (owned by `owner`) to `group_id` and register the listed
    /// members on the derived roster — the post-`fauna.folders.share` +
    /// `welcome.deliver` state S2-P3 gates against.
    async fn bind_and_add_members(
        db: &CacheDb,
        name: &str,
        owner: &[u8; 32],
        group_id: &[u8],
        members: &[[u8; 32]],
    ) {
        db.create_folder(name, owner).await.unwrap();
        db.set_folder_mls_group(name, owner, Some(group_id))
            .await
            .unwrap();
        let channel_id = ChannelId::from_group_id(group_id).0;
        db.register_actor_channel(owner, &channel_id).await.unwrap();
        for m in members {
            db.register_actor_channel(m, &channel_id).await.unwrap();
        }
    }

    /// The owner of an **owner-only** set reads it; nobody else does — not even an
    /// admin (the N1/ST-1 encryption-at-rest property).
    #[tokio::test]
    async fn owner_only_set_is_owner_scoped_no_admin_override() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0xa1u8; 32];
        let other = [0xbbu8; 32];
        let admin = [0xadu8; 32];
        db.create_folder("private", &owner).await.unwrap();
        db.add_admin_actor(&admin).await.unwrap();
        let fs = db
            .get_folder_for_actor("private", &owner)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(
            can_read_folder(&db, &fs, &owner).await.unwrap(),
            Some(FolderReadGrant::Owner)
        );
        assert_eq!(can_read_folder(&db, &fs, &other).await.unwrap(), None);
        assert_eq!(
            can_read_folder(&db, &fs, &admin).await.unwrap(),
            None,
            "an admin must NOT read an owner-only (non-shared) set"
        );
    }

    /// A **group-bound** set admits the owner, every roster member, and an admin;
    /// a non-member outsider is rejected.
    #[tokio::test]
    async fn group_bound_set_admits_owner_member_admin_rejects_outsider() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0xa1u8; 32];
        let member = [0xb2u8; 32];
        let outsider = [0xeeu8; 32];
        let admin = [0xadu8; 32];
        let group_id = vec![0x7cu8; 20]; // variable-length raw group id (not 32)
        bind_and_add_members(&db, "shared", &owner, &group_id, &[member]).await;
        db.add_admin_actor(&admin).await.unwrap();
        let fs = db
            .get_folder_for_actor("shared", &owner)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(
            can_read_folder(&db, &fs, &owner).await.unwrap(),
            Some(FolderReadGrant::Owner),
            "owner"
        );
        assert_eq!(
            can_read_folder(&db, &fs, &member).await.unwrap(),
            Some(FolderReadGrant::Member),
            "roster member"
        );
        assert_eq!(
            can_read_folder(&db, &fs, &admin).await.unwrap(),
            Some(FolderReadGrant::AdminDiscovery),
            "admin (discovery metadata only — bytes stay sealed)"
        );
        assert_eq!(
            can_read_folder(&db, &fs, &outsider).await.unwrap(),
            None,
            "non-member outsider rejected"
        );
    }

    /// The audience split the per-reader label projection rides on: owner and
    /// roster member hold a key that opens the set's labels, the Q5 admin does
    /// not. Pinned as a property of the enum rather than left to each read
    /// surface to re-decide — a surface that got it wrong would hand the
    /// non-audience reader the unkeyed `name_hash` salt, and with it the name
    /// back by dictionary.
    #[test]
    fn only_the_key_holding_grants_are_the_label_audience() {
        assert!(FolderReadGrant::Owner.is_label_audience());
        assert!(FolderReadGrant::Member.is_label_audience());
        assert!(!FolderReadGrant::AdminDiscovery.is_label_audience());
    }

    /// A caller who is **both** on the roster and a nest admin classifies as the
    /// stronger, key-holding grant. Walking the arms in the other order would
    /// withhold a set's labels from a reader who can actually open them — a
    /// silent degrade, since an unrenderable label omits rather than errors.
    #[tokio::test]
    async fn an_admin_who_is_also_a_roster_member_classifies_as_member() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0xa1u8; 32];
        let admin_member = [0xadu8; 32];
        let group_id = vec![0x7cu8; 20];
        bind_and_add_members(&db, "shared", &owner, &group_id, &[admin_member]).await;
        db.add_admin_actor(&admin_member).await.unwrap();
        let fs = db
            .get_folder_for_actor("shared", &owner)
            .await
            .unwrap()
            .unwrap();

        let grant = can_read_folder(&db, &fs, &admin_member).await.unwrap();
        assert_eq!(grant, Some(FolderReadGrant::Member));
        assert!(grant.unwrap().is_label_audience());
    }

    /// The name resolver: the owner resolves their own set; a member resolves the
    /// shared set by name; an outsider gets `None` (oracle closed).
    #[tokio::test]
    async fn resolver_owner_and_member_resolve_outsider_none() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0xa1u8; 32];
        let member = [0xb2u8; 32];
        let outsider = [0xeeu8; 32];
        let group_id = vec![0x33u8; 32];
        bind_and_add_members(&db, "shared", &owner, &group_id, &[member]).await;

        let owner_row = resolve_readable_folder(&db, "shared", None, &owner)
            .await
            .unwrap();
        assert!(owner_row.is_some(), "owner resolves their set");

        let member_row = resolve_readable_folder(&db, "shared", None, &member)
            .await
            .unwrap();
        assert_eq!(
            member_row.map(|r| r.actor_id),
            Some(owner.to_vec()),
            "member resolves the OWNER's shared row (actor_id stays the owner)"
        );

        assert!(
            resolve_readable_folder(&db, "shared", None, &outsider)
                .await
                .unwrap()
                .is_none(),
            "outsider resolves None (ST-RES-1 oracle closed)"
        );
    }

    /// Phase 1 write gate: the owner always resolves writable; a member with an
    /// explicit `writer` grant resolves; a plain member (absent role row =
    /// reader), a member explicitly granted `reader`, an outsider, and an
    /// admin all get `None` (writes carry no admin override — the admin's
    /// read grant is discovery-metadata only).
    #[tokio::test]
    async fn writable_resolver_admits_owner_and_writer_only() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0xa1u8; 32];
        let writer = [0xb2u8; 32];
        let default_member = [0xc3u8; 32];
        let reader = [0xd4u8; 32];
        let outsider = [0xeeu8; 32];
        let admin = [0xadu8; 32];
        let group_id = vec![0x55u8; 32];
        bind_and_add_members(
            &db,
            "shared",
            &owner,
            &group_id,
            &[writer, default_member, reader],
        )
        .await;
        db.add_admin_actor(&admin).await.unwrap();
        let channel_id = ChannelId::from_group_id(&group_id).0;
        db.set_folder_member_access(&channel_id, &writer, "writer", None)
            .await
            .unwrap();
        db.set_folder_member_access(&channel_id, &reader, "reader", None)
            .await
            .unwrap();

        let owner_row = resolve_writable_folder(&db, "shared", None, &owner)
            .await
            .unwrap();
        assert!(owner_row.is_some(), "owner always writable");

        let writer_row = resolve_writable_folder(&db, "shared", None, &writer)
            .await
            .unwrap();
        assert_eq!(
            writer_row.map(|r| r.actor_id),
            Some(owner.to_vec()),
            "writer member resolves the OWNER's row (actor_id stays the owner)"
        );

        for (who, label) in [
            (&default_member, "absent-role member (default reader)"),
            (&reader, "explicit reader member"),
            (&outsider, "outsider"),
            (&admin, "admin (no write override)"),
        ] {
            assert!(
                resolve_writable_folder(&db, "shared", None, who)
                    .await
                    .unwrap()
                    .is_none(),
                "{label} must NOT resolve writable"
            );
        }
    }

    /// A writer whose role is edited back to `reader` (no rotation — ratified)
    /// immediately loses the write gate; the read gate is untouched.
    #[tokio::test]
    async fn writable_resolver_follows_role_edit() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0xa1u8; 32];
        let member = [0xb2u8; 32];
        let group_id = vec![0x66u8; 32];
        bind_and_add_members(&db, "shared", &owner, &group_id, &[member]).await;
        let channel_id = ChannelId::from_group_id(&group_id).0;

        db.set_folder_member_access(&channel_id, &member, "writer", Some(1024))
            .await
            .unwrap();
        assert!(
            resolve_writable_folder(&db, "shared", None, &member)
                .await
                .unwrap()
                .is_some(),
            "writer grant admits"
        );

        db.set_folder_member_access(&channel_id, &member, "reader", None)
            .await
            .unwrap();
        assert!(
            resolve_writable_folder(&db, "shared", None, &member)
                .await
                .unwrap()
                .is_none(),
            "writer→reader edit revokes the write gate"
        );
        assert!(
            resolve_readable_folder(&db, "shared", None, &member)
                .await
                .unwrap()
                .is_some(),
            "…while the read gate is untouched (no rotation on role edits)"
        );
    }

    /// Name-collision: a caller who *owns* a set named `name` resolves to their
    /// own row even when a same-named shared set also exists (safe; no leak).
    #[tokio::test]
    async fn resolver_owner_collision_prefers_own_row() {
        let db = CacheDb::open_in_memory().unwrap();
        let a = [0xa1u8; 32]; // owns the shared "docs"
        let b = [0xb2u8; 32]; // member of A's "docs", but ALSO owns their own "docs"
        let group_id = vec![0x44u8; 32];
        bind_and_add_members(&db, "docs", &a, &group_id, &[b]).await;
        // B owns their own (owner-only) "docs".
        db.create_folder("docs", &b).await.unwrap();

        let row = resolve_readable_folder(&db, "docs", None, &b)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            row.actor_id,
            b.to_vec(),
            "B resolves their OWN docs, not A's shared docs (owner row wins)"
        );
        assert!(
            row.mls_group_id.is_none(),
            "and B's own row is the unbound owner-only one"
        );
    }

    /// the **hash arm** of the read resolver carries the same owner
    /// scope and roster gate the name arm does. Until this test the hash arm was
    /// entered by no test in the tree — sound by inspection, unguarded against a
    /// future edit — while every cross-actor negative passed `name_hash: None`.
    ///
    /// Both hash queries are exercised: the owner fast-path
    /// (`get_folder_for_actor_by_name_hash`, actor-scoped) on the owner-only
    /// set, and the deliberately **globally**-scoped candidate query
    /// (`get_group_bound_folders_by_name_hash`, which returns every actor's
    /// matching row) on the shared one — the roster check is that query's only
    /// boundary, so the outsider negative below is what holds it.
    #[tokio::test]
    async fn readable_resolver_hash_arm_is_owner_scoped_and_roster_gated() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0xa1u8; 32];
        let member = [0xb2u8; 32];
        let outsider = [0xeeu8; 32];
        let group_id = vec![0x77u8; 32];
        bind_and_add_members(&db, "shared", &owner, &group_id, &[member]).await;
        db.create_folder("private", &owner).await.unwrap();

        let shared_hash = fauna_core::path_crypto::set_name_hash("shared");
        let private_hash = fauna_core::path_crypto::set_name_hash("private");

        // Positive controls — without these the negatives below pass vacuously
        // for a hash arm that resolves nothing at all. The plaintext name is
        // deliberately `""`: post-flip that is what a client sends.
        assert!(
            resolve_readable_folder(&db, "", Some(&private_hash), &owner)
                .await
                .unwrap()
                .is_some(),
            "owner resolves their owner-only set by hash alone"
        );
        assert_eq!(
            resolve_readable_folder(&db, "", Some(&shared_hash), &member)
                .await
                .unwrap()
                .map(|r| r.actor_id),
            Some(owner.to_vec()),
            "roster member resolves the OWNER's shared row by hash alone"
        );

        // The negatives: another actor's hash is not an address they may use.
        assert!(
            resolve_readable_folder(&db, "", Some(&private_hash), &outsider)
                .await
                .unwrap()
                .is_none(),
            "B supplying A's owner-only name_hash resolves None (owner scope holds on the hash arm)"
        );
        assert!(
            resolve_readable_folder(&db, "", Some(&shared_hash), &outsider)
                .await
                .unwrap()
                .is_none(),
            "a non-member supplying a shared set's name_hash resolves None \
             (the global candidate query's roster check is the boundary)"
        );
    }

    /// write plane: the hash arm of [`resolve_writable_folder`] keeps
    /// the owner scope, the roster gate **and** the explicit `writer` role — a
    /// hash is an address, never a grant.
    #[tokio::test]
    async fn writable_resolver_hash_arm_keeps_owner_roster_and_writer_gates() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0xa1u8; 32];
        let writer = [0xb2u8; 32];
        let reader = [0xd4u8; 32];
        let outsider = [0xeeu8; 32];
        let group_id = vec![0x88u8; 32];
        bind_and_add_members(&db, "shared", &owner, &group_id, &[writer, reader]).await;
        let channel_id = ChannelId::from_group_id(&group_id).0;
        db.set_folder_member_access(&channel_id, &writer, "writer", None)
            .await
            .unwrap();

        let shared_hash = fauna_core::path_crypto::set_name_hash("shared");

        // Positive controls (see the read-plane twin for why they are here).
        assert!(
            resolve_writable_folder(&db, "", Some(&shared_hash), &owner)
                .await
                .unwrap()
                .is_some(),
            "owner writes their set addressed by hash alone"
        );
        assert!(
            resolve_writable_folder(&db, "", Some(&shared_hash), &writer)
                .await
                .unwrap()
                .is_some(),
            "writer-granted member writes it addressed by hash alone"
        );

        for (who, label) in [
            (&reader, "reader-role member"),
            (&outsider, "non-member outsider"),
        ] {
            assert!(
                resolve_writable_folder(&db, "", Some(&shared_hash), who)
                    .await
                    .unwrap()
                    .is_none(),
                "{label} must NOT resolve writable via the hash arm"
            );
        }
    }
}
