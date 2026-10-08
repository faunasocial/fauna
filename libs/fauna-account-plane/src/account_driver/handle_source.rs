//! Where a seat's live [`AccountStoreHandle`] is read from — the one shape
//! every plane-only consumer outside the runtime takes (the DNS management
//! record's store, the mail custody's store), and the bounded wait for a
//! runtime that has not come up yet.
//!
//! Every seat starts its account runtime fire-and-forget at sign-in (web only
//! once its conversations manager exists), so the first seconds after
//! `LoggedIn` can find no handle — exactly when onboarding seals a credential
//! it captured, or the mail auto-enable mints the account's MSEK. Waiting keeps
//! that write rather than dropping it; the bound keeps a runtime that failed
//! for the whole session from hanging the page.

use std::sync::Arc;
use std::time::Duration;

use fauna_client_config::{MailStore, StoreError};
use fauna_core::data::{MailConfig, MailCredential, MsekFingerprint};
use fauna_core::mail_rows::{MailRows, MailStateRow};

use super::AccountStoreHandle;

/// Where a seat's live account-store handle is read from, fresh on every use:
/// the seat's own "`None` before the assembly completes, after a sign-out, or
/// whenever it failed" source (`AccountRuntimeHost::handle` on linux and the
/// `fauna-ffi` seat, the `App`-owned slot on tui, the tab's runtime on web).
pub type AccountHandleSource = Arc<dyn Fn() -> Option<AccountStoreHandle> + Send + Sync>;

/// How long [`wait_for_account_handle`] waits for the account runtime to come
/// up before giving up (the module docs' first seconds after sign-in).
pub const ACCOUNT_HANDLE_WAIT: Duration = Duration::from_secs(60);
/// The poll interval of that wait.
const ACCOUNT_HANDLE_POLL: Duration = Duration::from_millis(250);

/// The seat's handle, polling `source` for up to [`ACCOUNT_HANDLE_WAIT`];
/// `None` (logged, naming `what`) once the wait runs out.
pub async fn wait_for_account_handle(
    source: &AccountHandleSource,
    what: &str,
) -> Option<AccountStoreHandle> {
    let mut waited = Duration::ZERO;
    loop {
        if let Some(handle) = source() {
            return Some(handle);
        }
        if waited >= ACCOUNT_HANDLE_WAIT {
            tracing::warn!("{what} found no account runtime after {ACCOUNT_HANDLE_WAIT:?}");
            return None;
        }
        fauna_sleep::sleep(ACCOUNT_HANDLE_POLL).await;
        waited += ACCOUNT_HANDLE_POLL;
    }
}

/// What a sourced store answers when no account runtime came up in time.
pub const RUNTIME_ABSENT: &str = "the account runtime is not running";

/// What a store-backed surface is handed to reach the account store: a live
/// [`AccountStoreHandle`], for a caller that already holds one, or the seat's
/// [`SeatAccountStore`], waited for.
#[allow(async_fn_in_trait)] // static dispatch only, as `RpcRequester`
pub trait AccountStoreAccess {
    /// The account store, for the gesture `what` names (the wait's log line).
    async fn account_store(&self, what: &str) -> anyhow::Result<AccountStoreHandle>;
}

impl AccountStoreAccess for AccountStoreHandle {
    async fn account_store(&self, _what: &str) -> anyhow::Result<AccountStoreHandle> {
        Ok(self.clone())
    }
}

/// The seat's account store as an app's page hands it to a store-backed
/// surface: the seat's [`AccountHandleSource`], **waited for**.
///
/// A page can be opened in the first seconds after sign-in, before the
/// runtime has assembled (the module docs). A gesture made through this then
/// waits for the runtime ([`wait_for_account_handle`]) rather than failing at
/// once, and fails with [`RUNTIME_ABSENT`] — it never reaches for another
/// store — when none comes (`config-dissolution.md` § The `__config`
/// dissolution schedule → *What replaces the bridge's two carriages*, case
/// (a)).
///
/// A named type rather than an impl on the source alias itself: a trait
/// implemented for `Arc<dyn Fn …>` is "not general enough" for the compiler
/// once the call sits inside a spawned (`Send`) future.
#[derive(Clone)]
pub struct SeatAccountStore(AccountHandleSource);

impl SeatAccountStore {
    pub fn new(source: AccountHandleSource) -> Self {
        Self(source)
    }
}

impl AccountStoreAccess for SeatAccountStore {
    async fn account_store(&self, what: &str) -> anyhow::Result<AccountStoreHandle> {
        wait_for_account_handle(&self.0, what)
            .await
            .ok_or_else(|| anyhow::anyhow!(RUNTIME_ABSENT))
    }
}

/// How long a read waits at the first-listing gate ([`first_listing_gate`]):
/// for the scope's first listing on a replica that has never listed it, and
/// for this launch's first pass on one that has.
pub const FIRST_PASS_WAIT: Duration = Duration::from_secs(30);

/// The refusal a gated read answers when the store has no answer to "what
/// does the account hold?" yet ([`first_listing_gate`]) — the one shared type
/// every gated read answers with. Transient: the read is good once what it
/// waits for has come. On a `StoreError` seam it is the not-ready class
/// `StoreError::is_not_ready` recognises ([`Self::into_load`]); a surface
/// renders it as its failed-load state with the reason [`Self::reason`] names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeNotReady {
    /// The account-state scope the read was of.
    pub scope: String,
    /// What the read waits for.
    pub reason: NotReadyReason,
}

/// What a refused read waits for (`account-client-lifecycle.md` § The
/// client-side lifecycle → *The first listing*, clauses (4) and (5)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotReadyReason {
    /// The replica has never listed the scope from its bound nest
    /// (clause (1)).
    NotListed,
    /// The unkeyed hold (clause (5)) with this device itself or the escrow
    /// holder a standing source of a held generation's key: the nest is what
    /// is missing, as for [`Self::NotListed`].
    HeldForNest,
    /// The unkeyed hold with a sibling device the only standing source:
    /// opening the app there, or removing that device, ends the wait.
    HeldForSibling,
}

impl NotReadyReason {
    /// Whether a surface says the nest is what is missing
    /// (`common.needs_nest`), rather than another device.
    pub fn needs_nest(self) -> bool {
        !matches!(self, Self::HeldForSibling)
    }
}

impl std::fmt::Display for ScopeNotReady {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.reason.needs_nest() {
            fauna_client_config::LEDGER_NOT_READY
        } else {
            fauna_client_config::LEDGER_AWAITING_SIBLING
        })
    }
}

impl std::error::Error for ScopeNotReady {}

impl ScopeNotReady {
    /// This refusal on a `StoreError` seam's read.
    pub fn into_load(self) -> StoreError {
        StoreError::Load(self.to_string())
    }

    /// The user-voice reason a surface shows for this refusal: the nest is
    /// what is missing (`common.needs_nest`), or another of the account's
    /// devices is (`common.needs_other_device`) — the failed-load state's
    /// reason, and the refused gesture's.
    pub fn reason_text(&self) -> &'static str {
        if self.reason.needs_nest() {
            fauna_i18n::strings::common::NEEDS_NEST
        } else {
            fauna_i18n::strings::common::NEEDS_OTHER_DEVICE
        }
    }

    /// [`Self::reason_text`] as its string key, for a surface that resolves
    /// text through its platform's own pipeline (the UniFFI apps).
    pub fn reason_key(&self) -> &'static str {
        if self.reason.needs_nest() {
            "common.needs_nest"
        } else {
            "common.needs_other_device"
        }
    }
}

/// [`ScopeNotReady::reason_key`] for an error that is the read gate's
/// refusal; `None` for any other failure.
pub fn not_ready_reason_key(e: &anyhow::Error) -> Option<&'static str> {
    e.downcast_ref::<ScopeNotReady>()
        .map(ScopeNotReady::reason_key)
}

/// [`ScopeNotReady::reason_text`] for an error that is the read gate's
/// refusal; `None` for any other failure, which a surface shows as itself.
pub fn not_ready_reason(e: &anyhow::Error) -> Option<&'static str> {
    e.downcast_ref::<ScopeNotReady>()
        .map(ScopeNotReady::reason_text)
}

/// **The first-listing gate** every read crosses that a write of a
/// latest-wins value is derived from (`account-client-lifecycle.md` § The
/// client-side lifecycle → *The first listing*, clauses (2) and (3)).
///
/// A freshly signed-in device, a successor's first launch and a re-created
/// store all hold only what they wrote themselves; the account's rows arrive
/// with the first listing of the scope from the bound nest. An empty answer
/// before it is worse than none — a page paints it as the account's value,
/// and a read-edit-write puts it back at a stamp of now, outranking the
/// account's real row on every device. So the read is keyed on the store's
/// durable **listed** fact for `scope` (`AccountStore::listed`):
///
/// * **Unlisted** — wait for the fact, bounded by [`FIRST_PASS_WAIT`], and
///   return the moment the bound plane records it (the delegable listing
///   runs early in the prologue, so a preference read does not wait out a
///   prologue that takes minutes). If the bound passes, or no pass is in
///   flight any more, with the scope still unlisted, the read is refused
///   ([`ScopeNotReady`]) and never answers from the store as it stands.
/// * **Listed** — this process's first read waits, with the same bound, for
///   this launch's first listing or the end of the pass in flight, and then
///   reads the store as it stands: stale and real, never empty out of
///   ignorance. Later reads do not wait.
///
/// A runtime that pumps no pass waits for nothing —
/// [`AccountStoreHandle::settled`] answers it at once — and reads the fact its
/// engine holder recorded. The fact is read through the store on every
/// crossing that this process's own listing does not settle, so nothing here
/// depends on the engine role, which reads `false` until the election settles.
pub async fn first_listing_gate(
    handle: &AccountStoreHandle,
    scope: &str,
) -> Result<(), ScopeNotReady> {
    let listings = handle.first_listings();
    if listings.listed(scope) {
        return Ok(());
    }
    if listings.launch_waited(scope) && stored_listed(handle, scope).await {
        return Ok(());
    }
    // The cross-target sleep, never tokio's timer (wasm32).
    tokio::select! {
        biased;
        () = listings.wait_listed(scope) => return Ok(()),
        () = handle.settled() => {}
        () = fauna_sleep::sleep(FIRST_PASS_WAIT) => {
            tracing::warn!("no first listing of {scope} within {FIRST_PASS_WAIT:?}");
        }
    }
    // Read after the wait: the fact may have been recorded during it, by this
    // process's pass as it ended or by the engine holder beside it.
    if listings.listed(scope) || stored_listed(handle, scope).await {
        listings.note_launch_waited(scope);
        return Ok(());
    }
    tracing::info!("read gate: {scope} refused as not ready — never listed from the bound nest");
    Err(ScopeNotReady {
        scope: scope.to_string(),
        reason: NotReadyReason::NotListed,
    })
}

/// **The gate a read of one kind crosses** — the first-listing gate on the
/// kind's home scope ([`first_listing_gate`]) and, for a kind sealed under
/// the generation tip, **the unkeyed hold** (`account-client-lifecycle.md`
/// § The client-side lifecycle → *The first listing*, clause (5)).
///
/// `listed` says the replica has seen every row the bound nest holds, not
/// that it could open them. While the store records a generation a listing
/// left rows unopened under, and a source of that generation's key still
/// stands for this device (`crate::unkeyed_hold`), a fold of the rows the
/// replica could open is no answer to "what does the account hold?" — the
/// unopened row may be the account's value of this very kind. So the read
/// waits, bounded by [`FIRST_PASS_WAIT`] and returning the moment no
/// generation holds (the pass in flight may key the generation and re-read
/// the scope), and is refused ([`ScopeNotReady`], with the hold's reason)
/// when the bound passes or the pass in flight ends with a hold standing.
/// It never answers the store as it stands while a generation holds, at a
/// launch's first read either. A `Gen0` kind crosses the first-listing gate
/// alone.
pub async fn read_gate(handle: &AccountStoreHandle, kind: &str) -> Result<(), ScopeNotReady> {
    use fauna_core::crypto::SealingEpoch;
    use fauna_protocol::merge_policy::{home_scope_for_kind, sealing_epoch};
    let scope = home_scope_for_kind(kind).unwrap_or_else(|| {
        // Every gated read names a registered kind; one that is not is a
        // caller's defect, and the fleet scope is the stricter guess.
        tracing::warn!("read gate: {kind} has no home scope");
        fauna_protocol::account_state::ACCOUNT_STATE_FLEET_SCOPE.into()
    });
    first_listing_gate(handle, &scope).await?;
    if sealing_epoch(kind) != Some(SealingEpoch::GenerationTip) {
        return Ok(());
    }
    unkeyed_hold_gate(handle, &scope).await
}

/// The unkeyed hold's half of [`read_gate`].
async fn unkeyed_hold_gate(handle: &AccountStoreHandle, scope: &str) -> Result<(), ScopeNotReady> {
    let listings = handle.first_listings();
    if !holding(handle).await.holds() {
        return Ok(());
    }
    // Wait for the hold to clear — each change the planes or the escrow
    // recovery tell is a reason to ask again — bounded by the pass in
    // flight and by the gate's own bound.
    let cleared = async {
        loop {
            // Registered before the question, so a change between the two is
            // not lost.
            let changed = listings.unkeyed_changed();
            if !holding(handle).await.holds() {
                return;
            }
            changed.await;
        }
    };
    // The cross-target sleep, never tokio's timer (wasm32).
    tokio::select! {
        biased;
        () = cleared => return Ok(()),
        () = handle.settled() => {}
        () = fauna_sleep::sleep(FIRST_PASS_WAIT) => {
            tracing::warn!("a generation still held reads of {scope} after {FIRST_PASS_WAIT:?}");
        }
    }
    let sources = holding(handle).await;
    if !sources.holds() {
        return Ok(());
    }
    tracing::info!(
        device = sources.device,
        holder = sources.holder,
        sibling = sources.sibling,
        "read gate: {scope} refused as not ready — the unkeyed hold stands"
    );
    Err(ScopeNotReady {
        scope: scope.to_string(),
        reason: if sources.needs_nest() {
            NotReadyReason::HeldForNest
        } else {
            NotReadyReason::HeldForSibling
        },
    })
}

/// The hold's sources, read on the store thread. A read that fails holds,
/// for want of the nest: the gate fails closed, and the read behind it would
/// fail the same way.
async fn holding(handle: &AccountStoreHandle) -> crate::unkeyed_hold::HoldSources {
    match handle.unkeyed_hold().await {
        Ok(sources) => sources,
        Err(e) => {
            tracing::warn!("the unkeyed hold could not be read: {e:#}");
            crate::unkeyed_hold::HoldSources {
                device: true,
                ..Default::default()
            }
        }
    }
}

/// The store's listed fact for `scope`. A read that fails answers unlisted:
/// the gate fails closed, and the read behind it would fail the same way.
async fn stored_listed(handle: &AccountStoreHandle, scope: &str) -> bool {
    match handle.scope_listed(scope).await {
        Ok(listed) => listed,
        Err(e) => {
            tracing::warn!("the listed fact of {scope} could not be read: {e:#}");
            false
        }
    }
}

/// The account's mail custody (`fauna.state.mail`) over the seat's account
/// runtime — the production [`MailStore`] every consumer built before the
/// runtime is up is handed (the mail-settings machine at sign-in, the DAV
/// context, grant minting). Each call reads the handle fresh from `source`,
/// waiting [`ACCOUNT_HANDLE_WAIT`] for one, then delegates to the handle's
/// own [`MailStore`] impl; no runtime → the load or save fails, and the
/// caller degrades as it does on any store failure.
#[derive(Clone)]
pub struct AccountMailStore {
    source: AccountHandleSource,
}

impl AccountMailStore {
    pub fn new(source: AccountHandleSource) -> Self {
        Self { source }
    }

    async fn handle(&self, load: bool) -> Result<AccountStoreHandle, StoreError> {
        let handle = wait_for_account_handle(&self.source, "the mail custody's store")
            .await
            .ok_or_else(|| {
                if load {
                    StoreError::Load(RUNTIME_ABSENT.into())
                } else {
                    StoreError::Save(RUNTIME_ABSENT.into())
                }
            })?;
        // (The first-listing gate a read crosses is the handle's own, on its
        // `MailStore` impl.)
        Ok(handle)
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl MailStore for AccountMailStore {
    async fn load(&self) -> Result<MailConfig, StoreError> {
        MailStore::load(&self.handle(true).await?).await
    }

    async fn load_rows(&self) -> Result<MailRows, StoreError> {
        self.handle(true).await?.load_rows().await
    }

    async fn write_state(&self, state: MailStateRow) -> Result<bool, StoreError> {
        self.handle(false).await?.write_state(state).await
    }

    async fn put_credential(&self, credential: MailCredential) -> Result<bool, StoreError> {
        self.handle(false).await?.put_credential(credential).await
    }

    async fn mark_wrapped(
        &self,
        credential_id: String,
        fingerprint: MsekFingerprint,
    ) -> Result<bool, StoreError> {
        self.handle(false)
            .await?
            .mark_wrapped(credential_id, fingerprint)
            .await
    }

    async fn revoke(&self, credential_id: String) -> Result<bool, StoreError> {
        self.handle(false).await?.revoke(credential_id).await
    }

    async fn retire_generation(
        &self,
        generation: fauna_core::data::PriorMsekRetirement,
    ) -> Result<bool, StoreError> {
        self.handle(false)
            .await?
            .retire_generation(generation)
            .await
    }
}
