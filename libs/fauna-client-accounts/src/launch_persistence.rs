//! Reconcile the launch machine's single-identity persistence seam onto
//! the multi-account registry — **one foreign seam, not two**.
//!
//! `fauna_launch_machine::LaunchPersistence` is what the launch state
//! machine reads at boot to route the four-case launch decision (identity
//! present? nest url present? pending invite?). Before multi-account, each
//! app implemented that trait *directly* over its native secret store.
//! With the account registry, a client would otherwise implement **two**
//! foreign traits — `LaunchPersistence` *and* [`SecretStore`]. This adapter
//! collapses that: the platform implements only [`SecretStore`], and this
//! type provides `LaunchPersistence` on top of [`AccountRegistry`], reading
//! the slots of the **active** account.
//!
//! Consequence: the existing three-/four-case launch routing (`machine.rs`
//! `start()`) is unchanged, but now reads whichever identity is active —
//! switching accounts is `AccountRegistry::set_active` followed by a client
//! teardown/rebuild of the launch machine (Stage 1 client work). This
//! follows the switch-first account-switching design (tracked internally);
//! see also `docs/goal/architecture/long-term-store.md`
//! § Multi-account evolution → Shared seam.

use fauna_launch_machine::{
    AwaitingDnsRecord, LaunchPersistence, PendingFactoryResetRecord, PendingInviteRecord,
    PendingProvisionStore,
};

use crate::AccountRegistry;

/// [`LaunchPersistence`] backed by the active account of an
/// [`AccountRegistry`]. Mint it from the same registry view the switcher UI
/// uses ([`AccountRegistry::launch_persistence`] — both are cheap stateless
/// views over one store), pass it to `LaunchMachine::new`, and the launch flow
/// routes on the active identity.
pub struct RegistryLaunchPersistence {
    registry: AccountRegistry,
    /// `Some(actor_id)` pins this adapter to one account for its whole
    /// lifetime — the **bound** adapter a secondary instance launches over
    /// (`account-scoping.md` § Concurrent instances). `None` is the ordinary
    /// adapter: it re-reads the active pointer on every call, which is what
    /// makes a primary instance follow a switch.
    bound: Option<String>,
}

impl RegistryLaunchPersistence {
    /// Build over an existing registry view (cheap clone of its `Arc`s).
    ///
    /// **There is deliberately no constructor taking a bare
    /// [`SecretStore`]**, and re-adding one would silently re-open a real hole:
    /// it could only build its registry with [`AccountRegistry::new`], i.e.
    /// with a [`NoopMutationLock`](crate::NoopMutationLock), so a platform that
    /// had adopted the cross-process mutation lock everywhere else would still
    /// run its *launch* writer — `save_authenticated`, a full index
    /// read-modify-write — unserialized, and nothing would say so. Every
    /// adapter therefore inherits the lock decision its registry already made.
    /// Prefer [`AccountRegistry::launch_persistence`] at call sites.
    pub fn from_registry(registry: AccountRegistry) -> Self {
        Self {
            registry,
            bound: None,
        }
    }

    /// A **bound** adapter for a secondary instance: every load/save resolves
    /// `actor_id`'s slots and the active pointer is never consulted nor moved
    /// (`account-scoping.md` § Concurrent instances).
    ///
    /// Gate the spawn with [`AccountRegistry::bind_account`] /
    /// [`AccountRegistry::bind_account_confirmed`] first; construction itself
    /// stays infallible because the adapter is a cheap stateless view. If the
    /// bound account has been removed by the time this adapter is read, every
    /// load returns `None` and every save refuses — the instance routes to
    /// onboarding (fail-safe), never to another account's slots.
    /// Prefer [`AccountRegistry::bound_launch_persistence`] at call sites.
    pub fn from_registry_bound(registry: AccountRegistry, actor_id: impl Into<String>) -> Self {
        Self {
            registry,
            bound: Some(actor_id.into()),
        }
    }

    /// A clone of the underlying registry view, so a caller that only holds
    /// the adapter can still list/switch accounts.
    pub fn registry(&self) -> AccountRegistry {
        self.registry.clone()
    }

    /// The account this adapter resolves: the bound account if pinned, else
    /// whatever is active *right now*. Fail-safe for a bound account that no
    /// longer exists: `secrets()`/`update_cache` on it resolve to
    /// nothing/error downstream, so loads read `None` and saves write
    /// nothing.
    fn session_account(&self) -> Option<String> {
        match &self.bound {
            Some(actor) => Some(actor.clone()),
            None => self.registry.active(),
        }
    }
}

impl LaunchPersistence for RegistryLaunchPersistence {
    /// Read **before every other row** by `LaunchMachine::start`: on either
    /// verdict `session_account()` answers nothing, so every method below
    /// reads the install as identity-less and the routing table's last row
    /// would drop the user into fresh onboarding
    /// (`onboarding.md` § App-launch routing).
    ///
    /// Deliberately **not** narrowed to the bound/unbound distinction: an
    /// index this build cannot parse is a fact about the install, not about
    /// which account a bound adapter was pinned to.
    fn account_index_refusal(&self) -> Option<fauna_launch_machine::AccountIndexRefusal> {
        self.registry.index_refusal()
    }

    fn load_identity(&self) -> Option<Vec<u8>> {
        let account = self.session_account()?;
        let secret_hex = self.registry.secrets(&account)?.secret_hex;
        // A malformed secret slot is not a valid identity — treat it as
        // "no identity" (routes to onboarding) rather than propagating a
        // half-decoded key. The registry only ever *writes* validated
        // 32-byte hex, so this is defensive.
        fauna_core::hex32::decode(&secret_hex)
            .ok()
            .map(|arr| arr.to_vec())
    }

    fn load_nest_url(&self) -> Option<String> {
        let account = self.session_account()?;
        self.registry.secrets(&account)?.nest_url
    }

    fn load_reach_ipv4(&self) -> Option<String> {
        let account = self.session_account()?;
        self.registry.reach_ipv4(&account)
    }

    fn delete_reach_ipv4(&self) {
        // No `session_account()` means no account to hold a hint, so there is
        // nothing to clear — the same silent no-op every other write on this
        // adapter takes rather than inventing an account to write against.
        if let Some(account) = self.session_account() {
            self.registry.clear_reach_ipv4(&account);
        }
    }

    fn load_pending_invite(&self) -> Option<PendingInviteRecord> {
        let account = self.session_account()?;
        let json = self.registry.pending_invite_json(&account)?;
        // Opaque-JSON contract (onboarding.md § Long-term store contract): a
        // corrupt slot degrades to "no pending invite", never a panic.
        serde_json::from_str(&json).ok()
    }

    fn load_awaiting_dns(&self) -> Option<AwaitingDnsRecord> {
        let account = self.session_account()?;
        let json = self.registry.awaiting_dns_json(&account)?;
        // Same opaque-JSON contract: a corrupt slot degrades to "no deferred
        // nest" (the launch falls through to the ordinary rows) rather than
        // stranding the user on an un-seedable "Almost ready" surface.
        serde_json::from_str(&json).ok()
    }

    fn load_pending_factory_reset(&self) -> Option<PendingFactoryResetRecord> {
        let account = self.session_account()?;
        let json = self.registry.pending_factory_reset_json(&account)?;
        // Same opaque-JSON contract: a corrupt slot degrades to "no pending
        // reset" and the launch falls through to the ordinary rows. That is the
        // safe direction — the box is at the fresh/unclaimed floor either way,
        // so the user lands on the ordinary (un-prefilled) claim surface rather
        // than on an un-seedable one.
        serde_json::from_str(&json).ok()
    }

    fn save_pending_factory_reset(&self, record: PendingFactoryResetRecord) {
        // Written on the session's account — the admin dispatching the reset.
        // No session account ⇒ nobody owns the re-claim, so nothing to write.
        //
        // Durability contract (CR-1): this must hit the platform store *before*
        // it returns. `SecretStore::set` is synchronous on every platform impl,
        // so the value is in libsecret/Keychain/DPAPI/localStorage by the time
        // `mint_and_persist_pending_factory_reset` hands the code back and the
        // caller dispatches the reset.
        let Some(account) = self.session_account() else {
            return;
        };
        // Membership gate — the pure twin of `save_authenticated`'s
        // `update_cache` disarm, and load-bearing for the same reason: `bound`
        // is process state that outlives a removal or a sign-out wipe. Belt-
        // and-braces now: `set_pending_factory_reset_json` below carries its
        // own `AccountRegistry::is_live` guard (`long-term-store.md`
        // § Cleanup contract), but this early return still matters for a
        // BOUND instance specifically — the bind gate answers `UnknownActor`
        // for a removed-elsewhere account, unbound reads follow `active`, and
        // `clear_all` never sweeps a stray write that a bound session made
        // AFTER the wipe (it enumerates the index, and a bound instance's own
        // account may no longer be in it) — a claim code the CR-1 recovery
        // path cannot serve. Refusing to write makes the caller's read-back
        // return `None`, i.e. "do not dispatch the reset" — the same
        // refusal-before-dispatch shape as having no session account at all.
        //
        // Pure `index()` on purpose (no lock): an unreadable index yields an
        // empty one ⇒ refuse, which is the fail-closed direction.
        if self.registry.index().position(&account).is_none() {
            return;
        }
        // Serializing a 3-String record cannot fail.
        let Ok(json) = serde_json::to_string(&record) else {
            return;
        };
        self.registry
            .set_pending_factory_reset_json(&account, &json);
    }

    fn delete_pending_factory_reset(&self) {
        if let Some(account) = self.session_account() {
            self.registry.clear_pending_factory_reset(&account);
        }
    }

    fn save_authenticated(
        &self,
        nest_url: String,
        user_handle: String,
        domain: String,
        tier: String,
    ) {
        // Best-effort, on the session's account (the one the challenge just
        // authenticated). No session account ⇒ nothing to write.
        //
        // This guard is what makes a silent challenge that races a sign-out
        // harmless: the challenge runs on a worker the client cannot cancel, so
        // it can land *after* the credential namespace was wiped. A wiped store
        // has no active account, so we write nothing and the identity stays
        // erased. `set_nest_url` below carries the same guard itself now
        // (`AccountRegistry::is_live`, `long-term-store.md` § Cleanup contract)
        // — this early return is belt-and-braces, not the only thing standing
        // between a late reply and a resurrected slot.
        let Some(account) = self.session_account() else {
            return;
        };
        let write = AuthenticatedWrite {
            registry: self.registry.clone(),
            account,
            nest_url,
            user_handle,
            domain,
            tier,
        };
        // Web: the write is one index read-modify-write like every other
        // mutator, so it runs inside the cross-tab mutation lock — which is
        // async, while this trait method is not. The trait permits an
        // asynchronous write ("Implementation may write asynchronously"), so
        // the guarded write is spawned; the machine reads nothing back from it
        // (the next launch does, and the switcher's own refresh re-writes the
        // same rows). Native writes under the registry's own file lock, inline.
        #[cfg(all(target_arch = "wasm32", feature = "web-localstorage"))]
        {
            wasm_bindgen_futures::spawn_local(async move {
                crate::with_web_mutation_lock(move || write.apply()).await;
            });
        }
        #[cfg(not(all(target_arch = "wasm32", feature = "web-localstorage")))]
        write.apply();
    }

    fn delete_pending_invite(&self) {
        if let Some(account) = self.session_account() {
            self.registry.clear_pending_invite(&account);
        }
    }
}

/// One `save_authenticated` write, detached from the adapter so web can carry
/// it into the cross-tab mutation lock's `'static` section (see the method).
struct AuthenticatedWrite {
    registry: AccountRegistry,
    account: String,
    nest_url: String,
    user_handle: String,
    domain: String,
    tier: String,
}

impl AuthenticatedWrite {
    fn apply(self) {
        // `update_cache` errors only if the actor is not in the index — either a
        // wipe landed between the account read and here, or the index is
        // unreadable and so empty. Either way it means "this account is gone";
        // write nothing rather than resurrect a slot for it. This same guard is
        // what disarms a *bound* adapter whose account was removed or wiped:
        // `bound` is process state that survives the wipe, but the index row is
        // gone, so nothing is written.
        if self
            .registry
            .update_cache(
                &self.account,
                Some(&self.user_handle),
                Some(&self.domain),
                Some(&self.tier),
            )
            .is_err()
        {
            return;
        }
        self.registry.set_nest_url(&self.account, &self.nest_url);
    }
}

// ---------------------------------------------------------------------------
// Wizard-exit slot writers.
//
// `LaunchPersistence` deliberately carries no `save_awaiting_dns` /
// `save_pending_invite` (see its docs: the machine never writes those rows), so
// the write side is the CLIENT's. That is exactly where it drifted: linux and
// tui each hand-rolled the same `add_account` + `set_active` + `set_*_json`
// composition, and the three UniFFI apps could not reach it at all — no
// export existed — so they wrote their own single-slot keys instead, whose
// composition lost a half-provisioned nest on apple's handle-less
// deferred-DNS exit.
//
// One implementation, shared by all seven apps, is the fix (priority #2):
// the per-actor slot is authoritative, carries opaque JSON with no non-empty
// gate, and survives an indexed (multi-account) install.
// ---------------------------------------------------------------------------

/// Persist the deferred-DNS resume slot at the wizard's `AwaitingManualDns`
/// exit, returning the actor id it was written under.
///
/// Registers the identity as an account first: the slot is addressed *per
/// actor*, so there has to be an actor to address it by, and the row this
/// resumes is (identity + slot) — `LaunchMachine::start` only takes the
/// awaiting-dns branch when both are present.
///
/// Deliberately writes **no `nest_url`** on the account: the nest is not
/// claimed yet, so a silent challenge against it could only fail — and the
/// awaiting-DNS row outranks the silent-challenge row anyway. The record
/// carries its own `nest_url` for the reseed (`onboarding.md` § Long-term
/// store contract).
///
/// **Completes the slot; does not replace it.** The pending-provision write
/// (`onboarding.md` § 6 *The pending-provision slot*) lands this same row
/// *before* `create_server` and fills `reach_ipv4` the moment the box exists;
/// the deferred-DNS exit arrives later carrying only "the records to paste"
/// and no address, which is exactly the wording the goal doc uses for it —
/// *"its later `AwaitingManualDns` exit only **completes** the record with the
/// records to paste"* (§ 6 *Deferred-DNS path*). So a `None` `reach_ipv4` on
/// the way in means "I have nothing to say about the address", never "forget
/// the address": the stored one is carried forward — and the same rule covers
/// `nest_actor_id`, the identity the box was built with, which the exit has
/// equally never heard of. Doing this here rather than in each app's exit glue
/// is what keeps all seven exits correct without any of them knowing the
/// fields exist (priority #2).
pub fn persist_awaiting_dns(
    registry: &AccountRegistry,
    secret_hex: &str,
    record: &AwaitingDnsRecord,
) -> Result<String, crate::AccountError> {
    write_awaiting_dns(registry, secret_hex, record, SlotWrite::Completing)
}

/// What an absent optional field on the way in *means* — the distinction the
/// carry-forward rule turns on, and the one a single writer could not express.
///
/// `None` is two different statements depending on who is writing, and reading
/// them the same way is how a freshly-minted row inherited the PREVIOUS box's
/// address.
enum SlotWrite {
    /// The writer is adding what it knows to a row someone else owns, and an
    /// absent field means **"I have nothing to say about this"** — carry the
    /// stored value forward. The deferred-DNS exit is the case: it arrives with
    /// the records to paste and has never heard of a reach address or a
    /// built-with identity (`onboarding.md` § 6 *Deferred-DNS path* — its exit
    /// "only *completes* the record").
    ///
    /// It is also a wizard **terminal**, so it **activates** the identity it
    /// writes — "the terminal registers and switches" (`onboarding.md`
    /// § Multi-account): in append mode this is the moment the appended
    /// identity becomes the account the next launch resumes.
    Completing,
    /// The writer owns the whole row, and an absent field means **"there is no
    /// such value"** — write it as absent. Every writer reaching the slot
    /// through [`PendingProvisionStore`] is this one: the mint holds a box that
    /// does not exist yet (so it has no address *by construction*), and the
    /// retry re-persist and the reach completion each hold the full row they are
    /// rewriting (`onboarding.md` § 6 *The pending-provision slot*).
    ///
    /// It is mid-run **custody**, never a routing decision, so it **never moves
    /// the active pointer** (2026-09-25). It
    /// registers the identity — the slot is addressed per actor, and the
    /// secret of the identity a box is being built for must be durable before
    /// `create_server`, or a quit orphans a paid box — and stops there. A
    /// first-run identity is already active (moment 1,
    /// [`persist_confirmed_identity`], ran before the wizard reached
    /// provisioning; a run seeded onto the provisioning page over an empty
    /// registry is covered by `add_account`'s first-account rule). An
    /// **append** identity must NOT become active here: the live session still
    /// runs as the account the user is adding from, and the app's
    /// abandon-append handler re-dispatches over the registry's active
    /// account — an activation here made an abandoned or interrupted append
    /// hijack the session onto a half-onboarded identity. Activation is the
    /// terminal's ([`persist_logged_in`] / [`persist_pending_invite`] / the
    /// exit above), exactly as for the confirm. Pinned by
    /// `an_append_mode_mint_registers_the_appended_identity_but_never_moves_the_active_pointer`.
    Authoritative,
}

/// The one write behind [`persist_awaiting_dns`] and
/// [`RegistryPendingProvisionStore::save_awaiting_dns`], differing in how it
/// reads an absent field and in whether it activates (see [`SlotWrite`]).
fn write_awaiting_dns(
    registry: &AccountRegistry,
    secret_hex: &str,
    record: &AwaitingDnsRecord,
    mode: SlotWrite,
) -> Result<String, crate::AccountError> {
    let actor_id = registry.add_account(secret_hex, None, None)?;
    match mode {
        // A terminal: register AND switch. Same `let _` as the two sibling
        // terminals — an activation refusal past the write is policy, not loss.
        SlotWrite::Completing => {
            let _ = registry.set_active(&actor_id);
        }
        // Custody: register, never switch (the variant's doc owns the why).
        SlotWrite::Authoritative => {}
    }
    let mut record = record.clone();
    if matches!(mode, SlotWrite::Completing)
        && (record.reach_ipv4.is_none() || record.nest_actor_id.is_none())
    {
        let stored = registry
            .awaiting_dns_json(&actor_id)
            .and_then(|json| serde_json::from_str::<AwaitingDnsRecord>(&json).ok());
        if record.reach_ipv4.is_none() {
            record.reach_ipv4 = stored.as_ref().and_then(|s| s.reach_ipv4.clone());
        }
        if record.nest_actor_id.is_none() {
            record.nest_actor_id = stored.and_then(|s| s.nest_actor_id);
        }
    }
    // Serializing a six-field record of strings cannot fail.
    let json = serde_json::to_string(&record).expect("AwaitingDnsRecord serializes");
    registry.set_awaiting_dns_json(&actor_id, &json);
    Ok(actor_id)
}

/// The one [`PendingProvisionStore`] — the wizard's pending-provision writer,
/// over the same per-actor slot every other awaiting-dns write and read uses.
///
/// Deliberately a *view over the registry* rather than a method on
/// [`RegistryLaunchPersistence`]: this writer is addressed by the identity being
/// onboarded (which is why it carries a secret), not by the session's account
/// (which may not exist yet during a first-run wizard) — the distinction the
/// trait's own doc comment draws.
#[derive(Clone)]
pub struct RegistryPendingProvisionStore {
    registry: AccountRegistry,
}

impl RegistryPendingProvisionStore {
    pub fn from_registry(registry: AccountRegistry) -> Self {
        Self { registry }
    }
}

impl PendingProvisionStore for RegistryPendingProvisionStore {
    fn save_awaiting_dns(
        &self,
        secret_hex: String,
        record: AwaitingDnsRecord,
    ) -> Option<AwaitingDnsRecord> {
        // **Authoritative, not completing — and custody, not a terminal**
        // ([`SlotWrite`]): every caller on this path holds the whole row, and
        // none of them moves the active pointer (an append's live session keeps
        // routing on the account it is adding from until the wizard's terminal
        // switches). The mint's `reach_ipv4: None` says the
        // box does not exist YET — carrying the stored address forward there
        // handed a freshly-minted row the previous box's address, so a resume
        // would dial box A while presenting box B's claim code, and an abandoned box's IPv4 is recycled to another tenant
        // within hours.
        let actor_id = write_awaiting_dns(
            &self.registry,
            &secret_hex,
            &record,
            SlotWrite::Authoritative,
        )
        .ok()?;
        // The read-back the trait's contract is built on: go back through the
        // store, not through the value we just handed it. `set_awaiting_dns_json`
        // cannot report failure (`SecretStore::set` is infallible by signature),
        // so this round trip is the only thing that can tell a durable write from
        // a swallowed one.
        let json = self.registry.awaiting_dns_json(&actor_id)?;
        serde_json::from_str(&json).ok()
    }

    fn clear_awaiting_dns(&self, secret_hex: String) {
        clear_awaiting_dns_for_secret(&self.registry, &secret_hex);
    }
}

/// Clear the deferred-DNS resume slot at a claim terminal.
///
/// Addressed by the *active* account — the same way
/// [`RegistryLaunchPersistence::load_awaiting_dns`] reads it — so the slot is
/// written, read, and cleared against one notion of whose slot it is. A no-op
/// when no account is registered, which is exactly when no slot can exist.
pub fn clear_awaiting_dns_for_active(registry: &AccountRegistry) {
    if let Some(actor_id) = registry.active() {
        registry.clear_awaiting_dns(&actor_id);
    }
}

/// Clear the deferred-DNS resume slot of the identity `secret_hex` names — the
/// "Almost ready" surface's explicit exit, which addresses the slot the way the
/// pending-provision writer does (by the identity being onboarded) rather than
/// the way a terminal does (by the active account). The two differ on an append
/// run: the live session keeps the account the user is adding from, so
/// [`clear_awaiting_dns_for_active`] would retire the wrong account's slot and
/// leave the abandoned box's to route every relaunch back onto the surface.
///
/// A secret that is not a valid identity names no slot; nothing happens.
pub fn clear_awaiting_dns_for_secret(registry: &AccountRegistry, secret_hex: &str) {
    if let Ok(keypair) = fauna_core::identity::ActorKeypair::from_secret_hex(secret_hex) {
        registry.clear_awaiting_dns(&keypair.actor_id_hex());
    }
}

/// Persist the identity the user just confirmed — **moment 1** of the
/// two-moment write contract (`long-term-store.md`: *"1. Confirm-identity
/// (generated or imported): write `secret_key`"*, and *"anything else is a
/// bug"*) — returning the actor id it was written under.
///
/// This is the wizard's **durable commit point** (`onboarding.md` § 1
/// Identity), and it is what makes the three-case launch routing's case 2
/// reachable: `secret_key` present with `node_url` empty is the state that
/// resumes a force-quit wizard at `HandleEntry` instead of losing a
/// freshly-generated secret that exists nowhere else. Deferring it to
/// complete-login is not a stricter store hygiene — it deletes case 2 and puts
/// client-only-resident key material at risk for the whole length of the
/// wizard.
///
/// Deliberately writes **no `nest_url`** and no `device_id`: those are moment
/// 2's, and the slots are independent by design (`long-term-store.md` § The
/// three slots — *"Partial state is intentional"*).
///
/// `set_active` is not redundant with `add_account`'s "the first account
/// becomes active" rule: a signed-out (or silent-challenge-failed) install
/// still has a registered account, and without this the relaunch would resume
/// that one rather than the identity the user is onboarding right now. The
/// three terminals below ([`persist_logged_in`] / [`persist_pending_invite`] /
/// [`persist_awaiting_dns`]) make the same call for the same reason — and
/// **only** those: the mid-run pending-provision writes never activate
/// ([`SlotWrite::Authoritative`]), which is what keeps an append's routing
/// untouched until its terminal.
///
/// **Append mode writes NOTHING here — and the rule lives in this function,
/// not in seven app-side `if !append` guards (2026-09-24).** A second-account
/// wizard run over a live session persists at its own terminal
/// ([`persist_logged_in`] / [`persist_pending_invite`] /
/// [`persist_awaiting_dns`], then the app's switch), so an abandoned append
/// cannot leave a half-account behind and — since nothing is written — cannot
/// shadow the active account either: the next launch routes on the registry's
/// active account exactly as before the append began. (A provisioning run
/// started inside the append does write, before `create_server`: the mint
/// registers the appended identity **inactive** beside its pending-provision
/// slot — custody of the box, `onboarding.md` § Multi-account → *Append-mode
/// deferred/incomplete states + abandonment* — and that shadows nothing
/// either, since routing reads the active account alone.) That is the crash-safe
/// single decision point `nest/common.md` § Client-state recoverability asks
/// for, and the reason the pre-registry single slot (which every app's
/// append confirm used to overwrite) could retire (`long-term-store.md`
/// § Downgrade mirror + abandoned-append recovery — RETIRED 2026-09-24). An
/// append user's identity is already durable under the account they are
/// adding *from*, so deferring costs nothing; a first-run user's
/// freshly generated secret exists nowhere else, so moment 1 must write.
/// With `append` the call is a pure derivation: it returns the actor id the
/// secret derives to and touches the store not at all. Pinned by
/// `an_append_confirm_writes_nothing_and_the_launch_still_routes_on_the_active_account`.
///
/// **It also retracts the previous run's abandoned identity**
/// ([`AccountRegistry::retire_superseded_provisionals`]). Writing a real,
/// activated row at Continue is what makes case 2 reachable, but it is also
/// what let an abandoned identity survive as a handle-less, nest-less,
/// activatable ghost in the account switcher once the user came Back and
/// onboarded a *different* one — the regression `long-term-store.md` § Eager
/// vs. lazy migration at native boot still forbids, whose stated mechanism
/// ("no mutator runs until an authentication succeeds") moment 1 retired
/// without replacing. Because this function is the only door a first-run
/// wizard's commit goes through, it is also the only place that sees *every*
/// way the previous identity was abandoned — Back, quit, crash, kill — so the
/// retraction belongs here rather than on any one of them.
///
/// The read-back is not defensive tidying. [`SecretStore::set`] is **infallible
/// by signature**, so `add_account` returning `Ok` does not prove the keystore
/// took the write — and this is the one write in the product whose silent
/// failure destroys an account outright, because a freshly generated secret
/// exists nowhere else. A caller that gets `Ok` here may rely on the secret
/// being readable back.
pub fn persist_confirmed_identity(
    registry: &AccountRegistry,
    secret_hex: &str,
    append: bool,
) -> Result<String, crate::AccountError> {
    if append {
        return Ok(fauna_core::identity::ActorKeypair::from_secret_hex(secret_hex)?.actor_id_hex());
    }
    let actor_id = registry.add_account(secret_hex, None, None)?;
    if registry.secrets(&actor_id).is_none() {
        return Err(crate::AccountError::NoStoredSecret(actor_id));
    }
    // Activation failures past the read-back are policy, not loss (a flagged
    // account wanting re-auth, say) — the secret is durable either way, so they
    // must not fail the commit point. Same `let _` as the two siblings below.
    let _ = registry.set_active(&actor_id);
    // Strictly AFTER the read-back and the activation: a wizard run that
    // superseded an abandoned identity must never be able to retire the old row
    // before the new one is proven durable — the ordering that keeps a crash
    // anywhere in here landing on "two rows" (recoverable, and swept by the next
    // confirm) rather than "none".
    registry.retire_superseded_provisionals(&actor_id);
    Ok(actor_id)
}

/// The wizard's **logged-in terminal** — moment 4, and the one that decides
/// whether the next launch reaches the main app at all.
///
/// Reaching `WizardOutcome::LoggedIn` means this identity now has a home nest.
/// Recording it per-actor is what makes the next launch route to the silent
/// challenge (`LaunchPhase::Online`) instead of back into onboarding. Without
/// it the launch machine's routing tuple degrades to `(Some(secret), None,
/// None)` → `WizardAt(HandleEntry)`: the freshly onboarded user lands back on
/// the handle-entry page, silently, with their identity intact — which is why
/// the moment is shared rather than left to each app to sequence
/// (`onboarding.md` § App-launch routing).
///
/// `device_id` rides along as the third per-actor companion row. Pass `None`
/// only when the app genuinely has no device id yet.
///
/// The pending-invite slot is spent here for the same routing reason: its row
/// is evaluated **before** the silent-challenge row, so a survivor would pin
/// every later launch on the invite-request surface for an invite already
/// redeemed.
///
/// **The awaiting-DNS slot is spent here too — and this is the ONE clearing
/// moment** (`onboarding.md` § Long-term store contract, ratified 2026-09-21).
/// Its row outranks the silent challenge the same way, so a survivor pins the
/// next launch on "Almost ready" for a nest the user is already on. It is
/// deliberately *not* cleared any earlier — not at the claim, not when the
/// outcome leaves `AwaitingManualDns`, not at the `NatModeChoice` routing:
/// clearing at `LoggedIn` is what lets a force-quit on the NAT page or on the
/// trust offer relaunch back into the resume, whose first poll takes the
/// already-claimed edge and asks once more (§ 3b-ter); an earlier clear would
/// drop straight into the app with the offer lost. A `LoggedIn` terminal that
/// does not reach this helper (windows, which has no call site yet; the
/// append-mode arms that register through their own add + switch) clears the
/// slot explicitly at that same terminal, never earlier.
///
/// Idempotent — `add_account` re-affirms an existing account rather than
/// duplicating it, so a re-entered wizard or a resumed claim is safe.
/// `set_active` carries the same rationale as [`persist_confirmed_identity`]'s:
/// a signed-out install still has a registered account, and without it the
/// relaunch would resume that one instead of the identity just onboarded.
pub fn persist_logged_in(
    registry: &AccountRegistry,
    secret_hex: &str,
    nest_url: &str,
    device_id: Option<&str>,
    reach_ipv4: Option<&str>,
) -> Result<String, crate::AccountError> {
    let actor_id = registry.add_account(secret_hex, Some(nest_url), device_id)?;
    let _ = registry.set_active(&actor_id);
    registry.clear_pending_invite(&actor_id);
    registry.clear_awaiting_dns(&actor_id);
    // The reach hint (`onboarding.md` § Reach hint), captured here because this
    // is the moment the account learns its `nest_url` — and the hint's whole job
    // is to be dialable while that URL is not. `None` is the ordinary case (a
    // sign-in by handle, a second device, any account that did not provision its
    // own box) and must leave the slot untouched rather than clear it: a
    // re-entered wizard is idempotent, and clearing here would drop a live hint
    // on the second pass through the same terminal.
    if let Some(ip) = reach_ipv4 {
        registry.set_reach_ipv4(&actor_id, ip);
    }
    Ok(actor_id)
}

/// Persist the pending-invite resume slot at the wizard's **submit return** —
/// the only write moment (`onboarding.md` § The pending-invite surface,
/// Persistence callouts) — returning the actor id it was written under. The
/// awaiting-dns helper's twin, and identical in why it exists.
///
/// Called on the `wizard_submit_invite_request()` return, NOT at a wizard exit:
/// the pending-review journey never exits (the `InviteSubmitted` outcome this
/// once keyed on retired 2026-08-11).
pub fn persist_pending_invite(
    registry: &AccountRegistry,
    secret_hex: &str,
    record: &PendingInviteRecord,
) -> Result<String, crate::AccountError> {
    let actor_id = registry.add_account(secret_hex, None, None)?;
    let _ = registry.set_active(&actor_id);
    let json = serde_json::to_string(record).expect("PendingInviteRecord serializes");
    registry.set_pending_invite_json(&actor_id, &json);
    Ok(actor_id)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{InMemorySecretStore, SecretStore};
    use fauna_core::identity::ActorKeypair;
    use fauna_launch_machine::mint_and_persist_pending_factory_reset;

    const SECRET_A: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const SECRET_B: &str = "2222222222222222222222222222222222222222222222222222222222222222";

    fn actor_of(secret_hex: &str) -> String {
        ActorKeypair::from_secret_hex(secret_hex)
            .unwrap()
            .actor_id_hex()
    }

    fn secret_bytes(secret_hex: &str) -> Vec<u8> {
        fauna_core::hex32::decode(secret_hex).unwrap().to_vec()
    }

    /// A plain adapter over a store — the unlocked (single-process) shape these
    /// tests exercise. Production platforms mint theirs from a registry that
    /// may carry a mutation lock; the lock's own coverage lives in
    /// `lib.rs::tests::mutation_lock_serialization` and the FFI seam pin.
    fn plain_adapter(store: Arc<dyn SecretStore>) -> RegistryLaunchPersistence {
        AccountRegistry::new(store).launch_persistence()
    }

    /// The bound (secondary-instance) twin of [`adapter`].
    fn bound_adapter(
        store: Arc<dyn SecretStore>,
        actor_id: impl Into<String>,
    ) -> RegistryLaunchPersistence {
        AccountRegistry::new(store).bound_launch_persistence(actor_id)
    }

    /// A registry + an adapter sharing one store.
    fn setup() -> (AccountRegistry, RegistryLaunchPersistence) {
        let store = Arc::new(InMemorySecretStore::new());
        (AccountRegistry::new(store.clone()), plain_adapter(store))
    }

    /// **The wizard's home nest must reach the next launch.** Identity-confirm
    /// materializes the index with a per-actor secret row and NO per-actor
    /// `nest_url` row (none is known yet); the logged-in terminal must then
    /// record the chosen nest per-actor, or the launch machine's routing tuple
    /// `(Some(secret), None, None)` lands a freshly onboarded user back on
    /// `WizardAt(HandleEntry)` — silently, with their identity intact (the
    /// macOS red behind
    /// `test_smoke_k_real_onboarding_completion_reaches_the_main_app[macos]`,
    /// when apple's terminal wrote a slot the next boot rewrote).
    #[test]
    fn the_wizard_terminal_records_the_home_nest_for_the_next_launch() {
        let store: Arc<dyn SecretStore> = Arc::new(InMemorySecretStore::new());
        let registry = AccountRegistry::new(store.clone());
        let a = actor_of(SECRET_A);

        // 1. Identity confirm, mid-wizard: no nest has been chosen yet.
        let confirmed = persist_confirmed_identity(&registry, SECRET_A, false).unwrap();
        assert_eq!(confirmed, a);

        // 2. The wizard's logged-in terminal: the user's chosen home nest.
        persist_logged_in(
            &registry,
            SECRET_A,
            "https://nest.example",
            Some("dev-a"),
            None,
        )
        .unwrap();

        // 3. The next launch.
        let adapter = plain_adapter(store.clone());
        assert_eq!(
            adapter.load_nest_url().as_deref(),
            Some("https://nest.example"),
            "the home nest must reach the next launch — otherwise the launch machine \
             routes a freshly onboarded user straight back to the handle-entry page"
        );
        assert_eq!(
            adapter.load_identity(),
            Some(secret_bytes(SECRET_A)),
            "and the identity is untouched, which is what makes the failure silent"
        );
        assert_eq!(
            registry.active().as_deref(),
            Some(a.as_str()),
            "the freshly onboarded account is the active one"
        );
    }

    /// The terminal is idempotent and does not disturb an existing account —
    /// re-running it (a re-entered wizard, a resumed claim) re-affirms the same
    /// account rather than adding a second one.
    #[test]
    fn the_wizard_terminal_is_idempotent() {
        let (registry, adapter) = setup();
        persist_logged_in(&registry, SECRET_A, "https://one.example", None, None).unwrap();
        persist_logged_in(&registry, SECRET_A, "https://two.example", None, None).unwrap();

        assert_eq!(registry.list().len(), 1, "no duplicate account");
        assert_eq!(
            adapter.load_nest_url().as_deref(),
            Some("https://two.example"),
            "the most recent home nest wins"
        );
    }

    /// **The awaiting-DNS slot is spent at `LoggedIn`, inside this helper — and
    /// nowhere earlier** (`onboarding.md` § Long-term store contract, ratified
    /// 2026-09-21). The slot's launch row is evaluated *before* the
    /// silent-challenge row, so a survivor would pin every later launch on
    /// "Almost ready" for a nest the user is already on; and clearing it any
    /// earlier (at the claim, as web and apple once did) drops the force-quit
    /// recovery the resume's trust offer relies on (§ 3b-ter). The deferred
    /// exit writes it, the terminal clears it — the same pair the
    /// pending-invite slot already forms here.
    #[test]
    fn the_wizard_terminal_spends_the_awaiting_dns_slot() {
        let (registry, adapter) = setup();
        persist_awaiting_dns(&registry, SECRET_A, &dns_record("alice@nest.example")).unwrap();
        assert!(
            adapter.load_awaiting_dns().is_some(),
            "precondition: the deferred exit wrote the slot"
        );

        persist_logged_in(&registry, SECRET_A, "https://nest.example", None, None).unwrap();

        assert_eq!(
            adapter.load_awaiting_dns(),
            None,
            "reaching LoggedIn spends the awaiting-DNS slot — the launch row that \
             outranks the silent challenge"
        );
        assert_eq!(
            adapter.load_nest_url().as_deref(),
            Some("https://nest.example"),
            "and the home nest is recorded, so the next launch takes the ordinary row"
        );
    }

    /// The reach hint's round trip (`onboarding.md` § Reach hint): captured at
    /// the `LoggedIn` terminal, readable per-actor, and gone once the domain
    /// first connects.
    #[test]
    fn the_terminal_records_the_reach_hint_and_it_can_be_dropped() {
        let (registry, _adapter) = setup();
        let a = persist_logged_in(
            &registry,
            SECRET_A,
            "https://nest.example",
            None,
            Some("203.0.113.9"),
        )
        .unwrap();

        assert_eq!(registry.reach_ipv4(&a).as_deref(), Some("203.0.113.9"));
        registry.clear_reach_ipv4(&a);
        assert_eq!(
            registry.reach_ipv4(&a),
            None,
            "the drop is what ends its life"
        );
    }

    /// **`None` means "nothing to say", never "forget it".** The terminal is
    /// idempotent by design — a re-entered wizard or a resumed claim runs it
    /// again — and the second pass may not carry the address the first one did
    /// (it is read off a machine that a `reset()` clears). Clearing on `None`
    /// would silently drop a live hint for the box the user is signing in to,
    /// which is the whole failure the hint exists to prevent.
    #[test]
    fn a_second_terminal_pass_without_an_address_keeps_the_stored_hint() {
        let (registry, _adapter) = setup();
        let a = persist_logged_in(
            &registry,
            SECRET_A,
            "https://nest.example",
            None,
            Some("203.0.113.9"),
        )
        .unwrap();
        persist_logged_in(&registry, SECRET_A, "https://nest.example", None, None).unwrap();

        assert_eq!(
            registry.reach_ipv4(&a).as_deref(),
            Some("203.0.113.9"),
            "an idempotent re-run must not drop the hint it did not bring"
        );
    }

    /// The ordinary sign-in — by handle, on a second device, on any account that
    /// did not provision its own box — writes no hint at all. The hint is a pure
    /// optimization and its absence must behave exactly as before it existed.
    #[test]
    fn an_ordinary_sign_in_records_no_reach_hint() {
        let (registry, _adapter) = setup();
        let a = persist_logged_in(&registry, SECRET_A, "https://nest.example", None, None).unwrap();
        assert_eq!(registry.reach_ipv4(&a), None);
    }

    /// "Use a different nest" takes the hint with the binding. A survivor would
    /// be an address for the box this account just walked away from, standing by
    /// as the fallback dial for whatever nest the user picks next.
    #[test]
    fn clearing_the_nest_binding_takes_the_reach_hint_with_it() {
        let (registry, _adapter) = setup();
        let a = persist_logged_in(
            &registry,
            SECRET_A,
            "https://nest.example",
            None,
            Some("203.0.113.9"),
        )
        .unwrap();

        registry.clear_nest_binding(&a).unwrap();

        assert_eq!(registry.reach_ipv4(&a), None);
    }

    /// Removing the account leaves no orphan row behind — the hint is deleted
    /// with every other per-actor slot.
    #[test]
    fn removing_the_account_deletes_the_reach_hint() {
        let (registry, _adapter) = setup();
        let a = persist_logged_in(
            &registry,
            SECRET_A,
            "https://nest.example",
            None,
            Some("203.0.113.9"),
        )
        .unwrap();

        registry.remove(&a).unwrap();

        assert_eq!(registry.reach_ipv4(&a), None);
    }

    /// The terminal spends the pending-invite slot: reaching `LoggedIn` means
    /// the invite was used, and a survivor would pin the next launch on the
    /// invite-request surface (that row is evaluated before the silent
    /// challenge). linux clears it inline at the same moment.
    #[test]
    fn the_wizard_terminal_spends_the_pending_invite_slot() {
        let (registry, adapter) = setup();
        persist_pending_invite(
            &registry,
            SECRET_A,
            &PendingInviteRecord {
                nest_url: "https://nest.example".into(),
                handle: "alice".into(),
                request_id: "req-1".into(),
                status_json: "{}".into(),
            },
        )
        .unwrap();
        assert!(adapter.load_pending_invite().is_some());

        persist_logged_in(
            &registry,
            SECRET_A,
            "https://nest.example",
            Some("dev-a"),
            None,
        )
        .unwrap();
        assert!(
            adapter.load_pending_invite().is_none(),
            "the invite is spent once the wizard reaches LoggedIn"
        );
    }

    #[test]
    fn empty_registry_loads_nothing() {
        // No active account → the launch machine routes to IdentityChoice.
        let (_reg, adapter) = setup();
        assert_eq!(adapter.load_identity(), None);
        assert_eq!(adapter.load_nest_url(), None);
        assert_eq!(adapter.load_pending_invite(), None);
    }

    #[test]
    fn loads_active_accounts_identity_and_nest_url() {
        let (reg, adapter) = setup();
        reg.add_account(SECRET_A, Some("https://a.example"), None)
            .unwrap();
        assert_eq!(adapter.load_identity(), Some(secret_bytes(SECRET_A)));
        assert_eq!(
            adapter.load_nest_url().as_deref(),
            Some("https://a.example")
        );
    }

    #[test]
    fn follows_the_active_pointer_on_switch() {
        let (reg, adapter) = setup();
        reg.add_account(SECRET_A, Some("https://a.example"), None)
            .unwrap();
        // Second account has no nest url — distinguishes it from A.
        let b = reg.add_account(SECRET_B, None, None).unwrap();

        // A is active (first added).
        assert_eq!(adapter.load_identity(), Some(secret_bytes(SECRET_A)));
        assert_eq!(
            adapter.load_nest_url().as_deref(),
            Some("https://a.example")
        );

        // Switch to B: the adapter now reflects B's slots.
        reg.set_active(&b).unwrap();
        assert_eq!(adapter.load_identity(), Some(secret_bytes(SECRET_B)));
        assert_eq!(adapter.load_nest_url(), None);
    }

    #[test]
    fn pending_invite_round_trips_for_active_account() {
        let (reg, adapter) = setup();
        let a = reg.add_account(SECRET_A, None, None).unwrap();
        assert_eq!(adapter.load_pending_invite(), None);

        let rec = PendingInviteRecord {
            nest_url: "https://nest.example".into(),
            handle: "alice".into(),
            request_id: "req-1".into(),
            status_json: r#"{"PendingReview":{"request_id":"req-1","last_checked_ms":0}}"#.into(),
        };
        reg.set_pending_invite_json(&a, &serde_json::to_string(&rec).unwrap());
        assert_eq!(adapter.load_pending_invite(), Some(rec));

        // delete_pending_invite clears the *active* account's slot.
        adapter.delete_pending_invite();
        assert_eq!(adapter.load_pending_invite(), None);
    }

    #[test]
    fn save_authenticated_writes_nest_url_and_cache_to_active() {
        let (reg, adapter) = setup();
        let a = reg.add_account(SECRET_A, None, None).unwrap();

        adapter.save_authenticated(
            "https://nest.example".into(),
            "alice".into(),
            "nest.example".into(),
            "pro".into(),
        );

        // nest_url slot now set on the active account.
        assert_eq!(
            reg.secrets(&a).unwrap().nest_url.as_deref(),
            Some("https://nest.example")
        );
        assert_eq!(
            adapter.load_nest_url().as_deref(),
            Some("https://nest.example")
        );

        // server-data cache updated on the active account's index entry.
        let entry = reg.list().into_iter().find(|e| e.actor_id == a).unwrap();
        assert_eq!(entry.handle.as_deref(), Some("alice"));
        assert_eq!(entry.domain.as_deref(), Some("nest.example"));
        assert_eq!(entry.tier.as_deref(), Some("pro"));
    }

    #[test]
    fn save_authenticated_writes_nothing_once_the_account_is_gone() {
        // The launch machine's silent challenge runs on a detached worker the
        // client cannot cancel (linux's `async_helper::run_on_tokio` spawns a
        // bare `std::thread`), so a sign-out can land while `start()` is still
        // in flight and this callback can fire *after* the credential namespace
        // was wiped. Once the wipe has happened there is no active account, and
        // the adapter must then write **nothing**: a stray `set` here would
        // resurrect slots the user just erased (`long-term-store.md`
        // § Cleanup contract). With reads pure, an un-cancellable writer that
        // races the wipe disarms itself.
        let store = Arc::new(InMemorySecretStore::new());
        let reg = AccountRegistry::new(store.clone());
        let adapter = plain_adapter(store.clone());
        let a = reg.add_account(SECRET_A, None, None).unwrap();

        let _ = reg.clear_all(); // sign-out

        adapter.save_authenticated(
            "https://nest.example".into(),
            "alice".into(),
            "nest.example".into(),
            "pro".into(),
        );

        assert_eq!(
            store.get(crate::INDEX_KEY),
            None,
            "a post-wipe save must not resurrect the index"
        );
        assert_eq!(
            store.get(&crate::nest_url_key(&a)),
            None,
            "a post-wipe save must not resurrect a per-actor slot"
        );
        assert_eq!(
            store.get(&crate::secret_key(&a)),
            None,
            "and above all not the secret"
        );
    }

    #[tokio::test]
    async fn real_launch_machine_routes_on_the_active_account() {
        // End-to-end: the *real* LaunchMachine, fed by the adapter, routes
        // the four-case decision on whichever account is active — no network
        // (cases 2 and 4 never touch the connector).
        use fauna_launch_machine::{LaunchMachine, LaunchPhase, LaunchWizardEntry, NullObserver};

        let store = Arc::new(InMemorySecretStore::new());
        let reg = AccountRegistry::new(store.clone());

        // Account A: identity + a pending invite, no nest url → InviteRequest.
        let a = reg.add_account(SECRET_A, None, None).unwrap();
        let rec = PendingInviteRecord {
            nest_url: "https://nest.example".into(),
            handle: "alice".into(),
            request_id: "req-1".into(),
            status_json: "{}".into(),
        };
        reg.set_pending_invite_json(&a, &serde_json::to_string(&rec).unwrap());

        // Account B: identity only → HandleEntry.
        let b = reg.add_account(SECRET_B, None, None).unwrap();

        let persistence: Arc<dyn LaunchPersistence> = Arc::new(plain_adapter(store.clone()));

        // A active → routes on A's per-actor pending-invite slot.
        let m = LaunchMachine::new(Arc::new(NullObserver), persistence.clone());
        m.start().await;
        assert_eq!(
            m.snapshot().phase,
            LaunchPhase::WizardAt {
                entry: LaunchWizardEntry::InviteRequest
            }
        );

        // Switch to B and rebuild the machine over the same live adapter
        // (the real "switch = teardown/rebuild" shape) → routes on B.
        reg.set_active(&b).unwrap();
        let m2 = LaunchMachine::new(Arc::new(NullObserver), persistence);
        m2.start().await;
        assert_eq!(
            m2.snapshot().phase,
            LaunchPhase::WizardAt {
                entry: LaunchWizardEntry::HandleEntry
            }
        );
    }

    /// CR-1 end-to-end through the *real* LaunchMachine: an admin who reset
    /// their box and crashed before the re-claim must relaunch straight onto the
    /// pre-filled claim — never onto a silent challenge against the wiped box
    /// (which would only fall through to `launch_retry`), and never onto an
    /// un-prefilled claim surface asking for a code that now exists nowhere.
    #[tokio::test]
    async fn pending_factory_reset_slot_routes_the_relaunch_to_the_prefilled_claim() {
        use fauna_launch_machine::{
            LaunchMachine, LaunchPhase, LaunchWizardEntry, NullObserver,
            mint_and_persist_pending_factory_reset,
        };

        let store = Arc::new(InMemorySecretStore::new());
        let reg = AccountRegistry::new(store.clone());
        // An established admin: identity + the nest_url they authenticated against.
        reg.add_account(SECRET_A, Some("https://nest.example"), None)
            .unwrap();

        let adapter = plain_adapter(store.clone());
        // Pre-dispatch: mint + persist. (In the client this is immediately
        // followed by `admin.factory_reset(Some(code))`.)
        let code = mint_and_persist_pending_factory_reset(
            &adapter,
            "https://nest.example".into(),
            "alice".into(),
        )
        .expect("the registry-backed store must persist the row");

        // ---- the client is SIGKILL'd here; everything above is already durable.

        // Relaunch over a *fresh* view of the same store — the slot survives.
        let persistence: Arc<dyn LaunchPersistence> = Arc::new(plain_adapter(store.clone()));
        let m = LaunchMachine::new(Arc::new(NullObserver), persistence.clone());
        m.start().await;
        assert_eq!(
            m.snapshot().phase,
            LaunchPhase::WizardAt {
                entry: LaunchWizardEntry::PendingFactoryReset
            },
            "a pending reset must outrank the saved nest_url's silent challenge"
        );

        // And the code the wizard seeds is the one the box was wiped with.
        let slot = persistence.load_pending_factory_reset().unwrap();
        assert_eq!(slot.claim_code, code);
        assert_eq!(slot.nest_url, "https://nest.example");
        assert_eq!(slot.handle, "alice");

        // Once the re-claim lands the slot is cleared and the ordinary
        // silent-challenge row takes over again.
        persistence.delete_pending_factory_reset();
        let m2 = LaunchMachine::new(Arc::new(NullObserver), persistence);
        m2.start().await;
        assert_ne!(
            m2.snapshot().phase,
            LaunchPhase::WizardAt {
                entry: LaunchWizardEntry::PendingFactoryReset
            },
            "a cleared slot must not re-route the next launch"
        );
    }

    /// The slot is per-actor, so signing out (the cleanup contract) must take it
    /// with the identity — a survivor would strand the next launch on a
    /// pre-filled claim for a box whose admin secret is gone.
    #[test]
    fn sign_out_sweeps_the_pending_factory_reset_slot() {
        let store = Arc::new(InMemorySecretStore::new());
        let reg = AccountRegistry::new(store.clone());
        let a = reg.add_account(SECRET_A, None, None).unwrap();
        let adapter = plain_adapter(store.clone());

        mint_and_persist_pending_factory_reset(
            &adapter,
            "https://nest.example".into(),
            "alice".into(),
        )
        .expect("the registry-backed store must persist the row");
        assert!(adapter.load_pending_factory_reset().is_some());

        let _ = reg.clear_all(); // sign-out

        assert_eq!(store.get(&crate::pending_factory_reset_key(&a)), None);
        assert_eq!(adapter.load_pending_factory_reset(), None);
    }

    /// **The CR-3 success criterion, literally:** two accounts on one install
    /// can EACH hold an outstanding factory reset without either losing its
    /// claim code — the collision the single-global-slot layout made
    /// inevitable (B's reset overwrote A's row; A's code was gone with no
    /// client able to learn it).
    #[test]
    fn two_accounts_each_keep_their_own_pending_reset() {
        let store = Arc::new(InMemorySecretStore::new());
        let reg = AccountRegistry::new(store.clone());
        let a = reg
            .add_account(SECRET_A, Some("https://a.example"), None)
            .unwrap();
        let b = reg
            .add_account(SECRET_B, Some("https://b.example"), None)
            .unwrap();
        let adapter = plain_adapter(store.clone());

        // A (active) dispatches a reset…
        let code_a = mint_and_persist_pending_factory_reset(
            &adapter,
            "https://a.example".into(),
            "a".into(),
        )
        .unwrap();
        // …then the user switches to B, which dispatches its own.
        reg.set_active(&b).unwrap();
        let code_b = mint_and_persist_pending_factory_reset(
            &adapter,
            "https://b.example".into(),
            "b".into(),
        )
        .unwrap();

        // B's write must NOT have clobbered A's row.
        assert_eq!(
            adapter.load_pending_factory_reset().unwrap().claim_code,
            code_b
        );
        reg.set_active(&a).unwrap();
        assert_eq!(
            adapter.load_pending_factory_reset().unwrap().claim_code,
            code_a,
            "account A's claim code must survive account B's factory reset"
        );

        // Each account's re-claim clears only its own slot.
        adapter.delete_pending_factory_reset(); // active = A
        assert_eq!(adapter.load_pending_factory_reset(), None);
        reg.set_active(&b).unwrap();
        assert_eq!(
            adapter.load_pending_factory_reset().unwrap().claim_code,
            code_b
        );
    }

    // --- Concurrent instances (`account-scoping.md` § Concurrent instances):
    // the BOUND adapter — bind-without-activate. -------------------------------

    /// The core decoupling: a bound adapter resolves the NAMED account's
    /// slots while a different account holds the active pointer — and the
    /// unbound adapter over the same store keeps following `active`.
    #[test]
    fn bound_adapter_reads_the_named_account_not_the_active_one() {
        let store = Arc::new(InMemorySecretStore::new());
        let reg = AccountRegistry::new(store.clone());
        reg.add_account(SECRET_A, Some("https://a.example"), None)
            .unwrap();
        let b = reg
            .add_account(SECRET_B, Some("https://b.example"), None)
            .unwrap();
        // A is active (first added); bind to B.
        let bound = bound_adapter(store.clone(), b.clone());
        let unbound = plain_adapter(store);

        assert_eq!(bound.load_identity(), Some(secret_bytes(SECRET_B)));
        assert_eq!(bound.load_nest_url().as_deref(), Some("https://b.example"));
        assert_eq!(unbound.load_identity(), Some(secret_bytes(SECRET_A)));

        // And a later switch moves the unbound adapter only.
        reg.set_active(&b).unwrap();
        assert_eq!(unbound.load_identity(), Some(secret_bytes(SECRET_B)));
        assert_eq!(bound.load_identity(), Some(secret_bytes(SECRET_B)));
    }

    /// Binding is a pure read end-to-end: gate check + a full sweep of the
    /// bound adapter's loads write nothing.
    #[test]
    fn bind_gate_and_bound_loads_never_write() {
        let counting = Arc::new(crate::tests_support::WriteCountingStore::new());
        // A migrated two-account store with A active (setup writes counted,
        // then baselined away — the assertion is on the DELTA).
        let reg = AccountRegistry::new(counting.clone());
        reg.add_account(SECRET_A, Some("https://a.example"), None)
            .unwrap();
        let b = reg.add_account(SECRET_B, None, None).unwrap();
        let baseline = counting.writes().len();

        reg.bind_account(&b).unwrap();
        let bound = bound_adapter(counting.clone(), b);
        let _ = bound.load_identity();
        let _ = bound.load_nest_url();
        let _ = bound.load_pending_invite();
        let _ = bound.load_awaiting_dns();
        let _ = bound.load_pending_factory_reset();

        assert_eq!(
            counting.writes().len(),
            baseline,
            "a bound launch must not write anything (new writes: {:?})",
            &counting.writes()[baseline..]
        );
    }

    /// A bound session's authenticated save updates ITS account's slots and
    /// nothing shared: the active pointer stays put.
    #[test]
    fn bound_save_authenticated_leaves_the_active_pointer_alone() {
        let store = Arc::new(InMemorySecretStore::new());
        let reg = AccountRegistry::new(store.clone());
        let a = reg
            .add_account(SECRET_A, Some("https://a.example"), None)
            .unwrap();
        let b = reg.add_account(SECRET_B, None, None).unwrap();

        let bound = bound_adapter(store.clone(), b.clone());
        bound.save_authenticated(
            "https://b.example".into(),
            "bob".into(),
            "b.example".into(),
            "pro".into(),
        );

        // B's own slots + cache updated…
        assert_eq!(
            reg.secrets(&b).unwrap().nest_url.as_deref(),
            Some("https://b.example")
        );
        let entry_b = reg.list().into_iter().find(|e| e.actor_id == b).unwrap();
        assert_eq!(entry_b.handle.as_deref(), Some("bob"));
        // …while the active pointer still belongs to A.
        assert_eq!(reg.active(), Some(a));
    }

    /// A bound instance whose account was removed elsewhere must **refuse to
    /// mint** a claim code, not write one into a swept namespace.
    ///
    /// `save_authenticated` already disarms on exactly this state (its
    /// `update_cache` membership error). `save_pending_factory_reset` is a raw
    /// slot write, so without the same gate a bound instance would persist a
    /// box-claiming credential under a removed account: unreachable by every
    /// reader (the bind gate refuses `UnknownActor`, unbound reads follow
    /// `active`) and never swept by `clear_all` (which enumerates the index),
    /// so it outlives the sign-out erase in the OS keychain — and the mint's
    /// read-back would still succeed, so the caller would go on to wipe a real
    /// box against a code nobody can reach (CR-1).
    #[test]
    fn bound_factory_reset_on_a_removed_account_refuses_before_dispatch() {
        let counting = Arc::new(crate::tests_support::WriteCountingStore::new());
        let reg = AccountRegistry::new(counting.clone());
        reg.add_account(SECRET_A, None, None).unwrap();
        let b = reg.add_account(SECRET_B, None, None).unwrap();
        let bound = bound_adapter(counting.clone(), b.clone());
        // The primary instance removes the account this one is bound to.
        reg.remove(&b).unwrap();
        let baseline = counting.writes().len();

        let code = mint_and_persist_pending_factory_reset(
            &bound,
            "https://b.example".into(),
            "bob".into(),
        );

        assert_eq!(
            code, None,
            "a removed account owns no re-claim — the caller must not dispatch the reset"
        );
        assert_eq!(
            counting.writes().len(),
            baseline,
            "no claim-code slot may land in a removed account's namespace (new writes: {:?})",
            &counting.writes()[baseline..]
        );
    }

    /// End-to-end through the REAL machine: a machine over a bound adapter
    /// routes on the bound account while a different account is active — the
    /// secondary-instance launch shape.
    #[tokio::test]
    async fn real_launch_machine_routes_on_the_bound_account_while_another_is_active() {
        use fauna_launch_machine::{LaunchMachine, LaunchPhase, LaunchWizardEntry, NullObserver};

        let store = Arc::new(InMemorySecretStore::new());
        let reg = AccountRegistry::new(store.clone());
        // A (active): identity + pending invite → would route InviteRequest.
        let a = reg.add_account(SECRET_A, None, None).unwrap();
        let rec = PendingInviteRecord {
            nest_url: "https://nest.example".into(),
            handle: "alice".into(),
            request_id: "req-1".into(),
            status_json: "{}".into(),
        };
        reg.set_pending_invite_json(&a, &serde_json::to_string(&rec).unwrap());
        // B: identity only → HandleEntry.
        let b = reg.add_account(SECRET_B, None, None).unwrap();
        assert_eq!(reg.active(), Some(a));

        let persistence: Arc<dyn LaunchPersistence> = Arc::new(bound_adapter(store, b));
        let m = LaunchMachine::new(Arc::new(NullObserver), persistence);
        m.start().await;
        assert_eq!(
            m.snapshot().phase,
            LaunchPhase::WizardAt {
                entry: LaunchWizardEntry::HandleEntry
            },
            "the bound machine must route on B's slots, not the active A's"
        );
    }

    // --- Wizard-exit slot writers -------------------------------------------

    fn dns_record(handle: &str) -> AwaitingDnsRecord {
        AwaitingDnsRecord {
            nest_url: "https://nest.example".into(),
            handle: handle.into(),
            dns_records_json: r#"[{"record_type":"A","name":"@","value":"203.0.113.7"}]"#.into(),
            claim_code: "CODE".into(),
            reach_ipv4: None,
            nest_actor_id: None,
        }
    }

    /// **The regression this family exists for.** The deferred-DNS exit can be
    /// reached with NO handle yet — the wizard goes identity → provisioning →
    /// `DnsPostInstructions` without a handle stage — and the resume must still
    /// survive a force-quit. The per-actor slot carries opaque JSON, so an empty
    /// field is data, not absence.
    ///
    /// The pre-registry per-field layout could not express this: it dropped any
    /// empty required field, so the whole record composed as *absent* and the
    /// relaunch fell through to `HandleEntry` — a half-provisioned nest lost,
    /// which is what `test_smoke_i` caught on apple.
    #[test]
    fn awaiting_dns_with_an_empty_handle_still_resumes() {
        let store = Arc::new(InMemorySecretStore::new());
        let reg = AccountRegistry::new(store.clone());

        persist_awaiting_dns(&reg, SECRET_A, &dns_record("")).unwrap();

        let rec = plain_adapter(store)
            .load_awaiting_dns()
            .expect("an empty handle must not compose the slot away");
        assert_eq!(rec.handle, "");
        assert_eq!(rec.claim_code, "CODE");
        assert!(rec.dns_records_json.contains("203.0.113.7"));
    }

    /// **The pending-provision slot's completion rule.** The slot is written
    /// twice on a deferred-DNS run — once before `create_server` (claim code, no
    /// records, and later the reach address) and once at the `AwaitingManualDns`
    /// exit (the records to paste, and no address, because the exit has never
    /// heard of one). The goal doc calls the second write a *completion*
    /// (`onboarding.md` § 6 *Deferred-DNS path*), so an incoming `None` must mean
    /// "nothing to say about the address", not "forget it".
    ///
    /// Without this the crash resume regresses in the case it matters most: the
    /// deferred path is the one where the box is unreachable by domain for hours,
    /// so the reach address is the *only* way back to it.
    #[test]
    fn the_exit_completes_the_reach_address_rather_than_clearing_it() {
        let store = Arc::new(InMemorySecretStore::new());
        let reg = AccountRegistry::new(store.clone());

        // 1. The pending-provision write, completed with the box's address.
        let mut provisioned = dns_record("alice");
        provisioned.dns_records_json = String::new();
        provisioned.reach_ipv4 = Some("203.0.113.9".into());
        persist_awaiting_dns(&reg, SECRET_A, &provisioned).unwrap();

        // 2. The deferred-DNS exit, arriving later with the records and no address.
        persist_awaiting_dns(&reg, SECRET_A, &dns_record("alice")).unwrap();

        let rec = plain_adapter(store).load_awaiting_dns().expect("slot");
        assert_eq!(
            rec.reach_ipv4.as_deref(),
            Some("203.0.113.9"),
            "the exit must complete the row, not replace it — losing the address \
             here strands the resume on DNS propagation"
        );
        assert!(
            rec.dns_records_json.contains("203.0.113.7"),
            "and the records it DID bring must land"
        );
    }

    /// The identity the box was built with is completed, not cleared, by the
    /// same rule: the pending-provision write carries it, the deferred-DNS exit
    /// (which has never heard of it) must not erase it — a resumed surface with
    /// no identity to hold would fall back to TOFU on a box whose identity the
    /// client knew a priori (`security.md` § Transport trust, the
    /// *Client-provisioned box* row: "no TOFU window").
    #[test]
    fn the_exit_completes_the_built_with_identity_rather_than_clearing_it() {
        let store = Arc::new(InMemorySecretStore::new());
        let reg = AccountRegistry::new(store.clone());

        let mut provisioned = dns_record("alice");
        provisioned.dns_records_json = String::new();
        provisioned.reach_ipv4 = Some("203.0.113.9".into());
        provisioned.nest_actor_id = Some("ab".repeat(32));
        persist_awaiting_dns(&reg, SECRET_A, &provisioned).unwrap();

        persist_awaiting_dns(&reg, SECRET_A, &dns_record("alice")).unwrap();

        let rec = plain_adapter(store).load_awaiting_dns().expect("slot");
        assert_eq!(
            rec.nest_actor_id.as_deref(),
            Some("ab".repeat(32).as_str()),
            "the exit must keep the identity the box was built with"
        );
        assert_eq!(rec.reach_ipv4.as_deref(), Some("203.0.113.9"));
    }

    /// The other direction of the same rule: a write that *carries* an address
    /// is authoritative. Otherwise the completion could never correct a stale
    /// address (a retried run builds a new box at a new IP).
    #[test]
    fn a_write_carrying_an_address_replaces_the_stored_one() {
        let store = Arc::new(InMemorySecretStore::new());
        let reg = AccountRegistry::new(store.clone());

        let mut first = dns_record("alice");
        first.reach_ipv4 = Some("203.0.113.9".into());
        persist_awaiting_dns(&reg, SECRET_A, &first).unwrap();

        let mut retried = dns_record("alice");
        retried.reach_ipv4 = Some("198.51.100.4".into());
        persist_awaiting_dns(&reg, SECRET_A, &retried).unwrap();

        assert_eq!(
            plain_adapter(store)
                .load_awaiting_dns()
                .expect("slot")
                .reach_ipv4
                .as_deref(),
            Some("198.51.100.4"),
        );
    }

    /// A record written before `reach_ipv4` existed must still deserialize —
    /// the `#[serde(default)]` half of the goal doc's store contract
    /// (`onboarding.md` § Long-term store contract), and the reason a slot
    /// written by yesterday's client still resumes today.
    #[test]
    fn a_pre_reach_address_record_still_loads() {
        let store = Arc::new(InMemorySecretStore::new());
        let reg = AccountRegistry::new(store.clone());
        let actor = reg.add_account(SECRET_A, None, None).unwrap();
        let _ = reg.set_active(&actor);
        reg.set_awaiting_dns_json(
            &actor,
            r#"{"nest_url":"https://nest.example","handle":"alice","dns_records_json":"[]","claim_code":"CODE"}"#,
        );

        let rec = plain_adapter(store)
            .load_awaiting_dns()
            .expect("a record from before the field existed must still resume");
        assert_eq!(rec.claim_code, "CODE");
        assert_eq!(rec.reach_ipv4, None);
        assert_eq!(rec.nest_actor_id, None);
    }

    /// The [`PendingProvisionStore`] contract: the returned record is the one
    /// read back **from the store**, so a caller can tell a durable write from a
    /// swallowed one. This is what `mint_and_persist_pending_provision` leans on
    /// to refuse building a box whose claim code did not survive.
    #[test]
    fn the_pending_provision_store_returns_what_the_store_read_back() {
        let store = Arc::new(InMemorySecretStore::new());
        let reg = AccountRegistry::new(store.clone());
        let writer = reg.pending_provision_store();

        let mut rec = dns_record("alice");
        rec.reach_ipv4 = Some("203.0.113.9".into());
        let back = writer
            .save_awaiting_dns(SECRET_A.into(), rec.clone())
            .expect("a taking store returns the row");
        assert_eq!(back, rec);
        assert_eq!(
            plain_adapter(store).load_awaiting_dns().as_ref(),
            Some(&back)
        );
    }

    /// **The third case of the carry-forward rule** — the one the two tests
    /// above pin either side of, and the one found
    /// missing: an incoming `None` that means **absent**, not *unspecified*.
    ///
    /// `mint_and_persist_pending_provision` writes `reach_ipv4: None` because
    /// the box does not exist yet, not because it has nothing to say. Read as a
    /// completion, that mint inherited the PREVIOUS box's address: the user
    /// starts over onto another domain (which per `onboarding.md` § 6 does *not*
    /// clear the slot), and the fresh row carries box B's url, handle and claim
    /// code beside box A's address. The resume then dials the abandoned box —
    /// whose IPv4 the provider recycles to another tenant within hours — and
    /// presents box B's claim code to it.
    ///
    /// So every write through [`PendingProvisionStore`] is
    /// [`SlotWrite::Authoritative`]: its callers hold the whole row.
    #[test]
    fn a_mint_does_not_inherit_the_previous_boxs_reach_address() {
        let store = Arc::new(InMemorySecretStore::new());
        let reg = AccountRegistry::new(store.clone());

        // Run 1: a box was built at .9 and the slot completed with its address.
        assert!(
            fauna_launch_machine::complete_pending_provision_reach(
                &reg.pending_provision_store(),
                SECRET_A.into(),
                "https://a.example".into(),
                "alice@a.example".into(),
                "AAAA-1111".into(),
                Some("ab".repeat(32)),
                "203.0.113.9".into(),
            ),
            "the completion must read back"
        );

        // The user hits "start over" and provisions a different domain. The
        // wizard's reset deliberately keeps the slot (§ 6), so this mint lands
        // on top of run 1's row.
        let code = fauna_launch_machine::mint_and_persist_pending_provision(
            &reg.pending_provision_store(),
            SECRET_A.into(),
            "https://b.example".into(),
            "alice@b.example".into(),
            Some("cd".repeat(32)),
            None,
        )
        .expect("the mint must read back");

        let rec = plain_adapter(store).load_awaiting_dns().expect("slot");
        assert_eq!(rec.nest_url, "https://b.example");
        assert_eq!(rec.claim_code, code);
        assert_eq!(
            rec.reach_ipv4, None,
            "a box that does not exist yet has no address — inheriting run 1's \
             would dial the abandoned box (whose IP is recycled) and present \
             this box's claim code to it"
        );
        assert_eq!(
            rec.nest_actor_id.as_deref(),
            Some("cd".repeat(32).as_str()),
            "and the identity it will boot with is this run's, not run 1's"
        );
    }

    /// The other half of "authoritative": the retry re-persist and the reach
    /// completion each hold the full row, so the store path never needs the
    /// carry-forward — while the deferred-DNS exit, which does not go through
    /// [`PendingProvisionStore`], still completes (pinned above).
    #[test]
    fn the_exit_still_completes_a_row_the_store_path_wrote() {
        let store = Arc::new(InMemorySecretStore::new());
        let reg = AccountRegistry::new(store.clone());

        let code = fauna_launch_machine::mint_and_persist_pending_provision(
            &reg.pending_provision_store(),
            SECRET_A.into(),
            "https://nest.example".into(),
            "alice".into(),
            Some("ab".repeat(32)),
            None,
        )
        .expect("mint");
        assert!(fauna_launch_machine::complete_pending_provision_reach(
            &reg.pending_provision_store(),
            SECRET_A.into(),
            "https://nest.example".into(),
            "alice".into(),
            code,
            Some("ab".repeat(32)),
            "203.0.113.9".into(),
        ));

        // The deferred-DNS exit, arriving with the records and no address.
        persist_awaiting_dns(&reg, SECRET_A, &dns_record("alice")).unwrap();

        let rec = plain_adapter(store).load_awaiting_dns().expect("slot");
        assert_eq!(rec.reach_ipv4.as_deref(), Some("203.0.113.9"));
        assert_eq!(rec.nest_actor_id.as_deref(), Some("ab".repeat(32).as_str()));
        assert!(rec.dns_records_json.contains("203.0.113.7"));
    }

    /// The latent half of the same bug: a pre-registry global write was
    /// invisible the moment an index existed, so an apple user who had ever
    /// added a second account lost a deferred-DNS nest even WITH a handle. The
    /// per-actor write is immune — this is the multi-account case working.
    #[test]
    fn awaiting_dns_survives_on_an_indexed_multi_account_install() {
        let store = Arc::new(InMemorySecretStore::new());
        let reg = AccountRegistry::new(store.clone());
        let b = reg
            .add_account(SECRET_B, Some("https://b.example"), None)
            .unwrap();

        // A provisions a deferred-DNS nest while B is already registered.
        let a = persist_awaiting_dns(&reg, SECRET_A, &dns_record("alice")).unwrap();
        let adapter = plain_adapter(store);
        assert_eq!(
            adapter.load_awaiting_dns().map(|r| r.claim_code),
            Some("CODE".into())
        );

        // B must not inherit it.
        reg.set_active(&b).unwrap();
        assert_eq!(
            adapter.load_awaiting_dns(),
            None,
            "account B never deferred a nest — it must not see A's slot"
        );
        reg.set_active(&a).unwrap();
        assert!(adapter.load_awaiting_dns().is_some());
    }

    /// End-to-end through the REAL launch machine: the exact `test_smoke_i`
    /// journey (deferred-DNS exit with no handle, then a relaunch) routes to
    /// the "Almost ready" surface instead of dropping to `HandleEntry`.
    #[tokio::test]
    async fn relaunch_after_a_handle_less_deferred_dns_exit_routes_to_awaiting_dns() {
        use fauna_launch_machine::{LaunchMachine, LaunchPhase, LaunchWizardEntry, NullObserver};

        let store = Arc::new(InMemorySecretStore::new());
        let reg = AccountRegistry::new(store.clone());
        persist_awaiting_dns(&reg, SECRET_A, &dns_record("")).unwrap();

        let persistence: Arc<dyn LaunchPersistence> = Arc::new(plain_adapter(store));
        let m = LaunchMachine::new(Arc::new(NullObserver), persistence);
        m.start().await;

        assert_eq!(
            m.snapshot().phase,
            LaunchPhase::WizardAt {
                entry: LaunchWizardEntry::AwaitingManualDns
            },
            "the relaunch must resume the deferred nest, not drop to the handle stage"
        );
    }

    /// The claim terminal clears only the active account's slot.
    #[test]
    fn clearing_at_the_claim_terminal_is_scoped_to_the_active_account() {
        let store = Arc::new(InMemorySecretStore::new());
        let reg = AccountRegistry::new(store.clone());
        persist_awaiting_dns(&reg, SECRET_A, &dns_record("alice")).unwrap();
        let adapter = plain_adapter(store);
        assert!(adapter.load_awaiting_dns().is_some());

        clear_awaiting_dns_for_active(&reg);

        assert_eq!(
            adapter.load_awaiting_dns(),
            None,
            "a completed claim must not pin the admin on 'Almost ready' forever"
        );
    }

    /// The "Almost ready" exit is addressed by the identity being ONBOARDED. On an
    /// append run that is not the active account — the live session keeps the
    /// account the user is adding from — so an exit that cleared the active
    /// account's slot would leave the abandoned box's slot behind and route every
    /// relaunch back onto the surface.
    #[test]
    fn the_almost_ready_exit_clears_the_onboarded_identitys_slot_never_the_active_accounts() {
        let store = Arc::new(InMemorySecretStore::new());
        let reg = AccountRegistry::new(store);
        let live = reg.add_account(SECRET_B, None, None).unwrap();
        let appended = reg.add_account(SECRET_A, None, None).unwrap();
        assert_eq!(
            reg.active().as_deref(),
            Some(live.as_str()),
            "precondition: appending an identity leaves the live session's account active"
        );
        let pending = RegistryPendingProvisionStore::from_registry(reg.clone());
        pending
            .save_awaiting_dns(SECRET_A.into(), dns_record("appended"))
            .unwrap();
        pending
            .save_awaiting_dns(SECRET_B.into(), dns_record("live"))
            .unwrap();

        pending.clear_awaiting_dns(SECRET_A.into());

        assert_eq!(
            reg.awaiting_dns_json(&appended),
            None,
            "the abandoned box's slot must be gone, or every relaunch resurrects the surface"
        );
        assert!(
            reg.awaiting_dns_json(&live).is_some(),
            "the active account's own slot is not the exit's to clear"
        );
    }

    /// A secret that names no identity names no slot: the exit is a no-op, not a
    /// panic, and touches nothing else.
    #[test]
    fn the_almost_ready_exit_with_an_invalid_secret_touches_nothing() {
        let store = Arc::new(InMemorySecretStore::new());
        let reg = AccountRegistry::new(store);
        let live = reg.add_account(SECRET_B, None, None).unwrap();
        let pending = RegistryPendingProvisionStore::from_registry(reg.clone());
        pending
            .save_awaiting_dns(SECRET_B.into(), dns_record("live"))
            .unwrap();

        pending.clear_awaiting_dns("not-hex".into());

        assert!(reg.awaiting_dns_json(&live).is_some());
    }

    /// The pending-invite twin: same per-actor guarantee, so the invite slot
    /// stops depending on a non-empty handle and on the index being absent.
    #[test]
    fn pending_invite_persists_per_actor_and_survives_an_index() {
        let store = Arc::new(InMemorySecretStore::new());
        let reg = AccountRegistry::new(store.clone());
        reg.add_account(SECRET_B, None, None).unwrap();

        persist_pending_invite(
            &reg,
            SECRET_A,
            &PendingInviteRecord {
                nest_url: "https://nest.example".into(),
                handle: "alice".into(),
                request_id: "req-1".into(),
                status_json: "{}".into(),
            },
        )
        .unwrap();

        assert_eq!(
            plain_adapter(store)
                .load_pending_invite()
                .map(|r| r.request_id),
            Some("req-1".into())
        );
    }

    // ── Moment 1 of the two-moment write contract ────────────────────────
    //
    // `long-term-store.md` § "Long-term state is only ever written at two
    // specific moments" — 1. confirm-identity writes `secret_key`. These pin
    // the write *as the launch router reads it back*, because the whole point
    // of moment 1 is the router's case 2 (`secret_key` present, `node_url`
    // empty → wizard pre-seeded at HandleEntry).

    /// The durable commit point, asserted end to end: after confirm-identity
    /// the launch router finds an identity and **no** `node_url`, which is
    /// exactly the three-case routing's case 2. A `node_url` written here
    /// would divert the relaunch onto the silent-challenge row against a nest
    /// the user has not even chosen yet.
    #[test]
    fn a_confirmed_identity_is_readable_by_the_launch_router_with_no_nest_url() {
        let store = Arc::new(InMemorySecretStore::new());
        let reg = AccountRegistry::new(store.clone());

        let actor_id = persist_confirmed_identity(&reg, SECRET_A, false).unwrap();

        let adapter = plain_adapter(store);
        assert_eq!(
            adapter.load_identity(),
            Some(fauna_core::hex32::decode(SECRET_A).unwrap().to_vec()),
            "case 2 routing reads the identity through this adapter"
        );
        assert_eq!(
            adapter.load_nest_url(),
            None,
            "moment 2 has not happened — a nest_url here diverts case 2 onto the \
             silent-challenge row"
        );
        assert_eq!(
            reg.active().as_deref(),
            Some(actor_id.as_str()),
            "the identity the user just confirmed is the one the wizard continues with"
        );
    }

    /// Back-navigation re-confirming, or a wizard driven twice, must not grow
    /// the account list — the registry is keyed by the derived actor id, so
    /// the second write is the same row.
    #[test]
    fn confirming_the_same_identity_twice_registers_one_account() {
        let store = Arc::new(InMemorySecretStore::new());
        let reg = AccountRegistry::new(store);

        let first = persist_confirmed_identity(&reg, SECRET_A, false).unwrap();
        let second = persist_confirmed_identity(&reg, SECRET_A, false).unwrap();

        assert_eq!(first, second);
        assert_eq!(reg.list().len(), 1);
    }

    /// The **abandoned-wizard ghost** (`long-term-store.md` § Eager vs. lazy
    /// migration at native boot): confirm identity A, press Back, confirm a
    /// *different* identity B in the same first-run wizard. Moment 1 wrote a
    /// real, activated row for A the instant the user clicked Continue, and
    /// nothing on the Back path retracts it — so A survives as a handle-less,
    /// nest-less, activatable second row in the account switcher, the exact
    /// regression that ruling declares structurally impossible.
    ///
    /// Confirming B **is** the retraction signal: in a non-append wizard run
    /// there is one identity in flight, so a leftover secret-only row is a
    /// previous run of this same wizard that the user has just superseded.
    #[test]
    fn superseding_an_abandoned_identity_leaves_no_ghost_row() {
        let store = Arc::new(InMemorySecretStore::new());
        let reg = AccountRegistry::new(store);

        let abandoned = persist_confirmed_identity(&reg, SECRET_A, false).unwrap();
        let kept = persist_confirmed_identity(&reg, SECRET_B, false).unwrap();

        assert_eq!(
            reg.list()
                .iter()
                .map(|e| e.actor_id.clone())
                .collect::<Vec<_>>(),
            vec![kept.clone()],
            "the abandoned identity must not survive as a ghost switcher row"
        );
        assert_eq!(
            reg.active().as_deref(),
            Some(kept.as_str()),
            "the identity the user actually confirmed stays the active one"
        );
        assert!(
            reg.secrets(&abandoned).is_none(),
            "retiring the row must not orphan its per-actor slots — an orphaned \
             secret is the `long-term-store.md` migration-gap bug class"
        );
    }

    /// A third identity, for the arms that need a row besides A and B.
    const SECRET_C: &str = "3333333333333333333333333333333333333333333333333333333333333333";

    /// The retirement's blast radius, pinned from the other side. Each arm
    /// puts one mark of "this identity is more than a moment-1 stub" on an
    /// otherwise-provisional row and asserts the row survives a later confirm.
    /// A delete that got any of these wrong destroys key material, so they are
    /// enumerated rather than sampled.
    #[test]
    fn retirement_spares_every_row_that_is_more_than_a_moment_1_stub() {
        // (what the mark is, how to put it on B's row).
        type Mark = (&'static str, fn(&AccountRegistry, &str));
        let marks: &[Mark] = &[
            ("authenticated (a nest_url)", |reg, b| {
                reg.set_nest_url(b, "https://b.example")
            }),
            ("a cached handle", |reg, b| {
                reg.update_cache(b, Some("b"), Some("b.example"), None).ok();
            }),
            ("a device id", |reg, _b| {
                // The device-id slot has no standalone setter; `add_account` on
                // an actor already indexed writes the slot without adding a row.
                reg.add_account(SECRET_B, None, Some("device-b")).unwrap();
            }),
            ("a pending invite", |reg, b| {
                reg.set_pending_invite_json(b, "{\"nest_url\":\"https://b.example\"}")
            }),
            ("an awaiting-DNS wait", |reg, b| {
                reg.set_awaiting_dns_json(b, "{\"nest_url\":\"https://b.example\"}")
            }),
            ("a re-auth flag the user set", |reg, b| {
                reg.set_require_confirm(b, true).ok();
            }),
            ("a succession link", |reg, b| {
                // B is the predecessor of C, so B carries `succeeded_by`…
                let c = reg.add_account(SECRET_C, None, None).unwrap();
                reg.record_succession(b, &c).unwrap();
            }),
        ];

        for (what, mark) in marks {
            let reg = AccountRegistry::new(Arc::new(InMemorySecretStore::new()));
            let kept = reg.add_account(SECRET_B, None, None).unwrap();
            mark(&reg, &kept);

            let fresh = persist_confirmed_identity(&reg, SECRET_A, false).unwrap();

            let ids: Vec<String> = reg.list().into_iter().map(|e| e.actor_id).collect();
            assert!(
                ids.contains(&kept),
                "a row carrying {what} is not an abandoned moment-1 stub and must \
                 survive — deleting it destroys key material; rows were {ids:?}"
            );
            assert!(ids.contains(&fresh), "the fresh identity is always kept");
        }
    }

    /// The per-actor-slot half of the blast radius, generated from
    /// `PER_ACTOR_KEY_BUILDERS` itself (via `per_actor_keys`) rather than
    /// hand-listed — the same derivation `is_provisional`'s guard now uses.
    /// A builder added to that list is pinned here
    /// automatically; before that fix, a hand-enumerated guard could fall
    /// behind the list (as it had, silently, for `device_auth_key` /
    /// `backup_key_key` / `generation_keys_key` / `grant_registered_key` /
    /// `store_writer_key`) and this test would have caught it red.
    #[test]
    fn retirement_spares_a_row_carrying_any_current_per_actor_slot() {
        let probe_actor = actor_of(SECRET_B);
        let secret = crate::secret_key(&probe_actor);
        let other_slots: Vec<String> = crate::per_actor_keys(&probe_actor)
            .filter(|key| *key != secret)
            .collect();
        assert!(
            other_slots.len() >= 7,
            "sanity: PER_ACTOR_KEY_BUILDERS should carry at least the seven \
             originally-guarded non-secret slots; got {other_slots:?}"
        );

        for slot_key in &other_slots {
            let store = Arc::new(InMemorySecretStore::new());
            let reg = AccountRegistry::new(store.clone());
            let kept = reg.add_account(SECRET_B, None, None).unwrap();
            assert_eq!(kept, probe_actor, "SECRET_B's actor id is deterministic");

            // Write directly under the builder's own key shape — bypassing
            // any registry setter, since some of these slots (the
            // principal_bundle shapes, the T10 writer key) are written by
            // other crates, never by AccountRegistry itself.
            store.set(slot_key, "anything");

            let fresh = persist_confirmed_identity(&reg, SECRET_A, false).unwrap();

            let ids: Vec<String> = reg.list().into_iter().map(|e| e.actor_id).collect();
            assert!(
                ids.contains(&kept),
                "a row carrying {slot_key} is not an abandoned moment-1 stub and \
                 must survive — deleting it destroys key material; rows were {ids:?}"
            );
            assert!(ids.contains(&fresh), "the fresh identity is always kept");
        }
    }

    /// The auxiliary-namespace twin of the test above: the
    /// four `principal_bundle` slots and the bare-actor-id T10 writer key are
    /// written through `production_credential_store()` into the auxiliary
    /// `fauna-account-store` namespace, never the app's own store
    /// (`long-term-store.md` § Implementation status today, hole 3). Before
    /// `is_provisional` widened its guard to `has_any_per_actor_slot`, it
    /// read the app's own store alone, so a row still carrying one of these
    /// five slots only in the auxiliary store looked like a pristine
    /// moment-1 stub and was destroyed along with the real key material the
    /// auxiliary sweep swept with it.
    #[test]
    fn retirement_spares_a_row_carrying_a_slot_only_the_auxiliary_store_holds() {
        let probe_actor = actor_of(SECRET_B);
        let secret = crate::secret_key(&probe_actor);
        let other_slots: Vec<String> = crate::per_actor_keys(&probe_actor)
            .filter(|key| *key != secret)
            .collect();

        for slot_key in &other_slots {
            let store = Arc::new(InMemorySecretStore::new());
            let aux = Arc::new(InMemorySecretStore::new());
            let reg = AccountRegistry::new(store.clone())
                .also_erasing(vec![aux.clone() as Arc<dyn SecretStore>]);
            let kept = reg.add_account(SECRET_B, None, None).unwrap();
            assert_eq!(kept, probe_actor, "SECRET_B's actor id is deterministic");

            // The app's own store never sees this slot — only the auxiliary
            // namespace does, exactly as `fauna-account-store` holds it in
            // production.
            aux.set(slot_key, "anything");

            let fresh = persist_confirmed_identity(&reg, SECRET_A, false).unwrap();

            let ids: Vec<String> = reg.list().into_iter().map(|e| e.actor_id).collect();
            assert!(
                ids.contains(&kept),
                "a row carrying {slot_key} only in the auxiliary store is not an \
                 abandoned moment-1 stub and must survive — deleting it destroys \
                 key material; rows were {ids:?}"
            );
            assert!(ids.contains(&fresh), "the fresh identity is always kept");
        }
    }

    /// …and the other end of that last arm: the row a succession link *points
    /// at* is spared too, even though nothing has been written on it yet. A
    /// half-built chain must not be finished off by a delete.
    #[test]
    fn retirement_spares_the_successor_a_half_built_chain_points_at() {
        let reg = AccountRegistry::new(Arc::new(InMemorySecretStore::new()));
        let predecessor = reg
            .add_account(SECRET_B, Some("https://b.example"), None)
            .unwrap();
        let successor = reg.add_account(SECRET_C, None, None).unwrap();
        reg.record_succession(&predecessor, &successor).unwrap();

        persist_confirmed_identity(&reg, SECRET_A, false).unwrap();

        assert!(
            reg.list().iter().any(|e| e.actor_id == successor),
            "the successor is named by a chain, so it is not an abandoned stub"
        );
    }

    /// A row a *newer* build wrote unknown fields into is a row this build
    /// cannot judge, so it is never provisional — the forward-compatibility
    /// half of `AccountEntry::extra`'s "preserved verbatim" contract, applied
    /// to a delete rather than to a rewrite.
    #[test]
    fn retirement_spares_a_row_a_newer_build_wrote_fields_into() {
        let store = Arc::new(InMemorySecretStore::new());
        let reg = AccountRegistry::new(store.clone());
        let stranger = reg.add_account(SECRET_B, None, None).unwrap();

        // Put the index back the way a newer build would have left it.
        let raw = store.get(crate::INDEX_KEY).expect("index materialized");
        let mut idx: serde_json::Value = serde_json::from_str(&raw).unwrap();
        idx["accounts"][0]["some_future_field"] = serde_json::json!("written by a newer build");
        store.set(crate::INDEX_KEY, &idx.to_string());

        persist_confirmed_identity(&reg, SECRET_A, false).unwrap();

        assert!(
            reg.list().iter().any(|e| e.actor_id == stranger),
            "an unknown field is evidence of state this build cannot see"
        );
    }

    /// The retirement must not be able to strand the client with an index
    /// naming an account it no longer holds a secret for — the unlaunchable
    /// half-state `set_active`'s own secret guard exists to prevent
    /// (`nest/common.md` § Client-state recoverability), reached here from the
    /// delete side instead of the activate side.
    #[test]
    fn retirement_never_leaves_the_index_pointing_at_a_retired_row() {
        let reg = AccountRegistry::new(Arc::new(InMemorySecretStore::new()));

        persist_confirmed_identity(&reg, SECRET_A, false).unwrap();
        let kept = persist_confirmed_identity(&reg, SECRET_B, false).unwrap();

        let active = reg.active().expect("an account is always active here");
        assert_eq!(active, kept);
        assert!(
            reg.secrets(&active).is_some(),
            "the active account must be launchable"
        );
    }

    /// Abandoning twice still converges on one row: the sweep is over *every*
    /// other provisional row, not just the most recent, so a wizard the user
    /// walked away from three times does not leave two ghosts behind.
    #[test]
    fn retirement_sweeps_every_abandoned_run_not_just_the_last() {
        let store = Arc::new(InMemorySecretStore::new());
        let reg = AccountRegistry::new(store.clone());
        // Two earlier abandoned runs, written exactly as moment 1 writes them.
        for secret in [SECRET_B, SECRET_C] {
            let id = reg.add_account(secret, None, None).unwrap();
            reg.set_active(&id).unwrap();
        }

        let kept = persist_confirmed_identity(&reg, SECRET_A, false).unwrap();

        assert_eq!(
            reg.list()
                .into_iter()
                .map(|e| e.actor_id)
                .collect::<Vec<_>>(),
            vec![kept],
            "both abandoned runs are swept, not merely the newest"
        );
    }

    /// A user who signed out (or whose silent challenge failed) still has a
    /// registered account, so `add_account`'s "first one becomes active" rule
    /// would leave the *old* identity active and route the relaunch to it.
    /// The explicit `set_active` is what makes the freshly confirmed identity
    /// the one case 2 resumes — the same reason its two resume-slot siblings
    /// make the call.
    #[test]
    fn a_confirmed_identity_becomes_active_over_an_already_registered_one() {
        let store = Arc::new(InMemorySecretStore::new());
        let reg = AccountRegistry::new(store.clone());
        reg.add_account(SECRET_B, Some("https://b.example"), None)
            .unwrap();

        let fresh = persist_confirmed_identity(&reg, SECRET_A, false).unwrap();

        assert_eq!(reg.active().as_deref(), Some(fresh.as_str()));
        assert_eq!(
            plain_adapter(store).load_nest_url(),
            None,
            "the active account is the fresh one, which has no nest yet — not B's"
        );
    }

    /// A store that takes every write and keeps none — the shape of a keystore
    /// the OS refused (a locked keyring, a revoked entitlement). `set` cannot
    /// report it, so only a read-back can.
    #[derive(Default)]
    struct AmnesiacStore;

    impl SecretStore for AmnesiacStore {
        fn get(&self, _key: &str) -> Option<String> {
            None
        }
        fn set(&self, _key: &str, _value: &str) {}
        fn delete(&self, _key: &str) {}
    }

    /// The whole point, through the **real** `LaunchMachine`: confirm-identity
    /// → force-quit → relaunch lands on `HandleEntry` with the identity intact,
    /// rather than back at `IdentityChoice` with the secret destroyed. This is
    /// case 2 of § Three-case launch routing, and moment 1 is the only thing
    /// that makes it reachable.
    #[tokio::test]
    async fn a_force_quit_after_confirm_identity_relaunches_at_handle_entry() {
        use fauna_launch_machine::{LaunchMachine, LaunchPhase, LaunchWizardEntry, NullObserver};

        let store = Arc::new(InMemorySecretStore::new());
        let reg = AccountRegistry::new(store.clone());

        persist_confirmed_identity(&reg, SECRET_A, false).unwrap();

        // A relaunch is a fresh machine over the store the wizard left behind.
        let m = LaunchMachine::new(Arc::new(NullObserver), Arc::new(plain_adapter(store)));
        m.start().await;

        assert_eq!(
            m.snapshot().phase,
            LaunchPhase::WizardAt {
                entry: LaunchWizardEntry::HandleEntry
            },
            "the user resumes where they left off — a wizard restart here means \
             a generated secret that existed nowhere else is gone"
        );
    }

    /// The failure mode that must never be silent: the wizard advances past
    /// confirm-identity believing the secret is durable, the user closes the
    /// app, and a freshly generated identity that existed nowhere else is gone.
    /// `Ok` from this function is a promise the secret reads back.
    #[test]
    fn a_keystore_that_silently_drops_the_write_is_an_error_not_a_success() {
        let reg = AccountRegistry::new(Arc::new(AmnesiacStore));

        assert!(
            matches!(
                persist_confirmed_identity(&reg, SECRET_A, false),
                Err(crate::AccountError::NoStoredSecret(_))
            ),
            "an infallible `set` that kept nothing must surface, not report success"
        );
    }

    // ── The pending-provision store never moves the active pointer ────────
    //
    // The mint holds CUSTODY (`onboarding-provisioning.md` § 6 *The
    // pending-provision slot*): the identity's secret and the slot must be
    // durable before `create_server`, or a quit orphans a paid box. What it
    // must NOT do is decide the next launch's routing — activation belongs to
    // moment 1 (the first-run confirm) and to the wizard's terminal (the
    // append's registration-and-switch), never to a mid-run write.

    /// An "Add account" provisioning run over a live session: the mint
    /// registers the appended identity beside its slot and leaves `active` on
    /// the account the user is adding FROM. Before this, `write_awaiting_dns`'s
    /// unconditional `set_active` made the appended identity active mid-run,
    /// so abandoning the append — or a quit before its terminal — re-dispatched
    /// the live session over a half-onboarded identity (the hijack
    /// `onboarding.md` § Multi-account forbids). The same holds for the two
    /// other writes on the store path, the reach completion and the retry
    /// re-persist, and the custody row is no provisional ghost: a box may be
    /// billing under it, so a later first-run sweep keeps it.
    #[test]
    fn an_append_mode_mint_registers_the_appended_identity_but_never_moves_the_active_pointer() {
        let store = Arc::new(InMemorySecretStore::new());
        let reg = AccountRegistry::new(store.clone());
        // The live account: onboarded first-run, home nest recorded.
        let live = persist_confirmed_identity(&reg, SECRET_A, false).unwrap();
        persist_logged_in(&reg, SECRET_A, "https://a.example", Some("dev-a"), None).unwrap();
        // "Add account": the append confirm writes nothing (moment 1's append arm).
        let appended = persist_confirmed_identity(&reg, SECRET_B, true).unwrap();
        assert_eq!(
            reg.list().len(),
            1,
            "precondition: the append confirm wrote nothing"
        );

        let code = fauna_launch_machine::mint_and_persist_pending_provision(
            &reg.pending_provision_store(),
            SECRET_B.into(),
            "https://b.example".into(),
            "bob@b.example".into(),
            Some("cd".repeat(32)),
            None,
        )
        .expect("custody precedes dispatch: the mint must read back");

        // Custody: the appended identity is registered, its secret durable, its
        // slot readable under ITS OWN actor id.
        assert_eq!(
            reg.list()
                .into_iter()
                .map(|e| e.actor_id)
                .collect::<Vec<_>>(),
            vec![live.clone(), appended.clone()],
            "the mint registers the appended identity beside the live one"
        );
        assert!(
            reg.secrets(&appended).is_some(),
            "its secret must be durable before the box exists"
        );
        let slot: AwaitingDnsRecord =
            serde_json::from_str(&reg.awaiting_dns_json(&appended).expect("the appended slot"))
                .unwrap();
        assert_eq!(slot.claim_code, code);

        // Routing: untouched. The next launch still resumes the live account and
        // sees no slot to mis-route on — the slot is the appended account's.
        assert_eq!(
            reg.active().as_deref(),
            Some(live.as_str()),
            "a mid-run custody write must never move the active pointer"
        );
        let launch = plain_adapter(store.clone());
        assert_eq!(launch.load_nest_url().as_deref(), Some("https://a.example"));
        assert!(launch.load_awaiting_dns().is_none());

        // The reach completion and the retry re-persist ride the same store path
        // and keep the same promise.
        assert!(fauna_launch_machine::complete_pending_provision_reach(
            &reg.pending_provision_store(),
            SECRET_B.into(),
            "https://b.example".into(),
            "bob@b.example".into(),
            code.clone(),
            Some("cd".repeat(32)),
            "203.0.113.9".into(),
        ));
        assert_eq!(
            reg.active().as_deref(),
            Some(live.as_str()),
            "the reach completion"
        );
        let row: AwaitingDnsRecord =
            serde_json::from_str(&reg.awaiting_dns_json(&appended).unwrap()).unwrap();
        assert!(
            fauna_launch_machine::persist_pending_provision(
                &reg.pending_provision_store(),
                SECRET_B.into(),
                row
            )
            .is_some()
        );
        assert_eq!(
            reg.active().as_deref(),
            Some(live.as_str()),
            "the retry re-persist"
        );

        // Not a ghost: the slot is what makes the row non-provisional, so the
        // sweep a later first-run confirm runs leaves the box's custody alone.
        assert!(
            reg.retire_superseded_provisionals(&live).is_empty(),
            "a row holding a pending-provision slot is custody of a box, never a \
             provisional to sweep"
        );
    }

    /// The deferred-DNS EXIT is the append's terminal — "the terminal registers
    /// and switches" (`onboarding.md` § Multi-account) — so it is the one
    /// awaiting-dns writer that DOES move the pointer: after it the appended
    /// identity is active and its slot carries the records to paste beside the
    /// address the run completed (the carry-forward rule).
    #[test]
    fn the_deferred_dns_exit_is_the_terminal_and_activates_the_appended_identity() {
        let store = Arc::new(InMemorySecretStore::new());
        let reg = AccountRegistry::new(store.clone());
        let live = persist_confirmed_identity(&reg, SECRET_A, false).unwrap();
        persist_logged_in(&reg, SECRET_A, "https://a.example", Some("dev-a"), None).unwrap();

        let code = fauna_launch_machine::mint_and_persist_pending_provision(
            &reg.pending_provision_store(),
            SECRET_B.into(),
            "https://b.example".into(),
            "bob@b.example".into(),
            Some("cd".repeat(32)),
            None,
        )
        .unwrap();
        assert!(fauna_launch_machine::complete_pending_provision_reach(
            &reg.pending_provision_store(),
            SECRET_B.into(),
            "https://b.example".into(),
            "bob@b.example".into(),
            code.clone(),
            Some("cd".repeat(32)),
            "203.0.113.9".into(),
        ));
        assert_eq!(
            reg.active().as_deref(),
            Some(live.as_str()),
            "mid-run: still the live account"
        );

        let mut exit = dns_record("bob@b.example");
        exit.nest_url = "https://b.example".into();
        exit.claim_code = code.clone();
        let appended = persist_awaiting_dns(&reg, SECRET_B, &exit).unwrap();

        assert_eq!(
            reg.active().as_deref(),
            Some(appended.as_str()),
            "the terminal is where the appended identity becomes the active one"
        );
        let resumed = plain_adapter(store)
            .load_awaiting_dns()
            .expect("the relaunch resumes it");
        assert_eq!(resumed.claim_code, code);
        assert_eq!(
            resumed.dns_records_json, exit.dns_records_json,
            "with the records to paste"
        );
        assert_eq!(
            resumed.reach_ipv4.as_deref(),
            Some("203.0.113.9"),
            "and the address the run completed, carried forward by the exit"
        );
    }

    /// The first-run side of the same rule. The ordinary first-run wizard has
    /// moment 1 activate the identity before the mint ever runs, so the mint
    /// never needed to; and a run seeded straight onto the provisioning page
    /// over an EMPTY registry (the e2e drives' `seed_identity`, which skips
    /// moment 1) still relaunches onto the "Almost ready" surface, because
    /// `add_account`'s first-account rule makes the only identity active.
    #[test]
    fn a_first_run_mint_still_leaves_the_relaunch_routing_onto_the_slot() {
        // Moment 1 ran: the identity is active before the mint.
        let store = Arc::new(InMemorySecretStore::new());
        let reg = AccountRegistry::new(store.clone());
        reg.add_account(SECRET_B, Some("https://old.example"), None)
            .unwrap(); // a signed-out install's earlier account
        let confirmed = persist_confirmed_identity(&reg, SECRET_A, false).unwrap();
        fauna_launch_machine::mint_and_persist_pending_provision(
            &reg.pending_provision_store(),
            SECRET_A.into(),
            "https://a.example".into(),
            "alice@a.example".into(),
            None,
            None,
        )
        .unwrap();
        assert_eq!(reg.active().as_deref(), Some(confirmed.as_str()));
        assert!(plain_adapter(store).load_awaiting_dns().is_some());

        // No moment 1, empty registry: the first account becomes active.
        let store = Arc::new(InMemorySecretStore::new());
        let reg = AccountRegistry::new(store.clone());
        fauna_launch_machine::mint_and_persist_pending_provision(
            &reg.pending_provision_store(),
            SECRET_A.into(),
            "https://a.example".into(),
            "alice@a.example".into(),
            None,
            None,
        )
        .unwrap();
        assert_eq!(reg.active().as_deref(), Some(actor_of(SECRET_A).as_str()));
        assert!(
            plain_adapter(store).load_awaiting_dns().is_some(),
            "a seeded first run still resumes on the slot after a relaunch"
        );
    }
}
