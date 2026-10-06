//! The credential store both direct-Rust desktop apps use — four backends
//! behind one [`SecretStore`] impl.
//!
//! (`fauna-desktop` and `fauna-tui` — `apps/linux.md` § Credential Storage,
//! `apps/tui.md` § Credential storage — and, through the shared account-store
//! slot, every native app and the sync agent.) The backends, resolved once at
//! construction:
//!
//! 1. **Keyring** — the OS-native secure store (the desktop-session default),
//!    one implementation per OS behind the same five `keyring_*` primitives:
//!    freedesktop Secret Service items namespaced by an `application`
//!    attribute (linux — lifted from `apps/fauna-linux/src/client.rs` when the
//!    tui app became the second consumer (priority #2); the linux-only trio
//!    wrappers (`load_credentials` / resume slots) stay in the linux app
//!    and build on the primitives exported here), macOS login-Keychain
//!    generic passwords (module [`mac_keychain`]), or Windows Credential
//!    Manager generic credentials (module [`win_credman`] — landed 2026-07-19
//!    with the sync-agent A2 track; `apps/tui.md` § Credential storage).
//! 2. **File** — the env-gated 0600 plaintext-JSON per-namespace map, for e2e
//!    runs (a headless box's gnome-keyring default collection can be locked,
//!    failing writes with `IsLocked` even though `connect` succeeds).
//! 3. **Sealed** ([`sealed::SealedFileStore`]) — the passphrase-encrypted
//!    headless backend the tui's [`CredentialStore::new_with_headless_fallback`]
//!    auto-selects when no usable Secret Service is reachable
//!    (`apps/tui.md` § Credential storage owns the design).
//! 4. **Foreign** — the **app's own platform secure store**, reached over the
//!    `SecretStore` seam the multi-account registry already rides (the Swift
//!    Keychain on iOS, `EncryptedSharedPreferences` on android), installed
//!    once per process by the app ([`install_foreign_store`]). Selected only
//!    on a target with **no native arm of its own** ([`HAS_NATIVE_KEYRING_ARM`]
//!    is `false` — the [`no_keyring`] targets), where until 2026-08-26 every
//!    slot this crate served was inert and the account-store writer key could
//!    never persist in production (`apps/common.md` § Credential storage →
//!    *The shared Rust credential slots on the phones*).
//!
//! Env contract (shared with the e2e drivers). **All three are compile-gated
//! behind `cfg(any(test, debug_assertions, feature = "e2e-agent"))`, each as a
//! real/production-twin pair — a shipping build names none of them**
//! (e2e-conventions.md convention 15; the redirect pair ruled 2026-08-11,
//! `FAUNA_KEYRING_APP` 2026-08-15):
//! - `FAUNA_KEYRING_APP` ([`keyring_app_override`]) — overrides the
//!   `application` namespace so parallel e2e runs never sweep each other's
//!   credentials.
//! - `FAUNA_E2E_CREDENTIAL_DIR` ([`cred_file_dir`]) — when set, the store
//!   reads/writes the per-namespace `{dir}/{app}.json` file instead of
//!   libsecret.
//! - `FAUNA_E2E_FORCE_HEADLESS_STORE` — test-only: forces the headless
//!   resolution onto the sealed backend on a desktop dev box.
//!
//! ⚠ **A release-profile e2e build must enable `e2e-agent`, or all three are
//! silently inert.** That is not hypothetical: the Flatpak real-session seam
//! test passes `FAUNA_E2E_CREDENTIAL_DIR` + `FAUNA_KEYRING_APP` to an artifact
//! its own manifest builds with a plain `cargo build --release`, so its
//! scripted sign-in has been a no-op since the 2026-08-11 gating — see
//! e2e-conventions.md convention 15's *release-profile e2e paths* note.

#[cfg(target_os = "macos")]
pub mod mac_keychain;
#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
pub mod no_keyring;
pub mod sealed;
#[cfg(target_os = "windows")]
pub mod win_credman;

#[cfg(target_os = "macos")]
use mac_keychain::{
    keyring_delete as native_keyring_delete,
    keyring_delete_namespace as native_keyring_delete_namespace, keyring_get as native_keyring_get,
    keyring_probe as native_keyring_probe, keyring_set as native_keyring_set,
};
#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
use no_keyring::{
    keyring_delete as native_keyring_delete,
    keyring_delete_namespace as native_keyring_delete_namespace, keyring_get as native_keyring_get,
    keyring_probe as native_keyring_probe, keyring_set as native_keyring_set,
};
#[cfg(target_os = "windows")]
use win_credman::{
    keyring_delete as native_keyring_delete,
    keyring_delete_namespace as native_keyring_delete_namespace, keyring_get as native_keyring_get,
    keyring_probe as native_keyring_probe, keyring_set as native_keyring_set,
};

// ── The public keyring surface: one gate over every platform arm ──────────
//
// Each arm above (and the freedesktop arm below, inline) is reached ONLY
// through these five wrappers, so "may this process touch the OS keyring?" is
// answered once, for every platform and every caller — the `CredentialStore`
// keyring backend, the headless-fallback probe, and the apps' own
// `keyring_probe` reads (tui's notification arm) alike.

/// Whether this process may reach the **live OS keyring** at all.
///
/// Always `true` in a shipped build. The one `false` is a **test build** — one
/// compiled with the `no-live-keyring` feature, which workspace crates enable
/// from their `[dev-dependencies]` ONLY, so resolver 2 keeps it out of every
/// plain build while workspace feature unification carries it into every test
/// build (`.cargo/config.toml` § feature unification). A unit-test binary must
/// never reach the developer's desktop keyring: `gnome-keyring-daemon` aborts on
/// a client that vanishes mid-session, and every abort re-locks the login
/// keyring for every process on the box (`e2e-launch-isolation.md` convention
/// 10 names the same hazard for app launches). Denied, the keyring behaves as
/// the inert arm ([`no_keyring`]'s contract): reads find nothing, writes and
/// deletes are dropped with a warning, the probe reports no usable keyring.
///
/// Two ways back to the real store, both explicit: a harness launch, which
/// always pins a per-launch namespace (`FAUNA_KEYRING_APP`, set by every e2e
/// driver and never by `cargo test`), so a binary that a `cargo test` happened
/// to build keeps working under the harness's private Secret Service; and the
/// `allow-live-keyring` feature, forwarded only by the opt-in live suites
/// (`live-keychain`, `fauna-linux/live-secret-service`).
#[cfg(all(feature = "no-live-keyring", not(feature = "allow-live-keyring")))]
pub fn live_keyring_allowed() -> bool {
    keyring_app_override().is_some()
}

/// Shipped-build twin (and the live suites'): the keyring is always reachable.
#[cfg(not(all(feature = "no-live-keyring", not(feature = "allow-live-keyring"))))]
pub fn live_keyring_allowed() -> bool {
    true
}

/// Calls that passed [`live_keyring_allowed`] and reached a native arm, since
/// process start. A test build's guard tests assert it stays zero — the
/// observable that a unit-test binary never opened a keyring session.
static LIVE_KEYRING_CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// See [`LIVE_KEYRING_CALLS`].
pub fn live_keyring_calls() -> usize {
    LIVE_KEYRING_CALLS.load(std::sync::atomic::Ordering::Relaxed)
}

/// The gate itself: `true` = go on to the native arm (and count it).
fn enter_live_keyring(op: &str, account: &str) -> bool {
    if live_keyring_allowed() {
        LIVE_KEYRING_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return true;
    }
    tracing::warn!(
        op,
        account,
        "credential store: live OS keyring refused in a test build \
         (see live_keyring_allowed); the op behaves as the inert arm"
    );
    false
}

/// Whether a usable OS keyring is reachable right now (the per-platform
/// meaning lives on each arm's own probe) — and never, in a test build.
pub fn keyring_probe() -> bool {
    // A refused probe is an ordinary answer ("no usable keyring"), never a
    // warning — hence the plain check before the counting gate.
    live_keyring_allowed() && enter_live_keyring("probe", "") && native_keyring_probe()
}

/// Read one item's value by (`application`, `account`). `None` on absent item,
/// any error, or a test build.
pub fn keyring_get(app: &str, account: &str) -> Option<String> {
    if !enter_live_keyring("get", account) {
        return None;
    }
    native_keyring_get(app, account)
}

/// Write one item (replace). Best-effort + log, like every arm.
pub fn keyring_set(app: &str, account: &str, value: &str) {
    if enter_live_keyring("set", account) {
        native_keyring_set(app, account, value);
    }
}

/// Delete the item(s) for one logical key. Best-effort + log.
pub fn keyring_delete(app: &str, account: &str) {
    if enter_live_keyring("delete", account) {
        native_keyring_delete(app, account);
    }
}

/// Delete every item in the namespace; a refused (test-build) call is the inert
/// arm's success — a namespace this process could never write is empty.
pub fn keyring_delete_namespace(app: &str) -> Result<(), anyhow::Error> {
    if !enter_live_keyring("delete_namespace", app) {
        return Ok(());
    }
    native_keyring_delete_namespace(app)
}

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use fauna_client_accounts::{AccountRegistry, MutationLock, SecretStore};

/// Whether this target has an OS-native keyring arm **this crate drives** —
/// the positive allow-list the arm modules above spell out (macOS login
/// Keychain, Windows Credential Manager, freedesktop Secret Service). `false`
/// lands on [`no_keyring`], whose every write is dropped by design: there the
/// platform's *app* owns the secure store, and the only way a slot this crate
/// serves can persist is the app lending that store back over the foreign
/// seam ([`install_foreign_store`]).
///
/// A compile-time constant, deliberately not [`keyring_probe`]: the probe is
/// real I/O on the native arms (a keychain unlock check, a D-Bus round trip),
/// and backend resolution must stay free of it on every construction — the
/// question here is *does an arm exist*, never *is it reachable right now*.
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
pub const HAS_NATIVE_KEYRING_ARM: bool = true;
/// See the native-arm twin above: this target's arm is [`no_keyring`].
#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
pub const HAS_NATIVE_KEYRING_ARM: bool = false;

/// The app-installed platform secure store, when one has been lent to this
/// crate — process-global, exactly like `fauna_client::trust::install_pin_store`
/// (the shell installs it once at launch; last install wins). `None` on every
/// desktop app and the sync agent, which never install one.
static FOREIGN_STORE: RwLock<Option<Arc<dyn SecretStore>>> = RwLock::new(None);

/// Lend the app's own platform secure store to this crate, so the slots it
/// serves — the T10 account-store writer key first of all — persist on a
/// target whose native arm is inert ([`HAS_NATIVE_KEYRING_ARM`] `false`).
///
/// Call **once at process start, before the first sign-in** — the account
/// runtime's assembly reads and mints its writer key on the post-auth path,
/// and a store installed after that has missed the mint. The `fauna-ffi`
/// export the phones call is `install_platform_credential_store`; it hands
/// over the **same** `FfiSecretStore` the multi-account registry rides, so the
/// writer key and the identity it belongs to live in one store by
/// construction (`apps/common.md` § Credential storage → *The shared Rust
/// credential slots on the phones*).
///
/// Harmless on a target with a native arm: resolution never consults the
/// foreign store there ([`resolve_backend`]), so a shell that installs
/// unconditionally (FaunaKit serves macOS and iOS from one code path) changes
/// nothing on the desktop.
pub fn install_foreign_store(store: Arc<dyn SecretStore>) {
    *FOREIGN_STORE.write().unwrap_or_else(|e| e.into_inner()) = Some(store);
}

/// The installed foreign store, if any. Exposed for the `fauna-ffi` install
/// export's own pin and for tests; production callers construct a
/// [`CredentialStore`] and let resolution pick.
pub fn foreign_store() -> Option<Arc<dyn SecretStore>> {
    FOREIGN_STORE
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// The foreign store's key for one (`application`, `account`) item.
///
/// **Namespace-prefixed, and that is load-bearing.** The native arms keep
/// namespaces apart by an attribute the store itself indexes (`application`);
/// a foreign store is one flat logical-key map shared with the account
/// registry (`fauna/index`, `fauna/<actor>/secret`, …) and with every other
/// namespace this crate serves. Two of those — the account-store slot and the
/// sync agent's — both key their per-account item by the **actor id hex**, so
/// an unprefixed key would let the agent's principal bundle and the store's
/// writer key overwrite each other. The prefix is the namespace verbatim, so
/// a row reads as `fauna-account-store/<actor hex>` in the platform store.
fn foreign_key(app: &str, account: &str) -> String {
    format!("{app}/{account}")
}

/// The **shared account-store namespace**: one slot per account, keyed by the
/// actor id hex, holding the material `fauna-sync-engine`'s account runtime
/// mints on an identity's behalf — the T10 store writer key and the four
/// `principal_bundle` slots (device-auth, backup-key, generation-keys,
/// grant-registered).
///
/// Deliberately **not** an app's own namespace, and that is the whole point:
/// the charter puts one store device principal on a machine
/// (`account-data-plane.md` § The store device principal — "the slot is
/// `libs/fauna-credential-store` under one shared namespace keyed by
/// account"), so the app and the app-dead sync agent beside it must reach the
/// same row. Collapsing it into each app's namespace would mint a writer key
/// per app.
///
/// It lives here, in the crate that owns namespaces, rather than beside its
/// writer: [`account_registry`] — the erase — needs it too, and one owner per
/// claim beats two literals in step.
/// `fauna_sync_engine::account_runtime::CRED_NAMESPACE` re-exports it.
pub const ACCOUNT_STORE_NAMESPACE: &str = "fauna-account-store";

/// The namespace [`CredentialStore::new`] and
/// [`CredentialStore::new_with_headless_fallback`] resolve `default_app` to,
/// given the harness's namespace override.
///
/// **Load-bearing for the account-store split under e2e.** Every driver sets
/// `FAUNA_KEYRING_APP` once per launch for run isolation, and — before this —
/// that one override replaced *any* `default_app` verbatim, so the app's own
/// namespace and [`ACCOUNT_STORE_NAMESPACE`] collapsed onto the identical
/// string and became one physical store for the life of a test
/// (`long-term-store.md` § Implementation status today, hole 3 — the bug that
/// let hole 3 itself ship green). Deriving the account-store's override from
/// the app's, rather than reusing it, keeps the two apart with **no new
/// environment surface**: one harness knob still produces exactly one
/// override value per run, this just resolves it to two distinct namespaces —
/// so the derivation owes none of the gating analysis a genuinely new var
/// would ([`keyring_app_override`]'s doc). Production (`keyring_app_override`
/// returning `None`) is unaffected: `default_app` passes through unchanged,
/// exactly as before.
fn resolved_namespace(default_app: &str) -> String {
    apply_namespace_override(default_app, keyring_app_override())
}

/// The pure decision [`resolved_namespace`] wraps around the env read, so the
/// derivation itself is testable without mutating process-global env state
/// (this crate has no established convention for serializing env-mutating
/// tests, and `std::env::var` is process-global — a parallel-running test
/// setting `FAUNA_KEYRING_APP` would race every other test that reads it).
fn apply_namespace_override(default_app: &str, over: Option<String>) -> String {
    match over {
        Some(over) if default_app == ACCOUNT_STORE_NAMESPACE => format!("{over}-account-store"),
        Some(over) => over,
        None => default_app.to_string(),
    }
}

/// A [`SecretStore`] view of one namespace that resolves its backend on
/// **every** call rather than once at construction.
///
/// Late binding is load-bearing on the phones. There the account-store
/// namespace rides the app's own store over the foreign seam, and that store
/// is installed process-globally at start-up
/// (`fauna_ffi::install_platform_credential_store`) — but an app is free to
/// mint its switcher registry first. A [`CredentialStore`] built at that
/// moment would snapshot the *inert* backend and stay inert for the life of
/// the process, silently erasing nothing. Resolution is an env read plus an
/// `RwLock` read, and the erase touches a handful of keys per account, so
/// paying it per call is free.
struct LateBoundNamespace(&'static str);

impl SecretStore for LateBoundNamespace {
    fn get(&self, key: &str) -> Option<String> {
        CredentialStore::new(self.0).get(key)
    }
    fn set(&self, key: &str, value: &str) {
        CredentialStore::new(self.0).set(key, value);
    }
    fn delete(&self, key: &str) {
        CredentialStore::new(self.0).delete(key);
    }
}

/// Every credential namespace *besides* the app's own that holds per-actor
/// slots — what [`fauna_client_accounts::AccountRegistry::clear_all`] and
/// `remove` must sweep in addition to the store they index
/// (`long-term-store.md` § Cleanup contract).
///
/// **The single home of that list.** Growing it here covers sign-out, account
/// removal, and every app at once; the alternative — each app assembling its
/// own — is the silent-omission failure the registry's own
/// `PER_ACTOR_KEY_BUILDERS` comment already records for key *shapes*, one
/// level up.
///
/// Not on the list, deliberately: the sync agent's `fauna-sync-agent`
/// namespace keys its capability record process-globally (`capability/v1`,
/// `signed-out/v1`), not per actor, and is un-provisioned through the
/// sign-out marker the app writes there — a different mechanism, owned by
/// `apps/sync-agent.md` § Credential model.
fn account_scoped_aux_stores() -> Vec<Arc<dyn SecretStore>> {
    vec![Arc::new(LateBoundNamespace(ACCOUNT_STORE_NAMESPACE))]
}

/// The account registry over `app_store`, **erase-complete**: its
/// `clear_all`/`remove` reach every auxiliary account-scoped namespace
/// ([`account_scoped_aux_stores`]) as well as the app's own store.
///
/// ⚠ **Every erase-capable native registry is built here.** `AccountRegistry::new`
/// still exists and is still right for a transient read-only view, but a
/// registry a sign-out or an account removal will run through must come from
/// this function (or [`account_registry_with_lock`]) — otherwise the erase
/// deletes the app-namespace rows, which were never written, and leaves the
/// real ones (`long-term-store.md` § Implementation status today, hole 3).
/// Production callers today: `fauna-ffi` (windows, macOS, iOS, android),
/// `fauna-linux`, `fauna-tui`. Web builds `AccountRegistry::new` directly and
/// correctly: it has no account runtime, so it has no second namespace.
pub fn account_registry(app_store: Arc<dyn SecretStore>) -> AccountRegistry {
    AccountRegistry::new(app_store).also_erasing(account_scoped_aux_stores())
}

/// [`account_registry`] with registry mutations serialized under `lock` — the
/// twin of `AccountRegistry::with_mutation_lock`
/// (`account-scoping.md` § Concurrent instances).
pub fn account_registry_with_lock(
    app_store: Arc<dyn SecretStore>,
    lock: Arc<dyn MutationLock>,
) -> AccountRegistry {
    AccountRegistry::with_mutation_lock(app_store, lock).also_erasing(account_scoped_aux_stores())
}

/// The sign-out credential erase for an app whose own namespace can be swept
/// wholesale — linux and tui — in the one order `long-term-store.md` § Cleanup
/// contract allows, returning what survived both passes.
///
/// **Both passes, in this order, and neither alone.** `registry.clear_all()`
/// first: only its per-actor sweep crosses into the shared
/// `fauna-account-store` namespace (writer key, principal bundles), and it
/// enumerates through the index the wipe destroys. Then
/// `namespace.delete_namespace()`, the superset within the app's own namespace.
/// Then a read-back of whatever the first pass left, which the wipe may since
/// have removed.
///
/// **The wipe's own failure is kept, not logged away.** A keyring that refuses
/// the wipe because it cannot be reached refuses the read-back the same way and
/// reads as empty, so the error is the only witness of that residue — and until
/// 2026-09-13 both callers discarded it (`let _ =` on linux, a `warn!` on tui)
/// and painted a clean "Signed out" over it (`account-scoping.md` § Erasure
/// follows scope → *the credential half is a residue class too*).
///
/// One home for the sequence, so the two freedesktop apps cannot drift on it —
/// they did hold it as two hand-kept copies, each with its own comment
/// explaining the order.
pub fn erase_all_credentials(
    registry: &AccountRegistry,
    namespace: &CredentialStore,
) -> fauna_client_accounts::CredentialSweep {
    let mut sweep = registry.clear_all();
    if let Err(e) = namespace.delete_namespace() {
        tracing::warn!(
            "[account-store] wiping the {:?} credential namespace failed: {e:#}",
            namespace.app
        );
        sweep.record_wipe_failure();
    }
    registry.reverify(sweep)
}

/// The credential half of a sign-out residue retry
/// (`fauna_client_accounts::retry_sign_out_residue`'s `erase_credentials`) for
/// the two apps whose namespace is swept wholesale: [`erase_all_credentials`]
/// again, plus a read-back of the keys the sign-out recorded as surviving.
///
/// The read-back is not redundant with the erase's own: by the time a retry
/// runs the registry is empty, so the per-actor pass can no longer enumerate
/// the shared-namespace keys (writer key, principal bundles) the sign-out
/// could not remove — only the record still names them. It reads and never
/// deletes them by name: a sibling app signed in to the same account since may
/// be relying on exactly those keys, and deleting them would strand its store.
pub fn re_erase_credentials(
    registry: &AccountRegistry,
    namespace: &CredentialStore,
    recorded: fauna_client_accounts::CredentialSweep,
) -> fauna_client_accounts::CredentialSweep {
    let fresh = erase_all_credentials(registry, namespace);
    let read_back = registry.reverify(fauna_client_accounts::CredentialSweep {
        survivors: recorded.survivors,
        wipe_failed: false,
    });
    let mut survivors = fresh.survivors;
    for key in read_back.survivors {
        if !survivors.contains(&key) {
            survivors.push(key);
        }
    }
    fauna_client_accounts::CredentialSweep {
        survivors,
        wipe_failed: fresh.wipe_failed,
    }
}

/// The largest value one item may carry, on **every** backend.
///
/// The binding constraint is Windows Credential Manager: a `CRED_TYPE_GENERIC`
/// credential's blob is capped by Win32 at `CRED_MAX_CREDENTIAL_BLOB_SIZE`
/// (5 × 512 = 2560 bytes), and [`win_credman::keyring_set`] — like every other
/// backend's write — is best-effort-and-log, so an over-cap write is a *warn*,
/// not an error the caller sees. The other backends are far roomier (a
/// Secret Service item, a Keychain generic password, the 0600 JSON file), but
/// this store's whole point is that a value written on one machine is readable
/// on the same account's other machines, so the capacity of a shared bundle
/// must be **one number everywhere** rather than a per-OS surprise discovered
/// only on Windows (priority #1). Callers that assemble a growable value
/// (today: the principal bundle's retained generation keys) bound it against
/// this before writing.
///
/// This is a ceiling, not a budget: nothing here reserves it, and an item well
/// under it is the normal case.
pub const MAX_ITEM_VALUE_BYTES: usize = 2560;

/// The account registry bounds its index against its own spelling of the item
/// cap (it cannot import this one — this crate depends on it); this store owns
/// the real one.
const _: () = assert!(fauna_client_accounts::MAX_INDEX_VALUE_BYTES == MAX_ITEM_VALUE_BYTES);

/// The (`application`, `account`) attribute pair every item carries.
pub fn cred_attrs<'a>(app: &'a str, account: &'a str) -> HashMap<&'a str, &'a str> {
    [("application", app), ("account", account)]
        .into_iter()
        .collect()
}

/// Directory selecting the file-backed credential store, or `None` when
/// libsecret should be used (the production default). Read fresh on each call —
/// cheap relative to the I/O it gates, and it lets in-process tests flip
/// backends between logical runs.
///
/// **The gate is the security boundary, not the env read** (e2e-conventions.md
/// convention 15): this is where the identity secret is read AND written, so an
/// ungated read lets whoever controls a released app's environment both harvest
/// what the app stores and supply what the app loads. Gated as a pair with the
/// production twin below — the `fauna_ipc::endpoint::e2e_pipe_override` shape.
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
pub fn cred_file_dir() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(dir) = tests::test_cred_dir() {
        return Some(dir);
    }
    match std::env::var("FAUNA_E2E_CREDENTIAL_DIR") {
        Ok(d) if !d.is_empty() => Some(PathBuf::from(d)),
        _ => None,
    }
}

/// Production twin: a plain `--release` build consults no harness environment —
/// the file backend is unreachable and the store resolves the OS keyring (or
/// the sealed headless arm) only. A release-profile e2e build keeps the
/// redirect through this crate's `e2e-agent` feature, which linux/tui/the
/// sync-agent forward from their own `e2e-agent` — so this twin is reached only
/// by a build that ships.
#[cfg(not(any(test, debug_assertions, feature = "e2e-agent")))]
pub fn cred_file_dir() -> Option<PathBuf> {
    None
}

/// The harness's keyring-namespace override, or `None` for the client's own
/// hard-coded default (the production answer).
///
/// **Gated, not allowlisted** — the
/// call [`cred_file_dir`]'s gating deliberately left open, because this one is
/// a different and lesser class: it selects a namespace *within the user's own
/// keyring* rather than relocating the store out of the OS secure store, so
/// controlling it neither harvests nor supplies the identity secret the way
/// `FAUNA_E2E_CREDENTIAL_DIR` did (an attacker who could plant a secret under
/// an arbitrary namespace could equally plant one under the default, so the
/// var confers no capability). It is gated anyway, for the reason conclusion
/// (3) gives and the reason the `FAUNA_E2E_AGENT_PORT` allowlist guess was
/// rejected: **the allowlist is for reads production genuinely needs**
/// (`APPIMAGE`, `FLATPAK_ID`), and this is a harness knob no deployment sets —
/// convention 15 requires the automation surface compiled out, so a shipped
/// binary should not name it.
///
/// There is no production multi-namespace story to preserve: each client's
/// default is a hard-coded constant, the app↔agent pair agree by both
/// resolving [`fauna_ipc::sync::CRED_NAMESPACE`]'s default, and multi-account
/// is per-*actor* **within** a namespace, never per-namespace.
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
pub fn keyring_app_override() -> Option<String> {
    match std::env::var("FAUNA_KEYRING_APP") {
        Ok(a) if !a.is_empty() => Some(a),
        _ => None,
    }
}

/// Production twin: a shipping build consults no harness environment and
/// always resolves the client's hard-coded default namespace. See
/// [`cred_file_dir`]'s twin for the pair shape and why the gate — not the env
/// read — is the boundary.
#[cfg(not(any(test, debug_assertions, feature = "e2e-agent")))]
pub fn keyring_app_override() -> Option<String> {
    None
}

/// Path to the per-namespace JSON file. One file per `application` namespace
/// mirrors the libsecret layout, where each namespace is an isolated surface.
pub fn cred_file_path(dir: &Path, app: &str) -> PathBuf {
    dir.join(format!("{app}.json"))
}

/// Path to the zero-byte lock file guarding one namespace's read-modify-write.
/// Named after the namespace file it guards, never deleted, never truncated —
/// the three [`fauna_core::fs_lock`] invariants.
pub fn cred_file_lock_path(dir: &Path, app: &str) -> PathBuf {
    dir.join(format!("{app}.json.lock"))
}

/// Take the namespace's exclusive lock for the duration of a mutation.
///
/// **Why a lock at all.** Every file-arm mutation is a whole-file
/// read-modify-write ([`cred_file_read`] → mutate → [`cred_file_write`]), and
/// the namespace has many concurrent writers: inside one client the launch
/// machine, the account runtime, the conversations rail and the settings pump
/// all write, and under e2e (`FAUNA_KEYRING_APP` collapses the agent's
/// namespace onto the app's) the **separate** `fauna-sync-agent` process writes
/// the same file. Unlocked, two overlapping RMWs lose one of the two writes,
/// and — the sharp edge — a write whose *read* preceded a
/// [`cred_file_remove`] and whose *write* follows it restores the entire
/// pre-wipe namespace. Both shapes produce the same user-visible failure, which
/// [`cred_file_remove`]'s own doc comment describes: a surviving `fauna/index`
/// (or a reverted per-actor secret) pins the active account to the previous
/// actor, the next sign-in mints a bearer for that actor while its `AuthClient`
/// holds the new actor's keypair, and the nest refuses the WS upgrade `403`
/// (`security.md` § Cross-connection binding) — measured as a ~15 s blackout of
/// every RPC on linux.
///
/// **Advisory degrade**, matching `fauna_client_accounts::FileMutationLock`: an
/// unlockable path yields today's unserialized behavior rather than a refused
/// sign-in. A lock narrows a race; it must never widen a failure.
///
/// The returned `File` is the guard — dropping it releases, and the kernel
/// releases it if the holder dies, so no stale lock is possible.
fn lock_cred_file(dir: &Path, app: &str) -> Option<std::fs::File> {
    lock_file_at(&cred_file_lock_path(dir, app))
}

/// Open and exclusively lock the file at `path` — shared by [`lock_cred_file`]
/// and `sealed::lock_sealed_file`, which otherwise re-derived this
/// identically. `None` on any I/O or lock failure (both callers already
/// treat that as "could not lock", not a hard error).
pub(crate) fn lock_file_at(path: &Path) -> Option<std::fs::File> {
    let file = fauna_core::fs_lock::open_lock_file(path).ok()?;
    file.lock().ok()?;
    Some(file)
}

/// Read the account→value map for one namespace. A missing or unparseable file
/// reads as empty, matching libsecret's "no items" → `None` contract.
///
/// **This arm deliberately keeps the collapsed contract the sealed arm split**
/// (`sealed::SealedFileStore::try_read_map`, 2026-08-31): there, an unreadable
/// map read as empty and then written back destroyed a *production* namespace
/// of client-only-resident key material. Here it cannot — [`cred_file_dir`] is
/// compiled out of a shipped build, so this backend holds only harness
/// fixtures, and its own writes are tmp+rename, so nothing it does can produce
/// the unparseable file in the first place. The asymmetry is the reachability
/// difference, not drift; if this backend ever becomes production-reachable it
/// needs the same fallible twin.
pub fn cred_file_read(dir: &Path, app: &str) -> BTreeMap<String, String> {
    match std::fs::read(cred_file_path(dir, app)) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => BTreeMap::new(),
    }
}

/// Write the account→value map for one namespace as 0600 JSON. The file holds
/// the identity secret in plaintext — the same trust model as a libsecret item
/// on an unlocked collection — so it is owner-read/write only. Creates the
/// directory if absent. (Windows has no unix mode bits; there the file relies
/// on the profile directory's owner-scoped NTFS ACL, the same posture every
/// other per-user file under %LocalAppData% gets.)
///
/// **Written atomically** via [`fauna_core::secret_file::write_secret_file_0600_at`]
/// — a fresh temp file in the same directory, created 0600 by `open(2)`
/// itself (never written first and chmod-ed after, which would expose the
/// plaintext secret at the umask default for the width of that window),
/// `fsync`ed, then a rename over the namespace path (`fsync`ed too — a
/// crash-durability property this function inherits from the shared
/// primitive). Writers serialize on [`lock_cred_file`], but *readers*
/// ([`cred_file_read`], `SecretStore::get`) deliberately take no lock, so the
/// rename is what guarantees a reader sees either the whole old map or the
/// whole new one and never a half-written file — and what keeps a crash
/// mid-write from truncating the only copy of the identity secret.
pub fn cred_file_write(
    dir: &Path,
    app: &str,
    map: &BTreeMap<String, String>,
) -> Result<(), anyhow::Error> {
    std::fs::create_dir_all(dir)?;
    let path = cred_file_path(dir, app);
    // Unique per writer: `cred_file_write` is `pub`, so a caller outside the
    // locked RMW paths (test seeding) must not collide with another's temp —
    // the reason this calls the `_at` variant instead of the plain
    // `write_secret_file_0600`, whose single default `<path>.tmp` sibling
    // would let two such unlocked writers collide.
    let tmp = dir.join(format!(
        "{app}.json.{}.{}.tmp",
        std::process::id(),
        TMP_WRITE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
    ));
    fauna_core::secret_file::write_secret_file_0600_at(
        &path,
        &tmp,
        &serde_json::to_vec_pretty(map)?,
    )
}

/// Disambiguates concurrent [`cred_file_write`] temp files within one process.
static TMP_WRITE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Drop an entire namespace from the file store — the exact analogue of the
/// libsecret arm's "delete every item carrying `application = <app>`". One file
/// holds the whole namespace (`fauna/index`, every per-actor
/// secret), so removing it clears them together. A missing file is success,
/// matching libsecret's delete-nothing-found contract.
///
/// Clearing the namespace *wholesale* is what makes a sign-out sound: leaving
/// `fauna/index` behind pins the active account to the signed-out actor, and the
/// next sign-in then mints a bearer for the OLD actor while its `AuthClient`
/// holds the NEW actor's keypair — a mismatch the nest rejects with a WS-handshake
/// 403 (`security.md` § Cross-connection binding).
///
/// Takes the namespace lock, so no in-flight read-modify-write can straddle the
/// removal and write the wiped map back ([`lock_cred_file`]). A write that
/// *starts* after this returns still re-creates the namespace — but it then
/// contains only what that writer put there, never the resurrected old
/// identity, which is the difference between a harmless straggler and the
/// 403.
pub fn cred_file_remove(dir: &Path, app: &str) -> Result<(), anyhow::Error> {
    let _lock = lock_cred_file(dir, app);
    match std::fs::remove_file(cred_file_path(dir, app)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Read the first item matching an arbitrary Secret Service attribute set on
/// the default collection. `Ok(None)` when nothing matches. The shared core
/// under both [`load_field`] (the `(application, account)`-keyed lookup) and
/// callers with a different attribute shape — e.g. a single-key `application`
/// flag with no per-account dimension.
#[cfg(target_os = "linux")]
pub async fn first_secret_by_attrs(
    ss: &secret_service::SecretService<'_>,
    attrs: HashMap<&str, &str>,
) -> Result<Option<String>, anyhow::Error> {
    let collection = ss.get_default_collection().await?;
    let items = collection.search_items(attrs).await?;
    let item = match items.first() {
        Some(i) => i,
        None => return Ok(None),
    };
    let bytes = item.get_secret().await?;
    Ok(Some(String::from_utf8(bytes)?))
}

/// Read a single credential item by its `account` attribute. Returns
/// `Ok(None)` when the item is absent. Errors on D-Bus / libsecret failures.
#[cfg(target_os = "linux")]
pub async fn load_field(
    ss: &secret_service::SecretService<'_>,
    app: &str,
    account: &str,
) -> Result<Option<String>, anyhow::Error> {
    first_secret_by_attrs(ss, cred_attrs(app, account)).await
}

// Sync/async: [`SecretStore`] is synchronous but libsecret is async-only, so
// each keyring op runs on a freshly-spawned OS thread with its own
// current-thread runtime — safe to call from a tokio worker as well as a UI
// main thread, with no nested-runtime panic. (The macOS arm needs none of
// this: Security-framework calls are plain blocking C calls — `mac_keychain`.)

/// Read one item's value by (`application`, `account`) on a dedicated OS
/// thread. `None` on absent item or any error.
#[cfg(target_os = "linux")]
fn native_keyring_get(app: &str, account: &str) -> Option<String> {
    let app = app.to_string();
    let account = account.to_string();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .ok()?;
        rt.block_on(async {
            let ss = secret_service::SecretService::connect(secret_service::EncryptionType::Dh)
                .await
                .ok()?;
            load_field(&ss, &app, &account).await.ok().flatten()
        })
    })
    .join()
    .ok()
    .flatten()
}

/// Write one item (`create_item` with replace) on a dedicated OS thread.
/// Best-effort + log, matching every other libsecret write.
#[cfg(target_os = "linux")]
fn native_keyring_set(app: &str, account: &str, value: &str) {
    let app = app.to_string();
    let account = account.to_string();
    let value = value.to_string();
    let account_for_log = account.clone();
    let joined = std::thread::spawn(move || -> Result<(), anyhow::Error> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        rt.block_on(async {
            let ss =
                secret_service::SecretService::connect(secret_service::EncryptionType::Dh).await?;
            let collection = ss.get_default_collection().await?;
            collection
                .create_item(
                    &format!("Fauna Account \u{2014} {account}"),
                    cred_attrs(&app, &account),
                    value.as_bytes(),
                    true,
                    "text/plain",
                )
                .await?;
            Ok(())
        })
    })
    .join();
    match joined {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            tracing::warn!("[account-store] keyring set {account_for_log:?} failed: {e:#}")
        }
        Err(_) => tracing::warn!("[account-store] keyring set thread panicked"),
    }
}

/// Delete the item(s) matching (`application`, `account`) on a dedicated OS
/// thread. Best-effort + log. Removes only the one logical key's item — so
/// removing an account never touches its siblings' slots.
#[cfg(target_os = "linux")]
fn native_keyring_delete(app: &str, account: &str) {
    let app = app.to_string();
    let account = account.to_string();
    let account_for_log = account.clone();
    let joined = std::thread::spawn(move || -> Result<(), anyhow::Error> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        rt.block_on(async {
            let ss =
                secret_service::SecretService::connect(secret_service::EncryptionType::Dh).await?;
            let collection = ss.get_default_collection().await?;
            let items = collection.search_items(cred_attrs(&app, &account)).await?;
            for item in items {
                item.delete().await?;
            }
            Ok(())
        })
    })
    .join();
    match joined {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            tracing::warn!("[account-store] keyring delete {account_for_log:?} failed: {e:#}")
        }
        Err(_) => tracing::warn!("[account-store] keyring delete thread panicked"),
    }
}

/// How many search→delete passes [`keyring_delete_namespace`] will make before
/// giving up. Each pass is one round trip per surviving item (D-Bus on the
/// Secret Service arm, a `SecItem` call on the macOS one); the writers it
/// races are bounded (a registry mutator is a handful of `set`s), so
/// convergence takes two or three. The cap only exists so a pathological writer
/// cannot spin a sign-out forever.
///
/// **The search→delete→retry shape is independently reimplemented three
/// times** (here, `mac_keychain::keyring_delete_namespace`,
/// `win_credman::keyring_delete_namespace`), scouted 2026-08-19 and
/// deliberately NOT unified: this arm's loop is async (tokio, D-Bus round
/// trips per pass) while the other two are sync, so a shared helper would
/// need to force a sync/async split anyway — the actual list/delete
/// primitives are unavoidably platform-specific either way, leaving little
/// for a generic wrapper to save. **Worth a second look, separately from the
/// dedup question:** the win_credman arm re-checks emptiness once more
/// *after* its `NAMESPACE_DELETE_PASSES` loop before deciding success/error,
/// while this arm and the macOS one return `Err` unconditionally if the loop
/// exhausts without an early "found it empty" exit — even when the LAST
/// pass's own deletes actually landed. That is a real asymmetry (a spurious
/// `Err` on an already-empty namespace), not just a style difference; flagged
/// here for whoever next touches this rather than fixed as part of a scout
/// pass.
pub(crate) const NAMESPACE_DELETE_PASSES: usize = 6;

/// Delete every item carrying `application = app`, on a dedicated OS thread.
/// The keyring arm of [`CredentialStore::delete_namespace`]; see
/// [`cred_file_remove`] for why a sign-out must clear the namespace *wholesale*.
///
/// Scoped to the exact `application` attribute, so the resume slots that
/// deliberately live in sibling namespaces (`<app>-pending-invite`,
/// `<app>-awaiting-manual-dns`, …) survive a sign-out, as their clients intend.
///
/// **Searches again until the namespace reads empty**, and returns `Err` if it
/// never does. libsecret offers no delete-by-attribute and no transaction, so a
/// single search→delete pass deletes only a *snapshot*: any item written after
/// the search survives the wipe. That is not hypothetical — `AccountRegistry`'s
/// since-retired lazy migration was four sequential `set`s triggered by an
/// ordinary `index()` read, each a D-Bus round trip, and a sign-out landing
/// inside that window left the per-actor secret and `fauna/index` behind: credentials
/// outliving the sign-out that was supposed to destroy them, plus the 403-hang
/// [`cred_file_remove`] describes. Verifying beats assuming. (The deeper fix —
/// quiescing the writers before the wipe — belongs to each app's reset
/// teardown; this makes the primitive itself honest either way.)
///
/// A single delete failure never aborts the sweep: the remaining items still get
/// their pass, and the final search is what decides success.
#[cfg(target_os = "linux")]
fn native_keyring_delete_namespace(app: &str) -> Result<(), anyhow::Error> {
    let app = app.to_string();
    std::thread::spawn(move || -> Result<(), anyhow::Error> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        rt.block_on(async {
            let ss =
                secret_service::SecretService::connect(secret_service::EncryptionType::Dh).await?;
            let collection = ss.get_default_collection().await?;
            for _ in 0..NAMESPACE_DELETE_PASSES {
                let items = collection
                    .search_items([("application", app.as_str())].into_iter().collect())
                    .await?;
                if items.is_empty() {
                    return Ok(());
                }
                for item in items {
                    if let Err(e) = item.delete().await {
                        tracing::warn!("[account-store] namespace delete: item failed: {e:#}");
                    }
                }
            }
            Err(anyhow::anyhow!(
                "[account-store] namespace {app:?} still holds items after \
                 {NAMESPACE_DELETE_PASSES} delete passes — a concurrent writer is \
                 re-creating them"
            ))
        })
    })
    .join()
    .map_err(|_| anyhow::anyhow!("[account-store] keyring namespace-delete thread panicked"))?
}

/// Probe whether a usable freedesktop Secret Service is reachable: connect,
/// resolve the default collection, and require it **unlocked**. On a headless
/// box (no session bus) the connect fails fast; on a box whose gnome-keyring
/// default collection is locked, writes would fail `IsLocked` even though
/// `connect` succeeds (the documented file-fallback motivation above) — so a
/// locked collection reads as *unreachable* here. Dedicated OS thread, like
/// every other keyring op. (The macOS analogue — default keychain present +
/// unlocked + writable — lives in [`mac_keychain::keyring_probe`].)
#[cfg(target_os = "linux")]
fn native_keyring_probe() -> bool {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .ok()?;
        rt.block_on(async {
            let ss = secret_service::SecretService::connect(secret_service::EncryptionType::Dh)
                .await
                .ok()?;
            let collection = ss.get_default_collection().await.ok()?;
            match collection.is_locked().await {
                Ok(locked) => Some(!locked),
                Err(_) => None,
            }
        })
    })
    .join()
    .ok()
    .flatten()
    .unwrap_or(false)
}

/// Test-only carve-out (the `FAUNA_E2E_CREDENTIAL_DIR` family): force the
/// headless-fallback resolution to skip the keyring probe and select the
/// sealed passphrase backend, so an e2e run on a desktop dev box (where the
/// probe would succeed) can drive the headless arm deterministically. Never a
/// user/admin knob — production never sets it, so the read is compile-gated
/// out of release artifacts with [`cred_file_dir`] (convention 15; same pair
/// shape, same family).
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
pub fn force_headless_store() -> bool {
    std::env::var("FAUNA_E2E_FORCE_HEADLESS_STORE").is_ok_and(|v| !v.is_empty() && v != "0")
}

/// Production twin: a shipped build never consults the harness environment —
/// backend resolution runs on data-present and the keyring probe alone.
#[cfg(not(any(test, debug_assertions, feature = "e2e-agent")))]
pub fn force_headless_store() -> bool {
    false
}

/// The backend for [`CredentialStore`], resolved once at construction.
enum SecretStoreBackend {
    /// `FAUNA_E2E_CREDENTIAL_DIR` file store — a generic per-namespace map.
    File(PathBuf),
    /// The OS-native secure store (the desktop-session default): the session
    /// Secret Service, or the macOS login Keychain ([`mac_keychain`]).
    Keyring,
    /// The passphrase-encrypted sealed file ([`sealed::SealedFileStore`]) —
    /// the headless arm (`apps/tui.md` § Credential storage). Constructed
    /// locked; the client unlocks/creates it before launch routing runs.
    Sealed(sealed::SealedFileStore),
    /// The app's own platform secure store, lent over the foreign seam
    /// ([`install_foreign_store`]) — the phones' arm, where the native one is
    /// inert. Keys are namespace-prefixed ([`foreign_key`]).
    Foreign(Arc<dyn SecretStore>),
}

/// Pick the backend for a namespace from the three inputs that decide it —
/// pure, so the whole matrix is pinned on every host rather than only on the
/// target each arm happens to compile for.
///
/// 1. **No native arm + a foreign store installed → Foreign, and it outranks
///    the e2e redirect.** On such a target the app's store is the *only*
///    store, and the app's own e2e carve-out (apple's in-memory keychain,
///    android's file backend — both selected by the same harness variables)
///    is what keeps a test run off the device's real store; the Rust redirect
///    exists to keep the *native* arm out of a developer's keyring, and there
///    is no native arm here. Letting the redirect win would make every phone
///    e2e exercise a file the production build can never reach and leave the
///    real seam — the one that was silently inert until 2026-08-26 — with no
///    e2e witness at all.
/// 2. Otherwise the redirect (test-capable builds only) → File.
/// 3. Otherwise → Keyring: the native arm, or [`no_keyring`]'s loud inert one
///    on a phone whose shell never installed its store — a wiring bug that
///    then surfaces as the writer-key read-back refusal, exactly as before.
fn resolve_backend(
    has_native_arm: bool,
    redirect: Option<PathBuf>,
    foreign: Option<Arc<dyn SecretStore>>,
) -> SecretStoreBackend {
    match (has_native_arm, redirect, foreign) {
        (false, _, Some(store)) => SecretStoreBackend::Foreign(store),
        (_, Some(dir), _) => SecretStoreBackend::File(dir),
        _ => SecretStoreBackend::Keyring,
    }
}

/// The `fauna_client_accounts::SecretStore` both direct-Rust desktop apps
/// use — thin glue over the credential primitives above, four backends (e2e
/// file / libsecret keyring / sealed passphrase file / the app's foreign
/// store). Construct with [`CredentialStore::new`] (desktop-only clients and
/// the shared account-store slot) or
/// [`CredentialStore::new_with_headless_fallback`] (tui) and hand to
/// `AccountRegistry::new` (whose `launch_persistence()` mints the launch
/// adapter — there is no store-taking adapter constructor, so the registry's
/// mutation-lock choice always travels with it).
pub struct CredentialStore {
    app: String,
    backend: SecretStoreBackend,
}

impl CredentialStore {
    /// Production constructor: namespace from `FAUNA_KEYRING_APP` or the
    /// client's default (`fauna-desktop` / `fauna-tui` / the account-store
    /// slot's), backend per [`resolve_backend`] — the app's foreign store on a
    /// target with no native arm, else the `FAUNA_E2E_CREDENTIAL_DIR` file,
    /// else the native keyring.
    ///
    /// No headless arm: a GTK client always runs inside a desktop session and
    /// has no unlock surface, so its keyring failures should stay loud
    /// warnings rather than silently switching stores.
    pub fn new(default_app: &str) -> Self {
        Self::for_namespace(resolved_namespace(default_app))
    }

    /// Env-routed backend over an **explicit** namespace, bypassing
    /// `FAUNA_KEYRING_APP`. For callers that already hold the namespace — a
    /// per-run e2e sweep, or a test exercising namespace isolation env-free.
    pub fn for_namespace(app: impl Into<String>) -> Self {
        let app = app.into();
        let backend = resolve_backend(HAS_NATIVE_KEYRING_ARM, cred_file_dir(), foreign_store());
        if matches!(backend, SecretStoreBackend::Foreign(_)) {
            tracing::debug!(
                namespace = %app,
                "credential store: no native keyring arm on this target — the namespace \
                 rides the app's platform store over the foreign seam"
            );
        }
        Self { app, backend }
    }

    /// Foreign-backed store over an explicit namespace + store — the twin of
    /// [`Self::with_file_backend`] for tests that pin the foreign arm without
    /// touching the process-global install, and for the `fauna-ffi` export's
    /// own round-trip pin.
    pub fn with_foreign_backend(app: impl Into<String>, store: Arc<dyn SecretStore>) -> Self {
        Self {
            app: app.into(),
            backend: SecretStoreBackend::Foreign(store),
        }
    }

    /// [`Self::new`] plus the **headless passphrase fallback** — the tui
    /// constructor (`apps/tui.md` § Credential storage). Resolution order,
    /// auto-detected (never a config knob — configuration invariant bucket 1):
    ///
    /// 1. `FAUNA_E2E_CREDENTIAL_DIR` set → the e2e **file** backend (as ever).
    /// 2. A sealed store file already exists in `data_dir` → **sealed**:
    ///    data-present wins, so a box that was headless when the identity was
    ///    stored can never silently switch to an empty keyring and strand it.
    /// 3. `FAUNA_E2E_FORCE_HEADLESS_STORE` (test-only carve-out) → **sealed**.
    /// 4. [`keyring_probe`] finds a usable Secret Service → **keyring** (the
    ///    desktop-session arm, byte-identical to [`Self::new`]).
    /// 5. Otherwise → **sealed** (the headless arm; the client shows its
    ///    unlock/create surface before launch routing reads anything).
    pub fn new_with_headless_fallback(default_app: &str, data_dir: PathBuf) -> Self {
        let app = resolved_namespace(default_app);
        if let Some(dir) = cred_file_dir() {
            return Self {
                app,
                backend: SecretStoreBackend::File(dir),
            };
        }
        let sealed_exists = sealed::sealed_file_path(&data_dir, &app).exists();
        let backend = if sealed_exists || force_headless_store() || !keyring_probe() {
            SecretStoreBackend::Sealed(sealed::SealedFileStore::new(app.clone(), data_dir))
        } else {
            SecretStoreBackend::Keyring
        };
        Self { app, backend }
    }

    /// File-backed store at an explicit dir + namespace — for tests that must
    /// not touch the process-global env or a D-Bus daemon.
    pub fn with_file_backend(app: impl Into<String>, dir: PathBuf) -> Self {
        Self {
            app: app.into(),
            backend: SecretStoreBackend::File(dir),
        }
    }

    /// Sealed-backed store at an explicit dir + namespace — the sealed twin of
    /// [`Self::with_file_backend`], for tests driving the unlock/create or
    /// change-passphrase surfaces env-free. Constructed locked, exactly like
    /// resolution arm ⑤ of [`Self::new_with_headless_fallback`].
    pub fn with_sealed_backend(app: impl Into<String>, dir: PathBuf) -> Self {
        let app = app.into();
        Self {
            backend: SecretStoreBackend::Sealed(sealed::SealedFileStore::new(app.clone(), dir)),
            app,
        }
    }

    /// The file backend's directory, or `None` when this store talks to
    /// libsecret or the sealed file. A test asserts on it to prove it can
    /// never sweep a developer's real keyring namespace.
    pub fn file_backend_dir(&self) -> Option<&Path> {
        match &self.backend {
            SecretStoreBackend::File(dir) => Some(dir),
            SecretStoreBackend::Keyring
            | SecretStoreBackend::Sealed(_)
            | SecretStoreBackend::Foreign(_) => None,
        }
    }

    /// Whether this store resolved to the app's foreign store — the phones'
    /// arm. A test asserts on it to pin the resolution order without a phone.
    pub fn is_foreign_backed(&self) -> bool {
        matches!(self.backend, SecretStoreBackend::Foreign(_))
    }

    /// Drop every credential in this store's namespace — the sign-out /
    /// factory-reset primitive. See [`cred_file_remove`] for why a partial
    /// clear (per-actor secrets without `fauna/index`) 403-hangs the next
    /// sign-in. The sealed arm also **relocks** — the next onboarding
    /// re-chooses its passphrase ([`sealed::SealedFileStore::delete_namespace`]).
    ///
    /// **The foreign arm refuses**: the seam is get/set/delete over logical
    /// keys with no enumeration, so a namespace cannot be swept from here.
    /// That is not a gap on the phones — their factory reset sweeps the whole
    /// platform store app-side (apple `KeychainStore.deleteAll`, android's
    /// backend clear), prefixed rows included — and no phone caller exists
    /// today; an `Err` keeps a future one from believing it wiped anything.
    pub fn delete_namespace(&self) -> Result<(), anyhow::Error> {
        match &self.backend {
            SecretStoreBackend::File(dir) => cred_file_remove(dir, &self.app),
            SecretStoreBackend::Keyring => keyring_delete_namespace(&self.app),
            SecretStoreBackend::Sealed(store) => store.delete_namespace(),
            SecretStoreBackend::Foreign(_) => Err(anyhow::anyhow!(
                "[account-store] namespace {:?} rides the app's foreign store, which \
                 cannot enumerate — sweep it app-side (the platform store's own reset)",
                self.app
            )),
        }
    }

    /// The sealed passphrase backend, when this store resolved to it — the
    /// client's unlock/create surface drives it ([`sealed::SealedFileStore`]).
    /// `None` on the file and keyring arms, which need no unlock.
    pub fn sealed_backend(&self) -> Option<&sealed::SealedFileStore> {
        match &self.backend {
            SecretStoreBackend::Sealed(store) => Some(store),
            _ => None,
        }
    }

    /// Whether the store cannot serve reads/writes until the client's
    /// unlock/create surface has run ([`sealed::SealedFileStore::is_locked`]).
    pub fn needs_unlock(&self) -> bool {
        self.sealed_backend().is_some_and(|s| s.is_locked())
    }
}

/// Every registry logical key is the native `account` attribute **verbatim**.
/// The pre-multi-account single-slot items (`secret_key` / `node_url` /
/// `device_id` / `handle` / `domain` / `tier`) that the registry's `legacy/*`
/// keys once mapped onto are no longer read or written by linux or tui
/// (2026-09-24, `long-term-store.md` § Downgrade mirror + abandoned-append
/// recovery): the wizard commits to the registry and every reader takes the
/// registry's session material.
impl SecretStore for CredentialStore {
    fn get(&self, key: &str) -> Option<String> {
        let account = key;
        match &self.backend {
            SecretStoreBackend::File(dir) => cred_file_read(dir, &self.app).get(account).cloned(),
            SecretStoreBackend::Keyring => keyring_get(&self.app, account),
            SecretStoreBackend::Sealed(store) => store.get(account),
            SecretStoreBackend::Foreign(store) => store.get(&foreign_key(&self.app, account)),
        }
    }

    fn set(&self, key: &str, value: &str) {
        let account = key;
        match &self.backend {
            SecretStoreBackend::File(dir) => {
                // The lock spans the read AND the write: an unserialized RMW
                // loses the other writer's key, or resurrects a namespace a
                // concurrent `delete_namespace` just wiped — see
                // [`lock_cred_file`].
                let _lock = lock_cred_file(dir, &self.app);
                #[cfg(test)]
                tests::run_rmw_hook(dir, &self.app);
                let mut map = cred_file_read(dir, &self.app);
                map.insert(account.to_string(), value.to_string());
                if let Err(e) = cred_file_write(dir, &self.app, &map) {
                    tracing::warn!("[account-store] folder {account:?} failed: {e:#}");
                }
            }
            SecretStoreBackend::Keyring => keyring_set(&self.app, account, value),
            SecretStoreBackend::Sealed(store) => store.set(account, value),
            SecretStoreBackend::Foreign(store) => {
                store.set(&foreign_key(&self.app, account), value);
            }
        }
    }

    fn delete(&self, key: &str) {
        let account = key;
        match &self.backend {
            SecretStoreBackend::File(dir) => {
                let _lock = lock_cred_file(dir, &self.app);
                let mut map = cred_file_read(dir, &self.app);
                if map.remove(account).is_some()
                    && let Err(e) = cred_file_write(dir, &self.app, &map)
                {
                    tracing::warn!("[account-store] file delete {account:?} failed: {e:#}");
                }
            }
            SecretStoreBackend::Keyring => keyring_delete(&self.app, account),
            SecretStoreBackend::Sealed(store) => store.delete(account),
            SecretStoreBackend::Foreign(store) => store.delete(&foreign_key(&self.app, account)),
        }
    }
}

#[cfg(test)]
mod tests {
    //! File-backed round-trip + registry-migration tests. They drive the FILE
    //! backend with an explicit per-test dir (no `FAUNA_E2E_CREDENTIAL_DIR`
    //! env, no D-Bus daemon), so they're deterministic under a parallel runner.
    use super::*;
    use fauna_client_accounts::{AccountRegistry, SecretStore};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn fresh_dir() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut d = std::env::temp_dir();
        d.push(format!(
            "fauna-credential-store-test-{}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn file_store(dir: PathBuf) -> CredentialStore {
        CredentialStore::with_file_backend("fauna-desktop", dir)
    }

    /// The e2e namespace-collapse fix, pinned env-free: one override value
    /// must resolve to two distinct namespaces (app vs. account-store), and a
    /// third default_app must pass the override through verbatim — including
    /// the sync agent's own `fauna-sync-agent` namespace, which this
    /// derivation deliberately does NOT special-case (`apps/sync-agent.md`
    /// § Credential model owns that separate, still-collapsing concern).
    #[test]
    fn account_store_namespace_derives_from_the_override_instead_of_reusing_it() {
        let over = Some("fauna-e2e-agent-9".to_string());
        assert_eq!(
            apply_namespace_override(ACCOUNT_STORE_NAMESPACE, over.clone()),
            "fauna-e2e-agent-9-account-store",
            "the account-store namespace must derive its own override, not reuse the app's"
        );
        assert_eq!(
            apply_namespace_override("fauna-desktop", over.clone()),
            "fauna-e2e-agent-9",
            "the app's own namespace keeps resolving to the override verbatim"
        );
        assert_eq!(
            apply_namespace_override("fauna-sync-agent", over),
            "fauna-e2e-agent-9",
            "the sync agent's own namespace still collapses onto the app's under e2e — \
             deliberate, unrelated to this derivation (long-term-store.md:237-238)"
        );
    }

    #[test]
    fn namespace_override_absent_passes_default_app_through_unchanged() {
        assert_eq!(
            apply_namespace_override(ACCOUNT_STORE_NAMESPACE, None),
            ACCOUNT_STORE_NAMESPACE,
            "production (no harness override) must be untouched by this derivation"
        );
        assert_eq!(
            apply_namespace_override("fauna-desktop", None),
            "fauna-desktop"
        );
    }

    // Any 32 bytes is a valid Ed25519 secret.
    const SECRET_A: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const SECRET_B: &str = "2222222222222222222222222222222222222222222222222222222222222222";

    // -----------------------------------------------------------------------
    // The mid-RMW seam
    //
    // `SecretStore::set`'s file arm calls [`run_rmw_hook`] between taking the
    // namespace lock and reading the map, so a test can observe the critical
    // section from *inside* it. That is what makes the exclusion assertion
    // below deterministic instead of a scheduling race: no threads, no sleeps,
    // no "run it 200 times and hope" (testing.md convention 14 — assert
    // latency-independent state, never wall-clock timing).
    //
    // `#[cfg(test)]` on both the hook and its call site, so a shipped build has
    // neither the static nor the branch.
    // -----------------------------------------------------------------------
    // -----------------------------------------------------------------------
    // Test redirect for the ENV-ROUTED constructors
    // -----------------------------------------------------------------------
    // `CredentialStore::new` resolves its backend from the environment, and the
    // erase must resolve the account-store namespace *exactly the way its
    // writer does* (`production_credential_store()` is the same constructor) —
    // so a test cannot simply hand the sweep a file-pinned store and still be
    // asserting the production path. Setting `FAUNA_E2E_CREDENTIAL_DIR` would
    // work but the process environment is shared by the whole test binary.
    //
    // A THREAD-LOCAL is the right scope: libtest runs each test on its own
    // thread, so a redirect set here reaches every `cred_file_dir()` call the
    // test makes — including the ones inside `AccountRegistry::clear_all` —
    // and no other test's. `#[cfg(test)]` on both the cell and its call site,
    // so a shipped build has neither.
    thread_local! {
        static CRED_DIR: std::cell::RefCell<Option<PathBuf>> =
            const { std::cell::RefCell::new(None) };
    }

    /// The redirect this test thread pinned, if any — read by `cred_file_dir`.
    pub(super) fn test_cred_dir() -> Option<PathBuf> {
        CRED_DIR.with(|c| c.borrow().clone())
    }

    /// Route every env-routed `CredentialStore` on THIS thread at `dir`.
    fn pin_cred_dir(dir: &Path) {
        CRED_DIR.with(|c| *c.borrow_mut() = Some(dir.to_path_buf()));
    }

    type RmwHook = Box<dyn Fn(&Path, &str)>;
    thread_local! {
        // Thread-local, not a shared `static`: `run_rmw_hook` fires on EVERY
        // file-arm `set` across every test in this suite (any test using
        // `file_store`), so a process-global slot races the test that armed it
        // against every sibling test's own `set` calls on other threads —
        // whichever reaches the hook first steals (`.take()`s) it, leaving the
        // arming test's own call to find nothing and silently pass its guard
        // unexercised. Scoping per-thread means only this test's own thread
        // ever sees the hook it set (measured: reproducible only under a full
        // `cargo test --workspace` run's heavier scheduling, never isolated).
        static RMW_HOOK: std::cell::RefCell<Option<RmwHook>> = const { std::cell::RefCell::new(None) };
    }

    /// Called from inside the file arm's locked read-modify-write.
    pub(super) fn run_rmw_hook(dir: &Path, app: &str) {
        // Take the hook OUT for the call: a hook that itself drives a store
        // operation would otherwise re-enter and recurse forever.
        let hook = RMW_HOOK.with(|cell| cell.borrow_mut().take());
        if let Some(h) = hook {
            h(dir, app);
        }
    }

    fn set_rmw_hook(h: impl Fn(&Path, &str) + 'static) {
        RMW_HOOK.with(|cell| *cell.borrow_mut() = Some(Box::new(h)));
    }

    /// A file-arm write must hold the namespace lock across BOTH halves of its
    /// read-modify-write — the property the 403 turned on.
    ///
    /// Unlocked, the window between `cred_file_read` and `cred_file_write` is
    /// open to every other writer of the same namespace: another thread of this
    /// client, or (under e2e, where `FAUNA_KEYRING_APP` collapses the two
    /// namespaces onto one file) the separate `fauna-sync-agent` process, which
    /// was measured writing this file twice within 40 ms of a reset's wipe. A
    /// write whose read preceded the wipe and whose write followed it put the
    /// SIGNED-OUT actor's secret and `fauna/index` back, so the next
    /// login minted a bearer for the previous actor while its `AuthClient` held
    /// the new one's keypair — `403` at the WS upgrade, measured 2026-08-19:
    /// `bearer_actor=72b24f0a… path_actor=34f4883e…`, the two consecutive
    /// tests' actors.
    #[test]
    fn a_file_arm_write_holds_the_namespace_lock_across_its_read_modify_write() {
        let dir = fresh_dir();
        let store = file_store(dir.clone());
        let excluded = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let seen = Arc::clone(&excluded);
        set_rmw_hook(move |dir, app| {
            // A fresh handle on the same lock file, from inside the critical
            // section. `try_lock` must report the lock already held.
            let f = fauna_core::fs_lock::open_lock_file(&cred_file_lock_path(dir, app))
                .expect("lock file opens");
            seen.store(
                matches!(f.try_lock(), Err(std::fs::TryLockError::WouldBlock)),
                Ordering::SeqCst,
            );
        });
        store.set("fauna/abc/secret", SECRET_A);

        assert!(
            excluded.load(Ordering::SeqCst),
            "a file-arm `set` left its read-modify-write unserialized: a second \
             holder could take the namespace lock mid-write, which is how a \
             concurrent writer resurrects a wiped namespace"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// No write may be lost when writers overlap — the other face of the same
    /// missing critical section. Every thread writes its own key; afterwards
    /// every key must be readable. Unlocked, the last writer's whole-file
    /// rewrite drops the keys written since it read.
    ///
    /// A state invariant, not a timing assertion: it never sleeps and never
    /// waits on a clock, and its verdict is the same however the threads
    /// interleave.
    #[test]
    fn concurrent_writers_do_not_lose_each_others_keys() {
        let dir = fresh_dir();
        const THREADS: usize = 8;
        const PER_THREAD: usize = 25;

        let barrier = Arc::new(std::sync::Barrier::new(THREADS));
        let handles: Vec<_> = (0..THREADS)
            .map(|t| {
                let dir = dir.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let store = file_store(dir);
                    // Start together, so the writes genuinely overlap.
                    barrier.wait();
                    for i in 0..PER_THREAD {
                        store.set(&format!("k-{t}-{i}"), &format!("v-{t}-{i}"));
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().expect("writer thread");
        }

        let store = file_store(dir.clone());
        let missing: Vec<String> = (0..THREADS)
            .flat_map(|t| (0..PER_THREAD).map(move |i| format!("k-{t}-{i}")))
            .filter(|k| store.get(k).is_none())
            .collect();
        assert!(
            missing.is_empty(),
            "{} of {} concurrent writes were lost (e.g. {:?}) — the file arm's \
             read-modify-write is not serialized",
            missing.len(),
            THREADS * PER_THREAD,
            &missing[..missing.len().min(3)],
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn get_set_delete_round_trip() {
        let dir = fresh_dir();
        let store = file_store(dir.clone());

        assert_eq!(store.get("fauna/index"), None, "empty store reads None");
        store.set("fauna/index", r#"{"active":null,"accounts":[]}"#);
        assert_eq!(
            store.get("fauna/index").as_deref(),
            Some(r#"{"active":null,"accounts":[]}"#)
        );

        store.set("fauna/abc/secret", SECRET_A);
        assert_eq!(store.get("fauna/abc/secret").as_deref(), Some(SECRET_A));
        store.delete("fauna/abc/secret");
        assert_eq!(store.get("fauna/abc/secret"), None);
        // The sibling delete leaves the index untouched.
        assert!(store.get("fauna/index").is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cred_file_remove_clears_the_namespace_so_the_next_actor_becomes_active() {
        // The sign-out contract, over the FILE backend: clearing the namespace must
        // take the account index with it, not just the per-actor secret. Otherwise
        // `active` stays pinned to the signed-out actor, `RegistryLaunchPersistence`
        // serves that actor's secret to the next sign-in, and the resulting
        // bearer/keypair mismatch is rejected by the nest's WS handshake (403).
        let dir = fresh_dir();
        let app = "fauna-desktop";

        // Actor A signs in: the wizard's hand-off registers it as account #1,
        // which elects it active.
        let registry = AccountRegistry::new(Arc::new(file_store(dir.clone())));
        let actor_a = registry
            .add_account(SECRET_A, Some("https://a.example"), None)
            .expect("A registers");
        assert_eq!(registry.active().as_deref(), Some(actor_a.as_str()));

        // Sign out.
        cred_file_remove(&dir, app).unwrap();
        assert!(
            !cred_file_path(&dir, app).exists(),
            "the namespace file is gone"
        );
        assert_eq!(
            AccountRegistry::new(Arc::new(file_store(dir.clone()))).active(),
            None,
            "no active account survives a sign-out"
        );

        // Actor B signs in: a fresh registration must elect B, not resurrect A.
        let registry = AccountRegistry::new(Arc::new(file_store(dir.clone())));
        registry
            .add_account(SECRET_B, Some("https://b.example"), None)
            .expect("B registers");
        let actor_b = registry
            .active()
            .expect("B is active after the second registration");
        assert_ne!(actor_a, actor_b, "the two secrets derive distinct actors");

        // Removing an absent namespace is success, matching libsecret's
        // delete-nothing-found contract (a second sign-out must not error).
        cred_file_remove(&dir, "never-written").unwrap();

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_namespace_clears_the_store_it_was_built_over() {
        // The store-level sign-out primitive both direct-Rust clients call. Over
        // the file backend it must empty the namespace and stay infallible on a
        // second call (sign-out is idempotent: a crash between the wipe and the
        // UI transition must not brick the next attempt).
        let dir = fresh_dir();
        let store = file_store(dir.clone());
        store.set("fauna/abc/secret", SECRET_A);
        store.set("fauna/index", r#"{"active":"abc","accounts":[]}"#);
        assert!(cred_file_path(&dir, "fauna-desktop").exists());

        store.delete_namespace().unwrap();

        assert_eq!(store.get("fauna/index"), None, "the index is gone");
        assert_eq!(store.get("fauna/abc/secret"), None);
        store.delete_namespace().unwrap();

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_namespace_survives_a_writer_landing_between_its_passes() {
        // The libsecret arm searches, deletes, and searches again (a namespace
        // delete is not atomic against a concurrent write). The file arm gets
        // that property for free — one `remove_file` — but the contract both
        // arms promise is the same: after `delete_namespace` returns `Ok`, the
        // namespace reads empty. A write that lands *after* the call resurrects
        // it, which is exactly why every app's reset drops its writers first.
        let dir = fresh_dir();
        let store = file_store(dir.clone());
        store.set("fauna/abc/secret", SECRET_A);

        store.delete_namespace().unwrap();
        assert!(!cred_file_path(&dir, "fauna-desktop").exists());

        // A straggler write re-creates the namespace — the observable shape of
        // the race, and why the wipe must be the *last* thing a reset does.
        store.set("fauna/index", r#"{"active":"abc","accounts":[]}"#);
        assert!(cred_file_path(&dir, "fauna-desktop").exists());
        store.delete_namespace().unwrap();
        assert_eq!(store.get("fauna/index"), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_backed_store_reports_its_dir_and_a_keyring_one_does_not() {
        // `file_backend_dir()` is how a client's unit tests prove their store can
        // never sweep a developer's real keyring namespace. It must not lie.
        let dir = fresh_dir();
        assert_eq!(
            file_store(dir.clone()).file_backend_dir(),
            Some(dir.as_path())
        );
        // `for_namespace` routes on `FAUNA_E2E_CREDENTIAL_DIR` alone — asserted
        // against `cred_file_dir()` rather than a hard `None`, so the test states
        // the routing contract instead of depending on the runner's ambient env.
        // Constructing either backend touches no D-Bus (the connection is
        // per-op, not per-store).
        assert_eq!(
            CredentialStore::for_namespace("fauna-desktop").file_backend_dir(),
            cred_file_dir().as_deref(),
            "for_namespace's backend follows FAUNA_E2E_CREDENTIAL_DIR"
        );
    }

    // -----------------------------------------------------------------------
    // The foreign arm — the phones' store, lent over the registry's seam
    // -----------------------------------------------------------------------

    /// An in-memory `SecretStore` standing in for the Swift Keychain /
    /// android `EncryptedSharedPreferences` behind the foreign seam.
    #[derive(Default)]
    struct MapStore(std::sync::Mutex<BTreeMap<String, String>>);

    impl SecretStore for MapStore {
        fn get(&self, key: &str) -> Option<String> {
            self.0.lock().unwrap().get(key).cloned()
        }
        fn set(&self, key: &str, value: &str) {
            self.0
                .lock()
                .unwrap()
                .insert(key.to_string(), value.to_string());
        }
        fn delete(&self, key: &str) {
            self.0.lock().unwrap().remove(key);
        }
    }

    /// A store that swallows every write — `no_keyring`'s shape, and what a
    /// platform store does under a denied ACL or a locked keychain.
    struct DroppingStore;

    impl SecretStore for DroppingStore {
        fn get(&self, _key: &str) -> Option<String> {
            None
        }
        fn set(&self, _key: &str, _value: &str) {}
        fn delete(&self, _key: &str) {}
    }

    /// One row of [`backend_resolution_matrix`]: whether the host has a native
    /// arm, the e2e redirect, the store the app lent, and the arm those three
    /// must resolve to.
    type ResolutionCase = (
        bool,
        Option<PathBuf>,
        Option<Arc<dyn SecretStore>>,
        &'static str,
    );

    fn arm_of(backend: &SecretStoreBackend) -> &'static str {
        match backend {
            SecretStoreBackend::File(_) => "file",
            SecretStoreBackend::Keyring => "keyring",
            SecretStoreBackend::Sealed(_) => "sealed",
            SecretStoreBackend::Foreign(_) => "foreign",
        }
    }

    /// The whole resolution matrix, on every host. The one row that carries
    /// the 2026-08-26 ruling is `(no native arm, redirect set, foreign
    /// installed) → foreign`: on a phone the app's store outranks the e2e
    /// redirect, so the phone e2e exercises the production seam rather than
    /// a file the shipped build can never reach. Every other row is the
    /// pre-existing order, unchanged.
    #[test]
    fn backend_resolution_matrix() {
        let dir = PathBuf::from("/e2e/creds");
        let foreign: Arc<dyn SecretStore> = Arc::new(MapStore::default());
        let cases: [ResolutionCase; 8] = [
            // A desktop target: the foreign store is never consulted.
            (true, None, None, "keyring"),
            (true, Some(dir.clone()), None, "file"),
            (true, None, Some(Arc::clone(&foreign)), "keyring"),
            (true, Some(dir.clone()), Some(Arc::clone(&foreign)), "file"),
            // A phone target: the app's store, whenever it lent one.
            (false, None, None, "keyring"),
            (false, Some(dir.clone()), None, "file"),
            (false, None, Some(Arc::clone(&foreign)), "foreign"),
            (false, Some(dir), Some(foreign), "foreign"),
        ];
        for (has_native_arm, redirect, foreign, expected) in cases {
            let got = arm_of(&resolve_backend(
                has_native_arm,
                redirect.clone(),
                foreign.clone(),
            ));
            assert_eq!(
                got,
                expected,
                "resolve_backend(native_arm={has_native_arm}, redirect={}, foreign={})",
                redirect.is_some(),
                foreign.is_some(),
            );
        }
    }

    /// The production shape on a phone, end to end over the seam: a slot
    /// written through the store reads back through the same store, and the
    /// row the platform store sees is namespace-prefixed.
    #[test]
    fn foreign_arm_round_trips_and_prefixes_the_namespace() {
        let platform = Arc::new(MapStore::default());
        let store = CredentialStore::with_foreign_backend(
            "fauna-account-store",
            Arc::clone(&platform) as Arc<dyn SecretStore>,
        );
        assert!(store.is_foreign_backed());
        assert_eq!(store.file_backend_dir(), None);
        assert_eq!(store.get(SECRET_A), None, "empty store reads None");

        store.set(SECRET_A, SECRET_B);
        assert_eq!(store.get(SECRET_A).as_deref(), Some(SECRET_B));
        assert_eq!(
            platform
                .get(&format!("fauna-account-store/{SECRET_A}"))
                .as_deref(),
            Some(SECRET_B),
            "the platform store holds the row under `<namespace>/<account>`"
        );
        assert_eq!(
            platform.get(SECRET_A),
            None,
            "never under the bare account attribute"
        );

        store.delete(SECRET_A);
        assert_eq!(store.get(SECRET_A), None);
        assert!(platform.0.lock().unwrap().is_empty());
    }

    /// Two namespaces keying their item by the SAME account attribute — the
    /// account-store slot and the sync agent's both use the actor id hex —
    /// must not overwrite each other in one flat platform store. Unprefixed,
    /// this test's second `set` would clobber the first.
    #[test]
    fn foreign_arm_keeps_namespaces_apart_over_one_platform_store() {
        let platform: Arc<dyn SecretStore> = Arc::new(MapStore::default());
        let actor = SECRET_A;
        let account_store =
            CredentialStore::with_foreign_backend("fauna-account-store", Arc::clone(&platform));
        let agent =
            CredentialStore::with_foreign_backend("fauna-sync-agent", Arc::clone(&platform));

        account_store.set(actor, "writer-key");
        agent.set(actor, "principal-bundle");

        assert_eq!(account_store.get(actor).as_deref(), Some("writer-key"));
        assert_eq!(agent.get(actor).as_deref(), Some("principal-bundle"));
        agent.delete(actor);
        assert_eq!(
            account_store.get(actor).as_deref(),
            Some("writer-key"),
            "deleting the agent's item leaves the store's writer key alone"
        );
    }

    /// A foreign namespace cannot be swept from Rust — the seam has no
    /// enumeration — and the primitive says so instead of reporting a wipe
    /// that never happened.
    #[test]
    fn foreign_arm_refuses_a_namespace_sweep_it_cannot_perform() {
        let platform: Arc<dyn SecretStore> = Arc::new(MapStore::default());
        let store =
            CredentialStore::with_foreign_backend("fauna-account-store", Arc::clone(&platform));
        store.set(SECRET_A, SECRET_B);
        let err = store
            .delete_namespace()
            .expect_err("a foreign namespace sweep must not claim success");
        assert!(
            err.to_string().contains("cannot enumerate"),
            "the refusal names why: {err:#}"
        );
        assert_eq!(
            store.get(SECRET_A).as_deref(),
            Some(SECRET_B),
            "and nothing was touched"
        );
    }

    /// A platform store that drops writes reads back nothing — the exact
    /// observable the writer-key mint's read-back refuses on. Pinned here so
    /// the foreign arm never papers over a swallowed write with a cached
    /// value of its own.
    #[test]
    fn foreign_arm_reports_a_dropped_write_honestly() {
        let store =
            CredentialStore::with_foreign_backend("fauna-account-store", Arc::new(DroppingStore));
        store.set(SECRET_A, SECRET_B);
        assert_eq!(
            store.get(SECRET_A),
            None,
            "a swallowed write must read back as absent, never as the value just written"
        );
    }

    /// The process-global install is what `fauna-ffi`'s
    /// `install_platform_credential_store` lands; `foreign_store()` is how
    /// resolution (and that export's own pin) reads it. Last install wins,
    /// matching the pin store's contract.
    #[test]
    fn install_foreign_store_is_process_global_and_last_wins() {
        let first: Arc<dyn SecretStore> = Arc::new(MapStore::default());
        let second: Arc<dyn SecretStore> = Arc::new(MapStore::default());
        install_foreign_store(Arc::clone(&first));
        assert!(Arc::ptr_eq(&foreign_store().expect("installed"), &first));
        install_foreign_store(Arc::clone(&second));
        assert!(Arc::ptr_eq(&foreign_store().expect("installed"), &second));
        // `for_namespace` on THIS host: a desktop target never routes to the
        // foreign store, however many are installed — the matrix above pins
        // the phone half without needing a phone.
        assert_eq!(
            CredentialStore::for_namespace("fauna-desktop").is_foreign_backed(),
            !HAS_NATIVE_KEYRING_ARM,
            "the foreign arm is selected exactly on targets with no native arm"
        );
    }

    /// The tui headless-fallback resolution over the REAL Windows Credential
    /// Manager arm (`apps/sync-agent.md` A2 win-verification rider,
    /// `apps/tui.md` § Credential storage): a fresh dir with no sealed file
    /// resolves to keyring (the credman probe arm resolves, no fallback), and
    /// once a sealed file exists — even though the probe would still succeed —
    /// it wins (data-present-wins, `new_with_headless_fallback`'s doc order).
    /// Neither construction touches a real credential item: `win_credman`'s
    /// probe is a constant `true` (Credential Manager has no lock state), and
    /// [`sealed_exists`](Self::new_with_headless_fallback) short-circuits the
    /// probe once a sealed file is present — so this is deterministic, no D-Bus
    /// / registry cleanup needed, unlike the libsecret arm's tests.
    #[test]
    #[cfg(target_os = "windows")]
    fn windows_credman_resolves_and_an_existing_sealed_file_still_wins() {
        let dir = fresh_dir();
        let app = format!("fauna-credman-headless-test-{}", std::process::id());

        let fresh = CredentialStore::new_with_headless_fallback(&app, dir.clone());
        assert!(
            fresh.sealed_backend().is_none(),
            "no sealed file yet: the credman probe arm resolves to keyring"
        );
        assert!(!fresh.needs_unlock(), "the keyring arm never needs unlock");

        sealed::SealedFileStore::new(app.clone(), dir.clone())
            .create("test-passphrase")
            .expect("creating the sealed file");

        let with_sealed = CredentialStore::new_with_headless_fallback(&app, dir.clone());
        assert!(
            with_sealed.sealed_backend().is_some(),
            "an existing sealed file wins over the keyring probe"
        );
        assert!(
            with_sealed.needs_unlock(),
            "a freshly-constructed store over an existing sealed file starts locked"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The five per-actor slots `fauna-sync-engine` writes into the shared
    /// account-store namespace — the four `principal_bundle` attributes and
    /// the bare-actor-id T10 writer key. Spelled out rather than imported:
    /// this crate is downstream of neither writer, and the point of the test
    /// is that the erase reaches these exact rows in the OTHER namespace.
    fn account_store_slots(actor: &str) -> [String; 5] {
        [
            format!("{actor}/device-auth"),
            format!("{actor}/backup-key"),
            format!("{actor}/generation-keys"),
            format!("{actor}/grant-registered"),
            actor.to_string(),
        ]
    }

    /// A registry over one namespace, seeded with the account-store namespace's
    /// five per-actor slots — both stores built through the **env-routed**
    /// constructor production uses, with only the backend pinned at `dir`.
    ///
    /// Pinning the backend and not the namespace is the whole point. The erase
    /// has to resolve the account-store namespace exactly the way its writer
    /// does (`production_credential_store()` is literally
    /// `CredentialStore::new(ACCOUNT_STORE_NAMESPACE)`), so handing the sweep a
    /// `with_file_backend` store would assert a path production never takes.
    fn seeded_split(dir: &Path) -> (AccountRegistry, CredentialStore, String) {
        assert!(
            std::env::var_os("FAUNA_KEYRING_APP").is_none(),
            "this fixture writes its fixture data straight into files named after \
             the literal `fauna-desktop`/`ACCOUNT_STORE_NAMESPACE` default_app \
             strings, bypassing CredentialStore — a set FAUNA_KEYRING_APP would \
             make the env-routed constructors below resolve to the override \
             (`apply_namespace_override`) instead and read nothing the fixture \
             wrote. Unset it to run this test."
        );
        pin_cred_dir(dir);
        let reg = account_registry(Arc::new(CredentialStore::new("fauna-desktop")));
        let actor = reg
            .add_account(SECRET_A, Some("https://a.example"), Some("dev-a"))
            .expect("A registers as account #1");
        assert_eq!(reg.active().as_deref(), Some(actor.as_str()), "A is active");

        let account_store = CredentialStore::new(ACCOUNT_STORE_NAMESPACE);
        for key in account_store_slots(&actor) {
            account_store.set(&key, "deadbeef");
        }
        assert!(
            account_store_slots(&actor)
                .iter()
                .all(|k| account_store.get(k).is_some()),
            "fixture: all five slots present before the erase"
        );
        (reg, account_store, actor)
    }

    fn account_store_survivors(store: &CredentialStore, actor: &str) -> Vec<String> {
        account_store_slots(actor)
            .into_iter()
            .filter(|k| store.get(k).is_some())
            .collect()
    }

    /// Sign-out must erase the account-store namespace too.
    ///
    /// **This is the production split, in-process and env-free.** Both
    /// env-routed constructors honour `FAUNA_KEYRING_APP` (`seeded_split`
    /// above refuses to run if it is set — see its assert), so this test pins
    /// the property with no override in play at all: `fauna-desktop` and
    /// `fauna-account-store` resolve to their literal default_app strings and
    /// only the *backend* is redirected, keeping the two namespaces two. An
    /// e2e now separately witnesses the same split under the harness, where an
    /// override IS in play
    /// (`tests/e2e-unified/tests/test_sign_out.py::test_sign_out_erases_the_credential_namespace`,
    /// `fauna_credential_store::apply_namespace_override` —
    /// `long-term-store.md` § Implementation status today, hole 3).
    #[test]
    fn clear_all_reaches_the_account_store_namespace() {
        let dir = fresh_dir();
        let (reg, account_store, actor) = seeded_split(&dir);

        let _ = reg.clear_all();

        let survivors = account_store_survivors(&account_store, &actor);
        assert!(
            survivors.is_empty(),
            "sign-out left identity-derived credentials in the {ACCOUNT_STORE_NAMESPACE:?} \
             namespace: {survivors:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The shared sign-out sequence over stores that take the erase: both
    /// namespaces empty, and the sweep says so — the ordinary sign-out keeps
    /// painting nothing.
    #[test]
    fn erase_all_credentials_is_clean_when_both_namespaces_take_the_erase() {
        let dir = fresh_dir();
        let (reg, account_store, actor) = seeded_split(&dir);
        let app_store = CredentialStore::new("fauna-desktop");

        let sweep = erase_all_credentials(&reg, &app_store);

        assert!(sweep.is_clean(), "{sweep:?}");
        assert!(account_store_survivors(&account_store, &actor).is_empty());
        assert_eq!(app_store.get(&format!("fauna/{actor}/secret")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A credential store that refuses the erase must come back as a
    /// DIRTY sweep naming the identity seed, with the wipe's own failure kept —
    /// not as a `warn!` beside a clean "Signed out". The fault is a read-only
    /// credential directory, the file arm's shape of a locked keyring: every
    /// rewrite and the wholesale remove fail, and the file still reads. The
    /// e2e twin drives the same fault through the app's own sign-out
    /// (`test_sign_out.py::test_a_sign_out_whose_credentials_cannot_be_erased_says_so`).
    #[cfg(unix)]
    #[test]
    fn erase_all_credentials_reports_what_a_store_refusing_the_erase_kept() {
        use std::os::unix::fs::PermissionsExt;

        let dir = fresh_dir();
        let (reg, _account_store, actor) = seeded_split(&dir);
        let app_store = CredentialStore::new("fauna-desktop");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).expect("chmod");

        // Probe: a process that writes through a read-only directory (root)
        // makes every assertion below pass for the wrong reason.
        let probe = dir.join(".fault-injection-probe");
        if std::fs::write(&probe, b"x").is_ok() {
            let _ = std::fs::remove_file(&probe);
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("back");
            eprintln!("skipping: this process can write through a read-only dir (root?)");
            return;
        }

        let sweep = erase_all_credentials(&reg, &app_store);
        // Restore before asserting, so a failing assert cannot strand the dir.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("back");

        assert!(!sweep.is_clean(), "a refused erase is not a clean one");
        assert!(
            sweep.wipe_failed,
            "the wholesale remove failed and must be kept, not logged away: {sweep:?}"
        );
        assert!(
            sweep.survivors.contains(&format!("fauna/{actor}/secret")),
            "the identity seed is still readable and must be named: {sweep:?}"
        );
        assert!(
            sweep
                .survivors
                .iter()
                .any(|k| account_store_slots(&actor).contains(k)),
            "the account-store namespace refused too, and its slots must be named: {sweep:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The per-account twin: removing one account owes the same sweep for that
    /// actor — its writer key and principal bundle are as identity-derived as
    /// its secret (`long-term-store.md` § Cleanup contract property 3).
    #[test]
    fn remove_reaches_the_account_store_namespace() {
        let dir = fresh_dir();
        let (reg, account_store, actor) = seeded_split(&dir);

        reg.remove(&actor).expect("the active account is known");

        let survivors = account_store_survivors(&account_store, &actor);
        assert!(
            survivors.is_empty(),
            "account removal left identity-derived credentials in the \
             {ACCOUNT_STORE_NAMESPACE:?} namespace: {survivors:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The app's own namespace keeps its own erase — the aux sweep is additive,
    /// not a replacement (a regression that swapped the two stores would leave
    /// the identity secret behind while looking green above).
    #[test]
    fn clear_all_still_erases_the_app_namespace() {
        let dir = fresh_dir();
        let (reg, _account_store, actor) = seeded_split(&dir);
        let app_store = CredentialStore::new("fauna-desktop");
        assert!(
            app_store.get(&format!("fauna/{actor}/secret")).is_some(),
            "fixture: the identity secret is in the app namespace"
        );

        let _ = reg.clear_all();

        assert_eq!(
            app_store.get("fauna/index"),
            None,
            "the app namespace's account index survived sign-out"
        );
        assert_eq!(
            app_store.get(&format!("fauna/{actor}/secret")),
            None,
            "the app namespace's per-actor secret survived sign-out"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
