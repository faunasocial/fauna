//! A removed account's credential slots, held in memory for the removal's
//! nest half (`account-scoping.md` § Erasure follows scope → *Removing an
//! account retires this machine from it, as a sign-out does*, decision 1).
//!
//! A remove-account owes the removed account's nest the sign-out's
//! retirement, and that retirement needs exactly the slots the registry
//! removal deletes — the identity secret to sign in with, the device
//! authorization and the store writer key the revoke is made with. The
//! removal is the gesture's one decision point and lands before the first
//! await, so the slots are taken into this value in the same mutation
//! ([`crate::AccountRegistry::remove_with_snapshot`]) and the nest half rides
//! it instead of the registry.
//!
//! Two properties are the reason this is a type and not a map:
//!
//! * **Nothing written here reaches the registry.** It is a [`SecretStore`]
//!   of its own, so the runtime assembled over it — which mints, re-keys and
//!   clears slots as any assembly does — writes into memory. An assembly
//!   over the registry's own namespaces could re-create a slot the removal
//!   just deleted: the ⚠ *async writer* case of the same section.
//! * **Every value is zeroized when it goes** — overwritten, deleted, or
//!   dropped with the value. The ruling refused a durable retry record for
//!   this reason: a removed account's credential kept on the device is the
//!   one thing the gesture exists to end, and that holds for memory too.

use std::collections::HashMap;
use std::sync::Mutex;

use fauna_core::secret::SecretString;

use crate::SecretStore;

/// The per-actor slots of one account the registry has just removed, in
/// memory, keyed by the registry's logical keys (every
/// `PER_ACTOR_KEY_BUILDERS` shape — `fauna/{actor}/*`, `{actor}/{suffix}` and
/// the bare `{actor}`).
///
/// One namespace by construction: the registry's own store and its auxiliary
/// erase namespaces never hold the same logical key (their key shapes are
/// disjoint), and where a key reads in both the auxiliary value is taken — it
/// is the account store's namespace, the one the runtime reads.
pub struct RemovedAccountCredentials {
    actor_id: String,
    slots: Mutex<HashMap<String, SecretString>>,
}

impl RemovedAccountCredentials {
    pub(crate) fn new(actor_id: String, slots: HashMap<String, SecretString>) -> Self {
        Self {
            actor_id,
            slots: Mutex::new(slots),
        }
    }

    /// The removed account's actor id, lowercase hex.
    pub fn actor_id(&self) -> &str {
        &self.actor_id
    }

    /// The identity secret the removal took, hex — what the nest half signs in
    /// with. `None` for an account that held none (a removal of a row with no
    /// secret has no nest half to run).
    pub fn secret_hex(&self) -> Option<SecretString> {
        self.read(&crate::secret_key(&self.actor_id))
    }

    /// The nest the removed account was bound to — where the nest half goes.
    /// `None` for an account never bound to one: there is no enrollment to
    /// retire.
    pub fn nest_url(&self) -> Option<String> {
        self.get(&crate::nest_url_key(&self.actor_id))
    }

    /// This machine's sync device id for the removed account, as the
    /// registry's per-account slot held it — the named row the nest half
    /// retires, on the seats that keep the id there (the FFI seats; linux and
    /// tui read theirs from the account-scoped `device.db`). `None` when the
    /// slot held no sync id.
    pub fn device_id(&self) -> Option<String> {
        self.get(&crate::device_id_key(&self.actor_id))
            .filter(|id| fauna_core::hex32::is_hex64(id))
    }

    /// Whether the removal took a device authorization — this machine's
    /// enrollment on the removed account's nest. Without one there is no grant
    /// to retire, and the nest half has nothing to do.
    pub fn holds_device_authorization(&self) -> bool {
        self.lock()
            .contains_key(&crate::device_auth_key(&self.actor_id))
    }

    fn read(&self, key: &str) -> Option<SecretString> {
        self.lock()
            .get(key)
            .map(|value| SecretString::from(value.as_str()))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, SecretString>> {
        // A poisoned map still holds the values; recover them rather than
        // panicking the removal's nest half (best-effort by ruling).
        self.slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl SecretStore for RemovedAccountCredentials {
    fn get(&self, key: &str) -> Option<String> {
        self.lock().get(key).map(|value| value.as_str().to_owned())
    }

    fn set(&self, key: &str, value: &str) {
        // The value it replaces drops here, zeroized.
        self.lock()
            .insert(key.to_owned(), SecretString::from(value));
    }

    fn delete(&self, key: &str) {
        self.lock().remove(key);
    }
}

impl std::fmt::Debug for RemovedAccountCredentials {
    /// The keys and never a value: a log line about the removal must not
    /// carry the credentials it exists to end.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut keys: Vec<String> = self.lock().keys().cloned().collect();
        keys.sort();
        f.debug_struct("RemovedAccountCredentials")
            .field("actor_id", &self.actor_id)
            .field("slots", &keys)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{AccountError, AccountRegistry, InMemorySecretStore, per_actor_keys};

    // Any 32 bytes is a valid Ed25519 secret; built rather than spelled, so no
    // literal reads as a leaked key to the publish scan.
    fn secret_a() -> String {
        "11".repeat(32)
    }
    fn secret_b() -> String {
        "22".repeat(32)
    }
    const NEST: &str = "https://nest.example";

    /// A registry over `store` that also erases `aux` — the shipping shape
    /// (`fauna_credential_store::account_registry`): the account store's slots
    /// live in a namespace the registry does not index.
    fn registry() -> (
        AccountRegistry,
        Arc<InMemorySecretStore>,
        Arc<InMemorySecretStore>,
    ) {
        let store = Arc::new(InMemorySecretStore::new());
        let aux = Arc::new(InMemorySecretStore::new());
        let reg = AccountRegistry::new(store.clone())
            .also_erasing(vec![aux.clone() as Arc<dyn SecretStore>]);
        (reg, store, aux)
    }

    /// Every slot either store holds for `actor`, as `(key, store)` pairs.
    fn held(actor: &str, stores: &[&InMemorySecretStore]) -> Vec<String> {
        per_actor_keys(actor)
            .filter(|k| stores.iter().any(|s| s.get(k).is_some()))
            .collect()
    }

    /// The snapshot holds every slot the removal deleted — the registry's own
    /// (`fauna/{actor}/*`) and the account store's (`{actor}/*`, the bare
    /// writer key) — and the removal itself is the ordinary one: entry gone,
    /// every slot gone from both namespaces, the other account untouched.
    #[test]
    fn the_snapshot_carries_every_slot_the_removal_deletes() {
        let (reg, store, aux) = registry();
        let a = reg.add_account(&secret_a(), None, None).unwrap();
        let b = reg.add_account(&secret_b(), None, None).unwrap();
        reg.set_nest_url(&a, NEST);
        for actor in [&a, &b] {
            aux.set(actor, "writer-key-hex");
            aux.set(&format!("{actor}/device-auth"), "authorization");
            aux.set(&format!("{actor}/grant-registered"), "latch");
        }

        let snapshot = reg.remove_with_snapshot(&a).expect("A is known");

        assert_eq!(snapshot.actor_id(), a);
        assert_eq!(snapshot.secret_hex().as_deref(), Some(secret_a().as_str()));
        assert_eq!(snapshot.nest_url().as_deref(), Some(NEST));
        for (key, want) in [
            (a.clone(), "writer-key-hex"),
            (format!("{a}/device-auth"), "authorization"),
            (format!("{a}/grant-registered"), "latch"),
        ] {
            assert_eq!(snapshot.get(&key).as_deref(), Some(want), "{key}");
        }

        assert!(
            reg.list().iter().all(|e| e.actor_id != a),
            "the entry is removed in the same mutation"
        );
        assert!(
            held(&a, &[&store, &aux]).is_empty(),
            "the removal left {:?}",
            held(&a, &[&store, &aux])
        );
        assert!(
            !held(&b, &[&aux]).is_empty() && reg.list().iter().any(|e| e.actor_id == b),
            "the other account keeps its entry and its slots"
        );
    }

    /// The FFI seats' named row: the per-account device id slot, carried by
    /// the snapshot; a slot holding anything but a sync id reads as none.
    #[test]
    fn the_snapshot_carries_the_per_account_device_id() {
        let (reg, _store, _aux) = registry();
        let id = "ab".repeat(32);
        let a = reg.add_account(&secret_a(), None, Some(&id)).unwrap();
        let b = reg
            .add_account(&secret_b(), None, Some("switcher-b"))
            .unwrap();

        assert_eq!(reg.remove_with_snapshot(&a).unwrap().device_id(), Some(id));
        assert_eq!(reg.remove_with_snapshot(&b).unwrap().device_id(), None);
    }

    /// Decision 1's rule: nothing the nest half writes reaches the registry. A
    /// runtime over the snapshot mints, re-keys and clears slots as any
    /// assembly does — every one of those lands in memory, and a slot the
    /// removal deleted is still absent afterwards.
    #[test]
    fn writes_to_the_snapshot_never_reach_the_registry() {
        let (reg, store, aux) = registry();
        let a = reg.add_account(&secret_a(), None, None).unwrap();
        aux.set(&a, "writer-key-hex");

        let snapshot = reg.remove_with_snapshot(&a).unwrap();
        for key in per_actor_keys(&a) {
            snapshot.set(&key, "written-by-the-nest-half");
        }
        snapshot.delete(&a);

        assert!(
            held(&a, &[&store, &aux]).is_empty(),
            "the nest half re-created {:?}",
            held(&a, &[&store, &aux])
        );
        assert!(reg.list().iter().all(|e| e.actor_id != a));
    }

    /// Where both namespaces hold one logical key, the account store's value is
    /// the one taken — it is the namespace the runtime reads.
    #[test]
    fn the_account_store_namespace_wins_a_shared_key() {
        let (reg, store, aux) = registry();
        let a = reg.add_account(&secret_a(), None, None).unwrap();
        store.set(&format!("{a}/device-auth"), "stale-app-namespace-copy");
        aux.set(&format!("{a}/device-auth"), "account-store-copy");

        let snapshot = reg.remove_with_snapshot(&a).unwrap();

        assert_eq!(
            snapshot.get(&format!("{a}/device-auth")).as_deref(),
            Some("account-store-copy")
        );
    }

    /// The values are held zeroizing (`SecretString`, whose drop wipes its
    /// buffer), and nothing prints them. The first half is a type-level pin —
    /// a change to a plain `String` map stops compiling here.
    #[test]
    fn the_snapshot_is_zeroizing_and_never_prints_a_value() {
        fn values_are_zeroizing(snapshot: &RemovedAccountCredentials) -> usize {
            let map: &HashMap<String, SecretString> = &snapshot.lock();
            map.len()
        }
        let (reg, _store, aux) = registry();
        let a = reg.add_account(&secret_a(), None, None).unwrap();
        aux.set(&a, "writer-key-hex");

        let snapshot = reg.remove_with_snapshot(&a).unwrap();

        assert_eq!(values_are_zeroizing(&snapshot), 2, "the secret and the key");
        let printed = format!("{snapshot:?}");
        assert!(
            !printed.contains(&secret_a()) && !printed.contains("writer-key-hex"),
            "{printed}"
        );
    }

    /// An unknown actor answers as `remove` does and takes nothing.
    #[test]
    fn an_unknown_actor_is_refused_and_nothing_moves() {
        let (reg, _store, _aux) = registry();
        let a = reg.add_account(&secret_a(), None, None).unwrap();

        let refused = reg.remove_with_snapshot(&"ff".repeat(32));

        assert!(matches!(refused, Err(AccountError::UnknownActor(_))));
        assert!(reg.list().iter().any(|e| e.actor_id == a));
    }
}
