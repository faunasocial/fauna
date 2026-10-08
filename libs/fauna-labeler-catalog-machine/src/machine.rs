//! The page-level Labeler-Catalog state machine.
//!
//! Mirrors `fauna_devices_machine::DevicesMachine`: each app holds an
//! `Arc<LabelerCatalogMachine>`, observes via a registered
//! `LabelerCatalogObserver`, drives gestures, and renders the whole page (plus
//! the personalization home's subscribed-labelers facet, which filters
//! `snapshot().entries` to `subscribed == true`) off `snapshot()`.
//!
//! Uses `std::sync::Mutex` (not tokio's) so getters and sync gestures work from
//! any thread context — including UI threads and `#[tokio::test]`. The async
//! gestures snapshot/clone under the lock, drop it, do IO, then re-acquire; the
//! lock is never held across an `await`.

use std::sync::{Arc, Mutex};

use fauna_client_capabilities::grant_log::{self, GrantEventSigner};
use fauna_client_capabilities::{
    DEFAULT_GRANT_WINDOW_SECS, DerivePayloadError, MintGrantError, mint_bounded_mail_labeler_grant,
};
use fauna_core::data::Timestamp;
use fauna_core::identity::ActorId;
use fauna_core::localized::LocalizedText;
use fauna_core::scoring::artifact_kind;
use fauna_mls::wrapped_blob::{GrantWindow, ScopeTuple};

use crate::nest_api::{LabelerCatalogApiError, LabelerCatalogNestApi};
use crate::observer::LabelerCatalogObserver;
use crate::snapshots::{LabelerCatalogEntry, LabelerCatalogSnapshot, LabelerInspectView};

/// i18n key for a page read (`refresh()`) failure ("Failed to load community
/// labelers: {message}").
const REFRESH_ERROR_KEY: &str = "labeler_catalog.error_refresh";
/// i18n key for an `inspect` failure.
const INSPECT_ERROR_KEY: &str = "labeler_catalog.error_inspect";
/// i18n key for a `subscribe` failure.
const SUBSCRIBE_ERROR_KEY: &str = "labeler_catalog.error_subscribe";
/// i18n key for an `unsubscribe` failure.
const UNSUBSCRIBE_ERROR_KEY: &str = "labeler_catalog.error_unsubscribe";
/// i18n key for the honest degrade after subscribing a `wasm` mail labeler
/// with no `mda` holder enrolled on the nest to seal its grant to: the row is
/// registered, and the labeler will not run until a holder exists.
pub const SUBSCRIBED_WITHOUT_HOLDER_KEY: &str = "labeler_catalog.subscribed_without_mail_holder";
/// i18n key for the honest degrade after subscribing a `wasm` mail labeler
/// while mail is not enabled for this account (no MSEK to derive a grant
/// payload from): registered, will not run until mail is set up.
pub const SUBSCRIBED_WITHOUT_MAIL_KEY: &str = "labeler_catalog.subscribed_without_mail";

/// The seams the per-labeler grant mint needs beyond the nest API: the owner's
/// identity (the grant's `owner_actor_id`), the account's mail custody (the MSEK generations the
/// grant's epoch wraps derive from), and the signer that keeps the identity key behind the
/// FFI/wasm boundary. The `fauna-client-pair` `TrustSeams` shape. A machine
/// built without them (`LabelerCatalogMachine::new`) subscribes a sealed-kind
/// labeler with no grant — the shape used by the identity-fault fallback and tests.
pub struct LabelerGrantSeams {
    /// The owner's identity pubkey (`mint_grant`'s `owner_actor_id`).
    pub actor_id: [u8; 32],
    /// The grant-event log (`fauna.state.succession-ledger`) the per-labeler
    /// grant's signed events are recorded on — the host's account store.
    pub ledger: Arc<dyn fauna_client_config::SuccessionLedgerStore>,
    pub mail: Arc<dyn fauna_client_config::MailStore>,
    pub signer: Arc<dyn GrantEventSigner>,
}

/// Why a subscribe's mint did not produce a grant to link — or that it did.
enum MintOutcome {
    /// A public-content labeler, or a non-`wasm` artifact: no grant is ever
    /// needed (`content-moderation-and-ranking.md` § Tier-3 → *Public vs.
    /// restricted content*; a `list` / `text-model` never has a holder).
    NotNeeded,
    /// The machine was built without [`LabelerGrantSeams`] — this app has not
    /// lifted the mint yet. Registered without a grant, silently: the honest
    /// notice below would blame the deployment for an app gap.
    Unwired,
    /// No `mda`-role holder is enrolled on the nest to seal the grant to.
    NoHolder,
    /// Mail is not enabled for this account (no MSEK) — nothing to license.
    MailNotEnabled,
    /// Minted, recorded, deposited: link the subscription to it.
    Minted { grant_id: [u8; 16] },
}

impl MintOutcome {
    fn grant_id(&self) -> Option<[u8; 16]> {
        match self {
            MintOutcome::Minted { grant_id } => Some(*grant_id),
            _ => None,
        }
    }

    /// The honest-degrade notice this outcome owes the user, if any.
    fn notice_key(&self) -> Option<&'static str> {
        match self {
            MintOutcome::NoHolder => Some(SUBSCRIBED_WITHOUT_HOLDER_KEY),
            MintOutcome::MailNotEnabled => Some(SUBSCRIBED_WITHOUT_MAIL_KEY),
            MintOutcome::NotNeeded | MintOutcome::Unwired | MintOutcome::Minted { .. } => None,
        }
    }
}

/// A per-labeler grant mint or revoke failed — every arm is a real failure
/// the gesture surfaces via `error-message`, never a degrade (those are
/// [`MintOutcome`]s). `Display` is what the page renders.
#[derive(Debug, thiserror::Error)]
enum LabelerGrantError {
    #[error(transparent)]
    Nest(#[from] LabelerCatalogApiError),
    /// Loading or persisting the signed grant log failed.
    #[error(transparent)]
    Store(#[from] fauna_client_config::StoreError),
    #[error(transparent)]
    Sign(#[from] grant_log::GrantEventSignError),
    /// Client-side crypto composing/serializing the `GrantBlob` failed.
    #[error("grant crypto: {0}")]
    Wrap(String),
    /// The stored log did not record the mint, so the blob was not deposited.
    #[error(transparent)]
    Unrecorded(#[from] grant_log::UnrecordedGrantError),
    #[error("invalid state: {0}")]
    InvalidState(String),
}

impl From<MintGrantError> for LabelerGrantError {
    fn from(e: MintGrantError) -> Self {
        LabelerGrantError::Wrap(e.to_string())
    }
}

impl From<fauna_mls::wrapped_blob::WrapError> for LabelerGrantError {
    fn from(e: fauna_mls::wrapped_blob::WrapError) -> Self {
        LabelerGrantError::Wrap(e.to_string())
    }
}

/// Whether subscribing `entry` is the act of minting a per-labeler grant: an
/// executable (`wasm`) module over a **sealed** content kind — `mail` today
/// (`content-moderation-and-ranking.md` § Tier-3 → *Subscribing = minting a
/// capability*). A `list` or `text-model` artifact never runs on a holder, and
/// a public kind needs no key.
fn needs_per_labeler_grant(entry: &LabelerCatalogEntry) -> bool {
    entry.artifact_kind == artifact_kind::WASM && entry.content_kind == ScopeTuple::KIND_MAIL
}

/// A fresh 16-byte grant id — random, never a counter (the nest keys on
/// `(owner, grant_id)`, so a counter would collide across re-subscribes).
fn new_grant_id() -> [u8; 16] {
    use rand::RngCore;
    let mut id = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut id);
    id
}

/// The grant window + event-log clock unit: seconds since the Unix epoch, the
/// (impossible) pre-1970 case clamped to 0.
fn now_epoch_secs() -> u64 {
    Timestamp::now_secs().max(0) as u64
}

/// Internal page state. In-memory only; clients read snapshots via the getter.
struct State {
    entries: Vec<LabelerCatalogEntry>,
    inspecting: Option<LabelerInspectView>,
    error: Option<LocalizedText>,
    /// Whether a refresh has ever returned successfully — surfaced as
    /// [`LabelerCatalogSnapshot::loaded`], which owns the full rationale. Set in
    /// the `Ok` arm of [`LabelerCatalogMachine::refresh`] and never cleared.
    loaded: bool,
}

impl State {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
            inspecting: None,
            error: None,
            loaded: false,
        }
    }
}

#[cfg_attr(feature = "uniffi", derive(uniffi::Object))]
pub struct LabelerCatalogMachine {
    state: Mutex<State>,
    observer: Arc<dyn LabelerCatalogObserver>,
    nest_api: Arc<dyn LabelerCatalogNestApi>,
    /// `Some` ⇒ subscribing a `wasm` mail labeler mints its per-labeler grant
    /// and unsubscribing revokes it; `None` ⇒ the grant-less shape (identity-fault fallback, tests).
    grants: Option<LabelerGrantSeams>,
}

impl LabelerCatalogMachine {
    /// Construct the page machine over an injected [`LabelerCatalogNestApi`]
    /// **without** grant seams — a sealed-kind subscription registers with no
    /// grant and never drains. State starts empty; the client calls
    /// `refresh()` to populate it.
    ///
    /// Not a `#[uniffi::constructor]` — the seam (`Arc<dyn …>`) has no FFI ABI.
    /// All 7 apps construct [`Self::with_grant_seams`] via
    /// `nest_api::build_labeler_catalog_machine_with_grants` (through
    /// `fauna-ffi` / wasm web / tui / linux); this grant-less constructor is
    /// reached only by tui's and linux's identity-fault fallback
    /// (`nest_api::build_labeler_catalog_machine`, a `secret_hex` that will
    /// not decode) and by tests passing a `FakeLabelerCatalogNestApi`.
    pub fn new(
        observer: Arc<dyn LabelerCatalogObserver>,
        nest_api: Arc<dyn LabelerCatalogNestApi>,
    ) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State::new()),
            observer,
            nest_api,
            grants: None,
        })
    }

    /// [`Self::new`] plus the [`LabelerGrantSeams`] that make a `wasm` mail
    /// labeler's subscribe mint its grant (and unsubscribe revoke it).
    pub fn with_grant_seams(
        observer: Arc<dyn LabelerCatalogObserver>,
        nest_api: Arc<dyn LabelerCatalogNestApi>,
        grants: LabelerGrantSeams,
    ) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State::new()),
            observer,
            nest_api,
            grants: Some(grants),
        })
    }
}

#[cfg_attr(feature = "uniffi", fauna_uniffi_async::export)]
impl LabelerCatalogMachine {
    // ── Read surface ────────────────────────────────────────────────────

    /// The whole renderable labeler-catalog page in one record. The
    /// personalization home's subscribed-labelers facet is the same
    /// `entries`, client-filtered to `subscribed == true` — no second fetch.
    pub fn snapshot(&self) -> LabelerCatalogSnapshot {
        let s = self.state.lock().unwrap();
        LabelerCatalogSnapshot {
            entries: s.entries.clone(),
            inspecting: s.inspecting.clone(),
            error: s.error.clone(),
            loaded: s.loaded,
        }
    }

    // ── Gestures ────────────────────────────────────────────────────────

    /// Re-read the full catalog (`fauna.labelers.list`). On a read failure the
    /// prior data is kept and the page error is set; on success the error
    /// clears. Notifies once.
    pub async fn refresh(&self) {
        match self.nest_api.list().await {
            Ok(entries) => {
                let mut s = self.state.lock().unwrap();
                s.entries = entries;
                s.error = None;
                // The one place the page becomes loaded. Only a read that
                // RETURNED may license an empty state; the `Err` arm below
                // deliberately leaves this alone (see the field's doc on
                // `LabelerCatalogSnapshot`).
                s.loaded = true;
            }
            Err(e) => self.set_error(REFRESH_ERROR_KEY, e.detail()),
        }
        self.observer.on_changed();
    }

    /// Open the inspect-before-subscribe panel for the catalog row at `index`
    /// (`fauna.labelers.inspect` — fetches + decodes + re-verifies the full
    /// signed metadata + WASM bytes). Out-of-range indices are ignored.
    pub async fn inspect(&self, index: u32) {
        let labeler_id = {
            let s = self.state.lock().unwrap();
            s.entries
                .get(index as usize)
                .map(|e| hex_decode(&e.labeler_id))
        };
        let Some(labeler_id) = labeler_id else {
            return;
        };
        match self.nest_api.inspect(labeler_id).await {
            Ok(result) => {
                self.state.lock().unwrap().inspecting = Some(result.view);
                self.state.lock().unwrap().error = None;
            }
            Err(e) => self.set_error(INSPECT_ERROR_KEY, e.detail()),
        }
        self.observer.on_changed();
    }

    /// Close the inspect panel (`labeler-inspect-close-button`).
    pub fn close_inspect(&self) {
        self.state.lock().unwrap().inspecting = None;
        self.observer.on_changed();
    }

    /// Subscribe to the catalog row at `index`, then refresh.
    ///
    /// **Subscribing a `wasm` labeler over sealed content IS minting it a
    /// capability** (`content-moderation-and-ranking.md` § Tier-3 →
    /// *Subscribing = minting a capability*): the per-labeler bounded mail
    /// grant — every tuple and wrap confined to `labeler:<hex>`, so the MDA
    /// holder opens the owner's mail only to compute that labeler's score —
    /// is minted to the nest's `mda` holder, **recorded in the owner's signed
    /// grant log first, then deposited** (`grant_log::UndepositedGrant`;
    /// `nests.md` § Record-then-deposit), and `fauna.labelers.subscribe` links
    /// the subscription row to it by `grant_id`. The standard window
    /// (`DEFAULT_GRANT_WINDOW_SECS`) — a subscription is not a one-off.
    ///
    /// **The honest degrades.** With no `mda` holder enrolled, or mail not
    /// enabled for this account, there is nothing to seal to / license: the
    /// row is still registered (the nest seeds its backlog) with no grant,
    /// and the page says so on `error-message` — the one text every app
    /// renders — because an un-granted subscription never drains
    /// (§ Tier-3's "obligation stays owed"), exactly like a revoked one, and
    /// silence would read as a working labeler. A public-content labeler, a
    /// `list` or a `text-model` needs no grant. A machine built without
    /// [`LabelerGrantSeams`] registers with no grant and no notice (an app
    /// gap, not a deployment state).
    ///
    /// A mint that fails outright (a refused deposit, an unsaved log) fails
    /// the subscribe; a subscribe that fails **after** the mint revokes the
    /// grant again best-effort, so a grant never outlives the subscription
    /// it exists for (and if even that fails, the log still names it — it is
    /// visible and revocable on the Nests page).
    pub async fn subscribe(&self, index: u32) {
        let entry = self
            .state
            .lock()
            .unwrap()
            .entries
            .get(index as usize)
            .cloned();
        let Some(entry) = entry else {
            return;
        };
        let labeler_id = hex_decode(&entry.labeler_id);
        let minted = if needs_per_labeler_grant(&entry) {
            match self.mint_labeler_grant(&labeler_id).await {
                Ok(outcome) => outcome,
                Err(e) => {
                    self.set_error(SUBSCRIBE_ERROR_KEY, &e.to_string());
                    self.observer.on_changed();
                    return;
                }
            }
        } else {
            MintOutcome::NotNeeded
        };
        match self
            .nest_api
            .subscribe(labeler_id.clone(), minted.grant_id())
            .await
        {
            Ok(()) => {
                self.refresh().await;
                if let Some(key) = minted.notice_key() {
                    self.set_notice(key);
                    self.observer.on_changed();
                }
            }
            Err(e) => {
                if minted.grant_id().is_some()
                    && let Err(revoke_err) = self.revoke_labeler_grant(&labeler_id).await
                {
                    tracing::warn!(
                        target: "fauna_labeler_catalog",
                        error = %revoke_err,
                        "subscribe failed after its grant was minted; the grant stays revocable on the Nests page"
                    );
                }
                self.set_error(SUBSCRIBE_ERROR_KEY, e.detail());
                self.observer.on_changed();
            }
        }
    }

    /// Unsubscribe from the catalog row at `index`, then refresh.
    ///
    /// **Unsubscribe = revoke** the subscription's per-labeler grant, found
    /// in the owner's grant log by the labeler factor its tuples carry
    /// (`grant_log::current_labeler_grant`): `fauna.capabilities.revoke`
    /// first — revoke narrows, so the nest leads (`nests.md`
    /// § Record-then-deposit) — then the signed `Revoke` event, then
    /// `fauna.labelers.unsubscribe`. A refused revoke fails the gesture
    /// before the row is dropped: dropping the row while the grant lives
    /// would leave a capability the page no longer explains. A subscription
    /// that never had a grant just drops its row.
    pub async fn unsubscribe(&self, index: u32) {
        let labeler_id = {
            let s = self.state.lock().unwrap();
            s.entries
                .get(index as usize)
                .map(|e| hex_decode(&e.labeler_id))
        };
        let Some(labeler_id) = labeler_id else {
            return;
        };
        if let Err(e) = self.revoke_labeler_grant(&labeler_id).await {
            self.set_error(UNSUBSCRIBE_ERROR_KEY, &e.to_string());
            self.observer.on_changed();
            return;
        }
        match self.nest_api.unsubscribe(labeler_id).await {
            Ok(()) => self.refresh().await,
            Err(e) => {
                self.set_error(UNSUBSCRIBE_ERROR_KEY, e.detail());
                self.observer.on_changed();
            }
        }
    }
}

// ── Internal helpers (not FFI-exported) ──────────────────────────────────
impl LabelerCatalogMachine {
    /// Set the page error and log it once at the producer (observability.md §
    /// Log on the *event*, not the *paint* — the per-app views paint
    /// `snapshot().error` reactively on every observer tick).
    fn set_error(&self, key: &str, detail: &str) {
        let err = LocalizedText::key_arg(key, "message", detail.to_string());
        tracing::warn!(target: "fauna_labeler_catalog", "{}", err.log_line());
        self.state.lock().unwrap().error = Some(err);
    }

    /// Set an honest-degrade notice on the page's one text slot (a subscribe
    /// that registered without its grant). Logged at the producer like an
    /// error; cleared by the next successful refresh like one.
    fn set_notice(&self, key: &str) {
        let notice = LocalizedText::key(key);
        tracing::info!(target: "fauna_labeler_catalog", "{}", notice.log_line());
        self.state.lock().unwrap().error = Some(notice);
    }

    /// Mint, record and deposit the per-labeler grant for `labeler_id` (see
    /// [`Self::subscribe`]). The degrades come back as [`MintOutcome`]s; only
    /// a real failure is an `Err`.
    async fn mint_labeler_grant(
        &self,
        labeler_id: &[u8],
    ) -> Result<MintOutcome, LabelerGrantError> {
        let Some(seams) = &self.grants else {
            return Ok(MintOutcome::Unwired);
        };
        let labeler = ActorId(<[u8; 32]>::try_from(labeler_id).map_err(|_| {
            LabelerGrantError::InvalidState("labeler id is not 32 bytes".to_string())
        })?);
        let holders = self.nest_api.content_processor_holders().await?;
        let Some(holder) = holders
            .iter()
            .find(|h| h.role == fauna_client_bridges::MDA_HOLDER_ROLE)
        else {
            return Ok(MintOutcome::NoHolder);
        };
        let mail = seams.mail.load().await?;
        let now = now_epoch_secs();
        let window_end = now + DEFAULT_GRANT_WINDOW_SECS;
        let grant_id = new_grant_id();
        let blob = match mint_bounded_mail_labeler_grant(
            &mail,
            &seams.actor_id,
            &grant_id,
            &holder.pubkey,
            holder.mlkem_ek.as_deref(),
            GrantWindow(now, window_end),
            &labeler,
        ) {
            Ok(blob) => blob,
            Err(MintGrantError::DerivePayload(DerivePayloadError::MailNotEnabled)) => {
                return Ok(MintOutcome::MailNotEnabled);
            }
            Err(e) => return Err(e.into()),
        };
        let pending = grant_log::UndepositedGrant::new(grant_id, blob.to_canonical_bytes()?);
        let unsigned = grant_log::build_mint_event(
            grant_id,
            holder.pubkey,
            grant_log::bounded_mail_labeler_event_scope(&labeler),
            now,
            window_end,
            now,
        );
        let signed = seams.signer.sign_grant_event(unsigned)?;
        // Record, publish, then deposit: the blob leaves only against the log
        // the ledger write actually stored and the bound nest acknowledged
        // (`UndepositedGrant::release`).
        let published = seams
            .ledger
            .merge_published(
                fauna_core::succession_ledger::SuccessionLedger::events_replica(
                    ActorId(seams.actor_id),
                    vec![signed],
                ),
            )
            .await?;
        let blob_bytes =
            pending.release(&grant_log::PublishedGrants::from_published(&published))?;
        self.nest_api.mint_grant(blob_bytes).await?;
        Ok(MintOutcome::Minted { grant_id })
    }

    /// Revoke the live per-labeler grant for `labeler_id`, if the log holds
    /// one: the nest first, then the signed `Revoke` event (see
    /// [`Self::unsubscribe`]). `Ok(())` when there is nothing to revoke — no
    /// seams, no grant, or a labeler id the log could never name.
    async fn revoke_labeler_grant(&self, labeler_id: &[u8]) -> Result<(), LabelerGrantError> {
        let Some(seams) = &self.grants else {
            return Ok(());
        };
        let Ok(labeler) = <[u8; 32]>::try_from(labeler_id).map(ActorId) else {
            return Ok(());
        };
        let ledger = seams.ledger.load().await?;
        let Some(grant) = grant_log::current_labeler_grant(&ledger, &labeler) else {
            return Ok(());
        };
        let grant_id = <[u8; 16]>::try_from(grant.grant_id.as_slice()).map_err(|_| {
            LabelerGrantError::InvalidState("the log's grant id is not 16 bytes".to_string())
        })?;
        let holder = <[u8; 32]>::try_from(grant.holder.as_slice()).map_err(|_| {
            LabelerGrantError::InvalidState("the log's holder is not 32 bytes".to_string())
        })?;
        self.nest_api.revoke_grant(grant_id).await?;
        let unsigned = grant_log::build_revoke_event(grant_id, holder, now_epoch_secs());
        let signed = seams.signer.sign_grant_event(unsigned)?;
        seams
            .ledger
            .merge(
                fauna_core::succession_ledger::SuccessionLedger::events_replica(
                    ActorId(seams.actor_id),
                    vec![signed],
                ),
            )
            .await?;
        Ok(())
    }
}

/// Decode a `LabelerCatalogEntry.labeler_id` hex string back to raw bytes for
/// an RPC call. Malformed hex (should never happen — the snapshot always
/// hex-encodes from raw bytes) degrades to empty, which the nest rejects as
/// `not_found` rather than panicking the UI thread.
fn hex_decode(hex_str: &str) -> Vec<u8> {
    hex::decode(hex_str).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nest_api::{FakeLabelerCatalogNestApi, InspectResult, LabelerCatalogApiError};
    use crate::observer::CountingObserver;
    use fauna_client_bridges::HolderInfo;
    use fauna_client_config::test_helpers::{FakeMailStore, FakeSuccessionLedgerStore};
    use fauna_core::grant_event::GrantEventKind;
    use fauna_core::identity::ActorKeypair;
    use fauna_mls::wrapped_blob::GrantBlob;

    fn entry(labeler_id_byte: u8, subscribed: bool) -> LabelerCatalogEntry {
        LabelerCatalogEntry {
            labeler_id: format!("{labeler_id_byte:02x}").repeat(32),
            version: 1,
            publisher_actor: "bb".repeat(32),
            artifact_kind: "wasm".into(),
            content_kind: "post".into(),
            factor: format!("labeler:{labeler_id_byte:02x}"),
            wasm_hash: "cc".repeat(36),
            wasm_size: 4096,
            subscribed,
            ..Default::default()
        }
    }

    /// A `wasm` labeler over sealed mail — the one kind whose subscription
    /// mints a grant.
    fn mail_entry(labeler_id_byte: u8, subscribed: bool) -> LabelerCatalogEntry {
        LabelerCatalogEntry {
            content_kind: "mail".into(),
            ..entry(labeler_id_byte, subscribed)
        }
    }

    fn labeler_of(labeler_id_byte: u8) -> ActorId {
        ActorId([labeler_id_byte; 32])
    }

    fn machine() -> (
        Arc<LabelerCatalogMachine>,
        Arc<FakeLabelerCatalogNestApi>,
        Arc<CountingObserver>,
    ) {
        let api = Arc::new(FakeLabelerCatalogNestApi::new());
        let observer = CountingObserver::new();
        let m = LabelerCatalogMachine::new(observer.clone(), api.clone());
        (m, api, observer)
    }

    /// The owner's signer over a real keypair — what the production glue does.
    struct KeypairSigner(ActorKeypair);

    impl GrantEventSigner for KeypairSigner {
        fn sign_grant_event(
            &self,
            event: fauna_core::grant_event::GrantEvent,
        ) -> Result<fauna_core::grant_event::GrantEvent, grant_log::GrantEventSignError> {
            event
                .sign(self.0.signing_key())
                .map_err(|e| grant_log::GrantEventSignError::Sign(e.to_string()))
        }
    }

    const HOLDER_PUBKEY: [u8; 32] = [7u8; 32];

    fn mda_holder() -> HolderInfo {
        HolderInfo {
            pubkey: HOLDER_PUBKEY,
            mlkem_ek: None,
            role: fauna_client_bridges::MDA_HOLDER_ROLE.into(),
            bridge_id: "mda-1".into(),
        }
    }

    /// A grant-wired machine: the owner's mail custody (an MSEK held unless a
    /// test builds it over another), a real signer, and the fake nest. Returns the
    /// store and keypair handles the assertions read through.
    fn granting_machine() -> (
        Arc<LabelerCatalogMachine>,
        Arc<FakeLabelerCatalogNestApi>,
        FakeSuccessionLedgerStore,
        ActorKeypair,
    ) {
        granting_machine_over(FakeMailStore::with(&fauna_core::data::MailConfig {
            msek: Some([0x42u8; 32].into()),
            ..Default::default()
        }))
    }

    /// [`granting_machine`] over a caller-supplied mail custody.
    fn granting_machine_over(
        mail: FakeMailStore,
    ) -> (
        Arc<LabelerCatalogMachine>,
        Arc<FakeLabelerCatalogNestApi>,
        FakeSuccessionLedgerStore,
        ActorKeypair,
    ) {
        let kp = ActorKeypair::generate();
        // The grant-event log the assertions read — the ledger double.
        let store = FakeSuccessionLedgerStore::empty(kp.actor_id());
        let api = Arc::new(FakeLabelerCatalogNestApi::new());
        let seams = LabelerGrantSeams {
            actor_id: kp.actor_id().0,
            ledger: Arc::new(store.clone()),
            mail: Arc::new(mail),
            signer: Arc::new(KeypairSigner(ActorKeypair::from_secret(*kp.secret_bytes()))),
        };
        let m =
            LabelerCatalogMachine::with_grant_seams(CountingObserver::new(), api.clone(), seams);
        (m, api, store, kp)
    }

    /// The i18n key the snapshot's one text slot carries, if any.
    fn snapshot_key(m: &LabelerCatalogMachine) -> Option<String> {
        m.snapshot().error.map(|t| t.key)
    }

    /// Subscribing a `wasm` mail labeler IS minting its per-labeler grant:
    /// the holder is read, the grant is recorded in the owner's log (the
    /// Mint event carries the labeler-folded scope), deposited, and the
    /// subscribe names it — in that order.
    #[tokio::test]
    async fn subscribing_a_wasm_mail_labeler_mints_records_then_deposits_its_grant() {
        let (m, api, store, kp) = granting_machine();
        let labeler = labeler_of(0xAA);
        api.set_entries(vec![mail_entry(0xAA, false)]);
        api.set_holders(vec![mda_holder()]);
        m.refresh().await;
        api.set_entries(vec![mail_entry(0xAA, true)]);

        m.subscribe(0).await;

        let calls = api.calls();
        assert_eq!(calls.len(), 3, "holders, deposit, subscribe: {calls:?}");
        assert_eq!(calls[0], crate::nest_api::FakeCall::ContentProcessorHolders);
        let crate::nest_api::FakeCall::MintGrant { grant_blob } = &calls[1] else {
            panic!("second call must be the deposit, got {:?}", calls[1]);
        };
        let crate::nest_api::FakeCall::Subscribe {
            labeler_id,
            grant_id: Some(grant_id),
        } = &calls[2]
        else {
            panic!(
                "third call must be the subscribe naming the grant, got {:?}",
                calls[2]
            );
        };
        assert_eq!(labeler_id, &labeler.0.to_vec());

        // The deposited blob is the per-labeler shape, under the id the
        // subscribe names.
        let blob = GrantBlob::from_canonical_bytes(grant_blob).expect("decode deposit");
        assert_eq!(blob.index.1.as_slice(), grant_id.as_slice());
        let factor = fauna_core::scoring::labeler_factor(&labeler);
        assert!(
            blob.scope
                .iter()
                .all(|t| t.factor.as_deref() == Some(factor.as_str())),
            "every tuple confined to the labeler"
        );
        assert_eq!(blob.window.1 - blob.window.0, DEFAULT_GRANT_WINDOW_SECS);

        // The owner's log names it: one Mint, the labeler-folded scope,
        // signed by the owner.
        let cfg = store.current();
        assert_eq!(store.merges(), 1);
        assert_eq!(cfg.grant_events.len(), 1);
        let event = &cfg.grant_events[0];
        assert_eq!(event.kind, GrantEventKind::Mint);
        assert_eq!(event.grant_id.as_slice(), grant_id.as_slice());
        assert_eq!(event.holder, HOLDER_PUBKEY.to_vec());
        assert_eq!(
            event.scope,
            grant_log::bounded_mail_labeler_event_scope(&labeler)
        );
        event.verify(&kp.actor_id()).expect("owner-signed");
        assert_eq!(
            grant_log::current_labeler_grant(&cfg, &labeler).map(|g| g.grant_id),
            Some(grant_id.to_vec()),
            "the subscription finds its grant by the labeler"
        );
        assert!(
            m.snapshot().entries[0].subscribed,
            "refreshed after subscribe"
        );
        assert!(m.snapshot().error.is_none(), "a full mint owes no notice");
    }

    /// A public-content labeler needs no grant: no holder read, no log write.
    #[tokio::test]
    async fn a_public_labeler_subscribes_with_no_grant_and_no_holder_read() {
        let (m, api, store, _kp) = granting_machine();
        api.set_entries(vec![entry(0xAA, false)]);
        api.set_holders(vec![mda_holder()]);
        m.refresh().await;

        m.subscribe(0).await;

        assert_eq!(
            api.calls(),
            vec![crate::nest_api::FakeCall::Subscribe {
                labeler_id: vec![0xAA; 32],
                grant_id: None,
            }]
        );
        assert_eq!(store.merges(), 0);
        assert!(m.snapshot().error.is_none());
    }

    /// No `mda` holder to seal to: the row registers with no grant and the
    /// page says the labeler will not run — never a silent dead subscription.
    #[tokio::test]
    async fn no_mda_holder_registers_without_a_grant_and_says_so() {
        let (m, api, store, _kp) = granting_machine();
        api.set_entries(vec![mail_entry(0xAA, false)]);
        m.refresh().await;

        m.subscribe(0).await;

        assert_eq!(
            api.calls(),
            vec![
                crate::nest_api::FakeCall::ContentProcessorHolders,
                crate::nest_api::FakeCall::Subscribe {
                    labeler_id: vec![0xAA; 32],
                    grant_id: None,
                },
            ]
        );
        assert_eq!(store.merges(), 0, "nothing minted, nothing recorded");
        assert_eq!(
            snapshot_key(&m).as_deref(),
            Some(SUBSCRIBED_WITHOUT_HOLDER_KEY)
        );
    }

    /// Mail not enabled (no MSEK): nothing to license — the same honest
    /// degrade, with its own wording.
    #[tokio::test]
    async fn mail_not_enabled_registers_without_a_grant_and_says_so() {
        let (m, api, store, _kp) = granting_machine_over(FakeMailStore::empty());
        api.set_entries(vec![mail_entry(0xAA, false)]);
        api.set_holders(vec![mda_holder()]);
        m.refresh().await;

        m.subscribe(0).await;

        let calls = api.calls();
        assert!(
            matches!(
                calls.last(),
                Some(crate::nest_api::FakeCall::Subscribe { grant_id: None, .. })
            ),
            "{calls:?}"
        );
        assert!(
            !calls
                .iter()
                .any(|c| matches!(c, crate::nest_api::FakeCall::MintGrant { .. }))
        );
        assert_eq!(store.merges(), 0);
        assert_eq!(
            snapshot_key(&m).as_deref(),
            Some(SUBSCRIBED_WITHOUT_MAIL_KEY)
        );
    }

    /// **Record-then-deposit.** A log save that fails deposits NOTHING and
    /// registers nothing: a grant the nest holds but the log does not name
    /// would be on no page and nameable by no revoke.
    #[tokio::test]
    async fn a_failed_log_save_deposits_nothing_and_fails_the_subscribe() {
        let (m, api, store, _kp) = granting_machine();
        api.set_entries(vec![mail_entry(0xAA, false)]);
        api.set_holders(vec![mda_holder()]);
        m.refresh().await;
        store.refuse_next_merges(1);

        m.subscribe(0).await;

        assert_eq!(
            api.calls(),
            vec![crate::nest_api::FakeCall::ContentProcessorHolders],
            "no deposit and no subscribe past a failed record"
        );
        assert_eq!(snapshot_key(&m).as_deref(), Some(SUBSCRIBE_ERROR_KEY));
        assert!(
            !m.snapshot().entries[0].subscribed,
            "no refresh happened on error"
        );
    }

    /// **Record, publish, then deposit.** A `Mint` the bound nest did not
    /// acknowledge deposits NOTHING and registers nothing: a sibling replica
    /// could not yet read the event its reconcile sweep judges the row by.
    #[tokio::test]
    async fn an_unpublished_mint_deposits_nothing_and_fails_the_subscribe() {
        let (m, api, store, _kp) = granting_machine();
        api.set_entries(vec![mail_entry(0xAA, false)]);
        api.set_holders(vec![mda_holder()]);
        m.refresh().await;
        store.publish_refuses(true);

        m.subscribe(0).await;

        assert_eq!(
            api.calls(),
            vec![crate::nest_api::FakeCall::ContentProcessorHolders],
            "no deposit and no subscribe past an unpublished record"
        );
        assert_eq!(snapshot_key(&m).as_deref(), Some(SUBSCRIBE_ERROR_KEY));
        assert_eq!(
            store.current().grant_events.len(),
            1,
            "the Mint stays recorded locally"
        );
    }

    /// A refused deposit fails the subscribe; the log's Mint stays as the
    /// (revocable, visible) record of the attempt.
    #[tokio::test]
    async fn a_refused_deposit_fails_the_subscribe() {
        let (m, api, _store, _kp) = granting_machine();
        api.set_entries(vec![mail_entry(0xAA, false)]);
        api.set_holders(vec![mda_holder()]);
        api.set_mint_grant_response(Err(LabelerCatalogApiError::Rejected {
            detail: "over cap".into(),
        }));
        m.refresh().await;

        m.subscribe(0).await;

        let calls = api.calls();
        assert_eq!(calls.len(), 2, "holders + the refused deposit: {calls:?}");
        assert!(matches!(
            calls[1],
            crate::nest_api::FakeCall::MintGrant { .. }
        ));
        assert_eq!(snapshot_key(&m).as_deref(), Some(SUBSCRIBE_ERROR_KEY));
    }

    /// A subscribe refused AFTER the mint revokes the grant again, so no
    /// capability outlives the subscription it was minted for.
    #[tokio::test]
    async fn a_subscribe_that_fails_after_the_mint_revokes_the_grant() {
        let (m, api, store, _kp) = granting_machine();
        let labeler = labeler_of(0xAA);
        api.set_entries(vec![mail_entry(0xAA, false)]);
        api.set_holders(vec![mda_holder()]);
        api.set_subscribe_response(Err(LabelerCatalogApiError::Transient {
            detail: "nope".into(),
        }));
        m.refresh().await;

        m.subscribe(0).await;

        let calls = api.calls();
        assert_eq!(calls.len(), 4, "{calls:?}");
        let crate::nest_api::FakeCall::Subscribe {
            grant_id: Some(grant_id),
            ..
        } = &calls[2]
        else {
            panic!("{:?}", calls[2]);
        };
        assert_eq!(
            calls[3],
            crate::nest_api::FakeCall::RevokeGrant {
                grant_id: *grant_id
            },
            "the just-minted grant is revoked on the nest"
        );
        let cfg = store.current();
        // The ledger keeps canonical (byte) order, not arrival order.
        let mut kinds: Vec<_> = cfg.grant_events.iter().map(|e| e.kind as u8).collect();
        kinds.sort_unstable();
        assert_eq!(
            kinds,
            vec![GrantEventKind::Mint as u8, GrantEventKind::Revoke as u8],
            "a Mint and its Revoke"
        );
        assert!(grant_log::current_labeler_grant(&cfg, &labeler).is_none());
        assert_eq!(snapshot_key(&m).as_deref(), Some(SUBSCRIBE_ERROR_KEY));
    }

    /// Unsubscribe = revoke the linked grant: the nest first (revoke
    /// narrows), then the signed Revoke event, then the row is dropped.
    #[tokio::test]
    async fn unsubscribing_revokes_the_linked_grant_nest_first_then_records() {
        let (m, api, store, kp) = granting_machine();
        let labeler = labeler_of(0xAA);
        let grant_id = [9u8; 16];
        store.mutate(|cfg| {
            grant_log::record_mint(
                cfg,
                kp.signing_key(),
                grant_id,
                HOLDER_PUBKEY,
                grant_log::bounded_mail_labeler_event_scope(&labeler),
                1000,
                2000,
                1000,
            )
            .unwrap();
        });
        api.set_entries(vec![mail_entry(0xAA, true)]);
        m.refresh().await;
        api.set_entries(vec![mail_entry(0xAA, false)]);

        m.unsubscribe(0).await;

        assert_eq!(
            api.calls(),
            vec![
                crate::nest_api::FakeCall::RevokeGrant { grant_id },
                crate::nest_api::FakeCall::Unsubscribe {
                    labeler_id: labeler.0.to_vec()
                },
            ]
        );
        let cfg = store.current();
        assert_eq!(store.merges(), 1);
        let revoke = cfg
            .grant_events
            .iter()
            .find(|e| e.kind == GrantEventKind::Revoke)
            .expect("a signed Revoke event");
        assert_eq!(revoke.grant_id, grant_id.to_vec());
        assert_eq!(revoke.holder, HOLDER_PUBKEY.to_vec());
        revoke.verify(&kp.actor_id()).expect("owner-signed");
        assert!(grant_log::current_labeler_grant(&cfg, &labeler).is_none());
        assert!(!m.snapshot().entries[0].subscribed);
        assert!(m.snapshot().error.is_none());
    }

    /// A subscription that never had a grant (a public labeler, or one
    /// registered while no holder existed) just drops its row.
    #[tokio::test]
    async fn unsubscribing_without_a_grant_just_drops_the_row() {
        let (m, api, store, _kp) = granting_machine();
        api.set_entries(vec![mail_entry(0xAA, true)]);
        m.refresh().await;

        m.unsubscribe(0).await;

        assert_eq!(
            api.calls(),
            vec![crate::nest_api::FakeCall::Unsubscribe {
                labeler_id: vec![0xAA; 32]
            }]
        );
        assert_eq!(store.merges(), 0);
    }

    /// A refused revoke keeps the row: dropping the subscription while its
    /// grant lives would leave a capability the page no longer explains.
    #[tokio::test]
    async fn a_refused_revoke_keeps_the_row_and_records_nothing() {
        let (m, api, store, kp) = granting_machine();
        let labeler = labeler_of(0xAA);
        store.mutate(|cfg| {
            grant_log::record_mint(
                cfg,
                kp.signing_key(),
                [9u8; 16],
                HOLDER_PUBKEY,
                grant_log::bounded_mail_labeler_event_scope(&labeler),
                1000,
                2000,
                1000,
            )
            .unwrap();
        });
        api.set_revoke_grant_response(Err(LabelerCatalogApiError::Transient {
            detail: "offline".into(),
        }));
        api.set_entries(vec![mail_entry(0xAA, true)]);
        m.refresh().await;

        m.unsubscribe(0).await;

        assert_eq!(
            api.calls(),
            vec![crate::nest_api::FakeCall::RevokeGrant {
                grant_id: [9u8; 16]
            }],
            "no unsubscribe past a refused revoke"
        );
        assert_eq!(
            store.merges(),
            0,
            "the log records no Revoke the nest refused"
        );
        assert_eq!(snapshot_key(&m).as_deref(), Some(UNSUBSCRIBE_ERROR_KEY));
        assert!(
            m.snapshot().entries[0].subscribed,
            "no refresh happened on error"
        );
    }

    #[tokio::test]
    async fn refresh_populates_the_snapshot_and_notifies() {
        let (m, api, observer) = machine();
        api.set_entries(vec![entry(0xAA, false), entry(0xBB, true)]);

        m.refresh().await;

        let snap = m.snapshot();
        assert_eq!(snap.entries.len(), 2);
        assert!(!snap.entries[0].subscribed);
        assert!(snap.entries[1].subscribed);
        assert!(snap.error.is_none());
        assert_eq!(observer.count(), 1);
    }

    /// The defect this field exists to kill: a page that has not finished its
    /// first read is NOT an empty page, and must not license an empty state.
    /// `entries.is_empty()` is true in both pictures, so `loaded` is the only
    /// thing that separates them (`docs/goal/ui/README.md` § *List pages:
    /// loading is not empty*).
    #[tokio::test]
    async fn a_page_that_has_not_refreshed_is_not_a_loaded_empty_page() {
        let (m, _api, _observer) = machine();

        let snap = m.snapshot();
        assert!(snap.entries.is_empty(), "nothing read yet");
        assert!(
            !snap.loaded,
            "a machine that has never refreshed must not claim to be loaded — \
             painting an empty state off `entries.is_empty()` alone is the bug"
        );
    }

    /// A read that RETURNED and found nothing is the genuine empty state — the
    /// one picture in which an empty-state element may paint.
    #[tokio::test]
    async fn a_refresh_that_returns_nothing_is_a_loaded_empty_page() {
        let (m, api, _observer) = machine();
        api.set_entries(vec![]);

        m.refresh().await;

        let snap = m.snapshot();
        assert!(snap.entries.is_empty());
        assert!(snap.loaded, "a returned read licenses the empty state");
    }

    /// `loaded` is monotonic: a FAILED first read leaves the page unloaded, so
    /// `error-message` does the talking rather than a false "no labelers
    /// published yet" beside it.
    #[tokio::test]
    async fn a_failed_first_refresh_leaves_the_page_unloaded() {
        let (m, api, _observer) = machine();
        api.fail_list(LabelerCatalogApiError::Transient {
            detail: "boom".into(),
        });

        m.refresh().await;

        let snap = m.snapshot();
        assert!(snap.error.is_some(), "the failure is on screen");
        assert!(
            !snap.loaded,
            "a read that never returned may not license an empty state"
        );
    }

    /// …and a LATER failure does not re-arm the loading state under rows the
    /// user can still see.
    #[tokio::test]
    async fn a_later_failure_does_not_clear_loaded() {
        let (m, api, _observer) = machine();
        api.set_entries(vec![entry(0xAA, false)]);
        m.refresh().await;
        assert!(m.snapshot().loaded);

        api.fail_list(LabelerCatalogApiError::Transient {
            detail: "boom".into(),
        });
        m.refresh().await;

        let snap = m.snapshot();
        assert_eq!(snap.entries.len(), 1, "prior rows still on screen");
        assert!(snap.loaded, "loaded is monotonic — never cleared");
    }

    #[tokio::test]
    async fn refresh_failure_keeps_prior_data_and_sets_error() {
        let (m, api, _observer) = machine();
        api.set_entries(vec![entry(0xAA, false)]);
        m.refresh().await;

        api.fail_list(LabelerCatalogApiError::Transient {
            detail: "boom".into(),
        });
        m.refresh().await;

        let snap = m.snapshot();
        assert_eq!(snap.entries.len(), 1, "prior data kept on failure");
        assert!(snap.error.is_some());
    }

    #[tokio::test]
    async fn inspect_opens_the_panel_from_the_decoded_result() {
        let (m, api, observer) = machine();
        let e = entry(0xAA, false);
        api.set_entries(vec![e.clone()]);
        m.refresh().await;

        let labeler_id_bytes = hex_decode(&e.labeler_id);
        api.set_inspect_response(
            labeler_id_bytes.clone(),
            Ok(InspectResult {
                view: LabelerInspectView {
                    labeler_id: e.labeler_id.clone(),
                    version: 1,
                    artifact_kind: "wasm".into(),
                    wasm_hash: e.wasm_hash.clone(),
                    wasm_size: e.wasm_size,
                    needs_text: true,
                    needs_hashtags: false,
                    needs_media_metadata: false,
                    needs_author: false,
                    needs_attachment_bytes: false,
                    verified: true,
                    list_name: None,
                    model_name: None,
                    list_entries: Vec::new(),
                    model_ngrams: Vec::new(),
                },
            }),
        );

        m.inspect(0).await;

        let snap = m.snapshot();
        let view = snap.inspecting.expect("inspect panel open");
        assert!(view.verified);
        assert!(view.needs_text);
        assert_eq!(
            api.calls(),
            vec![crate::nest_api::FakeCall::Inspect {
                labeler_id: labeler_id_bytes
            }]
        );
        assert_eq!(observer.count(), 2, "refresh + inspect each tick once");
    }

    #[tokio::test]
    async fn inspect_out_of_range_index_is_a_no_op() {
        let (m, api, observer) = machine();
        m.inspect(5).await;
        assert!(api.calls().is_empty());
        assert_eq!(observer.count(), 0);
    }

    #[tokio::test]
    async fn close_inspect_clears_the_panel() {
        let (m, api, _observer) = machine();
        let e = entry(0xAA, false);
        api.set_entries(vec![e.clone()]);
        m.refresh().await;
        api.set_inspect_response(
            hex_decode(&e.labeler_id),
            Ok(InspectResult {
                view: LabelerInspectView {
                    labeler_id: e.labeler_id.clone(),
                    version: 1,
                    artifact_kind: "wasm".into(),
                    wasm_hash: e.wasm_hash.clone(),
                    wasm_size: e.wasm_size,
                    needs_text: true,
                    needs_hashtags: false,
                    needs_media_metadata: false,
                    needs_author: false,
                    needs_attachment_bytes: false,
                    verified: true,
                    list_name: None,
                    model_name: None,
                    list_entries: Vec::new(),
                    model_ngrams: Vec::new(),
                },
            }),
        );
        m.inspect(0).await;
        assert!(m.snapshot().inspecting.is_some());

        m.close_inspect();

        assert!(m.snapshot().inspecting.is_none());
    }

    /// A machine built without grant seams (the identity-fault fallback) subscribes even a
    /// sealed-kind labeler with no grant — and no notice, since the gap is the
    /// app's, not the deployment's.
    #[tokio::test]
    async fn a_grant_less_machine_subscribes_with_no_grant_then_refreshes() {
        let (m, api, _observer) = machine();
        let e = mail_entry(0xAA, false);
        api.set_entries(vec![e.clone()]);
        m.refresh().await;
        // After the gesture, `list` reflects the now-subscribed row.
        api.set_entries(vec![mail_entry(0xAA, true)]);

        m.subscribe(0).await;

        assert_eq!(
            api.calls(),
            vec![crate::nest_api::FakeCall::Subscribe {
                labeler_id: hex_decode(&e.labeler_id),
                grant_id: None,
            }]
        );
        assert!(
            m.snapshot().entries[0].subscribed,
            "refreshed after subscribe"
        );
        assert!(m.snapshot().error.is_none());
    }

    #[tokio::test]
    async fn unsubscribe_then_refreshes() {
        let (m, api, _observer) = machine();
        let e = entry(0xAA, true);
        api.set_entries(vec![e.clone()]);
        m.refresh().await;
        api.set_entries(vec![entry(0xAA, false)]);

        m.unsubscribe(0).await;

        assert_eq!(
            api.calls(),
            vec![crate::nest_api::FakeCall::Unsubscribe {
                labeler_id: hex_decode(&e.labeler_id)
            }]
        );
        assert!(!m.snapshot().entries[0].subscribed);
    }

    #[tokio::test]
    async fn subscribe_failure_sets_error_without_refreshing_stale_data() {
        let (m, api, _observer) = machine();
        api.set_entries(vec![entry(0xAA, false)]);
        m.refresh().await;
        api.set_subscribe_response(Err(LabelerCatalogApiError::Transient {
            detail: "nope".into(),
        }));

        m.subscribe(0).await;

        let snap = m.snapshot();
        assert!(snap.error.is_some());
        assert!(!snap.entries[0].subscribed, "no refresh happened on error");
    }

    #[tokio::test]
    async fn subscribe_out_of_range_index_is_a_no_op() {
        let (m, api, observer) = machine();
        m.subscribe(5).await;
        assert!(api.calls().is_empty());
        assert_eq!(observer.count(), 0);
    }

    #[tokio::test]
    async fn unsubscribe_out_of_range_index_is_a_no_op() {
        let (m, api, observer) = machine();
        m.unsubscribe(5).await;
        assert!(api.calls().is_empty());
        assert_eq!(observer.count(), 0);
    }

    #[tokio::test]
    async fn unsubscribe_failure_sets_error_without_refreshing_stale_data() {
        let (m, api, _observer) = machine();
        api.set_entries(vec![entry(0xAA, true)]);
        m.refresh().await;
        api.set_unsubscribe_response(Err(LabelerCatalogApiError::Transient {
            detail: "nope".into(),
        }));

        m.unsubscribe(0).await;

        let snap = m.snapshot();
        assert!(snap.error.is_some());
        assert!(snap.entries[0].subscribed, "no refresh happened on error");
    }

    #[tokio::test]
    async fn inspect_failure_sets_error_and_leaves_the_panel_closed() {
        let (m, api, observer) = machine();
        let e = entry(0xAA, false);
        api.set_entries(vec![e.clone()]);
        m.refresh().await;
        api.set_inspect_response(
            hex_decode(&e.labeler_id),
            Err(LabelerCatalogApiError::NotFound {
                detail: "gone".into(),
            }),
        );

        m.inspect(0).await;

        let snap = m.snapshot();
        assert!(snap.inspecting.is_none(), "panel stays closed on failure");
        assert!(snap.error.is_some());
        assert_eq!(observer.count(), 2, "refresh + inspect each tick once");
    }

    #[test]
    fn not_found_and_transient_expose_their_detail_and_display() {
        let nf = LabelerCatalogApiError::NotFound {
            detail: "no such labeler".into(),
        };
        assert_eq!(nf.detail(), "no such labeler");
        assert_eq!(nf.to_string(), "no such labeler");

        let t = LabelerCatalogApiError::Transient {
            detail: "timed out".into(),
        };
        assert_eq!(t.detail(), "timed out");
        assert_eq!(t.to_string(), "timed out");
    }
}
