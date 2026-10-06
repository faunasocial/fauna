//! The sync device id on a secret-store app: persisted per account, derived
//! from an install-scoped secret when the account has none.
//!
//! Owner: `docs/goal/architecture/apps/sync-agent-credentials.md` § Credential
//! model, the 2026-09-20 ruling — the id an app registers under is
//! [`fauna_core::device_id::derive_device_id`]`(install_secret, actor_id)`.
//! This is the secret-store twin of the file-backed
//! `fauna_sync_engine::engine_lifecycle::load_device_id_for_actor`: the same
//! three rules over a [`SecretStore`] instead of a `device.db`.

use crate::{AccountRegistry, IndexState, SecretStore, device_id_key, secret_key};

/// Logical key of the **install device secret** — 32 bytes, lowercase hex.
///
/// Install-scoped: it names no account, and no erase in this crate names it
/// ([`AccountRegistry::clear_all`], [`AccountRegistry::remove`]), so it
/// survives the sign-out that takes every per-account id slot. A platform
/// whose sign-out ALSO wipes its whole store wholesale (android's
/// `SecureStorage.clear()`) keeps it in a second store that wipe does not
/// reach — which is why [`AccountRegistry::device_id_for_actor`] takes the
/// install store as its own argument rather than reading `self`'s.
pub const INSTALL_DEVICE_SECRET: &str = "install/device_secret";

/// Why no device id could be produced. The caller logs it and stays on its
/// "no device id" branch — never on an id that did not persist.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeviceIdError {
    /// The actor id is not 32-byte hex, so there is nothing to derive from.
    #[error("invalid actor id: {0}")]
    InvalidActor(String),
    /// The minted install secret did not read back — the store dropped the
    /// write. Deriving from an unpersisted secret would hand out an id the next
    /// read cannot reproduce.
    #[error("the install device secret did not persist")]
    SecretNotPersisted,
    /// The stored install secret is not 32-byte hex.
    #[error("the stored install device secret is not 32-byte hex")]
    SecretMalformed,
}

impl AccountRegistry {
    /// Read (or derive + persist) the sync device id `actor_id` registers
    /// under on this install.
    ///
    /// **A persisted id always wins** (the ruling's property (3)), so no install
    /// re-registers at upgrade and the e2e session door's forced id keeps
    /// working — where "id" means a sync-shaped value (32-byte hex): anything
    /// else in a slot never registered a row, and is replaced. In order:
    ///
    /// 1. The per-account slot `fauna/{actor}/device_id` — the account-scoped
    ///    cache every erase takes.
    /// 2. Derive from the install secret in `install`, minting it on first use
    ///    ([`Self::ensure_install_device_secret`]).
    ///
    /// Whatever step answers is persisted into the account's own slot when the
    /// account is materialized. An account this registry does not hold gets
    /// its id **unpersisted**: a wiped identity stays wiped, and an identity
    /// the wizard has not registered yet carries its id into the registration
    /// (`add_account` / `persist_logged_in`). The derivation is deterministic,
    /// so an unpersisted answer is still the stable one.
    ///
    /// **The lookup never touches any slot but the looked-up account's own.**
    /// A lookup that decided "this account is active" and then touched a
    /// shared slot could be overtaken by a switch between the two steps, and
    /// android's and web's registries have no lock that spans a switch
    /// (android's mutation lock is a no-op; web's cross-tab lock is async and
    /// this call is not); touching only the account's own slot is safe
    /// whatever the interleaving.
    ///
    /// A mutator: it takes the registry's [`crate::MutationLock`], which also
    /// serializes the install-secret mint across processes.
    pub fn device_id_for_actor(
        &self,
        install: &dyn SecretStore,
        actor_id: &str,
    ) -> Result<String, DeviceIdError> {
        let actor = fauna_core::hex32::decode(actor_id)
            .map_err(|_| DeviceIdError::InvalidActor(actor_id.to_string()))?;
        let _mutation = self.lock.acquire();
        if let Some(id) = self
            .store
            .get(&device_id_key(actor_id))
            .filter(|id| is_sync_id(id))
        {
            return Ok(id);
        }
        let secret = install_device_secret_locked(install)?;
        let id =
            fauna_core::hex32::encode(&fauna_core::device_id::derive_device_id(&secret, &actor));
        self.persist_device_id_locked(actor_id, &id);
        Ok(id)
    }

    /// Read-or-mint the install device secret in `install`, without deriving
    /// anything. Web runs it at boot inside its cross-tab mutation lock
    /// (`WasmAccountRegistry::ensureInstallDeviceSecret` under
    /// `with_web_mutation_lock`, from `accountsBoot`), so the synchronous
    /// get-or-create a page reaches later only ever reads the secret — the Web
    /// Locks API is async and [`Self::device_id_for_actor`] is not.
    pub fn ensure_install_device_secret(
        &self,
        install: &dyn SecretStore,
    ) -> Result<(), DeviceIdError> {
        let _mutation = self.lock.acquire();
        install_device_secret_locked(install).map(|_| ())
    }

    /// Cache `id` where the account's own erase will find it. Held under the
    /// mutation lock. Writes only for an account this registry holds — never a
    /// slot for an identity a wipe already took, or one not registered yet.
    fn persist_device_id_locked(&self, actor_id: &str, id: &str) {
        if let IndexState::Readable(idx) = self.index_state()
            && idx.position(actor_id).is_some()
            && self.store.get(&secret_key(actor_id)).is_some()
        {
            self.store.set(&device_id_key(actor_id), id);
        }
    }
}

/// Whether a persisted value is a sync device id at all: 32-byte hex, the only
/// shape the nest registers (`fauna_core::hex32::decode`). A registry slot is
/// free-form by contract, and a value of any other shape never registered a
/// row, so it is no persisted id to honour — deriving over it re-registers
/// nothing.
fn is_sync_id(value: &str) -> bool {
    fauna_core::hex32::is_hex64(value)
}

/// Serializes the install-secret mint between threads of one process — the
/// registry's [`crate::MutationLock`] is a no-op where the app never made it a
/// file lock (android, web), and even a file lock only narrows, never proves,
/// exclusion (it degrades open on I/O failure).
static INSTALL_SECRET_MINT: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Read-or-mint the install device secret. Called with the registry's
/// mutation lock held.
///
/// [`SecretStore`] has no create-only write, so the mint reads back what it
/// wrote and derives from THAT: a writer that raced past a degraded lock and
/// landed last is the secret every caller ends on, and a dropped write is an
/// error rather than an id nothing can reproduce.
fn install_device_secret_locked(
    install: &dyn SecretStore,
) -> Result<[u8; fauna_core::device_id::INSTALL_DEVICE_SECRET_LEN], DeviceIdError> {
    let _mint = INSTALL_SECRET_MINT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let decode =
        |hex: String| fauna_core::hex32::decode(&hex).map_err(|_| DeviceIdError::SecretMalformed);
    if let Some(stored) = install.get(INSTALL_DEVICE_SECRET) {
        return decode(stored);
    }
    let minted = fauna_core::device_id::mint_install_device_secret();
    install.set(INSTALL_DEVICE_SECRET, &fauna_core::hex32::encode(&minted));
    install
        .get(INSTALL_DEVICE_SECRET)
        .ok_or(DeviceIdError::SecretNotPersisted)
        .and_then(decode)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::InMemorySecretStore;

    const SECRET_A: &str = "0101010101010101010101010101010101010101010101010101010101010101";
    const SECRET_B: &str = "0202020202020202020202020202020202020202020202020202020202020202";

    // Persisted ids are sync-shaped (32-byte hex) — the only shape a nest ever
    // registered, so the only shape that outranks the derivation.
    const FORCED: &str = "f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0";
    const A_DEVICE: &str = "c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3";
    const ITS_OWN: &str = "b4b4b4b4b4b4b4b4b4b4b4b4b4b4b4b4b4b4b4b4b4b4b4b4b4b4b4b4b4b4b4b4";

    /// A registry slot is free-form by contract (apple's has held a value the
    /// account runtime refuses as "not 32 bytes"), and a value that is not a
    /// sync id never registered a row — the nest refuses it. It is no
    /// persisted id to honour: the derivation replaces it.
    #[test]
    fn a_persisted_value_that_is_not_a_sync_id_is_replaced_by_the_derivation() {
        let (reg, store, install) = install();
        let a = reg.add_account(SECRET_A, None, Some("free-form")).unwrap();
        let id = reg.device_id_for_actor(&install, &a).unwrap();
        assert!(fauna_core::hex32::is_lowercase_hex64(&id), "got {id:?}");
        assert_eq!(store.get(&device_id_key(&a)).as_deref(), Some(id.as_str()));
    }

    fn actor_of(secret: &str) -> String {
        fauna_core::identity::ActorKeypair::from_secret_hex(secret)
            .unwrap()
            .actor_id_hex()
    }

    /// One install: the registry's store, and a separate install store the
    /// sign-out never touches (android's shape; web passes the same store).
    fn install() -> (
        AccountRegistry,
        Arc<InMemorySecretStore>,
        InMemorySecretStore,
    ) {
        let store = Arc::new(InMemorySecretStore::new());
        (
            AccountRegistry::new(store.clone()),
            store,
            InMemorySecretStore::new(),
        )
    }

    /// The row's definition of success, and the ruling's whole point: the
    /// sign-in after a sign-out comes back to the SAME named row.
    #[test]
    fn a_sign_out_then_sign_in_comes_back_to_the_same_device_id() {
        let (reg, store, install) = install();
        let a = reg.add_account(SECRET_A, None, None).unwrap();
        let first = reg.device_id_for_actor(&install, &a).unwrap();

        let sweep = reg.clear_all();
        assert!(
            sweep.survivors.is_empty(),
            "the erase left {:?}",
            sweep.survivors
        );
        assert_eq!(
            store.get(crate::INDEX_KEY),
            None,
            "signed out: no account left"
        );

        reg.add_account(SECRET_A, None, None).unwrap();
        assert_eq!(reg.device_id_for_actor(&install, &a).unwrap(), first);
    }

    /// Property (2): two accounts on one install never share an id.
    #[test]
    fn two_accounts_on_one_install_never_share_a_device_id() {
        let (reg, _store, install) = install();
        let a = reg.add_account(SECRET_A, None, None).unwrap();
        let b = reg.add_account(SECRET_B, None, None).unwrap();
        assert_ne!(
            reg.device_id_for_actor(&install, &a).unwrap(),
            reg.device_id_for_actor(&install, &b).unwrap()
        );
    }

    /// The id IS the shared derivation — so the file-backed apps (linux, tui)
    /// and the secret-store apps compute one function, not two.
    #[test]
    fn the_id_is_the_shared_derivation_over_the_install_secret() {
        let (reg, _store, install) = install();
        let a = reg.add_account(SECRET_A, None, None).unwrap();
        let id = reg.device_id_for_actor(&install, &a).unwrap();

        let secret =
            fauna_core::hex32::decode(&install.get(INSTALL_DEVICE_SECRET).unwrap()).unwrap();
        let actor = fauna_core::hex32::decode(&a).unwrap();
        assert_eq!(
            id,
            fauna_core::hex32::encode(&fauna_core::device_id::derive_device_id(&secret, &actor))
        );
    }

    /// The per-account slot is the cache: written for a materialized account
    /// and erased by the sign-out like every other per-account slot.
    #[test]
    fn the_derived_id_is_cached_in_the_per_account_slot() {
        let (reg, store, install) = install();
        let a = reg.add_account(SECRET_A, None, None).unwrap();
        let b = reg.add_account(SECRET_B, None, None).unwrap();
        let id_a = reg.device_id_for_actor(&install, &a).unwrap();
        let id_b = reg.device_id_for_actor(&install, &b).unwrap();

        assert_eq!(
            store.get(&device_id_key(&a)).as_deref(),
            Some(id_a.as_str())
        );
        assert_eq!(
            store.get(&device_id_key(&b)).as_deref(),
            Some(id_b.as_str())
        );

        let _ = reg.clear_all();
        assert_eq!(store.get(&device_id_key(&a)), None);
        assert!(
            install.get(INSTALL_DEVICE_SECRET).is_some(),
            "the install secret is install-scoped: the sign-out must not take it"
        );
    }

    /// Web keeps the install secret in the SAME store as the registry; the
    /// sign-out's `clear_all` must still leave it.
    #[test]
    fn clear_all_never_names_the_install_secret_in_a_shared_store() {
        let store = Arc::new(InMemorySecretStore::new());
        let reg = AccountRegistry::new(store.clone());
        let a = reg.add_account(SECRET_A, None, None).unwrap();
        let first = reg.device_id_for_actor(store.as_ref(), &a).unwrap();

        let sweep = reg.clear_all();
        assert!(
            sweep.survivors.is_empty(),
            "the erase left {:?}",
            sweep.survivors
        );
        reg.add_account(SECRET_A, None, None).unwrap();
        assert_eq!(reg.device_id_for_actor(store.as_ref(), &a).unwrap(), first);
    }

    /// Property (3): a persisted id wins over the derivation.
    #[test]
    fn a_persisted_device_id_outranks_the_derivation() {
        let (reg, _store, install) = install();
        let a = reg.add_account(SECRET_A, None, Some(FORCED)).unwrap();
        assert_eq!(reg.device_id_for_actor(&install, &a).unwrap(), FORCED);
        assert_eq!(
            install.get(INSTALL_DEVICE_SECRET),
            None,
            "no mint was needed"
        );
    }

    /// An identity the append wizard has not registered yet must not inherit
    /// the active account's id (android's `completeAddAccount` once registered
    /// every added account under the active one's id).
    #[test]
    fn an_identity_mid_append_never_inherits_the_active_accounts_id() {
        let (reg, store, install) = install();
        let a = reg.add_account(SECRET_A, None, Some(A_DEVICE)).unwrap();
        let b = actor_of(SECRET_B);

        let id_b = reg.device_id_for_actor(&install, &b).unwrap();
        assert_ne!(id_b, A_DEVICE);
        assert_eq!(
            store.get(&device_id_key(&b)),
            None,
            "B is not registered yet: its registration carries the id"
        );
        assert_eq!(reg.device_id_for_actor(&install, &a).unwrap(), A_DEVICE);
    }

    /// A wiped identity stays wiped: a late read for an account the registry
    /// no longer holds persists nothing (`long-term-store.md` § Cleanup
    /// contract).
    #[test]
    fn an_unknown_actor_gets_its_derived_id_without_a_slot_write() {
        let (reg, store, install) = install();
        let a = reg.add_account(SECRET_A, None, None).unwrap();
        let first = reg.device_id_for_actor(&install, &a).unwrap();
        let _ = reg.clear_all();

        assert_eq!(reg.device_id_for_actor(&install, &a).unwrap(), first);
        assert_eq!(store.get(&device_id_key(&a)), None);
    }

    #[test]
    fn a_malformed_actor_id_is_refused() {
        let (reg, _store, install) = install();
        assert!(matches!(
            reg.device_id_for_actor(&install, "not-hex"),
            Err(DeviceIdError::InvalidActor(_))
        ));
    }

    /// A store that drops every write: the mint must not hand out an id
    /// derived from a secret nothing will read back.
    struct DroppingStore;
    impl SecretStore for DroppingStore {
        fn get(&self, _: &str) -> Option<String> {
            None
        }
        fn set(&self, _: &str, _: &str) {}
        fn delete(&self, _: &str) {}
    }

    #[test]
    fn an_install_secret_that_does_not_persist_yields_no_id() {
        let (reg, _store, _) = install();
        let a = reg.add_account(SECRET_A, None, None).unwrap();
        assert_eq!(
            reg.device_id_for_actor(&DroppingStore, &a),
            Err(DeviceIdError::SecretNotPersisted)
        );
    }

    /// `SecretStore` has no create-only write, so the mint reads back what it
    /// wrote: a racing writer whose value landed last (past a lock that
    /// degraded open) is the one both sides derive from.
    struct LastWriterStore {
        inner: InMemorySecretStore,
        racer: &'static str,
    }
    impl SecretStore for LastWriterStore {
        fn get(&self, key: &str) -> Option<String> {
            self.inner.get(key)
        }
        fn set(&self, key: &str, value: &str) {
            self.inner.set(key, value);
            if key == INSTALL_DEVICE_SECRET {
                self.inner.set(key, self.racer);
            }
        }
        fn delete(&self, key: &str) {
            self.inner.delete(key);
        }
    }

    #[test]
    fn the_mint_derives_from_the_secret_that_read_back_not_the_one_it_wrote() {
        let (reg, _store, _) = install();
        let a = reg.add_account(SECRET_A, None, None).unwrap();
        let racer = "0909090909090909090909090909090909090909090909090909090909090909";
        let install = LastWriterStore {
            inner: InMemorySecretStore::new(),
            racer,
        };
        let id = reg.device_id_for_actor(&install, &a).unwrap();
        let expected = fauna_core::device_id::derive_device_id(
            &fauna_core::hex32::decode(racer).unwrap(),
            &fauna_core::hex32::decode(&a).unwrap(),
        );
        assert_eq!(id, fauna_core::hex32::encode(&expected));
    }

    #[test]
    fn a_malformed_install_secret_is_an_error_not_a_re_mint() {
        let (reg, _store, install) = install();
        install.seed(INSTALL_DEVICE_SECRET, "garbage");
        let a = reg.add_account(SECRET_A, None, None).unwrap();
        assert_eq!(
            reg.device_id_for_actor(&install, &a),
            Err(DeviceIdError::SecretMalformed)
        );
        assert_eq!(
            install.get(INSTALL_DEVICE_SECRET).as_deref(),
            Some("garbage")
        );
    }

    #[test]
    fn ensure_install_device_secret_mints_once() {
        let (reg, _store, install) = install();
        reg.ensure_install_device_secret(&install).unwrap();
        let first = install.get(INSTALL_DEVICE_SECRET).expect("minted");
        assert!(fauna_core::hex32::is_lowercase_hex64(&first));
        reg.ensure_install_device_secret(&install).unwrap();
        assert_eq!(install.get(INSTALL_DEVICE_SECRET), Some(first));
    }

    /// Two threads of one process racing the first read (android's workers and
    /// view models; its registry has no cross-process lock) end on ONE secret.
    #[test]
    fn concurrent_first_reads_in_one_process_mint_one_secret() {
        let (reg, _store, _) = install();
        let a = reg.add_account(SECRET_A, None, None).unwrap();
        let b = reg.add_account(SECRET_B, None, None).unwrap();
        let install = Arc::new(SlowStore::default());
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let (reg, install) = (reg.clone(), install.clone());
                let actor = if i % 2 == 0 { a.clone() } else { b.clone() };
                std::thread::spawn(move || {
                    let _ = reg.device_id_for_actor(install.as_ref(), &actor).unwrap();
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(
            install.mints(),
            1,
            "every thread must derive from one secret"
        );
    }

    /// An in-memory store that yields between its read and its write, so an
    /// unserialized read-then-write race actually interleaves.
    #[derive(Default)]
    struct SlowStore {
        inner: InMemorySecretStore,
        mints: std::sync::atomic::AtomicUsize,
    }
    impl SlowStore {
        fn mints(&self) -> usize {
            self.mints.load(std::sync::atomic::Ordering::SeqCst)
        }
    }
    impl SecretStore for SlowStore {
        fn get(&self, key: &str) -> Option<String> {
            let v = self.inner.get(key);
            std::thread::yield_now();
            v
        }
        fn set(&self, key: &str, value: &str) {
            if key == INSTALL_DEVICE_SECRET {
                self.mints.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            self.inner.set(key, value);
        }
        fn delete(&self, key: &str) {
            self.inner.delete(key);
        }
    }

    // ── A switch racing a lookup ──────────────────────────────────────────────
    //
    // android's and web's registries carry no cross-process mutation lock, and
    // web's cross-tab lock cannot be taken from the synchronous lookup, so a
    // switch in another tab (or thread) can land between any two of a lookup's
    // store operations. These drive exactly that: `Interleave` runs the other
    // tab's switch once, right after one named store operation.

    #[derive(Clone, Copy, PartialEq)]
    enum Op {
        Get,
        Set,
    }

    struct Interleave {
        inner: InMemorySecretStore,
        op: Op,
        key: String,
        other_tab: std::sync::OnceLock<Box<dyn Fn() + Send + Sync>>,
        fired: std::sync::atomic::AtomicBool,
    }

    impl Interleave {
        fn after(op: Op, key: String) -> Arc<Self> {
            Arc::new(Self {
                inner: InMemorySecretStore::new(),
                op,
                key,
                other_tab: std::sync::OnceLock::new(),
                fired: std::sync::atomic::AtomicBool::new(false),
            })
        }

        fn registry(self: &Arc<Self>) -> AccountRegistry {
            AccountRegistry::new(self.clone())
        }

        /// Arm the other tab's switch to `actor_id` (`set_active`, the step
        /// web's `accountsSwitch` and android's switch run).
        fn switch_to(self: &Arc<Self>, actor_id: &str) {
            let other_tab = self.registry();
            let actor_id = actor_id.to_string();
            let _ = self.other_tab.set(Box::new(move || {
                other_tab.set_active(&actor_id).unwrap();
            }));
        }

        fn maybe_fire(&self, op: Op, key: &str) {
            if op == self.op
                && key == self.key
                && let Some(switch) = self.other_tab.get()
                && !self.fired.swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                switch();
            }
        }
    }

    impl SecretStore for Interleave {
        fn get(&self, key: &str) -> Option<String> {
            let value = self.inner.get(key);
            self.maybe_fire(Op::Get, key);
            value
        }
        fn set(&self, key: &str, value: &str) {
            self.inner.set(key, value);
            self.maybe_fire(Op::Set, key);
        }
        fn delete(&self, key: &str) {
            self.inner.delete(key);
        }
    }

    /// A's lookup decides "A is active", a switch to B lands, then A's lookup
    /// finishes. B must never end up presenting — or persisting — A's id.
    #[test]
    fn a_switch_landing_mid_lookup_never_leaves_the_outgoing_accounts_id_to_the_incoming_one() {
        let a = actor_of(SECRET_A);
        let store = Interleave::after(Op::Set, device_id_key(&a));
        let reg = store.registry();
        let install = InMemorySecretStore::new();
        reg.add_account(SECRET_A, None, None).unwrap();
        let b = reg.add_account(SECRET_B, None, None).unwrap();
        store.switch_to(&b);

        let id_a = reg.device_id_for_actor(&install, &a).unwrap();
        assert!(store.fired.load(std::sync::atomic::Ordering::SeqCst));
        let id_b = reg.device_id_for_actor(&install, &b).unwrap();
        assert_ne!(id_b, id_a);
        assert_eq!(
            store.get(&device_id_key(&b)).as_deref(),
            Some(id_b.as_str())
        );
    }

    /// The mirror image: A's lookup reads its own slot while B is active, a
    /// switch to A lands, and A must not take B's id for its own.
    #[test]
    fn a_switch_landing_mid_lookup_never_hands_the_outgoing_accounts_id_to_the_looked_up_one() {
        let a = actor_of(SECRET_A);
        let store = Interleave::after(Op::Get, device_id_key(&a));
        let reg = store.registry();
        let install = InMemorySecretStore::new();
        reg.add_account(SECRET_A, None, None).unwrap();
        let b = reg.add_account(SECRET_B, None, Some(ITS_OWN)).unwrap();
        reg.set_active(&b).unwrap();
        store.switch_to(&a);

        let id_a = reg.device_id_for_actor(&install, &a).unwrap();
        assert!(store.fired.load(std::sync::atomic::Ordering::SeqCst));
        assert_ne!(id_a, ITS_OWN);
        assert_ne!(
            store.get(&device_id_key(&a)).as_deref(),
            Some(ITS_OWN),
            "B's id must never be persisted as A's"
        );
    }
}
