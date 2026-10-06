//! WS-RPC production impls of the page seams, over
//! `fauna_client_folders::FoldersClient` + `fauna_client_sync::SyncClient`
//! (the shared `fauna.folders.*` / `fauna.sync.*` typed-call surfaces). This
//! is the directive-correct (`no-http-ws-rpc-everywhere`) consumer — no HTTP.
//!
//! Mirrors `fauna_folders_machine::nest_api::ws_rpc`: a generic
//! [`WsRpcDevicesNest<R>`] holds the kind-composition + wire→snapshot
//! transcription + error mapping once (priority #2); the per-target concrete
//! trait impls (native `Arc<NestClient>`, wasm `WsRpcClient`), the
//! [`WizardFactory`] impl, and the `build_devices_machine` constructors live in
//! the `cfg`-gated submodules below and just delegate.

use std::sync::Arc;

use async_trait::async_trait;
use fauna_client_folders::FoldersClient;
use fauna_client_sync::SyncClient;
use fauna_core::identity::ActorKeypair;
use fauna_protocol::folders::{FolderUpdateRequest, NestPlacePolicy, SyncConflict};
use fauna_protocol::sync::{SyncDevice, SyncDeviceP2pParticipationSetRequest};
use fauna_protocol::{RpcErrorClass, RpcRequester};

use super::{CandidateVerdict, ChosenWinner, DevicesApiError, DevicesNestApi};
use crate::machine::DevicesMachine;
use crate::observer::DevicesObserver;
// The wire → summary `From` impls this module used to host now live beside the
// summary types in `crate::snapshots`, ungated — see the note there.
use fauna_protocol::folders::FolderSummary as WireFolderSummary;

/// Generic WS-RPC seam over any [`RpcRequester`]. Native binds
/// `R = Arc<NestClient>`, wasm `R = WsRpcClient`; the per-target trait impls
/// below delegate to these inherent methods so the logic is written once.
pub struct WsRpcDevicesNest<R: RpcRequester> {
    /// The connection itself, kept beside the typed clients because the
    /// audience flip builds an *attesting* `FoldersClient` per call from the
    /// key the machine hands it (see `do_set_folder_audience`).
    nest: R,
    folders: FoldersClient<R>,
    sync: SyncClient<R>,
    web: fauna_client_web::WebClient<R>,
    /// The account's folder-key custody (`fauna.state.folder-keys`) — the
    /// delete helper retires the set's custody there before the nest delete
    /// ([`fauna_client_folders::delete_set`]), and a restore resolves the set
    /// nonce it signs under from it. `None` on a seam with no account custody,
    /// which cannot delete a set and records a restore unsigned.
    custody: Option<Arc<dyn fauna_client_folders::FolderKeyStore>>,
    /// The owner's identity as a change-record signer — a version restore
    /// records a new head, and every record is writer-signed. `None` with no
    /// identity (the restore then records unsigned).
    signer: Option<Arc<fauna_protocol::sync_writer_sig::ChangeSigner>>,
    /// The predecessor ids this seam's reader seat proved by its own
    /// statement walk ([`Self::judged_candidate`]) — kept for the seam's
    /// lifetime so a proven id is not asked for again.
    learned: fauna_client_sync::row_judge::LearnedPredecessors,
}

// `DevicesNestApi` requires `Debug`, but the clients aren't `Debug`; the
// requester carries no renderable state, so a name-only impl satisfies the bound.
impl<R: RpcRequester> std::fmt::Debug for WsRpcDevicesNest<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WsRpcDevicesNest")
    }
}

impl<R> WsRpcDevicesNest<R>
where
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass + core::fmt::Display,
{
    /// `identity` is the owner's keypair a restore's change record is signed
    /// with; `custody` the account's folder-key custody the delete helper
    /// writes (`None` builds a seam that refuses to delete a set).
    pub fn new(
        nest: R,
        identity: Option<ActorKeypair>,
        custody: Option<Arc<dyn fauna_client_folders::FolderKeyStore>>,
    ) -> Self {
        Self {
            folders: FoldersClient::new(nest.clone()),
            sync: SyncClient::new(nest.clone()),
            web: fauna_client_web::WebClient::new(nest.clone()),
            signer: identity
                .as_ref()
                .map(|kp| Arc::new(fauna_protocol::sync_writer_sig::ChangeSigner::direct(kp))),
            custody,
            nest,
            learned: Default::default(),
        }
    }

    async fn do_webdav_served(&self, rows: &[WireFolderSummary]) -> Vec<bool> {
        // Custody, never the row's `webdav_enabled` (ruling (7)(b)(ii) rule (2)).
        // No custody, or a read that fails, reads every row not served.
        let cfg = match &self.custody {
            Some(store) => store.load().await.ok(),
            None => None,
        };
        rows.iter()
            .map(|row| {
                cfg.as_ref()
                    .is_some_and(|cfg| fauna_client_folders::custody_served(row, cfg))
            })
            .collect()
    }

    async fn do_web_subdomain_enabled(&self) -> Option<bool> {
        // Best-effort by the trait's contract: any failure (a transport
        // fault) is `None`, and the website hint degrades
        // to the combined wording — never a page error, never a claim of live.
        self.web.get_subdomain_enabled().await.ok()
    }

    async fn do_list_devices(&self) -> Result<Vec<SyncDevice>, DevicesApiError> {
        // Handed on as wire rows — the machine renders the sealed label before
        // transcribing (see `DevicesNestApi::list_devices`).
        let reply = self.sync.devices_list().await.map_err(map_err)?;
        Ok(reply.devices)
    }

    async fn do_list_folders(&self) -> Result<Vec<WireFolderSummary>, DevicesApiError> {
        // Opt into the member-visible projection (B3): the folders management
        // page shows sets shared *with* the caller alongside their own. Member
        // rows carry `role == "member"` + the owner handle; the client filters
        // them to sets it has actually joined (see `FolderSummary::role`).
        //
        // Handed on as WIRE rows, unrendered — the machine renders the sealed
        // set name and selective-sync pair before transcribing (see
        // `DevicesMachine::render_folders`), exactly as it does for devices and
        // conflicts. This adapter holds no label custody: rendering here would
        // omit every set whose name rests sealed-only (schema 114), and
        // transcribing would drop the `*_sealed` columns before any custody
        // could open them.
        let reply = self
            .folders
            .list_owned_and_shared_wire()
            .await
            .map_err(map_err)?;
        Ok(reply.folders)
    }

    async fn do_list_conflicts(&self) -> Result<Vec<SyncConflict>, DevicesApiError> {
        // The review-list read (file-sync.md § Conflicts): resolved rows carry
        // the winner + retained parents; unresolved (mark-only) reports ride along.
        // Handed on as wire rows — the machine renders the sealed path before
        // transcribing (see `DevicesNestApi::list_conflicts`).
        let reply = self.sync.conflicts_list_with(true).await.map_err(map_err)?;
        Ok(reply.conflicts)
    }

    async fn do_remove_device(&self, device_id: &str) -> Result<(), DevicesApiError> {
        self.sync
            .devices_delete(device_id)
            .await
            .map(|_| ())
            .map_err(map_err)
    }

    async fn do_request_p2p_off(&self, device_id: &str) -> Result<(), DevicesApiError> {
        self.sync
            .devices_p2p_participation_set(SyncDeviceP2pParticipationSetRequest {
                device_id: device_id.to_string(),
                participating: false,
                timestamp_ms: None,
                nonce: None,
                signature: None,
                extra: Default::default(),
            })
            .await
            .map(|_| ())
            .map_err(map_err)
    }

    async fn do_delete_folder(&self, name: &str) -> Result<(), DevicesApiError> {
        let Some(custody) = &self.custody else {
            return Err(DevicesApiError::BadRequest {
                detail: "deleting a folder needs the account's folder-key custody".into(),
            });
        };
        match fauna_client_folders::delete_set(&self.folders, &**custody, name).await {
            Ok(_) => Ok(()),
            Err(fauna_client_folders::SetLifecycleError::Nest(e)) => Err(map_err(e)),
            Err(fauna_client_folders::SetLifecycleError::Custody(e)) => {
                Err(DevicesApiError::Transient {
                    detail: format!("{e:#}"),
                })
            }
        }
    }

    async fn do_resolve_conflict(
        &self,
        id: i64,
        winner: Option<ChosenWinner>,
    ) -> Result<(), DevicesApiError> {
        // A mark-only resolve signs nothing and is untouched.
        let Some(winner) = winner else {
            return self
                .sync
                .conflicts_resolve(id, None)
                .await
                .map(|_| ())
                .map_err(map_err);
        };
        // A choose-winner mints a head row the chooser signs (writer-signed
        // change records, ruling (1)(ii)) from the nest's own candidate row —
        // so it is signed only over a version the judged history vouches for,
        // equal to that row in device, size and generation (ruling (10)(f)).
        // Anything else is refused here, nothing sent.
        let listed = self.sync.conflicts_list().await.map_err(map_err)?;
        let Some(conflict) = listed.conflicts.iter().find(|c| c.id == id) else {
            return Err(DevicesApiError::NotFound {
                detail: "the conflict is no longer listed".into(),
            });
        };
        let Some(vouched) = self
            .judged_candidate(&conflict.folder, &winner.path, &winner.manifest_hash)
            .await?
        else {
            return Err(DevicesApiError::BadRequest {
                detail: NOT_A_VERSION.into(),
            });
        };
        self.signing_client(&conflict.folder)
            .await
            .conflicts_resolve_judged(conflict, &vouched)
            .await
            .map(|_| ())
            .map_err(|e| match e {
                fauna_client_sync::ChooseWinnerError::Rpc(e) => map_err(e),
                refused => DevicesApiError::BadRequest {
                    detail: refused.to_string(),
                },
            })
    }

    /// The version `manifest_hash` names in the **judged** history of `path`
    /// in `folder` (ruling (10)(a)): soft-pruned versions included, looked up
    /// under the hash of `path` itself. `None`: no admitted version carries
    /// it. The reader seat is this seam's own — the signer's actor id, the
    /// set's nonce from custody (the one its records are signed under), and
    /// the predecessors its own statement walk proves. A seam with no
    /// identity or custody cannot judge: an error, as it could not sign.
    async fn judged_candidate(
        &self,
        folder: &str,
        path: &str,
        manifest_hash: &str,
    ) -> Result<Option<fauna_client_sync::restore_branch::JudgedCandidate>, DevicesApiError> {
        let (Some(signer), Some(custody)) = (&self.signer, &self.custody) else {
            return Err(DevicesApiError::BadRequest {
                detail: "this device holds no identity to verify the version with".into(),
            });
        };
        let nonce = fauna_client_folders::record_nonce(&self.folders, &**custody, folder)
            .await
            .map_err(|e| DevicesApiError::Transient {
                detail: format!("the set nonce did not resolve ({e})"),
            })?
            .ok_or_else(|| DevicesApiError::BadRequest {
                detail: "custody holds no nonce for the set".into(),
            })?;
        let seat = fauna_client_sync::row_judge::ReaderSeat {
            own: Some(signer.actor_id()),
            nonces: Some(fauna_client_sync::SetNonceSource::Fixed(nonce)),
            predecessors: Vec::new(),
            learned: self.learned.clone(),
        };
        let hash = fauna_core::sync::path_hash(path);
        let listing = self
            .sync
            .versions_list_judged(hash, folder, true, &seat)
            .await
            .map_err(map_err)?;
        Ok(fauna_client_sync::restore_branch::JudgedCandidate::find(
            listing,
            hash,
            manifest_hash,
            Some(&signer.actor_id()),
        ))
    }

    async fn do_judge_candidate(
        &self,
        folder: &str,
        path: &str,
        manifest_hash: &str,
    ) -> Result<CandidateVerdict, DevicesApiError> {
        use fauna_client_sync::restore_branch::RestoreDecision;
        let Some(found) = self.judged_candidate(folder, path, manifest_hash).await? else {
            return Ok(CandidateVerdict::NotAVersion);
        };
        Ok(match found.decision() {
            RestoreDecision::Verbatim => CandidateVerdict::Verbatim {
                size_bytes: found.version.size_bytes,
                content_key_version: found.version.content_key_version,
            },
            RestoreDecision::NeedsReseal => CandidateVerdict::NeedsReseal,
            RestoreDecision::Refuse => CandidateVerdict::NotAVersion,
        })
    }

    async fn do_set_folder_paths(
        &self,
        name: &str,
        include_paths: Option<Vec<String>>,
        exclude_paths: Option<Vec<String>>,
        include_sealed: Option<Vec<u8>>,
        exclude_sealed: Option<Vec<u8>>,
    ) -> Result<(), DevicesApiError> {
        // Selective-sync save: only the path fields ride; retention stays
        // `None` so the nest leaves them unchanged.
        //
        // Each seal rides with its plaintext, including when it is `None` — the
        // nest writes the pair together, so an unsealed save clears a seal that
        // would otherwise open to the list being replaced (path-sealing S6-c).
        let req = FolderUpdateRequest {
            name: name.to_string(),
            include_paths,
            exclude_paths,
            include_paths_sealed: include_sealed.map(fauna_protocol::ByteBuf::from),
            exclude_paths_sealed: exclude_sealed.map(fauna_protocol::ByteBuf::from),
            // Struct-update for the untouched tail (the repo's fixture-shape
            // convention) — every other field means "unchanged".
            ..Default::default()
        };
        self.folders.update(req).await.map(|_| ()).map_err(map_err)
    }

    async fn do_set_folder_conflict_policy(
        &self,
        name: &str,
        conflict_policy: &str,
    ) -> Result<(), DevicesApiError> {
        // Policy edit: only `conflict_policy` rides; every other field stays
        // `None` so the nest leaves it unchanged (the same selective-update
        // contract as the paths save above).
        let req = FolderUpdateRequest {
            name: name.to_string(),
            conflict_policy: Some(conflict_policy.to_string()),
            ..Default::default()
        };
        self.folders.update(req).await.map(|_| ()).map_err(map_err)
    }

    async fn do_set_folder_audience(
        &self,
        name: &str,
        audience: &str,
        attestor: Option<Arc<fauna_core::identity::ActorKeypair>>,
    ) -> Result<(), DevicesApiError> {
        // No content key and nothing staged — only `audience` rides, the same
        // selective-update contract as the policy edit above; every direction
        // the control sends, the bound ->shared flip-back included,
        // converges off the projected audience. What the `->public` direction
        // DOES carry is the owner's signed attestation, which `set_audience`
        // mints only under a wired identity key — so the client is built per
        // call from the key the machine holds, rather than the keyless
        // `self.folders` the reads use.
        let folders = match attestor {
            Some(keypair) => FoldersClient::new(self.nest.clone()).with_audience_attestor(keypair),
            None => FoldersClient::new(self.nest.clone()),
        };
        folders.set_audience(name, audience).await.map_err(map_err)
    }

    async fn do_set_folder_website_enabled(
        &self,
        name: &str,
        enabled: bool,
    ) -> Result<(), DevicesApiError> {
        self.folders
            .set_website_enabled(name, enabled)
            .await
            .map_err(map_err)
    }

    async fn do_set_folder_residency(
        &self,
        name: &str,
        residency: &str,
    ) -> Result<(), DevicesApiError> {
        self.folders
            .set_residency(name, residency)
            .await
            .map_err(map_err)
    }

    async fn do_set_folder_place(
        &self,
        name: &str,
        device_id: &str,
        originates: bool,
        accepts: bool,
        applies_deletes: bool,
    ) -> Result<(), DevicesApiError> {
        // The point applies WHOLE — all three flags every time; every point
        // is writable and goes out unrounded.
        let req = fauna_protocol::folders::PlacesSetRequest {
            name: name.to_string(),
            device_id: device_id.to_string(),
            flags: fauna_protocol::folders::PlaceFlags {
                originates,
                accepts,
                applies_deletes,
                ..Default::default()
            },
            ..Default::default()
        };
        self.folders
            .places_set(req)
            .await
            .map(|_| ())
            .map_err(map_err)
    }

    async fn do_set_folder_nest_place(
        &self,
        name: &str,
        snapshots: Option<bool>,
        quiet_secs: Option<i64>,
        retention: Option<String>,
        version_retention: Option<fauna_folders_machine::VersionRetentionWrite>,
    ) -> Result<(), DevicesApiError> {
        // The nest place's policy rides WHOLE (backup-restore.md § 8b): an
        // omitted knob clears back to unset, which is what expresses the third
        // state over a wire that cannot carry a nested Option. So `nest_place`
        // is always `Some(...)` here — sending `None` would mean "leave the
        // whole policy alone", the one thing this gesture never wants.
        let req = FolderUpdateRequest {
            name: name.to_string(),
            nest_place: Some(NestPlacePolicy {
                snapshots,
                quiet_secs,
                ..Default::default()
            }),
            retention_policy: retention,
            // The fourth knob rides the SAME save as its own whole policy
            // (file-versions.md § Retention ruling 1): `None` = leave
            // unchanged; a binds-nothing policy clears (the nest rests NULL).
            version_retention: version_retention.map(|w| {
                fauna_protocol::folders::VersionRetention {
                    max_versions_per_path: w.max_versions_per_path,
                    max_age_days: w.max_age_days,
                    ..Default::default()
                }
            }),
            ..Default::default()
        };
        self.folders.update(req).await.map(|_| ()).map_err(map_err)
    }

    #[allow(clippy::too_many_arguments)]
    async fn do_restore_file_version(
        &self,
        folder: &str,
        device_id: &str,
        path: &str,
        manifest_hash: String,
        size_bytes: i64,
        content_key_version: Option<u64>,
        path_sealed: Option<Vec<u8>>,
    ) -> Result<(), DevicesApiError> {
        // Restore = re-point (file-sync.md § Restore): the one shared record
        // shape (`SyncClient::restore_version`, self-healing) Media, the
        // windows shell verb, and this review-list re-point all ride — the
        // three surfaces can never drift apart. The seal is minted by the
        // machine's `LabelCustody` (S8 D2 closed the keyless seam here) and
        // carried verbatim; `None` still records plaintext-only, best-effort.
        //
        // Signed like every record: the set's nonce is read fresh (this seam
        // holds no engine for the set) and the identity signs under it.
        self.signing_client(folder)
            .await
            .restore_version(
                folder,
                device_id,
                path,
                manifest_hash,
                size_bytes,
                content_key_version,
                path_sealed,
            )
            .await
            .map(|_| ())
            .map_err(map_err)
    }

    /// The sync client a restore records through, and a choose-winner resolves
    /// through: signing when this seam has an identity and the set's nonce
    /// resolves, else plain (unsigned, the helper logging why).
    async fn signing_client(&self, folder: &str) -> SyncClient<R> {
        let client = SyncClient::new(self.nest.clone());
        let (Some(signer), Some(custody)) = (&self.signer, &self.custody) else {
            return client;
        };
        match fauna_client_folders::record_nonce(&self.folders, &**custody, folder).await {
            Ok(Some(nonce)) => client.with_record_signing(fauna_client_sync::RecordSigning {
                signer: Arc::clone(signer),
                set_nonce: fauna_client_sync::SetNonceSource::Fixed(nonce),
            }),
            Ok(None) => {
                tracing::warn!("restore recorded unsigned: custody holds no nonce for the set");
                client
            }
            Err(e) => {
                tracing::warn!("restore recorded unsigned: the set nonce did not resolve ({e})");
                client
            }
        }
    }
}

/// The refusal a choose-winner answers when the judged history holds no
/// version for the candidate.
const NOT_A_VERSION: &str = "that version is not in this file's verified history";

fauna_core::map_rpc_error! {
    /// Map a transport `R::Error` onto [`DevicesApiError`], keyed on the WS-RPC
    /// `RpcError.code` suffix (`fauna.{folders,sync}.{conflict,not_found,…}`).
    /// A transport fault (the request never reached a server rejection) is
    /// `Transient`. Mirrors `fauna_folders_machine::nest_api::ws_rpc::map_err`,
    /// plus the refusal only `fauna.sync.devices.delete` answers — a
    /// guardian-enrolled device (`devices.md` § Removing a Device) — which is
    /// `Conflict`: definitive,
    /// so the removal's staged fleet leg settles `Kept` at once rather than
    /// waiting out the reconcile's in-flight bound.
    fn map_err(e) -> DevicesApiError {
        "conflict" | "guardian_marked" => Conflict,
        "not_found" => NotFound,
        "invalid_request" | "malformed" | "bad_candidate" => BadRequest,
    }
}

// ── The followed-folder source's shared halves ──────────────────────────────
//
// `StoreFollowedFoldersSource` has one impl per target (the transport types and
// the `Send` bound differ), so what they share is written once here and each
// impl only wires it.

/// One followed row from its stored record and its verdict — the projection
/// both targets make. The owner label is precomputed here, once, so no app
/// re-derives the handle-or-short-id fallback (the `FolderSummary::owner_display`
/// precedent).
fn followed_summary(
    record: fauna_core::data::FollowedFolder,
    verdict: fauna_client_folders::public_follow::CachedAvailability,
) -> crate::snapshots::FollowedFolderSummary {
    let owner_display = fauna_core::format::account_display_label(
        verdict.owner_handle.as_deref(),
        &record.owner_actor_id,
    );
    crate::snapshots::FollowedFolderSummary {
        folder_id: record.folder_id,
        home_nest_url: record.home_nest_url,
        owner_actor_id: record.owner_actor_id,
        owner_handle: verdict.owner_handle,
        owner_display,
        display_name: verdict.display_name,
        available: verdict.available,
    }
}

/// The Media page's scope for one followed row — the same row, so the filter
/// option's owner half is the very string the Folders row shows
/// (`ui/media.md` § Followed public folders).
fn followed_scope(
    home_nest_actor_id: Option<String>,
    s: crate::snapshots::FollowedFolderSummary,
) -> fauna_core::followed_media::FollowedMediaScope {
    fauna_core::followed_media::FollowedMediaScope {
        folder_id: s.folder_id,
        home_nest_url: s.home_nest_url,
        // The byte-plane trust root for a follow homed elsewhere — from the
        // record, since the FFI-facing summary deliberately carries none.
        home_nest_actor_id,
        owner_actor_id: s.owner_actor_id,
        owner_display: s.owner_display,
        display_name: s.display_name,
        available: s.available,
    }
}

/// The rows a refresh answers when the follow records could not be read.
///
/// ⚠ **A failed read is a network problem, never an answer about the follows**,
/// and answering it with no rows made every followed folder vanish from the page
/// on a dropped connection — which reads as the follows having been taken away
/// (`ui/folders.md` § Following a public folder; the same rule
/// `availability_from_probe` keeps for one row's verdict). So the last rows
/// this source answered stand, verdicts included, until a read succeeds.
fn rows_when_unreadable(
    last: &std::sync::Mutex<Vec<crate::snapshots::FollowedFolderSummary>>,
) -> Vec<crate::snapshots::FollowedFolderSummary> {
    last.lock().expect("followed-rows cache poisoned").clone()
}

// ── Native (`Arc<NestClient>`) ──────────────────────────────────────────────
#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::*;
    use crate::nest_api::WizardFactory;
    use fauna_client::NestClient;
    use fauna_folders_machine::{DeviceOption, FolderWizardMachine, FolderWizardObserver};

    #[async_trait]
    impl DevicesNestApi for WsRpcDevicesNest<Arc<NestClient>> {
        async fn list_devices(&self) -> Result<Vec<SyncDevice>, DevicesApiError> {
            self.do_list_devices().await
        }
        async fn list_folders(&self) -> Result<Vec<WireFolderSummary>, DevicesApiError> {
            self.do_list_folders().await
        }
        async fn list_conflicts(&self) -> Result<Vec<SyncConflict>, DevicesApiError> {
            self.do_list_conflicts().await
        }
        async fn webdav_served(&self, rows: &[WireFolderSummary]) -> Vec<bool> {
            self.do_webdav_served(rows).await
        }
        async fn web_subdomain_enabled(&self) -> Option<bool> {
            self.do_web_subdomain_enabled().await
        }
        async fn remove_device(&self, device_id: &str) -> Result<(), DevicesApiError> {
            self.do_remove_device(device_id).await
        }
        async fn request_p2p_off(&self, device_id: &str) -> Result<(), DevicesApiError> {
            self.do_request_p2p_off(device_id).await
        }
        async fn delete_folder(&self, name: &str) -> Result<(), DevicesApiError> {
            self.do_delete_folder(name).await
        }
        async fn resolve_conflict(
            &self,
            id: i64,
            winner: Option<ChosenWinner>,
        ) -> Result<(), DevicesApiError> {
            self.do_resolve_conflict(id, winner).await
        }
        async fn judge_candidate(
            &self,
            folder: &str,
            path: &str,
            manifest_hash: &str,
        ) -> Result<CandidateVerdict, DevicesApiError> {
            self.do_judge_candidate(folder, path, manifest_hash).await
        }
        async fn set_folder_paths(
            &self,
            name: &str,
            include_paths: Option<Vec<String>>,
            exclude_paths: Option<Vec<String>>,
            include_sealed: Option<Vec<u8>>,
            exclude_sealed: Option<Vec<u8>>,
        ) -> Result<(), DevicesApiError> {
            self.do_set_folder_paths(
                name,
                include_paths,
                exclude_paths,
                include_sealed,
                exclude_sealed,
            )
            .await
        }
        async fn set_folder_conflict_policy(
            &self,
            name: &str,
            conflict_policy: &str,
        ) -> Result<(), DevicesApiError> {
            self.do_set_folder_conflict_policy(name, conflict_policy)
                .await
        }
        async fn set_folder_audience(
            &self,
            name: &str,
            audience: &str,
            attestor: Option<Arc<fauna_core::identity::ActorKeypair>>,
        ) -> Result<(), DevicesApiError> {
            self.do_set_folder_audience(name, audience, attestor).await
        }
        async fn set_folder_website_enabled(
            &self,
            name: &str,
            enabled: bool,
        ) -> Result<(), DevicesApiError> {
            self.do_set_folder_website_enabled(name, enabled).await
        }
        async fn set_folder_residency(
            &self,
            name: &str,
            residency: &str,
        ) -> Result<(), DevicesApiError> {
            self.do_set_folder_residency(name, residency).await
        }
        async fn set_folder_place(
            &self,
            name: &str,
            device_id: &str,
            originates: bool,
            accepts: bool,
            applies_deletes: bool,
        ) -> Result<(), DevicesApiError> {
            self.do_set_folder_place(name, device_id, originates, accepts, applies_deletes)
                .await
        }
        async fn set_folder_nest_place(
            &self,
            name: &str,
            snapshots: Option<bool>,
            quiet_secs: Option<i64>,
            retention: Option<String>,
            version_retention: Option<fauna_folders_machine::VersionRetentionWrite>,
        ) -> Result<(), DevicesApiError> {
            self.do_set_folder_nest_place(name, snapshots, quiet_secs, retention, version_retention)
                .await
        }
        async fn restore_file_version(
            &self,
            folder: &str,
            device_id: &str,
            path: &str,
            manifest_hash: String,
            size_bytes: i64,
            content_key_version: Option<u64>,
            path_sealed: Option<Vec<u8>>,
        ) -> Result<(), DevicesApiError> {
            self.do_restore_file_version(
                folder,
                device_id,
                path,
                manifest_hash,
                size_bytes,
                content_key_version,
                path_sealed,
            )
            .await
        }
    }

    /// Builds the embedded wizard over the same native WS-RPC connection.
    pub struct WsRpcWizardFactory {
        nest: Arc<NestClient>,
        /// The account's folder-key custody for the wizard's create helper.
        custody: Option<Arc<dyn fauna_client_folders::FolderKeyStore>>,
    }

    impl WizardFactory for WsRpcWizardFactory {
        fn build_wizard(
            &self,
            observer: Arc<dyn FolderWizardObserver>,
            available_devices: Vec<DeviceOption>,
        ) -> Arc<FolderWizardMachine> {
            fauna_folders_machine::build_folder_wizard_machine(
                Arc::clone(&self.nest),
                self.custody.clone(),
                observer,
                available_devices,
            )
        }
    }

    /// Build a [`DevicesMachine`] over `nest`'s authenticated WS-RPC connection,
    /// the folder create and delete helpers writing into `custody` — the
    /// seat's `PlaneFolderKeys` (`None` → the machine lists and edits but
    /// refuses both). The native entry the linux app + `fauna-ffi` call.
    pub fn build_devices_machine(
        nest: Arc<NestClient>,
        custody: Option<Arc<dyn fauna_client_folders::FolderKeyStore>>,
        observer: Arc<dyn DevicesObserver>,
    ) -> Arc<DevicesMachine> {
        // The connection's own identity signs a restore's change record.
        let identity = nest
            .auth()
            .keypair()
            .map(|kp| ActorKeypair::from_secret(*kp.secret_bytes()));
        let api: Arc<dyn DevicesNestApi> = Arc::new(WsRpcDevicesNest::new(
            Arc::clone(&nest),
            identity,
            custody.clone(),
        ));
        let factory: Arc<dyn WizardFactory> = Arc::new(WsRpcWizardFactory { nest, custody });
        DevicesMachine::new(observer, api, factory)
    }

    /// Reads the user's followed public folders out of the account store
    /// (`fauna.state.follows`, through the [`fauna_client_config::FollowsStore`]
    /// seam) and probes each one's availability — the concrete
    /// [`crate::machine::FollowedFoldersSource`] every native app wires
    /// (`docs/goal/behavior/folders.md` § Publicly-synced follow).
    ///
    /// It holds both halves the machine deliberately does not: the account's
    /// follows (where the whole of a follow lives — the home nest keeps none)
    /// and the connection the public fetch rides.
    pub struct StoreFollowedFoldersSource {
        /// The account's follows — the host's per-call resolving store.
        store: Arc<dyn fauna_client_config::FollowsStore>,
        /// The connection the availability probe and the browse fetch ride.
        nest: Arc<NestClient>,
        /// Remembered availability verdicts, keyed by `folder_id`.
        ///
        /// Lives here because the machine holds this source for its own lifetime
        /// (`set_followed_folders_source` is called once), so the budget spans
        /// refreshes — which is the whole point: availability changes about once
        /// ever per follow, while the devices page refreshes on every nav edge.
        /// Locked only around the read and the write-back, never across an await.
        seen: std::sync::Mutex<
            std::collections::HashMap<i64, fauna_client_folders::public_follow::CachedAvailability>,
        >,
        /// The rows the last successful read answered — what a refresh shows
        /// while the records cannot be read ([`super::rows_when_unreadable`]).
        last: std::sync::Mutex<Vec<crate::snapshots::FollowedFolderSummary>>,
    }

    impl StoreFollowedFoldersSource {
        /// `nest` backs the public fetch relay; `store` holds the follows.
        pub fn new(
            nest: Arc<NestClient>,
            store: Arc<dyn fauna_client_config::FollowsStore>,
        ) -> Self {
            Self {
                store,
                nest,
                seen: std::sync::Mutex::new(std::collections::HashMap::new()),
                last: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    impl StoreFollowedFoldersSource {
        /// The rows both faces project — each followed row beside the
        /// `home_nest_actor_id` its record carries (the Media scope's
        /// byte-plane trust root, which the FFI-facing summary deliberately
        /// does not carry). On an unreadable config the last rows stand with
        /// no identity: a download in that window keeps the WebPKI floor, so a
        /// self-signed home fails closed rather than being dialed unverified.
        async fn followed_rows(
            &self,
        ) -> Vec<(Option<String>, crate::snapshots::FollowedFolderSummary)> {
            // Best-effort, like the foreign-set source beside it: an unreadable
            // store is never a page error — and never "no follows" either, so
            // the last rows stand (`rows_when_unreadable` owns why).
            let Ok(follows) = self.store.follows().await else {
                return super::rows_when_unreadable(&self.last)
                    .into_iter()
                    .map(|s| (None, s))
                    .collect();
            };
            let records = follows.followed;

            // Availability is resolved by the shared, tested policy rather than
            // here: probe only what the staleness budget says is stale, at most
            // `AVAILABILITY_PROBE_CONCURRENCY` at a time. The trade it encodes —
            // serial was throttle-safe but unusably slow, `join_all` is fast and
            // fans N concurrent relays out of the follower's OWN nest into the
            // per-source-IP and per-nest throttles the public plane rides by
            // design — is written up on those constants
            // (`fauna_client_folders::public_follow`). This glue is built over a
            // concrete client and cannot be unit-tested, so the decision it makes
            // is deliberately not its own.
            let now_ms = fauna_core::data::Timestamp::now_millis();
            let cached: Vec<Option<fauna_client_folders::public_follow::CachedAvailability>> = {
                let seen = self
                    .seen
                    .lock()
                    .expect("followed-availability cache poisoned");
                records
                    .iter()
                    .map(|r| seen.get(&r.folder_id).cloned())
                    .collect()
            };

            let verdicts = fauna_client_folders::public_follow::resolve_availability(
                &*self.nest,
                &records,
                &cached,
                now_ms,
                fauna_client_folders::public_follow::AVAILABILITY_TTL_MS,
            )
            .await;

            {
                let mut seen = self
                    .seen
                    .lock()
                    .expect("followed-availability cache poisoned");
                for (record, verdict) in records.iter().zip(&verdicts) {
                    seen.insert(record.folder_id, verdict.clone());
                }
                // Drop entries for follows the user has since removed, so the map
                // tracks the follows rather than growing without bound.
                seen.retain(|id, _| records.iter().any(|r| r.folder_id == *id));
            }

            let rows: Vec<(Option<String>, crate::snapshots::FollowedFolderSummary)> = records
                .into_iter()
                .zip(verdicts)
                .map(|(record, verdict)| {
                    let home_nest_actor_id = record.home_nest_actor_id.clone();
                    (home_nest_actor_id, super::followed_summary(record, verdict))
                })
                .collect();
            *self.last.lock().expect("followed-rows cache poisoned") =
                rows.iter().map(|(_, s)| s.clone()).collect();
            rows
        }
    }

    #[async_trait::async_trait]
    impl crate::machine::FollowedFoldersSource for StoreFollowedFoldersSource {
        async fn followed_folders(&self) -> Vec<crate::snapshots::FollowedFolderSummary> {
            self.followed_rows()
                .await
                .into_iter()
                .map(|(_, s)| s)
                .collect()
        }
    }

    /// The Media machine's followed browse-scope seam
    /// (`fauna_core::followed_media`; `media.md` § Followed public folders),
    /// on the SAME object that serves the Folders page — deliberately, so a
    /// Media browse fetch and the Folders-page probe share one availability
    /// cache and can never race contradicting verdicts. The platform builder
    /// injects one instance into both machines.
    #[async_trait::async_trait]
    impl fauna_core::followed_media::FollowedMediaSource for StoreFollowedFoldersSource {
        async fn followed_scopes(&self) -> Vec<fauna_core::followed_media::FollowedMediaScope> {
            // The Folders-page projection IS the scope list — one probe path;
            // the scope additionally carries the record's home identity.
            self.followed_rows()
                .await
                .into_iter()
                .map(|(home_nest_actor_id, s)| super::followed_scope(home_nest_actor_id, s))
                .collect()
        }

        async fn fetch_listing(
            &self,
            folder_id: i64,
            home_nest_url: &str,
        ) -> Result<
            Vec<fauna_core::followed_media::FollowedFileEntry>,
            fauna_core::followed_media::FollowedFetchError,
        > {
            use fauna_client_folders::public_follow::{
                self, FollowError, availability_from_probe, browse_verdict,
            };
            use fauna_core::followed_media::FollowedFetchError;

            let follows =
                self.store.follows().await.map_err(|e| {
                    FollowedFetchError::Transport(format!("follows read failed: {e}"))
                })?;
            let record = follows
                .followed
                .into_iter()
                .find(|r| r.folder_id == folder_id && r.home_nest_url == home_nest_url)
                .ok_or_else(|| {
                    FollowedFetchError::Transport("no such follow in this account".to_string())
                })?;

            let result = public_follow::fetch_followed_changes(&*self.nest, &record, 0).await;
            let listing = result
                .as_ref()
                .ok()
                .map(|page| public_follow::listing_from_changes(&page.changes));
            let unavailable = matches!(&result, Err(FollowError::Unavailable));
            let transport_detail = match &result {
                Err(FollowError::Rpc(e)) => Some(e.to_string()),
                _ => None,
            };

            // The browse fetch IS availability evidence — classified by the one
            // shared rule and written into the same cache the probe path
            // maintains, so the Folders page's verdict agrees on its next read.
            let now_ms = fauna_core::data::Timestamp::now_millis();
            let fetched = availability_from_probe(result, &record);
            {
                let mut seen = self
                    .seen
                    .lock()
                    .expect("followed-availability cache poisoned");
                let verdict = browse_verdict(&record, seen.get(&folder_id), fetched, now_ms);
                seen.insert(folder_id, verdict);
            }

            match listing {
                Some(rows) => Ok(rows),
                None if unavailable => Err(FollowedFetchError::Unavailable),
                None => Err(FollowedFetchError::Transport(
                    transport_detail.unwrap_or_else(|| "public fetch failed".to_string()),
                )),
            }
        }
    }
}
#[cfg(not(target_arch = "wasm32"))]
pub use native::{StoreFollowedFoldersSource, build_devices_machine};

// ── Wasm (`WsRpcClient`) ────────────────────────────────────────────────────
#[cfg(target_arch = "wasm32")]
mod wasm {
    use super::*;
    use crate::nest_api::WizardFactory;
    use fauna_folders_machine::{DeviceOption, FolderWizardMachine, FolderWizardObserver};
    use fauna_rpc_wasm::WsRpcClient;

    #[async_trait(?Send)]
    impl DevicesNestApi for WsRpcDevicesNest<WsRpcClient> {
        async fn list_devices(&self) -> Result<Vec<SyncDevice>, DevicesApiError> {
            self.do_list_devices().await
        }
        async fn list_folders(&self) -> Result<Vec<WireFolderSummary>, DevicesApiError> {
            self.do_list_folders().await
        }
        async fn list_conflicts(&self) -> Result<Vec<SyncConflict>, DevicesApiError> {
            self.do_list_conflicts().await
        }
        async fn webdav_served(&self, rows: &[WireFolderSummary]) -> Vec<bool> {
            self.do_webdav_served(rows).await
        }
        async fn web_subdomain_enabled(&self) -> Option<bool> {
            self.do_web_subdomain_enabled().await
        }
        async fn remove_device(&self, device_id: &str) -> Result<(), DevicesApiError> {
            self.do_remove_device(device_id).await
        }
        async fn request_p2p_off(&self, device_id: &str) -> Result<(), DevicesApiError> {
            self.do_request_p2p_off(device_id).await
        }
        async fn delete_folder(&self, name: &str) -> Result<(), DevicesApiError> {
            self.do_delete_folder(name).await
        }
        async fn resolve_conflict(
            &self,
            id: i64,
            winner: Option<ChosenWinner>,
        ) -> Result<(), DevicesApiError> {
            self.do_resolve_conflict(id, winner).await
        }
        async fn judge_candidate(
            &self,
            folder: &str,
            path: &str,
            manifest_hash: &str,
        ) -> Result<CandidateVerdict, DevicesApiError> {
            self.do_judge_candidate(folder, path, manifest_hash).await
        }
        async fn set_folder_paths(
            &self,
            name: &str,
            include_paths: Option<Vec<String>>,
            exclude_paths: Option<Vec<String>>,
            include_sealed: Option<Vec<u8>>,
            exclude_sealed: Option<Vec<u8>>,
        ) -> Result<(), DevicesApiError> {
            self.do_set_folder_paths(
                name,
                include_paths,
                exclude_paths,
                include_sealed,
                exclude_sealed,
            )
            .await
        }
        async fn set_folder_conflict_policy(
            &self,
            name: &str,
            conflict_policy: &str,
        ) -> Result<(), DevicesApiError> {
            self.do_set_folder_conflict_policy(name, conflict_policy)
                .await
        }
        async fn set_folder_audience(
            &self,
            name: &str,
            audience: &str,
            attestor: Option<Arc<fauna_core::identity::ActorKeypair>>,
        ) -> Result<(), DevicesApiError> {
            self.do_set_folder_audience(name, audience, attestor).await
        }
        async fn set_folder_website_enabled(
            &self,
            name: &str,
            enabled: bool,
        ) -> Result<(), DevicesApiError> {
            self.do_set_folder_website_enabled(name, enabled).await
        }
        async fn set_folder_residency(
            &self,
            name: &str,
            residency: &str,
        ) -> Result<(), DevicesApiError> {
            self.do_set_folder_residency(name, residency).await
        }
        async fn set_folder_place(
            &self,
            name: &str,
            device_id: &str,
            originates: bool,
            accepts: bool,
            applies_deletes: bool,
        ) -> Result<(), DevicesApiError> {
            self.do_set_folder_place(name, device_id, originates, accepts, applies_deletes)
                .await
        }
        async fn set_folder_nest_place(
            &self,
            name: &str,
            snapshots: Option<bool>,
            quiet_secs: Option<i64>,
            retention: Option<String>,
            version_retention: Option<fauna_folders_machine::VersionRetentionWrite>,
        ) -> Result<(), DevicesApiError> {
            self.do_set_folder_nest_place(name, snapshots, quiet_secs, retention, version_retention)
                .await
        }
        async fn restore_file_version(
            &self,
            folder: &str,
            device_id: &str,
            path: &str,
            manifest_hash: String,
            size_bytes: i64,
            content_key_version: Option<u64>,
            path_sealed: Option<Vec<u8>>,
        ) -> Result<(), DevicesApiError> {
            self.do_restore_file_version(
                folder,
                device_id,
                path,
                manifest_hash,
                size_bytes,
                content_key_version,
                path_sealed,
            )
            .await
        }
    }

    /// Builds the embedded wizard over the SPA's browser `WsRpcClient`.
    pub struct WsRpcWizardFactory {
        nest: WsRpcClient,
        /// The account's folder-key custody for the wizard's create helper.
        custody: Option<Arc<dyn fauna_client_folders::FolderKeyStore>>,
        /// The owner's seal root, derived from the handed-in identity — the
        /// browser connection carries no keypair to read it off.
        owner_seal: Option<fauna_core::crypto::BackupKey>,
    }

    impl WizardFactory for WsRpcWizardFactory {
        fn build_wizard(
            &self,
            observer: Arc<dyn FolderWizardObserver>,
            available_devices: Vec<DeviceOption>,
        ) -> Arc<FolderWizardMachine> {
            fauna_folders_machine::build_folder_wizard_machine(
                self.nest.clone(),
                self.custody.clone(),
                self.owner_seal.clone(),
                observer,
                available_devices,
            )
        }
    }

    /// Build a [`DevicesMachine`] over the SPA's browser `WsRpcClient`. The
    /// browser connection carries no keypair, so the caller hands the owner's
    /// `identity` in (it signs a restore's change record), and the account's
    /// folder-key `custody` — the account port's forwarder — the folder create
    /// and delete helpers write into (`None` → the machine lists and edits but
    /// refuses both).
    pub fn build_devices_machine(
        nest: WsRpcClient,
        identity: Option<ActorKeypair>,
        custody: Option<Arc<dyn fauna_client_folders::FolderKeyStore>>,
        observer: Arc<dyn DevicesObserver>,
    ) -> Arc<DevicesMachine> {
        let owner_seal = identity
            .as_ref()
            .map(|kp| fauna_core::crypto::BackupKey::derive(kp.secret_bytes()));
        let api: Arc<dyn DevicesNestApi> = Arc::new(WsRpcDevicesNest::new(
            nest.clone(),
            identity,
            custody.clone(),
        ));
        let factory: Arc<dyn WizardFactory> = Arc::new(WsRpcWizardFactory {
            nest,
            custody,
            owner_seal,
        });
        DevicesMachine::new(observer, api, factory)
    }

    /// Reads the user's followed public folders out of the account store and
    /// probes each one's availability — the browser twin of
    /// `native::StoreFollowedFoldersSource`, so web renders followed rows
    /// identically to the native apps (priority #2;
    /// `docs/goal/behavior/folders.md` § Publicly-synced follow).
    ///
    /// It holds both halves the machine deliberately does not: the account's
    /// follows (where the whole of a follow lives — the home nest keeps none;
    /// on web the store is the core chunk's, reached over the account port —
    /// `fauna_client_config::follows_port`) and the connection the public
    /// fetch rides.
    pub struct StoreFollowedFoldersSource {
        /// The account's follows — the account port's forwarder on web.
        store: Arc<dyn fauna_client_config::FollowsStore>,
        /// The connection the availability probe and the browse fetch ride.
        nest: WsRpcClient,
        /// Remembered availability verdicts, keyed by `folder_id`.
        ///
        /// Lives here because the machine holds this source for its own lifetime
        /// (`set_followed_folders_source` is called once), so the budget spans
        /// refreshes — which is the whole point: availability changes about once
        /// ever per follow, while the devices page refreshes on every nav edge.
        /// Locked only around the read and the write-back, never across an await.
        seen: std::sync::Mutex<
            std::collections::HashMap<i64, fauna_client_folders::public_follow::CachedAvailability>,
        >,
        /// The rows the last successful read answered — what a refresh shows
        /// while the records cannot be read ([`super::rows_when_unreadable`]).
        last: std::sync::Mutex<Vec<crate::snapshots::FollowedFolderSummary>>,
    }

    impl StoreFollowedFoldersSource {
        /// `nest` backs the public fetch relay; `store` holds the follows.
        pub fn new(nest: WsRpcClient, store: Arc<dyn fauna_client_config::FollowsStore>) -> Self {
            Self {
                store,
                nest,
                seen: std::sync::Mutex::new(std::collections::HashMap::new()),
                last: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    impl StoreFollowedFoldersSource {
        /// The rows both faces project — each followed row beside the
        /// `home_nest_actor_id` its record carries (the Media scope's
        /// byte-plane trust root, which the FFI-facing summary deliberately
        /// does not carry). On an unreadable config the last rows stand with
        /// no identity: a download in that window keeps the WebPKI floor, so a
        /// self-signed home fails closed rather than being dialed unverified.
        async fn followed_rows(
            &self,
        ) -> Vec<(Option<String>, crate::snapshots::FollowedFolderSummary)> {
            // Best-effort, like the foreign-set source beside it: an unreadable
            // store is never a page error — and never "no follows" either, so
            // the last rows stand (`rows_when_unreadable` owns why).
            let Ok(follows) = self.store.follows().await else {
                return super::rows_when_unreadable(&self.last)
                    .into_iter()
                    .map(|s| (None, s))
                    .collect();
            };
            let records = follows.followed;

            // Availability is resolved by the shared, tested policy rather than
            // here: probe only what the staleness budget says is stale, at most
            // `AVAILABILITY_PROBE_CONCURRENCY` at a time. The trade it encodes —
            // serial was throttle-safe but unusably slow, `join_all` is fast and
            // fans N concurrent relays out of the follower's OWN nest into the
            // per-source-IP and per-nest throttles the public plane rides by
            // design — is written up on those constants
            // (`fauna_client_folders::public_follow`). This glue is built over a
            // concrete client and cannot be unit-tested, so the decision it makes
            // is deliberately not its own.
            let now_ms = fauna_core::data::Timestamp::now_millis();
            let cached: Vec<Option<fauna_client_folders::public_follow::CachedAvailability>> = {
                let seen = self
                    .seen
                    .lock()
                    .expect("followed-availability cache poisoned");
                records
                    .iter()
                    .map(|r| seen.get(&r.folder_id).cloned())
                    .collect()
            };

            let verdicts = fauna_client_folders::public_follow::resolve_availability(
                &self.nest,
                &records,
                &cached,
                now_ms,
                fauna_client_folders::public_follow::AVAILABILITY_TTL_MS,
            )
            .await;

            {
                let mut seen = self
                    .seen
                    .lock()
                    .expect("followed-availability cache poisoned");
                for (record, verdict) in records.iter().zip(&verdicts) {
                    seen.insert(record.folder_id, verdict.clone());
                }
                // Drop entries for follows the user has since removed, so the map
                // tracks the follows rather than growing without bound.
                seen.retain(|id, _| records.iter().any(|r| r.folder_id == *id));
            }

            let rows: Vec<(Option<String>, crate::snapshots::FollowedFolderSummary)> = records
                .into_iter()
                .zip(verdicts)
                .map(|(record, verdict)| {
                    let home_nest_actor_id = record.home_nest_actor_id.clone();
                    (home_nest_actor_id, super::followed_summary(record, verdict))
                })
                .collect();
            *self.last.lock().expect("followed-rows cache poisoned") =
                rows.iter().map(|(_, s)| s.clone()).collect();
            rows
        }
    }

    #[async_trait::async_trait(?Send)]
    impl crate::machine::FollowedFoldersSource for StoreFollowedFoldersSource {
        async fn followed_folders(&self) -> Vec<crate::snapshots::FollowedFolderSummary> {
            self.followed_rows()
                .await
                .into_iter()
                .map(|(_, s)| s)
                .collect()
        }
    }

    /// The Media machine's followed browse-scope seam — the wasm twin of the
    /// native impl above (one instance serves both machines; the doc there
    /// owns the rationale).
    #[async_trait::async_trait(?Send)]
    impl fauna_core::followed_media::FollowedMediaSource for StoreFollowedFoldersSource {
        async fn followed_scopes(&self) -> Vec<fauna_core::followed_media::FollowedMediaScope> {
            // The Folders-page projection IS the scope list — one probe path;
            // the scope additionally carries the record's home identity.
            self.followed_rows()
                .await
                .into_iter()
                .map(|(home_nest_actor_id, s)| super::followed_scope(home_nest_actor_id, s))
                .collect()
        }

        async fn fetch_listing(
            &self,
            folder_id: i64,
            home_nest_url: &str,
        ) -> Result<
            Vec<fauna_core::followed_media::FollowedFileEntry>,
            fauna_core::followed_media::FollowedFetchError,
        > {
            use fauna_client_folders::public_follow::{
                self, FollowError, availability_from_probe, browse_verdict,
            };
            use fauna_core::followed_media::FollowedFetchError;

            let follows =
                self.store.follows().await.map_err(|e| {
                    FollowedFetchError::Transport(format!("follows read failed: {e}"))
                })?;
            let record = follows
                .followed
                .into_iter()
                .find(|r| r.folder_id == folder_id && r.home_nest_url == home_nest_url)
                .ok_or_else(|| {
                    FollowedFetchError::Transport("no such follow in this account".to_string())
                })?;

            let result = public_follow::fetch_followed_changes(&self.nest, &record, 0).await;
            let listing = result
                .as_ref()
                .ok()
                .map(|page| public_follow::listing_from_changes(&page.changes));
            let unavailable = matches!(&result, Err(FollowError::Unavailable));
            let transport_detail = match &result {
                Err(FollowError::Rpc(e)) => Some(e.to_string()),
                _ => None,
            };

            // The browse fetch IS availability evidence — classified by the one
            // shared rule and written into the same cache the probe path
            // maintains, so the Folders page's verdict agrees on its next read.
            let now_ms = fauna_core::data::Timestamp::now_millis();
            let fetched = availability_from_probe(result, &record);
            {
                let mut seen = self
                    .seen
                    .lock()
                    .expect("followed-availability cache poisoned");
                let verdict = browse_verdict(&record, seen.get(&folder_id), fetched, now_ms);
                seen.insert(folder_id, verdict);
            }

            match listing {
                Some(rows) => Ok(rows),
                None if unavailable => Err(FollowedFetchError::Unavailable),
                None => Err(FollowedFetchError::Transport(
                    transport_detail.unwrap_or_else(|| "public fetch failed".to_string()),
                )),
            }
        }
    }
}
#[cfg(target_arch = "wasm32")]
pub use wasm::{StoreFollowedFoldersSource, build_devices_machine};

/// The shared [`crate::machine::ForeignSetsSource`] — the member's foreign-set
/// records (`fauna_core::data::ForeignFolder`, written at share-accept) read out
/// of the account's folder-key custody (`fauna.state.folder-keys`), so every app
/// — web included, through the account port's forwarder — lists cross-nest
/// shared sets identically (priority #2; Phase 2 client read-side). Wired via
/// [`crate::machine::DevicesMachine::set_foreign_sets_source`], the
/// `set_mls_query` pattern.
pub struct CustodyForeignSetsSource {
    custody: Arc<dyn fauna_client_folders::FolderKeyReader>,
}

impl CustodyForeignSetsSource {
    pub fn new(custody: Arc<dyn fauna_client_folders::FolderKeyReader>) -> Self {
        Self { custody }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl crate::machine::ForeignSetsSource for CustodyForeignSetsSource {
    async fn foreign_sets(&self) -> Vec<crate::machine::ForeignSetRow> {
        // Best-effort: an unreadable custody yields no foreign rows this
        // refresh (the machine's contract) — never a page error.
        let Ok(custody) = self.custody.load().await else {
            return Vec::new();
        };
        // A left record is a tombstone, never a membership.
        fauna_client_folders::custody::live_foreign_sets(&custody)
            .map(|f| crate::machine::ForeignSetRow {
                set_name: f.set_name.clone(),
                mls_group_id_hex: hex::encode(&f.mls_group_id),
                home_nest_url: f.home_nest_url.clone(),
                access: f.access.clone(),
                metadata_only_residency: f.metadata_only_residency(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::CapturingRequester;

    fn seam(req: Arc<CapturingRequester>) -> WsRpcDevicesNest<Arc<CapturingRequester>> {
        WsRpcDevicesNest::new(
            req,
            Some(ActorKeypair::from_secret([0x0F; 32])),
            Some(Arc::new(
                fauna_client_folders::MemoryFolderKeyStore::default(),
            )),
        )
    }

    #[tokio::test]
    async fn gestures_target_the_right_kinds() {
        let req = Arc::new(CapturingRequester::transport_fault());
        let s = seam(Arc::clone(&req));
        let _ = s.do_remove_device("aa").await;
        let _ = s.do_delete_folder("docs").await;
        // A mark-only resolve (no winning hash): this gesture's plain path,
        // covered here for its kind. The signed choose-winner path — a
        // conflicts.list lookup before conflicts.resolve — is a different call
        // count and is pinned by `the_engines_signed_conflict_requests_land_
        // verified_winner_rows` instead of duplicated here.
        let _ = s.do_resolve_conflict(7, None).await;
        let _ = s
            .do_set_folder_paths("docs", Some(vec!["/a".into()]), None, None, None)
            .await;
        let _ = s.do_list_devices().await;
        let _ = s.do_list_conflicts().await;
        let _ = s.do_list_folders().await;

        let calls = req.calls();
        assert_eq!(
            req.kinds(),
            vec![
                "fauna.sync.devices.delete",
                "fauna.folders.delete",
                "fauna.sync.conflicts.resolve",
                "fauna.folders.update",
                "fauna.sync.devices.list",
                "fauna.sync.conflicts.list",
                "fauna.folders.list",
            ]
        );
        // The selective-sync update sends only the path fields.
        let (_, update_payload) = calls
            .iter()
            .find(|(k, _)| *k == "fauna.folders.update")
            .unwrap();
        // Addressed by hash alone: the funnel takes the plaintext name off.
        assert!(update_payload.get("name").is_none());
        assert!(update_payload.get("name_hash").is_some());
        assert!(update_payload.get("retention_policy").unwrap().is_null());
        assert_eq!(
            update_payload.get("include_paths").unwrap(),
            &serde_json::json!(["/a"])
        );
    }

    fn seam_returning(code: &str, detail: &str) -> WsRpcDevicesNest<Arc<CapturingRequester>> {
        seam(Arc::new(CapturingRequester::rejecting_code(code, detail)))
    }

    #[tokio::test]
    async fn rejection_codes_map_to_variants() {
        let conflict = seam_returning("fauna.folders.conflict", "in use")
            .do_delete_folder("x")
            .await;
        assert!(
            matches!(conflict, Err(DevicesApiError::Conflict { detail }) if detail == "in use")
        );

        let nf = seam_returning("fauna.sync.not_found", "no device")
            .do_remove_device("z")
            .await;
        assert!(matches!(nf, Err(DevicesApiError::NotFound { detail }) if detail == "no device"));

        let bad = seam_returning("fauna.sync.bad_candidate", "not a candidate")
            .do_resolve_conflict(
                1,
                Some(ChosenWinner {
                    manifest_hash: "h".into(),
                    path: "a.txt".into(),
                }),
            )
            .await;
        assert!(matches!(bad, Err(DevicesApiError::BadRequest { .. })));
    }

    /// The device deletion's definitive refusals are `Conflict`, so the
    /// page settles the staged fleet leg `Kept` rather than `Unknown` — the
    /// nest's own codes, as `sync_handlers`' `devices.delete` spells them.
    #[tokio::test]
    async fn a_kept_device_deletion_maps_to_conflict() {
        for (code, detail) in [(
            "fauna.sync.guardian_marked",
            "this device was enrolled by your guardian and cannot be removed",
        )] {
            let kept = seam_returning(code, detail).do_remove_device("z").await;
            assert!(
                matches!(&kept, Err(DevicesApiError::Conflict { detail: d }) if d == detail),
                "{code}: {kept:?}"
            );
        }
    }

    /// Ruling (7)(b)(ii) rule (2): the snapshot's served state is the owner's
    /// custody's word. A set the nest flags served that custody never served
    /// reads OFF, and one custody serves reads ON whatever the nest's flag;
    /// a seam with no custody reads every row not served.
    #[tokio::test]
    async fn the_served_state_is_custodys_word_never_the_nest_flag() {
        use fauna_client_folders::FolderKeyStore;
        use fauna_client_folders::custody::{record_new_set, serve_on};
        use fauna_core::folder_keys::serve_custody_channel_id;

        let store = Arc::new(fauna_client_folders::MemoryFolderKeyStore::default());
        let mut cfg = fauna_core::data::FoldersConfig::default();
        for name in ["served", "flagged"] {
            record_new_set(&mut cfg, serve_custody_channel_id(name), [0x42; 32], 1_000);
        }
        assert!(serve_on(
            &mut cfg,
            &serve_custody_channel_id("served"),
            2_000
        ));
        store.merge(cfg).await.unwrap();

        let row = |name: &str, flag: bool| WireFolderSummary {
            name: name.into(),
            webdav_enabled: flag,
            ..Default::default()
        };
        let rows = [row("served", false), row("flagged", true)];
        let s = WsRpcDevicesNest::new(
            Arc::new(CapturingRequester::transport_fault()),
            None,
            Some(store as Arc<dyn fauna_client_folders::FolderKeyStore>),
        );
        assert_eq!(s.do_webdav_served(&rows).await, vec![true, false]);

        let keyless =
            WsRpcDevicesNest::new(Arc::new(CapturingRequester::transport_fault()), None, None);
        assert_eq!(keyless.do_webdav_served(&rows).await, vec![false, false]);
    }

    #[tokio::test]
    async fn transport_fault_maps_to_transient() {
        let r = seam(Arc::new(CapturingRequester::transport_fault()))
            .do_list_devices()
            .await;
        assert!(matches!(r, Err(DevicesApiError::Transient { .. })));
    }
}
