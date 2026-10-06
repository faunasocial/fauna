//! The **registry** half of the cross-app E2E bridge.
//!
//! `fauna_onboarding_machine::call_machine_free_method` is this module's twin,
//! for seams that touch only *process-global* state (the TOFU nest-identity pin
//! store). The seams here need the app's own [`AccountRegistry`] — the long-term
//! identity store — which each app builds from its own `SecretStore` and which
//! therefore cannot be reached from a free function the way the pin store can.
//!
//! **Why a dispatcher and not one function per app.** The alternative is a
//! per-app arm reaching into the registry directly, i.e. seven copies of the
//! same slot writes drifting apart (priority #1). Here the *name table and the
//! semantics* live in shared Rust once; an app's agent adds a single delegation
//! that hands over its registry, and every later registry-level seam costs it
//! nothing. That is the same split `persist_logged_in` already makes for the
//! production `LoggedIn` moment: shared logic, app-supplied registry.
//!
//! **Compiled out of release artifacts** (`e2e-conventions.md` § point 15) by
//! the same `cfg` the machine's bridge uses. The runtime `FAUNA_E2E_*` gates are
//! the inner switch *within* a test-capable build, never the boundary.

use crate::{AccountRegistry, SecretStore};
use std::sync::Arc;

/// The reserved store row whose presence makes `refuse_secret_writes_for_test`'s
/// fault live: while it exists, [`AccountRegistry::write_secret_slot`] drops
/// every identity-secret write to that store.
///
/// **Held IN the store, not beside it.** Not on the registry value — every app
/// builds a fresh [`AccountRegistry`] view per operation, so a flag there would
/// die with the view that armed it. Not keyed by the store *object* either:
/// tui keeps one long-lived handle, but apple builds a fresh
/// `KeychainSecretStore` (and the UniFFI seam a fresh bridge) per registry, and
/// web a fresh `LocalStorageSecretStore` per registry in two wasm modules with
/// separate statics. What every app's views share is the store's *backing* —
/// the keychain, the localStorage origin — so that is where the flag lives. It
/// is scoped exactly as far as the backing: another store's writes (the aux
/// namespaces, every parallel unit test in this crate) are untouched.
///
/// Outside every per-actor key's shape (`fauna/<64-hex actor>/…`), so no
/// registry read or erase names it; the caller lifts it when done.
const REFUSE_SECRET_WRITES_KEY: &str = "fauna/e2e/refuse_secret_writes";

/// Whether secret-slot writes to `store` are being refused
/// ([`AccountRegistry::write_secret_slot`] drops them when so).
pub(crate) fn secret_writes_refused(store: &Arc<dyn SecretStore>) -> bool {
    store.get(REFUSE_SECRET_WRITES_KEY).is_some()
}

fn set_secret_writes_refused(store: &Arc<dyn SecretStore>, refuse: bool) {
    if refuse {
        store.set(REFUSE_SECRET_WRITES_KEY, "1");
    } else {
        store.delete(REFUSE_SECRET_WRITES_KEY);
    }
}

/// Arg of `refuse_secret_writes_for_test`. A malformed or absent arg reads as
/// `refuse: false` — lifting the fault is the safe direction for a typo.
#[derive(serde::Deserialize, Default)]
struct RefuseSecretWritesArg {
    #[serde(default)]
    refuse: bool,
}

/// Outcome of [`call_registry_method_for_test`]: whether the E2E-bridge name was
/// a registry method (and its optional reader value), or belongs to another
/// dispatcher.
pub enum RegistryMethodOutcome {
    /// Handled. The inner value is a reader's JSON-serialized result — `None`
    /// for a setter, exactly as `FreeMethodOutcome::Handled` spells it.
    Handled(Option<String>),
    /// Not a registry method; the caller tries the next dispatcher.
    NotMine,
}

/// Arg of `set_account_reach_for_test`.
///
/// Every field is optional and **absence means "leave it alone"**, so a test
/// moves one slot without having to restate the other. Clearing the hint is its
/// own flag rather than a `null` `reach_ipv4`, because a JSON `null` and an
/// absent key deserialize to the same `None` here and the two must not collide:
/// "do not touch the hint" and "delete the hint" are the difference between the
/// dial-rule test's two phases.
#[derive(serde::Deserialize, Default)]
struct AccountReachArg {
    #[serde(default)]
    nest_url: Option<String>,
    #[serde(default)]
    reach_ipv4: Option<String>,
    #[serde(default)]
    clear_reach_ipv4: bool,
}

/// What `account_reach_for_test` reads back — the active account's identity URL
/// and reach hint, so a failure diagnoses itself instead of leaving the test to
/// guess which of the two the launch path disagreed about
/// (`e2e-conventions.md` § point 6).
#[derive(serde::Serialize)]
struct AccountReachView {
    actor_id: Option<String>,
    nest_url: Option<String>,
    reach_ipv4: Option<String>,
}

/// The registry arms of the cross-app E2E bridge.
///
/// **`set_account_reach_for_test`** points the *active* account's `nest_url`
/// and/or reach hint wherever a test needs them. It exists because the reach
/// hint's behaviour is only observable when the two disagree — the domain
/// unreachable while the hint reaches the box — and nothing a user can do
/// produces that state on demand: the hint is a bucket-1 fact captured
/// automatically at the wizard's `LoggedIn` terminal, never a knob
/// (`onboarding.md` § Reach hint). Driving the *behaviour under test* still goes
/// through the app UI (`e2e-conventions.md` § point 8); this is fixture setup,
/// which that convention exempts by name.
///
/// **`account_reach_for_test`** reads both slots back for the active account.
///
/// Both operate on the active account and are no-ops when there is none — a
/// bridge call must never panic an app under test.
///
/// **`refuse_secret_writes_for_test`** (`{"refuse": true|false}`) makes this
/// registry's store take every identity-secret write and keep none — the
/// locked-keyring shape `SecretStore::set` cannot report — until lifted, so
/// [`AccountRegistry::add_account`]'s read-back refuses. It exists for the
/// stolen-identity ceremony's persist-failure message (`settings.md` §
/// Recovery kit → *The persist-failure message survives the page*), which only
/// renders when the successor's seed cannot be stored and which nothing a user
/// can do produces on demand. Sticky rather than one-shot because an app writes
/// the seed more than once on the way there; the caller lifts it when done.
/// Fault injection, not a stand-in for the user: the ceremony itself is still
/// driven through the UI (`e2e-conventions.md` § point 8).
///
/// **Compiled out of release artifacts** (convention 15 rule (a),
/// `e2e-automation-surface-gating.md` § The convention) — the module doc
/// above claimed this already; nothing actually enforced it until this
/// fix.
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
pub fn call_registry_method_for_test(
    registry: &AccountRegistry,
    name: &str,
    json_arg: &str,
) -> RegistryMethodOutcome {
    match name {
        "set_account_reach_for_test" => {
            let arg: AccountReachArg = serde_json::from_str(json_arg).unwrap_or_default();
            if let Some(actor_id) = registry.active() {
                if let Some(url) = arg.nest_url.as_deref() {
                    registry.set_nest_url(&actor_id, url);
                }
                // Order matters only in the impossible case where a caller asks
                // for both: an explicit address wins over an explicit clear, so
                // the request that says what the slot should CONTAIN is the one
                // that survives.
                if arg.clear_reach_ipv4 {
                    registry.clear_reach_ipv4(&actor_id);
                }
                if let Some(ip) = arg.reach_ipv4.as_deref() {
                    registry.set_reach_ipv4(&actor_id, ip);
                }
            }
            RegistryMethodOutcome::Handled(None)
        }
        "account_reach_for_test" => {
            let actor_id = registry.active();
            let view = AccountReachView {
                // Through `secrets()`, the slot's only reader — and the account
                // secret it also carries is dropped right here, never widened
                // into the bridge's JSON.
                nest_url: actor_id
                    .as_deref()
                    .and_then(|a| registry.secrets(a))
                    .and_then(|s| s.nest_url),
                reach_ipv4: actor_id.as_deref().and_then(|a| registry.reach_ipv4(a)),
                actor_id,
            };
            RegistryMethodOutcome::Handled(serde_json::to_string(&view).ok())
        }
        "refuse_secret_writes_for_test" => {
            let arg: RefuseSecretWritesArg = serde_json::from_str(json_arg).unwrap_or_default();
            set_secret_writes_refused(&registry.store, arg.refuse);
            RegistryMethodOutcome::Handled(None)
        }
        _ => RegistryMethodOutcome::NotMine,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AccountRegistry, InMemorySecretStore};
    use std::sync::Arc;

    /// A real Ed25519 secret, so `add_account` derives a genuine actor id
    /// rather than the test asserting against a hand-made one.
    const SECRET: &str = "11";

    fn registry() -> AccountRegistry {
        AccountRegistry::new(Arc::new(InMemorySecretStore::default()))
    }

    /// A registry holding one active account whose nest is reachable — the
    /// state every real app is in the first time a test touches this bridge.
    fn registry_with_active_account() -> (AccountRegistry, String) {
        let r = registry();
        let actor = r
            .add_account(&SECRET.repeat(32), Some("https://box.example.com"), None)
            .expect("a real secret adds an account");
        r.set_active(&actor).expect("the fresh account activates");
        (r, actor)
    }

    fn reach(r: &AccountRegistry, actor: &str) -> Option<String> {
        r.reach_ipv4(actor)
    }

    fn call(r: &AccountRegistry, name: &str, arg: &str) -> Option<String> {
        match call_registry_method_for_test(r, name, arg) {
            RegistryMethodOutcome::Handled(v) => v,
            RegistryMethodOutcome::NotMine => panic!("{name} should be a registry method"),
        }
    }

    #[test]
    fn an_unknown_name_belongs_to_another_dispatcher() {
        let r = registry();
        assert!(matches!(
            call_registry_method_for_test(&r, "seed_identity", "null"),
            RegistryMethodOutcome::NotMine
        ));
    }

    #[test]
    fn a_bridge_call_with_no_active_account_is_a_no_op_not_a_panic() {
        // An app under test must survive a bridge call made a moment too early.
        let r = registry();
        call(
            &r,
            "set_account_reach_for_test",
            r#"{"reach_ipv4":"127.0.0.1"}"#,
        );
        let read = call(&r, "account_reach_for_test", "null").expect("reader returns JSON");
        assert!(read.contains("\"actor_id\":null"), "got {read}");
    }

    #[test]
    fn the_setter_moves_each_slot_independently() {
        // The dial-rule journey needs exactly this: point the domain somewhere
        // unreachable WITHOUT disturbing the hint, then point it back.
        let (r, actor) = registry_with_active_account();

        call(
            &r,
            "set_account_reach_for_test",
            r#"{"nest_url":"http://nest.invalid:8443","reach_ipv4":"127.0.0.1"}"#,
        );
        assert_eq!(reach(&r, &actor).as_deref(), Some("127.0.0.1"));
        assert_eq!(
            r.secrets(&actor).and_then(|s| s.nest_url).as_deref(),
            Some("http://nest.invalid:8443")
        );

        // A url-only write leaves the hint exactly where it was — absence is
        // "leave it alone", which is the whole reason the clear is a flag.
        call(
            &r,
            "set_account_reach_for_test",
            r#"{"nest_url":"http://127.0.0.1:8443"}"#,
        );
        assert_eq!(
            reach(&r, &actor).as_deref(),
            Some("127.0.0.1"),
            "a url-only write must not touch the hint"
        );
        assert_eq!(
            r.secrets(&actor).and_then(|s| s.nest_url).as_deref(),
            Some("http://127.0.0.1:8443")
        );
    }

    #[test]
    fn clearing_the_hint_is_a_flag_not_an_absent_field() {
        // `{"reach_ipv4": null}` and `{}` deserialize identically, so the clear
        // cannot ride on the address field without making "leave it alone"
        // unexpressible.
        let (r, actor) = registry_with_active_account();
        call(
            &r,
            "set_account_reach_for_test",
            r#"{"reach_ipv4":"127.0.0.1"}"#,
        );

        call(&r, "set_account_reach_for_test", r#"{"reach_ipv4":null}"#);
        assert_eq!(
            reach(&r, &actor).as_deref(),
            Some("127.0.0.1"),
            "an explicit null must read as `leave it alone`, like an absent key"
        );

        call(
            &r,
            "set_account_reach_for_test",
            r#"{"clear_reach_ipv4":true}"#,
        );
        assert_eq!(reach(&r, &actor), None, "the flag is what deletes");
    }

    #[test]
    fn the_reader_reports_both_slots_and_never_the_secret() {
        let (r, actor) = registry_with_active_account();
        call(
            &r,
            "set_account_reach_for_test",
            r#"{"nest_url":"http://nest.invalid:8443","reach_ipv4":"127.0.0.1"}"#,
        );

        let read = call(&r, "account_reach_for_test", "null").expect("reader returns JSON");
        let v: serde_json::Value = serde_json::from_str(&read).expect("valid JSON");
        assert_eq!(v["actor_id"], serde_json::json!(actor));
        assert_eq!(v["nest_url"], serde_json::json!("http://nest.invalid:8443"));
        assert_eq!(v["reach_ipv4"], serde_json::json!("127.0.0.1"));
        assert!(
            !read.contains(&SECRET.repeat(32)),
            "the account secret must never cross the bridge: {read}"
        );
    }

    /// The door outcome 17's journey needs: a keystore that takes the
    /// successor's secret and keeps nothing (a locked keyring) is staged by the
    /// registry's own store, and `add_account` reports it rather than returning
    /// a success nothing backs. Sticky until lifted — every app writes the seed
    /// more than once on the way to the persist-failure message (tui: straight
    /// after the ceremony lands, then again at adoption), so a one-shot fault
    /// would be healed by the second write and the message would never show.
    #[test]
    fn refused_secret_writes_fail_add_account_until_lifted() {
        let r = registry();
        call(&r, "refuse_secret_writes_for_test", r#"{"refuse":true}"#);

        assert!(
            matches!(
                r.add_account(&SECRET.repeat(32), Some("https://box.example.com"), None),
                Err(crate::AccountError::NoStoredSecret(_))
            ),
            "a secret write the store dropped must fail the add, not read as saved"
        );
        assert!(
            matches!(
                r.add_account(&SECRET.repeat(32), None, None),
                Err(crate::AccountError::NoStoredSecret(_))
            ),
            "and it stays refused: the fault is the store's state, not one write's"
        );
        assert!(
            r.list().is_empty(),
            "a refused add must not index the account"
        );

        call(&r, "refuse_secret_writes_for_test", r#"{"refuse":false}"#);
        let actor = r
            .add_account(&SECRET.repeat(32), None, None)
            .expect("lifting the fault restores ordinary writes");
        assert!(r.secrets(&actor).is_some());
    }

    /// The fault belongs to ONE store's backing — the app's own, which every
    /// per-operation registry view of it shares — never to the process. A test arming it on
    /// one registry must not fail another store's writes (parallel unit tests;
    /// the aux namespaces an app's erase reaches).
    #[test]
    fn refused_secret_writes_are_scoped_to_the_arming_store() {
        let store: Arc<dyn crate::SecretStore> = Arc::new(InMemorySecretStore::default());
        let armed = AccountRegistry::new(Arc::clone(&store));
        call(
            &armed,
            "refuse_secret_writes_for_test",
            r#"{"refuse":true}"#,
        );

        let other = registry();
        other
            .add_account(&SECRET.repeat(32), None, None)
            .expect("another store is untouched by the fault");

        let same_store_view = AccountRegistry::new(Arc::clone(&store));
        assert!(
            same_store_view
                .add_account(&SECRET.repeat(32), None, None)
                .is_err(),
            "a fresh registry view over the SAME store sees the fault — apps \
             build one per operation"
        );
        call(
            &armed,
            "refuse_secret_writes_for_test",
            r#"{"refuse":false}"#,
        );
    }

    /// Two store OBJECTS over one backing — apple's fresh `KeychainSecretStore`
    /// per registry over the one keychain, web's fresh `LocalStorageSecretStore`
    /// per wasm module over the one origin. A fault armed through either must
    /// refuse writes through the other, or those apps' journeys arm a fault the
    /// ceremony's own registry never sees.
    #[test]
    fn refused_secret_writes_reach_every_store_object_over_the_same_backing() {
        struct Handle(Arc<InMemorySecretStore>);
        impl crate::SecretStore for Handle {
            fn get(&self, key: &str) -> Option<String> {
                self.0.get(key)
            }
            fn set(&self, key: &str, value: &str) {
                self.0.set(key, value);
            }
            fn delete(&self, key: &str) {
                self.0.delete(key);
            }
        }
        let backing = Arc::new(InMemorySecretStore::default());
        let view = || AccountRegistry::new(Arc::new(Handle(Arc::clone(&backing))));
        // All three held for the whole test: a view dropped early frees its
        // store object, and the next one can be allocated at the same address.
        let (arming, writing, lifting) = (view(), view(), view());

        call(
            &arming,
            "refuse_secret_writes_for_test",
            r#"{"refuse":true}"#,
        );
        assert!(
            matches!(
                writing.add_account(&SECRET.repeat(32), None, None),
                Err(crate::AccountError::NoStoredSecret(_))
            ),
            "a second store object over the same backing must see the fault"
        );

        call(
            &lifting,
            "refuse_secret_writes_for_test",
            r#"{"refuse":false}"#,
        );
        writing
            .add_account(&SECRET.repeat(32), None, None)
            .expect("lifting through a third object restores writes everywhere");
    }

    #[test]
    fn a_malformed_arg_is_a_no_op_not_a_panic() {
        // The bridge is driven by a test harness over HTTP; a typo must not take
        // the app under test down with it.
        let (r, actor) = registry_with_active_account();
        call(&r, "set_account_reach_for_test", "not json at all");
        assert_eq!(reach(&r, &actor), None);
    }
}
