//! The native T10 principal slot — the plane crate's
//! [`PrincipalBundle`] over `fauna_credential_store::CredentialStore` and the
//! store dir's `migration.lock` (`account-client-lifecycle.md` § The
//! client-side lifecycle → *Ruling (4)'s build decisions*, decision (c): one
//! bundle, every host). The bundle's mechanics — the attribute names, the
//! encodings, the registration latch, the standing refusal, the staged
//! removals, the retained-key carriage — live in
//! `fauna_account_plane::principal_bundle`, whose module doc is the
//! carriage's description; this module only instantiates it natively and
//! keeps every name at its old path (charter `account-data-plane.md` § The
//! store device principal).
//!
//! One [`PrincipalSlot`] per assembled runtime, resolved **inside the store's
//! migration/adoption critical section** exactly like the writer key: every
//! bundle item is a probe-then-act on state shared by all co-located
//! processes, and a measurement showed what an unserialized mint-or-load does
//! to the loser (`account_runtime`'s writer-key comment tells that story). The
//! bundle rests under the writer key's namespace
//! ([`crate::account_runtime::CRED_NAMESPACE`]).

use std::path::PathBuf;
use std::sync::Arc;

use fauna_account_plane::principal_bundle as bundle;
pub use fauna_account_plane::principal_bundle::{
    ChangeSignerCarriage, PrincipalBundle, SlotSection, retained_generation_keys, slot_writes,
};
/// The slot's data types and the seam it implements live in the plane crate
/// (`principal_custody` — ruling (2)'s slot seam), re-exported at their old
/// paths.
pub use fauna_account_plane::principal_custody::{
    EnrollmentRefusal, LoadedDeviceAuthorization, PrincipalBundleStatus, PrincipalCustody,
};
use fauna_account_store::locks::{MigrationLock, MigrationLockOutcome};
use fauna_core::crypto::BackupKey;
use fauna_credential_store::CredentialStore;

/// The native slot: the shared bundle over the platform credential store,
/// its read-modify-writes serialized under the store dir's `migration.lock`.
/// [`PrincipalSlot::resolve`] takes the store dir as its section.
pub type PrincipalSlot = PrincipalBundle<CredentialStore, MigrationSection>;

/// The native slot's retained-key carriage as a capability host's custody
/// ([`bundle::RetainedKeyCarriage`]): read fresh at every consult, written
/// under the same store dir's `migration.lock` the runtime's bundle writes
/// under.
pub type SlotRetainedKeys = bundle::RetainedKeyCarriage<CredentialStore, MigrationSection>;

/// The native [`SlotSection`]: the store dir whose `migration.lock`
/// serializes slot read-modify-write against sibling processes (the store
/// open's migration section, reused — one lock for every probe-then-act on
/// shared per-store state).
#[derive(Debug, Clone)]
pub struct MigrationSection {
    store_dir: PathBuf,
}

impl From<PathBuf> for MigrationSection {
    fn from(store_dir: PathBuf) -> Self {
        MigrationSection { store_dir }
    }
}

impl SlotSection for MigrationSection {
    type Guard = MigrationLock;

    fn enter(&self, degraded: &str) -> Option<MigrationLock> {
        match MigrationLock::acquire(&self.store_dir) {
            MigrationLockOutcome::Held(lock) => Some(lock),
            MigrationLockOutcome::Degraded(e) => {
                tracing::warn!(error = %e, "migration lock unavailable — {degraded}");
                None
            }
        }
    }
}

/// The bundle trims its retained-key carriage to the plane crate's spelling
/// of the item cap; the credential store owns the real one.
const _: () = assert!(bundle::MAX_SLOT_VALUE_BYTES == fauna_credential_store::MAX_ITEM_VALUE_BYTES);

/// The device-principal client's `not_registered` hook for `actor_id_hex`
/// (`fauna_client::ws_device_handshake_bearer::NotRegisteredHook`): void the
/// machine's registration latch in the production credential store
/// ([`bundle::void_grant_registration`] owns why), so the next owner-session
/// pass — this process's or a co-located app's — registers the grant again.
pub fn not_registered_voids_latch(actor_id_hex: &str) -> Arc<dyn Fn() + Send + Sync> {
    let actor_id_hex = actor_id_hex.to_string();
    Arc::new(move || {
        bundle::void_grant_registration(
            &crate::account_runtime::production_credential_store(),
            &actor_id_hex,
        );
    })
}

/// [`not_registered_voids_latch`] over the slot in `credentials` — the same
/// hook for a machine whose slot is not the production one (a conformance
/// suite's file-backed machine).
pub fn not_registered_voids_latch_in(
    credentials: CredentialStore,
    actor_id_hex: &str,
) -> Arc<dyn Fn() + Send + Sync> {
    let actor_id_hex = actor_id_hex.to_string();
    Arc::new(move || bundle::void_grant_registration(&credentials, &actor_id_hex))
}

/// A store is stamped with the slot's writer: the mint marker is spent.
pub(crate) fn clear_writer_unstamped(credentials: &CredentialStore, actor_id_hex: &str) {
    bundle::clear_writer_unstamped(credentials, actor_id_hex)
}

/// Whether the slot's writer key was minted and never yet stamped into a
/// store ([`bundle::writer_unstamped`]) — read natively only by tests now;
/// the lost-slot heal reads it through the plane crate.
#[cfg(test)]
pub(crate) fn writer_unstamped(credentials: &CredentialStore, actor_id_hex: &str) -> bool {
    bundle::writer_unstamped(credentials, actor_id_hex)
}

/// Load `<hex>/backup-key` — the seedless host's `BackupKey`
/// ([`bundle::load_backup_key`] owns the contract).
pub fn load_backup_key(credentials: &CredentialStore, actor_id_hex: &str) -> Option<BackupKey> {
    bundle::load_backup_key(credentials, actor_id_hex)
}

/// **Load-only** read of the store principal's signing key
/// ([`bundle::load_writer_key`] owns the contract — never a mint, and `Some`
/// is not enrollment evidence).
pub fn load_writer_key(
    credentials: &CredentialStore,
    actor_id_hex: &str,
) -> Option<ed25519_dalek::SigningKey> {
    bundle::load_writer_key(credentials, actor_id_hex)
}

/// This machine's change-record signer for `actor_id`, load-only
/// ([`bundle::load_change_signer`]).
pub fn load_change_signer(
    credentials: &CredentialStore,
    actor_id: &[u8; 32],
) -> Option<fauna_protocol::sync_writer_sig::ChangeSigner> {
    bundle::load_change_signer(credentials, actor_id)
}

/// The same signer as [`load_change_signer`], as the carriage a capability
/// host that mounts no slot is provisioned with
/// ([`bundle::load_change_signer_carriage`]).
pub fn load_change_signer_carriage(
    credentials: &CredentialStore,
    actor_id: &[u8; 32],
) -> Option<ChangeSignerCarriage> {
    bundle::load_change_signer_carriage(credentials, actor_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_account_plane::principal_bundle::test_support::*;
    use fauna_client_accounts::SecretStore;
    use fauna_core::crypto::GenerationKey;
    use fauna_core::data::DeviceAuthorization;
    use fauna_core::encoding::{EmbedAsBytes, canonical_decode, canonical_encode};
    use fauna_core::identity::ActorKeypair;

    fn slot_on(dir: &std::path::Path, account: &ActorKeypair) -> (PrincipalSlot, [u8; 32]) {
        let credentials =
            CredentialStore::with_file_backend("fauna-account-store", dir.join("creds"));
        let writer = ActorKeypair::generate();
        let writer_pub = writer.actor_id().0;
        let backup = BackupKey::derive(account.secret_bytes());
        let slot = PrincipalSlot::resolve(
            std::sync::Arc::new(credentials),
            fauna_core::hex32::encode(&account.actor_id().0),
            dir.join("store"),
            &writer_pub,
            Some(&backup),
        );
        (slot, writer_pub)
    }

    /// The same backing dir re-resolved — a second co-located process (or a
    /// relaunch) mounting the same slot.
    fn resolve_again(
        dir: &std::path::Path,
        account: &ActorKeypair,
        writer_pub: &[u8; 32],
    ) -> PrincipalSlot {
        let credentials =
            CredentialStore::with_file_backend("fauna-account-store", dir.join("creds"));
        PrincipalSlot::resolve(
            std::sync::Arc::new(credentials),
            fauna_core::hex32::encode(&account.actor_id().0),
            dir.join("store"),
            writer_pub,
            Some(&BackupKey::derive(account.secret_bytes())),
        )
    }

    fn grant_over(account: &ActorKeypair, device_key: [u8; 32]) -> EmbedAsBytes {
        let auth = DeviceAuthorization {
            actor_id: account.actor_id(),
            device_key,
            capabilities: vec![fauna_core::data::Capability::RenewBearer],
            created_at: fauna_core::data::Timestamp::now(),
            expires_at: None,
        };
        let (bytes, env) = fauna_core::encoding::sign_envelope(account, &auth).expect("sign");
        EmbedAsBytes::from_signed(bytes, env)
    }

    /// The machine's change-record signer loads only from a writer key whose
    /// grant carries `SyncWrite`, and signs as the account through that grant.
    #[test]
    fn the_change_signer_loads_only_under_a_sync_write_grant() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let account = ActorKeypair::generate();
        let actor = account.actor_id().0;
        let credentials =
            CredentialStore::with_file_backend("fauna-account-store", tmp.path().join("creds"));
        assert!(load_change_signer(&credentials, &actor).is_none(), "no key");

        let writer = ed25519_dalek::SigningKey::from_bytes(&[0x4B; 32]);
        let writer_pub = writer.verifying_key().to_bytes();
        credentials.set(&hex::encode(actor), &hex::encode(writer.to_bytes()));
        assert!(
            load_change_signer(&credentials, &actor).is_none(),
            "no grant"
        );

        let grant = |caps: Vec<fauna_core::data::Capability>| {
            let auth = DeviceAuthorization {
                actor_id: account.actor_id(),
                device_key: writer_pub,
                capabilities: caps,
                created_at: fauna_core::data::Timestamp::now(),
                expires_at: None,
            };
            let (bytes, env) = fauna_core::encoding::sign_envelope(&account, &auth).expect("sign");
            EmbedAsBytes::from_signed(bytes, env)
        };
        let store = |wire: EmbedAsBytes| {
            credentials.set(
                &attr(&hex::encode(actor), ATTR_DEVICE_AUTH),
                &hex::encode(canonical_encode(&wire).expect("encode")),
            );
        };
        store(grant(vec![fauna_core::data::Capability::RenewBearer]));
        assert!(
            load_change_signer(&credentials, &actor).is_none(),
            "a [RenewBearer]-only grant authors nothing"
        );
        store(grant(vec![
            fauna_core::data::Capability::RenewBearer,
            fauna_core::data::Capability::SyncWrite,
        ]));
        let signer = load_change_signer(&credentials, &actor).expect("loads");
        assert_eq!(signer.actor_id(), actor);
        assert_eq!(signer.signer_key(), writer_pub, "delegated: the device key");

        // The carriage a capability host is provisioned rebuilds that signer.
        let carriage = load_change_signer_carriage(&credentials, &actor).expect("carriage");
        assert_eq!(*carriage.writer_secret, writer.to_bytes());
        let rebuilt = fauna_protocol::sync_writer_sig::ChangeSigner::from_delegated_carriage(
            actor,
            &carriage.writer_secret,
            &carriage.device_authorization,
        )
        .expect("the carriage rebuilds");
        assert_eq!(rebuilt.signer_key(), writer_pub);
        // A re-minted writer key strands the old grant: no carriage until
        // the ceremony re-certifies.
        credentials.set(&hex::encode(actor), &hex::encode([0x5C; 32]));
        assert!(load_change_signer_carriage(&credentials, &actor).is_none());
    }

    /// A grant persisted in this process moves the slot-write signal a
    /// provisioning host re-reads the slot on.
    #[test]
    fn a_stored_grant_moves_the_slot_write_signal() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let account = ActorKeypair::generate();
        let (slot, writer_pub) = slot_on(tmp.path(), &account);
        let mut writes = slot_writes();
        writes.mark_unchanged();
        slot.store_device_authorization(grant_over(&account, writer_pub), &writer_pub)
            .expect("persist");
        assert!(
            writes.has_changed().expect("sender lives"),
            "a stored grant"
        );
    }

    #[test]
    fn a_fresh_slot_carries_backup_key_and_nothing_else() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let account = ActorKeypair::generate();
        let (slot, _) = slot_on(tmp.path(), &account);
        let status = slot.status();
        assert!(
            status.backup_key_persisted,
            "backup key persists at resolve"
        );
        assert!(status.device_authorization.is_none(), "not enrolled yet");
        assert_eq!(status.retained_generations, 0);
    }

    #[test]
    fn a_device_authorization_round_trips_into_a_second_slot() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let account = ActorKeypair::generate();
        let (slot, writer_pub) = slot_on(tmp.path(), &account);
        let wire = grant_over(&account, writer_pub);
        slot.store_device_authorization(wire.clone(), &writer_pub)
            .expect("persist");
        // The same slot answers immediately…
        assert_eq!(
            slot.device_authorization().expect("loaded").wire,
            wire,
            "the exact wire survives for re-registration"
        );
        // …and a second resolve (another process) loads it back verified.
        let second = resolve_again(tmp.path(), &account, &writer_pub);
        let loaded = second.device_authorization().expect("loads in a sibling");
        assert_eq!(loaded.authorization.device_key, writer_pub);
        assert_eq!(loaded.wire, wire);
    }

    #[test]
    fn a_grant_over_a_foreign_writer_is_refused_to_persist_and_to_load() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let account = ActorKeypair::generate();
        let (slot, writer_pub) = slot_on(tmp.path(), &account);
        let foreign = ActorKeypair::generate().actor_id().0;
        let wire = grant_over(&account, foreign);
        assert!(
            slot.store_device_authorization(wire.clone(), &writer_pub)
                .is_err(),
            "a grant covering a key that is not this store's writer is refused"
        );
        // Plant it raw (a stale slot from a re-minted writer key) — the
        // loader must not surface it either.
        let credentials =
            CredentialStore::with_file_backend("fauna-account-store", tmp.path().join("creds"));
        let hex_account = fauna_core::hex32::encode(&account.actor_id().0);
        credentials.set(
            &attr(&hex_account, ATTR_DEVICE_AUTH),
            &hex::encode(canonical_encode(&wire).expect("encode")),
        );
        let second = resolve_again(tmp.path(), &account, &writer_pub);
        assert!(
            second.device_authorization().is_none(),
            "a foreign-writer grant reads as not-enrolled"
        );
    }

    #[test]
    fn a_tampered_device_authorization_reads_as_not_enrolled() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let account = ActorKeypair::generate();
        let (slot, writer_pub) = slot_on(tmp.path(), &account);
        let wire = grant_over(&account, writer_pub);
        slot.store_device_authorization(wire.clone(), &writer_pub)
            .expect("persist");
        // Flip one byte of the inner canonical bytes — the CID no longer
        // matches, so verification fails and the loader treats it as absent.
        let mut tampered = wire;
        let last = tampered.bytes.len() - 1;
        tampered.bytes[last] ^= 0x01;
        let credentials =
            CredentialStore::with_file_backend("fauna-account-store", tmp.path().join("creds"));
        let hex_account = fauna_core::hex32::encode(&account.actor_id().0);
        credentials.set(
            &attr(&hex_account, ATTR_DEVICE_AUTH),
            &hex::encode(canonical_encode(&tampered).expect("encode")),
        );
        let second = resolve_again(tmp.path(), &account, &writer_pub);
        assert!(second.device_authorization().is_none());
    }

    /// The standing refusal (2026-09-15): recorded by whichever process met
    /// it, read fresh by a sibling resolve (the app reading what the agent's
    /// pump wrote), and cleared by the next accepted register — the remedy
    /// landing is what takes the Devices page's notice down.
    #[test]
    fn an_enrollment_refusal_is_shared_across_sibling_slots_and_cleared_by_a_register() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let account = ActorKeypair::generate();
        let (agent_side, writer_pub) = slot_on(tmp.path(), &account);
        let wire = grant_over(&account, writer_pub);
        agent_side
            .store_device_authorization(wire, &writer_pub)
            .expect("persist");
        assert_eq!(
            agent_side.enrollment_refusal(),
            None,
            "a fresh slot refuses nothing"
        );

        agent_side.record_enrollment_refused(EnrollmentRefusal::DeviceLimitExceeded);
        let app_side = resolve_again(tmp.path(), &account, &writer_pub);
        assert_eq!(
            app_side.enrollment_refusal(),
            Some(EnrollmentRefusal::DeviceLimitExceeded),
            "the app's slot reads the refusal the agent's pump recorded"
        );
        assert_eq!(
            EnrollmentRefusal::DeviceLimitExceeded.notice(),
            fauna_i18n::strings::error::sync::DEVICE_LIMIT_EXCEEDED
        );

        // The remedy landed: the nest accepted a register.
        agent_side.record_grant_registered_on("named-row");
        assert_eq!(
            app_side.enrollment_refusal(),
            None,
            "cleared for every sibling"
        );
        assert_eq!(
            app_side.grant_registration_row().as_deref(),
            Some("named-row")
        );

        // A value this build does not know renders nothing rather than a
        // guessed sentence (an older or newer sibling may have written it).
        let credentials =
            CredentialStore::with_file_backend("fauna-account-store", tmp.path().join("creds"));
        let hex_account = fauna_core::hex32::encode(&account.actor_id().0);
        credentials.set(
            &attr(&hex_account, ATTR_ENROLLMENT_REFUSED),
            "some_future_refusal",
        );
        assert_eq!(app_side.enrollment_refusal(), None);
    }

    #[test]
    fn a_mismatched_backup_key_slot_value_is_healed_to_the_derived_one() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let account = ActorKeypair::generate();
        let (slot, writer_pub) = slot_on(tmp.path(), &account);
        drop(slot);
        // Corrupt the slot value out-of-band.
        let credentials =
            CredentialStore::with_file_backend("fauna-account-store", tmp.path().join("creds"));
        let hex_account = fauna_core::hex32::encode(&account.actor_id().0);
        credentials.set(&attr(&hex_account, ATTR_BACKUP_KEY), &"ab".repeat(32));
        let _second = resolve_again(tmp.path(), &account, &writer_pub);
        let healed = credentials
            .get(&attr(&hex_account, ATTR_BACKUP_KEY))
            .expect("present");
        assert_eq!(
            healed,
            hex::encode(BackupKey::derive(account.secret_bytes()).to_bytes()),
            "the derived value is definitionally correct — the slot heals to it"
        );
    }

    /// The carriage's whole purpose: what the seed-holding side wrote is
    /// exactly what the seedless host reads back — the same `BackupKey` the
    /// account derives, so the agent unseals what every app sealed.
    #[test]
    fn the_seedless_reader_gets_the_key_the_seed_holding_side_persisted() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let account = ActorKeypair::generate();
        let (slot, _writer_pub) = slot_on(tmp.path(), &account);
        drop(slot);

        let credentials =
            CredentialStore::with_file_backend("fauna-account-store", tmp.path().join("creds"));
        let hex_account = fauna_core::hex32::encode(&account.actor_id().0);
        let read = load_backup_key(&credentials, &hex_account).expect("the carriage is present");
        assert_eq!(
            read.to_bytes(),
            BackupKey::derive(account.secret_bytes()).to_bytes(),
            "the seedless host must resolve the account's own key — any other \
             value seals state no app in the fleet can open"
        );
    }

    /// Absent and corrupt both read as `None`, because the seedless host's only
    /// safe response to either is the same: refuse to serve. Returning a
    /// fabricated key would be silent, unrecoverable corruption.
    #[test]
    fn the_seedless_reader_declines_an_absent_or_corrupt_carriage() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let account = ActorKeypair::generate();
        let credentials =
            CredentialStore::with_file_backend("fauna-account-store", tmp.path().join("creds"));
        let hex_account = fauna_core::hex32::encode(&account.actor_id().0);

        assert!(
            load_backup_key(&credentials, &hex_account).is_none(),
            "a machine no app has enrolled carries nothing"
        );

        credentials.set(&attr(&hex_account, ATTR_BACKUP_KEY), "not-hex-at-all");
        assert!(
            load_backup_key(&credentials, &hex_account).is_none(),
            "a corrupt value must not be salvaged into a key"
        );
    }

    /// A seedless resolve **must not write** the slot: it derives nothing, so it
    /// has no authoritative value to heal with — its own key came out of this
    /// very slot. If it healed, a host that had somehow resolved a wrong key
    /// would overwrite the account's real one and lock every app out.
    #[test]
    fn a_seedless_resolve_leaves_the_backup_key_slot_untouched() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let account = ActorKeypair::generate();
        let credentials =
            CredentialStore::with_file_backend("fauna-account-store", tmp.path().join("creds"));
        let hex_account = fauna_core::hex32::encode(&account.actor_id().0);
        let writer_pub = [0x42u8; 32];

        // Nothing in the slot, and a seedless resolve over it.
        let _slot = PrincipalSlot::resolve(
            std::sync::Arc::new(credentials),
            hex_account.clone(),
            tmp.path().join("store"),
            &writer_pub,
            None,
        );

        let after =
            CredentialStore::with_file_backend("fauna-account-store", tmp.path().join("creds"));
        assert!(
            after.get(&attr(&hex_account, ATTR_BACKUP_KEY)).is_none(),
            "the seedless side writes no backup key — persisting is the \
             seed-holding side's job, where the derived value is definitionally \
             correct for the account"
        );
    }

    /// The succession rider's carriage: a successor's fresh slot takes every
    /// retained key the ATTESTED predecessor's slot holds on this machine,
    /// idempotently, and nothing from a slot it did not name. Red-verified:
    /// with the carry loop gone the successor keys nothing, and the floor test
    /// in `conformance_account_state_walk.rs` has no key to re-escrow.
    #[test]
    fn a_successor_slot_carries_the_attested_predecessors_retained_keys() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (old, new, stranger) = (
            ActorKeypair::generate(),
            ActorKeypair::generate(),
            ActorKeypair::generate(),
        );
        let (predecessor, _) = slot_on(tmp.path(), &old);
        let (unnamed, _) = slot_on(tmp.path(), &stranger);
        let (id_a, key_a) = ([0xA1; 32], GenerationKey::mint());
        predecessor.record_generation_key(&id_a, &key_a);
        unnamed.record_generation_key(&[0xB2; 32], &GenerationKey::mint());

        let (successor, _) = slot_on(tmp.path(), &new);
        assert_eq!(successor.status().retained_generations, 0, "a fresh slot");
        assert_eq!(
            successor.carry_predecessor_generation_keys(&[old.actor_id()]),
            1
        );
        assert_eq!(
            successor
                .retained_generation_key(&id_a)
                .expect("carried")
                .as_bytes(),
            key_a.as_bytes()
        );
        assert_eq!(
            successor.status().retained_generations,
            1,
            "the unnamed slot's key stays where it is"
        );
        assert_eq!(
            successor.carry_predecessor_generation_keys(&[old.actor_id()]),
            0,
            "idempotent"
        );
        assert_eq!(
            successor.carry_predecessor_generation_keys(&[new.actor_id()]),
            0,
            "its own id is never a predecessor"
        );
    }

    #[test]
    fn retained_keys_round_trip_and_merge_across_sibling_slots() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let account = ActorKeypair::generate();
        let (first, writer_pub) = slot_on(tmp.path(), &account);
        // A second co-located process resolved BEFORE either records — its
        // in-memory view starts empty, which is exactly the lost-update shape
        // the read-merge-write exists for.
        let second = resolve_again(tmp.path(), &account, &writer_pub);

        let key_a = GenerationKey::mint();
        let key_b = GenerationKey::mint();
        let id_a = [0xAA; 32];
        let id_b = [0xBB; 32];
        first.record_generation_key(&id_a, &key_a);
        second.record_generation_key(&id_b, &key_b);

        // Both survive in the slot: a third resolve sees the union.
        let third = resolve_again(tmp.path(), &account, &writer_pub);
        assert_eq!(third.status().retained_generations, 2, "no lost update");
        assert_eq!(
            third
                .retained_generation_key(&id_a)
                .expect("A retained")
                .as_bytes(),
            key_a.as_bytes()
        );
        assert_eq!(
            third
                .retained_generation_key(&id_b)
                .expect("B retained")
                .as_bytes(),
            key_b.as_bytes()
        );
    }

    #[test]
    fn a_conflicting_key_for_a_retained_generation_never_clobbers() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let account = ActorKeypair::generate();
        let (slot, _) = slot_on(tmp.path(), &account);
        let id = [0xCC; 32];
        let original = GenerationKey::mint();
        slot.record_generation_key(&id, &original);
        slot.record_generation_key(&id, &GenerationKey::mint());
        assert_eq!(
            slot.retained_generation_key(&id)
                .expect("still retained")
                .as_bytes(),
            original.as_bytes(),
            "verified custody is never clobbered by a later disagreeing offer"
        );
    }

    #[test]
    fn a_dropped_key_leaves_the_slot_and_a_sibling_record_never_resurrects_it() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let account = ActorKeypair::generate();
        let (first, writer_pub) = slot_on(tmp.path(), &account);
        let shredded_id = [0xEE; 32];
        first.record_generation_key(&shredded_id, &GenerationKey::mint());
        // A sibling process resolves while the key is live — its in-memory
        // view now holds it.
        let second = resolve_again(tmp.path(), &account, &writer_pub);
        assert!(second.retained_generation_key(&shredded_id).is_some());
        // The first process observes the shred and drops (crypto-shred,
        // device-side half).
        first.drop_generation_key(&shredded_id);
        assert!(first.retained_generation_key(&shredded_id).is_none());
        // The sibling then records an unrelated key. Its stale in-memory copy
        // of the dropped key must NOT leak back into the slot — replace, not
        // merge-back ([`PrincipalSlot::record_generation_key`] docs).
        second.record_generation_key(&[0xEF; 32], &GenerationKey::mint());
        assert!(
            second.retained_generation_key(&shredded_id).is_none(),
            "the sibling's view converges to the drop"
        );
        let third = resolve_again(tmp.path(), &account, &writer_pub);
        assert!(
            third.retained_generation_key(&shredded_id).is_none(),
            "the shredded generation's key never resurrects in the slot"
        );
        assert_eq!(third.status().retained_generations, 1);
    }

    #[test]
    fn a_corrupt_retained_record_reads_as_empty_and_recovers_on_next_record() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let account = ActorKeypair::generate();
        let (slot, writer_pub) = slot_on(tmp.path(), &account);
        drop(slot);
        let credentials =
            CredentialStore::with_file_backend("fauna-account-store", tmp.path().join("creds"));
        let hex_account = fauna_core::hex32::encode(&account.actor_id().0);
        credentials.set(&attr(&hex_account, ATTR_GENERATION_KEYS), "not-hex!");
        let second = resolve_again(tmp.path(), &account, &writer_pub);
        assert_eq!(second.status().retained_generations, 0, "corrupt → empty");
        let key = GenerationKey::mint();
        second.record_generation_key(&[0xDD; 32], &key);
        let third = resolve_again(tmp.path(), &account, &writer_pub);
        assert_eq!(
            third.status().retained_generations,
            1,
            "the record self-heals on the next obtain"
        );
    }

    /// The carriage never hands the credential store a value the tightest
    /// backend would silently refuse — and the capacity is *measured* here
    /// rather than computed, because the encoding is the thing that decides it.
    ///
    /// Mutation: delete the trim loop in `persist_retained` and this reds on
    /// the length assert.
    #[test]
    fn the_retained_carriage_is_bounded_by_the_credential_item_cap() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let account = ActorKeypair::generate();
        let (slot, writer_pub) = slot_on(tmp.path(), &account);
        let hex_account = fauna_core::hex32::encode(&account.actor_id().0);
        let credentials =
            CredentialStore::with_file_backend("fauna-account-store", tmp.path().join("creds"));
        // Measure the SLOT, never `status()` — the latter reports this
        // process's in-memory view, which by design holds the entry just
        // obtained whether or not the carriage could take it.
        let carriage = || {
            let raw = credentials
                .get(&attr(&hex_account, ATTR_GENERATION_KEYS))
                .unwrap_or_default();
            let entries = if raw.is_empty() {
                0
            } else {
                canonical_decode::<RetainedGenerationKeysRecord>(&hex::decode(&raw).expect("hex"))
                    .expect("decodes")
                    .entries
                    .len()
            };
            (raw.len(), entries)
        };

        // Every byte high, and the keys FIXED rather than minted. The ids and
        // keys ride as byte strings, so their width no longer depends on the
        // byte values; the fixed high bytes stay because under the retired
        // integer-array spelling a minted key made the encoded size — and so
        // the capacity — a distribution rather than a number, and a
        // regression to that spelling must still measure at the top of the
        // range.
        let mut measured_at = Vec::new();
        for n in 1u16..=64 {
            let mut id = [0xE0u8; 32];
            id[0] = (n >> 8) as u8 | 0x80;
            id[1] = (n & 0xFF) as u8 | 0x80;
            slot.record_generation_key(&id, &GenerationKey::from_bytes([0xC3; 32]));
            let (len, entries) = carriage();
            measured_at.push((len, entries));
            assert!(
                len <= fauna_credential_store::MAX_ITEM_VALUE_BYTES,
                "after {n} obtains the slot value is {len} bytes, over the {} cap — \
                 the carriage handed the store a value Windows Credential Manager \
                 would drop on the floor",
                fauna_credential_store::MAX_ITEM_VALUE_BYTES,
            );
        }

        // The measurement the bound is documented from (item 1 of the finding
        // asked for this rather than more arithmetic).
        let per_entry = measured_at[1].0 - measured_at[0].0;
        let capacity = measured_at
            .iter()
            .map(|(_, entries)| *entries)
            .max()
            .expect("measurements");
        println!(
            "measured: {per_entry} hex bytes per retained generation; \
             {capacity} generations fit the {} byte item cap",
            fauna_credential_store::MAX_ITEM_VALUE_BYTES
        );
        assert!(
            capacity >= 15,
            "capacity collapsed to {capacity} generations — the record shape grew"
        );

        // Trimmed, not corrupted: the carriage still decodes, and a cold
        // re-resolve loads exactly what it carries.
        let (_, carried_now) = carriage();
        let reread = resolve_again(tmp.path(), &account, &writer_pub);
        assert_eq!(
            reread.status().retained_generations,
            carried_now,
            "a cold slot loads exactly the trimmed carriage"
        );
    }

    /// **A capacity trim never costs this process a key it obtained.** The
    /// carriage holds about fifteen generations; an escrow recovery records
    /// every generation the account has, one at a time. Rebuilding the
    /// in-memory view from the trimmed slot on each record kept only the
    /// slot's fifteen plus the newest, so a recovery of forty-five left the
    /// tip unkeyed: the re-presenting reconcile could not seal, the unkeyed
    /// hold stood, and the next tip-sealed write minted one generation more
    /// (measured live on a remote box, 2026-10-05: `Recovered(45)`, then
    /// `no candidate generation tip resolves`, then every gated read refused).
    ///
    /// Mutation: rebuild the view from the trimmed merge alone in
    /// `record_generation_key` and this reds on the first early id.
    #[test]
    fn every_key_this_process_obtained_stays_usable_past_the_carriage_cap() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let account = ActorKeypair::generate();
        let (slot, _writer_pub) = slot_on(tmp.path(), &account);
        let ids: Vec<[u8; 32]> = (1u8..=48)
            .map(|n| {
                let mut id = [0x40u8; 32];
                id[0] = n;
                id
            })
            .collect();
        for id in &ids {
            slot.record_generation_key(id, &GenerationKey::from_bytes([id[0]; 32]));
        }
        for id in &ids {
            assert_eq!(
                slot.retained_generation_key(id).map(|k| *k.as_bytes()),
                Some([id[0]; 32]),
                "generation {} was obtained by this process and must stay usable here",
                id[0]
            );
        }
        assert_eq!(slot.status().retained_generations, ids.len());
    }

    /// A capacity trim drops some *other* generation, never the one whose
    /// obtain motivated the write — the newly obtained key is the one this
    /// device is actively using.
    ///
    /// The ids are chosen so the motivating entry sorts FIRST, i.e. it is
    /// exactly what the id-ordered trim would evict if it were not protected.
    /// Mutation: pass `None` instead of `Some(generation)` from
    /// `record_generation_key` and this reds.
    #[test]
    fn a_capacity_trim_never_drops_the_generation_that_motivated_the_write() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let account = ActorKeypair::generate();
        let (slot, _writer_pub) = slot_on(tmp.path(), &account);

        // Fill well past the cap with high-sorting ids. Fixed keys, so the
        // carriage is provably AT capacity when the motivating entry arrives —
        // with minted keys the encoded size varies enough that the slot
        // sometimes sits one entry short, no trim fires, and this passes
        // whether or not `keep` is honored (measured: 2 green of 6 runs of its
        // own mutation).
        for n in 0u8..64 {
            let mut id = [0xF0u8; 32];
            id[1] = n | 0x80;
            slot.record_generation_key(&id, &GenerationKey::from_bytes([0xC3; 32]));
        }
        let hex_account = fauna_core::hex32::encode(&account.actor_id().0);
        let entries_when_full = {
            let credentials =
                CredentialStore::with_file_backend("fauna-account-store", tmp.path().join("creds"));
            let raw = credentials
                .get(&attr(&hex_account, ATTR_GENERATION_KEYS))
                .expect("a persisted record");
            canonical_decode::<RetainedGenerationKeysRecord>(&hex::decode(&raw).expect("hex"))
                .expect("decodes")
                .entries
                .len()
        };

        // Now obtain one whose id sorts below every entry already carried, so
        // an id-ordered trim that ignored `keep` would evict exactly this one.
        let lowest = [0x00u8; 32];
        let key = GenerationKey::from_bytes([0xD7; 32]);
        slot.record_generation_key(&lowest, &key);

        let carried = slot
            .retained_generation_key(&lowest)
            .expect("the just-obtained generation is still in memory");
        assert_eq!(carried.as_bytes(), key.as_bytes());

        // And it is the *slot* that must hold it — that is what carriage means.
        let credentials =
            CredentialStore::with_file_backend("fauna-account-store", tmp.path().join("creds"));
        let raw = credentials
            .get(&attr(&hex_account, ATTR_GENERATION_KEYS))
            .expect("a persisted record");
        let decoded: RetainedGenerationKeysRecord =
            canonical_decode(&hex::decode(&raw).expect("hex")).expect("decodes");
        // Precondition, asserted rather than assumed: the carriage was full, so
        // taking the new entry REQUIRED evicting something. Without this the
        // test passes vacuously the moment the record shape changes and the set
        // stops sitting at capacity.
        assert_eq!(
            decoded.entries.len(),
            entries_when_full,
            "the carriage was not at capacity, so no trim was forced and this \
             test proves nothing about `keep`"
        );
        assert!(
            decoded.entries.iter().any(|e| e.generation == lowest),
            "the trim evicted the very generation the write was for"
        );
        assert!(
            raw.len() <= fauna_credential_store::MAX_ITEM_VALUE_BYTES,
            "still within the item cap"
        );
    }

    /// A capability host's slot custody reads the carriage fresh: a generation
    /// the runtime's bundle records AFTER the host's custody was built is
    /// served at the host's next consult; the host's own record lands in the
    /// slot, where a freshly resolved bundle carries it; a drop leaves it; and
    /// every carriage write moves the slot-write watch the app's mirror rides.
    #[test]
    fn a_hosts_slot_custody_reads_the_carriage_fresh_and_records_into_it() {
        use fauna_account_plane::generation_tip::RetainedKeyCustody;

        let tmp = tempfile::tempdir().unwrap();
        let account = ActorKeypair::generate();
        let hex_account = fauna_core::hex32::encode(&account.actor_id().0);
        let (runtime, writer_pub) = slot_on(tmp.path(), &account);
        let host = SlotRetainedKeys::over(
            std::sync::Arc::new(CredentialStore::with_file_backend(
                "fauna-account-store",
                tmp.path().join("creds"),
            )),
            hex_account.clone(),
            tmp.path().join("store"),
        );

        let recovered = ([0x31; 32], GenerationKey::from_bytes([0x41; 32]));
        assert!(host.retained_generation_key(&recovered.0).is_none());
        runtime.record_generation_key(&recovered.0, &recovered.1);
        assert_eq!(
            host.retained_generation_key(&recovered.0)
                .map(|k| *k.as_bytes()),
            Some(*recovered.1.as_bytes()),
            "the runtime's later record reaches the host's next consult"
        );

        let mut writes = slot_writes();
        let before = *writes.borrow_and_update();
        let unwrapped = ([0x32; 32], GenerationKey::from_bytes([0x42; 32]));
        host.record_generation_key(&unwrapped.0, &unwrapped.1);
        assert_ne!(*writes.borrow(), before, "a carriage write moves the watch");
        let relaunched = resolve_again(tmp.path(), &account, &writer_pub);
        for (generation, key) in [&recovered, &unwrapped] {
            assert_eq!(
                relaunched
                    .retained_generation_key(generation)
                    .map(|k| *k.as_bytes()),
                Some(*key.as_bytes()),
                "both the runtime's and the host's records ride the slot"
            );
        }
        let mirrored: Vec<_> =
            bundle::retained_generation_keys(&*tmp_credentials(tmp.path()), &hex_account)
                .into_iter()
                .map(|(g, _)| g)
                .collect();
        assert_eq!(mirrored, vec![recovered.0, unwrapped.0]);

        host.drop_generation_key(&unwrapped.0);
        assert!(host.retained_generation_key(&unwrapped.0).is_none());
        assert!(
            resolve_again(tmp.path(), &account, &writer_pub)
                .retained_generation_key(&unwrapped.0)
                .is_none(),
            "the host's drop leaves the slot"
        );
    }

    fn tmp_credentials(dir: &std::path::Path) -> std::sync::Arc<CredentialStore> {
        std::sync::Arc::new(CredentialStore::with_file_backend(
            "fauna-account-store",
            dir.join("creds"),
        ))
    }
}
