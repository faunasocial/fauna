//! Trait abstraction for the page's nest-side surface.
//!
//! The page reads + gestures go through [`LabelerCatalogNestApi`]. Production
//! code uses the WS-RPC [`ws_rpc::WsRpcLabelerCatalogNest`] (over
//! `fauna_client_labelers::LabelersClient`); tests use
//! [`FakeLabelerCatalogNestApi`] (gated under
//! `#[cfg(any(test, feature = "test-helpers"))]`). Mirrors
//! `fauna_devices_machine::nest_api`.
//!
//! Transport: every read/gesture rides the authenticated WS-RPC connection —
//! the page runs inside an already-logged-in session, so the seam is
//! constructed with the session's connected requester (`Arc<NestClient>`
//! native / `WsRpcClient` wasm) and needs no per-call URL or token. There is
//! no HTTP impl (the `no-http-ws-rpc-everywhere` directive).

pub mod fake;
#[cfg(feature = "rpc-glue")]
pub mod ws_rpc;

#[cfg(any(test, feature = "test-helpers"))]
pub use fake::{FakeCall, FakeLabelerCatalogNestApi};
#[cfg(all(feature = "rpc-glue", not(target_arch = "wasm32")))]
pub use ws_rpc::build_labeler_catalog_machine;
#[cfg(feature = "rpc-glue")]
pub use ws_rpc::build_labeler_catalog_machine_with_grants;

use crate::snapshots::LabelerCatalogEntry;

fauna_core::declare_api_error!(
    /// Failure of a page-level nest call. Mirrors
    /// `fauna_devices_machine::DevicesApiError`.
    LabelerCatalogApiError {
        /// The labeler_id named by a gesture (e.g. `subscribe`/`inspect`) no
        /// longer exists in the catalog.
        NotFound,
        /// The nest reached a decision and refused — a grant deposit or
        /// holder read it would not honour (the `fauna.capabilities.*` and
        /// holder-discovery calls the subscribe mint makes).
        Rejected,
        /// Transport fault / 5xx — retryable.
        Transient,
    }
);

// The mint/revoke failure enum (`machine::LabelerGrantError`) wraps this as
// a `#[source]`, which needs the std trait; `Debug` + `Display` are enough
// for the empty impl (the `@uniffi_flat_error` form of the macro does the same).
impl std::error::Error for LabelerCatalogApiError {}

/// One `fauna.labelers.inspect` read: the full signed metadata (decoded +
/// re-verified client-side — the inspect-before-subscribe transparency gate
/// re-runs the same `fauna_core::scoring::verify_labeler_metadata` the nest's
/// publish gate and the FFI holder use, so a compromised/lying nest can't
/// silently misrepresent a module's hash/size/signature to the browsing user)
/// plus the raw WASM bytes' size, projected for the inspect panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectResult {
    pub view: crate::snapshots::LabelerInspectView,
}

// `MaybeSendSync` supertrait + dual `async_trait` arm so the one seam serves
// native (`Arc<NestClient>`, `Send + Sync`) and wasm (the single-threaded
// `Rc`-based `WsRpcClient`, `!Send`) — see the identical pattern on
// `fauna_devices_machine::DevicesNestApi`.
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait LabelerCatalogNestApi: fauna_core::MaybeSendSync + std::fmt::Debug {
    /// `fauna.labelers.list` — every catalog entry, each stamped with whether
    /// the caller currently subscribes to it.
    async fn list(&self) -> Result<Vec<LabelerCatalogEntry>, LabelerCatalogApiError>;

    /// `fauna.labelers.inspect` — fetch + decode + re-verify one labeler's
    /// full signed record + WASM bytes.
    async fn inspect(&self, labeler_id: Vec<u8>) -> Result<InspectResult, LabelerCatalogApiError>;

    /// `fauna.labelers.subscribe`. `grant_id` names the per-labeler grant the
    /// machine minted and deposited first for a `wasm` labeler over sealed
    /// content (`LabelerCatalogMachine::subscribe`; the nest links the
    /// subscription row to it 1:1), and is `None` for a public-content labeler
    /// — or for a sealed-kind one the machine could not mint for (no holder to
    /// seal to, mail not enabled), which the nest registers anyway and which
    /// never drains until a grant exists.
    async fn subscribe(
        &self,
        labeler_id: Vec<u8>,
        grant_id: Option<[u8; 16]>,
    ) -> Result<(), LabelerCatalogApiError>;

    /// `fauna.labelers.unsubscribe`.
    async fn unsubscribe(&self, labeler_id: Vec<u8>) -> Result<(), LabelerCatalogApiError>;

    /// `fauna.capabilities.mint` — deposit the client-built, HPKE-sealed
    /// per-labeler `GrantBlob` (canonical bytes) the machine records in the
    /// owner's signed grant log **before** calling this
    /// (`grant_log::UndepositedGrant` — record-then-deposit, `nests.md`
    /// § Trust facet — grants). The nest stores it opaque, keyed
    /// `(owner, grant_id)`, and never opens it.
    async fn mint_grant(&self, grant_blob: Vec<u8>) -> Result<(), LabelerCatalogApiError>;

    /// `fauna.capabilities.revoke` — delete the `(owner, grant_id)` row; the
    /// holder's next fetch goes dark and the labeler's drain stops. Called
    /// **before** the log records the revoke (revoke narrows, so the nest
    /// leads — `nests.md` § Record-then-deposit).
    async fn revoke_grant(&self, grant_id: [u8; 16]) -> Result<(), LabelerCatalogApiError>;

    /// Enumerate the nest's approved **content-processor holders** — the
    /// grant seal targets (`fauna.bridges.list_service_users` filtered to
    /// content-reading roles, then each one's `fetch_bridge_pubkey`;
    /// `fauna_client_bridges::discover_holders`, the one derivation every
    /// mint shares). The subscribe mint seals to the `mda`-role holder.
    async fn content_processor_holders(
        &self,
    ) -> Result<Vec<fauna_client_bridges::HolderInfo>, LabelerCatalogApiError>;
}
