//! Trait abstraction for the wizard's nest-side write surface.
//!
//! `submit()` goes through this trait. Production code uses the WS-RPC
//! [`ws_rpc::WsRpcFolderNest`] (over `fauna-client-folders`); tests use
//! [`FakeFolderNestApi`] (gated under `#[cfg(any(test, feature =
//! "test-helpers"))]`) to fixture responses without a transport. Mirrors
//! `fauna_client_mail_settings`'s `…Nest` seams.
//!
//! Transport: these kinds (`fauna.folders.{create,places.set}`) ride the
//! authenticated WS-RPC connection — the wizard runs inside an already-logged-in
//! session, so the seam is constructed with the session's connected requester
//! (`Arc<NestClient>` native / `WsRpcClient` wasm) and needs no per-call URL or
//! token. (This replaced the original `POST /api/v1/file-sets` HTTP seam in the
//! `no-http-ws-rpc-everywhere` migration; the HTTP twins are deprecated, slated
//! for eventual deletion.)

pub mod fake;
pub mod types;
#[cfg(feature = "rpc-glue")]
pub mod ws_rpc;

#[cfg(any(test, feature = "test-helpers"))]
pub use fake::{FakeCall, FakeFolderNestApi};
pub use types::*;
#[cfg(feature = "rpc-glue")]
pub use ws_rpc::build_folder_wizard_machine;

use async_trait::async_trait;

// `MaybeSendSync` supertrait + dual `async_trait` arm so the one seam serves
// native (`Arc<NestClient>`, `Send + Sync`) and wasm (the single-threaded
// `Rc`-based `WsRpcClient`, `!Send`) — see `fauna_core::MaybeSendSync` and the
// identical pattern on `fauna_client_mail_settings`'s `LocalDomainNest`. The
// native arm boxes `Send` futures (the submit task is driven by tokio); the wasm
// arm `!Send` ones (driven by `spawn_local`).
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait FolderNestApi: fauna_core::MaybeSendSync + std::fmt::Debug {
    /// `fauna.folders.list` — the actor's existing folders, as `(id, name)`
    /// rows ([`FolderRow`]).
    ///
    /// Read half of the seam. The wizard itself never needs it (the Devices page
    /// already holds the list), but the headless photo-library resolver does:
    /// deciding whether to reuse the "Photo Library" preset — or the set this
    /// device is already bound to, by its id — or create it, is a question about
    /// what already exists (`crate::photo_library`).
    async fn list_folder_rows(&self) -> Result<Vec<FolderRow>, FolderApiError>;

    /// `fauna.folders.create` — create the folder.
    async fn create_folder(&self, req: CreateFolderRequest) -> Result<(), FolderApiError>;

    /// `fauna.folders.places.set` — put one device place into `folder`, stated
    /// as flags.
    ///
    /// The one add/edit door onto a device place (the role-speaking
    /// `members.add` retired with the role contraction), so every point the
    /// wizard can express is sendable.
    async fn set_place(&self, folder: &str, place: SetPlaceRequest) -> Result<(), FolderApiError>;
}
