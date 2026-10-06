//! UniFFI façade for the shared multi-account registry
//! (`fauna_client_accounts`) — the Stage-1 prerequisite every native app's
//! account switcher and launch wiring consumes
//! (`docs/goal/architecture/long-term-store.md` § Multi-account evolution →
//! Shared seam).
//!
//! The platform implements ONLY [`FfiSecretStore`] — a logical-keyed
//! key/value secret store (Keychain / Credential Manager /
//! `EncryptedSharedPreferences` glue) — and everything else is shared Rust:
//! the account index, the per-actor namespacing, and the `LaunchPersistence`
//! adapter the `LaunchMachine` routes on. One foreign seam, not two (priority #2); the
//! wasm twin is `fauna-wasm`'s `WasmAccountRegistry` over the same registry.
//!
//! [`FfiAccountEntry`] mirrors `fauna_client_accounts::AccountEntry` minus the
//! forward-compat `extra` map — not FFI-representable and never rendered;
//! the registry preserves it internally on every rewrite (the same mirror
//! convention as `account.rs`, where adding a field to the source struct is a
//! compile error in the `From` impl here).

use std::sync::Arc;

use fauna_client_accounts::{AccountEntry, AccountRegistry, SecretStore};
use fauna_core::secret::SecretString;
use fauna_launch_machine::{AccountIndexRefusal, LaunchPersistence, PendingProvisionStore};

use crate::{FfiError, general_err};

/// The account THIS process was launched **bound** to, if any — the
/// concurrent-instances launch wiring (`account-scoping.md` § Concurrent
/// instances). `None` is an ordinary primary launch on the active account.
///
/// Bucket-1 IPC, never a human-facing knob: the spawning instance sets it on
/// the child off the account the user picked in the switcher, so the *choice*
/// stays client UI (`principles.md` § the one configuration surface). Shared
/// rather than per-app so all seven read the same channel and the same
/// normalization (priority #1).
///
/// A client that gets `Some` must launch bound **or refuse** — never fall back
/// to a plain launch. Gate it with [`FfiAccountRegistry::bind_account`] (or,
/// on `ConfirmationRequired`, run the platform re-auth and
/// [`FfiAccountRegistry::bind_account_confirmed`]), then build the launch
/// machine over [`FfiAccountRegistry::bound_launch_persistence`].
#[uniffi::export]
pub fn requested_bound_account() -> Option<String> {
    fauna_client_accounts::requested_bound_account()
}

/// Held (OS login, account) single-instance lock
/// (`fauna_client_accounts::AccountInstanceLock`; `account-scoping.md`
/// § Concurrent instances). RAII: the platform holds it for the session's
/// lifetime and drops it (or dies) to release. An unheld lock is the degraded
/// acquire — the caller proceeds unguarded, and drop is a no-op.
///
/// Backed by a private [`fauna_client_accounts::SessionInstanceHolder`] rather
/// than the bare lock, so the raw-object route gets the holder's two duties the
/// sign-out door needs without a second implementation: the cross-app serving
/// lock at the shared store root beside the instance lock, and putting both
/// down for the length of the erase's probe and taking them again afterwards
/// ([`crate::sign_out_blocked`]'s `own_lock`). Without that release a lone
/// macOS window would meet its own reflection and refuse every sign-out.
#[derive(uniffi::Object)]
pub struct FfiAccountInstanceLock {
    holder: std::sync::Mutex<fauna_client_accounts::SessionInstanceHolder>,
}

#[uniffi::export]
impl FfiAccountInstanceLock {
    /// `false` is the degraded acquire: nothing is actually held, the caller
    /// proceeds unguarded (and should log so a silent guard gap is visible).
    pub fn is_held(&self) -> bool {
        self.holder().holds_lock()
    }
}

impl FfiAccountInstanceLock {
    fn holder(&self) -> std::sync::MutexGuard<'_, fauna_client_accounts::SessionInstanceHolder> {
        // Nothing panics while holding it; a poisoned guard still holds the lock.
        self.holder.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The account this lock serves — recorded on a degraded acquire too,
    /// because a degraded instance still serves out of that account's stores.
    pub(crate) fn serving_actor(&self) -> Option<String> {
        self.holder().serving_actor().map(str::to_string)
    }

    /// Run `probe` with this lock (and its store-root serving lock) put down,
    /// then take both again — `SessionInstanceHolder::without_own_lock` over
    /// this object, for a seat that holds its lock as a raw object rather than
    /// in the process-global holder.
    pub(crate) fn without_own_lock<T>(&self, probe: impl FnOnce() -> T) -> T {
        let mut holder = self.holder();
        match holder.serving_actor().map(str::to_string) {
            Some(actor) => holder.without_own_lock(&actor, probe),
            None => probe(),
        }
    }
}

/// Take the raw lock through a fresh holder; `None` is the refusal.
fn acquire_raw_lock(
    bases: fauna_client_accounts::ServingBases<'_>,
    actor_id_hex: &str,
    mode: fauna_client_accounts::ServingMode,
) -> Option<Arc<FfiAccountInstanceLock>> {
    use fauna_client_accounts::SessionInstanceOutcome;
    let mut holder = fauna_client_accounts::SessionInstanceHolder::new();
    // No binding: the raw route never carried bound-or-refuse (apple has no
    // bound launch), and a fresh holder can neither reuse nor swap.
    match holder.become_session_instance(bases, actor_id_hex, None, mode) {
        SessionInstanceOutcome::Refused(_) => None,
        SessionInstanceOutcome::Acquired
        | SessionInstanceOutcome::Reused
        | SessionInstanceOutcome::Degraded(_) => Some(Arc::new(FfiAccountInstanceLock {
            holder: std::sync::Mutex::new(holder),
        })),
    }
}

/// Take a **shared** serving lock on `actor_id_hex` under `state_base` — the
/// install-scoped directory the per-account state dirs hang off (apple's
/// `AccountStateDir.base`; the same base the registry's mutation lock uses).
/// Never blocks. This is the only raw-lock export
/// (`account-scoping.md` § Concurrent instances, the W5.6
/// (account-data-plane.md § Workstreams) retirement): any number of
/// same-account instances coexist, while
/// [`FfiAccountInstanceLock::is_held`]'s exclusive probe still reads "served"
/// against it.
///
/// `None` means a live exclusive holder (the sign-out probe's brief exclusive
/// acquire) refused the acquire: the caller must refuse to run as it,
/// terminally — never fall back onto a different account (the same contract
/// as a refused binding). `Some` is either the held lock or the degraded
/// open-and-proceed (I/O failure — `is_held()` distinguishes, so the platform
/// can log; the guard narrows a race, it must not widen a failure into a
/// client that cannot launch).
///
/// Acquire at the point the session account resolves, before opening any of
/// the account's scoped state; a same-account session rebuild must reuse the
/// lock it already holds rather than re-acquire (the kernel treats a second
/// handle as a competing owner).
///
/// Apple has no app-level guard of its own to re-key (§ Concurrent
/// instances), so its leg calls the raw lock directly rather than the shared
/// [`become_process_session_instance`] holder.
///
/// **It also declares the instance at the shared account-store root** (the
/// serving lock — § *An erase refuses while a sibling serves the account*), so
/// a sibling app's sign-out sees this one. `store_container_dir` is the same
/// argument the erase pair takes: `None` resolves the per-user platform root,
/// which is what a production shell passes unless it hosts its runtime from a
/// container; a test passes a temp dir, so it never writes a serving lock into
/// the developer's own root.
///
/// [`ServingMode::Concurrent`]: fauna_client_accounts::ServingMode::Concurrent
#[uniffi::export]
pub fn acquire_account_instance_lock_shared(
    state_base: String,
    actor_id_hex: String,
    store_container_dir: Option<String>,
) -> Option<Arc<FfiAccountInstanceLock>> {
    let store_root = crate::account_state::store_root_for(store_container_dir.map(Into::into));
    acquire_raw_lock(
        fauna_client_accounts::ServingBases {
            state_base: Some(std::path::Path::new(&state_base)),
            store_root: Some(store_root.as_path()),
        },
        &actor_id_hex,
        fauna_client_accounts::ServingMode::Concurrent,
    )
}

/// The launch-collision chooser's list: those of `actor_ids` that **no live
/// instance currently serves**, order preserved
/// (`fauna_client_accounts::AccountInstanceLock::not_currently_served`;
/// `account-scoping.md` § Concurrent instances → the colliding instance's
/// surface).
///
/// **Display-only.** An account free at probe time can be taken before the
/// human clicks, so arbitration stays at
/// [`acquire_account_instance_lock_shared`] — the pick must still go through
/// `bind_account` + acquire and handle a refusal. Failures (missing lock
/// file, I/O, malformed key) read as "not served": a hiccup narrows the
/// chooser's list rather than leaving the user with no way in.
///
/// Exported for **windows**, whose chooser leg reaches this over UniFFI;
/// linux and tui — the other two `launch_instance_chooser` platforms — depend
/// on `fauna-client-accounts` directly and call the Rust fn with no FFI hop.
#[uniffi::export]
pub fn accounts_not_currently_served(state_base: String, actor_ids: Vec<String>) -> Vec<String> {
    fauna_client_accounts::AccountInstanceLock::not_currently_served(
        std::path::Path::new(&state_base),
        &actor_ids,
    )
}

/// The seat's two platform hooks for [`resolve_focus_existing`]: its own
/// raise channel and the display-only lock probe.
#[uniffi::export(with_foreign)]
pub trait FfiFocusExistingSeat: Send + Sync {
    /// Send the platform activation to `actor_id`'s endpoint; `false` ⇒ the
    /// endpoint is unowned (windows: the per-account named event).
    fn try_raise(&self, actor_id: String) -> bool;
    /// Does a live instance hold `actor_id`'s instance lock? Normally the
    /// shared display-only probe over the install's state base.
    fn is_served(&self, actor_id: String) -> bool;
}

/// [`fauna_client_accounts::FocusExistingOutcome`] over UniFFI.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum FfiFocusExistingOutcome {
    /// The serving instance was raised; this process exits.
    Raised,
    /// Nothing serves the account any more; continue as a plain launch.
    NoLongerServed,
    /// Still served but unreachable; the seat says so on `error-message`.
    StillServedNoChannel,
}

impl From<fauna_client_accounts::FocusExistingOutcome> for FfiFocusExistingOutcome {
    fn from(outcome: fauna_client_accounts::FocusExistingOutcome) -> Self {
        use fauna_client_accounts::FocusExistingOutcome as O;
        match outcome {
            O::Raised => Self::Raised,
            O::NoLongerServed => Self::NoLongerServed,
            O::StillServedNoChannel => Self::StillServedNoChannel,
        }
    }
}

/// The launch-collision chooser's focus-existing decision
/// (`fauna_client_accounts::resolve_focus_existing`; `account-scoping.md`
/// § Concurrent instances → the raise channel's ratified degrade): try the
/// raise; if it did not land, re-probe the lock.
///
/// Exported for **windows**; linux and tui call the Rust fn with no FFI hop.
#[uniffi::export]
pub fn resolve_focus_existing(
    served_actor_id: String,
    seat: Arc<dyn FfiFocusExistingSeat>,
) -> FfiFocusExistingOutcome {
    fauna_client_accounts::resolve_focus_existing(
        &served_actor_id,
        |actor| seat.try_raise(actor.to_owned()),
        |actor| seat.is_served(actor.to_owned()),
    )
    .into()
}

/// The normalized per-account token every per-account **name** keys off
/// (`fauna_client_accounts::account_instance_token`; `account-scoping.md`
/// § Concurrent instances → *The per-(OS login, account) raise channel*).
/// `None` when the value is not one of ours after normalization.
///
/// Exported for **windows**, whose raise-channel leg names a per-account
/// activate event `Local\FaunaApp-Activate-<token>`; linux derives its D-Bus
/// name `social.fauna.fauna.a<token>` from the same fn with no FFI hop, and
/// the lock file `instance-<token>.lock` is named from it too. **Do not
/// re-derive the trim+lowercase in C#/Swift/Kotlin** — a raiser derives the
/// name from the account it wants and the server from the account it serves,
/// so any drift between two spellings degrades into an unowned name, which
/// reads exactly like "that instance died" rather than like a bug.
#[uniffi::export]
pub fn account_instance_token(actor_id_hex: String) -> Option<String> {
    fauna_client_accounts::account_instance_token(&actor_id_hex)
}

/// Why a launch proceeded **unguarded** — the degrade-open cases
/// (`fauna_client_accounts::InstanceDegrade`). The platform logs it: the goal
/// doc assigns that log to the client, which is why the shared crate reports
/// the degrade upward instead of taking a logging dependency.
#[derive(uniffi::Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfiInstanceDegrade {
    /// No resolvable install-scoped state base to key the lock file off.
    NoStateBase,
    /// The lock file could not be opened or locked (I/O failure, or an actor
    /// id that is not a well-formed key).
    LockUnavailable,
}

/// Why this process may not run (or keep running) as an account
/// (`fauna_client_accounts::InstanceRefusal`). **Terminal by contract** — a
/// refused instance never falls back onto a different account.
///
/// The one platform latitude the goal doc grants: a chooser platform
/// (windows, linux, tui) may render `launch_instance_chooser` for
/// [`AlreadyServed`](Self::AlreadyServed) on a **plain** launch instead of
/// exiting. A *bound* launch stays terminally refused either way — the
/// chooser is strictly a human affordance, and wired IPC must be
/// deterministic.
#[derive(uniffi::Enum, Debug, Clone, PartialEq, Eq)]
pub enum FfiInstanceRefusal {
    /// Another live process already serves this account.
    AlreadyServed,
    /// The process was launched bound to one account but the session resolved
    /// another — "launch bound or refuse".
    BoundMismatch {
        /// The account named by the binding.
        bound: String,
    },
}

/// The outcome of [`become_process_session_instance`] — the shared holder's
/// four cases (`fauna_client_accounts::SessionInstanceOutcome`). Three of the
/// four mean **proceed**; only [`Refused`](Self::Refused) is terminal.
#[derive(uniffi::Enum, Debug, Clone, PartialEq, Eq)]
pub enum FfiSessionInstanceOutcome {
    /// The lock was taken for this account — a fresh acquire.
    Acquired,
    /// This process already held this account's lock: a same-account session
    /// rebuild reuses it rather than re-acquiring (the kernel treats a second
    /// open as a competing owner, so re-acquiring would refuse *itself*).
    Reused,
    /// Proceed unguarded, and **log** — the platform's duty.
    Degraded {
        /// Which degrade case fired.
        cause: FfiInstanceDegrade,
    },
    /// Refuse, terminally.
    Refused {
        /// Which refusal fired — the chooser latitude keys off this.
        refusal: FfiInstanceRefusal,
        /// The shared platform-neutral one-line reason, for the refusal log.
        /// Carried across the seam so all seven apps log the same sentence
        /// rather than each inventing one (priority #1).
        reason: String,
    },
}

/// Whether a second same-account instance of *this app* may run
/// (`fauna_client_accounts::ServingMode`).
///
/// Every app serves under the one mode now (`account-scoping.md` § Concurrent
/// instances); the pre-retirement exclusive mode is gone.
#[derive(uniffi::Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfiServingMode {
    /// The W5.6 successor: same-account instances coexist, serving takes a
    /// **shared** lock, and exclusivity narrows to the three genuinely
    /// exclusive critical sections (schema migration, the engine-singleton
    /// role, the conversations-engine role).
    Concurrent,
}

impl From<FfiServingMode> for fauna_client_accounts::ServingMode {
    fn from(m: FfiServingMode) -> Self {
        match m {
            FfiServingMode::Concurrent => Self::Concurrent,
        }
    }
}

impl From<fauna_client_accounts::SessionInstanceOutcome> for FfiSessionInstanceOutcome {
    fn from(o: fauna_client_accounts::SessionInstanceOutcome) -> Self {
        use fauna_client_accounts::{InstanceDegrade, InstanceRefusal, SessionInstanceOutcome};
        match o {
            SessionInstanceOutcome::Acquired => Self::Acquired,
            SessionInstanceOutcome::Reused => Self::Reused,
            SessionInstanceOutcome::Degraded(cause) => Self::Degraded {
                cause: match cause {
                    InstanceDegrade::NoStateBase => FfiInstanceDegrade::NoStateBase,
                    InstanceDegrade::LockUnavailable => FfiInstanceDegrade::LockUnavailable,
                },
            },
            SessionInstanceOutcome::Refused(r) => Self::Refused {
                reason: r.reason(),
                refusal: match r {
                    InstanceRefusal::AlreadyServed => FfiInstanceRefusal::AlreadyServed,
                    InstanceRefusal::BoundMismatch { bound } => {
                        FfiInstanceRefusal::BoundMismatch { bound }
                    }
                },
            },
        }
    }
}

/// The account this process runs as when it is not the plain/primary instance
/// — the environment's `FAUNA_BOUND_ACCOUNT` **or** the launch-collision
/// chooser's pick, whichever this process has
/// (`fauna_client_accounts::session_launch_binding`).
///
/// **Read this, never [`requested_bound_account`], once a client renders the
/// chooser.** A binding can arise *after* launch — the chooser's pick makes
/// the colliding process the chosen account's bound instance, with no third
/// process and no environment left to re-read — so a client that re-read the
/// environment would silently ignore its own chooser (the mistake linux
/// records as the second of the three that bite in order).
#[uniffi::export]
pub fn session_launch_binding() -> Option<String> {
    fauna_client_accounts::session_launch_binding()
}

/// Bind this process to `actor_id_hex` — the launch-collision chooser's pick.
///
/// The chooser is the one writer that **creates** a binding after launch, and
/// it runs only on a **plain** launch (a bound launch that collides is
/// terminally refused and never renders the chooser), so it can never
/// contradict an environment-carried binding.
/// [`FfiAccountRegistry::bind_account`] remains the gate that decides whether
/// the pick is *allowed*; this records the answer for the rest of the
/// process, so every later launch read resolves through it.
///
/// A binding also **moves** without any call from the app: a succession
/// re-points the account a binding names, and the shared
/// [`FfiAccountRegistry::record_succession`] — which every adopter of a
/// successor calls before it switches — re-points the binding with it
/// (`fauna_client_accounts::rebind_session_launch_after_succession`;
/// `account-scoping.md` § Concurrent instances → *The binding follows the
/// account*). So a bound instance's post-ceremony
/// [`become_process_session_instance`] on the successor passes bound-or-refuse
/// exactly as the predecessor's did; no app re-binds by hand.
#[uniffi::export]
pub fn bind_session_launch_to(actor_id_hex: String) {
    fauna_client_accounts::bind_session_launch_to(&actor_id_hex);
}

/// Become (or remain) this process's session account — the entry point every
/// native leg calls, wrapping the shared holder
/// (`fauna_client_accounts::become_process_session_instance`).
///
/// The four behaviours around the raw lock are shared, not per-app:
/// **bound-or-refuse**, **reuse** on a same-account rebuild,
/// **swap-by-replacement** on a cross-account switch, and **degrade open**.
/// The client supplies only its install-scoped `state_base` (`None` when it
/// cannot resolve one — a degrade, not a refusal) and the two duties the goal
/// doc assigns it: log the degrade, choose the refusal surface.
///
/// Call it at the point the session account resolves, **before opening any of
/// that account's scoped state**. The held lock lives in Rust process state,
/// so the caller keeps no handle alive and a swap releases the outgoing
/// account's lock by itself.
///
/// Exported for **windows** (and android next), which reach the holder over
/// UniFFI; linux and tui depend on `fauna-client-accounts` directly.
///
/// **Serving mode is the caller's** ([`FfiServingMode`]) — today only
/// `Concurrent` exists (`account-scoping.md` § Concurrent instances).
///
/// `store_container_dir` names the shared account-store root the instance
/// declares itself at, exactly as the erase pair takes it: `None` resolves the
/// per-user platform root (production); a test passes a temp dir, so it never
/// writes a serving lock into the developer's own root.
#[uniffi::export]
pub fn become_process_session_instance(
    state_base: Option<String>,
    actor_id_hex: String,
    mode: FfiServingMode,
    store_container_dir: Option<String>,
) -> FfiSessionInstanceOutcome {
    // Both bases: the launch law's lock under the install base, and the
    // cross-app serving lock at the shared store root, so a sibling app's erase
    // sees this instance (`account-scoping.md` § Concurrent instances → *An
    // erase refuses while a sibling serves the account*). The root resolves in
    // Rust, exactly as the erase pair resolves it, never from a shell's path.
    let store_root = crate::account_state::store_root_for(store_container_dir.map(Into::into));
    fauna_client_accounts::become_process_session_instance(
        fauna_client_accounts::ServingBases {
            state_base: state_base.as_deref().map(std::path::Path::new),
            store_root: Some(store_root.as_path()),
        },
        &actor_id_hex,
        mode.into(),
    )
    .into()
}

/// The per-platform logical-keyed secret store, as a UniFFI callback
/// interface. Kotlin implements it over `EncryptedSharedPreferences`, Swift
/// over the Keychain, C# over Credential Manager (through
/// [`native_keyring_get`] and its siblings below). All values are strings, so
/// every platform's string-keyed secure store implements this trivially.
///
/// Contract (mirrors `fauna_client_accounts::SecretStore`):
/// - Logical keys, not native names: every key the registry names (`fauna/...`,
///   the per-actor slots, the install device secret) is stored under its
///   logical name verbatim.
/// - `set` failures are swallowed by every platform store, so durability is
///   proven by read-back where it matters (the shared
///   `mint_and_persist_pending_factory_reset` rail does exactly that).
#[uniffi::export(with_foreign)]
pub trait FfiSecretStore: Send + Sync {
    /// Return the value for `key`, or `None` if unset.
    fn get(&self, key: String) -> Option<String>;
    /// Set `key` to `value`, creating it if absent.
    fn set(&self, key: String, value: String);
    /// Delete `key` if present (no-op otherwise).
    fn delete(&self, key: String);
}

/// Adapts a foreign [`FfiSecretStore`] to the shared crate's borrowed-key
/// [`SecretStore`] seam.
struct SecretStoreBridge(Arc<dyn FfiSecretStore>);

impl SecretStore for SecretStoreBridge {
    fn get(&self, key: &str) -> Option<String> {
        self.0.get(key.to_string())
    }
    fn set(&self, key: &str, value: &str) {
        self.0.set(key.to_string(), value.to_string());
    }
    fn delete(&self, key: &str) {
        self.0.delete(key.to_string());
    }
}

/// Lend the app's platform secure store to the shared credential-slot crate,
/// so the slots shared Rust serves — the W3 account-store **writer key** in the
/// T10 slot first of all — persist on a target whose Rust arm is inert.
///
/// **Why the phones need it.** `fauna-credential-store` drives the macOS login
/// Keychain, Windows Credential Manager and freedesktop Secret Service itself;
/// on iOS and android it has no arm, and every write it made there was dropped
/// on the floor. The account runtime's assembly mints its writer key, reads it
/// back, finds nothing, and refuses — so until this seam existed **no phone
/// ever assembled an account runtime in production**, only under the e2e
/// redirect (`account-data-plane.md` § Implementation status today → *Built —
/// W3 the apple host*). With the store lent, the crate's foreign arm writes
/// namespace-prefixed rows (`fauna-account-store/<actor hex>`) through the
/// same `get`/`set`/`delete` the registry uses, into the same platform store.
///
/// **Call once at process start, before the first sign-in**, with the very
/// `FfiSecretStore` the app hands `FfiAccountRegistry` — one store for the
/// identity and for the keys minted on its behalf (apple:
/// `FaunaAccounts.installPlatformCredentialStore()`; android:
/// `LaunchModule.provideSecretStore`). Process-global, last install wins —
/// the `install_nest_identity_pin_store` shape next door. Harmless on a
/// desktop target: resolution never consults the lent store where a native
/// arm exists, which is what lets the shared FaunaKit code path install
/// unconditionally for macOS and iOS alike.
///
/// Durability is the platform store's own ruling, not this seam's: apple's
/// rows are `ThisDeviceOnly` (device-bound, excluded from iCloud sync and
/// backup restore — `ios.md` § Credential Storage), android's live in
/// `EncryptedSharedPreferences` under `allowBackup=false`. The consequence
/// that follows is recorded at `apps/common.md` § Credential storage.
#[uniffi::export]
pub fn install_platform_credential_store(store: Arc<dyn FfiSecretStore>) {
    fauna_credential_store::install_foreign_store(Arc::new(SecretStoreBridge(store)));
}

// ── The windows shell's production backend: the shared Credential Manager arm ──
//
// The other direction of the seam above. A phone *lends* its store to shared
// Rust because the crate has no arm there; windows is the one FFI shell whose
// platform the crate drives itself (`fauna_credential_store::win_credman` —
// generic credentials, `CRED_PERSIST_LOCAL_MACHINE`, so they never roam), so
// the shell *borrows* that arm instead of keeping a second implementation of
// the same store. One writer of the `"{app}/{account}"` target-name grammar
// means the app, the terminal app and the sync agent read each other's rows
// by construction (`apps/common.md` § Credential storage, the Windows row).
//
// Windows-only by `cfg`, like `FfiWindowsDetachedSpawner`: apple and android
// own their platform stores app-side, and linux and tui call the crate
// directly. These are the crate's gated public wrappers, so a test build's
// `no-live-keyring` refusal reaches this path too.

/// Read one item of the `app` namespace from the OS credential store. `None`
/// when the item is absent or unreadable.
#[cfg(windows)]
#[uniffi::export]
pub fn native_keyring_get(app: String, account: String) -> Option<String> {
    fauna_credential_store::keyring_get(&app, &account)
}

/// Write (replace) one item of the `app` namespace. Best-effort and log-only,
/// as every arm's write is: a caller that needs the write to have landed reads
/// it back.
#[cfg(windows)]
#[uniffi::export]
pub fn native_keyring_set(app: String, account: String, value: String) {
    fauna_credential_store::keyring_set(&app, &account, &value);
}

/// Delete one item of the `app` namespace; an absent item is a quiet no-op.
#[cfg(windows)]
#[uniffi::export]
pub fn native_keyring_delete(app: String, account: String) {
    fauna_credential_store::keyring_delete(&app, &account);
}

/// FFI mirror of `fauna_client_accounts::AccountEntry` — one switcher row.
/// The forward-compat `extra` map is dropped (not FFI-representable, never
/// rendered; preserved internally by the registry on rewrite).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiAccountEntry {
    /// Actor id (Ed25519 public key), lowercase hex.
    pub actor_id: String,
    pub handle: Option<String>,
    pub domain: Option<String>,
    pub tier: Option<String>,
    /// When true, activating this account requires a re-auth confirmation.
    pub require_confirm_to_activate: bool,
}

impl From<AccountEntry> for FfiAccountEntry {
    fn from(e: AccountEntry) -> Self {
        FfiAccountEntry {
            actor_id: e.actor_id,
            handle: e.handle,
            domain: e.domain,
            tier: e.tier,
            require_confirm_to_activate: e.require_confirm_to_activate,
        }
    }
}

/// The paired predecessor chain ([`FfiAccountRegistry::predecessor_chain`]):
/// `actor_ids[i]` is the identity `keys[i]` (a 32-byte retired owner
/// `BackupKey`) belongs to, nearest hop first. No `Debug` — the keys are
/// key material.
#[derive(uniffi::Record, Clone, PartialEq, Eq)]
pub struct FfiPredecessorChain {
    pub actor_ids: Vec<Vec<u8>>,
    pub keys: Vec<Vec<u8>>,
}

impl FfiPredecessorChain {
    /// The chain as an engine host carries it — each retired root named by
    /// its identity ([`fauna_core::file_download::PredecessorSealKey`]). A
    /// chain whose two lists differ in length, or with an entry that is not
    /// 32 bytes, is refused whole: a shifted pairing would offer one
    /// identity's root to another's rows.
    #[cfg(feature = "sync-engine-host")]
    pub(crate) fn seal_keys(
        &self,
    ) -> Result<Vec<fauna_core::file_download::PredecessorSealKey>, crate::FfiError> {
        let malformed = || crate::FfiError::General {
            msg: "predecessor_chain: actor_ids and keys must pair one to one, 32 bytes each".into(),
        };
        if self.actor_ids.len() != self.keys.len() {
            return Err(malformed());
        }
        self.actor_ids
            .iter()
            .zip(&self.keys)
            .map(|(id, key)| {
                let id = <[u8; 32]>::try_from(id.as_slice()).map_err(|_| malformed())?;
                let key = <[u8; 32]>::try_from(key.as_slice()).map_err(|_| malformed())?;
                Ok(fauna_core::file_download::PredecessorSealKey::named(
                    fauna_core::identity::ActorId(id),
                    fauna_core::crypto::BackupKey::from_bytes(key),
                ))
            })
            .collect()
    }
}

/// FFI mirror of `fauna_client_accounts::SessionMaterial` — the one-read
/// session-identity material for a process's session account
/// (`account-scoping.md` § Concurrent instances → *Session identity resolves
/// through the session's account*). Clients build their authenticated session
/// from this.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiSessionMaterial {
    /// Actor id (Ed25519 public key), lowercase hex.
    pub actor_id: String,
    pub secret_hex: SecretString,
    pub nest_url: Option<String>,
    pub device_id: Option<String>,
    pub handle: Option<String>,
    pub domain: Option<String>,
    pub tier: Option<String>,
}

/// Revert `secret_hex` to a bare `String` and this fails the build — `String`
/// derives `Debug` verbatim, so `{:?}` on `FfiSessionMaterial` would print the
/// account secret in clear (`key-material-hierarchy.md` § Carrier shape, the
/// residual). `SecretString`'s own `Debug` impl redacts.
const _FFI_SESSION_MATERIAL_SECRET_IS_REDACTED: fn(&FfiSessionMaterial) -> &SecretString =
    |m| &m.secret_hex;

impl From<fauna_client_accounts::SessionMaterial> for FfiSessionMaterial {
    fn from(m: fauna_client_accounts::SessionMaterial) -> Self {
        FfiSessionMaterial {
            actor_id: m.actor_id,
            secret_hex: m.secret_hex,
            nest_url: m.nest_url,
            device_id: m.device_id,
            handle: m.handle,
            domain: m.domain,
            tier: m.tier,
        }
    }
}

/// UniFFI handle onto the shared [`AccountRegistry`], backed by the
/// platform's [`FfiSecretStore`]. Cheap to construct (a stateless view over
/// the store) — build one for the switcher UI and pass
/// [`Self::launch_persistence`] to `LaunchMachine::new` so the launch flow
/// routes on the active identity through the SAME store.
#[derive(uniffi::Object)]
pub struct FfiAccountRegistry {
    registry: AccountRegistry,
}

impl FfiAccountRegistry {
    /// The inner [`AccountRegistry`] for sibling FFI modules that must drive
    /// several registry calls as one ordered sequence rather than as separate
    /// exports the app could interleave. Plain (non-exported) — `AccountRegistry`
    /// is not an FFI type, so callers stay inside the crate.
    ///
    /// `recovery.rs` is the caller whose ordering *is* the contract: persist
    /// the successor seed, verify it by read-back, then link the predecessor
    /// only once the arm resolves. Exposing those three as three app-callable
    /// steps would put that ordering back in every app. `erase_guard.rs` reads
    /// the registry's accounts for the sign-out door.
    pub(crate) fn registry(&self) -> &AccountRegistry {
        &self.registry
    }

    /// The retired owner keys of the identity `identity_secret` seeds,
    /// **paired with the identities they belong to**, nearest hop first —
    /// what the sync agent's capability carries as
    /// `SyncCapability::predecessor_keys_by_actor`
    /// (`sync-agent-credentials.md` § Credential model). The one registry
    /// walk, off the provisioned identity's own actor and never the
    /// registry's active account. Empty for an identity that never succeeded
    /// and for a seed that is not 32 bytes (the shared provisioner refuses
    /// that seed itself).
    #[cfg(feature = "sync-agent-provisioning")]
    pub(crate) fn paired_predecessor_keys(
        &self,
        identity_secret: &[u8],
    ) -> Vec<(Vec<u8>, Vec<u8>)> {
        let Ok(secret) = <[u8; 32]>::try_from(identity_secret) else {
            return Vec::new();
        };
        let actor = fauna_core::identity::ActorKeypair::from_secret(secret).actor_id_hex();
        self.registry
            .predecessor_backup_keys_by_actor(&actor)
            .into_iter()
            .map(|(id, key)| (id.0.to_vec(), key.to_bytes().to_vec()))
            .collect()
    }

    /// The attested predecessors of the identity `secret` is the seed of,
    /// each paired with its retired owner key, nearest hop first — what an
    /// in-process engine host binds
    /// ([`crate::sync_engine_host::HostContext`]). Resolved here, off the one
    /// registry walk, so no app pairs the two halves itself.
    #[cfg(feature = "sync-engine-host")]
    pub(crate) fn predecessor_seal_keys(
        &self,
        secret: [u8; 32],
    ) -> Vec<fauna_core::file_download::PredecessorSealKey> {
        let actor = fauna_core::identity::ActorKeypair::from_secret(secret).actor_id_hex();
        fauna_core::file_download::PredecessorSealKey::chain(
            self.registry.predecessor_backup_keys_by_actor(&actor),
        )
    }

    /// A refused activation as the line the seat paints: the shared
    /// `fauna_client_accounts::switch_refused_copy`, naming the target by its
    /// switcher label and saying the user is still where they were
    /// (`long-term-store.md` § Multi-account evolution, "Activating refuses an
    /// account it cannot launch as"). Built here, once, so no seat phrases the
    /// refusal itself or paints the registry's debug text.
    fn switch_refusal(
        &self,
        actor_id: &str,
        err: &fauna_client_accounts::AccountError,
    ) -> FfiError {
        let label = self
            .registry
            .list()
            .into_iter()
            .find(|a| a.actor_id == actor_id)
            .map(|a| fauna_core::format::account_display_label(a.handle.as_deref(), &a.actor_id))
            .unwrap_or_else(|| actor_id.to_string());
        FfiError::General {
            msg: fauna_client_accounts::switch_refused_copy(err, &label)
                .resolve(fauna_i18n::strings::lookup),
        }
    }
}

#[uniffi::export]
impl FfiAccountRegistry {
    #[uniffi::constructor]
    pub fn new(store: Arc<dyn FfiSecretStore>) -> Arc<Self> {
        let store = Arc::new(SecretStoreBridge(store));
        Arc::new(Self {
            registry: fauna_credential_store::account_registry(store),
        })
    }

    /// [`Self::new`], with registry mutations serialized under the shared
    /// cross-process advisory file lock at `<lock_dir>/account-registry.lock`
    /// (`long-term-store.md` § Multi-account evolution → Cross-process
    /// mutation lock). `lock_dir` is the platform's **install-scoped** state
    /// directory (one per OS login, never per account — the lock protects the
    /// shared `fauna/index`). Required on any platform before concurrent
    /// instances ship (`account-scoping.md` § Concurrent instances); reads
    /// and the bind gate stay lock-free, and a lock-file I/O failure degrades
    /// to unserialized mutation rather than a broken sign-out/switch.
    #[uniffi::constructor]
    pub fn new_with_lock_dir(store: Arc<dyn FfiSecretStore>, lock_dir: String) -> Arc<Self> {
        let store = Arc::new(SecretStoreBridge(store));
        let lock = Arc::new(fauna_client_accounts::FileMutationLock::new(
            std::path::Path::new(&lock_dir),
        ));
        Arc::new(Self {
            registry: fauna_credential_store::account_registry_with_lock(store, lock),
        })
    }

    /// The active account's actor id (hex), or `None`.
    pub fn active(&self) -> Option<String> {
        self.registry.active()
    }

    /// The account **this window is using** (`fauna_client_accounts::session_account`):
    /// the process-global holder's actor, else — before any account has been
    /// admitted — [`Self::active`]. Key the switcher's "account in use" row (active
    /// indicator, no switch, no `account-remove-button`) on this, never on
    /// [`Self::active`]: a bound secondary serves an account the registry's active
    /// pointer does not name (`account-scoping.md` § Concurrent instances →
    /// *Remove-account also refuses the account THIS instance serves*).
    ///
    /// Only a seat on the process holder ([`become_process_session_instance`] —
    /// windows) gets its served account here; a raw-lock seat names its own.
    pub fn session_account(&self) -> Option<String> {
        fauna_client_accounts::session_account(&self.registry)
    }

    /// The retired owner `BackupKey`s of the identities `actor_id` succeeded
    /// from, nearest hop first — 32 bytes each, empty for every identity that
    /// never succeeded (`sync-agent.md` § Credential model → *Retired owner
    /// keys after an identity succession*). Feeds
    /// [`crate::sync_agent_provisioning::FfiSyncAgentProvisioner::build`]'s
    /// `predecessor_backup_keys` and any label-custody equivalent — resolve
    /// ONCE post-auth and share across consumers, mirroring tui's `session.rs`
    /// post-auth hook (`succession_predecessor_backup_keys`).
    pub fn predecessor_backup_keys(&self, actor_id: String) -> Vec<Vec<u8>> {
        self.registry
            .predecessor_backup_keys(&actor_id)
            .iter()
            .map(|k| k.to_bytes().to_vec())
            .collect()
    }

    /// The **attested** predecessor actor ids of the identities `actor_id`
    /// succeeded from, nearest hop first — 32 bytes each, empty for every
    /// identity that never succeeded (`account-data-taxonomy.md` § The
    /// generation machinery → *The source of `prior`*, ruled 2026-09-13).
    /// [`Self::predecessor_backup_keys`]'s sibling walk — the same paired
    /// resolution, narrowed to the ids — and the generation machinery's
    /// fleet-view `prior`: feeds
    /// [`crate::sync_agent_provisioning::FfiSyncAgentProvisioner::build`]'s
    /// `predecessor_actor_ids` (the two agent-hosting apps, windows and
    /// macOS). `start_account_runtime` (every UniFFI app) takes this registry
    /// itself and runs the same walk in Rust. Resolve ONCE post-auth off THIS session's own actor and share
    /// across consumers, mirroring tui's `session.rs` post-auth hook
    /// (`attested_predecessors`) and this type's own
    /// [`Self::predecessor_backup_keys`] convention.
    pub fn attested_predecessor_actor_ids(&self, actor_id: String) -> Vec<Vec<u8>> {
        self.registry
            .attested_predecessor_actor_ids(&actor_id)
            .into_iter()
            .map(|id| id.0.to_vec())
            .collect()
    }

    /// The attested predecessors of `actor_id` **paired with their retired
    /// owner keys**, nearest hop first — the one registry walk
    /// (`AccountRegistry::predecessor_backup_keys_by_actor`) both halves come
    /// off, so `actor_ids[i]` is always the identity `keys[i]` belongs to.
    /// Feeds `MediaMachine::set_predecessor_chain` (ruling (8)(c): a row a
    /// predecessor signed opens only under that identity's own root and its
    /// predecessors'). Zipping [`Self::attested_predecessor_actor_ids`] with
    /// [`Self::predecessor_backup_keys`] on the app side is NOT equivalent: the
    /// key-only walk keeps a row whose id does not decode, so the two lists
    /// can differ in length and the pairing would silently shift.
    pub fn predecessor_chain(&self, actor_id: String) -> FfiPredecessorChain {
        let (actor_ids, keys) = self
            .registry
            .predecessor_backup_keys_by_actor(&actor_id)
            .into_iter()
            .map(|(id, key)| (id.0.to_vec(), key.to_bytes().to_vec()))
            .unzip();
        FfiPredecessorChain { actor_ids, keys }
    }

    /// Record that `old_actor` was succeeded by `new_actor` — the predecessor →
    /// successor **link**, which is a different write from persisting the
    /// successor's seed.
    ///
    /// ⚠ **A ceremony leg that calls only `add_account` is not finished.**
    /// [`Self::predecessor_backup_keys`] above — and every other aftermath
    /// consumer: the `__mls` re-seal, the escrow
    /// re-put's predecessor section — resolves through `predecessors_of`, which
    /// walks the `succeeded_by` edge that *only this call* writes. So an app
    /// that persists the seed alone answers "no predecessors" everywhere, and
    /// each consumer then degrades **quietly**: a stuck config plane reads as an
    /// empty list, never as an error. Web shipped exactly that gap on
    /// 2026-08-20 and only the journey caught it
    /// (`identity-succession.md` § Implementation status today,
    /// the correction paragraph's ⚠).
    ///
    /// **Never fatal, and ordered after the arm resolves**, not beside
    /// `add_account`: the seed must be persisted before anything that can fail,
    /// but the link is a *claim that the account moved*, and on the unconfirmed
    /// arm that is not known until the reconcile answers.
    /// [`crate::succession_succeed_with_held_kit`] does this for the ceremony
    /// itself; this export is for the app-side repair paths that do not run it.
    ///
    /// A self-link (`old_actor == new_actor`) is a no-op, and an unknown
    /// *predecessor* row is skipped silently — the shared registry's own rules.
    /// Only an unknown **successor** is an error.
    /// Resolve this process's launch binding through the succession chain —
    /// **a bound launch whose named id has a recorded successor binds to the
    /// terminal successor** (`account-scoping.md` § Concurrent instances →
    /// *The binding follows the account*, rider 2). Returns the binding as it
    /// stands afterwards (`None` for a plain launch) and re-points the process
    /// cell, so [`session_launch_binding`] and the holder's bound-or-refuse
    /// meet the account the binding names *today*.
    ///
    /// Call it once, where the registry is first in hand and **before** the
    /// session account resolves from the binding — and re-read the binding
    /// after it rather than trusting a value captured earlier
    /// (`fauna_client_accounts::AccountRegistry::resolve_launch_binding`).
    pub fn resolve_launch_binding(&self) -> Option<String> {
        self.registry.resolve_launch_binding()
    }

    pub fn record_succession(&self, old_actor: String, new_actor: String) -> Result<(), FfiError> {
        self.registry
            .record_succession(&old_actor, &new_actor)
            .map_err(general_err)
    }

    /// A launch refused as superseded whose chain-verified successor this
    /// device holds: `true` → the link is recorded and the app switches to
    /// `verified_successor` now, owing it the kit and the group sweep; `false`
    /// → the import screen stays the answer. The shared
    /// `AccountRegistry::adopt_held_successor` — pass the successor
    /// `succession_resolve_verified_successor` proved, never the refusal's
    /// claimed one.
    pub fn adopt_held_successor(&self, predecessor: String, verified_successor: String) -> bool {
        self.registry
            .adopt_held_successor(&predecessor, &verified_successor)
    }

    /// Persist the predecessor seeds a phrase-only restore recovered (the
    /// wizard's `restoredPredecessors()`), linked to `restored_actor` — the
    /// identity they are predecessors *of*, named explicitly because an
    /// add-account restore persists before the switch that activates it. The
    /// shared `AccountRegistry::persist_restored_predecessors` tui, linux and
    /// web call; best-effort per row, never an error (the account is back).
    pub fn persist_restored_predecessors(
        &self,
        restored_actor: Option<String>,
        predecessors: Vec<fauna_onboarding_machine::nest_api::RestoredPredecessorSeed>,
    ) {
        self.registry.persist_restored_predecessors(
            restored_actor.as_deref(),
            predecessors
                .iter()
                .map(|p| (p.seed_hex.as_str(), p.actor_id_hex.as_str())),
        );
    }

    /// Every identity `actor_id` succeeded from, nearest hop first — the whole
    /// chain, not just the immediate predecessor, and empty for an identity that
    /// never succeeded.
    ///
    /// The un-derived twin of [`Self::predecessor_backup_keys`]: that one
    /// answers "which retired keys does this identity inherit", this one answers
    /// "which identities were they", which is what a surface naming the
    /// predecessor (and the aftermath's own progress lines) needs.
    pub fn predecessors_of(&self, actor_id: String) -> Vec<String> {
        self.registry.predecessors_of(&actor_id)
    }

    /// All known accounts, in add order — one switcher row per entry.
    pub fn list(&self) -> Vec<FfiAccountEntry> {
        self.registry.list().into_iter().map(Into::into).collect()
    }

    /// One-read session material for `actor_id`: the per-actor secret slots +
    /// the index-entry server-data cache. `None` when the account has no
    /// resolvable secret (unknown, or removed out from under a running
    /// session) — fail closed; never fall back to another account's material.
    pub fn session_material(&self, actor_id: String) -> Option<FfiSessionMaterial> {
        self.registry.session_material(&actor_id).map(Into::into)
    }

    /// The sync device id `actor_id` registers under on this install: the
    /// account's persisted id when it has one, else
    /// `derive_device_id(install_secret, actor_id)` — so a sign-out → sign-in
    /// comes back to the same named `sync_devices` row, and two accounts on one
    /// install never share an id (`sync-agent-credentials.md` § Credential
    /// model, the 2026-09-20 ruling). The rules — what is persisted where, the
    /// serialized mint — are
    /// `fauna_client_accounts::AccountRegistry::device_id_for_actor`'s.
    ///
    /// `install_store` holds the install device secret. It must be a store
    /// the app's sign-out does not wipe: `clear_all` never names the secret,
    /// so an app whose sign-out is `clear_all` alone may pass the registry's
    /// own store, while one that also resets its store wholesale (android's
    /// `SecureStorage.clear()`) passes a second one that reset does not reach.
    ///
    /// Errors only when no stable id exists — the minted secret did not read
    /// back, or the stored one is malformed. The caller stays on its "no device
    /// id" branch rather than use an id the next read cannot reproduce.
    pub fn device_id_for_actor(
        &self,
        install_store: Arc<dyn FfiSecretStore>,
        actor_id: String,
    ) -> Result<String, FfiError> {
        self.registry
            .device_id_for_actor(&SecretStoreBridge(install_store), &actor_id)
            .map_err(general_err)
    }

    /// Add (or update) an account from its secret hex + optional slots.
    /// Derives the actor id; the first account becomes active. Returns the
    /// actor id (hex). Append-mode "Add account" calls this on onboarding
    /// success.
    pub fn add_account(
        &self,
        secret_hex: String,
        nest_url: Option<String>,
        device_id: Option<String>,
    ) -> Result<String, FfiError> {
        self.registry
            .add_account(&secret_hex, nest_url.as_deref(), device_id.as_deref())
            .map_err(general_err)
    }

    /// Moment 1 — the confirm-identity commit point, through the shared
    /// `fauna_client_accounts::persist_confirmed_identity`, **the one call
    /// every wizard's confirm arm makes, in both modes**: on a first-run
    /// wizard it creates the per-actor account, **reads the secret back** (an
    /// infallible `SecretStore::set` means a bare add reports success on a
    /// keystore that kept nothing — and this is the one write whose silent
    /// failure destroys an account outright, since a freshly generated secret
    /// exists nowhere else), activates, and retracts the previous run's
    /// abandoned identity; with `append` (the "Add account" wizard over a live
    /// session) it **writes nothing** and only derives the actor id — the
    /// appended identity stays in the wizard machine (`effectiveSecret()`)
    /// until its own terminal registers and switches, so an abandoned append
    /// can neither leave a half-account nor shadow the active one. The rule
    /// lives in shared Rust, not in an app-side `if !append`. Returns the
    /// actor id.
    pub fn confirm_identity(&self, secret_hex: String, append: bool) -> Result<String, FfiError> {
        fauna_client_accounts::persist_confirmed_identity(&self.registry, &secret_hex, append)
            .map_err(general_err)
    }

    /// Moment 4 — the wizard's **logged-in terminal**, through the shared
    /// helper (`fauna_client_accounts::persist_logged_in`): register the
    /// identity's home nest per-actor, activate it, and spend the
    /// pending-invite and awaiting-DNS slots (the awaiting slot is cleared here
    /// and nowhere earlier — `onboarding.md` § Long-term store contract).
    /// Returns the actor id.
    ///
    /// **This is the write every UniFFI app must use at `WizardOutcome::
    /// LoggedIn`**, and it is the exact sibling of [`Self::
    /// persist_awaiting_dns`]'s story one moment later: apple had no export to
    /// reach the per-actor `nest_url` row and wrote its own single slot
    /// instead, which the next primary launch then lost (the per-actor rows
    /// never recorded the URL). The launch machine's routing tuple degraded to
    /// `(Some(secret), None, None)` → `WizardAt(HandleEntry)`, so a completed
    /// onboarding silently re-rendered the handle-entry page
    /// (`test_smoke_k_real_onboarding_completion_reaches_the_main_app[macos]`).
    /// linux already called the shared helper's inline equivalent.
    pub fn persist_logged_in(
        &self,
        secret_hex: String,
        nest_url: String,
        device_id: Option<String>,
        reach_ipv4: Option<String>,
    ) -> Result<String, FfiError> {
        fauna_client_accounts::persist_logged_in(
            &self.registry,
            &secret_hex,
            &nest_url,
            device_id.as_deref(),
            reach_ipv4.as_deref(),
        )
        .map_err(general_err)
    }

    /// Persist the deferred-DNS resume slot at the wizard's `AwaitingManualDns`
    /// exit, returning the actor id it was written under.
    ///
    /// **This is the write every UniFFI app must use** — apple, windows and
    /// android previously had no way to reach the per-actor slot (no export
    /// existed) and wrote their own single-slot keys instead, which composed
    /// back only while no account index existed *and* every required field
    /// was non-empty. The deferred-DNS exit has no handle yet, so the slot
    /// round-tripped to nothing and the relaunch dropped to the handle stage,
    /// losing a half-provisioned nest (`test_smoke_i`). linux and tui already
    /// called the shared helper this wraps.
    pub fn persist_awaiting_dns(
        &self,
        secret_hex: String,
        nest_url: String,
        handle: String,
        dns_records_json: String,
        claim_code: String,
    ) -> Result<String, FfiError> {
        fauna_client_accounts::persist_awaiting_dns(
            &self.registry,
            &secret_hex,
            &fauna_launch_machine::AwaitingDnsRecord {
                nest_url,
                handle,
                dns_records_json,
                claim_code,
                // The exit COMPLETES the slot rather than replacing it
                // (`onboarding.md` § 6 *Deferred-DNS path*), and the shared
                // writer is what preserves a reach address the pending-provision
                // write already put there — and the identity the box was
                // built with, by the same rule. Nothing for this signature to
                // carry.
                reach_ipv4: None,
                nest_actor_id: None,
            },
        )
        .map_err(general_err)
    }

    /// Clear the deferred-DNS resume slot at a claim terminal (the slot
    /// outranks every other launch-routing row, so a survivor would pin the
    /// admin on "Almost ready" forever). Scoped to the active account.
    pub fn clear_awaiting_dns(&self) {
        fauna_client_accounts::clear_awaiting_dns_for_active(&self.registry);
    }

    /// Persist the pending-invite resume slot at the wizard's **submit return**
    /// — the only write moment (`onboarding.md` § The pending-invite surface,
    /// Persistence callouts) — returning the actor id.
    /// [`Self::persist_awaiting_dns`]'s twin, and the same reason: a single-slot
    /// write was invisible on an indexed (multi-account) install.
    ///
    /// Called on the `wizard_submit_invite_request()` return, NOT at a wizard
    /// exit: the pending-review journey never exits (the `InviteSubmitted`
    /// outcome this once keyed on retired 2026-08-11).
    pub fn persist_pending_invite(
        &self,
        secret_hex: String,
        nest_url: String,
        handle: String,
        request_id: String,
        status_json: String,
    ) -> Result<String, FfiError> {
        fauna_client_accounts::persist_pending_invite(
            &self.registry,
            &secret_hex,
            &fauna_launch_machine::PendingInviteRecord {
                nest_url,
                handle,
                request_id,
                status_json,
            },
        )
        .map_err(general_err)
    }

    /// Make `actor_id` the active account (the switch primitive). The caller
    /// follows with a client teardown/rebuild of the launch machine.
    ///
    /// Refuses an account whose `require_confirm_to_activate` flag is set
    /// (Stage 2): the client pre-reads the flag from [`Self::list`], runs its
    /// re-auth prompt, and calls [`Self::set_active_confirmed`] on success —
    /// this error is the backstop for a client that forgot the prompt.
    ///
    /// A refusal's `FfiError::General` message is the paint-ready line
    /// ([`Self::switch_refusal`]): the seat shows it as-is on the Account
    /// page's `error-message`, as every boundary `FfiError` is shown.
    pub fn set_active(&self, actor_id: String) -> Result<(), FfiError> {
        self.registry
            .set_active(&actor_id)
            .map_err(|e| self.switch_refusal(&actor_id, &e))
    }

    /// [`Self::set_active`], asserting the user has **just completed** a
    /// re-auth confirmation for this activation (Stage 2). Only ever call
    /// adjacent to the platform's re-auth prompt (apple `LAContext`, …); call
    /// sites are the audit surface for the gate.
    pub fn set_active_confirmed(&self, actor_id: String) -> Result<(), FfiError> {
        self.registry
            .set_active_confirmed(&actor_id)
            .map_err(|e| self.switch_refusal(&actor_id, &e))
    }

    /// Remove an account (its per-actor slots + index entry). If it was
    /// active, the first remaining account becomes active — the
    /// registry-routed form of "log out (keep data)" (`account-scoping.md`
    /// § Concurrent instances, the delete corollary).
    pub fn remove(&self, actor_id: String) -> Result<(), FfiError> {
        self.registry.remove(&actor_id).map_err(general_err)
    }

    /// Walk away from `actor_id`'s nest, keeping the identity: clears the
    /// per-actor (nest_url, device_id) slots — the registry-routed form of
    /// the "use a different nest" fallthrough (same delete corollary as
    /// `remove`).
    pub fn clear_nest_binding(&self, actor_id: String) -> Result<(), FfiError> {
        self.registry
            .clear_nest_binding(&actor_id)
            .map_err(general_err)
    }

    /// Sign-out cleanup: erase every account's per-actor slots and the index —
    /// `long-term-store.md` § Cleanup contract. Delete-only, so a crash mid-clear cannot resurrect the
    /// identity being erased.
    ///
    /// Returns what still reads back afterwards. This store's `delete` reports
    /// nothing on any platform (apple drops the keychain status, android and
    /// windows swallow), so the read-back is the only witness the seat has.
    /// Hand it to `sign_out_residue_record` beside the filesystem sweep; the
    /// keys go to the log, never to the user.
    pub fn clear_all(&self) -> crate::account_state::FfiCredentialSweep {
        self.registry.clear_all().into()
    }

    /// Re-read what [`Self::clear_all`] reported surviving, after the platform
    /// store's own wholesale reset ran (android's `SecureStorage.clear()`): a key
    /// that reset removed drops out, one it did not stays. A seat whose reset
    /// follows `clear_all` must re-ask here before it paints, or it reports
    /// credentials the reset already took.
    pub fn reverify_erase(
        &self,
        sweep: crate::account_state::FfiCredentialSweep,
    ) -> crate::account_state::FfiCredentialSweep {
        self.registry.reverify(sweep.into()).into()
    }

    /// Update an account's server-data cache (handle/domain/tier) shown in
    /// the switcher rows — after a silent sign-in, for the active account.
    pub fn update_cache(
        &self,
        actor_id: String,
        handle: Option<String>,
        domain: Option<String>,
        tier: Option<String>,
    ) -> Result<(), FfiError> {
        self.registry
            .update_cache(
                &actor_id,
                handle.as_deref(),
                domain.as_deref(),
                tier.as_deref(),
            )
            .map_err(general_err)
    }

    /// Set (or overwrite) the per-actor nest-URL slot (raw slot write).
    pub fn set_nest_url(&self, actor_id: String, nest_url: String) {
        self.registry.set_nest_url(&actor_id, &nest_url);
    }

    /// Set the per-account "require confirmation to activate" flag (the
    /// toggle's write path — marks the flag user-set, so the admin
    /// auto-default never overrides it afterwards).
    pub fn set_require_confirm(&self, actor_id: String, require: bool) -> Result<(), FfiError> {
        self.registry
            .set_require_confirm(&actor_id, require)
            .map_err(general_err)
    }

    /// The admin auto-default: flip the flag ON iff the user has never touched
    /// this account's toggle (`long-term-store.md` § Multi-account evolution —
    /// *"a client turns it on for its admin identity"*). Idempotent; call on
    /// every `am-i-admin = true` observation for the active account. Returns
    /// whether this call flipped it (a client may refresh its switcher UI on
    /// `true`).
    pub fn auto_enable_require_confirm(&self, actor_id: String) -> Result<bool, FfiError> {
        self.registry
            .auto_enable_require_confirm(&actor_id)
            .map_err(general_err)
    }

    /// Fold a **successful** `fauna.family.status` reply into `actor_id`'s
    /// persisted last-known supervision snapshot (family-safety.md § Content
    /// policy, clause 2 — "written on every successful status read"). Call on
    /// the app's one status-read choke point's success path and nowhere else:
    /// clause 1 forbids moving this slot on a failed read, and routing every
    /// caller through the choke point is what keeps the clause true for them
    /// all. The fold — including the graduation gate, under which a policy
    /// naming no guardian persists nothing enforceable — is the shared
    /// `SupervisionSnapshot::from_status`; only the two fields it reads cross
    /// back to the wire shape here.
    pub fn persist_supervision_snapshot(&self, actor_id: String, status: crate::FfiFamilyStatus) {
        use fauna_client_family::family::{FamilyGuardianInfo, FamilyStatusReply};
        let wire = FamilyStatusReply {
            supervised_by: status.supervised_by.map(|g| FamilyGuardianInfo {
                actor_id: g.actor_id.into(),
                handle: g.handle,
                ..Default::default()
            }),
            policy: status.policy.map(Into::into),
            ..Default::default()
        };
        self.registry.set_supervision_snapshot_json(
            &actor_id,
            &fauna_client_family::SupervisionSnapshot::from_status(&wire).to_json(),
        );
    }

    /// The persisted last-known supervision snapshot for `actor_id`, or
    /// `None` when there is nothing to enforce — the slot is absent or
    /// malformed ("no information", the ruled fail direction), or its last
    /// read said unsupervised (see [`crate::FfiSupervisionSnapshot`]'s
    /// guardian-less refusal). The restore-at-launch call site: run it ahead
    /// of the first read, feed the same stores the read's success path feeds.
    pub fn supervision_snapshot(&self, actor_id: String) -> Option<crate::FfiSupervisionSnapshot> {
        let snap = self
            .registry
            .supervision_snapshot_json(&actor_id)
            .and_then(|raw| fauna_client_family::SupervisionSnapshot::from_json(&raw))?;
        crate::FfiSupervisionSnapshot::from_fold(snap)
    }

    /// The `LaunchPersistence` adapter over **this registry** — pass to
    /// `LaunchMachine::new` (and to `mint_and_persist_pending_factory_reset`)
    /// so the launch flow reads and writes the ACTIVE account's slots. This
    /// replaces the platform's hand-rolled `LaunchPersistence` implementation
    /// wholesale: with it, the platform's only foreign seam is
    /// [`FfiSecretStore`].
    ///
    /// Built from `self.registry`, never from a fresh registry over the same
    /// store: the adapter must inherit whatever mutation lock this registry was
    /// constructed with, or a platform adopting [`Self::new_with_lock_dir`]
    /// would leave its launch writer unserialized (see
    /// `AccountRegistry::launch_persistence`).
    pub fn launch_persistence(&self) -> Arc<dyn LaunchPersistence> {
        Arc::new(self.registry.launch_persistence())
    }

    /// The `PendingProvisionStore` over **this registry** — pass to
    /// `OnboardingMachine::new_with_persistence` so the wizard lands the
    /// pending-provision slot before it builds a box
    /// (`docs/goal/behavior/onboarding.md` § 6 *The pending-provision slot*).
    ///
    /// [`Self::launch_persistence`]'s twin, built from `self.registry` for the
    /// same mutation-lock reason, and separate from it for the reason the trait
    /// documents: this writer is addressed by the identity being onboarded,
    /// which during a first-run wizard is not yet anybody's *session* account.
    pub fn pending_provision_store(&self) -> Arc<dyn PendingProvisionStore> {
        Arc::new(self.registry.pending_provision_store())
    }

    /// Validate that a secondary instance may launch **bound** to `actor_id`
    /// without moving the active pointer — the concurrent-instances spawn
    /// gate (`account-scoping.md` § Concurrent instances). Pure read; mirrors
    /// every activation guard, so a flagged account refuses until the client
    /// runs its re-auth prompt and calls [`Self::bind_account_confirmed`].
    pub fn bind_account(&self, actor_id: String) -> Result<(), FfiError> {
        self.registry.bind_account(&actor_id).map_err(general_err)
    }

    /// [`Self::bind_account`], asserting a just-completed re-auth
    /// confirmation. Only ever call adjacent to the platform's re-auth
    /// prompt; call sites are the audit surface, like
    /// [`Self::set_active_confirmed`]'s.
    pub fn bind_account_confirmed(&self, actor_id: String) -> Result<(), FfiError> {
        self.registry
            .bind_account_confirmed(&actor_id)
            .map_err(general_err)
    }

    /// The `LaunchPersistence` adapter **bound** to one account for a
    /// secondary instance: every load/save resolves `actor_id`'s slots and the
    /// active pointer is never consulted nor moved (`account-scoping.md`
    /// § Concurrent instances). Gate
    /// the spawn with [`Self::bind_account`] first; a bound adapter whose
    /// account has since been removed loads nothing (fail-safe to
    /// onboarding, never another account's slots).
    pub fn bound_launch_persistence(&self, actor_id: String) -> Arc<dyn LaunchPersistence> {
        Arc::new(self.registry.bound_launch_persistence(actor_id))
    }

    /// Which account-index verdict is active, if any (`version-compatibility.md`
    /// § 5 item 9) — an additive query, not a new `FfiError` variant: every
    /// mutator above still flattens `AccountError::IndexUnreadable`/
    /// `IndexMalformed` to an opaque string via [`general_err`], because a new
    /// exported error variant would break every app's exhaustive `switch`/`when`
    /// over `FfiError` and windows/apple can't be verified from this machine.
    /// A caller whose mutation just failed opaquely can ask this instead of
    /// parsing the message.
    pub fn index_refusal(&self) -> Option<AccountIndexRefusal> {
        self.registry.index_refusal()
    }
}

/// Outcome of [`FfiAccountRegistry::call_registry_method_for_test`] — the
/// UniFFI spelling of `fauna_client_accounts::RegistryMethodOutcome`.
#[cfg(feature = "test-helpers")]
#[derive(uniffi::Enum)]
pub enum FfiRegistryMethodOutcome {
    /// Handled. `result_json` is a reader's JSON-serialized result — `None` for
    /// a setter, as the machine dispatcher's `Option<String>` spells it.
    Handled { result_json: Option<String> },
    /// Not a registry method; the caller tries the machine dispatcher next.
    NotMine,
}

/// The registry half of the cross-app E2E bridge for the UniFFI apps. The name
/// table and the semantics are shared Rust
/// (`fauna_client_accounts::call_registry_method_for_test`); an app's agent
/// tries this before `OnboardingMachine::call_machine_method_async`, exactly as
/// tui's automation does, handing over nothing but its own registry.
///
/// `test-helpers` only (convention 15 rule (b)): the `*-ffi-test` flavors carry
/// it; the production flavors — the shipped artifacts — compile it out.
#[cfg(feature = "test-helpers")]
#[uniffi::export]
impl FfiAccountRegistry {
    pub fn call_registry_method_for_test(
        &self,
        name: String,
        json_arg: String,
    ) -> FfiRegistryMethodOutcome {
        match fauna_client_accounts::call_registry_method_for_test(&self.registry, &name, &json_arg)
        {
            fauna_client_accounts::RegistryMethodOutcome::Handled(result_json) => {
                FfiRegistryMethodOutcome::Handled { result_json }
            }
            fauna_client_accounts::RegistryMethodOutcome::NotMine => {
                FfiRegistryMethodOutcome::NotMine
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Default)]
    struct MapStore(Mutex<HashMap<String, String>>);

    impl FfiSecretStore for MapStore {
        fn get(&self, key: String) -> Option<String> {
            self.0.lock().unwrap().get(&key).cloned()
        }
        fn set(&self, key: String, value: String) {
            self.0.lock().unwrap().insert(key, value);
        }
        fn delete(&self, key: String) {
            self.0.lock().unwrap().remove(&key);
        }
    }

    const SECRET_A: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const SECRET_B: &str = "2222222222222222222222222222222222222222222222222222222222222222";

    /// apple's shape: `FaunaAccounts.registry()` builds a fresh
    /// `FfiAccountRegistry` (and a fresh store bridge) per call over the one
    /// keychain, so the agent's arming registry is never the ceremony's. The
    /// fault must still reach the ceremony's `add_account`.
    #[cfg(feature = "test-helpers")]
    #[test]
    fn the_registry_bridge_fault_reaches_another_registry_over_the_same_store() {
        let store: Arc<MapStore> = Arc::new(MapStore::default());
        let arming = FfiAccountRegistry::new(store.clone());
        let ceremony = FfiAccountRegistry::new(store.clone());

        assert!(matches!(
            arming.call_registry_method_for_test(
                "refuse_secret_writes_for_test".into(),
                r#"{"refuse":true}"#.into(),
            ),
            FfiRegistryMethodOutcome::Handled { result_json: None }
        ));
        assert!(
            ceremony.add_account(SECRET_A.into(), None, None).is_err(),
            "the ceremony's registry must see the fault the agent armed"
        );

        arming.call_registry_method_for_test(
            "refuse_secret_writes_for_test".into(),
            r#"{"refuse":false}"#.into(),
        );
        assert!(ceremony.add_account(SECRET_A.into(), None, None).is_ok());
        assert!(matches!(
            arming.call_registry_method_for_test("seed_identity".into(), "null".into()),
            FfiRegistryMethodOutcome::NotMine
        ));
    }

    #[test]
    fn registry_round_trips_over_a_foreign_store() {
        let reg = FfiAccountRegistry::new(Arc::new(MapStore::default()));
        assert_eq!(reg.active(), None);
        let a = reg
            .add_account(SECRET_A.into(), Some("https://a.example".into()), None)
            .unwrap();
        assert_eq!(reg.active().as_deref(), Some(a.as_str()));
        let rows = reg.list();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].actor_id, a);
    }

    #[test]
    fn launch_persistence_reads_the_active_account() {
        let reg = FfiAccountRegistry::new(Arc::new(MapStore::default()));
        reg.add_account(SECRET_A.into(), Some("https://a.example".into()), None)
            .unwrap();
        let p = reg.launch_persistence();
        assert!(p.load_identity().is_some());
        assert_eq!(p.load_nest_url().as_deref(), Some("https://a.example"));
    }

    /// Row 337 (`key-material-hierarchy.md` § Carrier shape) — `{:?}` on
    /// `FfiSessionMaterial` must never print the account secret. Mutation:
    /// reverting `secret_hex` to a bare `String` reds this (the struct-level
    /// `#[derive(Debug)]` would then print the field verbatim) as well as the
    /// build-time pin above.
    #[test]
    fn ffi_session_material_debug_never_prints_the_secret() {
        let reg = FfiAccountRegistry::new(Arc::new(MapStore::default()));
        let a = reg
            .add_account(SECRET_A.into(), Some("https://a.example".into()), None)
            .unwrap();
        let m = reg
            .session_material(a)
            .expect("just-added account resolves");
        let debug = format!("{m:?}");
        assert!(
            !debug.contains(SECRET_A),
            "Debug output must redact the secret, got {debug:?}"
        );
    }

    /// A supervised `fauna.family.status` reply, as the FFI face carries it —
    /// built through the real wire conversion, so its `supervision` fold is
    /// the one a live read hands an app.
    fn supervised_ffi_status() -> crate::FfiFamilyStatus {
        use fauna_client_family::family::{FamilyGuardianInfo, FamilyStatusReply, ReachPolicy};
        use fauna_core::obligation::{ContentFloor, ContentPolicy};
        use fauna_core::screen_time::ScreenTimePolicy;
        FamilyStatusReply {
            supervised_by: Some(FamilyGuardianInfo {
                actor_id: vec![0xab, 0xcd, 0x01].into(),
                handle: "parent@example.org".into(),
                ..Default::default()
            }),
            policy: Some(ReachPolicy {
                content_policy: Some(ContentPolicy {
                    nsfw: ContentFloor::Block,
                    ..Default::default()
                }),
                screen_time: Some(ScreenTimePolicy {
                    window_start: Some(1260),
                    window_end: Some(420),
                    daily_minutes: Some(90),
                }),
                content_notify: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        }
        .into()
    }

    /// Clause 2 through the FFI face (family-safety.md § Content policy):
    /// persist a successful supervised read, restore it, and every field a
    /// cold launch enforces survives — including `content_notify` (the field
    /// tui's mutation run proved the happy-path pins do not discriminate on)
    /// and the guardian's id through its hex round trip.
    ///
    /// The three supervision tests each `add_account` first because the
    /// writer no-ops for an actor absent from the index (`lib.rs`'s
    /// `set_supervision_snapshot_json` guard): a bare literal actor id would
    /// make the write vanish and the gate assertions pass vacuously.
    #[test]
    fn a_successful_status_persists_and_restores_the_supervision() {
        let reg = FfiAccountRegistry::new(Arc::new(MapStore::default()));
        let a = reg.add_account(SECRET_A.into(), None, None).unwrap();
        let b = reg.add_account(SECRET_B.into(), None, None).unwrap();
        reg.persist_supervision_snapshot(a.clone(), supervised_ffi_status());
        let snap = reg
            .supervision_snapshot(a)
            .expect("a supervised read restores");
        assert_eq!(snap.supervised_by.handle, "parent@example.org");
        assert_eq!(
            snap.supervised_by.actor_id,
            vec![0xab, 0xcd, 0x01],
            "the guardian id survives the hex round trip"
        );
        assert_eq!(
            snap.content_policy.expect("the floor restores").nsfw,
            "block"
        );
        assert!(
            snap.content_notify,
            "notify counting resumes on a cold launch"
        );
        assert_eq!(
            snap.screen_time
                .expect("the bedtime window restores")
                .daily_minutes,
            Some(90)
        );
        assert_eq!(reg.supervision_snapshot(b), None, "the slot is per-actor");
    }

    /// A cold launch restores exactly the fold the live read handed the app
    /// ([`crate::FfiFamilyStatus::supervision`]) — one shape through one
    /// conversion, so an app that seeds its stores at launch and moves them on
    /// a successful read cannot feed them two different values.
    #[test]
    fn the_live_supervision_fold_is_what_a_cold_launch_restores() {
        let reg = FfiAccountRegistry::new(Arc::new(MapStore::default()));
        let a = reg.add_account(SECRET_A.into(), None, None).unwrap();
        let live = supervised_ffi_status();
        assert!(
            live.supervision.is_some(),
            "precondition: a supervised read carries its fold"
        );
        reg.persist_supervision_snapshot(a.clone(), live.clone());
        assert_eq!(reg.supervision_snapshot(a), live.supervision);
    }

    /// The graduation gate holds across the FFI conversion: a reply that still
    /// carries a policy document but names NO guardian persists nothing
    /// enforceable, so the restore yields no snapshot at all (the shared
    /// `SupervisionSnapshot::from_status` gate, which this face must route
    /// through rather than hand-folding the reply). The fixture's carried
    /// `supervision` is left stale on purpose: persisting re-folds the raw
    /// fields and never trusts the fold an app hands back.
    #[test]
    fn a_policy_without_a_guardian_restores_as_no_snapshot() {
        let reg = FfiAccountRegistry::new(Arc::new(MapStore::default()));
        let a = reg.add_account(SECRET_A.into(), None, None).unwrap();
        let mut status = supervised_ffi_status();
        status.supervised_by = None;
        reg.persist_supervision_snapshot(a.clone(), status);
        assert_eq!(reg.supervision_snapshot(a), None);
    }

    /// Clause 3's clearing rule: a later successful read reporting
    /// unsupervised overwrites the slot, so the next cold launch restores
    /// nothing — graduation offline ends at the first successful read.
    #[test]
    fn an_unsupervised_read_overwrites_a_supervised_slot() {
        let reg = FfiAccountRegistry::new(Arc::new(MapStore::default()));
        let a = reg.add_account(SECRET_A.into(), None, None).unwrap();
        reg.persist_supervision_snapshot(a.clone(), supervised_ffi_status());
        assert!(reg.supervision_snapshot(a.clone()).is_some());
        let mut status = supervised_ffi_status();
        status.supervised_by = None;
        status.policy = None;
        reg.persist_supervision_snapshot(a.clone(), status);
        assert_eq!(reg.supervision_snapshot(a), None);
    }

    /// Counts acquisitions, so a test can pin WHICH registry an adapter got.
    struct CountingLock(std::sync::atomic::AtomicUsize);

    impl fauna_client_accounts::MutationLock for CountingLock {
        fn acquire(&self) -> fauna_client_accounts::MutationLockGuard {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            fauna_client_accounts::MutationLockGuard::unheld()
        }
    }

    /// A registry built with a mutation lock hands that lock to the launch
    /// adapters it mints — **both** of them.
    ///
    /// This is the whole point of adopting the lock: `save_authenticated` runs
    /// `update_cache`, a full read-modify-write of the shared `fauna/index`,
    /// and it is the *launch* path — the busiest writer of all. An adapter
    /// getter that rebuilt its registry from the raw store instead would leave
    /// that writer unserialized while every other mutator locked, so a
    /// platform could adopt the lock, pass every lock test, and still lose the
    /// exact update the lock exists to protect (`long-term-store.md`
    /// § Multi-account evolution → Cross-process mutation lock).
    #[test]
    fn launch_adapters_inherit_the_registrys_mutation_lock() {
        for bound in [false, true] {
            let lock = Arc::new(CountingLock(std::sync::atomic::AtomicUsize::new(0)));
            let store = Arc::new(SecretStoreBridge(Arc::new(MapStore::default())));
            let reg = FfiAccountRegistry {
                registry: AccountRegistry::with_mutation_lock(store, lock.clone()),
            };
            let actor = reg
                .add_account(SECRET_A.into(), None, None)
                .expect("seed account");
            let before = lock.0.load(std::sync::atomic::Ordering::SeqCst);

            let adapter = if bound {
                reg.bound_launch_persistence(actor)
            } else {
                reg.launch_persistence()
            };
            adapter.save_authenticated(
                "https://a.example".into(),
                "alice".into(),
                "a.example".into(),
                "free".into(),
            );

            assert!(
                lock.0.load(std::sync::atomic::Ordering::SeqCst) > before,
                "a {} launch adapter's save_authenticated must serialize under the \
                 registry's mutation lock",
                if bound { "bound" } else { "plain" }
            );
        }
    }

    const ACTOR_A: &str = "aa11111111111111111111111111111111111111111111111111111111111111";
    const ACTOR_B: &str = "bb22222222222222222222222222222222222222222222222222222222222222";
    const ACTOR_C: &str = "cc33333333333333333333333333333333333333333333333333333333333333";

    /// The whole session-instance seam in ONE test, deliberately.
    ///
    /// [`become_process_session_instance`] and [`session_launch_binding`] read
    /// and write **process-global** state (`PROCESS_SESSION_INSTANCE` /
    /// `SESSION_LAUNCH_BINDING`). cargo runs a crate's tests as threads in one
    /// process, so splitting these into sibling `#[test]`s would let a binding
    /// set by one race the holder read by another — green in isolation, red in
    /// a full run. That is the exact shape of the xUnit `MutedKeywordsCache`
    /// race the windows suite hit on 2026-07-22; the fix there and here is one
    /// owner for the global, not a sprinkling of retries.
    ///
    #[test]
    fn the_process_session_instance_seam_translates_all_four_outcomes() {
        let base = tempfile::tempdir().expect("temp base");
        let base_str = base.path().to_string_lossy().to_string();
        // The store container the holder declares itself under — a temp dir, so
        // no run writes a serving lock into the developer's own store root.
        let store = tempfile::tempdir().expect("temp store container");
        let store_str = store.path().to_string_lossy().to_string();
        let store_root = crate::account_state::store_root_for(Some(store.path().to_path_buf()));

        // A plain launch has no binding until something sets one.
        assert_eq!(session_launch_binding(), None);

        // Fresh acquire, then the same-account rebuild reuses it.
        assert_eq!(
            become_process_session_instance(
                Some(base_str.clone()),
                ACTOR_A.into(),
                FfiServingMode::Concurrent,
                Some(store_str.clone()),
            ),
            FfiSessionInstanceOutcome::Acquired
        );
        assert_eq!(
            become_process_session_instance(
                Some(base_str.clone()),
                ACTOR_A.into(),
                FfiServingMode::Concurrent,
                Some(store_str.clone()),
            ),
            FfiSessionInstanceOutcome::Reused
        );

        // The holder declares itself at the shared store root too, so a sibling
        // app's erase can see this instance.
        assert!(
            fauna_account_store::locks::ServingLock::is_served(&store_root, ACTOR_A),
            "the FFI holder route declares the instance at the store root"
        );

        // ⚠ The single-window box — the case a wrong erase door refuses. With
        // this process's OWN holder serving the account and nobody else, the
        // door answers "not blocked" (asked here through the start-over form,
        // whose accounts come from the disk: an empty registry, ACTOR_A's scope
        // on disk), and the holder still serves at both bases afterwards. Here
        // rather than in `erase_guard`'s tests because the holder is this
        // test's global.
        std::fs::create_dir_all(base.path().join(ACTOR_A)).expect("ACTOR_A scope");
        let empty = FfiAccountRegistry::new(Arc::new(MapStore::default()));
        assert_eq!(
            crate::start_over_blocked(empty, base_str.clone(), Some(store_str.clone()), None),
            None,
            "a lone holder instance must not refuse its own erase"
        );
        assert!(
            accounts_not_currently_served(base_str.clone(), vec![ACTOR_A.into()]).is_empty(),
            "the holder's instance lock was restored after the probe"
        );
        assert!(
            fauna_account_store::locks::ServingLock::is_served(&store_root, ACTOR_A),
            "and its serving lock"
        );
        // And the account this process serves is refused for removal: this
        // window is running from its stores.
        assert!(matches!(
            crate::remove_account_blocked(
                base_str.clone(),
                ACTOR_A.into(),
                Some(store_str.clone()),
                None,
                None,
            ),
            Some(crate::FfiEraseRemoveBlocked::ServedHere { .. })
        ));

        // A cross-account switch swaps by replacement — the outgoing account's
        // lock is released by the holder, so ACTOR_A is free again afterwards.
        assert_eq!(
            become_process_session_instance(
                Some(base_str.clone()),
                ACTOR_B.into(),
                FfiServingMode::Concurrent,
                Some(store_str.clone()),
            ),
            FfiSessionInstanceOutcome::Acquired
        );
        assert_eq!(
            accounts_not_currently_served(base_str.clone(), vec![ACTOR_A.into()]),
            vec![ACTOR_A.to_string()],
            "the swap must release the outgoing account's lock"
        );

        // The switcher's "account in use" key is the served account, not the
        // registry's active pointer: a registry whose active account is some
        // other actor still answers ACTOR_B, which this process serves.
        let elsewhere_active = FfiAccountRegistry::new(Arc::new(MapStore::default()));
        let registry_active = elsewhere_active
            .add_account("9".repeat(64), None, None)
            .expect("add account");
        assert_eq!(elsewhere_active.active(), Some(registry_active));
        assert_eq!(
            elsewhere_active.session_account().as_deref(),
            Some(ACTOR_B),
            "session_account follows the holder, never the registry's active pointer"
        );

        // AlreadyServed: a competing owner holds ACTOR_C's lock (a second
        // handle conflicts even in-process — pinned in `instance_lock.rs`).
        // The competitor is an exclusive holder — the sign-out probe's shape,
        // the only exclusive acquire left — which refuses every shared acquire.
        let competitor =
            match fauna_client_accounts::AccountInstanceLock::acquire(base.path(), ACTOR_C) {
                fauna_client_accounts::InstanceLockOutcome::Held(lock) => lock,
                _ => panic!("the competing exclusive acquire must succeed"),
            };
        let served = become_process_session_instance(
            Some(base_str.clone()),
            ACTOR_C.into(),
            FfiServingMode::Concurrent,
            Some(store_str.clone()),
        );
        assert_eq!(
            served,
            FfiSessionInstanceOutcome::Refused {
                refusal: FfiInstanceRefusal::AlreadyServed,
                reason: "another instance already serves this account".into(),
            }
        );
        drop(competitor);

        // The chooser's pick binds this process — and normalizes, so the
        // binding, the lock key and the bound-or-refuse compare cannot
        // disagree on spelling.
        bind_session_launch_to(format!("  {}  ", ACTOR_B.to_uppercase()));
        assert_eq!(session_launch_binding().as_deref(), Some(ACTOR_B));

        // Bound-or-refuse now outranks everything, including a free lock.
        assert_eq!(
            become_process_session_instance(
                Some(base_str.clone()),
                ACTOR_A.into(),
                FfiServingMode::Concurrent,
                Some(store_str.clone()),
            ),
            FfiSessionInstanceOutcome::Refused {
                refusal: FfiInstanceRefusal::BoundMismatch {
                    bound: ACTOR_B.into()
                },
                reason: format!(
                    "launched bound to {ACTOR_B} but the session resolved a different account"
                ),
            }
        );

        // Reuse is case-insensitive on the actor id, and precedes the base
        // check — a same-account rebuild never needs a resolvable base.
        assert_eq!(
            become_process_session_instance(
                None,
                ACTOR_B.to_uppercase(),
                FfiServingMode::Concurrent,
                Some(store_str.clone()),
            ),
            FfiSessionInstanceOutcome::Reused
        );

        // An unresolvable state base degrades OPEN, never refuses: a
        // filesystem hiccup must not become a client that cannot launch.
        bind_session_launch_to(ACTOR_C.into());
        assert_eq!(
            become_process_session_instance(
                None,
                ACTOR_C.into(),
                FfiServingMode::Concurrent,
                Some(store_str.clone()),
            ),
            FfiSessionInstanceOutcome::Degraded {
                cause: FfiInstanceDegrade::NoStateBase
            }
        );
    }

    /// `predecessor_chain` is the registry's paired walk: each id sits beside
    /// the key its own seed derives, and both agree with the two single-list
    /// accessors on a chain where the key-only walk would be unsafe to zip.
    #[test]
    fn the_predecessor_chain_pairs_each_identity_with_its_own_key() {
        let store: Arc<MapStore> = Arc::new(MapStore::default());
        let registry = FfiAccountRegistry::new(store);
        let old = registry.add_account(SECRET_A.into(), None, None).unwrap();
        let new = registry.add_account(SECRET_B.into(), None, None).unwrap();
        registry
            .record_succession(old.clone(), new.clone())
            .unwrap();

        let chain = registry.predecessor_chain(new.clone());
        assert_eq!(
            chain.actor_ids,
            registry.attested_predecessor_actor_ids(new.clone())
        );
        assert_eq!(chain.keys, registry.predecessor_backup_keys(new.clone()));
        assert_eq!(chain.actor_ids.len(), 1);
        assert_eq!(hex_of(&chain.actor_ids[0]), old);
        assert_eq!(chain.keys.len(), chain.actor_ids.len());

        let none = registry.predecessor_chain(old);
        assert!(none.actor_ids.is_empty() && none.keys.is_empty());
    }

    /// What the two engine-hosting FFI faces resolve off the registry they
    /// are handed: the sync agent's capability pairs
    /// (`SyncCapability::predecessor_keys_by_actor`) and the in-process
    /// host's named retired roots are the registry's one paired walk, off the
    /// seed's own actor — and an identity that never succeeded gets neither.
    #[cfg(all(feature = "sync-agent-provisioning", feature = "sync-engine-host"))]
    #[test]
    fn the_engine_hosts_resolve_the_paired_chain_off_the_seeds_own_actor() {
        let store: Arc<MapStore> = Arc::new(MapStore::default());
        let registry = FfiAccountRegistry::new(store);
        let old = registry.add_account(SECRET_A.into(), None, None).unwrap();
        let new = registry.add_account(SECRET_B.into(), None, None).unwrap();
        registry
            .record_succession(old.clone(), new.clone())
            .unwrap();
        let chain = registry.predecessor_chain(new);
        let (old_seed, new_seed) = ([0x11u8; 32], [0x22u8; 32]);

        // The provisioner's capability input.
        let pairs = registry.paired_predecessor_keys(&new_seed);
        assert_eq!(pairs.len(), 1);
        assert_eq!(hex_of(&pairs[0].0), old);
        assert_eq!(pairs[0].1, chain.keys[0]);
        assert!(registry.paired_predecessor_keys(&old_seed).is_empty());
        assert!(registry.paired_predecessor_keys(&new_seed[..16]).is_empty());

        // The in-process host's retired roots, each named by its identity.
        let roots = registry.predecessor_seal_keys(new_seed);
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].actor_id.map(|a| a.to_hex()), Some(old));
        assert!(registry.predecessor_seal_keys(old_seed).is_empty());

        // The File Provider host's provisioned chain is the same pairing, and
        // a chain that does not pair one to one is refused whole.
        let provisioned = chain.seal_keys().expect("a well-formed chain");
        assert_eq!(provisioned.len(), 1);
        assert_eq!(provisioned[0].actor_id, roots[0].actor_id);
        let shifted = FfiPredecessorChain {
            actor_ids: chain.actor_ids.clone(),
            keys: Vec::new(),
        };
        assert!(shifted.seal_keys().is_err());
        let short = FfiPredecessorChain {
            actor_ids: vec![vec![1u8; 16]],
            keys: chain.keys.clone(),
        };
        assert!(short.seal_keys().is_err());
    }

    fn hex_of(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// The token an FFI client names its per-account activate event from must
    /// be the SAME string the lock file is named from — one derivation, or a
    /// raiser and its server can address different names and neither ever
    /// learns why the raise "found nothing".
    #[test]
    fn the_account_token_crosses_the_seam_normalized() {
        assert_eq!(
            account_instance_token(format!("  {}  ", ACTOR_A.to_uppercase())).as_deref(),
            Some(ACTOR_A),
            "trim + lowercase must survive the seam"
        );
        assert_eq!(
            account_instance_token("not-an-actor".into()),
            None,
            "a value that is not ours must name no endpoint"
        );
    }

    /// A refused activation reaches the seat as the SHARED paint-ready line,
    /// never the registry's debug text: the target named by its switcher
    /// label, the user told they are still where they were
    /// (`switch_refused_copy`). The seats show a boundary `FfiError` as-is.
    #[test]
    fn a_refused_activation_carries_the_shared_refusal_line() {
        let store: Arc<MapStore> = Arc::new(MapStore::default());
        let registry = FfiAccountRegistry::new(store.clone());
        registry.add_account(SECRET_A.into(), None, None).unwrap();
        let b = registry.add_account(SECRET_B.into(), None, None).unwrap();
        store.delete(format!("fauna/{b}/secret"));

        let msg = match registry.set_active(b.clone()) {
            Err(FfiError::General { msg }) => msg,
            other => panic!("expected a General refusal, got {other:?}"),
        };
        let label = fauna_core::format::account_display_label(None, &b);
        let want = fauna_client_accounts::switch_refused_copy(
            &fauna_client_accounts::AccountError::NoStoredSecret(b.clone()),
            &label,
        )
        .resolve(fauna_i18n::strings::lookup);
        assert_eq!(msg, want);
        assert!(
            !msg.contains("no stored secret"),
            "the debug text must not reach a seat: {msg}"
        );
    }

    struct Seat {
        raises: bool,
        served: bool,
        asked: Mutex<Vec<String>>,
    }

    impl FfiFocusExistingSeat for Seat {
        fn try_raise(&self, actor_id: String) -> bool {
            self.asked.lock().unwrap().push(format!("raise {actor_id}"));
            self.raises
        }
        fn is_served(&self, actor_id: String) -> bool {
            self.asked.lock().unwrap().push(format!("probe {actor_id}"));
            self.served
        }
    }

    fn focus(actor: &str, raises: bool, served: bool) -> (FfiFocusExistingOutcome, Vec<String>) {
        let seat = Arc::new(Seat {
            raises,
            served,
            asked: Mutex::new(Vec::new()),
        });
        let outcome = resolve_focus_existing(actor.into(), seat.clone());
        let asked = seat.asked.lock().unwrap().clone();
        (outcome, asked)
    }

    /// The export forwards every arm of the shared decision — the windows
    /// seat's chooser consumes exactly this.
    #[test]
    fn focus_existing_over_the_ffi_forwards_every_arm_of_the_shared_decision() {
        let (outcome, asked) = focus(ACTOR_A, true, true);
        assert_eq!(outcome, FfiFocusExistingOutcome::Raised);
        assert_eq!(
            asked,
            vec![format!("raise {ACTOR_A}")],
            "a landed raise never probes"
        );

        let (outcome, asked) = focus(ACTOR_A, false, false);
        assert_eq!(outcome, FfiFocusExistingOutcome::NoLongerServed);
        assert_eq!(
            asked,
            vec![format!("raise {ACTOR_A}"), format!("probe {ACTOR_A}")]
        );

        let (outcome, _) = focus(ACTOR_A, false, true);
        assert_eq!(outcome, FfiFocusExistingOutcome::StillServedNoChannel);

        let (outcome, asked) = focus("  ", true, false);
        assert_eq!(outcome, FfiFocusExistingOutcome::StillServedNoChannel);
        assert!(
            asked.is_empty(),
            "an empty actor id has nothing to address: {asked:?}"
        );
    }
}
