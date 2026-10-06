//! The account's **attested predecessors** as a runtime is handed them: each
//! succeeded-from identity this device holds the seed of, with the three
//! pieces of that identity's key material the runtime may use, all open-only —
//! its generation-0 **delegable** account-state schedule, its generation-0
//! keys for the ONE fleet-only kind `fauna.state.generation-mint`, and its
//! generation-0 keys for EVERY fleet-only machinery kind (the retired
//! machinery keys).
//!
//! Four consumers read it, and they must never disagree about which
//! identities count:
//!
//! - the generation writer door's `prior`
//!   ([`crate::generation_tip::GenerationTrust::prior`]) reads the ids;
//! - the delegable scope's walk reads the schedules, to carry a predecessor's
//!   delegable rows across the succession
//!   (`succession-aftermath.md` § Re-key scope → *The account-state plane's
//!   generation-0 delegable rows are carried by the successor's walk*);
//! - the fleet scope's walk reads the mint-kind keys, to carry the mint
//!   record of each generation whose key this device holds
//!   (`owner-key-material.md` § Path A-sibling-2 → *Rotation*, the succession
//!   rider → *What crosses*);
//! - the fleet scope's reclamation pass reads the retired machinery keys, to
//!   open a predecessor's generation-0 machinery rows and retire them
//!   (`account-data-taxonomy.md` § The generation machinery → *Fleet-scope
//!   reclamation*, clause (3)(i)).
//!
//! So they travel as ONE value, and every constructor but [`none`] takes
//! key material: a host cannot attest an identity and leave the keys behind,
//! which would be a silent failure (the trust works, the successor's muted
//! words never arrive and its pre-succession generations are never
//! re-escrowed). A registry-holding host builds it from the registry's own
//! walk ([`from_registry`]); the seedless agent, which holds no registry, from
//! the two lists its capability carries ([`from_lists`]). Every key list
//! derives from the same retired `BackupKey` inside the constructor, so no
//! host, FFI call or capability field carries the fleet-only keys.
//!
//! The fleet-only branch is derived per kind, never as a schedule, and each
//! list has ONE consumer. The walk is handed the mint kind's pair alone
//! ([`mint_kind_keys`]): it cannot open that identity's device-set, wrap,
//! escrow-target, escrow-receipt, unkeyable, closed or reach rows, which a
//! succession re-makes and never carries (the rider → *What is re-made, never
//! carried*). The wider list ([`retired_machinery_keys`]) — one pair per kind
//! the registry seals fleet-only at generation 0, taken from the registry so
//! a new machinery kind is covered without an edit here — goes to the
//! reclamation pass on the bound nest and to nothing else: it opens a row
//! only to learn that it is the retired identity's machinery and retire it
//! (the rider → *The retired machinery keys open only to retire*).
//!
//! The lists are sets, not an index-aligned table: the door asks "is this
//! signer attested", a walk asks "does any retired key open this row", and
//! neither asks which key belongs to which id.
//!
//! [`none`]: AttestedPredecessors::none
//! [`from_registry`]: AttestedPredecessors::from_registry
//! [`from_lists`]: AttestedPredecessors::from_lists
//! [`mint_kind_keys`]: AttestedPredecessors::mint_kind_keys
//! [`retired_machinery_keys`]: AttestedPredecessors::retired_machinery_keys

use std::sync::Arc;

use fauna_core::crypto::{
    AccountStateKindKeys, AudienceRung, BackupKey, DelegableSchedule, FleetOnlySchedule,
    SealingEpoch,
};
use fauna_core::identity::ActorId;
use fauna_protocol::merge_policy::{
    KIND_GENERATION_MINT, audience_rung, class2_kinds, sealing_epoch,
};

/// See the module docs. Cheap to clone (the runtime's host and its driver each
/// keep one); holds no `BackupKey` and no fleet-only schedule — the delegable
/// branch, the mint kind's pair and the per-kind machinery pairs are derived
/// at construction and the key is dropped.
#[derive(Clone, Default)]
pub struct AttestedPredecessors(Arc<Inner>);

#[derive(Default)]
struct Inner {
    actor_ids: Vec<ActorId>,
    delegable: Vec<DelegableSchedule>,
    mint_kind: Vec<AccountStateKindKeys>,
    /// Every predecessor's pair for every fleet-only generation-0 kind, flat
    /// (the module docs: a set).
    machinery: Vec<AccountStateKindKeys>,
}

/// The registry's fleet-only generation-0 kinds — the generation machinery,
/// eight kinds today, the mint kind among them.
fn machinery_kinds() -> impl Iterator<Item = &'static str> {
    class2_kinds().filter(|kind| {
        matches!(audience_rung(kind), Some(AudienceRung::FleetOnly))
            && sealing_epoch(kind) == Some(SealingEpoch::Gen0)
    })
}

impl Inner {
    /// The one derivation every constructor shares, so the key lists can
    /// never come from different retired keys.
    fn derive<'k>(
        actor_ids: Vec<ActorId>,
        retired_backup_keys: impl IntoIterator<Item = &'k BackupKey>,
    ) -> Self {
        let mut inner = Self {
            actor_ids,
            ..Self::default()
        };
        for key in retired_backup_keys {
            inner.delegable.push(DelegableSchedule::derive(key));
            inner
                .mint_kind
                .push(FleetOnlySchedule::generation_0_kind_keys(
                    key,
                    KIND_GENERATION_MINT,
                ));
            inner.machinery.extend(
                machinery_kinds().map(|kind| FleetOnlySchedule::generation_0_kind_keys(key, kind)),
            );
        }
        inner
    }
}

impl AttestedPredecessors {
    /// Nothing attested — every identity that never succeeded. Fail-safe on a
    /// successor: predecessor-signed enrollments stop verifying and no
    /// predecessor row is carried; nothing extra is admitted.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// Each attested identity with the `BackupKey` its seed derives, nearest
    /// hop first — the pairs `AccountRegistry::predecessor_backup_keys_by_actor`
    /// answers.
    #[must_use]
    pub fn from_backup_keys<'k>(
        predecessors: impl IntoIterator<Item = (ActorId, &'k BackupKey)>,
    ) -> Self {
        let (actor_ids, keys): (Vec<ActorId>, Vec<&BackupKey>) = predecessors.into_iter().unzip();
        Self(Arc::new(Inner::derive(actor_ids, keys)))
    }

    /// What `actor_id_hex`'s registry rows attest: the succeeded-from
    /// identities whose seeds this device holds
    /// (`AccountRegistry::predecessor_backup_keys_by_actor`). The constructor
    /// every registry-holding host uses, so the ids and the schedules come
    /// off one walk.
    #[cfg(feature = "account-driver")]
    #[must_use]
    pub fn from_registry(
        registry: &fauna_client_accounts::AccountRegistry,
        actor_id_hex: &str,
    ) -> Self {
        let pairs = registry.predecessor_backup_keys_by_actor(actor_id_hex);
        Self::from_backup_keys(pairs.iter().map(|(id, key)| (*id, key)))
    }

    /// The seedless host's constructor: the attested ids and the retired owner
    /// keys as the identity-holding app pushed them — two lists of one
    /// registry walk (`SyncCapability::predecessor_actor_ids` and
    /// `predecessor_backup_keys`), which the agent cannot pair itself (a
    /// `BackupKey` does not derive its actor id) and does not need to (the
    /// module docs: both are sets).
    #[must_use]
    pub fn from_lists<'k>(
        actor_ids: Vec<ActorId>,
        retired_backup_keys: impl IntoIterator<Item = &'k BackupKey>,
    ) -> Self {
        Self(Arc::new(Inner::derive(actor_ids, retired_backup_keys)))
    }

    /// The attested identities — the writer door's `prior`.
    #[must_use]
    pub fn actor_ids(&self) -> &[ActorId] {
        &self.0.actor_ids
    }

    /// Their generation-0 delegable schedules — the walk's trial-open keys
    /// for the inherited carry, in [`Self::actor_ids`]' order.
    #[must_use]
    pub fn delegable_schedules(&self) -> &[DelegableSchedule] {
        &self.0.delegable
    }

    /// Their generation-0 keys for the mint kind alone — the fleet walk's
    /// trial-open keys for the mint-record carry, in [`Self::actor_ids`]'
    /// order. Open-only by type: an [`AccountStateKindKeys`] mints no grant,
    /// and the walk seals nothing under it.
    #[must_use]
    pub fn mint_kind_keys(&self) -> &[AccountStateKindKeys] {
        &self.0.mint_kind
    }

    /// Their generation-0 keys for every fleet-only machinery kind — the
    /// reclamation pass's keys for clause (3)(i)'s predecessor arm
    /// (`AccountStatePlane::with_predecessor_machinery_keys`), and nothing
    /// else's: the walk reads [`Self::mint_kind_keys`]. Open-only by type,
    /// like the mint kind's pair; a flat set, one pair per (predecessor,
    /// kind).
    #[must_use]
    pub fn retired_machinery_keys(&self) -> &[AccountStateKindKeys] {
        &self.0.machinery
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.actor_ids.is_empty()
    }
}

/// Same identities, same key material — what a host compares to decide
/// whether a re-pushed capability changed what it mounted. The mint-kind
/// pairs are compared by a blinded item key (the pair's secret halves have no
/// accessor, by design); they derive from the same retired key as the
/// schedule beside them, so this half can only ever agree with the first.
/// The machinery pairs derive from that key too, and are not compared again.
impl PartialEq for AttestedPredecessors {
    fn eq(&self, other: &Self) -> bool {
        let roots = |inner: &Inner| -> Vec<([u8; 32], [u8; 32])> {
            inner
                .delegable
                .iter()
                .map(|s| (**s.seal_root(), **s.item_blind_root()))
                .collect()
        };
        let mint_kind = |inner: &Inner| -> Vec<[u8; 32]> {
            inner.mint_kind.iter().map(|k| k.item_key(&[])).collect()
        };
        self.0.actor_ids == other.0.actor_ids
            && roots(&self.0) == roots(&other.0)
            && mint_kind(&self.0) == mint_kind(&other.0)
    }
}

impl Eq for AttestedPredecessors {}

/// Ids only: the schedules and the mint-kind pairs are key material.
impl std::fmt::Debug for AttestedPredecessors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("AttestedPredecessors")
            .field(&self.0.actor_ids)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ids_and_the_schedules_are_one_list() {
        let (a, b) = (
            BackupKey::from_bytes([0x11; 32]),
            BackupKey::from_bytes([0x22; 32]),
        );
        let attested = AttestedPredecessors::from_backup_keys([
            (ActorId([0xA1; 32]), &a),
            (ActorId([0xB2; 32]), &b),
        ]);
        assert_eq!(
            attested.actor_ids(),
            [ActorId([0xA1; 32]), ActorId([0xB2; 32])]
        );
        let schedules = attested.delegable_schedules();
        assert_eq!(schedules.len(), 2);
        assert_eq!(
            **schedules[1].seal_root(),
            **DelegableSchedule::derive(&b).seal_root()
        );
        // The seedless shape: the same two lists, handed apart.
        let unpaired = AttestedPredecessors::from_lists(
            vec![ActorId([0xA1; 32]), ActorId([0xB2; 32])],
            [&a, &b],
        );
        assert_eq!(unpaired, attested);
        assert_ne!(
            AttestedPredecessors::from_lists(vec![ActorId([0xA1; 32])], [&b]),
            AttestedPredecessors::from_lists(vec![ActorId([0xA1; 32])], [&a]),
            "a changed retired key is a changed value"
        );
        assert!(!format!("{attested:?}").is_empty());
        assert!(AttestedPredecessors::none().is_empty());
        assert!(
            AttestedPredecessors::none()
                .delegable_schedules()
                .is_empty()
        );
        assert!(AttestedPredecessors::none().mint_kind_keys().is_empty());
    }

    /// The mint-kind pair is the one the predecessor's own schedule seals a
    /// generation-mint row under (`merge_policy::kind_keys`) — it opens such
    /// a row — and it opens no other fleet-only kind's.
    #[test]
    fn the_mint_kind_keys_open_a_predecessors_mint_row_and_no_other_fleet_only_kind() {
        use fauna_core::account_entry_crypto::{
            EntryCoordinates, EntryPlaintext, open_entry, seal_entry,
        };
        use fauna_core::crypto::AccountStateKeySchedule;
        use fauna_protocol::merge_policy::{KIND_DEVICE_SET, kind_keys};

        let retired = BackupKey::from_bytes([0x11; 32]);
        let attested = AttestedPredecessors::from_backup_keys([(ActorId([0xA1; 32]), &retired)]);
        let [mint_keys] = attested.mint_kind_keys() else {
            panic!("one predecessor, one pair");
        };
        assert_eq!(mint_keys.kind(), KIND_GENERATION_MINT);

        let schedule = AccountStateKeySchedule::derive(&retired);
        let signer = ed25519_dalek::SigningKey::from_bytes(&[0x07; 32]);
        let coords = EntryCoordinates {
            writer_id: signer.verifying_key().to_bytes(),
            writer_seq: 3,
            scope: "fleet",
        };
        for (kind, opens) in [(KIND_GENERATION_MINT, true), (KIND_DEVICE_SET, false)] {
            let own = kind_keys(&schedule, kind).expect("a registered kind");
            let sealed = seal_entry(
                &own,
                &coords,
                &EntryPlaintext {
                    kind: kind.into(),
                    key: "k".into(),
                    value: vec![1, 2, 3].into(),
                    merge_meta: None,
                    tombstone: false,
                },
                &signer,
            )
            .expect("seal");
            assert_eq!(
                open_entry(mint_keys, &coords, &sealed.item_key, &sealed.envelope).is_ok(),
                opens,
                "{kind}"
            );
        }
    }

    /// The retired machinery keys are one pair per registry kind the
    /// predecessor's own schedule seals fleet-only at generation 0 — the
    /// eight machinery kinds today — and each predecessor row of such a kind
    /// opens under exactly one of them. A delegable row opens under none.
    #[test]
    fn the_retired_machinery_keys_open_every_fleet_only_generation_0_kind_and_no_delegable_one() {
        use fauna_core::account_entry_crypto::{
            EntryCoordinates, EntryPlaintext, open_entry, seal_entry,
        };
        use fauna_core::crypto::AccountStateKeySchedule;
        use fauna_protocol::merge_policy::{
            KIND_DEVICE_REACH, KIND_DEVICE_SET, KIND_ESCROW_RECEIPT, KIND_ESCROW_TARGET,
            KIND_GENERATION_CLOSED, KIND_GENERATION_UNKEYABLE, KIND_GENERATION_WRAP,
            KIND_MODERATION, kind_keys,
        };

        let machinery = [
            KIND_DEVICE_SET,
            KIND_GENERATION_MINT,
            KIND_GENERATION_WRAP,
            KIND_ESCROW_TARGET,
            KIND_ESCROW_RECEIPT,
            KIND_GENERATION_UNKEYABLE,
            KIND_DEVICE_REACH,
            KIND_GENERATION_CLOSED,
        ];
        let (a, b) = (
            BackupKey::from_bytes([0x11; 32]),
            BackupKey::from_bytes([0x22; 32]),
        );
        let attested = AttestedPredecessors::from_backup_keys([
            (ActorId([0xA1; 32]), &a),
            (ActorId([0xB2; 32]), &b),
        ]);
        let keys = attested.retired_machinery_keys();
        let mut kinds: Vec<&str> = keys[..machinery.len()].iter().map(|k| k.kind()).collect();
        kinds.sort_unstable();
        let mut expected = machinery.to_vec();
        expected.sort_unstable();
        assert_eq!(
            kinds, expected,
            "the registry's machinery kinds, per predecessor"
        );
        assert_eq!(keys.len(), 2 * machinery.len(), "both predecessors' pairs");
        assert!(
            AttestedPredecessors::none()
                .retired_machinery_keys()
                .is_empty()
        );

        let schedule = AccountStateKeySchedule::derive(&b);
        let signer = ed25519_dalek::SigningKey::from_bytes(&[0x07; 32]);
        let coords = EntryCoordinates {
            writer_id: signer.verifying_key().to_bytes(),
            writer_seq: 3,
            scope: "fleet",
        };
        for kind in machinery.iter().copied().chain([KIND_MODERATION]) {
            let own = kind_keys(&schedule, kind).expect("a registered kind");
            let sealed = seal_entry(
                &own,
                &coords,
                &EntryPlaintext {
                    kind: kind.into(),
                    key: "k".into(),
                    value: vec![1, 2, 3].into(),
                    merge_meta: None,
                    tombstone: false,
                },
                &signer,
            )
            .expect("seal");
            let opened: Vec<&str> = keys
                .iter()
                .filter(|k| open_entry(k, &coords, &sealed.item_key, &sealed.envelope).is_ok())
                .map(|k| k.kind())
                .collect();
            let want: Vec<&str> = if kind == KIND_MODERATION {
                vec![]
            } else {
                vec![kind]
            };
            assert_eq!(opened, want, "{kind}");
        }
    }
}
