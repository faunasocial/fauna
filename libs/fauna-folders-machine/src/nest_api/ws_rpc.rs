//! WS-RPC production impl of the [`FolderNestApi`] seam, over
//! `fauna_client_folders::FoldersClient` (the shared `fauna.folders.*`
//! typed-call surface). Replaced the deleted HTTP `ReqwestFolderNestApi` in the
//! `no-http-ws-rpc-everywhere` migration: behavior-preserving transport swap —
//! the wizard's `submit()` flow is unchanged; only the bytes go over WS-RPC
//! instead of `POST /api/v1/file-sets` + `.../members`.
//!
//! Mirrors `fauna_client_mail_settings::rpc_glue`: a generic
//! [`WsRpcFolderNest<R>`] holds the kind-composition + error mapping once
//! (priority #2); the per-target concrete trait impls (native `Arc<NestClient>`,
//! wasm `WsRpcClient`) and the `build_folder_wizard_machine` constructors live
//! in the `cfg`-gated submodules below and just delegate.

use std::sync::Arc;

use async_trait::async_trait;
use fauna_client_folders::FolderKeyStore;
use fauna_client_folders::{FoldersClient, SetLifecycleError};
use fauna_protocol::folders::{FolderCreateRequest, PlacesSetRequest};
use fauna_protocol::{RpcErrorClass, RpcRequester};

use super::FolderNestApi;
use super::types::{CreateFolderRequest, FolderApiError, FolderRow, SetPlaceRequest};
use crate::machine::FolderWizardMachine;
use crate::observer::FolderWizardObserver;
use crate::state::DeviceOption;

/// Generic WS-RPC seam over any [`RpcRequester`]. Native binds
/// `R = Arc<NestClient>`, wasm `R = WsRpcClient`; the per-target trait impls
/// below delegate to these inherent methods so the logic is written once.
pub struct WsRpcFolderNest<R: RpcRequester> {
    folders: FoldersClient<R>,
    /// The account's folder-key custody (`fauna.state.folder-keys`) — the
    /// create helper writes the new set's nonce there before the nest sees the
    /// set ([`fauna_client_folders::create_set`]). `None` on a seam with no
    /// account custody (a bearer-only connection), which cannot create a set.
    custody: Option<Arc<dyn FolderKeyStore>>,
    /// The owner's seal root for a new set's `name_sealed` +
    /// `retention_policy_sealed` ([`fauna_client_folders::create_set_with_owner_root`]).
    /// Held here rather than as label custody on [`Self::folders`] on purpose: a
    /// new set is owner-only, but an owner-only custody on a client that could
    /// later update a bound set would seal under the wrong root. `None` (a
    /// bearer-only connection) creates with `name_hash` and no seal, and the
    /// update-time backfill stamps it.
    owner_seal: Option<fauna_core::crypto::BackupKey>,
}

// `FolderNestApi` requires `Debug`, but `FoldersClient` isn't `Debug`; the
// requester carries no renderable state worth printing, so a name-only impl
// satisfies the bound (mirrors the reqwest impl's derived `Debug`).
impl<R: RpcRequester> std::fmt::Debug for WsRpcFolderNest<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WsRpcFolderNest")
    }
}

impl<R> WsRpcFolderNest<R>
where
    R: RpcRequester,
    R::Error: RpcErrorClass + core::fmt::Display,
{
    /// `custody` is the account's folder-key custody the create helper writes
    /// the new set's nonce into — the seat's `PlaneFolderKeys` (web: the
    /// account port's forwarder); `None` builds a seam that can list and
    /// enroll but refuses to create.
    pub fn new(nest: R, custody: Option<Arc<dyn FolderKeyStore>>) -> Self {
        Self {
            custody,
            folders: FoldersClient::new(nest),
            owner_seal: None,
        }
    }

    /// Seal every set this seam creates under `owner` from birth.
    pub fn with_owner_seal_key(mut self, owner: fauna_core::crypto::BackupKey) -> Self {
        self.owner_seal = Some(owner);
        self
    }

    /// Owner-scoped on purpose (`list`, not `list_owned_and_shared`): the photo
    /// ingress writes into the user's OWN set, and the nest's `changes.record`
    /// gate (`owned_folder`) is owner-scoped too — so a set shared *with* the
    /// user must not be a candidate for adoption.
    ///
    /// Read unrendered and opened here with the owner key ([`Self::owner_seal`]):
    /// since schema 114 a sealed set's row rests no plaintext name, and
    /// [`Self::folders`] holds no label custody, so its rendered `list` would
    /// omit every sealed set — the Photo Library included. *Opening* with an
    /// owner-only custody is safe (the wrong-root hazard above is a sealing
    /// one); a row it cannot open — a set sealed under a group generation —
    /// is dropped, never listed nameless.
    async fn do_list_rows(&self) -> Result<Vec<FolderRow>, FolderApiError> {
        let reply = self.folders.list_wire().await.map_err(map_err)?;
        let custody = self
            .owner_seal
            .clone()
            .map(fauna_core::label_custody::LabelCustody::owner_only)
            .unwrap_or_default();
        let mut rows = Vec::with_capacity(reply.folders.len());
        for fs in reply.folders {
            let wire_hash = fs.name_hash.as_ref().map(|b| &b[..]);
            let (keys, _) = custody
                .keys_for_hash(&fauna_core::label_custody::set_name_label_salt(
                    wire_hash, &fs.name,
                ))
                .await;
            match fauna_core::label_custody::render_set_name(
                &keys,
                fs.name_sealed.as_ref().map(|b| &b[..]),
                &fs.name,
                wire_hash,
            ) {
                fauna_core::path_crypto::SealedLabelRender::Sealed(name)
                | fauna_core::path_crypto::SealedLabelRender::Plaintext(name) => {
                    rows.push(FolderRow { id: fs.id, name })
                }
                fauna_core::path_crypto::SealedLabelRender::Omit => {}
            }
        }
        Ok(rows)
    }

    async fn do_create(&self, req: CreateFolderRequest) -> Result<(), FolderApiError> {
        // The machine seam carries a typed `RetentionPolicy`; the wire kind takes
        // the opaque JSON string the nest stores (`folders.retention_policy`
        // is a TEXT column the nest treats opaquely). Serialize before send.
        let retention_policy = req
            .retention_policy
            .as_ref()
            .map(|r| serde_json::to_string(r).unwrap_or_default());
        let wire = FolderCreateRequest {
            name: req.name,
            retention_policy,
            conflict_policy: req.conflict_policy,
            ..Default::default()
        };
        let Some(custody) = &self.custody else {
            return Err(FolderApiError::BadRequest {
                detail: "creating a folder needs the account's folder-key custody".into(),
            });
        };
        match fauna_client_folders::create_set_with_owner_root(
            &self.folders,
            &**custody,
            wire,
            self.owner_seal.clone(),
        )
        .await
        {
            Ok(_) => Ok(()),
            Err(SetLifecycleError::Nest(e)) => Err(map_err(e)),
            Err(SetLifecycleError::Custody(e)) => Err(FolderApiError::Transient {
                detail: format!("{e:#}"),
            }),
        }
    }

    /// The one enrollment door: every one of the eight points the wizard can
    /// express is sendable.
    async fn do_set_place(
        &self,
        folder: &str,
        place: SetPlaceRequest,
    ) -> Result<(), FolderApiError> {
        let wire = PlacesSetRequest {
            name: folder.to_string(),
            device_id: place.device_id,
            flags: place.flags,
            ..Default::default()
        };
        self.folders
            .places_set(wire)
            .await
            .map(|_| ())
            .map_err(map_err)
    }
}

fauna_core::map_rpc_error! {
    /// Map a transport `R::Error` onto the seam's [`FolderApiError`], mirroring the
    /// deleted reqwest impl's status→variant mapping but keyed on the WS-RPC
    /// `RpcError.code` (`fauna.folders.{conflict,not_found,invalid_request}`)
    /// rather than an HTTP status. A transport fault (the request never reached a
    /// server rejection) is `Transient` — the same class the reqwest impl gave a
    /// network error.
    fn map_err(e) -> FolderApiError {
        "conflict" => Conflict,
        "not_found" => NotFound,
        "invalid_request" | "malformed" => BadRequest,
    }
}

// ── Native (`Arc<NestClient>`) ──────────────────────────────────────────────
#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::*;
    use fauna_client::NestClient;

    #[async_trait]
    impl FolderNestApi for WsRpcFolderNest<Arc<NestClient>> {
        async fn list_folder_rows(&self) -> Result<Vec<FolderRow>, FolderApiError> {
            self.do_list_rows().await
        }
        async fn create_folder(&self, req: CreateFolderRequest) -> Result<(), FolderApiError> {
            self.do_create(req).await
        }
        async fn set_place(
            &self,
            folder: &str,
            place: SetPlaceRequest,
        ) -> Result<(), FolderApiError> {
            self.do_set_place(folder, place).await
        }
    }

    /// Build a [`FolderWizardMachine`] over `nest`'s authenticated WS-RPC
    /// connection, its create helper writing into `custody` (`None`: the
    /// wizard lists and enrolls but refuses to create). The native entry the
    /// linux app + `fauna-ffi` call.
    pub fn build_folder_wizard_machine(
        nest: Arc<NestClient>,
        custody: Option<Arc<dyn FolderKeyStore>>,
        observer: Arc<dyn FolderWizardObserver>,
        available_devices: Vec<DeviceOption>,
    ) -> Arc<FolderWizardMachine> {
        // The authenticated connection already carries the identity, so a native
        // wizard seals a new set at birth with no per-app plumbing.
        let owner = nest
            .auth()
            .keypair()
            .map(|kp| fauna_core::crypto::BackupKey::derive(kp.secret_bytes()));
        let mut seam = WsRpcFolderNest::new(nest, custody);
        if let Some(owner) = owner {
            seam = seam.with_owner_seal_key(owner);
        }
        let api: Arc<dyn FolderNestApi> = Arc::new(seam);
        FolderWizardMachine::new(observer, available_devices, api)
    }
}
#[cfg(not(target_arch = "wasm32"))]
pub use native::build_folder_wizard_machine;

// ── Wasm (`WsRpcClient`) ────────────────────────────────────────────────────
#[cfg(target_arch = "wasm32")]
mod wasm {
    use super::*;
    use fauna_rpc_wasm::WsRpcClient;

    #[async_trait(?Send)]
    impl FolderNestApi for WsRpcFolderNest<WsRpcClient> {
        async fn list_folder_rows(&self) -> Result<Vec<FolderRow>, FolderApiError> {
            self.do_list_rows().await
        }
        async fn create_folder(&self, req: CreateFolderRequest) -> Result<(), FolderApiError> {
            self.do_create(req).await
        }
        async fn set_place(
            &self,
            folder: &str,
            place: SetPlaceRequest,
        ) -> Result<(), FolderApiError> {
            self.do_set_place(folder, place).await
        }
    }

    /// Build a [`FolderWizardMachine`] over the SPA's browser `WsRpcClient`.
    /// The wasm entry `fauna-wasm-folders` wraps for the web wizard; the
    /// create helper writes into `custody` (the account port's forwarder).
    /// The browser connection carries no keypair, so the caller hands in the
    /// owner's seal root (`BackupKey::derive` of the actor secret) — the twin
    /// of what the native builder reads off the connection — and every set the
    /// wizard creates is sealed from birth.
    pub fn build_folder_wizard_machine(
        nest: WsRpcClient,
        custody: Option<Arc<dyn FolderKeyStore>>,
        owner_seal: Option<fauna_core::crypto::BackupKey>,
        observer: Arc<dyn FolderWizardObserver>,
        available_devices: Vec<DeviceOption>,
    ) -> Arc<FolderWizardMachine> {
        let mut seam = WsRpcFolderNest::new(nest, custody);
        if let Some(owner) = owner_seal {
            seam = seam.with_owner_seal_key(owner);
        }
        let api: Arc<dyn FolderNestApi> = Arc::new(seam);
        FolderWizardMachine::new(observer, available_devices, api)
    }
}
#[cfg(target_arch = "wasm32")]
pub use wasm::build_folder_wizard_machine;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::RetentionPolicy;
    use fauna_client_testkit::CapturingRequester;

    fn create_req() -> CreateFolderRequest {
        CreateFolderRequest {
            name: "docs".into(),
            retention_policy: Some(RetentionPolicy {
                max_snapshots: 5,
                max_age_days: 90,
            }),
            conflict_policy: None,
        }
    }

    #[tokio::test]
    async fn create_targets_kind_and_serializes_retention_as_json_string() {
        // A transport fault → the result is ignored here; the request is what
        // is under test.
        let req = Arc::new(CapturingRequester::transport_fault());
        let seam = WsRpcFolderNest::new(Arc::clone(&req), Some(custody()));
        let _ = seam.do_create(create_req()).await;

        let calls = req.calls();
        assert_eq!(calls.len(), 1);
        let (kind, payload) = &calls[0];
        assert_eq!(*kind, "fauna.folders.create");
        // The typed RetentionPolicy must ride as the opaque JSON *string* the
        // nest stores, not a nested object.
        let rp = payload.get("retention_policy").unwrap();
        assert!(rp.is_string(), "retention_policy must be a JSON string");
        assert_eq!(
            rp.as_str().unwrap(),
            r#"{"max_snapshots":5,"max_age_days":90}"#
        );
        assert!(
            payload.get("rescan_interval_secs").is_none(),
            "the create request carries no cadence"
        );
    }

    /// The wizard's create is sealed from birth when the seam holds the owner
    /// key (every native build: the connection's identity), and carries the
    /// address with no seal when it does not (a bearer-only seam) — never a path list.
    #[tokio::test]
    async fn create_seals_the_name_at_birth_only_with_an_owner_key() {
        let keyed = Arc::new(CapturingRequester::transport_fault());
        let seam = WsRpcFolderNest::new(Arc::clone(&keyed), Some(custody()))
            .with_owner_seal_key(fauna_core::crypto::BackupKey::from_bytes([7u8; 32]));
        let _ = seam.do_create(create_req()).await;
        let (_, payload) = &keyed.calls()[0];
        assert!(payload.get("name_hash").is_some());
        assert!(payload.get("name_sealed").is_some(), "{payload:?}");
        assert!(payload.get("retention_policy_sealed").is_some());
        assert!(payload.get("include_paths").is_none());

        let keyless = Arc::new(CapturingRequester::transport_fault());
        let seam = WsRpcFolderNest::new(Arc::clone(&keyless), Some(custody()));
        let _ = seam.do_create(create_req()).await;
        let (_, payload) = &keyless.calls()[0];
        assert!(payload.get("name_hash").is_some());
        assert!(payload.get("name_sealed").is_none());
    }

    #[tokio::test]
    async fn set_place_targets_kind_with_folder_name_and_flags() {
        let req = Arc::new(CapturingRequester::transport_fault());
        let seam = WsRpcFolderNest::new(Arc::clone(&req), None);
        // A point no legacy role ever named — the seam carries flags, so every
        // point is sendable.
        let _ = seam
            .do_set_place(
                "docs",
                SetPlaceRequest {
                    device_id: "aa".repeat(32),
                    flags: fauna_protocol::folders::PlaceFlags {
                        originates: false,
                        accepts: true,
                        applies_deletes: true,
                        ..Default::default()
                    },
                },
            )
            .await;
        let calls = req.calls();
        let (kind, payload) = &calls[0];
        assert_eq!(*kind, "fauna.folders.places.set");
        // Addressed by hash alone: the funnel takes the plaintext name off.
        assert!(payload.get("name").is_none());
        assert!(payload.get("name_hash").is_some());
        let flags = payload.get("flags").unwrap();
        assert_eq!(flags.get("originates").unwrap(), false);
        assert_eq!(flags.get("accepts").unwrap(), true);
        assert_eq!(flags.get("applies_deletes").unwrap(), true);
        assert!(
            payload.get("role").is_none(),
            "the seam puts no role spelling on the wire — a place is its flags"
        );
    }

    fn seam_returning(code: &str, detail: &str) -> WsRpcFolderNest<Arc<CapturingRequester>> {
        WsRpcFolderNest::new(
            Arc::new(CapturingRequester::rejecting_code(code, detail)),
            Some(custody()),
        )
    }

    #[tokio::test]
    async fn rejection_codes_map_to_variants_with_detail_text() {
        let conflict = seam_returning("fauna.folders.conflict", "name already exists")
            .do_create(create_req())
            .await;
        assert!(
            matches!(conflict, Err(FolderApiError::Conflict { detail }) if detail == "name already exists")
        );

        let not_found = seam_returning("fauna.folders.not_found", "no such set")
            .do_set_place(
                "x",
                SetPlaceRequest {
                    device_id: "z".into(),
                    flags: fauna_protocol::folders::PlaceFlags::default_place(),
                },
            )
            .await;
        assert!(
            matches!(not_found, Err(FolderApiError::NotFound { detail }) if detail == "no such set")
        );

        let bad = seam_returning("fauna.folders.invalid_request", "bad mode")
            .do_create(create_req())
            .await;
        assert!(matches!(bad, Err(FolderApiError::BadRequest { detail }) if detail == "bad mode"));
    }

    #[tokio::test]
    async fn transport_fault_maps_to_transient() {
        // No RpcError → never reached a server rejection.
        let seam = WsRpcFolderNest::new(
            Arc::new(CapturingRequester::transport_fault()),
            Some(custody()),
        );
        let r = seam.do_create(create_req()).await;
        assert!(matches!(r, Err(FolderApiError::Transient { .. })));
    }

    #[tokio::test]
    async fn create_sends_a_nonce_minted_into_custody_first() {
        let req = Arc::new(CapturingRequester::transport_fault());
        let held = Arc::new(fauna_client_folders::MemoryFolderKeyStore::default());
        let seam = WsRpcFolderNest::new(
            Arc::clone(&req),
            Some(Arc::clone(&held) as Arc<dyn FolderKeyStore>),
        );
        let _ = seam.do_create(create_req()).await;
        let calls = req.calls();
        let (_, payload) = &calls[0];
        let sent: Vec<u8> = serde_json::from_value(payload.get("set_nonce").unwrap().clone())
            .expect("a 32-byte nonce on the wire");
        assert_eq!(sent.len(), 32);
        let cfg = held.snapshot();
        assert_eq!(
            fauna_client_folders::custody::live_set_nonce(&cfg, "docs").map(|n| n.to_vec()),
            Some(sent),
            "the nest gets the nonce custody holds"
        );
    }

    #[tokio::test]
    async fn a_keyless_seam_refuses_to_create_and_sends_nothing() {
        let req = Arc::new(CapturingRequester::transport_fault());
        let seam = WsRpcFolderNest::new(Arc::clone(&req), None);
        let r = seam.do_create(create_req()).await;
        assert!(matches!(r, Err(FolderApiError::BadRequest { .. })));
        assert!(req.calls().is_empty());
    }

    fn custody() -> Arc<dyn FolderKeyStore> {
        Arc::new(fauna_client_folders::MemoryFolderKeyStore::default())
    }
}
