//! The peer witness door's production evaluator: what lets a node's share
//! plane admit a carried group-membership certificate
//! (`fauna_peer_share::admission::verdict_for_group_membership`) against its
//! OWN state rather than refusing every one.
//!
//! Authority: `docs/goal/architecture/account-data-taxonomy.md` § The
//! recipient-set scheme → *Severance, per axis* (every authority check reads
//! the authority line through one implementation, built from the reader's own
//! merged `fauna.group.authority-revocation` rows) and
//! `docs/goal/architecture/account-data-plane.md` § The admission seam (the
//! membership witness behind a pump-fed `GroupRosterState` seam).
//!
//! # Where the evaluator's rows come from
//!
//! From this replica's account store and nowhere else: the pump reads every
//! held scope's persisted group-plane rows once per pass
//! ([`GroupRosterSnapshot::load_held`], through the same fold the folders page
//! lists with — `crate::group_scope_view::read_group_scope`) and swaps the
//! value into a [`LiveGroupRoster`]. The admit path reads memory. Nothing a
//! witness carries ever reaches the authority line.
//!
//! # What this door admits into — nothing serves a group scope yet
//!
//! Wiring the door makes a node's admit exchange VERIFY a group witness and
//! record the verdict on the connection. No share data arm consumes a group
//! verdict today (every arm is M2/folder-family), so admission is the whole
//! effect. The data arm that will — a group serve path — owes the per-request
//! live re-consult of [`GroupRosterState::is_entry_removed`] that
//! `fauna_peer_share::admission::verdict_admits_group` documents; this
//! module's snapshot is what that consult will read.

use std::sync::{Arc, RwLock};

use fauna_core::group_scope::GroupAuthority;
use fauna_peer_share::admission::GroupRosterState;

pub use crate::group_scope_view::GroupRosterSnapshot;

/// The pump-fed evaluator cell the share plane's driver owns for its whole
/// loop: empty (every group refused) until the first pass reads the store,
/// then replaced wholesale each pass. Replacing — never merging — is what
/// lets a scope that left the store (a departure dropped its rows) stop
/// being vouched for on the next pass.
#[derive(Default)]
pub struct LiveGroupRoster(RwLock<Arc<GroupRosterSnapshot>>);

impl LiveGroupRoster {
    /// Swap in this pass's snapshot.
    pub fn replace(&self, snapshot: GroupRosterSnapshot) {
        *self.0.write().unwrap() = Arc::new(snapshot);
    }

    /// Forget every scope — the fail-closed answer when a pass could not read
    /// the store: a snapshot that can no longer be refreshed can no longer
    /// learn a removal either.
    pub fn clear(&self) {
        self.replace(GroupRosterSnapshot::default());
    }

    fn current(&self) -> Arc<GroupRosterSnapshot> {
        Arc::clone(&self.0.read().unwrap())
    }
}

impl GroupRosterState for LiveGroupRoster {
    fn authority(&self, scope_id: &[u8; 32]) -> Option<GroupAuthority> {
        self.current().authority(scope_id)
    }

    fn is_entry_removed(&self, scope_id: &[u8; 32], entry_id: &[u8; 32]) -> bool {
        self.current().is_entry_removed(scope_id, entry_id)
    }
}

#[cfg(test)]
mod tests {
    //! The door's definition of success, over a REAL store: an enrolled
    //! member admitted, a `Removed` entry refused (the roster feed), an entry
    //! whose authority device the store learned revoked refused (the
    //! revocation feed), a
    //! birth row that is another scope's rooting nothing (the birth feed), and
    //! a scope this device does not hold refused (the enumeration).
    use super::*;
    use ed25519_dalek::SigningKey;
    use fauna_account_store::sqlite::SqliteBackend;
    use fauna_account_store::store::AccountStore;
    use fauna_account_store::types::{StateEntry, WriterId};
    use fauna_core::crypto::GroupMachineryRoot;
    use fauna_core::data::{Capability, DeviceAuthorization, Timestamp};
    use fauna_core::encoding::{EmbedAsBytes, canonical_encode, sign_envelope};
    use fauna_core::group_generation::GroupHeldRootRecord;
    use fauna_core::group_scope::{
        GroupBirthRecord, RosterEntryCore, group_scope_id, roster_cell_key,
        sign_authority_revocation, sign_roster_enrollment, sign_roster_removal,
    };
    use fauna_core::identity::ActorKeypair;
    use fauna_peer_share::admission::verdict_for_group_membership;
    use fauna_protocol::group_state::{
        GROUP_BIRTH_KEY, KIND_GROUP_AUTHORITY_REVOCATION, KIND_GROUP_BIRTH, KIND_GROUP_ROSTER,
    };
    use fauna_protocol::merge_policy::{KIND_GROUP_MACHINERY_ROOT, home_scope_for_kind};
    use fauna_protocol::scope::GroupScope;

    /// The scope's authority account — and this store's own account.
    fn us() -> ActorKeypair {
        ActorKeypair::from_secret([21u8; 32])
    }
    /// The member presenting a witness at the door.
    fn them() -> ActorKeypair {
        ActorKeypair::from_secret([31u8; 32])
    }
    /// The authority device that signed `them`'s entry.
    fn authority_device() -> SigningKey {
        SigningKey::from_bytes(&[0x41; 32])
    }

    fn root() -> GroupMachineryRoot {
        GroupMachineryRoot::from_bytes([0xD7; 32])
    }

    fn birth() -> GroupBirthRecord {
        GroupBirthRecord {
            authority_actor: us().actor_id(),
            salt: [0xB1; 32],
            machinery_root_commit: root().commitment(),
            created_at_ms: 1_700_000_000_000,
        }
    }

    fn cert(device: &SigningKey) -> Vec<u8> {
        let cert = DeviceAuthorization {
            actor_id: us().actor_id(),
            device_key: device.verifying_key().to_bytes(),
            capabilities: vec![Capability::RenewBearer],
            created_at: Timestamp(1_000),
            expires_at: None,
        };
        let (bytes, env) = sign_envelope(&us(), &cert).expect("sign cert");
        canonical_encode(&EmbedAsBytes::from_signed(bytes, env))
            .expect("carriage")
            .to_vec()
    }

    struct Fx {
        _dir: tempfile::TempDir,
        store: AccountStore<SqliteBackend>,
        scope_id: [u8; 32],
        scope: String,
        /// `them`'s `Enrolled` entry — the row the store holds AND the
        /// certificate `them` carries to the door.
        carried: Vec<u8>,
        entry_id: [u8; 32],
    }

    /// A store holding one group scope: the held root (unless `held` is
    /// false), the birth row and `them`'s enrolled roster cell.
    async fn fixture(held: bool) -> Fx {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = AccountStore::open(
            SqliteBackend::open(dir.path()).unwrap(),
            &us().actor_id_hex(),
            WriterId(authority_device().verifying_key().to_bytes()),
        )
        .await
        .unwrap();
        let scope_id = group_scope_id(&birth()).expect("scope id");
        if held {
            store
                .put_state(StateEntry {
                    kind: KIND_GROUP_MACHINERY_ROOT.into(),
                    key: GroupHeldRootRecord::logical_key_for(&scope_id),
                    scope: home_scope_for_kind(KIND_GROUP_MACHINERY_ROOT)
                        .unwrap()
                        .into(),
                    value: canonical_encode(&GroupHeldRootRecord {
                        scope_id,
                        root: fauna_core::secret::SecretByteBuf::from(root().as_bytes().to_vec()),
                        held_since_ms: 1_700_000_000_000,
                    })
                    .unwrap()
                    .to_vec(),
                    merge_meta: None,
                    entry_version: 0,
                    tombstone: false,
                })
                .await
                .unwrap();
        }
        let (entry_id, record) = sign_roster_enrollment(
            &authority_device(),
            RosterEntryCore {
                scope_id,
                member_actor: them().actor_id(),
                admission_salt: [0x01; 32],
            },
            vec![0xE0; 32],
            cert(&authority_device()),
            2_000,
        )
        .expect("entry");
        let carried = canonical_encode(&record).unwrap().to_vec();
        let fx = Fx {
            _dir: dir,
            store,
            scope_id,
            scope: GroupScope::new(scope_id).to_string(),
            carried: carried.clone(),
            entry_id,
        };
        fx.put_group(
            KIND_GROUP_BIRTH,
            GROUP_BIRTH_KEY,
            canonical_encode(&birth()).unwrap().to_vec(),
        )
        .await;
        fx.put_group(
            KIND_GROUP_ROSTER,
            &roster_cell_key(&entry_id, &authority_device().verifying_key().to_bytes()),
            carried,
        )
        .await;
        fx
    }

    impl Fx {
        async fn put_group(&self, kind: &str, key: &str, value: Vec<u8>) {
            self.store
                .put_group_state(StateEntry {
                    kind: kind.into(),
                    key: key.into(),
                    scope: self.scope.clone(),
                    value,
                    merge_meta: None,
                    entry_version: 0,
                    tombstone: false,
                })
                .await
                .unwrap();
        }

        /// The door's evaluation of `them`'s carried entry through the
        /// evaluator the pump would build from this store right now.
        async fn admit_them(&self) -> anyhow::Result<()> {
            let live = LiveGroupRoster::default();
            live.replace(GroupRosterSnapshot::load_held(&self.store).await.unwrap());
            verdict_for_group_membership(&self.carried, &self.scope_id, &live, &them().actor_id().0)
                .map(|_| ())
        }
    }

    #[tokio::test]
    async fn an_enrolled_member_is_admitted_through_the_store_fed_evaluator() {
        let fx = fixture(true).await;
        fx.admit_them()
            .await
            .expect("an enrolled member is admitted");
    }

    /// The ROSTER feed's pin: cut `is_entry_removed` off the store's roster
    /// rows and this goes green on a removed member. The removal is the
    /// authority device's own authored `Removed`, in its own cell — the one
    /// shape a reader honors (the per-writer roster cell ruling).
    #[tokio::test]
    async fn a_removed_entry_is_refused() {
        let fx = fixture(true).await;
        let (cell, removed) = sign_roster_removal(
            &authority_device(),
            fx.entry_id,
            cert(&authority_device()),
            3_000,
        );
        fx.put_group(
            KIND_GROUP_ROSTER,
            &cell,
            canonical_encode(&removed).unwrap().to_vec(),
        )
        .await;
        let err = fx
            .admit_them()
            .await
            .expect_err("a removed member is refused");
        assert!(err.to_string().contains("removal"), "{err}");
    }

    /// The REVOCATION feed's pin:
    /// build the line from an empty iterator instead of the store's own
    /// `fauna.group.authority-revocation` rows and this admits an entry
    /// signed by a device the authority has since revoked.
    #[tokio::test]
    async fn an_entry_signed_by_a_revoked_authority_device_is_refused() {
        let fx = fixture(true).await;
        let (key, record) = sign_authority_revocation(
            us().signing_key(),
            Vec::new(),
            fx.scope_id,
            authority_device().verifying_key().to_bytes(),
            5_000,
        );
        fx.put_group(
            KIND_GROUP_AUTHORITY_REVOCATION,
            &key,
            canonical_encode(&record).unwrap().to_vec(),
        )
        .await;
        let err = fx
            .admit_them()
            .await
            .expect_err("a revoked authority device enrols nobody");
        assert!(err.to_string().contains("does not verify"), "{err}");
    }

    /// The BIRTH feed's pin: under a held root, the stored birth row is
    /// replaced with one naming `them` as authority — the same root
    /// commitment, so any root holder could seal it, but a record that hashes
    /// to another scope's id. `them`'s own device then self-enrols `mallory`.
    /// Take the row's authority without re-deriving the id and `them` is the
    /// scope's root and `mallory` is admitted; re-derived, the scope is not
    /// held at all.
    #[tokio::test]
    async fn a_birth_row_that_is_another_scopes_roots_nothing_and_admits_nobody() {
        let fx = fixture(true).await;
        let forged = GroupBirthRecord {
            authority_actor: them().actor_id(),
            ..birth()
        };
        assert_ne!(group_scope_id(&forged).unwrap(), fx.scope_id);
        fx.put_group(
            KIND_GROUP_BIRTH,
            GROUP_BIRTH_KEY,
            canonical_encode(&forged).unwrap().to_vec(),
        )
        .await;

        let mallory = ActorKeypair::from_secret([51u8; 32]);
        let their_device = SigningKey::from_bytes(&[0x42; 32]);
        let their_cert = {
            let cert = DeviceAuthorization {
                actor_id: them().actor_id(),
                device_key: their_device.verifying_key().to_bytes(),
                capabilities: vec![Capability::RenewBearer],
                created_at: Timestamp(1_000),
                expires_at: None,
            };
            let (bytes, env) = sign_envelope(&them(), &cert).expect("sign cert");
            canonical_encode(&EmbedAsBytes::from_signed(bytes, env))
                .unwrap()
                .to_vec()
        };
        let (entry_id, record) = sign_roster_enrollment(
            &their_device,
            RosterEntryCore {
                scope_id: fx.scope_id,
                member_actor: mallory.actor_id(),
                admission_salt: [0x02; 32],
            },
            vec![0xE1; 32],
            their_cert,
            2_500,
        )
        .expect("entry");
        let carried = canonical_encode(&record).unwrap().to_vec();
        fx.put_group(
            KIND_GROUP_ROSTER,
            &roster_cell_key(&entry_id, &their_device.verifying_key().to_bytes()),
            carried.clone(),
        )
        .await;

        let snapshot = GroupRosterSnapshot::load_held(&fx.store).await.unwrap();
        assert!(
            snapshot.authority(&fx.scope_id).is_none(),
            "a birth row that is another scope's roots no authority here"
        );
        let live = LiveGroupRoster::default();
        live.replace(snapshot);
        let err =
            verdict_for_group_membership(&carried, &fx.scope_id, &live, &mallory.actor_id().0)
                .expect_err("a self-enrolled third party is refused");
        assert!(err.to_string().contains("holds no group scope"), "{err}");
    }

    /// The enumeration's pin: group rows alone, with no held machinery root,
    /// are a scope this device does not hold — and vouches for nothing.
    #[tokio::test]
    async fn a_scope_this_device_does_not_hold_is_refused() {
        let fx = fixture(false).await;
        let err = fx.admit_them().await.expect_err("no held root, no vouch");
        assert!(err.to_string().contains("holds no group scope"), "{err}");
    }

    #[tokio::test]
    async fn the_live_cell_refuses_until_fed_and_after_it_is_cleared() {
        let fx = fixture(true).await;
        let live = LiveGroupRoster::default();
        let admit = |live: &LiveGroupRoster| {
            verdict_for_group_membership(&fx.carried, &fx.scope_id, live, &them().actor_id().0)
        };
        assert!(admit(&live).is_err(), "an unfed cell holds no scope");
        live.replace(GroupRosterSnapshot::load_held(&fx.store).await.unwrap());
        assert!(admit(&live).is_ok(), "fed, it admits");
        live.clear();
        assert!(admit(&live).is_err(), "cleared, it refuses again");
    }

    /// The live cell cleared BETWEEN check 3's `authority()` and check 4's
    /// `is_entry_removed()` — a pass whose store read failed, landing in the
    /// window between the door's two reads.
    struct ClearsBetweenReads<'a>(&'a LiveGroupRoster);

    impl GroupRosterState for ClearsBetweenReads<'_> {
        fn authority(&self, scope_id: &[u8; 32]) -> Option<GroupAuthority> {
            let authority = self.0.authority(scope_id);
            self.0.clear();
            authority
        }

        fn is_entry_removed(&self, scope_id: &[u8; 32], entry_id: &[u8; 32]) -> bool {
            self.0.is_entry_removed(scope_id, entry_id)
        }
    }

    /// The two-read race's pin: answer `false` for a
    /// scope the evaluator no longer holds and this admits a member the
    /// first read's snapshot held as `Removed`.
    #[tokio::test]
    async fn a_removed_entry_is_refused_when_the_cell_clears_between_the_two_reads() {
        let fx = fixture(true).await;
        let (cell, removed) = sign_roster_removal(
            &authority_device(),
            fx.entry_id,
            cert(&authority_device()),
            3_000,
        );
        fx.put_group(
            KIND_GROUP_ROSTER,
            &cell,
            canonical_encode(&removed).unwrap().to_vec(),
        )
        .await;
        let live = LiveGroupRoster::default();
        live.replace(GroupRosterSnapshot::load_held(&fx.store).await.unwrap());
        let err = verdict_for_group_membership(
            &fx.carried,
            &fx.scope_id,
            &ClearsBetweenReads(&live),
            &them().actor_id().0,
        )
        .expect_err("a removed member is refused whichever snapshot check 4 reads");
        assert!(err.to_string().contains("removal"), "{err}");
    }

    /// The serve re-consult's answer: a scope not held reads as removed, so
    /// a cleared cell or a departed scope severs an admitted connection.
    #[tokio::test]
    async fn a_scope_not_held_reads_as_removed() {
        let fx = fixture(true).await;
        let live = LiveGroupRoster::default();
        assert!(live.is_entry_removed(&fx.scope_id, &fx.entry_id), "unfed");
        live.replace(GroupRosterSnapshot::load_held(&fx.store).await.unwrap());
        assert!(
            !live.is_entry_removed(&fx.scope_id, &fx.entry_id),
            "held, enrolled"
        );
        live.clear();
        assert!(live.is_entry_removed(&fx.scope_id, &fx.entry_id), "cleared");
    }
}
