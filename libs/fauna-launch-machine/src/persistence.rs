//! Long-term store contract.
//!
//! Clients implement `LaunchPersistence` against their platform store
//! (Keychain on Apple, libsecret on Linux, `EncryptedSharedPreferences`
//! on Android, Credential Manager on Windows, `localStorage` on web). The
//! LaunchMachine reads from and writes to the trait synchronously.
//!
//! Pre-load model: clients load their store values into memory BEFORE
//! calling `LaunchMachine::start()`, so the trait methods don't need to
//! be async. See `docs/goal/behavior/onboarding.md` § Long-term store contract
//! for the per-platform field naming conventions.

use serde::{Deserialize, Serialize};

/// One row in the pending-invite slot. Field names match
/// `docs/goal/behavior/onboarding.md` § Long-term store contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct PendingInviteRecord {
    pub nest_url: String,
    pub handle: String,
    pub request_id: String,
    /// Opaque to the launch machine. Carried through to the wizard's
    /// `seed_pending_invite` when the launch flow lands on `InviteRequest`.
    pub status_json: String,
}

/// One row in the awaiting-manual-dns slot — the deferred-DNS analogue of
/// [`PendingInviteRecord`]. Field names match
/// `docs/goal/behavior/onboarding.md` § Long-term store contract.
///
/// `handle` is **required**: the eventual `LoggedIn` outcome carries it and it
/// is not derivable from `nest_url` alone, and the wizard's
/// `seed_awaiting_manual_dns(nest_url, handle, dns_records, claim_code)` seeder
/// takes it. (Every pre-2026-07-11 per-app slot omitted it and so could not
/// satisfy that seeder — see the goal doc's § Implementation status today.)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct AwaitingDnsRecord {
    pub nest_url: String,
    pub handle: String,
    /// Opaque to the launch machine: `serde_json::to_string(&dns_records)` over
    /// the wizard's `Vec<DnsRecordPlain>`. Carried through to the wizard's
    /// `seed_awaiting_manual_dns` when the launch flow lands on
    /// `AwaitingManualDns`, exactly as `status_json` is for the invite slot.
    pub dns_records_json: String,
    pub claim_code: String,
    /// The box's public IPv4 — the **reach address** every link of the
    /// post-build chain dials while the domain's A record is still
    /// propagating and the box holds no cert for a name it learns at the
    /// claim (`docs/goal/behavior/onboarding.md` § 6 *Reaching the box*).
    ///
    /// `None` in three honest cases, and a reader must handle all three: a
    /// record written before this field existed (`#[serde(default)]`), the
    /// deferred-DNS exit's own record (that box is reached by the domain the
    /// user is about to point at it), and the pending-provision slot in the
    /// window between its pre-`create_server` write and the moment
    /// `create_server` returns — which is precisely the window where the box
    /// does not exist yet, so there is no address to hold.
    #[serde(default)]
    #[cfg_attr(feature = "uniffi", uniffi(default = None))]
    pub reach_ipv4: Option<String>,
    /// The identity the box was **built with** — the `nest_actor_id` (64 hex)
    /// derived from the deployment seed this client injected into its
    /// cloud-init — and therefore the first-contact root every pre-identity
    /// dial of this box verifies against (`docs/goal/architecture/security.md`
    /// § Transport trust, the *Client-provisioned box* row).
    ///
    /// Persisted so the root survives what the machine's memory does not: a
    /// Retry after a failed run reuses it instead of expecting a freshly-minted
    /// identity of a box that was built with the old one, and a relaunch onto
    /// the "Almost ready" surface re-holds it before its first poll
    /// (`onboarding.md` § 6 *The pending-provision slot*). It is the derived
    /// **public** key only — the seed itself is never persisted pre-claim
    /// (`nest/box-recovery.md` § Mechanism); a box that has to be re-created
    /// gets a fresh seed, and this field is rewritten with it.
    ///
    /// `None` (`#[serde(default)]`) for a slot whose
    /// box was found already at the provider without this client ever having
    /// built it — a reader with no identity to hold falls back to the
    /// DNS-`self=`/TOFU ladder.
    #[serde(default)]
    #[cfg_attr(feature = "uniffi", uniffi(default = None))]
    pub nest_actor_id: Option<String>,
}

/// One row in the pending-factory-reset slot — the crash-atomic decision point
/// for `fauna.admin.factory_reset` (gap CR-1,
/// `docs/goal/architecture/nest/common.md` § Client-state recoverability).
///
/// The post-reset claim code used to exist *only* in the synchronous
/// `FactoryResetReply`, so a client SIGKILL'd between dispatch and reply-render
/// lost it with no client able to learn it — the box landed at the recovery
/// floor (fresh/unclaimed) but un-claimable. The client therefore mints the code
/// and writes this row **before** dispatching, then pins it via
/// `FactoryResetRequest.new_claim_code`; a relaunch resumes the pre-filled claim
/// from the slot. Write it only through
/// [`mint_and_persist_pending_factory_reset`], which makes the wrong ordering
/// unrepresentable: the code cannot be obtained without having been persisted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct PendingFactoryResetRecord {
    pub nest_url: String,
    pub handle: String,
    /// The code the client minted and pinned onto the reset request. The nest
    /// honors a pinned code verbatim (trimmed), so this is the code the wiped
    /// box will boot with.
    pub claim_code: String,
    /// Unix seconds at mint time. The boot reconcile's `Claimed` arm reads a
    /// probe within [`FACTORY_RESET_CLAIM_GRACE_SECS`] of this as
    /// "reset in flight" (the box has staged the wipe but not yet executed it —
    /// it still answers `Claimed` for a moment after the dispatch) and HONORS
    /// the slot instead of clearing it; without this the machine's own CR-2
    /// stale-slot reconcile destroys the freshly-minted code in that window —
    /// CR-1 data loss through CR-2's door (measured on web, 2026-07-16).
    /// Required: the one writer ([`mint_and_persist_pending_factory_reset`])
    /// always stamps it, and a record without it does not parse (the adapter
    /// reads an unparseable slot as no slot).
    pub minted_at_secs: u64,
}

/// How long after minting a `Claimed` probe is read as "reset in flight"
/// (honor the slot) rather than "the dispatch failed for good" (clear it —
/// the CR-2 stale-slot reconcile). The reset-to-wipe window is seconds on a
/// local box and at most a reboot on a VPS; the cost of a too-long grace is
/// only that a *permanently failed* reset keeps routing launches to the
/// pre-filled claim page (which shows a visible "already claimed" error) for
/// this long before the reconcile reclaims the launch path.
pub const FACTORY_RESET_CLAIM_GRACE_SECS: u64 = 15 * 60;

/// Mint the post-reset claim code and durably persist it **before** the caller
/// dispatches `fauna.admin.factory_reset` — the single atomic decision point
/// that closes gap CR-1 (`docs/goal/architecture/nest/common.md`
/// § Client-state recoverability).
///
/// Returning the code only *after* the store write is what makes the crash-unsafe
/// ordering unrepresentable: a caller cannot hold a code it has not already
/// persisted. Pin the returned code via `AdminClient::factory_reset(Some(code))`;
/// if the client dies anywhere after this call, the relaunch finds the slot and
/// resumes the claim ([`LaunchWizardEntry::PendingFactoryReset`]).
///
/// The code format is `fauna_core::claim_code` (8 chars / 40 bits, grouped),
/// shared with the nest's own minting path, so a client-minted and a nest-minted
/// code are byte-identical.
///
/// [`LaunchWizardEntry::PendingFactoryReset`]: crate::LaunchWizardEntry::PendingFactoryReset
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn mint_and_persist_pending_factory_reset(
    store: &dyn LaunchPersistence,
    nest_url: String,
    handle: String,
) -> Option<String> {
    let claim_code = fauna_provisioning::generate_claim_code();
    store.save_pending_factory_reset(PendingFactoryResetRecord {
        nest_url,
        handle,
        claim_code: claim_code.clone(),
        minted_at_secs: crate::launch_clock::now_millis_or_zero() / 1000,
    });

    // Read the row back before handing out the code. The write above cannot
    // report failure — `SecretStore::set` is infallible on every platform (a
    // locked libsecret collection, a Keychain denial, a full disk are all
    // swallowed), and the wasm shim likewise drops a JS `QuotaExceededError` on
    // the floor. So "saved" is a claim to VERIFY, not to trust: a silently-lost
    // write would hand back a code the caller then pins onto a real factory
    // reset, wiping the box against a code that exists nowhere — CR-1 again, and
    // worse, because the client would believe it was safe.
    //
    // `None` means the caller MUST NOT dispatch the reset. Leave no half-written
    // row behind on the way out.
    match store.load_pending_factory_reset() {
        Some(back) if back.claim_code == claim_code => Some(claim_code),
        _ => {
            store.delete_pending_factory_reset();
            None
        }
    }
}

/// Writer for the **pending-provision slot** — the onboarding wizard's one
/// durable side effect outside its own state
/// (`docs/goal/behavior/onboarding.md` § 6 *The pending-provision slot*).
///
/// Separate from [`LaunchPersistence`] rather than a method on it, because the
/// contract is genuinely different in the one way that matters: every
/// `LaunchPersistence` write is addressed by *the session's* account (the bound
/// one, or whoever is active), and during the first-run wizard there may not be
/// one yet. This write is addressed by **the identity being onboarded**, which
/// the wizard knows and the session does not — so the secret comes in as an
/// argument and the implementation registers the account if it has to.
///
/// One implementation (`fauna_client_accounts::persist_awaiting_dns`, over the
/// per-actor registry slot) serves all seven apps; nothing here is per-app glue.
#[cfg_attr(feature = "uniffi", uniffi::export(with_foreign))]
pub trait PendingProvisionStore: Send + Sync {
    /// Write (or **complete** — see the note on the shared implementation) the
    /// awaiting-dns slot for `secret_hex`'s identity, registering it as an
    /// account first if it is not one yet, and return the record **read back**
    /// from the store.
    ///
    /// The read-back is the whole contract, and it is not defensive tidying:
    /// the platform stores swallow failures (`SecretStore::set` is infallible
    /// by signature on every platform, and the wasm shim drops a JS
    /// `QuotaExceededError` on the floor), so "saved" is a claim to VERIFY.
    /// Return `None` when the row cannot be read back — the caller must then
    /// treat the code as unpersisted and refuse to build a box with it.
    fn save_awaiting_dns(
        &self,
        secret_hex: String,
        record: AwaitingDnsRecord,
    ) -> Option<AwaitingDnsRecord>;

    /// Clear the awaiting-dns slot of `secret_hex`'s identity — the durable half
    /// of the "Almost ready" surface's explicit exit (`onboarding-provisioning.md`
    /// § "Almost ready" surface → *Exit*).
    ///
    /// Addressed by the identity being onboarded, exactly as
    /// [`Self::save_awaiting_dns`] is, and for the same reason: on an append run
    /// the active account is a different one, and clearing *its* slot would leave
    /// the abandoned box's slot to route every relaunch back onto the surface.
    /// A no-op when the identity is not registered (no slot can exist) or holds
    /// no slot.
    fn clear_awaiting_dns(&self, secret_hex: String);
}

/// Write the pending-provision slot and prove it stuck — the one write every
/// member of this family goes through.
///
/// Returns the record **read back** from the store, and only when its
/// `claim_code` is the one that was written: the platform stores swallow
/// failures (`SecretStore::set` is infallible by signature on every platform,
/// and the wasm shim drops a JS `QuotaExceededError` on the floor), so "saved"
/// is a claim to verify, never to assume. `None` means the row is not durably
/// there — a caller about to build a box with `record.claim_code` MUST NOT.
///
/// Three callers, one discipline (**custody precedes dispatch** — CR-1,
/// `docs/goal/architecture/nest/common.md` § Client-state recoverability):
/// [`mint_and_persist_pending_provision`] for a run that mints a fresh code,
/// the Retry path that re-persists a slot it already holds before it re-runs
/// (`onboarding.md` § 6 *The pending-provision slot* — a retry must never
/// overwrite the code the box was built with), and
/// [`complete_pending_provision_reach`] once the box exists.
pub fn persist_pending_provision(
    store: &dyn PendingProvisionStore,
    secret_hex: String,
    record: AwaitingDnsRecord,
) -> Option<AwaitingDnsRecord> {
    let claim_code = record.claim_code.clone();
    store
        .save_awaiting_dns(secret_hex, record)
        .filter(|back| back.claim_code == claim_code)
}

/// Mint the box's claim code and durably persist the pending-provision slot
/// **before** the caller calls `create_server` — the provisioning twin of
/// [`mint_and_persist_pending_factory_reset`], and the same discipline for the
/// same reason: **custody precedes dispatch.**
///
/// The claim code exists only in the client until the box is claimed, so a
/// client that dies between minting it and claiming would orphan a box nobody
/// can claim and a bill nobody can stop from the app — the shape
/// `docs/goal/architecture/nest/common.md` § Client-state recoverability
/// forbids. Returning the code only *after* the store write makes the
/// crash-unsafe ordering unrepresentable: a caller cannot put a code into
/// cloud-init that it has not already persisted.
///
/// `nest_actor_id` is the identity the caller is about to inject beside the
/// code (the seed's derived public key, hex) — persisted with it so a later
/// Retry or relaunch expects the identity the box was actually built with
/// (`AwaitingDnsRecord::nest_actor_id`).
///
/// `claim_code` is `None` in production — this fn mints one, which is the
/// whole point of its name. A **test** that must know the code before the run
/// starts (a tier_3 journey pointing the run at a real nest that already booted
/// with a claim code of its own) passes `Some`, and that is the *only* thing it
/// changes: the persist, the read-back and the refusal-on-swallow below are
/// byte-identical either way, so the discipline this fn exists to enforce is
/// still exercised rather than stepped around. Production reaches this only
/// through `OnboardingMachine`, which passes `None` unless
/// `set_provision_claim_code_for_test` was called.
///
/// `reach_ipv4` is deliberately `None` here and completed by
/// [`complete_pending_provision_reach`] the moment `create_server` returns:
/// this write happens in the window where the box does not exist yet, so there
/// is no address to hold. `dns_records_json` is empty for the same reason —
/// on the deferred path the records arrive at the `AwaitingManualDns` exit,
/// which *completes* this row rather than replacing it.
///
/// `None` means the caller MUST NOT provision. Unlike its factory-reset twin
/// this does not delete a half-written row on the way out, and deliberately so:
/// a `None` from the store means nothing stuck, and the one case that *could*
/// leave a row behind — a read-back carrying a different code, i.e. a
/// concurrent writer — leaves a row that is still internally consistent and
/// lands the user on the "Almost ready" surface for a box that may not exist.
/// That is the same state a failed `create_server` produces, which the surface's
/// exit affordance already owns (`onboarding.md` § "Almost ready" surface,
/// *Exit*) — so it needs no `delete` on this trait.
pub fn mint_and_persist_pending_provision(
    store: &dyn PendingProvisionStore,
    secret_hex: String,
    nest_url: String,
    handle: String,
    nest_actor_id: Option<String>,
    claim_code: Option<String>,
) -> Option<String> {
    let claim_code = claim_code.unwrap_or_else(fauna_provisioning::generate_claim_code);
    persist_pending_provision(
        store,
        secret_hex,
        AwaitingDnsRecord {
            nest_url,
            handle,
            dns_records_json: String::new(),
            claim_code,
            reach_ipv4: None,
            nest_actor_id,
        },
    )
    .map(|back| back.claim_code)
}

/// Complete the pending-provision slot with the box's reach address, the moment
/// the Server step settles (`onboarding.md` § 6 — *"then completes it with the
/// box's `reach_ipv4` the moment `create_server` returns"*).
///
/// Takes the same `(nest_url, handle, claim_code)` the mint wrote, so the row is
/// rewritten whole rather than read-modify-written: the caller minted them and
/// still holds them, and a read-modify-write would have to decide what to do
/// about a row that changed underneath it. `nest_actor_id` is the identity the
/// box **boots with** — this run's injected identity when the box was created
/// now, the retained one when the Server step found a box an earlier run
/// built — so the completion is also what corrects the identity on a retry
/// that had to re-create the box. Returns whether the completed row read
/// back — a `false` costs the crash-resume its address (the surface would
/// have to wait for DNS) but never the claim code, so unlike the mint it is
/// not grounds to refuse the run.
pub fn complete_pending_provision_reach(
    store: &dyn PendingProvisionStore,
    secret_hex: String,
    nest_url: String,
    handle: String,
    claim_code: String,
    nest_actor_id: Option<String>,
    reach_ipv4: String,
) -> bool {
    persist_pending_provision(
        store,
        secret_hex,
        AwaitingDnsRecord {
            nest_url,
            handle,
            dns_records_json: String::new(),
            claim_code,
            reach_ipv4: Some(reach_ipv4.clone()),
            nest_actor_id,
        },
    )
    .is_some_and(|back| back.reach_ipv4.as_deref() == Some(reach_ipv4.as_str()))
}

/// Why this build cannot read the account index at `fauna/index`, when it is
/// present but unusable — the two verdicts `fauna-client-accounts` draws, in
/// the shape the launch seam and the seven apps consume
/// (`version-compatibility.md` § 5 item 9; owner of the verdicts themselves is
/// that crate's `AccountError`).
///
/// The distinction is the whole point: the two have **opposite remedies**, so
/// an app that collapses them tells half its users to do something that cannot
/// work. Neither ever licenses a rewrite — a blob nothing here can parse is
/// not thereby known to name no accounts (I1) — so this type carries no
/// "repair" arm and never will.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum AccountIndexRefusal {
    /// A newer build wrote the index and said so in a stamp this build could
    /// read. **The accounts are intact**; updating the app brings them back,
    /// and nothing else will. The numbers are the ones actually read off the
    /// blob, never invented.
    NewerBuild {
        index_v: u16,
        index_min: u16,
        bin_v: u16,
    },
    /// The index is present, unparseable, and carries no readable stamp
    /// either — so nothing about a newer build explains it and **updating
    /// cannot help**. The only route forward is the documented client-side
    /// floor (`long-term-store.md` § Cleanup contract), whose own residual for
    /// a corrupt index an app must state rather than imply.
    Malformed,
}

/// Long-term store interface.
#[cfg_attr(feature = "uniffi", uniffi::export(with_foreign))]
pub trait LaunchPersistence: Send + Sync {
    /// The account index is present but this build cannot use it
    /// (`onboarding.md` § App-launch routing — the "present but unreadable"
    /// row). Read **before every other row**, for the same reason the
    /// pending-factory-reset row is: on either verdict the registry answers no
    /// session account, so every row below this one reads the install as having
    /// **no identity** and routes the user into fresh onboarding — with their
    /// accounts sitting intact behind a blob this build merely cannot parse.
    ///
    /// No default: `uniffi::export`'d trait methods cannot have one. Every
    /// implementor is Rust-side today (no app implements this trait — each
    /// receives the shared `RegistryLaunchPersistence` from
    /// `FfiAccountRegistry::launch_persistence`), so an implementation with no
    /// account index answers `None` in one line.
    fn account_index_refusal(&self) -> Option<AccountIndexRefusal>;
    /// 32-byte Ed25519 secret if an identity has been imported or generated.
    fn load_identity(&self) -> Option<Vec<u8>>;
    /// Cached nest URL from a prior successful authentication.
    fn load_nest_url(&self) -> Option<String>;
    /// Pending-invite slot. At most one outstanding invite per identity.
    fn load_pending_invite(&self) -> Option<PendingInviteRecord>;
    /// Awaiting-manual-dns slot. At most one deferred-DNS nest per identity.
    ///
    /// Read **before** `load_nest_url`'s silent-challenge row (see
    /// [`super::LaunchMachine::start`]): while DNS is still pending the nest is
    /// unreachable by definition, so a silent challenge against a saved
    /// `nest_url` would only fail through to the `launch_retry` surface.
    ///
    /// The client clears the slot at the wizard's `LoggedIn` terminal — inside
    /// the shared `persist_logged_in` moment, never at the claim itself
    /// (`onboarding.md` § Long-term store contract, ratified 2026-09-21) —
    /// deletion is a store-side concern (the machine never writes it), so
    /// there is no `delete_awaiting_dns` on this trait.
    fn load_awaiting_dns(&self) -> Option<AwaitingDnsRecord>;
    /// Pending-factory-reset slot. At most one outstanding reset per identity.
    ///
    /// Read **before** every other row (see [`super::LaunchMachine::start`]): the
    /// box this identity last authenticated against has just been wiped to
    /// fresh/unclaimed, so a silent challenge against the saved `nest_url` would
    /// only fail through to the `launch_retry` surface, and the claim the user
    /// must complete is the one this row pins.
    fn load_pending_factory_reset(&self) -> Option<PendingFactoryResetRecord>;
    /// Write the pending-factory-reset slot. Called **before** the reset is
    /// dispatched, via [`mint_and_persist_pending_factory_reset`] — never
    /// directly, so the code cannot be held without having been persisted.
    ///
    /// Unlike [`Self::save_authenticated`], this write must be **durable before
    /// it returns**: the whole point of the row is to survive a SIGKILL that
    /// lands microseconds later, so an implementation that defers the write to a
    /// background task reopens gap CR-1.
    fn save_pending_factory_reset(&self, record: PendingFactoryResetRecord);
    /// Clear the pending-factory-reset slot once the re-claim completes.
    fn delete_pending_factory_reset(&self);
    /// Persist `(nest_url, handle, domain, tier)` after a successful silent
    /// challenge or wizard completion. Each app caches all four in its
    /// long-term store (libsecret on Linux, Keychain on Apple, etc.) so
    /// the next relaunch can show "Welcome back, @handle@domain · tier"
    /// while the silent challenge is in flight. Implementation may write
    /// asynchronously.
    fn save_authenticated(
        &self,
        nest_url: String,
        user_handle: String,
        domain: String,
        tier: String,
    );
    /// Clear the pending-invite slot after a successful wizard completion.
    fn delete_pending_invite(&self);

    /// The account's **reach hint** — the freshly-provisioned box's public IPv4,
    /// kept beside `nest_url` while the domain is still propagating
    /// (`onboarding.md` § Reach hint, `long-term-store.md` § Multi-account
    /// evolution). `None` is the ordinary case and means "dial the domain, once,
    /// exactly as before": every account that did not provision its own box —
    /// a second device, any sign-in by handle — has no hint and needs none.
    ///
    /// ⚠ **Required, not defaulted** — this trait is `uniffi::export(with_foreign)`
    /// and UniFFI refuses a default body on an exported trait method. A store
    /// with no hint to offer therefore writes the `None` out explicitly, which
    /// is the honest spelling anyway: "this store never captured one".
    fn load_reach_ipv4(&self) -> Option<String>;

    /// Drop the reach hint. Called on the **first successful domain dial** and
    /// at no other time (`onboarding.md` § Reach hint): a hint that merely
    /// failed is a failed fallback, not a wrong address, and deleting it there
    /// would throw away the one thing that can reach a box whose DNS is still
    /// hours out.
    fn delete_reach_ipv4(&self);
}

// ---------------------------------------------------------------------------
// Test helper: in-memory implementation.
// ---------------------------------------------------------------------------

#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
use std::sync::Mutex;

#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
#[derive(Default)]
pub struct InMemoryPersistence {
    identity: Mutex<Option<Vec<u8>>>,
    nest_url: Mutex<Option<String>>,
    pending_invite: Mutex<Option<PendingInviteRecord>>,
    awaiting_dns: Mutex<Option<AwaitingDnsRecord>>,
    pending_factory_reset: Mutex<Option<PendingFactoryResetRecord>>,
    reach_ipv4: Mutex<Option<String>>,
    account_index_refusal: Mutex<Option<AccountIndexRefusal>>,
    /// Most recent `(nest_url, handle, domain, tier)` saved via
    /// `save_authenticated`. Tests inspect this to assert post-success
    /// persistence.
    pub authenticated: Mutex<Option<(String, String, String, String)>>,
}

#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
impl InMemoryPersistence {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_identity(self, secret: Vec<u8>) -> Self {
        *self.identity.lock().unwrap() = Some(secret);
        self
    }

    pub fn with_nest_url(self, nest_url: impl Into<String>) -> Self {
        *self.nest_url.lock().unwrap() = Some(nest_url.into());
        self
    }

    pub fn with_pending_invite(self, rec: PendingInviteRecord) -> Self {
        *self.pending_invite.lock().unwrap() = Some(rec);
        self
    }

    pub fn with_awaiting_dns(self, rec: AwaitingDnsRecord) -> Self {
        *self.awaiting_dns.lock().unwrap() = Some(rec);
        self
    }

    pub fn with_pending_factory_reset(self, rec: PendingFactoryResetRecord) -> Self {
        *self.pending_factory_reset.lock().unwrap() = Some(rec);
        self
    }

    pub fn with_reach_ipv4(self, ipv4: impl Into<String>) -> Self {
        *self.reach_ipv4.lock().unwrap() = Some(ipv4.into());
        self
    }

    pub fn with_account_index_refusal(self, refusal: AccountIndexRefusal) -> Self {
        *self.account_index_refusal.lock().unwrap() = Some(refusal);
        self
    }
}

#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
impl LaunchPersistence for InMemoryPersistence {
    fn account_index_refusal(&self) -> Option<AccountIndexRefusal> {
        *self.account_index_refusal.lock().unwrap()
    }
    fn load_identity(&self) -> Option<Vec<u8>> {
        self.identity.lock().unwrap().clone()
    }
    fn load_nest_url(&self) -> Option<String> {
        self.nest_url.lock().unwrap().clone()
    }
    fn load_pending_invite(&self) -> Option<PendingInviteRecord> {
        self.pending_invite.lock().unwrap().clone()
    }
    fn load_awaiting_dns(&self) -> Option<AwaitingDnsRecord> {
        self.awaiting_dns.lock().unwrap().clone()
    }
    fn load_pending_factory_reset(&self) -> Option<PendingFactoryResetRecord> {
        self.pending_factory_reset.lock().unwrap().clone()
    }
    fn save_pending_factory_reset(&self, record: PendingFactoryResetRecord) {
        *self.pending_factory_reset.lock().unwrap() = Some(record);
    }
    fn delete_pending_factory_reset(&self) {
        *self.pending_factory_reset.lock().unwrap() = None;
    }
    fn save_authenticated(
        &self,
        nest_url: String,
        user_handle: String,
        domain: String,
        tier: String,
    ) {
        *self.authenticated.lock().unwrap() = Some((nest_url, user_handle, domain, tier));
    }
    fn delete_pending_invite(&self) {
        *self.pending_invite.lock().unwrap() = None;
    }
    fn load_reach_ipv4(&self) -> Option<String> {
        self.reach_ipv4.lock().unwrap().clone()
    }
    fn delete_reach_ipv4(&self) {
        *self.reach_ipv4.lock().unwrap() = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_invite() -> PendingInviteRecord {
        PendingInviteRecord {
            nest_url: "https://nest.example".into(),
            handle: "alice".into(),
            request_id: "req-1".into(),
            status_json: r#"{"PendingReview":{"request_id":"req-1","last_checked_ms":0}}"#.into(),
        }
    }

    #[test]
    fn pending_invite_record_round_trip() {
        let r = sample_invite();
        let s = serde_json::to_string(&r).unwrap();
        let r2: PendingInviteRecord = serde_json::from_str(&s).unwrap();
        assert_eq!(r, r2);
    }

    #[test]
    fn empty_in_memory_returns_none_for_everything() {
        let p = InMemoryPersistence::new();
        assert_eq!(p.load_identity(), None);
        assert_eq!(p.load_nest_url(), None);
        assert_eq!(p.load_pending_invite(), None);
        assert_eq!(p.authenticated.lock().unwrap().clone(), None);
    }

    #[test]
    fn with_identity_round_trips_via_load_identity() {
        let secret = vec![0xaa; 32];
        let p = InMemoryPersistence::new().with_identity(secret.clone());
        assert_eq!(p.load_identity(), Some(secret));
    }

    #[test]
    fn save_authenticated_records_all_four_fields() {
        let p = InMemoryPersistence::new();
        p.save_authenticated(
            "https://nest.example".into(),
            "alice".into(),
            "nest.example".into(),
            "free".into(),
        );
        let saved = p.authenticated.lock().unwrap().clone();
        assert_eq!(
            saved,
            Some((
                "https://nest.example".into(),
                "alice".into(),
                "nest.example".into(),
                "free".into(),
            ))
        );
    }

    #[test]
    fn delete_pending_invite_clears_slot() {
        let p = InMemoryPersistence::new().with_pending_invite(sample_invite());
        assert!(p.load_pending_invite().is_some());
        p.delete_pending_invite();
        assert_eq!(p.load_pending_invite(), None);
    }

    #[test]
    fn pending_factory_reset_record_round_trips() {
        let r = PendingFactoryResetRecord {
            nest_url: "https://nest.example".into(),
            handle: "alice".into(),
            claim_code: "k7q2-m9xj-4pr8-wt3n-z6bv-cd5f-ah".into(),
            minted_at_secs: 1_700_000_000,
        };
        let s = serde_json::to_string(&r).unwrap();
        assert_eq!(
            serde_json::from_str::<PendingFactoryResetRecord>(&s).unwrap(),
            r
        );
    }

    /// `minted_at_secs` is required: a record without it does not parse.
    #[test]
    fn pending_factory_reset_record_without_minted_at_does_not_parse() {
        assert!(
            serde_json::from_str::<PendingFactoryResetRecord>(
                r#"{"nest_url":"https://nest.example","handle":"alice","claim_code":"k7q2-m9xj-4pr8-wt3n-z6bv-cd5f-ah"}"#,
            )
            .is_err()
        );
    }

    /// The CR-1 invariant: the minted code is in the store *before* the caller
    /// can hold it. A crash the instant `mint_and_persist_...` returns must still
    /// leave a resumable slot — which is what lets the caller pin the code onto
    /// the reset request instead of learning it from the (losable) reply.
    #[test]
    fn mint_and_persist_writes_the_slot_before_returning_the_code() {
        let p = InMemoryPersistence::new();
        assert_eq!(p.load_pending_factory_reset(), None);

        let code = mint_and_persist_pending_factory_reset(
            &p,
            "https://nest.example".into(),
            "alice".into(),
        )
        .expect("a working store must yield a code");

        let slot = p
            .load_pending_factory_reset()
            .expect("slot must be durable by the time the code is returned");
        assert_eq!(slot.claim_code, code);
        assert_eq!(slot.nest_url, "https://nest.example");
        assert_eq!(slot.handle, "alice");
    }

    /// Two resets must never collide on the same code — the slot is overwritten,
    /// and the code the box boots with is the one the latest dispatch pinned.
    #[test]
    fn mint_and_persist_mints_a_fresh_code_each_time() {
        let p = InMemoryPersistence::new();
        let first =
            mint_and_persist_pending_factory_reset(&p, "https://a.example".into(), "a".into())
                .unwrap();
        let second =
            mint_and_persist_pending_factory_reset(&p, "https://b.example".into(), "b".into())
                .unwrap();
        assert_ne!(first, second);
        assert_eq!(p.load_pending_factory_reset().unwrap().claim_code, second);
    }

    /// A store that takes the write (returns the row) or swallows it (returns
    /// nothing) — the two shapes every platform store can really take.
    struct SlotStore {
        row: Mutex<Option<AwaitingDnsRecord>>,
        takes: bool,
    }

    impl SlotStore {
        fn taking() -> Self {
            Self {
                row: Mutex::new(None),
                takes: true,
            }
        }
        fn swallowing() -> Self {
            Self {
                row: Mutex::new(None),
                takes: false,
            }
        }
        fn row(&self) -> Option<AwaitingDnsRecord> {
            self.row.lock().unwrap().clone()
        }
    }

    impl PendingProvisionStore for SlotStore {
        fn save_awaiting_dns(
            &self,
            _secret_hex: String,
            record: AwaitingDnsRecord,
        ) -> Option<AwaitingDnsRecord> {
            if !self.takes {
                return None;
            }
            *self.row.lock().unwrap() = Some(record.clone());
            Some(record)
        }

        fn clear_awaiting_dns(&self, _secret_hex: String) {
            *self.row.lock().unwrap() = None;
        }
    }

    fn provision_row(code: &str) -> AwaitingDnsRecord {
        AwaitingDnsRecord {
            nest_url: "https://box.example".into(),
            handle: "alice@box.example".into(),
            dns_records_json: String::new(),
            claim_code: code.into(),
            reach_ipv4: None,
            nest_actor_id: Some("ab".repeat(32)),
        }
    }

    /// The read-back IS the contract: a taking store hands the row back, a
    /// swallowing one yields nothing, and a row that came back carrying a
    /// different code (a concurrent writer) is not "persisted" either.
    #[test]
    fn persist_pending_provision_returns_the_row_only_when_its_code_read_back() {
        let taking = SlotStore::taking();
        let back = persist_pending_provision(&taking, "s".into(), provision_row("C1"))
            .expect("a taking store returns the row");
        assert_eq!(back, provision_row("C1"));
        assert_eq!(taking.row(), Some(provision_row("C1")));

        let swallowing = SlotStore::swallowing();
        assert_eq!(
            persist_pending_provision(&swallowing, "s".into(), provision_row("C1")),
            None,
            "a swallowed write must not read as persisted"
        );
    }

    /// The Retry path's use of the core: re-persisting the slot a run already
    /// holds keeps the code the box was built with — nothing here mints.
    #[test]
    fn re_persisting_a_held_slot_keeps_its_code_and_identity() {
        let store = SlotStore::taking();
        let code = mint_and_persist_pending_provision(
            &store,
            "s".into(),
            "https://box.example".into(),
            "alice@box.example".into(),
            Some("ab".repeat(32)),
            None,
        )
        .expect("minted");
        let held = store.row().expect("slot written by the mint");
        assert_eq!(held.claim_code, code);
        assert_eq!(
            held.nest_actor_id.as_deref(),
            Some("ab".repeat(32).as_str())
        );

        let back =
            persist_pending_provision(&store, "s".into(), held.clone()).expect("re-persisted");
        assert_eq!(back, held, "a re-persist changes nothing about the row");
        assert_eq!(store.row(), Some(held));
    }

    /// The test pin: a caller that must know the code in advance gets exactly
    /// that code, and **nothing else about the write changes** — the row is
    /// still persisted and read back before the code is handed out, so the
    /// tier_3 journey that uses this exercises custody-precedes-dispatch rather
    /// than stepping around it. Production passes `None` and mints.
    #[test]
    fn a_pinned_claim_code_is_persisted_and_returned_like_a_minted_one() {
        let store = SlotStore::taking();
        let code = mint_and_persist_pending_provision(
            &store,
            "s".into(),
            "https://box.example".into(),
            "alice@box.example".into(),
            Some("ab".repeat(32)),
            Some("TEST42".into()),
        )
        .expect("the pinned code must come back out of the store, as a minted one does");
        assert_eq!(code, "TEST42");
        assert_eq!(store.row().expect("slot written").claim_code, "TEST42");

        // And a store that swallows still refuses, pin or no pin: the refusal is
        // what stops a run building a box whose code survives nowhere.
        assert_eq!(
            mint_and_persist_pending_provision(
                &SlotStore::swallowing(),
                "s".into(),
                "https://box.example".into(),
                "alice@box.example".into(),
                None,
                Some("TEST42".into()),
            ),
            None,
        );
    }

    /// The mint writes the identity beside the code, and the reach completion
    /// carries the identity the box boots with — a retry that had to re-create
    /// the box rewrites it here.
    #[test]
    fn the_reach_completion_carries_the_identity_the_box_boots_with() {
        let store = SlotStore::taking();
        let code = mint_and_persist_pending_provision(
            &store,
            "s".into(),
            "https://box.example".into(),
            "alice@box.example".into(),
            Some("ab".repeat(32)),
            None,
        )
        .expect("minted");
        assert!(complete_pending_provision_reach(
            &store,
            "s".into(),
            "https://box.example".into(),
            "alice@box.example".into(),
            code.clone(),
            Some("cd".repeat(32)),
            "203.0.113.9".into(),
        ));
        let row = store.row().expect("slot");
        assert_eq!(row.claim_code, code);
        assert_eq!(row.reach_ipv4.as_deref(), Some("203.0.113.9"));
        assert_eq!(row.nest_actor_id.as_deref(), Some("cd".repeat(32).as_str()));
    }

    /// A record written before `nest_actor_id` existed still parses — the
    /// `#[serde(default)]` half of the store contract.
    #[test]
    fn an_awaiting_dns_record_parses_without_nest_actor_id() {
        let rec: AwaitingDnsRecord = serde_json::from_str(
            r#"{"nest_url":"https://n","handle":"h","dns_records_json":"[]","claim_code":"C","reach_ipv4":"1.2.3.4"}"#,
        )
        .unwrap();
        assert_eq!(rec.nest_actor_id, None);
        assert_eq!(rec.reach_ipv4.as_deref(), Some("1.2.3.4"));
    }

    #[test]
    fn delete_pending_factory_reset_clears_slot() {
        let p = InMemoryPersistence::new();
        mint_and_persist_pending_factory_reset(&p, "https://nest.example".into(), "alice".into())
            .unwrap();
        assert!(p.load_pending_factory_reset().is_some());
        p.delete_pending_factory_reset();
        assert_eq!(p.load_pending_factory_reset(), None);
    }

    /// A store whose write silently does nothing — the shape every real platform
    /// can take (`SecretStore::set` is infallible, so a locked keyring / denied
    /// Keychain / `QuotaExceededError` on web all land here), and the shape the
    /// wasm shim takes when JS throws.
    #[derive(Default)]
    struct SilentlyFailingStore;

    impl LaunchPersistence for SilentlyFailingStore {
        fn account_index_refusal(&self) -> Option<AccountIndexRefusal> {
            None
        }
        fn load_identity(&self) -> Option<Vec<u8>> {
            None
        }
        fn load_nest_url(&self) -> Option<String> {
            None
        }
        fn load_pending_invite(&self) -> Option<PendingInviteRecord> {
            None
        }
        fn load_awaiting_dns(&self) -> Option<AwaitingDnsRecord> {
            None
        }
        fn load_pending_factory_reset(&self) -> Option<PendingFactoryResetRecord> {
            None // the write above went nowhere
        }
        fn save_pending_factory_reset(&self, _record: PendingFactoryResetRecord) {}
        fn delete_pending_factory_reset(&self) {}
        fn save_authenticated(&self, _: String, _: String, _: String, _: String) {}
        fn delete_pending_invite(&self) {}
        fn load_reach_ipv4(&self) -> Option<String> {
            None
        }
        fn delete_reach_ipv4(&self) {}
    }

    /// If the store silently dropped the row, the caller must NOT get a code.
    /// Handing one back would be worse than the original CR-1: the client would
    /// pin it onto a real reset and wipe the box against a code that exists
    /// nowhere, while believing it had a resumable slot.
    #[test]
    fn mint_and_persist_yields_no_code_when_the_write_silently_failed() {
        let p = SilentlyFailingStore;
        let got = mint_and_persist_pending_factory_reset(
            &p,
            "https://nest.example".into(),
            "alice".into(),
        );
        assert_eq!(
            got, None,
            "a code must never outlive a failed persist — the caller aborts the reset instead"
        );
    }
}
