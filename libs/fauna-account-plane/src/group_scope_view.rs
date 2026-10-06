//! The paint-ready reading of one group scope's persisted machinery rows —
//! what an app's folders page lists once an offline share ceremony has
//! landed.
//!
//! Authority: `docs/goal/architecture/account-data-plane.md` § Implementation
//! status today (the group-scope paragraph: *"the listing read is
//! `AccountStore::group_scope_states`"*) and § The recipient-set scheme (the
//! roster this projects). The ceremony itself is owned by
//! `docs/goal/behavior/p2p.md` § Offline share initiation.
//!
//! # Why the STORE is the listing's source, never the ceremony record
//!
//! The ceremony record (`fauna.state.group-share-ceremony`) says what
//! *happened* — an offer arrived, a
//! deliver was admitted. The plane says what *landed*. Only the second can
//! list a set: a config record whose rows never reached the store names a
//! scope this device cannot read a byte of, and painting it would be exactly
//! the stub the standing tui-parity rule forbids. So an app asks the store
//! for the scope's rows and hands them here; no birth row, no listing.
//!
//! # No text here
//!
//! Like every shared projection, this decides *facts* (who minted it, who is
//! enrolled, when) and never sentences. The short id is not a translation —
//! it is a truncation of the scope id itself, the one string a user compares
//! against the other side's screen while the set is still nameless (v1 has no
//! naming kinds; they arrive with group content-kind sealing).

use fauna_account_store::types::StateEntry;
use fauna_core::group_scope::{
    GroupAuthority, GroupBirthRecord, RosterView, decode_birth_for_scope,
};
use fauna_core::identity::ActorId;
use fauna_protocol::group_state::{
    GROUP_BIRTH_KEY, KIND_GROUP_AUTHORITY_REVOCATION, KIND_GROUP_BIRTH, KIND_GROUP_ROSTER,
};

/// How many hex characters of a scope id a user is asked to compare. Eight is
/// the shipped short-id length everywhere else an id is shown to a human
/// (`fauna_core::identity`'s canonical short form) — one habit, not a second.
const SHORT_ID_CHARS: usize = 8;

/// One group scope, as a folders page paints it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupScopeSummary {
    /// The scope's content-derived id.
    pub scope_id: [u8; 32],
    /// Its first [`SHORT_ID_CHARS`] hex characters — the nameless set's
    /// only human handle in v1.
    pub short_id: String,
    /// The account that minted the scope (the birth record's authority).
    pub authority: ActorId,
    /// Every VERIFIED enrolled member, ascending by actor key so two devices
    /// reading the same rows paint the same order.
    pub members: Vec<ActorId>,
    /// When the scope was born, unix ms (advisory — the birth record's own
    /// assertion).
    pub created_at_ms: i64,
}

impl GroupScopeSummary {
    /// Is `actor` the account that minted this scope? The one bit a page
    /// needs to choose between "Shared · N" and "Shared by ‹them›".
    #[must_use]
    pub fn is_authority(&self, actor: &ActorId) -> bool {
        self.authority == *actor
    }

    /// The other members — everyone enrolled who is not `actor`.
    pub fn counterparts<'a>(&'a self, actor: &'a ActorId) -> impl Iterator<Item = &'a ActorId> {
        self.members.iter().filter(move |m| *m != actor)
    }
}

/// One group scope's authority line and verified roster, folded from this
/// replica's OWN persisted rows — the single fold every local reader of a
/// scope's machinery rows goes through: the folders page's listing
/// ([`summarize_group_scope`]) and the peer witness door's evaluator
/// ([`GroupRosterSnapshot`]). Two folds would be two answers to "who is
/// enrolled here", and the door must refuse exactly whom the page does not
/// list.
#[derive(Debug, Clone)]
pub struct GroupScopeLine {
    /// The scope's birth record (its authority actor and advisory stamp).
    pub birth: GroupBirthRecord,
    /// The authority line per this replica's own rows: the birth record's
    /// authority actor plus the authority devices its own merged
    /// `fauna.group.authority-revocation` rows revoke.
    pub authority: GroupAuthority,
    /// The verified roster over that line, `Removed` cells excluded.
    pub roster: RosterView,
}

/// Fold one scope's persisted rows into its [`GroupScopeLine`].
///
/// `None` means **this replica cannot say what the scope is**: no live birth
/// row reached the store, it does not decode, or it is not this scope's —
/// its content-derived id is another scope's — so there is no authority root
/// to verify anything against. Tombstoned rows are never read.
///
/// The re-derivation is what makes the birth row's authority *this* scope's:
/// a record filed under `scope_id` that hashes elsewhere names whatever
/// authority its writer chose, and every reader here — the page, the door,
/// the pump's authority legs — would answer to it.
#[must_use]
pub fn read_group_scope(scope_id: &[u8; 32], rows: &[StateEntry]) -> Option<GroupScopeLine> {
    let birth_row = rows
        .iter()
        .find(|r| r.kind == KIND_GROUP_BIRTH && r.key == GROUP_BIRTH_KEY && !r.tombstone)?;
    let birth = decode_birth_for_scope(&birth_row.value, scope_id).ok()?;

    let roster_rows: Vec<(&str, &[u8])> = rows
        .iter()
        .filter(|r| r.kind == KIND_GROUP_ROSTER && !r.tombstone)
        .map(|r| (r.key.as_str(), r.value.as_slice()))
        .collect();
    // An entry authored by an authority device this store has learned is
    // revoked enrols nobody — for the page that lists it and the door that
    // admits it alike.
    let revocation_rows: Vec<(&str, &[u8])> = rows
        .iter()
        .filter(|r| r.kind == KIND_GROUP_AUTHORITY_REVOCATION && !r.tombstone)
        .map(|r| (r.key.as_str(), r.value.as_slice()))
        .collect();
    let authority = GroupAuthority::build(
        scope_id,
        &birth.authority_actor,
        // Cross-account succession facts are not known to a local reader; a
        // succeeded member's entry re-verifies at the next sync, and until
        // then it neither lists nor admits — the monotone, fail-closed
        // direction RosterView documents for `prior`.
        &[],
        revocation_rows.iter().copied(),
    );
    let roster = RosterView::build(scope_id, &authority, roster_rows.iter().copied());
    Some(GroupScopeLine {
        birth,
        authority,
        roster,
    })
}

/// Project one scope's persisted rows into its summary.
///
/// `None` means **this scope is not listable on this device**: no birth row
/// reached the store, so nothing here knows what the scope is or who may read
/// it. That is the honest answer for a ceremony recorded but never adopted,
/// and it is why a caller must not synthesize a row from its config record.
///
/// Roster rows that fail their own verification (wrong scope, broken
/// authority chain, a key that is not the entry's content-derived id) are
/// dropped by [`RosterView`] rather than listed — an unverifiable cell is
/// never a member.
#[must_use]
pub fn summarize_group_scope(
    scope_id: &[u8; 32],
    rows: &[StateEntry],
) -> Option<GroupScopeSummary> {
    let line = read_group_scope(scope_id, rows)?;
    let mut members: Vec<ActorId> = line.roster.wrap_targets().map(|m| m.member_actor).collect();
    members.sort_by_key(|a| a.0);
    members.dedup();

    Some(GroupScopeSummary {
        scope_id: *scope_id,
        short_id: fauna_core::hex32::encode(scope_id)
            .chars()
            .take(SHORT_ID_CHARS)
            .collect(),
        authority: line.birth.authority_actor,
        members,
        created_at_ms: line.birth.created_at_ms,
    })
}

/// Every group scope this device holds, folded — the peer witness door's
/// evaluator state (`fauna_peer_share::admission::GroupRosterState`, whose
/// impl for this type lives with the share plane,
/// `fauna_sync_engine::group_roster_door`). A value, not a view: the pump builds a fresh
/// one per pass from the store and swaps it in, so the admit path reads
/// memory and never waits on I/O.
///
/// A scope is present iff this device holds its machinery root AND its birth
/// row landed; every other scope id answers "not held", the door's refusal.
#[derive(Debug, Clone, Default)]
pub struct GroupRosterSnapshot {
    scopes: std::collections::HashMap<[u8; 32], GroupScopeLine>,
}

impl GroupRosterSnapshot {
    /// Fold `(scope id, that scope's rows)` pairs. A scope whose rows carry no
    /// readable birth row is left out — not held, not vouched for.
    pub fn from_scope_rows<I>(scopes: I) -> Self
    where
        I: IntoIterator<Item = ([u8; 32], Vec<StateEntry>)>,
    {
        let scopes = scopes
            .into_iter()
            .filter_map(|(scope_id, rows)| {
                read_group_scope(&scope_id, &rows).map(|line| (scope_id, line))
            })
            .collect();
        Self { scopes }
    }

    /// Build the snapshot from this replica's store: every held scope
    /// ([`crate::group_state_plane::held_group_roots`], the one enumeration)
    /// with its persisted group-plane rows. All local reads.
    pub async fn load_held<B: fauna_account_store::backend::StoreBackend>(
        store: &fauna_account_store::store::AccountStore<B>,
    ) -> anyhow::Result<Self> {
        let held = store
            .states_of_kind(fauna_protocol::merge_policy::KIND_GROUP_MACHINERY_ROOT)
            .await?;
        let mut scopes = Vec::new();
        for (scope_id, _root) in crate::group_state_plane::held_group_roots(&held) {
            let scope = fauna_protocol::scope::GroupScope::new(scope_id).to_string();
            scopes.push((scope_id, store.group_scope_states(&scope).await?));
        }
        Ok(Self::from_scope_rows(scopes))
    }

    /// The scope's authority line per this replica's own rows; `None` for a
    /// scope it does not hold.
    #[must_use]
    pub fn authority(&self, scope_id: &[u8; 32]) -> Option<GroupAuthority> {
        self.scopes.get(scope_id).map(|line| line.authority.clone())
    }

    /// Does this replica's merged roster carry an honored `Removed` for
    /// `entry_id` (one authored under a device cert chaining to the authority
    /// root — the remover's own standing not consulted)?
    /// `true` for a scope it does not hold — "not held" refuses here exactly
    /// as it does at `authority`, so a reader that consults this alone (the
    /// serve re-consult) or on a later snapshot than its `authority` read
    /// (the door's two reads) never admits on it.
    #[must_use]
    pub fn is_entry_removed(&self, scope_id: &[u8; 32], entry_id: &[u8; 32]) -> bool {
        self.scopes
            .get(scope_id)
            .is_none_or(|line| line.roster.is_excluded_entry(entry_id))
    }

    /// How many scopes this snapshot holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.scopes.len()
    }

    /// Does it hold none?
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.scopes.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::crypto::GroupMachineryRoot;
    use fauna_core::group_scope::group_scope_id;
    use fauna_core::identity::ActorKeypair;

    fn entry(kind: &str, key: &str, value: Vec<u8>) -> StateEntry {
        StateEntry {
            kind: kind.into(),
            key: key.into(),
            scope: "group:test".into(),
            value,
            merge_meta: None,
            entry_version: 1,
            tombstone: false,
        }
    }

    fn birth_of(authority: &ActorKeypair) -> (GroupBirthRecord, [u8; 32]) {
        let record = GroupBirthRecord {
            authority_actor: authority.actor_id(),
            salt: [0x5A; 32],
            machinery_root_commit: GroupMachineryRoot::from_bytes([0xD7; 32]).commitment(),
            created_at_ms: 1_700_000_000_000,
        };
        let id = group_scope_id(&record).unwrap();
        (record, id)
    }

    #[test]
    fn a_scope_with_no_birth_row_is_not_listable() {
        let (_, scope_id) = birth_of(&ActorKeypair::from_secret([1u8; 32]));
        // Roster rows alone: the ceremony half-landed, and the device cannot
        // say what the scope is. Listing it would name a set it cannot read.
        let rows = vec![entry(KIND_GROUP_ROSTER, "aa", b"whatever".to_vec())];
        assert!(summarize_group_scope(&scope_id, &rows).is_none());
    }

    #[test]
    fn the_birth_row_alone_lists_the_scope_with_its_authority() {
        let authority = ActorKeypair::from_secret([2u8; 32]);
        let (birth, scope_id) = birth_of(&authority);
        let rows = vec![entry(
            KIND_GROUP_BIRTH,
            GROUP_BIRTH_KEY,
            fauna_core::encoding::canonical_encode(&birth)
                .unwrap()
                .to_vec(),
        )];

        let s = summarize_group_scope(&scope_id, &rows).expect("birth row lists the scope");
        assert_eq!(s.scope_id, scope_id);
        assert_eq!(s.authority, authority.actor_id());
        assert!(s.is_authority(&authority.actor_id()));
        assert_eq!(s.short_id.len(), SHORT_ID_CHARS);
        assert!(
            fauna_core::hex32::encode(&scope_id).starts_with(&s.short_id),
            "the short id is a truncation of the scope id, never a re-encoding"
        );
        // No verified roster rows — an empty membership, never a fabricated one.
        assert!(s.members.is_empty());
    }

    #[test]
    fn an_unverifiable_roster_row_is_never_a_member() {
        let authority = ActorKeypair::from_secret([3u8; 32]);
        let (birth, scope_id) = birth_of(&authority);
        let rows = vec![
            entry(
                KIND_GROUP_BIRTH,
                GROUP_BIRTH_KEY,
                fauna_core::encoding::canonical_encode(&birth)
                    .unwrap()
                    .to_vec(),
            ),
            // Garbage at a plausible key: the listing must drop it rather than
            // count a member nothing verified.
            entry(KIND_GROUP_ROSTER, &"ab".repeat(32), vec![0xFF; 40]),
        ];
        let s = summarize_group_scope(&scope_id, &rows).expect("listable");
        assert!(
            s.members.is_empty(),
            "a roster cell that does not verify is not a member"
        );
    }

    /// A birth row filed under a scope it does not hash to names no authority
    /// here: the same root commitment with another actor as authority is a
    /// different scope's record, and the scope is unlisted, not re-rooted.
    #[test]
    fn a_birth_row_that_is_another_scopes_stops_the_listing() {
        let (birth, scope_id) = birth_of(&ActorKeypair::from_secret([5u8; 32]));
        let forged = GroupBirthRecord {
            authority_actor: ActorKeypair::from_secret([6u8; 32]).actor_id(),
            ..birth
        };
        assert_ne!(group_scope_id(&forged).unwrap(), scope_id);
        let rows = vec![entry(
            KIND_GROUP_BIRTH,
            GROUP_BIRTH_KEY,
            fauna_core::encoding::canonical_encode(&forged)
                .unwrap()
                .to_vec(),
        )];
        assert!(read_group_scope(&scope_id, &rows).is_none());
        assert!(summarize_group_scope(&scope_id, &rows).is_none());
    }

    #[test]
    fn a_tombstoned_birth_row_stops_the_listing() {
        let authority = ActorKeypair::from_secret([4u8; 32]);
        let (birth, scope_id) = birth_of(&authority);
        let mut row = entry(
            KIND_GROUP_BIRTH,
            GROUP_BIRTH_KEY,
            fauna_core::encoding::canonical_encode(&birth)
                .unwrap()
                .to_vec(),
        );
        row.tombstone = true;
        assert!(summarize_group_scope(&scope_id, &[row]).is_none());
    }
}
