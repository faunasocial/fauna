//! Principal succession, natively — the ceremony-time probe, the in-place
//! writer rotation, the lost-slot heal and the un-pushed-tail re-author
//! (owner: `account-replica-posture.md` § The store device
//! principal → *Principal succession after a device delete*, refinements 10
//! and 11).
//!
//! The mechanics are platform-generic and live in
//! `fauna_account_plane::principal_succession`, whose module doc is the
//! description of every trigger and of what the rotation deliberately
//! never does; web's host runs the same two steps in the same order. This
//! module instantiates them over the native slot — the platform
//! `CredentialStore`, the store dir's `migration.lock` as the section
//! ([`MigrationSection`]) and `SqliteBackend` — and keeps every name at its
//! old path, so the native worker's assembly calls them unchanged.

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use ed25519_dalek::SigningKey;
use fauna_account_plane::principal_succession as generic;
use fauna_account_store::sqlite::SqliteBackend;
use fauna_core::identity::ActorKeypair;
use fauna_credential_store::CredentialStore;
use fauna_protocol::{RpcErrorClass, RpcRequester};

use crate::principal_bundle::{MigrationSection, PrincipalSlot};
/// The un-pushed-tail re-author (decision 3) — a store-only pump step, so
/// it lives in the wasm-capable driver's crate; re-exported at its old path.
pub use fauna_account_plane::succession_tail::{ReauthorPass, tail_reauthor_pass};

/// What the ceremony-time succession probe concluded.
pub(crate) use fauna_account_plane::principal_succession::CeremonyProbe;
/// What [`lost_slot_heal`] found and did.
pub(crate) use fauna_account_plane::principal_succession::LostSlotHeal;
/// Where the assembly's writer key came from — the one fact that tells a
/// first launch from the journal-bound writer's inverse shape.
pub(crate) use fauna_account_plane::principal_succession::WriterKeyProvenance;

/// Decision 1's evidence gate over the native slot
/// ([`generic::ceremony_probe`] owns the contract): the section is the store
/// dir's `migration.lock`, and the rotation's backend is the store dir's
/// `SqliteBackend`.
// The native call's own ten inputs.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn ceremony_probe<R>(
    rpc: &R,
    credentials: &Arc<CredentialStore>,
    actor_id_hex: &str,
    store_dir: &Path,
    seed: &ActorKeypair,
    held_writer: &SigningKey,
    slot: &PrincipalSlot,
    enrollment_target: &str,
    allow_rotation: bool,
    own_row_removed: Option<[u8; 32]>,
) -> Result<CeremonyProbe>
where
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass,
{
    generic::ceremony_probe(
        rpc,
        credentials,
        actor_id_hex,
        &MigrationSection::from(store_dir.to_path_buf()),
        || std::future::ready(SqliteBackend::open(store_dir)),
        seed,
        held_writer,
        slot,
        enrollment_target,
        allow_rotation,
        own_row_removed,
    )
    .await
}

/// The lost-slot arm and the journal-bound writer's two arms over the native
/// slot ([`generic::lost_slot_heal`] owns the contract): the section is the
/// store dir's `migration.lock`, entered only on a disagreement.
pub(crate) async fn lost_slot_heal(
    credentials: &Arc<CredentialStore>,
    actor_id_hex: &str,
    store_dir: &Path,
    backend: &SqliteBackend,
    held: &SigningKey,
    provenance: WriterKeyProvenance,
    allow_remint: bool,
) -> Result<LostSlotHeal> {
    generic::lost_slot_heal(
        &**credentials,
        actor_id_hex,
        &MigrationSection::from(store_dir.to_path_buf()),
        backend,
        held,
        provenance,
        allow_remint,
    )
    .await
}

/// A fresh writer minted into the native slot, read-back compared
/// ([`generic::mint_into_slot`]).
#[cfg(test)]
fn mint_into_slot(credentials: &CredentialStore, actor_id_hex: &str) -> Result<SigningKey> {
    generic::mint_into_slot(credentials, actor_id_hex)
}

#[cfg(test)]
use fauna_account_store::types::WriterId;

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_account_plane::principal_bundle as bundle;
    use fauna_account_store::store::AccountStore;
    use fauna_client_accounts::SecretStore;

    use std::sync::Arc;

    fn writer(n: u8) -> WriterId {
        WriterId([n; 32])
    }

    /// **A benign sibling race must not burn the re-mint budget** — the
    /// secondary half of the same finding.
    ///
    /// `RestartAssembly` used to carry two causes in one variant, and its own
    /// doc comment said so: *"a sibling changed it under the section, **or** it
    /// held a RETIRED writer and a fresh successor was minted into it"*. Only
    /// the second mints. The caller sets its cap on the variant, so the first
    /// spent a budget whose declared contract is *"at most ONE fresh mint over
    /// a RETIRED writer found in the slot per runtime worker"* — and once
    /// burned, the genuine retired-writer path (a credential backup older than
    /// a rotation, landed over a current store) takes the refusal instead of
    /// healing, ending the account runtime thread over a race that cost
    /// nothing. Recoverable by relaunch, so a transient strand rather than the
    /// permanent one the heal removed — but a strand nobody asked for.
    ///
    /// Here the slot holds one key and the assembly arrives holding another:
    /// the section's re-read sees the disagreement and yields. Nothing is
    /// minted, so the answer must be the non-minting variant.
    #[tokio::test]
    async fn a_sibling_race_restarts_assembly_without_claiming_a_mint() {
        let dir = tempfile::tempdir().unwrap();
        let store_dir = dir.path().join("store");
        std::fs::create_dir_all(&store_dir).unwrap();
        let actor = "aa11";

        // The slot holds the sibling's key.
        let credentials = Arc::new(CredentialStore::with_file_backend(
            crate::account_runtime::CRED_NAMESPACE,
            dir.path().join("creds"),
        ));
        let siblings_key = mint_into_slot(&credentials, actor).unwrap();

        // The store is stamped with a writer neither key names, so the heal
        // gets past its agreement fast-path and into the section.
        {
            let backend = SqliteBackend::open(&store_dir).unwrap();
            AccountStore::open(backend, actor, writer(0x11))
                .await
                .unwrap();
        }
        let backend = SqliteBackend::open(&store_dir).unwrap();

        // We arrive holding a DIFFERENT key from the one in the slot — the
        // sibling moved it between our resolve and our lock.
        let ours = SigningKey::from_bytes(&[0x77; 32]);
        assert_ne!(
            ours.verifying_key().to_bytes(),
            siblings_key.verifying_key().to_bytes()
        );

        let outcome = lost_slot_heal(
            &credentials,
            actor,
            &store_dir,
            &backend,
            &ours,
            WriterKeyProvenance::Loaded,
            true,
        )
        .await
        .unwrap();

        assert!(
            matches!(outcome, LostSlotHeal::RestartAssembly),
            "a sibling race must answer with the NON-minting restart — answering \
             `RemintedIntoSlot` burns the re-mint cap, and the next genuine \
             retired-writer-in-slot then refuses instead of healing: {outcome:?}"
        );
    }

    /// The slot as the second of two concurrent cold assemblies sees it: the
    /// first read of the unstamped-mint marker is where the sibling's open
    /// lands — it stamps the store with the key it minted and then spends
    /// the marker, both outside the migration section, exactly as the
    /// assembly does after its own heal.
    struct SiblingOpensAtTheMarkerRead {
        slot: Arc<CredentialStore>,
        actor: &'static str,
        sibling: std::sync::Mutex<Option<(SqliteBackend, WriterId)>>,
    }

    impl SecretStore for SiblingOpensAtTheMarkerRead {
        fn get(&self, key: &str) -> Option<String> {
            if key.ends_with("/writer-unstamped")
                && let Some((backend, writer)) = self.sibling.lock().unwrap().take()
            {
                let actor = self.actor;
                // The open is async and this seam is not: its own thread and
                // runtime, joined before the read answers.
                std::thread::scope(|s| {
                    s.spawn(move || {
                        tokio::runtime::Builder::new_current_thread()
                            .build()
                            .unwrap()
                            .block_on(AccountStore::open(backend, actor, writer))
                            .expect("the sibling's open stamps the cold store");
                    })
                    .join()
                    .unwrap();
                });
                bundle::clear_writer_unstamped(&*self.slot, actor);
            }
            self.slot.get(key)
        }
        fn set(&self, key: &str, value: &str) {
            self.slot.set(key, value)
        }
        fn delete(&self, key: &str) {
            self.slot.delete(key)
        }
    }

    /// **The second of two concurrent cold assemblies adopts the stamp it
    /// lost the race to** — § Multi-instance concurrency: two runtimes
    /// assembling at once on one cold store dir must both come up.
    ///
    /// The first assembly minted the writer key (marker set) and has its
    /// backend open; the second LOADED that key over a store nobody has
    /// stamped yet, so its heal enters the section on the loaded-over-fresh
    /// shape. The sibling's open then stamps the store and spends the marker
    /// — two writes outside the section. Read stamp-then-marker, both fit
    /// between the heal's two reads: no stamp, no marker, which is the
    /// lost-journal picture, so the heal minted a second key into the slot
    /// and was refused by the stamp it had just missed ("retire unjournaled
    /// writer: the store is already stamped …") — the assembly failed, and
    /// the slot was left naming a key the store does not.
    ///
    /// Deterministic: the sibling's two writes land inside the heal's marker
    /// read, the one point both read orders pass through.
    #[tokio::test]
    async fn a_sibling_stamping_a_cold_store_under_the_heal_is_adopted_not_reminted() {
        let dir = tempfile::tempdir().unwrap();
        let store_dir = dir.path().join("store");
        std::fs::create_dir_all(&store_dir).unwrap();
        let actor = "aa11";

        let slot = Arc::new(CredentialStore::with_file_backend(
            crate::account_runtime::CRED_NAMESPACE,
            dir.path().join("creds"),
        ));
        // The first assembly's mint, inside its section: key and marker.
        let (minted, provenance) = bundle::mint_or_load_writer_key(&*slot, actor).unwrap();
        assert_eq!(provenance, WriterKeyProvenance::Minted);
        assert!(bundle::writer_unstamped(&*slot, actor));
        let minted_writer = WriterId(minted.verifying_key().to_bytes());

        // Both assemblies have their backends open before either stamps.
        let siblings_backend = SqliteBackend::open(&store_dir).unwrap();
        let backend = SqliteBackend::open(&store_dir).unwrap();
        let credentials = SiblingOpensAtTheMarkerRead {
            slot: Arc::clone(&slot),
            actor,
            sibling: std::sync::Mutex::new(Some((siblings_backend, minted_writer))),
        };

        // The second assembly LOADED the sibling's key.
        let outcome = generic::lost_slot_heal(
            &credentials,
            actor,
            &MigrationSection::from(store_dir.clone()),
            &backend,
            &minted,
            WriterKeyProvenance::Loaded,
            true,
        )
        .await
        .expect(
            "the heal must adopt the stamp its sibling landed under it, never refuse \
             the assembly",
        );

        assert!(
            credentials.sibling.lock().unwrap().is_none(),
            "the sibling's open never ran — the heal did not read the marker"
        );
        assert!(
            matches!(outcome, LostSlotHeal::Consistent),
            "the store carries the very key this assembly holds: {outcome:?}"
        );
        assert_eq!(
            bundle::load_writer_key(&*slot, actor)
                .unwrap()
                .verifying_key()
                .to_bytes(),
            minted_writer.0,
            "no second key was minted into the slot"
        );
        assert_eq!(
            fauna_account_store::store::stamped_writer(&backend)
                .await
                .unwrap(),
            Some(minted_writer),
            "the store keeps the sibling's stamp"
        );
    }
}
