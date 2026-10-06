//! Multi-account registry for Fauna apps (Stage 0).
//!
//! One app install (one OS-user context) may hold several Fauna
//! identities (actors) and switch between them. This crate owns the
//! **shared** account-management logic — the account list and the active
//! pointer — so every app consumes one implementation (priority #2) and
//! platforms provide only a thin key/value glue.
//!
//! ## Seam
//!
//! [`SecretStore`] is a generic **logical-keyed** key/value secret store.
//! Each platform maps the logical keys this crate uses onto its native
//! store (Keychain / libsecret / Credential Manager /
//! `EncryptedSharedPreferences` / `localStorage`). Logical keys — rather
//! than native names — keep *all* namespacing logic here.
//!
//! Storage layout:
//! - `fauna/index` — the non-secret [`AccountIndex`] blob (JSON): which
//!   accounts exist, which is active, and each account's cache + flags.
//!   Authoritative for existence, so no platform needs to *enumerate*
//!   its secret store.
//! - `fauna/{actor_id}/{secret,nest_url,device_id}` — the three
//!   per-actor slots (the existing single-identity three-slot contract,
//!   namespaced by actor id).
//!
//! The registry is the only identity store on every app: the onboarding
//! hand-off writes it directly ([`persist_confirmed_identity`],
//! [`persist_logged_in`]) and launch routes on it alone. (The pre-registry
//! `legacy/*` single slot and its downgrade mirror were retired 2026-09-24;
//! android, the last app on them, moved off 2026-09-28.)
//!
//! Relationship to `fauna_launch_machine::LaunchPersistence`: that is the
//! current typed, single-identity persistence seam. Stage 1 reconciles it
//! onto this registry (one underlying seam, not two parallel foreign
//! traits) — this reconciliation design is tracked internally.

use std::collections::BTreeMap;
use std::sync::Arc;

use fauna_core::identity::ActorKeypair;
use fauna_core::secret::SecretString;
use fauna_launch_machine::AccountIndexRefusal;
use serde::{Deserialize, Serialize};

/// The user-facing half of a sign-out's erase — what an app tells the user when
/// the sweep could not remove everything (`account-scoping.md` § Erasure follows
/// scope, the ⚠ *What this does NOT yet do is tell the USER* note).
mod credential_sweep;
pub use credential_sweep::CredentialSweep;
mod erase_residue;
pub use erase_residue::{
    EraseResidueCopy, EraseResidueView, remove_account_blocked_copy,
    remove_account_served_here_copy, sign_out_blocked_copy, sign_out_residue_retry_blocked_copy,
    start_over_blocked_copy,
};
mod add_refusal;
pub use add_refusal::add_refused_copy;
mod switch_refusal;
pub use switch_refusal::switch_refused_copy;

/// The sync device id's secret-store get-or-create (the 2026-09-20 derived
/// named-row id, `sync-agent-credentials.md` § Credential model).
mod device_id;
pub use device_id::{DeviceIdError, INSTALL_DEVICE_SECRET};

mod launch_persistence;
pub use launch_persistence::{
    RegistryLaunchPersistence, RegistryPendingProvisionStore, clear_awaiting_dns_for_active,
    persist_awaiting_dns, persist_confirmed_identity, persist_logged_in, persist_pending_invite,
};

#[cfg(not(target_arch = "wasm32"))]
mod lock_file;

mod mutation_lock;
#[cfg(not(target_arch = "wasm32"))]
pub use mutation_lock::FileMutationLock;
pub use mutation_lock::{MutationLock, MutationLockGuard, NoopMutationLock};

mod instance_lock;
#[cfg(not(target_arch = "wasm32"))]
pub use instance_lock::{
    AccountInstanceLock, FocusExistingOutcome, InstanceDegrade, InstanceLockOutcome,
    InstanceRefusal, ServingBases, ServingMode, SessionInstanceHolder, SessionInstanceOutcome,
    account_instance_token, become_process_session_instance, bind_session_launch_to,
    choosable_accounts, process_session_account, rebind_session_launch_after_succession,
    refuse_if_bound_from_onboarding, resolve_focus_existing, session_account,
    session_launch_binding,
};

/// The sign-out/remove-account guard: which accounts another live instance is
/// still serving, so an erase never unlinks the directory a sibling is running
/// out of (`account-scoping.md` § Concurrent instances).
#[cfg(not(target_arch = "wasm32"))]
mod sole_instance;
#[cfg(not(target_arch = "wasm32"))]
pub use sole_instance::{
    EraseBlocked, RemoveAccountBlocked, actors_served_by_another_instance, remove_account_blocked,
    remove_account_blocked_as, sign_out_blocked,
};

/// The sign-out residue that outlives the process: its install-scoped record
/// and the one gated re-sweep the retry control and the launch re-check share
/// (`account-scoping.md` § Erasure follows scope).
#[cfg(not(target_arch = "wasm32"))]
mod sign_out_residue;
#[cfg(not(target_arch = "wasm32"))]
pub use sign_out_residue::{
    ResiduePath, ResidueRetry, SIGN_OUT_RESIDUE_FILE, SignOutResidue, retry_sign_out_residue,
};

/// Web's sign-out record: the durable decision a browser sign-out (and a
/// remove-account) is made behind, and what the next load finishes it from
/// (`account-scoping.md` § Erasure follows scope, the web paragraph). Pure over
/// [`SecretStore`], so it builds and is tested on every target.
mod sign_out_record;
pub use sign_out_record::{SIGN_OUT_RECORD_KEY, SignOutPlan, SignOutRecord};

#[cfg(feature = "web-localstorage")]
mod web_store;
#[cfg(all(feature = "web-localstorage", target_arch = "wasm32"))]
pub use web_store::LocalStorageSecretStore;

/// Web's cross-tab registry mutation lock — the Web Locks leg of
/// [`MutationLock`], taken by every wasm-facing mutator entry from OUTSIDE
/// the synchronous mutator (a Web Lock is async; module docs).
#[cfg(feature = "web-localstorage")]
mod web_mutation_lock;
#[cfg(all(feature = "web-localstorage", target_arch = "wasm32"))]
pub use web_mutation_lock::{WEB_MUTATION_LOCK_NAME, with_web_mutation_lock};

/// The registry arms of the cross-app E2E bridge — the twin of
/// `fauna_onboarding_machine::call_machine_free_method` for seams that need the
/// app's own registry rather than process-global state. Gated by the same `cfg`
/// that bridge uses, so the automation surface is compiled out of release
/// artifacts (`e2e-conventions.md` § point 15).
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
mod e2e_bridge;
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
pub use e2e_bridge::{RegistryMethodOutcome, call_registry_method_for_test};

/// Logical key of the account-index blob.
pub const INDEX_KEY: &str = "fauna/index";

/// The widest `fauna/index` value the registry will write — one credential
/// item on the tightest backend (Windows Credential Manager), whose write is
/// best-effort and reports nothing, so an over-cap index would silently keep
/// its previous value and leave the slots just written beside it named by
/// nothing. The owner is `fauna_credential_store::MAX_ITEM_VALUE_BYTES`; that
/// crate depends on this one, so it pins the two equal at compile time rather
/// than this crate importing it. One number on every platform
/// (`long-term-store.md` § Multi-account evolution → *The index is bounded*).
pub const MAX_INDEX_VALUE_BYTES: usize = 2560;

fn secret_key(actor_id: &str) -> String {
    format!("fauna/{actor_id}/secret")
}
fn nest_url_key(actor_id: &str) -> String {
    format!("fauna/{actor_id}/nest_url")
}
fn device_id_key(actor_id: &str) -> String {
    format!("fauna/{actor_id}/device_id")
}
/// The **reach hint** — the freshly-provisioned box's public IP, kept beside
/// `nest_url` until the domain first connects (`long-term-store.md`
/// § Multi-account evolution; semantics owned by `onboarding.md` § Reach hint).
///
/// Deliberately NOT in the index blob: the index is the account *list* every
/// platform enumerates, and this is a per-account optimization absent on every
/// account that did not provision its own box.
fn reach_ipv4_key(actor_id: &str) -> String {
    format!("fauna/{actor_id}/reach_ipv4")
}
fn pending_invite_key(actor_id: &str) -> String {
    format!("fauna/{actor_id}/pending_invite")
}
fn awaiting_dns_key(actor_id: &str) -> String {
    format!("fauna/{actor_id}/awaiting_dns")
}
fn pending_factory_reset_key(actor_id: &str) -> String {
    format!("fauna/{actor_id}/pending_factory_reset")
}
fn pending_aftermath_ceremony_key(actor_id: &str) -> String {
    format!("fauna/{actor_id}/pending_aftermath_ceremony")
}
fn supervision_snapshot_key(actor_id: &str) -> String {
    format!("fauna/{actor_id}/supervision_snapshot")
}

/// The four slots `fauna-sync-engine::principal_bundle` writes directly
/// against this same store, over its own `attr(actor_id_hex, suffix)` key
/// shape — deliberately `{actor_id}/{suffix}`, WITHOUT the `fauna/` prefix
/// every builder above uses, because that crate builds its keys
/// independently of this module. `fauna-sync-engine` depends on
/// `fauna-client-accounts`, never the reverse, so its `ATTR_*` constants
/// cannot be imported here; the four literals are duplicated on purpose —
/// this is the one place outside that crate allowed to know their names,
/// because leaving them out means a signed-out (or factory-reset) user's
/// device authorization and backup key stay readable on disk.
fn device_auth_key(actor_id: &str) -> String {
    format!("{actor_id}/device-auth")
}
fn backup_key_key(actor_id: &str) -> String {
    format!("{actor_id}/backup-key")
}
fn generation_keys_key(actor_id: &str) -> String {
    format!("{actor_id}/generation-keys")
}
fn grant_registered_key(actor_id: &str) -> String {
    format!("{actor_id}/grant-registered")
}
/// The latch's sibling (2026-09-15): the nest's standing refusal of this
/// machine's enrollment (`principal_bundle::ATTR_ENROLLMENT_REFUSED` — the
/// tier device-cap verdict the Devices page renders). Not a secret, but
/// enrollment state with the grant's lifecycle all the same: a signed-out
/// machine has no enrollment to be refused, and a stale notice surviving into
/// the next sign-in would tell a fresh account about the old one's cap.
fn enrollment_refused_key(actor_id: &str) -> String {
    format!("{actor_id}/enrollment-refused")
}

/// The T10 **store writer signing key** slot, at the BARE actor id — no
/// `fauna/` prefix and, unlike the four above, no suffix either
/// (`fauna-sync-engine::account_runtime` § The writer key and the T10 slot:
/// "account attribute = the actor id hex"). It holds this machine's Ed25519
/// writer secret for the account's replica, so
/// `long-term-store.md` § Cleanup contract property 3 — no credential
/// *derived* from the identity outlives it — owes it the same erase as the
/// identity secret beside it.
///
/// Two sites in `fauna-sync-engine` write this one key, and they are the same
/// slot with the same meaning rather than a collision:
/// `account_runtime::writer_key_from_slot` mints it on the account's first
/// assembly on this machine, and `principal_succession::mint_into_slot`
/// re-keys it *in place* on the two rotation triggers. That second trigger is
/// also why erasing it is safe for a replica dir that outlives the erase:
/// refinement 10's lost-slot heal turns an empty slot over a stamped store
/// into a mint-and-fence with the un-pushed tail re-authored, never a
/// stranded replica.
fn store_writer_key(actor_id: &str) -> String {
    actor_id.to_string()
}

/// Every per-actor slot — the namespace [`Self::remove`], [`Self::clear_all`]
/// and [`Self::retire_superseded_provisionals`] all owe in full
/// (`long-term-store.md` § Cleanup contract). Single-sourced so a new slot is
/// covered by all three cleanup sites (the first two via
/// [`Self::delete_per_actor_everywhere`], the third both there and in
/// [`Self::is_provisional`]'s guard) and the witness test by construction,
/// rather than remembered by hand in three places (
/// `reach_ipv4` was remembered in two of the three and the test asserted only
/// four of the eight, silently, for a security-relevant erase; apps row 492 —
/// a third cleanup site existed for two hours before this doc comment, or its
/// witness census, ever saw it).
///
/// Three key *shapes* live here, and only the first is this module's own:
/// `fauna/{actor_id}/*` (the builders above), `{actor_id}/{suffix}` (the four
/// `principal_bundle` slots), and the bare `{actor_id}` (the T10 writer key).
/// A slot minted outside this file is invisible to `per_actor_keys` until it
/// is named here literally — which is exactly how the last two shapes came to
/// survive a sign-out.
///
/// [`Self::clear_nest_binding`] deliberately does NOT use this — it clears
/// only the nest binding (`nest_url`/`device_id`/`reach_ipv4`), keeping the
/// identity's `secret` and every other per-actor slot intact. That exclusion
/// is load-bearing now that [`store_writer_key`] is on the list: walking away
/// from a nest keeps the replica, and the replica's stamped writer must keep
/// the slot that holds its key.
const PER_ACTOR_KEY_BUILDERS: &[fn(&str) -> String] = &[
    secret_key,
    nest_url_key,
    device_id_key,
    reach_ipv4_key,
    pending_invite_key,
    awaiting_dns_key,
    pending_factory_reset_key,
    pending_aftermath_ceremony_key,
    supervision_snapshot_key,
    device_auth_key,
    backup_key_key,
    generation_keys_key,
    grant_registered_key,
    enrollment_refused_key,
    store_writer_key,
];

fn per_actor_keys(actor_id: &str) -> impl Iterator<Item = String> + '_ {
    PER_ACTOR_KEY_BUILDERS
        .iter()
        .map(move |build| build(actor_id))
}

/// A per-platform key/value secret store, addressed by *logical* keys.
///
/// Implementations map logical keys to native storage. All values are
/// strings (hex secrets, URLs, device ids, the JSON index) so every
/// platform's string-keyed secure store implements this trivially.
pub trait SecretStore: Send + Sync {
    /// Return the value for `key`, or `None` if unset.
    fn get(&self, key: &str) -> Option<String>;
    /// Set `key` to `value`, creating it if absent.
    fn set(&self, key: &str, value: &str);
    /// Delete `key` if present (no-op otherwise).
    fn delete(&self, key: &str);
}

/// Environment variable naming the account a **secondary instance** is bound to
/// for its whole lifetime (`account-scoping.md` § Concurrent instances). Value:
/// the actor id as 64 hex chars; unset or empty means an ordinary *primary*
/// launch on the active account.
///
/// This is **bucket-1 IPC, not configuration** (`principles.md` § the one
/// configuration surface): nobody ever types it. The spawning instance sets it
/// on the child it launches, off the account the user picked in the switcher —
/// so the *choice* is client UI, and this variable is only how one Fauna
/// process tells another which choice was made. It is deliberately the same
/// channel every app already uses for launch wiring, so the shape is uniform
/// across the fleet rather than per-platform (priority #1).
pub const BOUND_ACCOUNT_ENV: &str = "FAUNA_BOUND_ACCOUNT";

/// The account this process was launched **bound** to, if any — read from
/// [`BOUND_ACCOUNT_ENV`].
///
/// Returns the raw (trimmed, lowercased) value without judging it: validation
/// is [`AccountRegistry::bind_account`]'s job, and it must stay the **single**
/// gate. A second, weaker check here would be a side door around the activation
/// guards the bind gate mirrors — and a malformed value simply fails that gate
/// as `UnknownActor`, which is the fail-closed answer anyway.
///
/// A client that finds `Some` here must launch bound **or refuse**: falling
/// back to a plain launch would silently make a secondary instance a second
/// view of the *active* account — the confusion concurrent instances exists to
/// remove, and, for a `require_confirm_to_activate` account, a way past the
/// re-auth prompt.
pub fn requested_bound_account() -> Option<String> {
    parse_bound_account(std::env::var(BOUND_ACCOUNT_ENV).ok().as_deref())
}

/// The pure half of [`requested_bound_account`] (the process environment is
/// global mutable state, so the parsing rule is tested here rather than through
/// a `set_var` race).
fn parse_bound_account(raw: Option<&str>) -> Option<String> {
    let value = raw?.trim().to_ascii_lowercase();
    // An explicitly EMPTY value reads as "not bound", so a spawner can clear an
    // inherited binding without having to unset the variable.
    (!value.is_empty()).then_some(value)
}

/// This build's `fauna/index` schema. Bump on **every** shape change.
pub const CURRENT_INDEX_VERSION: u16 = 1;

/// The oldest build `schema_version` that can still safely **rewrite** an index
/// this build writes. Bumped **only** on a breaking, non-additive change (a
/// major-version event per I3). Leaving it at `1` while [`CURRENT_INDEX_VERSION`]
/// grows is what keeps an older build working against a newer index (I2).
pub const MIN_READER_INDEX_VERSION: u16 = 1;

/// An index with no version fields reads as this. Every current writer stamps
/// both, so a stamp-less blob is malformed, never evidence of a newer writer.
pub const BASELINE_INDEX_VERSION: u16 = 1;

const _: () = assert!(MIN_READER_INDEX_VERSION <= CURRENT_INDEX_VERSION);

fn baseline_index_version() -> u16 {
    BASELINE_INDEX_VERSION
}

/// The non-secret account index (serialized at `fauna/index`).
///
/// # Forward-compatible unknown-field preservation
///
/// This is a store an **older build rewrites wholesale**, so it carries the same
/// two defenses as the account-plane records in `fauna-core/src/data.rs` (the
/// richest existing pattern) plus the § 2.2 two-number version scheme:
///
/// - [`Self::extra`] captures any key this build has no named field for — a field
///   a *newer* build added — and re-emits it on save, so an older build never
///   silently **drops** what it did not understand.
/// - `(schema_version, min_reader_version)` let an older build **honestly detect**
///   an index it must not rewrite at all, rather than rewriting it from a guessed
///   single-account shape and orphaning every other account's identity secret (those
///   per-actor secret slots are reachable *only* through this index).
///
/// `Eq` is deliberately absent: `serde_json::Value` carries a float variant and
/// so is `PartialEq` but not `Eq`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountIndex {
    /// Actor id (hex) of the currently-active account, if any.
    #[serde(default)]
    pub active: Option<String>,
    /// All accounts known on this install, in add order.
    #[serde(default)]
    pub accounts: Vec<AccountEntry>,
    /// The shape this index was written in. Absent (a malformed blob) reads as
    /// [`BASELINE_INDEX_VERSION`].
    #[serde(default = "baseline_index_version")]
    pub schema_version: u16,
    /// The oldest build that may safely rewrite this index. Absent reads as
    /// [`BASELINE_INDEX_VERSION`].
    #[serde(default = "baseline_index_version")]
    pub min_reader_version: u16,
    /// Unknown keys a newer build wrote — preserved verbatim across a rewrite by
    /// this build. Empty in the steady state.
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// The version pair alone, decoded from a `fauna/index` blob **without**
/// requiring the rest of the index to parse.
///
/// The ratified peek pattern, mirroring `fauna-core`'s `ConfigVersionStamp` and
/// `fauna-index`'s `IndexManifest::peek_stamp` — `version-compatibility.md` § 5
/// item 9 note (a) calls it "the difference between an actionable 'update the
/// app; your config is intact' and an unactionable 'corrupt'".
///
/// It names only the two version fields on purpose: serde ignores map keys a
/// struct has no field for, so this still decodes out of an index whose *other*
/// fields a future **non-additive** change has made unreadable to this build —
/// which is exactly the `actor_id`-retype cliff § 5 item 9 names. Without the
/// peek that case reads as "corrupt" and its real numbers are lost.
///
/// Both fields default to [`BASELINE_INDEX_VERSION`], so a stamp-less blob
/// peeks as baseline rather than failing — and a baseline peek is therefore
/// *not* evidence of a newer writer, which is why the caller treats it as
/// malformed rather than as a version refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountIndexStamp {
    /// The shape this index was written in.
    #[serde(default = "baseline_index_version")]
    pub schema_version: u16,
    /// The oldest build that may safely rewrite this index.
    #[serde(default = "baseline_index_version")]
    pub min_reader_version: u16,
}

impl Default for AccountIndexStamp {
    fn default() -> Self {
        Self {
            schema_version: BASELINE_INDEX_VERSION,
            min_reader_version: BASELINE_INDEX_VERSION,
        }
    }
}

/// Hand-written rather than derived: `u16::default()` is `0`, and a version of
/// `0` is not a thing this store can mean. A fresh index is a *baseline* index.
impl Default for AccountIndex {
    fn default() -> Self {
        Self {
            active: None,
            accounts: Vec::new(),
            schema_version: BASELINE_INDEX_VERSION,
            min_reader_version: BASELINE_INDEX_VERSION,
            extra: BTreeMap::new(),
        }
    }
}

impl AccountIndex {
    fn position(&self, actor_id: &str) -> Option<usize> {
        self.accounts.iter().position(|a| a.actor_id == actor_id)
    }
}

/// One account's non-secret metadata: identity + server-data cache
/// (handle/domain/tier, mirroring `long-term-store.md` § cache) + flags.
///
/// Carries the same [`AccountIndex::extra`] catch-all, for the same reason: an
/// older build must re-emit a per-entry field a newer build added rather than
/// drop it on the next rewrite. `Eq` absent for the same reason as on
/// [`AccountIndex`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountEntry {
    /// Actor id (Ed25519 public key), lowercase hex.
    pub actor_id: String,
    #[serde(default)]
    pub handle: Option<String>,
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default)]
    pub tier: Option<String>,
    /// When true, activating this account requires a re-auth confirmation
    /// (biometric / OS prompt). Default off; a client turns it on for its
    /// admin identity (design Decision 2). The registry never learns
    /// admin-ness itself — that stays a nest `am-i-admin` concern.
    #[serde(default)]
    pub require_confirm_to_activate: bool,
    /// True once a human has set (or cleared) `require_confirm_to_activate`
    /// via the toggle ([`AccountRegistry::set_require_confirm`]). While false,
    /// [`AccountRegistry::auto_enable_require_confirm`] — the admin
    /// auto-default — may still flip the flag on; once true, the user's
    /// explicit choice sticks and the auto-default never writes again.
    /// Additive at rest (`long-term-store.md` § Multi-account evolution).
    #[serde(default)]
    pub require_confirm_user_set: bool,
    /// The identity that **succeeded** this one, when this row is a retired
    /// predecessor — set by [`AccountRegistry::record_succession`]
    /// (`succession-aftermath.md` § Re-key scope).
    ///
    /// It exists for the post-succession corpus re-seal, which must open
    /// at-rest blobs under a retired identity's `BackupKey` and therefore has
    /// to know **which** rows are predecessors. Without the link the only
    /// available answer is "every other account", and that answer leaks: an
    /// unrelated account's key legitimately opens *its own* sealed
    /// custody, so a pass fed the whole registry would fold a
    /// different account's deployment seeds into this one.
    ///
    /// The **immediate** successor, never re-pointed as the chain grows: "B
    /// succeeded A" is a fact about one ceremony, and a later A→B→C hop leaves
    /// it alone. Walking the chain is [`AccountRegistry::predecessors_of`]'s
    /// job, which is free over an in-memory account list. ⚠ The nest
    /// deliberately means the *other* thing — `succession_ownership::heal_at_boot`
    /// moves a crash-interrupted directory straight to the **terminal**
    /// successor — because a filesystem owner has no chain to walk; the two
    /// sides differ on purpose, so do not "fix" either to match the other. Additive at rest
    /// (`long-term-store.md` § Multi-account evolution): an index written
    /// before this field existed loads with `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub succeeded_by: Option<String>,
    /// Every retired identity this one **succeeded from**, nearest hop first —
    /// the same link as [`Self::succeeded_by`], kept on the *successor's* row so
    /// it outlives the predecessor's.
    ///
    /// `succeeded_by` lives on the retired row, and that row is legitimately
    /// absent: a device that never held it, a factory reset, or simply the user
    /// removing the retired account ([`AccountRegistry::remove`] deletes the row
    /// outright). A link stored only there dies with it, and the successor's
    /// inherited profile then has no writer anywhere (`profile.md` § After an
    /// identity succession). This list is what [`AccountRegistry::predecessors_of`]
    /// falls back on. It holds ids only, never material: a listed identity whose
    /// row is gone is the ordinary "no material here" case every predecessor
    /// consumer already handles.
    ///
    /// Written by [`AccountRegistry::record_succession`] (the ceremony, the
    /// escrow restore) and [`AccountRegistry::record_predecessors`] (a link
    /// proven from the landed statement). Additive at rest: an empty list is omitted on write, so an entry without the
    /// field loads with an empty list, and an older build re-emits the key through `extra`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub succeeded_from: Vec<String>,
    /// Unknown keys a newer build wrote — preserved verbatim across a rewrite.
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl AccountEntry {
    fn new(actor_id: String) -> Self {
        Self {
            actor_id,
            handle: None,
            domain: None,
            tier: None,
            require_confirm_to_activate: false,
            require_confirm_user_set: false,
            succeeded_by: None,
            succeeded_from: Vec::new(),
            extra: BTreeMap::new(),
        }
    }
}

/// [`AccountRegistry::predecessors_of`] over an account list already in hand —
/// a mutator computes it under its own lock, over the index it is about to
/// write.
///
/// Two sources, one answer. The `succeeded_by` walk comes first: it is the
/// witnessed record, and it is what makes index 0 a *direct* predecessor
/// whenever that row is here (`fauna_client_recovery::ceremony::retry_predecessor`
/// leans on that). The successor-side [`AccountEntry::succeeded_from`] lists —
/// `actor_id`'s own, then each reached row's — append whatever the walk could
/// not see because a row is gone; each list is itself nearest hop first.
fn predecessors_in(accounts: &[AccountEntry], actor_id: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    // A queue, not a stack: nearest-hop-first requires draining the frontier
    // in the order its hops were discovered (`pop_front`), not the reverse (a
    // `Vec`'s `pop` is LIFO — depth-first — and produces a hop sequence like
    // [1, 1, 2, 3, 2] instead of [1, 1, 2, 2, 3] the instant a successor has
    // more than one direct predecessor; every doc comment over this walk,
    // `predecessor_backup_keys` and `predecessor_seeds` included, promises
    // nearest-hop-first, and nothing pinned that this queue actually behaves
    // that way).
    let mut frontier = std::collections::VecDeque::from([actor_id.to_string()]);
    // Breadth-first over "who was succeeded by someone already in the set",
    // bounded by the account count so a corrupted cyclic index cannot spin.
    while let Some(successor) = frontier.pop_front() {
        for entry in accounts {
            if entry.succeeded_by.as_deref() != Some(successor.as_str()) {
                continue;
            }
            if entry.actor_id == actor_id || out.contains(&entry.actor_id) {
                continue;
            }
            out.push(entry.actor_id.clone());
            frontier.push_back(entry.actor_id.clone());
        }
    }
    // Ids only from here on, so nothing is walked further: a listed identity
    // with no row has no `succeeded_by` edge to follow, and its own ancestors
    // are already in the list that named it (`record_succession` stores the
    // whole chain).
    let reached: Vec<String> = std::iter::once(actor_id.to_string())
        .chain(out.iter().cloned())
        .collect();
    for holder in &reached {
        let Some(entry) = accounts.iter().find(|e| e.actor_id == *holder) else {
            continue;
        };
        for id in &entry.succeeded_from {
            if id != actor_id && !out.contains(id) {
                out.push(id.clone());
            }
        }
    }
    out
}

/// Merge `predecessors` into `new_actor`'s [`AccountEntry::succeeded_from`],
/// keeping existing order and dropping a self-link. Returns whether the index
/// changed; an unknown `new_actor` changes nothing (the callers refuse it
/// first).
fn merge_succeeded_from(idx: &mut AccountIndex, new_actor: &str, predecessors: &[String]) -> bool {
    let Some(pos) = idx.position(new_actor) else {
        return false;
    };
    let list = &mut idx.accounts[pos].succeeded_from;
    let before = list.len();
    for id in predecessors {
        if id != new_actor && !list.contains(id) {
            list.push(id.clone());
        }
    }
    list.len() != before
}

/// The three per-actor secret slots read back for a given account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredAccount {
    pub actor_id: String,
    pub secret_hex: SecretString,
    pub nest_url: Option<String>,
    pub device_id: Option<String>,
}

/// Same pin as [`SessionMaterial`]'s below, on the read it is built *from*.
/// `StoredAccount` holds the identical account secret, so leaving it a bare
/// `String` split the family the carrier-shape rule is about
/// (`key-material-hierarchy.md` § Carrier shape — the residual that bullet
/// named on 2026-08-19 and this flip closes).
const _STORED_ACCOUNT_SECRET_IS_REDACTED: fn(&StoredAccount) -> &SecretString = |s| &s.secret_hex;

/// Everything a client needs to build its authenticated session for one
/// account, in one read: the three per-actor secret slots
/// ([`AccountRegistry::secrets`]) plus the account's index-entry server-data
/// cache (handle/domain/tier).
///
/// This is the **session-identity read** of `account-scoping.md` § Concurrent
/// instances → *Session identity resolves through the session's account*: a
/// process resolves its session account once at launch-binding resolution and
/// reads its material through this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionMaterial {
    pub actor_id: String,
    pub secret_hex: SecretString,
    pub nest_url: Option<String>,
    pub device_id: Option<String>,
    pub handle: Option<String>,
    pub domain: Option<String>,
    pub tier: Option<String>,
}

/// Revert `secret_hex` to a bare `String` and this fails the build — `String`
/// derives `Debug` verbatim, so `{:?}` on `SessionMaterial` would print the
/// account secret in clear (`key-material-hierarchy.md` § Carrier shape, the
/// residual). `SecretString`'s own `Debug` impl redacts.
const _SESSION_MATERIAL_SECRET_IS_REDACTED: fn(&SessionMaterial) -> &SecretString =
    |m| &m.secret_hex;

/// Errors from registry operations.
#[derive(Debug, thiserror::Error)]
pub enum AccountError {
    /// The provided secret hex was not a valid 32-byte Ed25519 secret.
    #[error("invalid secret: {0}")]
    InvalidSecret(#[from] fauna_core::hex32::Hex32Error),
    /// No account with this actor id is known on this install.
    #[error("unknown actor: {0}")]
    UnknownActor(String),
    /// The account is listed in the index but has no resolvable secret, so the
    /// client cannot launch as it. Refused by [`AccountRegistry::set_active`] —
    /// activating it would tear the live session down and strand the client in
    /// the onboarding wizard with no route back to the switcher.
    #[error("no stored secret for actor: {0}")]
    NoStoredSecret(String),
    /// The account's `require_confirm_to_activate` flag is set and the caller
    /// has not asserted a completed re-auth confirmation. Returned by
    /// [`AccountRegistry::set_active`] and by [`AccountRegistry::bind_account`]
    /// — binding a secondary instance is held to the same bar as activating, so
    /// "open as new instance" cannot become a side door around the flag. A
    /// client that has just walked the user through the biometric/OS re-auth
    /// prompt calls [`AccountRegistry::set_active_confirmed`] /
    /// [`AccountRegistry::bind_account_confirmed`] instead (Stage 2,
    /// `long-term-store.md` § Multi-account evolution). Enforced here rather
    /// than left to each app's pre-check so a client that forgets the
    /// prompt fails loudly instead of silently skipping the security gate.
    #[error("activating actor {0} requires a re-auth confirmation")]
    ConfirmationRequired(String),
    /// An index **exists** at `fauna/index` and a newer build raised
    /// `min_reader_version` past us. **Every number in this error was read off
    /// the blob**, never invented — including in the case where the rest of the
    /// payload does not parse and only the stamp peeks out
    /// ([`AccountIndexStamp`]). A blob with no readable stamp is
    /// [`AccountError::IndexMalformed`] instead, because "update the app" is
    /// false there.
    ///
    /// Every mutator refuses on this, and that refusal is the whole point. The
    /// per-actor identity secrets (`fauna/{actor}/secret`) are reachable *only*
    /// through the index, so a build that "helpfully" fell back to a guessed
    /// single-account shape and rewrote the index would leave every other account's
    /// secret stranded in the platform keystore, referenced by nothing —
    /// unrecoverable from any client UI (I1). Refusing keeps the index, and the
    /// accounts it names, exactly as they are: the user updates the app and
    /// everything is still there.
    #[error(
        "the account index was written by a newer build (schema_version={index_v}, min_reader_version={index_min}, this build={bin_v}) — the accounts are intact; update the app"
    )]
    IndexUnreadable {
        index_v: u16,
        index_min: u16,
        bin_v: u16,
    },
    /// An index **exists** at `fauna/index`, this build cannot parse it, and no
    /// version stamp can be peeked out of it either — so nothing about a newer
    /// build explains it and **updating the app cannot help**.
    ///
    /// Split out of [`AccountError::IndexUnreadable`] because the two have
    /// opposite remedies and that arm used to report this one with *invented*
    /// baseline numbers — printing `schema_version=1, min_reader_version=1,
    /// this build=1 — update the app`, which is self-contradictory on its face.
    /// `version-compatibility.md` § 5 item 10 states the general rule this
    /// follows: collapsing "written by a newer build, data intact" into a
    /// generic "corrupt" is what licenses a caller to *heal* by overwriting.
    /// The inverse conflation is just as wrong, and is what this variant fixes.
    ///
    /// The refusal itself is **unchanged and deliberate**: this build still
    /// rewrites nothing. A blob nothing here can parse is not thereby known to
    /// name no accounts — only that *this* build cannot see them — so the
    /// no-overwrite guarantee (I1) applies exactly as it does to the version
    /// case. Recovery is the client-side floor, not a rewrite:
    /// `long-term-store.md` § Cleanup contract, whose own residual for a
    /// corrupt index is stated there.
    #[error(
        "the account index at `fauna/index` is present but cannot be parsed, and carries no readable version stamp — so this is not an out-of-date app and updating will not help; nothing has been changed and the index is left byte-for-byte intact, but this build can neither list nor modify accounts while it stands"
    )]
    IndexMalformed,
    /// The change would grow `fauna/index` past [`MAX_INDEX_VALUE_BYTES`], the
    /// most one credential item holds. Refused **before anything is written**
    /// — no slot, no index — because the tightest backend's write reports
    /// nothing: an over-cap index there keeps its previous value, and a secret
    /// slot written ahead of it would be an identity the app holds and cannot
    /// reach. A change that does not grow the stored index is never refused,
    /// so removing an account always lands.
    #[error(
        "this device's account list is full: the change needs {needed} bytes and the list holds {limit} — nothing was changed; remove an account this device no longer uses, then try again"
    )]
    IndexFull { needed: usize, limit: usize },
}

/// What `fauna/index` currently holds. The distinction this type exists to draw
/// is between *absent* and *present-but-unreadable* — collapsing those two into
/// one `None` is what once let an unreadable index be "healed" by overwriting
/// it with a fresh shape.
enum IndexState {
    /// No index blob at all — a fresh install.
    Absent,
    /// Present and fully understood by this build.
    Readable(Box<AccountIndex>),
    /// Present, and a `min_reader_version` past this build says so — numbers
    /// **read** off the blob, whether the whole index parsed or only its stamp
    /// peeked. **Never** rewrite it.
    Unreadable { index_v: u16, index_min: u16 },
    /// Present, unparseable, and carrying no readable stamp either. Same
    /// no-rewrite rule; opposite remedy — a newer binary cannot help here, so
    /// this must never be reported as a version refusal.
    Malformed,
}

/// The shared multi-account registry over a [`SecretStore`].
///
/// Cheap to clone (it wraps a single `Arc<dyn SecretStore>` and holds no
/// other state — every read/write goes straight to the store), so the
/// launch adapter ([`RegistryLaunchPersistence`]) and the switcher UI can
/// each hold an independent view over the same underlying store.
///
/// # Invariant: a read never writes
///
/// [`Self::index`] (and everything over it — [`Self::active`], [`Self::list`],
/// [`Self::secrets`]) is a **pure read**.
///
/// This is load-bearing for sign-out, not a stylistic preference. `index()`
/// once performed a migration's four sequential `set`s as a *side effect of
/// a read*, reachable from any thread. An erase is not atomic against a
/// concurrent writer (no secure store offers delete-by-attribute in one
/// transaction), so a wipe racing that lazy migration left its later writes —
/// up to and including the identity secret — behind in the namespace the user
/// had just erased: sign-out that did not sign them out. Ordering each caller
/// around the accessor is whack-a-mole; making reads pure kills the writer
/// class outright, so an un-cancellable reader that races a wipe is harmless by
/// construction. See `long-term-store.md` § Cleanup contract.
/// # Cross-process mutation serialization
///
/// Every **mutator** additionally acquires the registry's [`MutationLock`]
/// exactly once at entry (concurrent instances make mutation multi-process;
/// see `mutation_lock.rs` module docs). Reads and the [`Self::bind_account`]
/// spawn gate never acquire it — the purity invariant above extends to
/// "a read never locks", so launch stays wait-free against a wedged holder.
/// Internal nesting goes through `*_locked` variants: OS file locks contend
/// between separate opens even within one process, so a nested public
/// re-acquire would self-deadlock.
#[derive(Clone)]
pub struct AccountRegistry {
    store: Arc<dyn SecretStore>,
    lock: Arc<dyn MutationLock>,
    /// **Erase-only** additional stores: other credential namespaces that hold
    /// per-actor slots for the *same* accounts this registry indexes.
    ///
    /// A credential key is a *(namespace, key)* pair and every backend binds
    /// the namespace into the physical row (keyring: the service name; file:
    /// one file per namespace; foreign: a `{app}/{account}` prefix), so
    /// deleting `{actor}/device-auth` in *this* store cannot reach the row
    /// `fauna-account-store` wrote under the same logical key. The registry
    /// cannot construct that other store — a platform implements only
    /// [`SecretStore`], one foreign seam and not two (this crate's manifest) —
    /// so it is *handed* one instead, by the crate that owns namespaces:
    /// `fauna_credential_store::account_registry`, the single constructor
    /// every erase-capable app builds through.
    ///
    /// Only [`Self::clear_all`] and [`Self::remove`] ever touch these. The
    /// index and every read stay on `store` alone: the
    /// auxiliary namespace holds slots *about* accounts, never the account
    /// list itself. Deleting an absent key is a no-op on every backend, so
    /// sweeping the full builder set in every store is safe and uniform —
    /// there is no per-namespace key subset to keep in step.
    ///
    /// Empty is a real answer, not an omission: web's registry
    /// (`fauna-wasm`) has no account runtime at all — the account data plane
    /// declares web an absence (`account-data-plane.md` § Implementation
    /// status today) — so no second namespace exists for it to sweep.
    ///
    /// Contract: `long-term-store.md` § Cleanup contract.
    aux_erase: Vec<Arc<dyn SecretStore>>,
}

impl AccountRegistry {
    /// Registry with **no** cross-process lock ([`NoopMutationLock`]) — the
    /// single-process constructor: wasm (its cross-tab story is the Web Locks
    /// leg of concurrent instances) and pre-concurrent-instances call sites.
    pub fn new(store: Arc<dyn SecretStore>) -> Self {
        Self::with_mutation_lock(store, Arc::new(NoopMutationLock))
    }

    /// Registry whose mutators serialize under `lock` — required on any
    /// platform before concurrent instances ship
    /// (`account-scoping.md` § Concurrent instances). Native platforms pass a
    /// [`FileMutationLock`] over the install-scoped state dir.
    pub fn with_mutation_lock(store: Arc<dyn SecretStore>, lock: Arc<dyn MutationLock>) -> Self {
        Self {
            store,
            lock,
            aux_erase: Vec::new(),
        }
    }

    /// Widen the erase to `stores` — every other credential namespace holding
    /// per-actor slots for these accounts (see [`Self::aux_erase`]).
    ///
    /// ⚠ Do not call this from an app. Its one production caller is
    /// `fauna_credential_store::account_registry`, which owns the list of
    /// auxiliary namespaces; an app that assembled its own list would be one
    /// more place to forget the next namespace, which is precisely the failure
    /// this whole seam exists to close.
    #[must_use]
    pub fn also_erasing(mut self, stores: Vec<Arc<dyn SecretStore>>) -> Self {
        self.aux_erase = stores;
        self
    }

    /// Write `actor_id`'s identity secret — the one door every secret-slot
    /// write goes through, so the E2E bridge's `refuse_secret_writes_for_test`
    /// fault (a keystore that takes the write and keeps nothing) reaches every
    /// writer. The fault check is compiled out of release artifacts with the
    /// bridge (`e2e-conventions.md` § point 15).
    fn write_secret_slot(&self, actor_id: &str, secret_hex: &str) {
        #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
        if e2e_bridge::secret_writes_refused(&self.store) {
            return;
        }
        self.store.set(&secret_key(actor_id), secret_hex);
    }

    /// Delete every per-actor slot for `actor_id` from `store` **and** from
    /// each auxiliary namespace. The one place either erase deletes per-actor
    /// keys, so a namespace added to [`Self::aux_erase`] is covered by
    /// [`Self::clear_all`], [`Self::remove`] and
    /// [`Self::retire_superseded_provisionals`] alike, by construction.
    fn delete_per_actor_everywhere(&self, actor_id: &str) {
        for key in per_actor_keys(actor_id) {
            self.store.delete(&key);
            for aux in &self.aux_erase {
                aux.delete(&key);
            }
        }
    }

    /// True if `actor_id` holds any per-actor slot **but the secret**, in
    /// `self.store` **or** any auxiliary erase namespace. The read-side twin
    /// of [`Self::delete_per_actor_everywhere`]'s write-side sweep, single-
    /// sourced so [`Self::is_provisional`]'s guard can never test a narrower
    /// namespace set than the erase it gates.
    ///
    /// before this, the guard read `self.store` alone, but
    /// the four `principal_bundle` slots and the bare-actor-id T10 writer key
    /// are written through `production_credential_store()` into the
    /// auxiliary `fauna-account-store` namespace, never this one
    /// (`long-term-store.md` § Implementation status today, hole 3). So those
    /// five of thirteen slots were vacuously "absent" to the guard in every
    /// shipping build, and a row still carrying real key material there could
    /// pass as a pristine moment-1 stub and be destroyed.
    ///
    /// Not reachable today, verified rather than assumed: every account
    /// runtime assembles from an already-`LoggedIn` client
    /// (`resolve_and_start` needs a `nest_url`), and `save_authenticated`
    /// writes `handle`/`domain`/`tier` before `set_nest_url` — so by the time
    /// any of these five slots could be minted, the entry already fails
    /// `pristine_entry` on its first, cheaper test. This closes the guard
    /// against a *design* it no longer matches, not a data loss the fleet has
    /// seen; the fix stands regardless, since the two tests are not supposed
    /// to overlap in what they defend.
    fn has_any_per_actor_slot(&self, actor_id: &str) -> bool {
        let secret = secret_key(actor_id);
        per_actor_keys(actor_id)
            .filter(|key| *key != secret)
            .any(|key| {
                self.store.get(&key).is_some()
                    || self.aux_erase.iter().any(|aux| aux.get(&key).is_some())
            })
    }

    /// The current account index. No index blob yet means no accounts — a
    /// fresh install, until the wizard's first commit
    /// ([`persist_confirmed_identity`]) writes account #1.
    pub fn index(&self) -> AccountIndex {
        match self.index_state() {
            IndexState::Readable(idx) => *idx,
            IndexState::Absent => AccountIndex::default(),
            // An index exists that this build cannot read. Report *nothing*
            // rather than any guessed shape: a guess is a lie (it hides every
            // other account), and it used to be the first step on the path
            // that overwrote the real index and orphaned their secrets. Reads
            // are pure, so nothing is lost here — and every write
            // path refuses outright (`AccountError::IndexUnreadable`).
            //
            // A malformed index reports nothing for the same reason, and the
            // reason is NOT "it names no accounts" — we cannot know that. It is
            // that this build has no honest reading of it at all.
            IndexState::Unreadable { .. } | IndexState::Malformed => AccountIndex::default(),
        }
    }

    /// The actor id (hex) of the active account, if any.
    pub fn active(&self) -> Option<String> {
        self.index().active
    }

    /// All known accounts, in add order.
    pub fn list(&self) -> Vec<AccountEntry> {
        self.index().accounts
    }

    /// Add (or update) an account from its secret hex + optional slots.
    /// Derives the actor id from the secret. If this is the first
    /// account, it becomes active. Returns the actor id (hex).
    pub fn add_account(
        &self,
        secret_hex: &str,
        nest_url: Option<&str>,
        device_id: Option<&str>,
    ) -> Result<String, AccountError> {
        let actor_id = ActorKeypair::from_secret_hex(secret_hex)?.actor_id_hex();
        let _mutation = self.lock.acquire();

        // The index FIRST: an unreadable index refuses before any slot lands.
        let mut idx = self.writable_index()?;
        if idx.position(&actor_id).is_none() {
            idx.accounts.push(AccountEntry::new(actor_id.clone()));
        }
        if idx.active.is_none() {
            idx.active = Some(actor_id.clone());
        }
        // The bound BEFORE the secret: an add the index cannot hold must write
        // nothing, above all not a secret the unchanged index would not name
        // (`long-term-store.md` § Multi-account evolution → *The index is bounded*).
        let raw = self.encode_index(&idx)?;

        self.write_secret_slot(&actor_id, secret_hex);
        // ⚠ Read back BEFORE anything else lands. `SecretStore::set` is
        // infallible by signature, so a keystore that refused the write (a
        // locked keyring, a full quota) is visible only here — and `Ok` from
        // this fn is the promise every caller relies on that the account can
        // launch. Refusing before the other slots and the index keeps a failed
        // add from leaving an indexed-but-unlaunchable row behind.
        if self.store.get(&secret_key(&actor_id)).is_none() {
            return Err(AccountError::NoStoredSecret(actor_id));
        }
        if let Some(url) = nest_url {
            self.store.set(&nest_url_key(&actor_id), url);
        }
        if let Some(dev) = device_id {
            self.store.set(&device_id_key(&actor_id), dev);
        }
        self.store.set(INDEX_KEY, &raw);
        Ok(actor_id)
    }

    /// Make `actor_id` the active account. Errors if unknown, or if the account
    /// has no resolvable secret.
    ///
    /// **The secret check is load-bearing, not defensive tidying.** Activating is
    /// the one point where a client commits to tearing down its live session and
    /// relaunching as somebody else, so this is the last place the switch can fail
    /// safely. An index entry whose secret slot is gone is reachable — [`SecretStore::set`]
    /// is infallible by signature, so a keystore write that silently failed (or a row
    /// the OS/user removed out from under us) leaves the entry listed but unlaunchable.
    /// Activating it anyway would strand the client: the caller tears the session down,
    /// relaunch finds no identity and routes to the onboarding wizard — with the broken
    /// account now *active* and the switcher (which lives behind an authenticated
    /// session) unreachable, so the healthy account cannot be selected back. That is a
    /// half-state a user cannot get out of from their client, which the crash-safe
    /// corollary forbids (`nest/common.md` § Client-state recoverability).
    ///
    /// It is enforced **here** rather than in each app's switch handler because
    /// seven apps each remembering the same `if` is seven chances to forget it. Linux's pre-`set_active` `secrets()`
    /// probe is now belt-and-braces rather than the only guard.
    ///
    /// **The re-auth gate (Stage 2) is enforced here for the same reason.** An
    /// account whose `require_confirm_to_activate` flag is set refuses with
    /// [`AccountError::ConfirmationRequired`]; the client runs its re-auth
    /// prompt (biometric / OS prompt) and, on success, calls
    /// [`Self::set_active_confirmed`]. The registry cannot verify a real
    /// re-auth happened — the client asserts it — but routing every activation
    /// through this gate turns "forgot to prompt" into a hard error instead of
    /// a silent skip. A declined prompt is a pure no-op: nothing was mutated,
    /// the current account stays active.
    pub fn set_active(&self, actor_id: &str) -> Result<(), AccountError> {
        self.activate(actor_id, false)
    }

    /// [`Self::set_active`], with the caller asserting the user has **just
    /// completed** a re-auth confirmation for this activation. Only ever call
    /// this adjacent to the platform's re-auth prompt (apple `LAContext`, …);
    /// call sites are the audit surface for the Stage-2 gate.
    pub fn set_active_confirmed(&self, actor_id: &str) -> Result<(), AccountError> {
        self.activate(actor_id, true)
    }

    fn activate(&self, actor_id: &str, confirmed: bool) -> Result<(), AccountError> {
        let _mutation = self.lock.acquire();
        let mut idx = self.writable_index()?;
        let Some(pos) = idx.position(actor_id) else {
            return Err(AccountError::UnknownActor(actor_id.to_string()));
        };
        if self.secrets(actor_id).is_none() {
            return Err(AccountError::NoStoredSecret(actor_id.to_string()));
        }
        // After the launchability guard: an unlaunchable account reads
        // NoStoredSecret even when flagged — no re-auth prompt could fix it.
        if idx.accounts[pos].require_confirm_to_activate && !confirmed {
            return Err(AccountError::ConfirmationRequired(actor_id.to_string()));
        }
        idx.active = Some(actor_id.to_string());
        self.write_index(&idx)?;
        Ok(())
    }

    /// Validate that a **secondary instance** may launch bound to `actor_id`
    /// without moving the active pointer — the concurrent-instances gate
    /// (`architecture/apps/account-scoping.md` § Concurrent instances: the
    /// instance unit is (OS user, account); `active` stays the launch default
    /// + sync-surface owner, and a bound launch never touches it).
    ///
    /// **Pure read**, unlike [`Self::set_active`]: binding mutates nothing —
    /// a spawn-time check that wrote slots would re-open the read-that-writes
    /// hazard from a code path that can race a concurrent sign-out.
    ///
    /// Mirrors the activation guards 1:1 so "open as new instance" cannot
    /// become a side door around them: unknown account →
    /// [`AccountError::UnknownActor`]; no resolvable secret →
    /// [`AccountError::NoStoredSecret`] (launchability first, same order as
    /// activation); a `require_confirm_to_activate`-flagged account →
    /// [`AccountError::ConfirmationRequired`] — the client runs the same
    /// platform re-auth it runs for activation, then calls
    /// [`Self::bind_account_confirmed`], whose call sites are audit points
    /// exactly like [`Self::set_active_confirmed`]'s.
    pub fn bind_account(&self, actor_id: &str) -> Result<(), AccountError> {
        self.check_bindable(actor_id, false)
    }

    /// [`Self::bind_account`], with the caller asserting the user has **just
    /// completed** a re-auth confirmation for this bind. Only ever call this
    /// adjacent to the platform's re-auth prompt.
    pub fn bind_account_confirmed(&self, actor_id: &str) -> Result<(), AccountError> {
        self.check_bindable(actor_id, true)
    }

    /// The [`PendingProvisionStore`](fauna_launch_machine::PendingProvisionStore)
    /// over **this registry** — hand to `OnboardingMachine::new_with_persistence`
    /// so the wizard can land the pending-provision slot before it builds a box
    /// (`docs/goal/behavior/onboarding.md` § 6 *The pending-provision slot*).
    ///
    /// The twin of [`Self::launch_persistence`], and separate for the reason its
    /// trait documents: this one is addressed by the identity being onboarded,
    /// that one by the session's account.
    pub fn pending_provision_store(&self) -> RegistryPendingProvisionStore {
        RegistryPendingProvisionStore::from_registry(self.clone())
    }

    /// The [`LaunchPersistence`](fauna_launch_machine::LaunchPersistence)
    /// adapter over **this** registry — pass to `LaunchMachine::new` so the
    /// launch flow reads and writes the active account's slots.
    ///
    /// Minting the adapter *from the registry* is what makes the mutation lock
    /// whole. The adapter's `save_authenticated` is a full index
    /// read-modify-write on the busiest write path there is; a constructor that
    /// took a bare store would have to invent a registry, and an invented one
    /// carries [`NoopMutationLock`] — so a platform could adopt
    /// [`Self::with_mutation_lock`] and still leave its launch writer
    /// unserialized, with nothing to notice. There is deliberately no
    /// store-taking constructor on [`RegistryLaunchPersistence`]: every adapter
    /// inherits its registry's lock by construction, which is what
    /// `long-term-store.md` § Cross-process mutation lock means by "no second
    /// writer implementation to forget it".
    pub fn launch_persistence(&self) -> RegistryLaunchPersistence {
        RegistryLaunchPersistence::from_registry(self.clone())
    }

    /// [`Self::launch_persistence`], **bound** to one account for a secondary
    /// instance: every load/save resolves `actor_id`'s slots, the active
    /// pointer is never consulted nor moved (`account-scoping.md` § Concurrent
    /// instances). Gate
    /// the spawn with [`Self::bind_account`] first.
    pub fn bound_launch_persistence(
        &self,
        actor_id: impl Into<String>,
    ) -> RegistryLaunchPersistence {
        RegistryLaunchPersistence::from_registry_bound(self.clone(), actor_id)
    }

    fn check_bindable(&self, actor_id: &str, confirmed: bool) -> Result<(), AccountError> {
        // `index()` is pure; on an unreadable (newer-build) index it reports
        // no accounts, so binding fails closed as UnknownActor — consistent
        // with every mutator refusing outright on that state.
        let idx = self.index();
        let Some(pos) = idx.position(actor_id) else {
            return Err(AccountError::UnknownActor(actor_id.to_string()));
        };
        if self.secrets(actor_id).is_none() {
            return Err(AccountError::NoStoredSecret(actor_id.to_string()));
        }
        if idx.accounts[pos].require_confirm_to_activate && !confirmed {
            return Err(AccountError::ConfirmationRequired(actor_id.to_string()));
        }
        Ok(())
    }

    /// Read back the three per-actor secret slots for `actor_id`.
    /// Returns `None` if the account has no stored secret.
    pub fn secrets(&self, actor_id: &str) -> Option<StoredAccount> {
        let secret_hex = self.store.get(&secret_key(actor_id))?;
        Some(StoredAccount {
            actor_id: actor_id.to_string(),
            secret_hex: secret_hex.into(),
            nest_url: self.store.get(&nest_url_key(actor_id)),
            device_id: self.store.get(&device_id_key(actor_id)),
        })
    }

    /// One-read session material for `actor_id`: [`Self::secrets`] + the
    /// account's index-entry cache. `None` when the account has no resolvable
    /// secret (unknown, or removed out from under a running session) — the
    /// fail-closed answer; a session must never fall back to another
    /// account's material. See [`SessionMaterial`].
    pub fn session_material(&self, actor_id: &str) -> Option<SessionMaterial> {
        let stored = self.secrets(actor_id)?;
        let idx = self.index();
        let entry = idx.accounts.iter().find(|a| a.actor_id == actor_id);
        Some(SessionMaterial {
            actor_id: stored.actor_id,
            secret_hex: stored.secret_hex,
            nest_url: stored.nest_url,
            device_id: stored.device_id,
            handle: entry.and_then(|e| e.handle.clone()),
            domain: entry.and_then(|e| e.domain.clone()),
            tier: entry.and_then(|e| e.tier.clone()),
        })
    }

    /// Whether `actor_id` is currently a known account — the guard every raw
    /// per-actor setter below consults before writing, so a read/response
    /// that lands **after** a factory reset or sign-out
    /// (`Self::remove`/`Self::clear_all`) cannot resurrect the slot the wipe
    /// just erased (`long-term-store.md` § Cleanup contract: "a wiped
    /// identity stays wiped"). Call from inside the mutation lock.
    ///
    /// Funnels through [`Self::writable_index`] — the same "every write path
    /// must go through this" chokepoint every other mutator uses
    /// (`long-term-store.md` § Cleanup contract) — so a raw setter is a write
    /// path like any other.
    ///
    /// An unreadable/malformed index answers `false` here, exactly as it
    /// always did for those states (`Self::index` reports an empty index for
    /// them) — refusing the write, never destructive.
    fn is_live(&self, actor_id: &str) -> bool {
        match self.writable_index() {
            Ok(idx) => idx.position(actor_id).is_some(),
            Err(_) => false,
        }
    }

    /// Set (or overwrite) the per-actor nest-URL slot. Raw slot write
    /// (parallels [`Self::secrets`]) — the launch adapter calls it for the
    /// *active* account after a successful silent challenge
    /// (`LaunchPersistence::save_authenticated`), which re-affirms the home
    /// nest alongside the cache update. Guarded by [`Self::is_live`]: a
    /// reply landing after a factory reset or sign-out must not resurrect
    /// the slot the wipe just erased.
    pub fn set_nest_url(&self, actor_id: &str, nest_url: &str) {
        let _mutation = self.lock.acquire();
        if !self.is_live(actor_id) {
            return;
        }
        self.store.set(&nest_url_key(actor_id), nest_url);
    }

    /// The per-actor pending-invite record, as opaque JSON. The shape is
    /// `fauna_launch_machine::PendingInviteRecord`; the registry stores it
    /// verbatim and never parses it (the adapter / wizard own the serde).
    /// `None` if the account has no outstanding invite. This is the
    /// multi-account home of the single pending-invite slot in
    /// `onboarding.md` § Long-term store contract — now namespaced per actor.
    pub fn pending_invite_json(&self, actor_id: &str) -> Option<String> {
        self.store.get(&pending_invite_key(actor_id))
    }

    /// Write the per-actor pending-invite record (opaque JSON). Raw slot
    /// write, like [`Self::set_nest_url`] (same [`Self::is_live`] guard).
    pub fn set_pending_invite_json(&self, actor_id: &str, json: &str) {
        let _mutation = self.lock.acquire();
        if !self.is_live(actor_id) {
            return;
        }
        self.store.set(&pending_invite_key(actor_id), json);
    }

    /// Clear the per-actor pending-invite slot (no-op if absent).
    pub fn clear_pending_invite(&self, actor_id: &str) {
        let _mutation = self.lock.acquire();
        self.store.delete(&pending_invite_key(actor_id));
    }

    /// The per-actor awaiting-manual-dns record, as opaque JSON. The shape is
    /// `fauna_launch_machine::AwaitingDnsRecord`; the registry stores it
    /// verbatim and never parses it, exactly as for the pending-invite slot.
    /// `None` if the account has no nest mid-provisioning.
    ///
    /// This is the multi-account home of the awaiting-manual-dns slot in
    /// `onboarding.md` § Long-term store contract — namespaced per actor, and
    /// so swept by [`Self::remove`] / [`Self::clear_all`] with the identity it
    /// belongs to (a sibling libsecret namespace would instead *survive* a
    /// factory reset and strand the next launch on a nest whose secret is gone).
    pub fn awaiting_dns_json(&self, actor_id: &str) -> Option<String> {
        self.store.get(&awaiting_dns_key(actor_id))
    }

    /// Write the per-actor awaiting-manual-dns record (opaque JSON). Raw slot
    /// write, like [`Self::set_pending_invite_json`] (same [`Self::is_live`]
    /// guard).
    pub fn set_awaiting_dns_json(&self, actor_id: &str, json: &str) {
        let _mutation = self.lock.acquire();
        if !self.is_live(actor_id) {
            return;
        }
        self.store.set(&awaiting_dns_key(actor_id), json);
    }

    /// Clear the per-actor awaiting-manual-dns slot (no-op if absent). Called at
    /// the wizard's `LoggedIn` terminal — inside `persist_logged_in`, the one
    /// clearing moment (`onboarding.md` § Long-term store contract) — or by the
    /// "Almost ready" surface's explicit exit; the launch path then takes the
    /// ordinary silent-challenge row.
    pub fn clear_awaiting_dns(&self, actor_id: &str) {
        let _mutation = self.lock.acquire();
        self.store.delete(&awaiting_dns_key(actor_id));
    }

    /// Read the per-actor **reach hint** — the freshly-provisioned box's public
    /// IP, or `None` (`onboarding.md` § Reach hint).
    ///
    /// `None` is the ordinary case and must behave exactly as today: the hint is
    /// a pure optimization, absent on every account that did not provision its
    /// own box and on every record written before 2026-08-29.
    pub fn reach_ipv4(&self, actor_id: &str) -> Option<String> {
        self.store.get(&reach_ipv4_key(actor_id))
    }

    /// Write the per-actor reach hint. Captured automatically at the wizard's
    /// `LoggedIn` terminal — a bucket-1 fact, never a knob. Guarded by
    /// [`Self::is_live`], like the slots above.
    pub fn set_reach_ipv4(&self, actor_id: &str, ipv4: &str) {
        let _mutation = self.lock.acquire();
        if !self.is_live(actor_id) {
            return;
        }
        self.store.set(&reach_ipv4_key(actor_id), ipv4);
    }

    /// Drop the reach hint — called on the **first successful domain dial**,
    /// which is the moment the hint has served its purpose
    /// (`onboarding.md` § Reach hint). No-op if absent.
    ///
    /// Deletion is client-side, like the awaiting slot's: nothing nest-side
    /// knows this row exists.
    pub fn clear_reach_ipv4(&self, actor_id: &str) {
        let _mutation = self.lock.acquire();
        self.store.delete(&reach_ipv4_key(actor_id));
    }

    /// Read the per-actor pending-factory-reset record (opaque JSON) — the slot
    /// the client writes *before* dispatching `fauna.admin.factory_reset`, so a
    /// crash between dispatch and reply can't lose the post-reset claim code
    /// (gap CR-1, `common.md` § Client-state recoverability).
    ///
    /// Per-actor like the two slots above, and so swept with the identity it
    /// belongs to: the admin who reset the box is the one who owes the re-claim.
    pub fn pending_factory_reset_json(&self, actor_id: &str) -> Option<String> {
        self.store.get(&pending_factory_reset_key(actor_id))
    }

    /// Write the per-actor pending-factory-reset record (opaque JSON). Raw slot
    /// write, like [`Self::set_awaiting_dns_json`] (same [`Self::is_live`]
    /// guard) — but the caller must treat it as durable-before-dispatch, so
    /// it goes through the store synchronously.
    pub fn set_pending_factory_reset_json(&self, actor_id: &str, json: &str) {
        let _mutation = self.lock.acquire();
        if !self.is_live(actor_id) {
            return;
        }
        self.store.set(&pending_factory_reset_key(actor_id), json);
    }

    /// Clear the per-actor pending-factory-reset slot (no-op if absent). Called
    /// once the re-claim completes — the launch path then takes the ordinary
    /// silent-challenge row against the freshly re-claimed box.
    pub fn clear_pending_factory_reset(&self, actor_id: &str) {
        let _mutation = self.lock.acquire();
        self.store.delete(&pending_factory_reset_key(actor_id));
    }

    /// Read the per-**successor** parked aftermath ceremony (opaque JSON —
    /// `fauna_client_recovery::PendingCeremony`, which this registry stores
    /// verbatim and never parses): what only the ceremony knows and the
    /// successor's post-store-ready pass needs — the raising predecessor, the
    /// group sweep's review roster, the nest's succession stamp
    /// (`succession-aftermath.md` § Re-key scope → *Adjudicating what the
    /// aftermath carries across*, the 2026-09-30 paragraph).
    ///
    /// The registry is its home because it is the one durable per-device store
    /// that exists at the park moment on every host: the ceremony parks
    /// pre-switch, as the predecessor, before the successor's account store
    /// exists. Per-actor, so it is erased with the successor's row.
    pub fn pending_aftermath_ceremony(&self, actor_id: &str) -> Option<String> {
        self.store.get(&pending_aftermath_ceremony_key(actor_id))
    }

    /// Park the successor's aftermath ceremony (opaque JSON). The single
    /// durable decision point of the raise: written by the post-ceremony fold
    /// before the switch, so a crash anywhere after it leaves the raise owed,
    /// never lost (`common.md` § Client-state recoverability). Replaces any
    /// earlier park — the sweep retry re-parks the union it computed. Same
    /// [`Self::is_live`] guard as every per-actor slot write.
    pub fn park_aftermath_ceremony(&self, actor_id: &str, json: &str) {
        let _mutation = self.lock.acquire();
        if !self.is_live(actor_id) {
            return;
        }
        self.store
            .set(&pending_aftermath_ceremony_key(actor_id), json);
    }

    /// Clear the parked aftermath ceremony (no-op if absent) — only once every
    /// put its raises owed has landed.
    pub fn clear_aftermath_ceremony(&self, actor_id: &str) {
        let _mutation = self.lock.acquire();
        self.store.delete(&pending_aftermath_ceremony_key(actor_id));
    }

    /// The per-actor **last-known supervision snapshot**, as opaque JSON. The
    /// shape is `fauna_client_family::SupervisionSnapshot`; the registry stores
    /// it verbatim and never parses it, exactly as for the three slots above.
    /// `None` until this account has completed one successful
    /// `fauna.family.status` read on this device.
    ///
    /// This is clause 2 of `family-safety.md` § Content policy's
    /// **unfetched-policy ruling** — the record that lets a cold start
    /// distinguish *unsupervised* (no snapshot) from *supervised, floor
    /// unknown* (a snapshot carrying the floor), which is what stops airplane
    /// mode from being a bedtime-lock bypass. Class: nest-authoritative
    /// replica, account-scoped placement (`account-scoping.md` § The scoping
    /// taxonomy, classes 4/1).
    ///
    /// Per-actor like every slot above, and so swept by [`Self::remove`] /
    /// [`Self::clear_all`] with the identity it describes — which is the
    /// ruling's "erased with the account's other stores". A sibling namespace
    /// would instead let a *removed* ward's floor outlive them and bind on
    /// whoever next signs in on the box.
    pub fn supervision_snapshot_json(&self, actor_id: &str) -> Option<String> {
        self.store.get(&supervision_snapshot_key(actor_id))
    }

    /// Write the per-actor supervision snapshot (opaque JSON), after a
    /// **successful** status read only — clause 1 forbids moving enforcement
    /// state on a failed read, and this slot is enforcement state.
    ///
    /// No-op once `actor_id` is gone from the index — [`Self::is_live`], the
    /// same guard every raw per-actor setter now shares, and the same
    /// reasoning `save_authenticated` documents at its own call site
    /// (`launch_persistence.rs`,
    /// `save_authenticated_writes_nothing_once_the_account_is_gone`): the
    /// status read that produces this snapshot can land after a factory
    /// reset or sign-out already wiped the account, and an unguarded write
    /// here would resurrect a slot `Self::remove`/`Self::clear_all` just
    /// erased (`long-term-store.md` § Cleanup contract).
    pub fn set_supervision_snapshot_json(&self, actor_id: &str, json: &str) {
        let _mutation = self.lock.acquire();
        if !self.is_live(actor_id) {
            return;
        }
        self.store.set(&supervision_snapshot_key(actor_id), json);
    }

    /// Clear the per-actor supervision snapshot (no-op if absent).
    ///
    /// The ordinary clearing path is *not* this: a successful read reporting no
    /// guardianship overwrites the slot with an unsupervised snapshot, which is
    /// what clause 3's "a graduated ward offline keeps the last-known floor
    /// until the next successful read clears it" describes. This exists for the
    /// identity-change closer (`account-scoping.md` § The scoping taxonomy) and
    /// for tests.
    pub fn clear_supervision_snapshot(&self, actor_id: &str) {
        let _mutation = self.lock.acquire();
        self.store.delete(&supervision_snapshot_key(actor_id));
    }

    /// True when `entry` holds a **provisional** identity: one that moment 1
    /// wrote and that nothing since has touched.
    ///
    /// Deliberately paranoid, because the caller *deletes* what this accepts:
    /// every per-actor slot but the secret must be absent, the index entry's
    /// display cache and both re-auth flags must be untouched, it must sit in
    /// no succession chain, and it must carry no `extra` keys — a row a newer
    /// build wrote fields into is a row this build cannot judge. Anything at
    /// all beyond "a secret and nothing else" fails the test and the row
    /// survives; a false negative merely leaves a ghost row for the user to
    /// remove by hand, while a false positive destroys key material.
    fn is_provisional(&self, entry: &AccountEntry) -> bool {
        let actor_id = &entry.actor_id;
        let pristine_entry = entry.handle.is_none()
            && entry.domain.is_none()
            && entry.tier.is_none()
            && !entry.require_confirm_to_activate
            && !entry.require_confirm_user_set
            && entry.succeeded_by.is_none()
            && entry.extra.is_empty();
        if !pristine_entry {
            return false;
        }
        // Every per-actor slot but the secret, derived from `PER_ACTOR_KEY_BUILDERS`
        // (via `per_actor_keys`) rather than hand-listed, so a *new* builder added
        // there tightens this guard automatically — a hand-list can only ever fall
        // behind and go laxer, never the "stricter, never laxer" a hand-enumeration
        // suggests. `has_any_per_actor_slot` reads every namespace the erase below
        // would sweep (`self.store` + each `aux_erase` store), not `self.store`
        // alone — see its own doc for why that widening matters.
        self.store.get(&secret_key(actor_id)).is_some() && !self.has_any_per_actor_slot(actor_id)
    }

    /// Retire every **other** provisional row when the first-run wizard
    /// commits an identity at moment 1 — the retraction the abandon path
    /// itself cannot perform.
    ///
    /// **Why the cleanup lives here and not on the Back button.** Moment 1
    /// ([`crate::launch_persistence::persist_confirmed_identity`]) writes a
    /// real, activated row the instant the user clicks Continue, and it must:
    /// the alternative is losing a freshly generated secret that exists
    /// nowhere else (`long-term-store.md` § Multi-account evolution, moment
    /// 1). But the ways *away* from a half-onboarded identity are unbounded —
    /// `OnboardingMachine::back`, quitting, a crash, a kill, closing to tray
    /// and never returning — so a cleanup hung on any one of them is a
    /// cleanup the other doors walk straight past. Only the **next commit**
    /// sees them all, whichever door was taken, which is why this is the one
    /// place the retraction is complete.
    ///
    /// **Why confirming a different identity is the retraction signal.**
    /// Append ("Add account") mode never reaches moment 1 (see
    /// `persist_confirmed_identity`'s own contract), so a caller here is a
    /// *first-run* wizard, which has exactly one identity in flight. A
    /// second secret-only row therefore cannot be anything but an earlier run
    /// of this same wizard that the user has just walked away from — and it
    /// is dead by construction: it has authenticated to no nest, so it owns
    /// no data anywhere, and no path exists to reach it again once a
    /// different identity is active. Retiring it restores the no-ghost-row
    /// property `long-term-store.md` § Eager vs. lazy migration at native
    /// boot still requires, while keeping every hour of the moment-1
    /// durability guarantee the user is actually inside the wizard for.
    ///
    /// Returns the actor ids retired (for the caller's logs and for tests).
    pub fn retire_superseded_provisionals(&self, keep_actor_id: &str) -> Vec<String> {
        let _mutation = self.lock.acquire();
        let Ok(mut idx) = self.writable_index() else {
            // An unreadable index hides every other account (`Self::index`), so
            // there is nothing safe to judge — and a write would be refused
            // anyway. Leave it entirely alone.
            return Vec::new();
        };
        // A row named as somebody's predecessor is never provisional, even if
        // its own `succeeded_by` is clear — `record_succession` writes the link
        // on the *predecessor*, but a half-written chain must not be finished
        // off by a delete.
        let in_a_chain: std::collections::BTreeSet<&str> = idx
            .accounts
            .iter()
            .filter_map(|a| a.succeeded_by.as_deref())
            .collect();
        let doomed: Vec<String> = idx
            .accounts
            .iter()
            .filter(|a| a.actor_id != keep_actor_id)
            .filter(|a| !in_a_chain.contains(a.actor_id.as_str()))
            .filter(|a| self.is_provisional(a))
            .map(|a| a.actor_id.clone())
            .collect();
        if doomed.is_empty() {
            return doomed;
        }
        idx.accounts.retain(|a| !doomed.contains(&a.actor_id));
        // The caller activated `keep_actor_id` before calling us, so the active
        // pointer should already name a survivor; re-seat it defensively for
        // the case where that activation was refused (a flagged account — the
        // `let _` in `persist_confirmed_identity`).
        if idx
            .active
            .as_deref()
            .is_none_or(|a| doomed.iter().any(|d| d == a))
        {
            idx.active = idx.accounts.first().map(|a| a.actor_id.clone());
        }
        // Encoded before the deletes, stored after them. Retiring rows only
        // shrinks the index, so the bound cannot refuse it.
        let Ok(raw) = self.encode_index(&idx) else {
            return Vec::new();
        };
        for actor_id in &doomed {
            // The same helper `Self::remove` uses, so the erase and the
            // emptiness test above can never drift apart, and a new slot
            // added to `PER_ACTOR_KEY_BUILDERS` is covered here too without a
            // third place to remember it. `is_provisional` already proved
            // every slot but the secret absent, so the extra deletes below
            // are no-ops today — this removes the possibility of an orphan
            // by construction rather than by keeping two lists in sync.
            self.delete_per_actor_everywhere(actor_id);
        }
        self.store.set(INDEX_KEY, &raw);
        doomed
    }

    /// Remove an account: delete its per-actor slots and drop it from the
    /// index. If it was active, the first remaining account (if any)
    /// becomes active. Errors if unknown.
    pub fn remove(&self, actor_id: &str) -> Result<(), AccountError> {
        let _mutation = self.lock.acquire();
        let mut idx = self.writable_index()?;
        let pos = idx
            .position(actor_id)
            .ok_or_else(|| AccountError::UnknownActor(actor_id.to_string()))?;
        let was_active = idx.active.as_deref() == Some(actor_id);
        idx.accounts.remove(pos);
        if was_active {
            idx.active = idx.accounts.first().map(|a| a.actor_id.clone());
        }
        // Encoded before the deletes, stored after them; a removal only
        // shrinks the index, so the bound never refuses it.
        let raw = self.encode_index(&idx)?;
        self.delete_per_actor_everywhere(actor_id);
        self.store.set(INDEX_KEY, &raw);
        Ok(())
    }

    /// Walk away from `actor_id`'s nest, keeping the identity: delete the
    /// per-actor (nest_url, device_id) slots so the next launch of this
    /// account routes into nest selection with its secret intact — the
    /// registry-routed form of the "use a different nest" fallthrough. Errors
    /// if unknown. The index entry's cached handle/domain/tier are left as-is
    /// (display cache; the next `save_authenticated` rewrites them).
    pub fn clear_nest_binding(&self, actor_id: &str) -> Result<(), AccountError> {
        let _mutation = self.lock.acquire();
        let idx = self.index();
        if idx.position(actor_id).is_none() {
            return Err(AccountError::UnknownActor(actor_id.to_string()));
        }
        self.store.delete(&nest_url_key(actor_id));
        self.store.delete(&device_id_key(actor_id));
        // The reach hint goes with the binding it is an optimization *for*.
        // Surviving here would leave an address for the box this account just
        // stopped using, to be dialled as a fallback against whatever nest the
        // user picks next (`onboarding.md` § Reach hint).
        self.store.delete(&reach_ipv4_key(actor_id));
        Ok(())
    }

    /// Sign-out cleanup: delete **every** credential this registry owns — each
    /// account's per-actor slots and the index blob.
    /// Afterwards the store holds no identity, so the next launch routes to
    /// case 3 (fresh onboarding) — `long-term-store.md` § Cleanup contract.
    ///
    /// Linux gets this for free by wiping its whole libsecret `application`
    /// namespace (`delete_credentials_in`). Platforms whose store has no
    /// namespace sweep (web's `localStorage`) consume this instead of
    /// hand-rolling the key layout.
    ///
    /// Reads the index blob directly rather than through [`Self::index`]. (Before
    /// reads were made pure, this raw read was also what kept `clear_all` from
    /// re-creating the very slots it deletes — that hazard is now structural;
    /// see the type-level invariant.) A corrupt/unparseable index leaves
    /// per-actor slots unreachable (the [`SecretStore`] seam cannot scan), but
    /// the index — the one thing launch routing reads — is still removed.
    ///
    /// **Returns what survived, by reading every deleted key back.**
    /// [`SecretStore::delete`] reports nothing — each arm logs or swallows its
    /// own failure — so until 2026-09-13 a sign-out whose delete a locked or
    /// failing store refused painted a clean "Signed out" over a device still
    /// holding the identity seed. A key that reads back after its delete
    /// survived it, whatever the store's reason, so the erase now asks
    /// ([`CredentialSweep`]; `account-scoping.md` § Erasure follows scope). The
    /// cost is one read per deleted key, paid only at sign-out. A caller that
    /// wipes further afterwards (a whole-namespace sweep, a platform store's
    /// own reset) re-asks with [`Self::reverify`], which reads only the
    /// survivors again.
    pub fn clear_all(&self) -> CredentialSweep {
        let _mutation = self.lock.acquire();
        // Enumerate before deleting anything — nothing below can be recovered
        // once the two pointers are gone.
        let idx = self
            .store
            .get(INDEX_KEY)
            .and_then(|raw| serde_json::from_str::<AccountIndex>(&raw).ok())
            .unwrap_or_default();
        let doomed: Vec<String> = idx.accounts.iter().map(|e| e.actor_id.clone()).collect();

        // The routing pointer dies first, so a crash part-way through lands
        // signed OUT rather than signed back in: launch routes on the index
        // (`RegistryLaunchPersistence`), and erasing the per-actor secrets
        // first would leave the pointer valid.
        self.store.delete(INDEX_KEY);

        for actor_id in &doomed {
            self.delete_per_actor_everywhere(actor_id);
        }

        // Read back exactly the set deleted above, in exactly the namespaces it
        // was deleted from: the index lives only in `self.store`; per-actor
        // slots in it and every auxiliary namespace.
        let mut survivors = credential_sweep::still_readable(
            self.store.as_ref(),
            &[],
            std::iter::once(INDEX_KEY.to_string()),
        );
        for key in credential_sweep::still_readable(
            self.store.as_ref(),
            &self.aux_erase,
            doomed.iter().flat_map(|actor| per_actor_keys(actor)),
        ) {
            if !survivors.contains(&key) {
                survivors.push(key);
            }
        }
        CredentialSweep {
            survivors,
            wipe_failed: false,
        }
    }

    /// Re-read what a [`CredentialSweep`] reported surviving, after the caller
    /// wiped further — `CredentialStore::delete_namespace` on the freedesktop
    /// apps, the platform store's own reset on android. A key the later wipe
    /// removed drops out; one it did not stays; a recorded wipe failure is kept,
    /// since a read-back cannot see what a store that refuses reads is holding.
    ///
    /// Reads only the survivors, not the whole deleted set: nothing the erase
    /// already verified gone can come back while the caller's writers are down
    /// (`long-term-store.md` § Cleanup contract — quiesce first, erase last),
    /// and on a keyring every read is a round trip.
    pub fn reverify(&self, sweep: CredentialSweep) -> CredentialSweep {
        CredentialSweep {
            survivors: credential_sweep::still_readable(
                self.store.as_ref(),
                &self.aux_erase,
                sweep.survivors,
            ),
            wipe_failed: sweep.wipe_failed,
        }
    }

    /// Persist the predecessor identities a phrase-only restore recovered from
    /// the escrow blob's additive section (`identity-succession.md` § Seed
    /// escrow), each as `(seed_hex, actor_id_hex)` — the restore's half of the
    /// device-loss race: the successor's kit sealed these seeds precisely so a
    /// user who lost every device gets them back, and dropping them here would
    /// make that seal pointless. They land as ordinary rows plus the succession
    /// link to the restored identity, exactly the state the ceremony's own
    /// device is left in. One body for every app's wizard handoff.
    ///
    /// `restored_actor` is the identity these are predecessors *of* — named
    /// explicitly, never read off `active()`: an add-account restore persists
    /// before the switch that makes it active. `None` still lands every seed
    /// (they are the only copies left anywhere), just without the link.
    ///
    /// **Ordering:** call *after* the restored identity is added —
    /// `add_account` claims `active` when no account holds it, so persisting a
    /// predecessor first would land the app on an identity the nest refuses.
    ///
    /// Best-effort per row: a failure is logged and the rest still land. The
    /// account is already back, so a predecessor's bad row must not turn into a
    /// failed sign-in.
    pub fn persist_restored_predecessors<'a>(
        &self,
        restored_actor: Option<&str>,
        predecessors: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) {
        let restored = restored_actor;
        for (seed_hex, named_actor) in predecessors {
            match self.add_account(seed_hex, None, None) {
                Ok(actor) if actor == named_actor => {
                    // The durable link that lets a freshly-restored device run
                    // the corpus re-seal at all (`succession-aftermath.md`
                    // § Re-key scope).
                    if let Some(restored) = restored
                        && let Err(e) = self.record_succession(&actor, restored)
                    {
                        tracing::warn!(
                            predecessor = %actor,
                            successor = %restored,
                            error = %e,
                            "recording a restored predecessor's succession link failed"
                        );
                    }
                    tracing::info!(
                        actor = %actor,
                        "restored a predecessor identity's seed from the escrow blob"
                    );
                }
                // The seed did not derive to the actor the entry named; the row
                // landed under whichever actor it does derive to — say so
                // rather than logging success.
                Ok(actor) => tracing::warn!(
                    named = %named_actor,
                    derived = %actor,
                    "a predecessor entry's seed does not derive to the actor it names"
                ),
                Err(e) => tracing::warn!(
                    actor = %named_actor,
                    error = %e,
                    "persisting a restored predecessor identity failed"
                ),
            }
        }
    }

    /// Set the per-account "require confirmation to activate" flag.
    /// Record that `old_actor` was succeeded by `new_actor` — the durable half
    /// of an identity succession on *this device*
    /// (`succession-aftermath.md` § Re-key scope).
    ///
    /// Called wherever a client learns the link: at the ceremony's own
    /// `adopt_successor`, and at the phrase-only restore that writes recovered
    /// predecessors back as ordinary rows. Its consumer is
    /// [`Self::predecessors_of`], which the post-succession corpus re-seal
    /// needs in order to know which retired identity's `BackupKey` opens the
    /// blobs it now owns.
    ///
    /// `succeeded_by` records the **immediate** successor and is never
    /// rewritten afterwards: "B succeeded A" is a fact about one ceremony, and
    /// a later A→B→C hop does not change it. Walking the chain is
    /// [`Self::predecessors_of`]'s job, which is free over an in-memory account
    /// list — the nest re-points its rows to the *terminal* successor instead
    /// only because a SQL owner lookup has no chain to walk. Idempotent:
    /// re-running a ceremony's persist rewrites the same value.
    ///
    /// A predecessor row absent from the registry is not refused — it can
    /// legitimately be gone (a factory reset, a user who removed the old
    /// account, a successor signing in on a device that never held it), and a
    /// succession that already landed on the nest must not fail here over
    /// bookkeeping. The link is still recorded, on the successor's own row
    /// (`AccountEntry::succeeded_from`), which is also what keeps it alive when
    /// the predecessor's row is removed later. Only an unknown *successor* is
    /// an error, since that is a caller bug.
    ///
    /// **This is also where a bound instance's launch binding follows the
    /// account** (`account-scoping.md` § Concurrent instances → *The binding
    /// follows the account*): a process bound to `old_actor` is bound to the
    /// account that just moved, and every adopter of a successor crosses this
    /// seam *before* it switches — so the rule lives here once, rather than in
    /// each app's post-ceremony fold where any one of them could forget it.
    /// Independent of the row check above on purpose: the predecessor's row
    /// may legitimately be gone while the binding still names it. Native
    /// only — a wasm process has no launch binding.
    pub fn record_succession(&self, old_actor: &str, new_actor: &str) -> Result<(), AccountError> {
        if old_actor == new_actor {
            return Ok(()); // a self-link is not a chain; nothing to record
        }
        let _mutation = self.lock.acquire();
        let mut idx = self.writable_index()?;
        if idx.position(new_actor).is_none() {
            return Err(AccountError::UnknownActor(new_actor.to_string()));
        }
        if let Some(pos) = idx.position(old_actor) {
            idx.accounts[pos].succeeded_by = Some(new_actor.to_string());
        }
        // The successor's own row carries the chain too, so the link outlives
        // the predecessor's row — and exists at all where that row never did
        // (`AccountEntry::succeeded_from`). The whole chain, not one hop: a
        // later `remove` of a middle row must not orphan what lies behind it.
        let mut chain = vec![old_actor.to_string()];
        chain.extend(predecessors_in(&idx.accounts, old_actor));
        merge_succeeded_from(&mut idx, new_actor, &chain);
        let written = self.write_index(&idx);
        // The launch binding follows the account even when the link itself is
        // refused for room in the index.
        #[cfg(not(target_arch = "wasm32"))]
        instance_lock::rebind_session_launch_after_succession(old_actor, new_actor);
        written
    }

    /// A launch refused as superseded, whose **chain-verified** successor this
    /// device already holds the key to: say whether to adopt it, and record the
    /// succession link if so. `true` means the caller switches to
    /// `verified_successor` now (its own switch — this sets nothing active).
    ///
    /// The state it resolves is the one a lost succession reply leaves behind
    /// (`identity-succession.md` § Implementation status today, *a lost submit
    /// reply no longer destroys the account*): the ceremony persists the
    /// successor before anything can fail, so when its outcome could not be
    /// confirmed the successor's seed sits in this registry with the old
    /// identity still active, and the undecidable arm promises that reopening
    /// the app signs in as it. Adopting is the import the user would perform by
    /// hand, minus a secret they were never shown.
    ///
    /// **`verified_successor` must be the registration chain's answer, never
    /// the nest's claim** — that is the caller's half of the proof, and the one
    /// this function cannot check. The other half is checked here: the key must
    /// be one this device holds, *with* the nest it signs in to (a held key with
    /// no nest would switch into onboarding, so the import screen stays the
    /// answer). The link is what the ceremony's lost fold never recorded, and
    /// the corpus re-seal needs it at every later sign-in; recording it is
    /// best-effort, like every other writer of it — a succession that already
    /// landed must not be refused over bookkeeping.
    ///
    /// ⚠ The kit and the group sweep the adoption also owes are the app's to
    /// carry across its switch — they live beside its ceremony's own owed
    /// flags, which a registry has no business holding
    /// (`succession-propagation.md` § Propagation → *Own device fleet*).
    pub fn adopt_held_successor(&self, predecessor: &str, verified_successor: &str) -> bool {
        let held_with_nest = self
            .secrets(verified_successor)
            .is_some_and(|held| held.nest_url.is_some());
        if !held_with_nest {
            return false;
        }
        tracing::info!(
            predecessor = %predecessor,
            successor = %verified_successor,
            "this device holds the verified successor; adopting it"
        );
        if let Err(e) = self.record_succession(predecessor, verified_successor) {
            tracing::warn!(
                predecessor = %predecessor,
                successor = %verified_successor,
                error = %e,
                "recording the adopted successor's link failed — the corpus \
                 re-seal will not find this predecessor automatically"
            );
        }
        true
    }

    /// Every retired identity in this registry that `actor_id` succeeded from,
    /// nearest hop first — the input the post-succession corpus re-seal opens
    /// at-rest blobs with (`succession-aftermath.md` § Re-key scope).
    ///
    /// **Every ancestor, not just the immediate one.** A corpus can still be
    /// sealed under a *grand*predecessor if an intermediate re-seal never
    /// completed (two successions in quick order, or an interrupted pass), and
    /// a one-hop answer would leave those bytes stranded — the pass would
    /// report "no key opens it" forever with the material sitting in the
    /// registry all along.
    ///
    /// Rows whose secret this device does not hold are still listed; the caller
    /// resolves material per row and treats a missing one as an honest "no
    /// material here", which is the same rule the owed-kit's predecessor lookup
    /// follows. The same goes for an identity with **no row at all** — one the
    /// successor's own `AccountEntry::succeeded_from` remembers after the
    /// retired account was removed, or that was proven from the landed
    /// statement on a device that never held it
    /// ([`Self::record_predecessors`]).
    pub fn predecessors_of(&self, actor_id: &str) -> Vec<String> {
        predecessors_in(&self.index().accounts, actor_id)
    }

    /// Record predecessors of `new_actor` that were **proven rather than
    /// witnessed** — a link learned from the landed succession statement's own
    /// `new_sig` (`fauna_client_profile::learn_inherited_predecessors`), on a
    /// device whose registry never held the predecessor's row or has since
    /// lost it. `predecessors` is nearest hop first.
    ///
    /// Merges into `AccountEntry::succeeded_from`: existing order is kept, new
    /// ids append, a self-link is dropped, and a repeat writes nothing. No
    /// predecessor row is created — this device holds no material for them, and
    /// an id-only row would be a switcher entry for an account nobody can open.
    /// An unknown `new_actor` is a caller bug.
    ///
    /// ⚠ The caller vouches for the list. Feed it only what a signature this
    /// device can check established — never a nest's say-so: every predecessor
    /// consumer ([`Self::predecessors_of`]) trusts it, the profile writers'
    /// admission rule first among them.
    pub fn record_predecessors(
        &self,
        new_actor: &str,
        predecessors: &[String],
    ) -> Result<(), AccountError> {
        let _mutation = self.lock.acquire();
        let mut idx = self.writable_index()?;
        if idx.position(new_actor).is_none() {
            return Err(AccountError::UnknownActor(new_actor.to_string()));
        }
        if merge_succeeded_from(&mut idx, new_actor, predecessors) {
            self.write_index(&idx)?;
        }
        Ok(())
    }

    /// The identity at the END of `actor_id`'s succession chain — the account
    /// as it is today — or `None` when `actor_id` was never succeeded (or is
    /// unknown here). The forward walk [`Self::predecessors_of`] is the reverse
    /// of: `succeeded_by` records each hop's immediate successor, so a
    /// twice-succeeded chain A→B→C answers `C` for `A` and for `B`.
    ///
    /// Bounded by the account count so a corrupted cyclic index cannot spin,
    /// same as the backward walk.
    pub fn terminal_successor_of(&self, actor_id: &str) -> Option<String> {
        let accounts = self.index().accounts;
        let mut current = actor_id.to_string();
        let mut moved = false;
        for _ in 0..accounts.len() {
            let next = accounts
                .iter()
                .find(|e| e.actor_id == current)
                .and_then(|e| e.succeeded_by.clone());
            match next {
                Some(next) if next != current => {
                    current = next;
                    moved = true;
                }
                _ => break,
            }
        }
        moved.then_some(current)
    }

    /// Resolve this process's **launch binding** through the succession chain
    /// — **a bound launch whose named id has a recorded successor binds to the
    /// terminal successor** (`account-scoping.md` § Concurrent instances → *The
    /// binding follows the account*, rider 2, ratified 2026-08-27). Returns the
    /// binding as it stands afterwards: `None` for a plain launch, the named id
    /// when it was never succeeded, else the successor — and re-points the
    /// process cell to it, so every later read (`session_launch_binding`, the
    /// holder's bound-or-refuse) sees the account the binding names *today*.
    ///
    /// A binding names an **account** by the id that identified it when the
    /// spawner minted it. The same rule that moves the binding when *this*
    /// process records a succession ([`Self::record_succession`]) has to hold
    /// for a binding minted before this process could observe one — a sibling
    /// seat ran the ceremony, a phrase-only restore persisted the successor's
    /// row — and the registry is the only place that knows the chain, which is
    /// why this is a registry method rather than a holder concern (the holder
    /// stays registry-free: it runs before any scoped state opens). Call it
    /// where a registry is first in hand, before the session account resolves;
    /// idempotent, and free for every never-succeeded binding.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn resolve_launch_binding(&self) -> Option<String> {
        let bound = instance_lock::session_launch_binding()?;
        match self.terminal_successor_of(&bound) {
            Some(successor) => {
                instance_lock::rebind_session_launch_after_succession(&bound, &successor);
                Some(successor)
            }
            None => Some(bound),
        }
    }

    /// The retired owner `BackupKey`s `actor_id`'s corpus may still be sealed
    /// under — [`Self::predecessors_of`] resolved to key material, nearest hop
    /// first. Empty for an identity that never succeeded, which is what keeps
    /// every consumer free for the overwhelmingly common case.
    ///
    /// This is the **one** resolution of predecessor rows to keys, shared rather
    /// than re-derived per plane: the `__mls` state-replica
    /// re-seal and the chunk-shaped corpus read
    /// ([`fauna_core::file_download::FileDownloadKeys::predecessor_backup_keys`])
    /// each need exactly this list, and the seven apps must not each grow their
    /// own copy of a filter whose failure mode is silent (a dropped row reads as
    /// "no key opens it" forever while the material sits in the registry).
    ///
    /// **A row whose secret this device does not hold is skipped, never an
    /// error** — that is the ordinary state on a device the user did not succeed
    /// from, and it is honest: this device genuinely cannot open those bytes, and
    /// another one is owed the pass. Same rule for material that fails to parse,
    /// which is logged by the caller rather than swallowed here, since this
    /// method has no context to report against.
    ///
    /// Returns keys, never seeds: the secret is derived and dropped inside, so a
    /// consumer that only needs to *open* bytes never gets a copy of the identity
    /// seed to mislay.
    pub fn predecessor_backup_keys(&self, actor_id: &str) -> Vec<fauna_core::crypto::BackupKey> {
        self.predecessor_material(actor_id)
            .map(|(_hex, keypair)| fauna_core::crypto::BackupKey::derive(keypair.secret_bytes()))
            .collect()
    }

    /// The material walk both predecessor accessors above resolve through:
    /// [`Self::predecessors_of`] rows this device holds parseable secrets for,
    /// nearest hop first, as `(actor id hex, keypair)`.
    ///
    /// Private on purpose — it yields keypairs, and the two public accessors
    /// exist precisely so a consumer that only needs to *open* bytes never
    /// receives one. It is the shared half; what each accessor does with a row
    /// is its own contract, which is why this hands back an iterator rather
    /// than either of their two shapes.
    fn predecessor_material(
        &self,
        actor_id: &str,
    ) -> impl Iterator<Item = (String, fauna_core::identity::ActorKeypair)> + '_ {
        self.predecessors_of(actor_id)
            .into_iter()
            .filter_map(|hex| {
                let stored = self.secrets(&hex)?;
                let keypair =
                    fauna_core::identity::ActorKeypair::from_secret_hex(&stored.secret_hex).ok()?;
                Some((hex, keypair))
            })
    }

    /// [`Self::predecessor_backup_keys`] with each key still **paired to the
    /// actor it opens** — the input shape the post-succession aftermath's
    /// predecessor lists are built from.
    ///
    /// **Why this exists rather than each driver walking the registry itself.**
    /// A re-key driver needs the pair, not the bare key: it opens each
    /// predecessor's blob *by actor id*. Neither of the two accessors above
    /// hands one over — [`Self::predecessor_backup_keys`] drops the id and
    /// [`Self::predecessor_seeds`] is declared for escrow-container producers
    /// only ("every consumer that only needs to *open* bytes takes
    /// `predecessor_backup_keys` instead, which deliberately never hands out
    /// seeds"). So the aftermath's two drivers each grew their own walk — tui
    /// over `predecessors_of` + `secrets`, web over `predecessor_seeds` +
    /// `backup_key_from_seed` — and web's route handed a seed to a consumer
    /// that only ever needed to open bytes. The aftermath's inputs
    /// ask for "the shared registry walk, never a hand-rolled filter"; this is
    /// that walk.
    ///
    /// Skip rules, ordering and the never-an-error contract are
    /// [`Self::predecessor_backup_keys`]' — it is now expressed in terms of
    /// this, so the two can never drift apart.
    pub fn predecessor_backup_keys_by_actor(
        &self,
        actor_id: &str,
    ) -> Vec<(fauna_core::identity::ActorId, fauna_core::crypto::BackupKey)> {
        self.predecessor_material(actor_id)
            .filter_map(|(hex, keypair)| {
                // The one skip this accessor adds over its key-only sibling: a
                // row whose id does not decode cannot be *named*, and the pair
                // is the whole point here. Deliberately NOT pushed down into
                // the shared walk — narrowing `predecessor_backup_keys` by an
                // id parse it never needed would drop a key that opens real
                // bytes, which is the silent failure that accessor's own doc
                // exists to prevent.
                let id = fauna_core::hex32::decode(&hex).ok()?;
                Some((
                    fauna_core::identity::ActorId(id),
                    fauna_core::crypto::BackupKey::derive(keypair.secret_bytes()),
                ))
            })
            .collect()
    }

    /// The **attested** predecessor set — the actor ids of
    /// [`Self::predecessor_backup_keys_by_actor`], nearest hop first: exactly
    /// the succeeded-from identities whose seeds this device holds, and
    /// therefore the generation machinery's fleet-view `prior`
    /// (`account-data-taxonomy.md` § The generation machinery → *The source of
    /// `prior`*, ruled 2026-09-13).
    ///
    /// Attestation is possession: a registry row exists only because this
    /// device ran the ceremony or restored the seed out of the successor's own
    /// escrow container, and a seed derives its id — so this list cannot name
    /// an identity the owner never had, where a
    /// writer-asserted `prior_actor_ids` list can. A row whose secret this device
    /// does not hold is skipped, never an error, for the sibling accessor's
    /// reason: that device genuinely attests nothing about it.
    ///
    /// Expressed over the paired walk rather than `predecessors_of` so the
    /// three lists — keys, pairs, ids — can never disagree about which rows
    /// count.
    pub fn attested_predecessor_actor_ids(
        &self,
        actor_id: &str,
    ) -> Vec<fauna_core::identity::ActorId> {
        self.predecessor_backup_keys_by_actor(actor_id)
            .into_iter()
            .map(|(id, _key)| id)
            .collect()
    }

    /// [`Self::predecessors_of`] resolved to the raw identity **seeds**, nearest
    /// hop first — `(actor id hex, seed bytes)` per ancestor this device holds
    /// parseable material for.
    ///
    /// Two consumer classes, both needing the *seed* because what they need
    /// derives from it and from no derived key: **escrow-container
    /// producers** (the recovery kit's seed-escrow blob —
    /// `identity-succession.md` § Seed escrow), whose restore must recover the
    /// seed itself; and the **seed-holding runtime's escrow recovery**
    /// (`fauna_account_plane::account_driver::SeedHolder::from_registry`),
    /// which opens a succession's kept wrap under the retired identity's
    /// escrow secret (`owner-key-material.md` § Path A-sibling-2 →
    /// *Rotation*, the succession rider → *The kept wrap*). Every consumer
    /// that only needs to *open* account-state bytes takes
    /// [`Self::predecessor_backup_keys`] instead, which deliberately never
    /// hands out seeds.
    ///
    /// The **full chain** is resolved, never one hop: the previous kit's blob
    /// dies in the succession transaction (§ Seed escrow — "the row dies with
    /// the key it is sealed to"), so nothing else carries a grandparent's seed
    /// forward into the new blob. Skip rules are identical to
    /// [`Self::predecessor_backup_keys`]: a row whose secret this device does
    /// not hold, or whose material fails to parse, is skipped — never an
    /// error.
    pub fn predecessor_seeds(&self, actor_id: &str) -> Vec<(String, [u8; 32])> {
        self.predecessor_material(actor_id)
            .map(|(hex, keypair)| (hex, *keypair.secret_bytes()))
            .collect()
    }

    pub fn set_require_confirm(&self, actor_id: &str, require: bool) -> Result<(), AccountError> {
        let _mutation = self.lock.acquire();
        let mut idx = self.writable_index()?;
        let pos = idx
            .position(actor_id)
            .ok_or_else(|| AccountError::UnknownActor(actor_id.to_string()))?;
        idx.accounts[pos].require_confirm_to_activate = require;
        // The toggle is a human choice — from here on the admin auto-default
        // (`auto_enable_require_confirm`) must never override it.
        idx.accounts[pos].require_confirm_user_set = true;
        self.write_index(&idx)?;
        Ok(())
    }

    /// The admin auto-default (`long-term-store.md` § Multi-account evolution:
    /// *"Default off; a client turns it on for its admin identity"*): flip
    /// `require_confirm_to_activate` ON **iff the user has never touched the
    /// toggle** for this account. Idempotent — a client calls it on every
    /// `am-i-admin = true` observation (the registry never learns admin-ness
    /// itself; the *client* decides when this account counts as admin). Never
    /// turns the flag off, and does not consume the user's override right
    /// (`require_confirm_user_set` stays false). Returns whether this call
    /// flipped the flag.
    pub fn auto_enable_require_confirm(&self, actor_id: &str) -> Result<bool, AccountError> {
        let _mutation = self.lock.acquire();
        let mut idx = self.writable_index()?;
        let pos = idx
            .position(actor_id)
            .ok_or_else(|| AccountError::UnknownActor(actor_id.to_string()))?;
        let entry = &mut idx.accounts[pos];
        if entry.require_confirm_user_set || entry.require_confirm_to_activate {
            return Ok(false);
        }
        entry.require_confirm_to_activate = true;
        self.write_index(&idx)?;
        Ok(true)
    }

    /// Update an account's server-data cache (handle/domain/tier).
    pub fn update_cache(
        &self,
        actor_id: &str,
        handle: Option<&str>,
        domain: Option<&str>,
        tier: Option<&str>,
    ) -> Result<(), AccountError> {
        let _mutation = self.lock.acquire();
        let mut idx = self.writable_index()?;
        let pos = idx
            .position(actor_id)
            .ok_or_else(|| AccountError::UnknownActor(actor_id.to_string()))?;
        let entry = &mut idx.accounts[pos];
        entry.handle = handle.map(str::to_string);
        entry.domain = domain.map(str::to_string);
        entry.tier = tier.map(str::to_string);
        self.write_index(&idx)?;
        Ok(())
    }

    // --- internals ---

    /// The index is present and this build cannot use it — the launch seam's
    /// view of [`IndexState`], in the shape the seven apps render
    /// (`version-compatibility.md` § 5 item 9; `onboarding.md` § App-launch
    /// routing's "present but unreadable" row).
    ///
    /// The two verdicts are kept apart all the way to the surface because their
    /// remedies are opposites: updating the app is the whole answer to one and
    /// no answer at all to the other. `Absent` and `Readable` both answer
    /// `None` — a fresh install is not a refusal.
    ///
    /// `pub` (not `pub(crate)`) so `fauna-ffi`/`fauna-wasm` can expose it as an
    /// additive query beside the mutators that flatten `AccountError` to an
    /// opaque string — a mutation failure can then ask which verdict is active
    /// without a new variant on `FfiError`/`JsError`.
    pub fn index_refusal(&self) -> Option<AccountIndexRefusal> {
        match self.index_state() {
            IndexState::Absent | IndexState::Readable(_) => None,
            IndexState::Unreadable { index_v, index_min } => {
                Some(AccountIndexRefusal::NewerBuild {
                    index_v,
                    index_min,
                    bin_v: CURRENT_INDEX_VERSION,
                })
            }
            IndexState::Malformed => Some(AccountIndexRefusal::Malformed),
        }
    }

    /// Persist the index, stamped — **never restamping down.**
    ///
    /// This build can legitimately rewrite an index a *newer* build wrote, as
    /// long as that build did not raise the reader floor past us (a
    /// newer-but-additive index: `schema_version` 5, `min_reader_version` 1).
    /// Because [`AccountIndex::extra`] round-trips the fields we don't know, the
    /// blob we write back is still that newer shape — so it must keep the newer
    /// numbers. Stamping our own lower `CURRENT_INDEX_VERSION` over them would
    /// tell the newer build its index had been downgraded when it had not
    /// (`version-compatibility.md` § 2.2 — "do not restamp down").
    fn write_index(&self, idx: &AccountIndex) -> Result<(), AccountError> {
        let raw = self.encode_index(idx)?;
        self.store.set(INDEX_KEY, &raw);
        Ok(())
    }

    /// The stamped blob [`Self::write_index`] would store — or
    /// [`AccountError::IndexFull`] when it would not fit one credential item
    /// (`long-term-store.md` § Multi-account evolution → *The index is
    /// bounded*). Writes nothing, so a mutator that writes other slots too
    /// encodes FIRST and refuses before any of them land.
    ///
    /// Only GROWTH past the cap is refused: a blob over
    /// [`MAX_INDEX_VALUE_BYTES`] that is no longer than the stored one still
    /// encodes. A rewrite that does not grow the index must always land — a
    /// switch, a removal, or an index a build predating the bound wrote past
    /// the cap on a roomier store, which may shrink but never grow.
    fn encode_index(&self, idx: &AccountIndex) -> Result<String, AccountError> {
        let stamped = AccountIndex {
            schema_version: idx.schema_version.max(CURRENT_INDEX_VERSION),
            min_reader_version: idx.min_reader_version.max(MIN_READER_INDEX_VERSION),
            ..idx.clone()
        };
        let raw = serde_json::to_string(&stamped).expect("AccountIndex serializes");
        if raw.len() > MAX_INDEX_VALUE_BYTES {
            let stored = self.store.get(INDEX_KEY).map_or(0, |s| s.len());
            if raw.len() > stored {
                return Err(AccountError::IndexFull {
                    needed: raw.len(),
                    limit: MAX_INDEX_VALUE_BYTES,
                });
            }
        }
        Ok(raw)
    }

    /// Classify what is on disk (`version-compatibility.md` § 2.2 verdict, in
    /// the four states this store actually has). Pure.
    ///
    /// **Failing to parse is not by itself evidence of corruption.** A newer
    /// build that retypes a field non-additively writes an index this build
    /// cannot decode *and* that is perfectly intact — § 5 item 9 names that
    /// exact cliff — so a failed parse is retried as a stamp-only
    /// [`AccountIndexStamp`] peek before any verdict is reached. A stamp above
    /// our floor is the **version** case with its real numbers; anything else
    /// is [`IndexState::Malformed`], where claiming a version problem would be
    /// a lie. Either way the blob is not ours to rewrite.
    fn index_state(&self) -> IndexState {
        let Some(raw) = self.store.get(INDEX_KEY) else {
            return IndexState::Absent;
        };
        match serde_json::from_str::<AccountIndex>(&raw) {
            Ok(idx) if idx.min_reader_version <= CURRENT_INDEX_VERSION => {
                IndexState::Readable(Box::new(idx))
            }
            Ok(idx) => IndexState::Unreadable {
                index_v: idx.schema_version,
                index_min: idx.min_reader_version,
            },
            // The whole index did not parse. Peek for the stamp before
            // concluding anything: serde ignores keys it has no field for, so
            // this still reads the numbers out of an index a newer build made
            // undecodable here. Only a stamp genuinely above our floor earns
            // the version refusal — a baseline peek is what a stamp-less
            // blob yields, so treating it as "newer build" would resurrect the
            // invented-numbers message this split exists to remove.
            Err(_) => match serde_json::from_str::<AccountIndexStamp>(&raw) {
                Ok(stamp) if stamp.min_reader_version > CURRENT_INDEX_VERSION => {
                    IndexState::Unreadable {
                        index_v: stamp.schema_version,
                        index_min: stamp.min_reader_version,
                    }
                }
                _ => IndexState::Malformed,
            },
        }
    }

    /// The index, or a typed refusal. **Every write path must go through this**
    /// — it is what stops an unreadable index being overwritten (see
    /// [`AccountError::IndexUnreadable`]).
    ///
    /// Callers must hold the mutation lock (every write path does).
    fn writable_index(&self) -> Result<AccountIndex, AccountError> {
        match self.index_state() {
            IndexState::Readable(idx) => Ok(*idx),
            IndexState::Absent => Ok(AccountIndex::default()),
            IndexState::Unreadable { index_v, index_min } => Err(AccountError::IndexUnreadable {
                index_v,
                index_min,
                bin_v: CURRENT_INDEX_VERSION,
            }),
            IndexState::Malformed => Err(AccountError::IndexMalformed),
        }
    }
}

// ---------------------------------------------------------------------------
// Test helper: in-memory SecretStore.
// ---------------------------------------------------------------------------

#[cfg(any(test, feature = "test-helpers"))]
mod in_memory {
    use super::SecretStore;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// In-memory [`SecretStore`] for tests and downstream harnesses.
    #[derive(Default)]
    pub struct InMemorySecretStore {
        map: Mutex<HashMap<String, String>>,
    }

    impl InMemorySecretStore {
        pub fn new() -> Self {
            Self::default()
        }
        /// Seed a raw logical key before use.
        pub fn seed(&self, key: &str, value: &str) {
            self.map.lock().unwrap().insert(key.into(), value.into());
        }
    }

    impl SecretStore for InMemorySecretStore {
        fn get(&self, key: &str) -> Option<String> {
            self.map.lock().unwrap().get(key).cloned()
        }
        fn set(&self, key: &str, value: &str) {
            self.map.lock().unwrap().insert(key.into(), value.into());
        }
        fn delete(&self, key: &str) {
            self.map.lock().unwrap().remove(key);
        }
    }
}

#[cfg(any(test, feature = "test-helpers"))]
pub use in_memory::InMemorySecretStore;

/// Test-only helpers shared between this module's tests and the adapter's
/// (`launch_persistence.rs`).
#[cfg(test)]
pub(crate) mod tests_support {
    use super::{InMemorySecretStore, SecretStore};
    use std::sync::Mutex;

    /// Counts `set` calls so a test can assert an operation is a pure read /
    /// delete-only.
    pub(crate) struct WriteCountingStore {
        pub(crate) inner: InMemorySecretStore,
        writes: Mutex<Vec<String>>,
    }

    impl WriteCountingStore {
        pub(crate) fn new() -> Self {
            Self {
                inner: InMemorySecretStore::new(),
                writes: Mutex::new(Vec::new()),
            }
        }
        pub(crate) fn writes(&self) -> Vec<String> {
            self.writes.lock().unwrap().clone()
        }
    }

    impl SecretStore for WriteCountingStore {
        fn get(&self, key: &str) -> Option<String> {
            self.inner.get(key)
        }
        fn set(&self, key: &str, value: &str) {
            self.writes.lock().unwrap().push(key.to_string());
            self.inner.set(key, value);
        }
        fn delete(&self, key: &str) {
            self.inner.delete(key);
        }
    }

    /// A store whose `delete` silently does nothing — the shape every production
    /// arm takes when its delete fails, since [`SecretStore::delete`] reports
    /// nothing: the keyring arm warn-logs, the file arm warn-logs a failed
    /// rewrite, apple drops the keychain status, android swallows. Reach through
    /// `inner` to stand in for a later wholesale reset that DOES land.
    #[derive(Default)]
    pub(crate) struct RefusingDeleteStore {
        pub(crate) inner: InMemorySecretStore,
    }

    impl SecretStore for RefusingDeleteStore {
        fn get(&self, key: &str) -> Option<String> {
            self.inner.get(key)
        }
        fn set(&self, key: &str, value: &str) {
            self.inner.set(key, value);
        }
        fn delete(&self, _key: &str) {}
    }
}

#[cfg(test)]
mod index_bound_tests;

#[cfg(test)]
mod tests {
    use super::tests_support::WriteCountingStore;
    use super::*;

    // Any 32 bytes is a valid Ed25519 secret.
    const SECRET_A: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const SECRET_B: &str = "2222222222222222222222222222222222222222222222222222222222222222";

    fn actor_of(secret_hex: &str) -> String {
        ActorKeypair::from_secret_hex(secret_hex)
            .unwrap()
            .actor_id_hex()
    }

    fn registry() -> (AccountRegistry, Arc<InMemorySecretStore>) {
        let store = Arc::new(InMemorySecretStore::new());
        (AccountRegistry::new(store.clone()), store)
    }

    /// Seed one account straight into `store` (index + secret slot), without a
    /// registry — so a write-counting or delete-refusing wrapper starts from a
    /// populated store its own counters never saw being written.
    fn seed_account(store: &InMemorySecretStore, secret_hex: &str) -> String {
        let actor = actor_of(secret_hex);
        store.seed(
            INDEX_KEY,
            &format!(r#"{{"active":"{actor}","accounts":[{{"actor_id":"{actor}"}}]}}"#),
        );
        store.seed(&secret_key(&actor), secret_hex);
        actor
    }

    /// An index a newer build wrote that this build must not touch: it raised
    /// the reader floor past us. Two accounts are named in it.
    fn newer_breaking_index(actor_a: &str, actor_b: &str) -> String {
        format!(
            r#"{{"active":"{actor_a}","accounts":[
                 {{"actor_id":"{actor_a}"}},{{"actor_id":"{actor_b}"}}
               ],"schema_version":9,"min_reader_version":9}}"#
        )
    }

    /// A keystore that refuses exactly the secret row — a locked keyring, a
    /// full localStorage quota — while every other write lands.
    #[derive(Default)]
    struct SecretDroppingStore(InMemorySecretStore);

    impl SecretStore for SecretDroppingStore {
        fn get(&self, key: &str) -> Option<String> {
            self.0.get(key)
        }
        fn set(&self, key: &str, value: &str) {
            if !key.ends_with("/secret") {
                self.0.set(key, value);
            }
        }
        fn delete(&self, key: &str) {
            self.0.delete(key);
        }
    }

    /// `Ok` from `add_account` is a promise the secret reads back. `set` is
    /// infallible by signature, so a dropped write is invisible unless the add
    /// asks; before this it returned the actor id and indexed a row nothing
    /// could launch, and a caller that trusted it — tui's successor adoption
    /// — switched onto an account with no key instead of showing the only copy
    /// (`settings.md` § Recovery kit → *The persist-failure message survives
    /// the page*). The index and the other slots stay untouched: a refused add
    /// leaves nothing behind for `set_active` to strand the client on.
    #[test]
    fn add_account_refuses_a_secret_the_store_did_not_keep() {
        let store = Arc::new(SecretDroppingStore::default());
        let reg = AccountRegistry::new(store.clone());
        let actor = actor_of(SECRET_A);

        let err = reg
            .add_account(SECRET_A, Some("https://a.example"), Some("dev-1"))
            .expect_err("a secret that did not land must not read as added");
        assert!(
            matches!(&err, AccountError::NoStoredSecret(a) if *a == actor),
            "got {err:?}"
        );
        assert!(
            reg.list().is_empty(),
            "no index row for an unlaunchable account"
        );
        assert_eq!(store.get(INDEX_KEY), None, "the index was never written");
        assert_eq!(
            store.get(&nest_url_key(&actor)),
            None,
            "no orphan slots beside a secret that is not there"
        );
    }

    /// The launch-wiring parse (`account-scoping.md` § Concurrent instances):
    /// what a spawning instance puts in [`BOUND_ACCOUNT_ENV`] and what the child
    /// makes of it. Normalizing but **not** validating is the point — the bind
    /// gate is the one gate, so junk arrives there and is refused as
    /// `UnknownActor` rather than being quietly dropped into a plain launch.
    #[test]
    fn the_bound_account_binding_normalizes_but_never_validates() {
        let hex = actor_of(SECRET_A);
        assert_eq!(parse_bound_account(None), None, "unset = a primary launch");
        assert_eq!(
            parse_bound_account(Some("   ")),
            None,
            "an empty/blank value clears an inherited binding rather than binding to nothing"
        );
        assert_eq!(
            parse_bound_account(Some(&format!("  {}\n", hex.to_uppercase()))),
            Some(hex.clone()),
            "trimmed + lowercased, so it matches the registry's own actor-id form"
        );
        // Junk survives the parse ON PURPOSE and dies at the gate — one gate.
        assert_eq!(
            parse_bound_account(Some("not-an-actor")),
            Some("not-an-actor".to_string())
        );
        let (reg, _store) = registry();
        assert!(matches!(
            reg.bind_account("not-an-actor"),
            Err(AccountError::UnknownActor(_))
        ));
    }

    /// **The cliff this track exists to kill.** A newer build wrote an index this
    /// build cannot read, and both accounts' identity secrets live in per-actor
    /// slots reachable *only* through that index. Before the fix, an unreadable
    /// index was indistinguishable from an absent one, and the next write
    /// "healed" (overwrote) the index — silently stranding every other
    /// account's secret in the keystore forever.
    ///
    /// Now every mutator refuses, and the index is left byte-for-byte intact.
    #[test]
    fn unreadable_index_is_refused_and_never_overwritten() {
        let (reg, store) = registry();
        let (a, b) = (actor_of(SECRET_A), actor_of(SECRET_B));
        let raw = newer_breaking_index(&a, &b);
        store.set(INDEX_KEY, &raw);

        // Every write path refuses, with the honest typed error.
        let err = reg
            .add_account(SECRET_B, None, None)
            .expect_err("a write against an unreadable index must refuse");
        assert!(
            matches!(
                err,
                AccountError::IndexUnreadable {
                    index_v: 9,
                    index_min: 9,
                    ..
                }
            ),
            "expected the typed IndexUnreadable verdict, got: {err:?}",
        );
        assert!(matches!(
            reg.set_active(&a),
            Err(AccountError::IndexUnreadable { .. })
        ));
        assert!(matches!(
            reg.remove(&a),
            Err(AccountError::IndexUnreadable { .. })
        ));
        assert!(matches!(
            reg.update_cache(&a, Some("h"), None, None),
            Err(AccountError::IndexUnreadable { .. })
        ));

        // A read never writes.
        let _ = reg.index();

        // I1: the newer build's index is still exactly what it wrote — so both
        // accounts, and the secrets they point at, are still reachable by it.
        assert_eq!(
            store.get(INDEX_KEY).as_deref(),
            Some(raw.as_str()),
            "refusing must not rewrite the index — that is what orphans the secrets",
        );
    }

    /// I2: an index a newer build wrote *additively* (it never raised the reader
    /// floor) stays usable — and the fields we don't understand survive our
    /// rewrite instead of being silently dropped.
    #[test]
    fn newer_additive_index_round_trips_unknown_fields_and_is_not_restamped_down() {
        let (reg, store) = registry();
        let a = actor_of(SECRET_A);
        store.set(
            INDEX_KEY,
            &format!(
                r#"{{"active":"{a}","accounts":[{{"actor_id":"{a}","future_flag":true}}],
                     "schema_version":5,"min_reader_version":1,"future_root":{{"k":1}}}}"#
            ),
        );

        // Readable (the floor is still within us), and a write succeeds.
        reg.update_cache(&a, Some("handle"), None, None).unwrap();

        let raw = store.get(INDEX_KEY).expect("index still present");
        let back: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            back["future_root"],
            serde_json::json!({"k": 1}),
            "an unknown top-level field a newer build wrote must survive our rewrite",
        );
        assert_eq!(
            back["accounts"][0]["future_flag"],
            serde_json::json!(true),
            "an unknown per-entry field must survive too",
        );
        assert_eq!(
            back["schema_version"],
            serde_json::json!(5),
            "we must not restamp a newer index's version down",
        );
        assert_eq!(
            back["accounts"][0]["handle"],
            serde_json::json!("handle"),
            "and our own write must still land",
        );
    }

    /// Malformed JSON is "present but unreadable" too — not "absent". Same
    /// refusal, same no-overwrite guarantee — but its own verdict: nothing
    /// about a newer binary explains an unparseable blob, so reporting it as a
    /// version problem tells the user to do something that cannot work.
    #[test]
    fn malformed_index_is_unreadable_not_absent() {
        let (reg, store) = registry();
        store.set(INDEX_KEY, "{ this is not json");

        assert!(matches!(
            reg.add_account(SECRET_B, None, None),
            Err(AccountError::IndexMalformed)
        ));
        assert_eq!(
            store.get(INDEX_KEY).as_deref(),
            Some("{ this is not json"),
            "a blob we cannot parse is still not ours to destroy",
        );

        // The message must not send the user after an app update that cannot
        // help, and must not quote numbers it never read.
        let msg = AccountError::IndexMalformed.to_string();
        assert!(
            !msg.contains("update the app"),
            "a malformed index is not an out-of-date app: {msg}"
        );
        assert!(
            !msg.contains("schema_version="),
            "the malformed arm must not invent version numbers it did not read: {msg}"
        );
    }

    /// The cliff `version-compatibility.md` § 5 item 9 names: a newer build
    /// retypes a field non-additively, so the whole index is undecodable HERE
    /// while being perfectly intact THERE. Failing to parse is not evidence of
    /// corruption — the stamp still peeks out, and this is the version case
    /// with its real numbers, not an invented baseline pair.
    #[test]
    fn a_retyped_index_that_cannot_parse_still_peeks_its_real_stamp() {
        let (reg, store) = registry();
        // `accounts` retyped from a list to a map: `AccountIndex` cannot decode
        // it, `AccountIndexStamp` does not look at it.
        store.set(
            INDEX_KEY,
            r#"{"schema_version":7,"min_reader_version":6,"accounts":{"retyped":"by a newer build"}}"#,
        );

        match reg.add_account(SECRET_B, None, None) {
            Err(AccountError::IndexUnreadable {
                index_v,
                index_min,
                bin_v,
            }) => {
                assert_eq!(
                    (index_v, index_min),
                    (7, 6),
                    "the numbers must be READ, not invented"
                );
                assert_eq!(bin_v, CURRENT_INDEX_VERSION);
            }
            other => panic!("expected the version refusal with real numbers, got {other:?}"),
        }
        assert!(
            store.get(INDEX_KEY).unwrap().contains("retyped"),
            "the version refusal still rewrites nothing",
        );
    }

    /// The peek must not turn every unparseable blob into a version refusal:
    /// a baseline stamp is what a stamp-less blob yields, so it is no
    /// evidence of a newer writer.
    #[test]
    fn an_unparseable_index_stamped_within_our_floor_is_malformed_not_a_version_refusal() {
        let (reg, _store) = registry();
        _store.set(
            INDEX_KEY,
            r#"{"schema_version":1,"min_reader_version":1,"accounts":"not a list"}"#,
        );

        assert!(matches!(
            reg.add_account(SECRET_B, None, None),
            Err(AccountError::IndexMalformed)
        ));
    }

    /// Whatever the cause, the read path reports nothing for a blob it cannot
    /// read — never a guessed shape, which is what used to precede the overwrite.
    #[test]
    fn a_malformed_index_reads_as_no_accounts() {
        let (reg, store) = registry();
        store.set(INDEX_KEY, "{ this is not json");

        assert!(reg.index().accounts.is_empty());
        assert!(reg.active().is_none());
    }

    #[test]
    fn add_lists_and_first_becomes_active() {
        let (reg, _s) = registry();
        let a = reg
            .add_account(SECRET_A, Some("https://a.example"), Some("dev-a"))
            .unwrap();
        assert_eq!(a, actor_of(SECRET_A));
        assert_eq!(reg.list().len(), 1);
        assert_eq!(reg.active().as_deref(), Some(a.as_str()));

        let b = reg.add_account(SECRET_B, None, None).unwrap();
        assert_eq!(reg.list().len(), 2);
        // First-added stays active; adding a second doesn't steal focus.
        assert_eq!(reg.active().as_deref(), Some(a.as_str()));

        reg.set_active(&b).unwrap();
        assert_eq!(reg.active().as_deref(), Some(b.as_str()));
    }

    #[test]
    fn secrets_round_trip() {
        let (reg, _s) = registry();
        let a = reg
            .add_account(SECRET_A, Some("https://a.example"), Some("dev-a"))
            .unwrap();
        let got = reg.secrets(&a).unwrap();
        assert_eq!(got.secret_hex.as_str(), SECRET_A);
        assert_eq!(got.nest_url.as_deref(), Some("https://a.example"));
        assert_eq!(got.device_id.as_deref(), Some("dev-a"));
    }

    /// Activating is the point of no return: the client tears its live session
    /// down and relaunches as the target. So an account that is *listed* but has
    /// no resolvable secret must be refused HERE — after teardown there is no
    /// route back, because relaunch finds no identity, drops into the onboarding
    /// wizard, and the switcher only exists behind an authenticated session. The
    /// entry is reachable in the wild: `SecretStore::set` cannot report failure,
    /// so a keystore write that silently dropped (or a row the OS removed) leaves
    /// exactly this shape.
    #[test]
    fn activating_an_account_whose_secret_is_gone_is_refused_and_leaves_the_live_one_active() {
        let (reg, store) = registry();
        let a = reg
            .add_account(SECRET_A, Some("https://a.example"), None)
            .unwrap();
        let b = reg
            .add_account(SECRET_B, Some("https://b.example"), None)
            .unwrap();
        reg.set_active(&a).unwrap();

        // B's secret row vanishes underneath us; the index still lists it.
        store.delete(&secret_key(&b));
        assert!(
            reg.list().iter().any(|e| e.actor_id == b),
            "precondition: still listed"
        );

        let err = reg
            .set_active(&b)
            .expect_err("must refuse an unlaunchable account");
        assert!(matches!(err, AccountError::NoStoredSecret(ref who) if who == &b));
        assert_eq!(
            reg.active().as_deref(),
            Some(a.as_str()),
            "the refusal must leave the healthy account active — a client that tore down \
             on this error and relaunched would strand the user in the wizard"
        );
    }

    #[test]
    fn add_is_idempotent_for_same_identity() {
        let (reg, _s) = registry();
        reg.add_account(SECRET_A, Some("https://a.example"), None)
            .unwrap();
        reg.add_account(SECRET_A, Some("https://a2.example"), None)
            .unwrap();
        assert_eq!(reg.list().len(), 1);
    }

    #[test]
    fn remove_falls_back_active_and_clears_secrets() {
        let (reg, _s) = registry();
        let a = reg.add_account(SECRET_A, None, None).unwrap();
        let b = reg.add_account(SECRET_B, None, None).unwrap();
        reg.set_active(&b).unwrap();

        reg.remove(&b).unwrap();
        assert_eq!(reg.list().len(), 1);
        assert_eq!(reg.active().as_deref(), Some(a.as_str())); // fell back
        assert!(reg.secrets(&b).is_none());

        assert!(matches!(
            reg.remove("deadbeef"),
            Err(AccountError::UnknownActor(_))
        ));
    }

    /// `clear_nest_binding` keeps the identity but drops the nest wiring.
    #[test]
    fn clear_nest_binding_clears_slots_keeping_the_secret() {
        let (reg, _store) = registry();
        let a = reg
            .add_account(SECRET_A, Some("https://a.example"), Some("dev-a"))
            .unwrap();

        reg.clear_nest_binding(&a).unwrap();
        let s = reg.secrets(&a).expect("the identity must survive");
        assert_eq!(s.secret_hex.as_str(), SECRET_A);
        assert_eq!(s.nest_url, None);
        assert_eq!(s.device_id, None);

        assert!(matches!(
            reg.clear_nest_binding("deadbeef"),
            Err(AccountError::UnknownActor(_))
        ));
    }

    #[test]
    fn require_confirm_flag_persists() {
        let (reg, store) = registry();
        let a = reg.add_account(SECRET_A, None, None).unwrap();
        assert!(!reg.list()[0].require_confirm_to_activate);
        reg.set_require_confirm(&a, true).unwrap();

        // Reload over the same store: flag survived (it's in the index blob).
        let reg2 = AccountRegistry::new(store);
        assert!(reg2.list()[0].require_confirm_to_activate);
    }

    #[test]
    fn auto_enable_flips_an_untouched_account_once_and_persists() {
        let (reg, store) = registry();
        let a = reg.add_account(SECRET_A, None, None).unwrap();

        assert!(
            reg.auto_enable_require_confirm(&a).unwrap(),
            "first observation on an untouched account must flip the flag on"
        );
        assert!(reg.list()[0].require_confirm_to_activate);
        assert!(
            !reg.auto_enable_require_confirm(&a).unwrap(),
            "already on — the second observation is a no-op"
        );

        // Persisted: a reload still carries the flag, and the auto-set did NOT
        // consume the user's override right (user_set stays false).
        let reg2 = AccountRegistry::new(store);
        assert!(reg2.list()[0].require_confirm_to_activate);
        assert!(!reg2.list()[0].require_confirm_user_set);
    }

    #[test]
    fn auto_enable_never_overrides_an_explicit_user_off() {
        let (reg, _s) = registry();
        let a = reg.add_account(SECRET_A, None, None).unwrap();

        // The user touched the toggle: on, then explicitly OFF.
        reg.set_require_confirm(&a, true).unwrap();
        reg.set_require_confirm(&a, false).unwrap();

        assert!(
            !reg.auto_enable_require_confirm(&a).unwrap(),
            "an explicit user OFF must stick against the admin auto-default"
        );
        assert!(!reg.list()[0].require_confirm_to_activate);

        assert!(matches!(
            reg.auto_enable_require_confirm("deadbeef"),
            Err(AccountError::UnknownActor(_))
        ));
    }

    #[test]
    fn set_active_refuses_a_flagged_account_without_confirmation() {
        let (reg, _s) = registry();
        let a = reg.add_account(SECRET_A, None, None).unwrap();
        let b = reg.add_account(SECRET_B, None, None).unwrap();
        reg.set_active(&a).unwrap();
        reg.set_require_confirm(&b, true).unwrap();

        let err = reg
            .set_active(&b)
            .expect_err("a flagged account must demand re-auth confirmation");
        assert!(matches!(err, AccountError::ConfirmationRequired(ref who) if who == &b));
        assert_eq!(
            reg.active().as_deref(),
            Some(a.as_str()),
            "the refusal must be a pure no-op — a declined re-auth keeps the current account"
        );
    }

    #[test]
    fn set_active_confirmed_activates_a_flagged_account() {
        let (reg, _s) = registry();
        reg.add_account(SECRET_A, None, None).unwrap();
        let b = reg.add_account(SECRET_B, None, None).unwrap();
        reg.set_require_confirm(&b, true).unwrap();

        reg.set_active_confirmed(&b).unwrap();
        assert_eq!(reg.active().as_deref(), Some(b.as_str()));
    }

    /// The concurrent-instances bind gate mirrors every activation guard 1:1
    /// (`account-scoping.md` § Concurrent instances) — "open as new instance"
    /// must not be a side door around them — and moves nothing: the active
    /// pointer is untouched by a successful bind.
    #[test]
    fn bind_account_mirrors_the_activation_guards_and_moves_nothing() {
        let (reg, store) = registry();
        let a = reg.add_account(SECRET_A, None, None).unwrap();
        let b = reg.add_account(SECRET_B, None, None).unwrap();
        reg.set_active(&a).unwrap();

        // Happy path: bindable, and `active` stays on A.
        reg.bind_account(&b).unwrap();
        assert_eq!(reg.active().as_deref(), Some(a.as_str()));

        // Unknown account.
        assert!(matches!(
            reg.bind_account("deadbeef"),
            Err(AccountError::UnknownActor(_))
        ));

        // Flagged account: refused unless confirmed — and launchability
        // outranks the flag, exactly as for activation.
        reg.set_require_confirm(&b, true).unwrap();
        assert!(matches!(
            reg.bind_account(&b),
            Err(AccountError::ConfirmationRequired(ref who)) if who == &b
        ));
        reg.bind_account_confirmed(&b).unwrap();
        store.delete(&secret_key(&b));
        assert!(matches!(
            reg.bind_account_confirmed(&b),
            Err(AccountError::NoStoredSecret(_))
        ));
    }

    /// The bind gate is a PURE read: it fires at instance-spawn, a path that
    /// can race a concurrent sign-out (the read-that-writes hazard § Cleanup
    /// contract).
    #[test]
    fn bind_account_is_a_pure_read() {
        let counting = Arc::new(tests_support::WriteCountingStore::new());
        let a = seed_account(&counting.inner, SECRET_A);
        let reg = AccountRegistry::new(counting.clone());

        reg.bind_account(&a).unwrap();

        assert_eq!(
            counting.writes(),
            Vec::<String>::new(),
            "binding must not write"
        );
    }

    #[test]
    fn set_active_confirmed_keeps_the_unknown_and_no_secret_guards() {
        let (reg, store) = registry();
        let a = reg.add_account(SECRET_A, None, None).unwrap();
        let b = reg.add_account(SECRET_B, None, None).unwrap();
        reg.set_active(&a).unwrap();
        reg.set_require_confirm(&b, true).unwrap();

        assert!(matches!(
            reg.set_active_confirmed("deadbeef"),
            Err(AccountError::UnknownActor(_))
        ));

        // The confirm gate must not outrank the launchability guard: an account
        // whose secret is gone reads NoStoredSecret, not ConfirmationRequired —
        // there is nothing a re-auth prompt could fix.
        store.delete(&secret_key(&b));
        assert!(matches!(
            reg.set_active(&b),
            Err(AccountError::NoStoredSecret(_))
        ));
        assert!(matches!(
            reg.set_active_confirmed(&b),
            Err(AccountError::NoStoredSecret(_))
        ));
        assert_eq!(reg.active().as_deref(), Some(a.as_str()));
    }

    #[test]
    fn index_persists_across_reload() {
        let (reg, store) = registry();
        let a = reg.add_account(SECRET_A, None, None).unwrap();
        drop(reg);
        let reg2 = AccountRegistry::new(store);
        assert_eq!(reg2.list().len(), 1);
        assert_eq!(reg2.active().as_deref(), Some(a.as_str()));
    }

    #[test]
    fn empty_store_has_no_accounts() {
        let (reg, _s) = registry();
        assert!(reg.list().is_empty());
        assert_eq!(reg.active(), None);
    }

    #[test]
    fn clear_all_wipes_every_account_and_the_index() {
        let (reg, store) = registry();
        let a = reg
            .add_account(SECRET_A, Some("https://a.example"), Some("dev-a"))
            .unwrap();
        let b = reg.add_account(SECRET_B, None, Some("dev-b")).unwrap();

        // Populate every per-actor slot on both accounts — clear_all's
        // contract is "every account's fauna/{actor_id}/* slots", not just
        // the three slots `add_account` happens to write, so the witness
        // must exercise all eight or it proves nothing about the other five
        // (the prior version of this test asserted four of
        // eight, silently, for a security-relevant erase).
        for actor in [&a, &b] {
            reg.set_pending_invite_json(actor, "{}");
            reg.set_awaiting_dns_json(actor, "{}");
            reg.set_reach_ipv4(actor, "203.0.113.1");
            reg.set_pending_factory_reset_json(actor, "{}");
            reg.park_aftermath_ceremony(actor, "{}");
            reg.set_supervision_snapshot_json(actor, "{}");
        }

        let _ = reg.clear_all();

        // Every per-actor slot of EVERY account, not just the active one —
        // iterating the SAME `per_actor_keys` builder list `clear_all`/
        // `remove` delete from, so a slot added to the namespace is covered
        // by construction rather than by a hand-kept list beside it.
        for actor in [&a, &b] {
            for key in per_actor_keys(actor) {
                assert_eq!(store.get(&key), None, "{key} of {actor}");
            }
        }
        assert_eq!(store.get(INDEX_KEY), None, "the index blob is gone");
        assert_eq!(reg.active(), None);
        assert!(reg.list().is_empty());
    }

    #[test]
    fn clear_all_never_writes_to_the_store() {
        // clear_all() must be delete-only: a sign-out that creates a fresh copy
        // of the user's secret (a libsecret item, a localStorage key) before
        // deleting it is a crash-unsafe way to erase a credential — a crash
        // mid-clear resurrects the identity being erased.
        let store = Arc::new(WriteCountingStore::new());
        let a = seed_account(&store.inner, SECRET_A);
        let reg = AccountRegistry::new(store.clone());

        let _ = reg.clear_all();

        assert_eq!(
            store.writes(),
            Vec::<String>::new(),
            "clear_all must be delete-only; it wrote these keys"
        );
        assert_eq!(store.get(&secret_key(&a)), None);
        assert_eq!(store.get(INDEX_KEY), None);
    }

    /// A store that takes every delete leaves a CLEAN sweep — the ordinary
    /// sign-out, which must keep painting nothing.
    #[test]
    fn clear_all_over_a_store_that_takes_the_erase_is_clean() {
        let store = Arc::new(InMemorySecretStore::new());
        seed_account(&store, SECRET_A);
        let reg = AccountRegistry::new(store);

        assert_eq!(reg.clear_all(), CredentialSweep::default());
    }

    /// ⚠ **The false negative this closes.** Every production arm's
    /// `delete` swallows its own failure, so a store that refuses the erase used
    /// to leave `clear_all` returning `()` over an identity seed still on the
    /// device, and the seat painted a clean "Signed out". The read-back names
    /// exactly what is still readable — the per-actor secret and the index
    /// among them.
    #[test]
    fn a_credential_the_store_refused_to_delete_survives_the_sweep() {
        let store = Arc::new(super::tests_support::RefusingDeleteStore::default());
        let actor = seed_account(&store.inner, SECRET_A);
        let reg = AccountRegistry::new(store.clone());

        let sweep = reg.clear_all();

        assert!(!sweep.is_clean(), "a refused erase is not a clean one");
        assert!(
            sweep.survivors.contains(&secret_key(&actor)),
            "the identity seed is still readable and must be named: {:?}",
            sweep.survivors
        );
        assert!(
            sweep.survivors.contains(&INDEX_KEY.to_string()),
            "{:?}",
            sweep.survivors
        );
        assert!(
            !sweep.survivors.iter().any(|k| store.get(k).is_none()),
            "a survivor is only ever a key that still reads back: {:?}",
            sweep.survivors
        );
        assert!(!sweep.wipe_failed, "no namespace wipe ran here");
    }

    /// The auxiliary namespace is read back too — it is where the store writer
    /// key and principal bundles live, and the wholesale wipe of the app's own
    /// namespace never reaches it.
    #[test]
    fn a_refusal_in_an_auxiliary_namespace_survives_the_sweep_too() {
        let store = Arc::new(InMemorySecretStore::new());
        let actor = seed_account(&store, SECRET_A);
        let aux = Arc::new(super::tests_support::RefusingDeleteStore::default());
        aux.inner.seed(&store_writer_key(&actor), "writer");
        let reg = AccountRegistry::new(store).also_erasing(vec![aux as Arc<dyn SecretStore>]);

        let sweep = reg.clear_all();

        assert_eq!(sweep.survivors, vec![store_writer_key(&actor)]);
    }

    /// `reverify` re-asks after a later wholesale reset: what the reset took
    /// drops out, and a recorded wipe failure is kept even when nothing reads
    /// back — the locked-keyring shape a read-back cannot see.
    #[test]
    fn reverify_drops_what_a_later_reset_removed_and_keeps_a_wipe_failure() {
        let store = Arc::new(super::tests_support::RefusingDeleteStore::default());
        seed_account(&store.inner, SECRET_A);
        let reg = AccountRegistry::new(store.clone());
        let sweep = reg.clear_all();
        assert!(!sweep.is_clean());

        // Nothing changed: the same survivors come back.
        let unchanged = reg.reverify(sweep.clone());
        assert_eq!(unchanged, sweep);

        // A later reset that lands (android's `SecureStorage.clear()`).
        for key in &sweep.survivors {
            store.inner.delete(key);
        }
        assert!(reg.reverify(sweep.clone()).is_clean());

        let mut refused = sweep;
        refused.record_wipe_failure();
        let after = reg.reverify(refused);
        assert!(after.survivors.is_empty());
        assert!(
            !after.is_clean(),
            "a failed wipe stays a residue after the read-back clears"
        );
    }

    #[test]
    fn a_read_never_writes_to_the_store() {
        // A migrating accessor was once the sign-out hazard: `index()` ran a
        // one-shot migration's four `set`s as a side effect of an ordinary
        // READ, reachable from anywhere in the app. An erase landing inside
        // that window left the later writes behind — up to and including the
        // identity secret — in the namespace the user had just wiped, so
        // sign-out did not sign them out (`long-term-store.md` § Cleanup
        // contract). This is the property, not the ordering of any one caller:
        // it holds for every reader, on every thread, forever.
        let store = Arc::new(WriteCountingStore::new());
        let a = seed_account(&store.inner, SECRET_A);
        store.inner.seed(&nest_url_key(&a), "https://a.example");
        let reg = AccountRegistry::new(store.clone());

        let active = reg.active();
        let list = reg.list();
        let secrets = reg.secrets(&a);
        let _ = reg.session_material(&a);

        assert_eq!(
            store.writes(),
            Vec::<String>::new(),
            "a read must never write; it wrote these keys"
        );
        assert_eq!(active.as_deref(), Some(a.as_str()));
        assert_eq!(list.len(), 1);
        let s = secrets.expect("the seeded account resolves");
        assert_eq!(s.secret_hex.as_str(), SECRET_A);
        assert_eq!(s.nest_url.as_deref(), Some("https://a.example"));
    }

    #[test]
    fn sign_out_then_new_identity_does_not_revert_to_the_signed_out_actor() {
        // A sign-out that left `fauna/index` pointing at the signed-out actor A
        // once signed the user back in as A on the next boot, whose secret was
        // never erased.
        let (reg, store) = registry();
        let a = reg.add_account(SECRET_A, None, None).unwrap();

        let _ = reg.clear_all(); // "Sign out"

        // A's secret must not survive sign-out on a shared browser/desktop.
        assert_eq!(store.get(&secret_key(&a)), None);

        // Onboarding the next identity B lands on B, never back on A.
        let reg = AccountRegistry::new(store.clone());
        let b = reg.add_account(SECRET_B, None, None).unwrap();
        assert_eq!(reg.active().as_deref(), Some(b.as_str()));
        assert_eq!(reg.list().len(), 1);
    }

    /// The cross-process advisory mutation lock (`mutation_lock.rs`): mutators
    /// serialize, reads and the bind gate stay lock-free, and a lock-file
    /// failure degrades open. `account-scoping.md` § Concurrent instances /
    /// `long-term-store.md` § Multi-account evolution.
    mod mutation_lock_serialization {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Mutex, mpsc};
        use std::thread;

        use super::*;

        /// Counts acquisitions so tests can pin which operations lock.
        struct CountingLock(AtomicUsize);

        impl CountingLock {
            fn new() -> Self {
                Self(AtomicUsize::new(0))
            }
            fn count(&self) -> usize {
                self.0.load(Ordering::SeqCst)
            }
        }

        impl MutationLock for CountingLock {
            fn acquire(&self) -> MutationLockGuard {
                self.0.fetch_add(1, Ordering::SeqCst);
                MutationLockGuard::unheld()
            }
        }

        /// A store that, when armed for a thread, pauses that thread's FIRST
        /// `set(fauna/index)` — signalling `paused` and blocking on `release`
        /// BEFORE the write applies. This holds a mutator open exactly inside
        /// its read-modify-write window, deterministically (no sleeps).
        struct PausingStore {
            inner: InMemorySecretStore,
            armed: Mutex<Vec<thread::ThreadId>>,
            paused_tx: mpsc::Sender<()>,
            release_rx: Mutex<mpsc::Receiver<()>>,
        }

        impl PausingStore {
            fn new() -> (Arc<Self>, mpsc::Receiver<()>, mpsc::Sender<()>) {
                let (paused_tx, paused_rx) = mpsc::channel();
                let (release_tx, release_rx) = mpsc::channel();
                let store = Arc::new(Self {
                    inner: InMemorySecretStore::new(),
                    armed: Mutex::new(Vec::new()),
                    paused_tx,
                    release_rx: Mutex::new(release_rx),
                });
                (store, paused_rx, release_tx)
            }

            fn arm_for_current_thread(&self) {
                self.armed.lock().unwrap().push(thread::current().id());
            }
        }

        impl SecretStore for PausingStore {
            fn get(&self, key: &str) -> Option<String> {
                self.inner.get(key)
            }
            fn set(&self, key: &str, value: &str) {
                if key == INDEX_KEY {
                    let mut armed = self.armed.lock().unwrap();
                    if let Some(pos) = armed.iter().position(|id| *id == thread::current().id()) {
                        armed.remove(pos); // one-shot per armed thread
                        drop(armed);
                        self.paused_tx.send(()).unwrap();
                        // Hold this mutator open mid-RMW until the test says go.
                        self.release_rx.lock().unwrap().recv().unwrap();
                    }
                }
                self.inner.set(key, value);
            }
            fn delete(&self, key: &str) {
                self.inner.delete(key);
            }
        }

        /// Documents the race the lock exists for: with no lock, an
        /// `add_account` paused between reading and writing the index swallows
        /// a concurrent add wholesale — the second account's entry vanishes
        /// from the index even though its slot writes landed.
        #[test]
        fn without_the_lock_a_paused_writer_swallows_a_concurrent_add() {
            let (store, paused, release) = PausingStore::new();
            let reg_a = AccountRegistry::new(store.clone());
            let reg_b = AccountRegistry::new(store.clone());

            let a = thread::spawn({
                let store = store.clone();
                move || {
                    store.arm_for_current_thread();
                    reg_a.add_account(SECRET_A, None, None).unwrap();
                }
            });
            paused.recv().unwrap(); // A is mid-RMW, index computed but unwritten
            reg_b.add_account(SECRET_B, None, None).unwrap(); // completes fully
            release.send(()).unwrap(); // A writes its pre-B index
            a.join().unwrap();

            let reg = AccountRegistry::new(store);
            let listed: Vec<String> = reg.list().into_iter().map(|e| e.actor_id).collect();
            assert_eq!(
                listed,
                vec![actor_of(SECRET_A)],
                "expected the documented lost update: B's entry swallowed by A's rewrite"
            );
        }

        /// The same interleaving under `FileMutationLock`: B cannot enter its
        /// RMW while A is paused inside its own, so both accounts survive.
        ///
        /// BOTH writers pause at their index write, so a no-op lock fails this
        /// in every interleaving (each reads the index before the other's
        /// write lands, and the later rewrite swallows the earlier) — the
        /// discriminating half of the red-green pair above.
        #[test]
        fn under_the_file_lock_contended_adds_both_survive() {
            let dir = tempfile::tempdir().unwrap();
            let (store, paused, release) = PausingStore::new();
            let lock_a: Arc<dyn MutationLock> = Arc::new(FileMutationLock::new(dir.path()));
            let lock_b: Arc<dyn MutationLock> = Arc::new(FileMutationLock::new(dir.path()));
            let reg_a = AccountRegistry::with_mutation_lock(store.clone(), lock_a);
            let reg_b = AccountRegistry::with_mutation_lock(store.clone(), lock_b);

            let a = thread::spawn({
                let store = store.clone();
                move || {
                    store.arm_for_current_thread();
                    reg_a.add_account(SECRET_A, None, None).unwrap();
                }
            });
            paused.recv().unwrap(); // A holds the file lock, mid-RMW
            let b = thread::spawn({
                let store = store.clone();
                move || {
                    store.arm_for_current_thread();
                    reg_b.add_account(SECRET_B, None, None).unwrap();
                }
            });
            // Bounded negative check (same pattern as the exclusion unit
            // test): B reaching its own index write while A is still paused
            // inside the lock IS the violation — a no-op lock trips this in
            // microseconds; a real one holds B at acquire.
            if paused
                .recv_timeout(std::time::Duration::from_millis(500))
                .is_ok()
            {
                panic!("B entered its index write while A still held the mutation lock");
            }
            release.send(()).unwrap(); // A completes and releases the lock
            paused.recv().unwrap(); // B, admitted, pauses at its own write
            release.send(()).unwrap();
            a.join().unwrap();
            b.join().unwrap();

            let reg = AccountRegistry::new(store);
            let mut listed: Vec<String> = reg.list().into_iter().map(|e| e.actor_id).collect();
            listed.sort();
            let mut expected = vec![actor_of(SECRET_A), actor_of(SECRET_B)];
            expected.sort();
            assert_eq!(listed, expected, "both contended adds must survive");
        }

        /// Every mutator acquires exactly once per call — which also proves no
        /// mutator nests a second public acquire (a real file lock would
        /// self-deadlock on separate opens within one process).
        #[test]
        fn every_mutator_acquires_the_mutation_lock_exactly_once() {
            let store = Arc::new(InMemorySecretStore::new());
            let lock = Arc::new(CountingLock::new());
            let reg = AccountRegistry::with_mutation_lock(
                store,
                Arc::clone(&lock) as Arc<dyn MutationLock>,
            );
            let a = actor_of(SECRET_A);

            let mut expected = 0usize;
            let mut assert_one = |label: &str| {
                expected += 1;
                assert_eq!(lock.count(), expected, "{label} must acquire exactly once");
            };

            reg.add_account(SECRET_A, Some("https://n.example"), None)
                .unwrap();
            assert_one("add_account");
            reg.set_active(&a).unwrap();
            assert_one("set_active");
            reg.update_cache(&a, Some("h"), Some("d"), Some("t"))
                .unwrap();
            assert_one("update_cache");
            reg.auto_enable_require_confirm(&a).unwrap();
            assert_one("auto_enable_require_confirm");
            reg.set_require_confirm(&a, false).unwrap();
            assert_one("set_require_confirm");
            reg.set_nest_url(&a, "https://n.example");
            assert_one("set_nest_url");
            reg.set_pending_invite_json(&a, "{}");
            assert_one("set_pending_invite_json");
            reg.clear_pending_invite(&a);
            assert_one("clear_pending_invite");
            reg.set_awaiting_dns_json(&a, "{}");
            assert_one("set_awaiting_dns_json");
            reg.clear_awaiting_dns(&a);
            assert_one("clear_awaiting_dns");
            reg.set_pending_factory_reset_json(&a, "{}");
            assert_one("set_pending_factory_reset_json");
            reg.clear_pending_factory_reset(&a);
            assert_one("clear_pending_factory_reset");
            reg.park_aftermath_ceremony(&a, "{}");
            assert_one("park_aftermath_ceremony");
            reg.clear_aftermath_ceremony(&a);
            assert_one("clear_aftermath_ceremony");
            let install = InMemorySecretStore::new();
            reg.ensure_install_device_secret(&install).unwrap();
            assert_one("ensure_install_device_secret");
            reg.device_id_for_actor(&install, &a).unwrap();
            assert_one("device_id_for_actor");
            reg.remove(&a).unwrap();
            assert_one("remove");
            let _ = reg.clear_all();
            assert_one("clear_all");
        }

        /// Reads and the bind gate never lock: purity extends to "a read never
        /// locks", so a spawn/launch is wait-free even against a wedged holder.
        #[test]
        fn reads_and_the_bind_gate_never_acquire_the_mutation_lock() {
            let store = Arc::new(InMemorySecretStore::new());
            // Seed through an unlocked registry so the counting one sees a
            // populated store.
            let seed = AccountRegistry::new(store.clone());
            let a = seed
                .add_account(SECRET_A, Some("https://n.example"), None)
                .unwrap();

            let lock = Arc::new(CountingLock::new());
            let reg = AccountRegistry::with_mutation_lock(
                store,
                Arc::clone(&lock) as Arc<dyn MutationLock>,
            );

            reg.index();
            reg.active();
            reg.list();
            reg.secrets(&a);
            reg.pending_invite_json(&a);
            reg.awaiting_dns_json(&a);
            reg.pending_factory_reset_json(&a);
            reg.pending_aftermath_ceremony(&a);
            reg.bind_account(&a).unwrap();
            reg.bind_account_confirmed(&a).unwrap();
            assert_eq!(lock.count(), 0, "reads and the bind gate must never lock");
        }

        /// Advisory degrade: a lock path that cannot possibly work (its parent
        /// "directory" is a regular file) must not break mutation — the lock
        /// narrows a race, never widens a failure.
        #[test]
        fn a_broken_lock_path_degrades_to_working_unlocked_mutation() {
            let dir = tempfile::tempdir().unwrap();
            let not_a_dir = dir.path().join("plain-file");
            std::fs::write(&not_a_dir, b"x").unwrap();
            let lock: Arc<dyn MutationLock> =
                Arc::new(FileMutationLock::new(&not_a_dir.join("sub")));
            let store = Arc::new(InMemorySecretStore::new());
            let reg = AccountRegistry::with_mutation_lock(store, lock);
            let a = reg.add_account(SECRET_A, None, None).unwrap();
            assert_eq!(reg.active().as_deref(), Some(a.as_str()));
        }
    }

    /// The session-identity read (`account-scoping.md` § Concurrent instances →
    /// *Session identity resolves through the session's account*).
    mod session_material {
        use super::*;

        /// The read resolves the REQUESTED account's slots + cache — not the
        /// active account's. This is the exact confusion the seam exists to
        /// remove: a bound secondary asking for account B while A is active
        /// must get B's material.
        #[test]
        fn session_material_resolves_the_requested_account_not_the_active_one() {
            let (reg, _store) = registry();
            let a = reg
                .add_account(SECRET_A, Some("https://a.example"), Some("dev-a"))
                .unwrap();
            let b = reg
                .add_account(SECRET_B, Some("https://b.example"), Some("dev-b"))
                .unwrap();
            reg.update_cache(&a, Some("alice"), Some("a.example"), None)
                .unwrap();
            reg.update_cache(&b, Some("bob"), Some("b.example"), Some("plus"))
                .unwrap();
            assert_eq!(reg.active().as_deref(), Some(a.as_str()), "A is active");

            let m = reg
                .session_material(&b)
                .expect("B resolves while A is active");
            assert_eq!(m.actor_id, b);
            assert_eq!(m.secret_hex.as_str(), SECRET_B);
            assert_eq!(m.nest_url.as_deref(), Some("https://b.example"));
            assert_eq!(m.device_id.as_deref(), Some("dev-b"));
            assert_eq!(m.handle.as_deref(), Some("bob"));
            assert_eq!(m.domain.as_deref(), Some("b.example"));
            assert_eq!(m.tier.as_deref(), Some("plus"));
        }

        /// Row 337 (`key-material-hierarchy.md` § Carrier shape) — `{:?}` on
        /// `SessionMaterial` must never print the account secret. Mutation:
        /// reverting `secret_hex` to a bare `String` reds this (the struct-level
        /// `#[derive(Debug)]` would then print the field verbatim) as well as
        /// the build-time pin above the struct.
        #[test]
        fn session_material_debug_never_prints_the_secret() {
            let (reg, _store) = registry();
            let a = reg.add_account(SECRET_A, None, None).unwrap();
            let m = reg
                .session_material(&a)
                .expect("just-added account resolves");
            let debug = format!("{m:?}");
            assert!(
                !debug.contains(SECRET_A),
                "Debug output must redact the secret, got {debug:?}"
            );
        }

        /// The same property on the read `SessionMaterial` is built *from*.
        /// `StoredAccount` holds the identical account secret, so it carries
        /// the identical guarantee — leaving it a bare `String` was the
        /// residual `key-material-hierarchy.md` § Carrier shape named on
        /// 2026-08-19, and splitting a family is exactly what that section
        /// warns against. Mutation: revert `secret_hex` to `String` and this
        /// reds alongside the build-time pin above the struct.
        #[test]
        fn stored_account_debug_never_prints_the_secret() {
            let (reg, _store) = registry();
            let a = reg.add_account(SECRET_A, None, None).unwrap();
            let stored = reg.secrets(&a).expect("just-added account resolves");
            let debug = format!("{stored:?}");
            assert!(
                !debug.contains(SECRET_A),
                "Debug output must redact the secret, got {debug:?}"
            );
        }

        /// Unknown or removed ⇒ `None`, never another account's material — a
        /// session must fail closed, not fall back (the fallback IS the bug
        /// class this seam closes).
        #[test]
        fn session_material_fails_closed_for_unknown_or_removed_accounts() {
            let (reg, _store) = registry();
            let a = reg.add_account(SECRET_A, None, None).unwrap();
            let b = reg.add_account(SECRET_B, None, None).unwrap();
            assert!(reg.session_material("00ff").is_none(), "junk id");

            reg.remove(&b).unwrap();
            assert!(
                reg.session_material(&b).is_none(),
                "an account removed out from under a session stops resolving"
            );
            assert!(
                reg.session_material(&a).is_some(),
                "the survivor still resolves"
            );
        }
    }

    // ── The succession link (`succession-aftermath.md` § Re-key scope) ──
    //
    // The post-succession corpus re-seal opens at-rest blobs under a retired
    // identity's `BackupKey`, so the successor's client has to know *which* of
    // its registry rows are predecessors. Without a recorded link the only
    // available answer is "every other account", which is wrong in a way that
    // leaks: an unrelated account's key opens that account's own device-local
    // sealed custody, so a re-seal pass fed the whole registry would fold a
    // different account's deployment seeds into this one.

    const SECRET_C: &str = "3333333333333333333333333333333333333333333333333333333333333333";
    const SECRET_D: &str = "4444444444444444444444444444444444444444444444444444444444444444";
    const SECRET_E: &str = "5555555555555555555555555555555555555555555555555555555555555555";
    const SECRET_F: &str = "6666666666666666666666666666666666666666666666666666666666666666";

    #[test]
    fn a_fresh_account_has_no_predecessors() {
        let (reg, _s) = registry();
        let a = reg.add_account(SECRET_A, None, None).unwrap();
        assert!(
            reg.predecessors_of(&a).is_empty(),
            "an identity that never succeeded from anything has no predecessors"
        );
    }

    /// A relaunch after a lost succession reply: the ceremony persisted the
    /// successor (with its nest) but never activated it, and the chain has now
    /// proven it — so the device takes it, and learns the link the lost fold
    /// never recorded.
    #[test]
    fn a_held_verified_successor_is_adopted_and_linked() {
        let (reg, _s) = registry();
        let old = reg
            .add_account(SECRET_A, Some("https://nest.example"), None)
            .unwrap();
        let new = reg
            .add_account(SECRET_B, Some("https://nest.example"), None)
            .unwrap();

        assert!(
            reg.adopt_held_successor(&old, &new),
            "the device holds the proven successor's key: adopt it"
        );
        assert_eq!(
            reg.predecessors_of(&new),
            vec![old.clone()],
            "the adoption records the link the ceremony's lost fold never did"
        );
    }

    /// The whole safety half: a successor the device holds no key for is the
    /// import screen's job — nothing is adopted and nothing is recorded.
    #[test]
    fn a_verified_successor_this_device_does_not_hold_is_not_adopted() {
        let (reg, _s) = registry();
        let old = reg
            .add_account(SECRET_A, Some("https://nest.example"), None)
            .unwrap();
        let elsewhere = actor_of(SECRET_B);

        assert!(!reg.adopt_held_successor(&old, &elsewhere));
        assert!(
            reg.predecessors_of(&elsewhere).is_empty(),
            "a refused adoption leaves no link behind"
        );
    }

    /// A held key with no nest to sign in to cannot be switched to: the switch
    /// would re-launch into onboarding. The import screen stays the answer.
    #[test]
    fn a_held_successor_with_no_nest_is_not_adopted() {
        let (reg, _s) = registry();
        let old = reg
            .add_account(SECRET_A, Some("https://nest.example"), None)
            .unwrap();
        let new = reg.add_account(SECRET_B, None, None).unwrap();

        assert!(!reg.adopt_held_successor(&old, &new));
        assert!(reg.predecessors_of(&new).is_empty());
    }

    #[test]
    fn recording_a_succession_makes_the_old_row_a_predecessor_of_the_new() {
        let (reg, _s) = registry();
        let old = reg.add_account(SECRET_A, None, None).unwrap();
        let new = reg.add_account(SECRET_B, None, None).unwrap();

        reg.record_succession(&old, &new).unwrap();

        assert_eq!(
            reg.predecessors_of(&new),
            vec![old.clone()],
            "the retired identity is the successor's predecessor"
        );
        assert!(
            reg.predecessors_of(&old).is_empty(),
            "and the link points one way — the successor is not its own \
             predecessor's predecessor"
        );
    }

    /// The link must outlive the predecessor's row. `remove` deletes that row
    /// outright, and with it the only copy of a link stored *on* it — which
    /// re-strands the successor's inherited profile the moment the user tidies
    /// the retired account away (`profile.md` § After an identity succession).
    #[test]
    fn the_succession_link_survives_removing_the_retired_account() {
        let (reg, _s) = registry();
        let a = reg.add_account(SECRET_A, None, None).unwrap();
        let b = reg.add_account(SECRET_B, None, None).unwrap();
        let c = reg.add_account(SECRET_C, None, None).unwrap();
        reg.record_succession(&a, &b).unwrap();
        reg.record_succession(&b, &c).unwrap();

        reg.remove(&b).unwrap();
        assert_eq!(
            reg.predecessors_of(&c),
            vec![b.clone(), a.clone()],
            "the successor's own row remembers the whole chain, nearest first, \
             with the middle hop's row gone"
        );
        reg.remove(&a).unwrap();
        assert_eq!(reg.predecessors_of(&c), vec![b, a]);
    }

    /// A succession performed on a device that never held the predecessor's
    /// row still records the link — on the successor's row, the only one here.
    #[test]
    fn a_succession_from_an_absent_row_is_still_recorded() {
        let (reg, _s) = registry();
        let new = reg.add_account(SECRET_B, None, None).unwrap();
        let absent = "ab".repeat(32);

        reg.record_succession(&absent, &new).unwrap();
        assert_eq!(reg.predecessors_of(&new), vec![absent]);
    }

    /// The statement-proven path (`fauna_client_profile::
    /// learn_inherited_predecessors`): merged, order-preserving, idempotent,
    /// never a self-link, and an unknown successor is a caller bug.
    #[test]
    fn proven_predecessors_are_recorded_on_the_successors_row() {
        let (reg, _s) = registry();
        let new = reg.add_account(SECRET_B, None, None).unwrap();
        let (near, far) = ("ab".repeat(32), "cd".repeat(32));

        reg.record_predecessors(&new, &[near.clone(), new.clone()])
            .unwrap();
        reg.record_predecessors(&new, &[near.clone(), far.clone()])
            .unwrap();
        assert_eq!(reg.predecessors_of(&new), vec![near.clone(), far]);
        assert!(matches!(
            reg.record_predecessors(&"ef".repeat(32), &[near]),
            Err(AccountError::UnknownActor(_))
        ));
    }

    /// **The launch binding follows the account** (`account-scoping.md`
    /// § Concurrent instances → *The binding follows the account*): a process
    /// bound to the retired identity is bound to the account that just moved,
    /// and this — the one seam every adopter crosses before it switches — is
    /// where it moves with it. A binding on an unrelated account, and a plain
    /// launch, are untouched. Serialized on the binding cell's test lock with
    /// `instance_lock.rs`'s own cell test.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn recording_a_succession_re_points_a_launch_binding_on_the_retired_identity() {
        let _serial = instance_lock::SESSION_LAUNCH_BINDING_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (reg, _s) = registry();
        let old = reg.add_account(SECRET_A, None, None).unwrap();
        let new = reg.add_account(SECRET_B, None, None).unwrap();
        let unrelated = reg.add_account(SECRET_C, None, None).unwrap();

        // Bound to an UNRELATED account: the succession is not this process's.
        instance_lock::set_session_launch_binding_for_tests(Some(Some(unrelated.clone())));
        reg.record_succession(&old, &new).unwrap();
        assert_eq!(
            session_launch_binding().as_deref(),
            Some(unrelated.as_str()),
            "another account's succession must not move this process's binding"
        );

        // Bound to the RETIRED identity: this process serves the account that
        // moved, so its binding moves to the successor.
        instance_lock::set_session_launch_binding_for_tests(Some(Some(old.clone())));
        reg.record_succession(&old, &new).unwrap();
        assert_eq!(
            session_launch_binding().as_deref(),
            Some(new.as_str()),
            "a bound instance's binding must follow its account to the successor — \
             left on the retired id, bound-or-refuse exits the process mid-ceremony"
        );

        // A plain launch stays plain.
        instance_lock::set_session_launch_binding_for_tests(Some(None));
        reg.record_succession(&old, &new).unwrap();
        assert_eq!(session_launch_binding(), None);

        // The rule does not depend on the predecessor's ROW surviving — only
        // on the binding naming it (the row can legitimately be gone).
        reg.remove(&old).unwrap();
        instance_lock::set_session_launch_binding_for_tests(Some(Some(old.clone())));
        reg.record_succession(&old, &new).unwrap();
        assert_eq!(session_launch_binding().as_deref(), Some(new.as_str()));
    }

    /// The forward walk: the account as it is today, from any hop of its chain.
    #[test]
    fn the_terminal_successor_is_the_end_of_the_chain_from_any_hop() {
        let (reg, _s) = registry();
        let a = reg.add_account(SECRET_A, None, None).unwrap();
        let b = reg.add_account(SECRET_B, None, None).unwrap();
        let c = reg.add_account(SECRET_C, None, None).unwrap();
        assert_eq!(reg.terminal_successor_of(&a), None, "never succeeded");
        reg.record_succession(&a, &b).unwrap();
        assert_eq!(reg.terminal_successor_of(&a).as_deref(), Some(b.as_str()));
        reg.record_succession(&b, &c).unwrap();
        assert_eq!(
            reg.terminal_successor_of(&a).as_deref(),
            Some(c.as_str()),
            "A→B→C: A's account is C today"
        );
        assert_eq!(reg.terminal_successor_of(&b).as_deref(), Some(c.as_str()));
        assert_eq!(
            reg.terminal_successor_of(&c),
            None,
            "the head has no successor"
        );
        assert_eq!(reg.terminal_successor_of("not-here"), None);
    }

    /// **A bound launch whose named id has a recorded successor binds to the
    /// terminal successor** (`account-scoping.md` § Concurrent instances → *The
    /// binding follows the account*, rider 2). The process cell moves with it,
    /// so the holder's bound-or-refuse meets the successor — and a plain launch
    /// or a never-succeeded binding is untouched. Serialized on the binding
    /// cell's test lock like every test that reads or writes it.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn resolving_the_launch_binding_follows_the_chain_to_the_successor() {
        let _serial = instance_lock::SESSION_LAUNCH_BINDING_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (reg, _s) = registry();
        let a = reg.add_account(SECRET_A, None, None).unwrap();
        let b = reg.add_account(SECRET_B, None, None).unwrap();
        let c = reg.add_account(SECRET_C, None, None).unwrap();

        // A plain launch stays plain.
        instance_lock::set_session_launch_binding_for_tests(Some(None));
        assert_eq!(reg.resolve_launch_binding(), None);
        assert_eq!(session_launch_binding(), None);

        // Bound to a live account: unchanged.
        instance_lock::set_session_launch_binding_for_tests(Some(Some(c.clone())));
        assert_eq!(reg.resolve_launch_binding().as_deref(), Some(c.as_str()));
        assert_eq!(session_launch_binding().as_deref(), Some(c.as_str()));

        // Bound to a retired id — a spawn minted before this install learned of
        // the succession, or a sibling seat's ceremony: the account is B today.
        reg.record_succession(&a, &b).unwrap();
        instance_lock::set_session_launch_binding_for_tests(Some(Some(a.clone())));
        assert_eq!(
            reg.resolve_launch_binding().as_deref(),
            Some(b.as_str()),
            "the binding names the account, and the account is the successor now"
        );
        assert_eq!(
            session_launch_binding().as_deref(),
            Some(b.as_str()),
            "the process cell moved too — the holder must meet the successor"
        );
        // Idempotent, and it walks the whole chain.
        reg.record_succession(&b, &c).unwrap();
        assert_eq!(reg.resolve_launch_binding().as_deref(), Some(c.as_str()));
        assert_eq!(reg.resolve_launch_binding().as_deref(), Some(c.as_str()));
    }

    /// The unrelated-account guard, stated as a test because it is the whole
    /// reason the link exists: a second account the user simply happens to hold
    /// must never read as a predecessor.
    #[test]
    fn an_unrelated_account_is_never_a_predecessor() {
        let (reg, _s) = registry();
        let old = reg.add_account(SECRET_A, None, None).unwrap();
        let new = reg.add_account(SECRET_B, None, None).unwrap();
        let unrelated = reg.add_account(SECRET_C, None, None).unwrap();
        reg.record_succession(&old, &new).unwrap();

        let preds = reg.predecessors_of(&new);
        assert!(
            !preds.contains(&unrelated),
            "an account that is merely also present must not read as a \
             predecessor: {preds:?}"
        );
    }

    /// A twice-succeeded chain A → B → C. The corpus can still be sealed under
    /// **A** if the B-stage re-seal never completed (the user succeeded twice in
    /// quick succession, or the first pass was interrupted), so C must be
    /// offered A as well as B — otherwise the re-seal silently reports
    /// "no key opens it" forever and the corpus stays stranded.
    ///
    /// Collapsing to the terminal successor mirrors the nest's own ratified
    /// rule (`succession_ownership::heal_at_boot` heals to the terminal
    /// successor, newest hop first), so the two sides use one concept.
    #[test]
    fn a_twice_succeeded_chain_offers_every_ancestor() {
        let (reg, _s) = registry();
        let a = reg.add_account(SECRET_A, None, None).unwrap();
        let b = reg.add_account(SECRET_B, None, None).unwrap();
        let c = reg.add_account(SECRET_C, None, None).unwrap();

        reg.record_succession(&a, &b).unwrap();
        reg.record_succession(&b, &c).unwrap();

        let mut preds = reg.predecessors_of(&c);
        preds.sort();
        let mut want = vec![a, b];
        want.sort();
        assert_eq!(
            preds, want,
            "both ancestors must be offered — a corpus stranded at the A-stage \
             seal is exactly the case a one-hop answer cannot re-key"
        );
    }

    /// Pin for `predecessors_of`'s "nearest hop first" promise: a shape with two independent chains
    /// meeting at the same successor, one longer than the other — A→B→C and
    /// F→E→D→C, so C has two DIRECT predecessors (B and D) plus deeper
    /// ancestors at hop 2 (A, E) and hop 3 (F). A depth-first walk visits one
    /// branch to its end before returning to the other and produces the
    /// non-monotonic hop sequence [1, 1, 2, 3, 2] (F's hop-3 before A's
    /// hop-2) — exactly the shape the filed review probe measured — while a
    /// genuine breadth-first walk cannot: every hop-2 entry must precede
    /// every hop-3 one.
    ///
    /// `retry_predecessor`'s actual safety property — `.next()` is always a
    /// direct predecessor — holds independent of this (both B and D land at
    /// positions 0/1 either way, since the very first frontier pop only ever
    /// pushes direct predecessors); this test pins the ordering promise the
    /// doc comments separately make, not that property.
    #[test]
    fn predecessors_of_is_genuinely_nearest_hop_first_not_just_direct_hop_first() {
        let (reg, _s) = registry();
        let a = reg.add_account(SECRET_A, None, None).unwrap();
        let b = reg.add_account(SECRET_B, None, None).unwrap();
        let c = reg.add_account(SECRET_C, None, None).unwrap();
        let d = reg.add_account(SECRET_D, None, None).unwrap();
        let e = reg.add_account(SECRET_E, None, None).unwrap();
        let f = reg.add_account(SECRET_F, None, None).unwrap();

        reg.record_succession(&a, &b).unwrap();
        reg.record_succession(&b, &c).unwrap();
        reg.record_succession(&f, &e).unwrap();
        reg.record_succession(&e, &d).unwrap();
        reg.record_succession(&d, &c).unwrap();

        let preds = reg.predecessors_of(&c);
        let pos = |actor: &str| -> usize {
            preds
                .iter()
                .position(|p| p == actor)
                .unwrap_or_else(|| panic!("{actor} missing from {preds:?}"))
        };
        // Same-hop order is unconstrained (B vs. D, A vs. E), but every entry
        // at a nearer hop must land before every entry at a farther one —
        // the whole hop-1 group before the whole hop-2 group before hop-3.
        let hop1_last = [pos(&b), pos(&d)].into_iter().max().unwrap();
        let hop2_first = [pos(&a), pos(&e)].into_iter().min().unwrap();
        let hop2_last = [pos(&a), pos(&e)].into_iter().max().unwrap();
        let hop3 = pos(&f);
        assert!(
            hop1_last < hop2_first,
            "every direct predecessor (B, D) must precede every hop-2 \
             ancestor (A, E) in a genuinely nearest-hop-first walk: {preds:?}"
        );
        assert!(
            hop2_last < hop3,
            "every hop-2 ancestor (A, E) must precede the hop-3 ancestor (F) \
             in a genuinely nearest-hop-first walk: {preds:?}"
        );
    }

    /// The shared resolution every aftermath plane consumes: predecessor rows →
    /// the owner keys their corpus is still sealed under, in the same
    /// nearest-hop-first order, and derived identically to what the seal side
    /// used (`BackupKey::derive` over the identity seed).
    /// The paired walk is the SAME walk, still carrying the id each key opens.
    ///
    /// The whole reason it exists: the aftermath's predecessor inputs
    /// need `(actor, key)`, and before this the two drivers each resolved it
    /// themselves — one of them by way of `predecessor_seeds`, handing a raw
    /// identity seed to a consumer that only ever needed to open bytes. This
    /// asserts the pairing is exact rather than merely the right length: key
    /// *i* must be the key derived from the secret of actor *i*.
    #[test]
    fn the_paired_walk_names_the_actor_each_key_opens() {
        let (reg, _s) = registry();
        let a = reg.add_account(SECRET_A, None, None).unwrap();
        let b = reg.add_account(SECRET_B, None, None).unwrap();
        let c = reg.add_account(SECRET_C, None, None).unwrap();
        reg.record_succession(&a, &b).unwrap();
        reg.record_succession(&b, &c).unwrap();

        let paired = reg.predecessor_backup_keys_by_actor(&c);
        assert_eq!(
            paired.len(),
            reg.predecessors_of(&c).len(),
            "the paired walk must offer every ancestor its key-only sibling does"
        );

        for (actor, key) in &paired {
            let hex = fauna_core::hex32::encode(&actor.0);
            let secret = reg
                .secrets(&hex)
                .unwrap_or_else(|| panic!("{hex} is not a known account"))
                .secret_hex;
            let kp = fauna_core::identity::ActorKeypair::from_secret_hex(&secret).unwrap();
            assert_eq!(
                key.convergent_chunk_root(),
                fauna_core::crypto::BackupKey::derive(kp.secret_bytes()).convergent_chunk_root(),
                "the key paired with {hex} is not the key {hex}'s own secret derives"
            );
        }
    }

    /// Order and membership agree with the key-only accessor, which is what
    /// lets the two be read as one walk rather than two policies. Both are
    /// nearest-hop-first.
    #[test]
    fn the_paired_walk_agrees_with_its_key_only_sibling() {
        let (reg, _s) = registry();
        let a = reg.add_account(SECRET_A, None, None).unwrap();
        let b = reg.add_account(SECRET_B, None, None).unwrap();
        let c = reg.add_account(SECRET_C, None, None).unwrap();
        reg.record_succession(&a, &b).unwrap();
        reg.record_succession(&b, &c).unwrap();

        let bare: Vec<[u8; 32]> = reg
            .predecessor_backup_keys(&c)
            .iter()
            .map(|k| k.convergent_chunk_root())
            .collect();
        let paired: Vec<[u8; 32]> = reg
            .predecessor_backup_keys_by_actor(&c)
            .iter()
            .map(|(_actor, k)| k.convergent_chunk_root())
            .collect();

        assert_eq!(
            bare, paired,
            "the two accessors resolve one walk — same rows, same order"
        );
    }

    /// A device that holds no secret for an ancestor skips that row in the
    /// paired walk exactly as it does in the key-only one — never an error,
    /// and never a placeholder pair naming an actor this device cannot open.
    #[test]
    fn the_paired_walk_skips_a_row_this_device_holds_no_secret_for() {
        let (reg, store) = registry();
        let a = reg.add_account(SECRET_A, None, None).unwrap();
        let b = reg.add_account(SECRET_B, None, None).unwrap();
        let c = reg.add_account(SECRET_C, None, None).unwrap();
        reg.record_succession(&a, &b).unwrap();
        reg.record_succession(&b, &c).unwrap();

        // Drop just A's secret, as a device that joined mid-chain would have.
        store.delete(&secret_key(&a));

        let paired = reg.predecessor_backup_keys_by_actor(&c);
        let named: Vec<String> = paired
            .iter()
            .map(|(x, _)| fauna_core::hex32::encode(&x.0))
            .collect();
        assert_eq!(
            named,
            vec![b.clone()],
            "only the holdable ancestor is named; the unholdable one is absent"
        );
    }

    #[test]
    fn predecessor_backup_keys_derive_the_keys_the_corpus_is_sealed_under() {
        let (reg, _s) = registry();
        let a = reg.add_account(SECRET_A, None, None).unwrap();
        let b = reg.add_account(SECRET_B, None, None).unwrap();
        let c = reg.add_account(SECRET_C, None, None).unwrap();
        reg.record_succession(&a, &b).unwrap();
        reg.record_succession(&b, &c).unwrap();

        let got: Vec<[u8; 32]> = reg
            .predecessor_backup_keys(&c)
            .iter()
            .map(|k| k.convergent_chunk_root())
            .collect();

        // Parallel to `predecessors_of`, so a consumer can zip the two — and
        // derived from the seed exactly as the sealing client did.
        let want: Vec<[u8; 32]> = reg
            .predecessors_of(&c)
            .iter()
            .map(|hex| {
                let secret = reg.secrets(hex).unwrap().secret_hex;
                let kp = fauna_core::identity::ActorKeypair::from_secret_hex(&secret).unwrap();
                fauna_core::crypto::BackupKey::derive(kp.secret_bytes()).convergent_chunk_root()
            })
            .collect();

        assert_eq!(got.len(), 2, "both ancestors resolve to material");
        assert_eq!(got, want, "same order, same derivation as the seal side");
    }

    /// The escrow-side resolution walks the **whole chain** — a grandparent's
    /// seed survives nowhere else once its own blob died in the succession
    /// transaction (`identity-succession.md` § Seed escrow), so a one-hop
    /// answer here would drop exactly the material a total-device-loss restore
    /// inside an un-drained earlier window still depends on.
    #[test]
    fn predecessor_seeds_carry_the_whole_chain_not_one_hop() {
        let (reg, _s) = registry();
        let a = reg.add_account(SECRET_A, None, None).unwrap();
        let b = reg.add_account(SECRET_B, None, None).unwrap();
        let c = reg.add_account(SECRET_C, None, None).unwrap();
        reg.record_succession(&a, &b).unwrap();
        reg.record_succession(&b, &c).unwrap();

        let seeds = reg.predecessor_seeds(&c);
        assert_eq!(seeds.len(), 2, "both ancestors' seeds resolve");
        // Parallel to `predecessors_of` (nearest hop first), carrying the raw
        // seed the escrow blob seals — not a derived key.
        for ((hex, seed), want_hex) in seeds.iter().zip(reg.predecessors_of(&c)) {
            assert_eq!(hex, &want_hex);
            let stored = reg.secrets(hex).unwrap().secret_hex;
            let kp = fauna_core::identity::ActorKeypair::from_secret_hex(&stored).unwrap();
            assert_eq!(seed, kp.secret_bytes(), "the raw seed, verbatim");
        }
    }

    /// Same skip rule as the key-side resolution: an unholdable ancestor is
    /// absent, never an error, and never blocks the ancestors this device can
    /// still back-stop.
    #[test]
    fn predecessor_seeds_skip_rows_this_device_holds_no_secret_for() {
        let (reg, store) = registry();
        let a = reg.add_account(SECRET_A, None, None).unwrap();
        let b = reg.add_account(SECRET_B, None, None).unwrap();
        let c = reg.add_account(SECRET_C, None, None).unwrap();
        reg.record_succession(&a, &b).unwrap();
        reg.record_succession(&b, &c).unwrap();
        store.delete(&secret_key(&a));

        let seeds = reg.predecessor_seeds(&c);
        assert_eq!(seeds.len(), 1);
        assert_eq!(seeds[0].0, b, "the holdable ancestor, nearest first");
    }

    /// An identity that never succeeded pays nothing — the empty answer is what
    /// keeps every consuming plane free in the overwhelmingly common case.
    #[test]
    fn an_identity_that_never_succeeded_has_no_predecessor_keys() {
        let (reg, _s) = registry();
        let a = reg.add_account(SECRET_A, None, None).unwrap();
        assert!(reg.predecessor_backup_keys(&a).is_empty());
    }

    /// A predecessor row this device holds no secret for is **skipped, not an
    /// error** — the ordinary state on a device the user did not succeed from.
    /// Refusing there would take down the whole read path for the ancestors this
    /// device *can* open; reporting it as material would be worse.
    #[test]
    fn a_predecessor_whose_secret_this_device_lacks_is_skipped_not_refused() {
        let (reg, store) = registry();
        let a = reg.add_account(SECRET_A, None, None).unwrap();
        let b = reg.add_account(SECRET_B, None, None).unwrap();
        let c = reg.add_account(SECRET_C, None, None).unwrap();
        reg.record_succession(&a, &b).unwrap();
        reg.record_succession(&b, &c).unwrap();

        // Drop just A's secret, as a device that joined mid-chain would have.
        store.delete(&secret_key(&a));

        let keys = reg.predecessor_backup_keys(&c);
        assert_eq!(
            keys.len(),
            1,
            "the holdable ancestor still resolves; the unholdable one is absent"
        );
        let b_secret = reg.secrets(&b).unwrap().secret_hex;
        let b_kp = fauna_core::identity::ActorKeypair::from_secret_hex(&b_secret).unwrap();
        assert_eq!(
            keys[0].convergent_chunk_root(),
            fauna_core::crypto::BackupKey::derive(b_kp.secret_bytes()).convergent_chunk_root(),
        );
    }

    #[test]
    fn recording_a_succession_is_idempotent() {
        let (reg, _s) = registry();
        let old = reg.add_account(SECRET_A, None, None).unwrap();
        let new = reg.add_account(SECRET_B, None, None).unwrap();

        reg.record_succession(&old, &new).unwrap();
        reg.record_succession(&old, &new).unwrap();

        assert_eq!(
            reg.predecessors_of(&new),
            vec![old],
            "re-running the ceremony's persist must not duplicate the link"
        );
    }

    /// The link is at-rest client state, so it obeys the additive rule: an
    /// entry without the field still loads, with no predecessors.
    #[test]
    fn an_entry_without_the_field_still_loads() {
        let (reg, store) = registry();
        let a = reg.add_account(SECRET_A, None, None).unwrap();
        store.set(
            INDEX_KEY,
            &format!(r#"{{"active":"{a}","accounts":[{{"actor_id":"{a}"}}]}}"#),
        );
        assert!(
            reg.predecessors_of(&a).is_empty(),
            "a field-less entry reads as no predecessors, not as an error"
        );
    }

    /// The supervision-snapshot slot is per-actor and opaque: two accounts on
    /// one box keep separate floors, and the registry stores whatever the
    /// domain crate hands it (`family-safety.md` § Content policy clause 2).
    #[test]
    fn the_supervision_snapshot_slot_is_per_account_and_opaque() {
        let (reg, _s) = registry();
        let a = reg.add_account(SECRET_A, None, None).unwrap();
        let b = reg.add_account(SECRET_B, None, None).unwrap();

        assert_eq!(
            reg.supervision_snapshot_json(&a),
            None,
            "no snapshot until this account completes one successful read"
        );

        reg.set_supervision_snapshot_json(&a, r#"{"content_notify":true}"#);
        assert_eq!(
            reg.supervision_snapshot_json(&a).as_deref(),
            Some(r#"{"content_notify":true}"#),
            "stored verbatim — the registry never parses this slot"
        );
        assert_eq!(
            reg.supervision_snapshot_json(&b),
            None,
            "a ward's floor must not bind on the sibling account beside them"
        );

        reg.clear_supervision_snapshot(&a);
        assert_eq!(reg.supervision_snapshot_json(&a), None);
    }

    /// Clause 2's "erased with the account's other stores". A snapshot that
    /// outlived its identity would let a removed ward's floor bind on whoever
    /// next signs in on the box.
    #[test]
    fn removing_an_account_sweeps_its_supervision_snapshot() {
        let (reg, _s) = registry();
        let a = reg.add_account(SECRET_A, None, None).unwrap();
        let b = reg.add_account(SECRET_B, None, None).unwrap();
        reg.set_supervision_snapshot_json(&a, r#"{"content_notify":true}"#);
        reg.set_supervision_snapshot_json(&b, r#"{"content_notify":false}"#);

        reg.remove(&a).unwrap();
        assert_eq!(
            reg.supervision_snapshot_json(&a),
            None,
            "the removed identity's floor dies with it"
        );
        assert!(
            reg.supervision_snapshot_json(&b).is_some(),
            "and only that identity's — the remaining account keeps its own"
        );
    }

    /// The factory-reset path: `clear_all` sweeps every account's snapshot,
    /// same rule, enumerated from the index like the slots beside it.
    #[test]
    fn clear_all_sweeps_every_supervision_snapshot() {
        let (reg, _s) = registry();
        let a = reg.add_account(SECRET_A, None, None).unwrap();
        let b = reg.add_account(SECRET_B, None, None).unwrap();
        reg.set_supervision_snapshot_json(&a, r#"{"content_notify":true}"#);
        reg.set_supervision_snapshot_json(&b, r#"{"content_notify":true}"#);

        let _ = reg.clear_all();
        assert_eq!(reg.supervision_snapshot_json(&a), None);
        assert_eq!(reg.supervision_snapshot_json(&b), None);
    }

    /// `fauna-sync-engine::principal_bundle` writes four more per-actor slots
    /// directly against this same store (`<actor_id>/device-auth`,
    /// `<actor_id>/backup-key`, `<actor_id>/generation-keys`,
    /// `<actor_id>/grant-registered` — deliberately WITHOUT the `fauna/`
    /// prefix every other per-actor slot uses, since `principal_bundle::attr`
    /// builds its own key shape independently of this module's builders).
    /// `fauna-sync-engine` depends on `fauna-client-accounts`, never the
    /// reverse, so these four cannot be swept by importing its constants —
    /// they must be named here too, literally, or `clear_all`/`remove` leave
    /// a signed-out (or factory-reset) user's device authorization and backup
    /// key readable on disk (windows e2e `test_sign_out_erases_the_credential_namespace`
    /// and `test_smoke_g_factory_reset_leaves_no_identity_for_the_next_launch`
    /// reproduced exactly this survival). Same class of miss as row 458
    /// (`reach_ipv4`) and row 235 (`supervision_snapshot`) — a slot minted
    /// outside this file's own setters is invisible to `per_actor_keys`
    /// unless someone remembers to add it by hand.
    #[test]
    fn clear_all_sweeps_the_principal_bundle_attributes() {
        let (reg, store) = registry();
        let a = reg.add_account(SECRET_A, None, None).unwrap();
        let b = reg.add_account(SECRET_B, None, None).unwrap();
        for actor in [&a, &b] {
            for suffix in [
                "device-auth",
                "backup-key",
                "generation-keys",
                "grant-registered",
                "enrollment-refused",
            ] {
                store.set(&format!("{actor}/{suffix}"), "dummy");
            }
        }

        let _ = reg.clear_all();

        for actor in [&a, &b] {
            for suffix in [
                "device-auth",
                "backup-key",
                "generation-keys",
                "grant-registered",
                "enrollment-refused",
            ] {
                let key = format!("{actor}/{suffix}");
                assert_eq!(store.get(&key), None, "{key} survived clear_all");
            }
        }
    }

    /// `remove` and `clear_all` reach an AUXILIARY store — the registry-side
    /// half of the fix for the split that let every per-actor slot survive a
    /// production sign-out (`long-term-store.md` § Implementation status
    /// today, hole 3). `retire_superseded_provisionals` is a third cleanup
    /// site, but it shares this exact `delete_per_actor_everywhere` sweep
    /// rather than a sweep of its own, so pinning the mechanism here covers
    /// all three by construction; its own guard (`is_provisional`, which
    /// decides what to erase rather than erasing) needed a separate,
    /// namespace-widening fix — apps row 540, pinned in
    /// `launch_persistence.rs`'s retirement tests.
    ///
    /// The two tests above pin the key *strings*, and that is all they can pin:
    /// they write their fixtures through the registry's own store handle, so
    /// there is only one store by construction and a sweep aimed at the wrong
    /// one still looks green. Here the fixture lives in a store the registry
    /// does **not** index, exactly as `fauna-account-store` does in a shipping
    /// build. (Whether the *production* aux store is the right one is
    /// `fauna-credential-store`'s to pin — it owns the namespace and the
    /// constructor; this asserts the mechanism it rides on.)
    #[test]
    fn both_erases_sweep_the_auxiliary_stores() {
        let aux = Arc::new(InMemorySecretStore::new());
        let (plain, store) = registry();
        let reg = plain.also_erasing(vec![aux.clone() as Arc<dyn SecretStore>]);
        let a = reg.add_account(SECRET_A, None, None).unwrap();
        let b = reg.add_account(SECRET_B, None, None).unwrap();

        // Every builder shape, in the store the registry never reads.
        let seed = |actor: &str| {
            for key in per_actor_keys(actor) {
                aux.set(&key, "dummy");
            }
        };
        let survivors = |actor: &str| -> Vec<String> {
            per_actor_keys(actor)
                .filter(|k| aux.get(k).is_some())
                .collect()
        };

        seed(&a);
        seed(&b);
        reg.remove(&a).expect("A is known");
        assert!(
            survivors(&a).is_empty(),
            "remove left {:?} in the auxiliary store",
            survivors(&a)
        );
        assert!(
            !survivors(&b).is_empty(),
            "remove swept an actor it was not asked about"
        );

        let _ = reg.clear_all();
        assert!(
            survivors(&b).is_empty(),
            "clear_all left {:?} in the auxiliary store",
            survivors(&b)
        );
        // …and the registry's own store is still erased, not merely redirected.
        assert_eq!(store.get(&format!("fauna/{b}/secret")), None);
    }

    /// No auxiliary store is a real answer, not an omission: web's registry has
    /// no account runtime behind it, so it has no second namespace to sweep.
    /// The default must therefore stay a plain, working erase.
    #[test]
    fn an_empty_auxiliary_list_erases_exactly_as_before() {
        let (reg, store) = registry();
        let a = reg.add_account(SECRET_A, None, None).unwrap();
        store.set(&format!("{a}/device-auth"), "dummy");

        let _ = reg.clear_all();

        assert_eq!(store.get(&format!("fauna/{a}/secret")), None);
        assert_eq!(store.get(&format!("{a}/device-auth")), None);
    }

    /// The FIFTH slot `fauna-sync-engine` writes against this same store, and
    /// the one the four above missed: the **T10 store writer signing key**, at
    /// the BARE `{actor_id}` key — no `fauna/` prefix AND no suffix at all
    /// (`account_runtime.rs` § The writer key and the T10 slot: "account
    /// attribute = the actor id hex"). It holds a real Ed25519 secret, so
    /// `long-term-store.md` § Cleanup contract property 3 — no credential
    /// *derived* from the identity outlives it — puts it squarely in the
    /// erase. It was the single survivor left after the four suffixed slots
    /// were swept, still failing the two cross-app witnesses
    /// (`test_sign_out_erases_the_credential_namespace`,
    /// `test_smoke_g_factory_reset_leaves_no_identity_for_the_next_launch`).
    ///
    /// Two Rust sites write this one key, and they are the SAME slot with the
    /// same meaning, not a collision: `account_runtime::writer_key_from_slot`
    /// mints it on an account's first assembly on a machine, and
    /// `principal_succession::mint_into_slot` **re-keys it in place** on the
    /// two rotation triggers (revocation succession, and refinement 10's
    /// lost-slot heal). Erasing it is safe for a store dir that outlives the
    /// erase precisely because of that second trigger: an empty slot over a
    /// stamped store self-heals — the assembly mints and fences, re-authoring
    /// the un-pushed tail, rather than stranding the replica.
    #[test]
    fn clear_all_and_remove_sweep_the_bare_actor_id_writer_key_slot() {
        let (reg, store) = registry();
        let a = reg.add_account(SECRET_A, None, None).unwrap();
        let b = reg.add_account(SECRET_B, None, None).unwrap();
        for actor in [&a, &b] {
            store.set(
                actor,
                "d0dbeef00000000000000000000000000000000000000000000000000000dead",
            );
        }

        reg.remove(&a).unwrap();
        assert_eq!(
            store.get(&a),
            None,
            "the removed identity's writer signing key survived remove"
        );
        assert!(
            store.get(&b).is_some(),
            "and only that identity's — the remaining account keeps its own writer key"
        );

        let _ = reg.clear_all();
        assert_eq!(
            store.get(&b),
            None,
            "the writer signing key survived clear_all (sign-out / factory reset)"
        );
    }

    /// One row of [`every_raw_per_actor_setter_writes_nothing_once_the_account_is_gone`]'s
    /// table: the setter's name for the assertion message, the setter itself,
    /// and the store key its write would land in.
    type RawSetterCase = (
        &'static str,
        fn(&AccountRegistry, &str, &str),
        fn(&str) -> String,
    );

    /// Every raw per-actor writer shares
    /// one guard, [`AccountRegistry::is_live`] — a read/response landing
    /// **after** a factory reset or sign-out (`clear_all`) already wiped the
    /// account must resurrect nothing it wrote. One table, one arm per
    /// setter, rather than six near-identical copies of
    /// `set_supervision_snapshot_writes_nothing_once_the_account_is_gone`
    /// (that slot alone survived a factory reset on macOS for
    /// exactly this race before it got its own guard — the shape every
    /// sibling setter now shares). `set_active`/`set_active_confirmed`/
    /// `set_require_confirm` are NOT in this table: they already refuse an
    /// unknown actor via `writable_index()` + `idx.position()` returning
    /// `AccountError::UnknownActor`, a *different*, pre-existing mechanism —
    /// see `activation_refuses_an_actor_thats_gone` below for their own
    /// post-wipe coverage.
    #[test]
    fn every_raw_per_actor_setter_writes_nothing_once_the_account_is_gone() {
        let cases: &[RawSetterCase] = &[
            ("set_nest_url", AccountRegistry::set_nest_url, nest_url_key),
            (
                "set_pending_invite_json",
                AccountRegistry::set_pending_invite_json,
                pending_invite_key,
            ),
            (
                "set_awaiting_dns_json",
                AccountRegistry::set_awaiting_dns_json,
                awaiting_dns_key,
            ),
            (
                "set_reach_ipv4",
                AccountRegistry::set_reach_ipv4,
                reach_ipv4_key,
            ),
            (
                "set_pending_factory_reset_json",
                AccountRegistry::set_pending_factory_reset_json,
                pending_factory_reset_key,
            ),
            (
                "park_aftermath_ceremony",
                AccountRegistry::park_aftermath_ceremony,
                pending_aftermath_ceremony_key,
            ),
            (
                "set_supervision_snapshot_json",
                AccountRegistry::set_supervision_snapshot_json,
                supervision_snapshot_key,
            ),
        ];

        for (name, setter, key_fn) in cases {
            let (reg, store) = registry();
            let a = reg.add_account(SECRET_A, None, None).unwrap();

            let _ = reg.clear_all(); // factory reset / sign-out

            setter(&reg, &a, "value-that-must-not-land");

            assert_eq!(
                store.get(&key_fn(&a)),
                None,
                "{name}: a post-wipe write must not resurrect the erased identity's slot"
            );
        }
    }

    /// `set_active`/`set_active_confirmed`/`set_require_confirm` are the
    /// three per-actor mutators NOT in the `is_live` table above: they
    /// already refuse an actor the index doesn't know, via
    /// `writable_index()` + `idx.position()` → `AccountError::UnknownActor`
    /// (`Self::activate`, `Self::set_require_confirm`) — a mechanism that
    /// predates and is independent of `is_live`. This pins that the
    /// post-wipe refusal still holds, so the family's coverage is complete
    /// without adding a second, redundant guard to functions that already
    /// have one.
    #[test]
    fn activation_refuses_an_actor_thats_gone() {
        let (reg, _s) = registry();
        let a = reg.add_account(SECRET_A, None, None).unwrap();

        let _ = reg.clear_all(); // factory reset / sign-out

        assert!(matches!(
            reg.set_active(&a),
            Err(AccountError::UnknownActor(_))
        ));
        assert!(matches!(
            reg.set_active_confirmed(&a),
            Err(AccountError::UnknownActor(_))
        ));
        assert!(matches!(
            reg.set_require_confirm(&a, true),
            Err(AccountError::UnknownActor(_))
        ));
    }
}
