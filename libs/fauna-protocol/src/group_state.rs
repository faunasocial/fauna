//! The storage-group plane's class-2 kind registry — the group siblings of
//! [`crate::merge_policy`]'s account-state `POLICIES` table (T20,
//! `account-data-plane.md` § The audience ladder → *The recipient-set
//! scheme*; sealing split: `key-material-hierarchy.md` § Audience: a storage
//! group).
//!
//! **Why a sibling registry and not rows in `POLICIES` (the 1b home
//! decision):** the account table's other two frozen columns are
//! account-plane concepts with no meaning here. [`crate::merge_policy`]'s
//! `AudienceRung` selects between the delegable and fleet-only branches of
//! the *account-state key schedule* — but a group kind's audience IS the
//! scope's membership (admission hands the machinery root; no grant
//! machinery reaches this plane), and `home_scope_for_kind` routes into the
//! account's `state`/`state-fleet` sub-scopes — but every group kind's home
//! is its own `group:<scope-id-hex>` scope. Registering group kinds there
//! would also grow the account plane's trial-open set with kinds that can
//! never appear in it. So the group plane keeps its own two frozen columns —
//! merge policy and [`GroupSealing`] — and the shared mechanics live where
//! they always did: [`crate::merge_policy::apply_class2`] carries the group
//! kinds' merge arms (one dispatcher, one `EntryPlaintext` form), and the
//! joins live in `fauna_core`.
//!
//! Both columns freeze with the kind string, for the account table's
//! reasons: a row here is a claim that `apply_class2` knows how to merge the
//! kind's bytes, and the sealing column names which key material every
//! resting entry of the kind is under.

use crate::merge_policy::MergePolicy;

/// Class-2 kind: the scope's birth record
/// (`fauna_core::group_scope::GroupBirthRecord`) at the fixed
/// [`GROUP_BIRTH_KEY`] cell — the record the content-derived scope id
/// commits to, resting in-plane so every member (and every joiner's
/// commitment check) reads the same bytes. Deterministic given the scope id,
/// so first-wins is also only-wins. **Frozen.**
pub const KIND_GROUP_BIRTH: &str = "fauna.group.birth";

/// The logical key of a scope's single [`KIND_GROUP_BIRTH`] row.
pub const GROUP_BIRTH_KEY: &str = "self";

/// Class-2 kind: one roster entry per content-derived entry id — the
/// per-member monotone lattice with `Removed` absorbing
/// (`fauna_core::group_scope::GroupRosterRecord` owns the join + its laws;
/// re-admission is a fresh cell, so add-wins resurrection is
/// unrepresentable). **Frozen.**
pub const KIND_GROUP_ROSTER: &str = "fauna.group.roster";

/// Class-2 kind: one immutable-by-lattice entry per group generation,
/// logical key = the content-derived group generation id; the shred marker
/// is the in-value absorbing `Shredded` state
/// (`fauna_core::group_generation::GroupGenerationMintRecord`). **Frozen.**
pub const KIND_GROUP_GENERATION_MINT: &str = "fauna.group.generation-mint";

/// Class-2 kind: the member-authored top-up wrap vehicle, logical key =
/// `"<generation-id-hex>/<target-entry-id-hex>/<healer-actor-hex>"` — the
/// per-healer hardened cell shape from birth, actor-keyed (this plane never
/// had a legacy two-segment cell; rows rank
/// (verifies-under-the-cell's-healer, signed stamp, bytes) —
/// `fauna_core::group_generation::GroupTopupRecord` owns the record and the
/// join). **Frozen.**
pub const KIND_GROUP_GENERATION_WRAP: &str = "fauna.group.generation-wrap";

/// Class-2 kind: the member-authored "cannot key generation G" signal,
/// logical key = `"<generation-id-hex>/<target-actor-hex>"`, one cell per
/// (generation, member actor), authored ONLY by that member — rows verify
/// under the cell's own actor key
/// (`fauna_core::group_generation::GroupUnkeyableRecord`). **Frozen.**
pub const KIND_GROUP_GENERATION_UNKEYABLE: &str = "fauna.group.generation-unkeyable";

/// Class-2 kind: an authority device's shred attestation (ruled 2026-10-13 — `recipient-set-scheme.md` § The recipient-set scheme →
/// *Severance, per axis*, the shred clause), logical key =
/// `"<generation-id-hex>/<shredder-device-id-hex>"`, one cell per
/// (generation, shredder), authored ONLY by the shredder — rows verify under
/// the cell's own device key and carry its cert, which a reader checks
/// against the authority (`fauna_core::group_generation::GroupShredAttestation`).
/// The one row a key drop counts; the mint row's `Shredded` is attribution
/// only. **Frozen.**
pub const KIND_GROUP_GENERATION_SHRED: &str = "fauna.group.generation-shred";

/// Class-2 kind: an authority-device revocation, logical key =
/// `"<revoked-device-hex>/<revoker-hex>"` — one cell per (revoked device,
/// revoker), each revoker owning its own cell. How a group-plane reader
/// learns that a device was removed from the AUTHORITY's own account, whose
/// account plane it can never read; authored (root- or chain-signed), never
/// unconditional, because the machinery root outlives membership
/// (`fauna_core::group_scope::AuthorityRevocationRecord` owns the record, the
/// join and the ruling). **Frozen.**
pub const KIND_GROUP_AUTHORITY_REVOCATION: &str = "fauna.group.authority-revocation";

/// Class-2 kind: a **group set's path state** — one cell per path, logical
/// key = the set's keyed path hash hex, value
/// `fauna_core::group_file::GroupFileRecord` (sealed path, the manifest or
/// the delete, the content generation id, and the author device's signed
/// carriage — `p2p-shared-set-build.md` § Build design — the group set,
/// decision 2). A delete is a value phase, never a tombstone. **Frozen.**
pub const KIND_GROUP_FILE: &str = "fauna.group.file";

/// Class-2 kind: the set's **display name**, one cell at [`GROUP_NAME_KEY`],
/// value `fauna_core::group_name::GroupNameRecord` (the UTF-8 name, signed
/// by an authority device under its carried `DeviceAuthorization`, checked
/// against the authority line at the walk and in the fold); a nameless set
/// keeps its short id. **Frozen.**
pub const KIND_GROUP_NAME: &str = "fauna.group.name";

/// The logical key of a scope's single [`KIND_GROUP_NAME`] cell.
pub const GROUP_NAME_KEY: &str = "self";

/// Class-2 kind: a member device's **dial targets** for the scope, one cell
/// per writer device (logical key = the device id hex), value
/// `fauna_core::group_endpoints::GroupEndpointsRecord` (the
/// `DeviceEndpointsEntry` naming the writer's own actor, signed by the
/// writer device under its carried `DeviceAuthorization`, checked against
/// the authority line and the live roster at the walk and in the pump's
/// read — the group twin of `fauna.state.share-endpoints`, with the file
/// row's carriage). Machinery-sealed: a member must
/// dial before it holds any generation key, and a custodian serving the
/// scope must route it. **Frozen.**
pub const KIND_GROUP_ENDPOINTS: &str = "fauna.group.endpoints";

/// Which **key material seals** a group kind's entries — the group plane's
/// third-column sibling of `crate::merge_policy::SealingEpoch`, with the T20
/// sealing ruling's two strata (`key-material-hierarchy.md` § Audience: a
/// storage group, the machinery-root bullet).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupSealing {
    /// Seals under the scope's birth-minted, never-rotated **group machinery
    /// root** (`fauna_core::crypto::GroupMachineryRoot`) — the group's
    /// `Gen0` analogue, with possession replacing derivability: a joiner
    /// unwraps the root from its admission bundle and can then read the
    /// whole roster and mint DAG before holding any generation key.
    MachineryRoot,
    /// Seals under the current admissible, roster-covered group generation
    /// tip (`fauna_core::group_generation::resolve_admissible_group_tip`)
    /// through the content branch (`fauna_core::crypto::GroupContentSchedule`),
    /// in the v2 envelope that names the generation — content kinds only.
    GenerationTip,
}

/// The registered group-plane class-2 kinds: two frozen columns per kind.
/// Grows one row per kind as kinds land — not pre-populated for kinds whose
/// value shape nobody has built (the account table's rule).
const GROUP_POLICIES: &[(&str, MergePolicy, GroupSealing)] = &[
    (
        KIND_GROUP_BIRTH,
        // The scope id is derived from these bytes, so a second value under
        // the key is a different scope's record, never a newer one.
        MergePolicy::Immutable,
        GroupSealing::MachineryRoot,
    ),
    (
        KIND_GROUP_ROSTER,
        // Per-entry monotone lattice, `Removed` absorbing — a CRDT join in
        // bytes, owned and law-tested by `fauna_core::group_scope`.
        MergePolicy::CrdtPerField,
        GroupSealing::MachineryRoot,
    ),
    (
        KIND_GROUP_GENERATION_MINT,
        // Immutable-by-lattice: `Shredded` is the absorbing in-value shred
        // marker (the R14 (account-data-plane.md § The ratified decisions) build ruling — CrdtPerField refuses tombstones, so
        // deletion is a lattice phase).
        MergePolicy::CrdtPerField,
        GroupSealing::MachineryRoot,
    ),
    (
        KIND_GROUP_GENERATION_WRAP,
        // Per-healer actor-keyed cells, verifying-preferred rank; the join
        // lives in `fauna_core::group_generation`, total on purpose with
        // first-contact strictness in the adoption arm.
        MergePolicy::CrdtPerField,
        GroupSealing::MachineryRoot,
    ),
    (
        KIND_GROUP_GENERATION_UNKEYABLE,
        // One honest author per cell — the member — same rank and the same
        // total-join/strict-adoption split as the wrap kind.
        MergePolicy::CrdtPerField,
        GroupSealing::MachineryRoot,
    ),
    (
        KIND_GROUP_GENERATION_SHRED,
        // One honest author per cell — the shredder — rank (verifies-at-cell,
        // bytes), decode-or-fail, strict adoption: the account shred kind's
        // shape at this plane.
        MergePolicy::CrdtPerField,
        GroupSealing::MachineryRoot,
    ),
    (
        KIND_GROUP_AUTHORITY_REVOCATION,
        // Per-revoker cells, verifying-preferred rank, no removing phase (the
        // revoked set only grows); decode-or-fail join, first-contact strictness in
        // the adoption arm — the wrap kind's split.
        MergePolicy::CrdtPerField,
        GroupSealing::MachineryRoot,
    ),
    (
        KIND_GROUP_FILE,
        // One value per path, ordered by the author's signed stamp; a delete
        // is a value phase. Content: severed by the next generation.
        MergePolicy::LatestWins,
        GroupSealing::GenerationTip,
    ),
    (
        KIND_GROUP_NAME,
        // One cell, the authority's latest name.
        MergePolicy::LatestWins,
        GroupSealing::GenerationTip,
    ),
    (
        KIND_GROUP_ENDPOINTS,
        // Per-device cells, each device's latest advertisement.
        MergePolicy::LatestWins,
        GroupSealing::MachineryRoot,
    ),
];

/// The merge policy for a group-plane `kind`, or `None` when this build does
/// not know it — the same compat answer as
/// [`crate::merge_policy::merge_policy`]: a newer writer's kind is left
/// alone, never guessed at.
pub fn group_merge_policy(kind: &str) -> Option<MergePolicy> {
    GROUP_POLICIES
        .iter()
        .find(|(k, _, _)| *k == kind)
        .map(|(_, policy, _)| *policy)
}

/// The registered sealing stratum for a group-plane `kind`, or `None` when
/// this build does not know it.
pub fn group_sealing(kind: &str) -> Option<GroupSealing> {
    GROUP_POLICIES
        .iter()
        .find(|(k, _, _)| *k == kind)
        .map(|(_, _, sealing)| *sealing)
}

/// Every group-plane class-2 kind this build knows — the group scope's
/// trial-open set, deliberately disjoint from
/// [`crate::merge_policy::class2_kinds`] (a law test pins the disjointness).
pub fn group_kinds() -> impl Iterator<Item = &'static str> {
    GROUP_POLICIES.iter().map(|(k, _, _)| *k)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_registered_kind_answers_both_columns() {
        for kind in group_kinds() {
            assert!(group_merge_policy(kind).is_some(), "{kind}");
            assert!(group_sealing(kind).is_some(), "{kind}");
        }
        assert_eq!(group_merge_policy("fauna.group.unheard-of"), None);
        assert_eq!(group_sealing("fauna.group.unheard-of"), None);
    }

    /// The two registries are disjoint by construction: a kind lives in
    /// exactly one plane's table, so no lookup can answer for the wrong
    /// plane and neither trial-open set grows the other's kinds.
    #[test]
    fn group_kinds_and_account_kinds_are_disjoint() {
        for kind in group_kinds() {
            assert!(
                crate::merge_policy::merge_policy(kind).is_none(),
                "{kind} must not be registered in the account-state table"
            );
        }
        for kind in crate::merge_policy::class2_kinds() {
            assert!(
                group_merge_policy(kind).is_none(),
                "{kind} must not be registered in the group table"
            );
        }
    }

    /// The stratum of every registered kind, pinned by name: the content
    /// kinds — a set's path state and its name — seal under the tip, so a
    /// removal severs them; the machinery a member needs before it holds any
    /// generation key (dial targets included) seals under the root. A new
    /// kind fails here until someone states its stratum.
    #[test]
    fn every_kind_seals_under_its_stated_stratum() {
        let content = [KIND_GROUP_FILE, KIND_GROUP_NAME];
        for kind in group_kinds() {
            let want = if content.contains(&kind) {
                GroupSealing::GenerationTip
            } else {
                GroupSealing::MachineryRoot
            };
            assert_eq!(group_sealing(kind), Some(want), "{kind}");
        }
        assert_eq!(
            group_kinds().count(),
            10,
            "a kind landed without a stratum line here"
        );
    }

    /// The group set's three kinds order by the author's stamp: a
    /// stampless write is refused at first contact, like every LatestWins
    /// kind's.
    #[test]
    fn the_group_set_kinds_merge_latest_wins() {
        for kind in [KIND_GROUP_FILE, KIND_GROUP_NAME, KIND_GROUP_ENDPOINTS] {
            assert_eq!(
                group_merge_policy(kind),
                Some(MergePolicy::LatestWins),
                "{kind}"
            );
        }
    }
}
