//! WASM bindings for the Fauna Media page — the page-level `MediaMachine` (the
//! cross-set all-media browser). The web Media route renders off the shared
//! `MediaMachine` via `$lib/wasm-media`, exactly as the Devices route renders off
//! `DevicesMachine` via `$lib/wasm-folders` — one shared surface, all 7 apps
//! (priority #1/#2/#4; `docs/goal/ui/media.md` rule 2). Mirrors the page-level
//! wrapper half of `fauna-wasm-folders`.

use std::sync::Arc;

use wasm_bindgen::prelude::*;

use fauna_media_machine::{MediaMachine as InnerMedia, MediaObserver as InnerMediaObserver};
use fauna_wasm_panic_hook::err_to_js;

/// The `sync-state-badge` label for a `media-item` row. Web is a
/// **control-plane** client (`fauna.sync.files` carries no per-file status and
/// there is no local sync engine behind this page), so it renders only the
/// `Synced` state — exactly what `file-sync.md` § Per-file sync-status display
/// sanctions for that class (linux's `media` page + tui render the identical
/// leg). The label comes from the shared `sync_display_state_label`, never a
/// hand-written string, so it can't drift from the desktop-engine clients'
/// text. Only the badge's icon/color is a per-app render.
#[wasm_bindgen(js_name = syncedStateBadgeLabel)]
pub fn synced_state_badge_label() -> Result<JsValue, JsValue> {
    serde_wasm_bindgen::to_value(&fauna_core::format::sync_display_state_label(
        fauna_core::format::SyncDisplayState::Synced,
    ))
    .map_err(err_to_js)
}

/// The `share-link-expiry-select` option label for a raw value (`"1d"` /
/// `"7d"` / `"30d"` / `"1y"`) — the shared
/// `fauna_core::format::share_link_expiry_label` map; nothing for an unknown
/// value (`undefined` across the boundary), which the page paints raw.
#[wasm_bindgen(js_name = shareLinkExpiryLabel)]
pub fn share_link_expiry_label(value: String) -> Result<JsValue, JsValue> {
    serde_wasm_bindgen::to_value(&fauna_core::format::share_link_expiry_label(&value))
        .map_err(err_to_js)
}

/// The `share-link-item-state` label for a row's stable state (`"active"` /
/// `"expired"` / `"revoked"`) — the shared
/// `fauna_core::format::share_link_state_label` map; nothing for an unknown
/// state (`undefined` across the boundary), painted raw.
#[wasm_bindgen(js_name = shareLinkStateLabel)]
pub fn share_link_state_label(state: String) -> Result<JsValue, JsValue> {
    serde_wasm_bindgen::to_value(&fauna_core::format::share_link_state_label(&state))
        .map_err(err_to_js)
}

#[wasm_bindgen]
extern "C" {
    pub type JsMediaObserver;
    #[wasm_bindgen(method, js_name = onChanged)]
    fn on_changed(this: &JsMediaObserver);
}

struct MediaObserverShim(JsMediaObserver);
// SAFETY: wasm32 is single-threaded; the JS object never crosses a thread.
unsafe impl Send for MediaObserverShim {}
unsafe impl Sync for MediaObserverShim {}
impl InnerMediaObserver for MediaObserverShim {
    fn on_changed(&self) {
        self.0.on_changed()
    }
}

/// The page machine plus the connection it was built over.
///
/// The second field mirrors `fauna-wasm-folders`'s `DevicesMachine`: a
/// post-construction seam (here `setFollowedMediaSource`) has to build its
/// source over the SAME connection the machine rides, and `build_media_machine`
/// consumes the client. `WsRpcClient` is `Clone`, so the wrapper keeps a clone
/// rather than an `Arc`.
#[wasm_bindgen]
pub struct MediaMachine(Arc<InnerMedia>, fauna_rpc_wasm::WsRpcClient);

#[wasm_bindgen]
impl MediaMachine {
    /// Build the page machine over the SPA core chunk's socket, lent as
    /// `port` (a `SharedRpcPort` — `$lib/rpc`'s `sharedRpcPort`; the owner's
    /// `requestRaw` runs every request this machine makes, so web keeps one
    /// WebSocket per actor; the blob legs ride the port's `bearer` + `nestUrl`).
    /// Throws on an object that is not a port. State starts empty (all-media
    /// view, name sort ascending, list mode); call `refresh()` to populate it.
    ///
    /// `secretHex` is the caller's own 32-byte actor secret, taken at
    /// construction — not by a later `set*` call like the other keyed seams —
    /// because the shared-folder content-key resolver is a construction-time
    /// field of the shared machine on every app (tui/linux/the FFI apps pass
    /// it to `build_media_machine_with_folder_keys`; there is no setter, on
    /// purpose: a machine without it silently reads every shared set as
    /// owner-only). The resolver is the same `NestFolderKeyResolver` those
    /// apps build, over this machine's own socket, so a shared set's bytes
    /// (`download`) and its sealed names (the listing) open under the content
    /// keys in the member's own `fauna.state.folder-keys` custody — the custody the
    /// conversations rail's `NestFolderCustodySink` ingests on web too. The
    /// keypair is derived here; the raw secret never leaves this call.
    #[wasm_bindgen(constructor)]
    pub fn new(
        observer: JsMediaObserver,
        port: fauna_rpc_wasm::JsRpcPort,
        secret_hex: String,
        account_port: fauna_account_port::JsAccountPort,
    ) -> Result<MediaMachine, JsValue> {
        let observer: Arc<dyn InnerMediaObserver> = Arc::new(MediaObserverShim(observer));
        let client = fauna_rpc_wasm::WsRpcClient::over_port(port.into())
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        // The secret signs the change records the machine writes.
        let identity = fauna_rpc_wasm::keypair_from_secret_hex(&secret_hex)?;
        // The shared-set resolver reads the account's folder-key custody
        // through the tab's account port (`fauna_client_folders::port`).
        let transport = fauna_account_port::JsAccountTransport::new(account_port.into())
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        let folder_keys: Arc<dyn fauna_media_machine::FolderKeyResolver> =
            Arc::new(fauna_client_folders::NestFolderKeyResolver::new(
                client.clone(),
                Arc::new(fauna_client_folders::port::PortFolderKeys::new(transport)),
            ));
        Ok(MediaMachine(
            fauna_media_machine::build_media_machine_with_folder_keys(
                client.clone(),
                Some(&identity),
                observer,
                Some(folder_keys),
            ),
            client,
        ))
    }

    /// Wire the **followed public folders** source — the browser twin of tui's
    /// `machine.set_followed_media_source(StoreFollowedFoldersSource::new(…))`
    /// (`apps/fauna-tui/src/media/mod.rs`), and the Media-page sibling of
    /// `fauna-wasm-folders`'s `setFollowedFoldersSource`.
    ///
    /// Without it `snapshotJson`'s `followed` is permanently empty, so
    /// `media-folder-filter` offers no followed scopes and `selectFollowedScope`
    /// has nothing to select — the correct render for a page that has not built
    /// the surface, not an error (`docs/goal/ui/media.md` § Followed public
    /// folders). Call once right after construction, before the first
    /// `refresh()`, mirroring `setOwnerBackupKey`.
    ///
    /// The SAME type backs the Devices page's followed rows, so the availability
    /// verdicts a browse fetch writes and the ones the staleness-budgeted probe
    /// writes are one mechanism per page rather than two racing ones — the
    /// shared source owns that cache. The whole of a follow lives in the
    /// account's `fauna.state.follows` rows (the home nest keeps none), held by
    /// the core chunk's account runtime and read across the account `port` (a
    /// `SharedAccountPort` minted for this account).
    #[wasm_bindgen(js_name = setFollowedMediaSource)]
    pub fn set_followed_media_source(
        &self,
        port: fauna_account_port::JsAccountPort,
    ) -> Result<(), JsValue> {
        let follows = fauna_client_config::follows_port::from_js_port(port)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        self.0.set_followed_media_source(Arc::new(
            fauna_devices_machine::StoreFollowedFoldersSource::new(self.1.clone(), follows),
        ));
        Ok(())
    }

    /// Wire the owner `BackupKey` the delete/restore gestures seal their
    /// records with (S8 D2 — `file-sync.md` § Sealed names & paths →
    /// Implementation status today). Call once right after construction,
    /// mirroring linux/tui/android (`apps/fauna-linux/src/views/media/mod.rs`).
    /// `secretHex` is the identity seed hex — the key is derived here,
    /// mirroring `uploadSelected`/`fetchThumbnail`, so the raw key never
    /// crosses into JS.
    ///
    /// Web hands this over inside `$lib/wasm-media::createMediaMachine`, so a
    /// keyless machine is unrepresentable rather than merely discouraged — the
    /// construction-seam half of `file-sync.md`'s CONSUMER-WIRING RULE.
    ///
    /// **Why it is not optional.** Unlike `upload`, which seals from the key
    /// passed to it per call (`MediaMachine::do_upload`), `delete` and
    /// `restore_version` take no key argument: they seal only from this injected
    /// one, via `MediaMachine::seal_gesture_path`. Without it that seam resolves
    /// no seal root, the record goes out with `path_sealed = None`, and the nest
    /// **refuses** it post-S9-flip with `fauna.sync.path_seal_required`
    /// (`bins/fauna-nest/src/sync_handlers.rs`) — the gesture fails outright
    /// rather than degrading. That asymmetry is why healthy uploads hide the
    /// break; the client half is pinned by
    /// `an_unkeyed_machines_restore_seals_nothing_so_the_post_flip_nest_refuses_it`
    /// (`libs/fauna-media-machine/tests/media_lifecycle.rs`).
    #[wasm_bindgen(js_name = setOwnerBackupKey)]
    pub fn set_owner_backup_key(&self, secret_hex: String) -> Result<(), JsValue> {
        let backup_key = derive_backup_key(&secret_hex).map_err(|e| JsValue::from_str(&e))?;
        self.0.set_owner_backup_key(backup_key);
        Ok(())
    }

    /// READ-side custody for a **successor**: the owner keys of the identities
    /// this account succeeded from, so a media corpus a succession re-pointed
    /// but did not re-seal still opens (`succession-aftermath.md` § Re-key scope
    /// — *media*, folders and backups).
    ///
    /// **Why this is a second injection rather than another key on
    /// [`Self::set_owner_backup_key`].** That one is the delete/restore *seal*
    /// root, and a retired key must never reach a seal — the shared machine
    /// keeps the two in separate fields precisely so that is unrepresentable
    /// rather than merely asserted (`FileDownloadKeys::predecessor_backup_keys`).
    /// tui orders it the same way at `apps/fauna-tui/src/media/mod.rs`.
    ///
    /// **Takes the successor's own seed, not the retired keys**, and walks the
    /// account registry here — the same shape `setOwnerBackupKey` and
    /// `setFollowedMediaSource` use, and for the reason `wasm-media.ts` states
    /// at the call site: the raw key never enters JS. The walk reaches **every**
    /// ancestor, not just the immediate one, because a corpus can still be
    /// sealed under a grandpredecessor when an intermediate re-seal never
    /// finished.
    ///
    /// **The same walk feeds the reader's half**: the attested predecessor ids
    /// (`AccountRegistry::attested_predecessor_actor_ids`) go to the listing's
    /// judge, so a row a retired identity signed is read as this account's own
    /// (`mls-group-key-material.md` § M2 → *Writer-signed change records*,
    /// ruling (8)(b)) — keys without ids would open what never lists.
    ///
    /// Empty for every identity that never succeeded, so the ordinary fleet is
    /// untouched; fail-closed by construction, since an unwired machine simply
    /// leaves a predecessor-sealed item unopenable rather than opening it under
    /// the wrong root.
    #[wasm_bindgen(js_name = setPredecessorBackupKeys)]
    pub fn set_predecessor_backup_keys(&self, secret_hex: String) -> Result<(), JsValue> {
        let keypair = fauna_core::identity::ActorKeypair::from_secret_hex(secret_hex.trim())
            .map_err(|e| JsValue::from_str(&format!("bad secret hex: {e}")))?;
        let registry = fauna_client_accounts::AccountRegistry::new(std::sync::Arc::new(
            fauna_client_accounts::LocalStorageSecretStore,
        ));
        let keys: Vec<Vec<u8>> = registry
            .predecessor_backup_keys(&keypair.actor_id_hex())
            .into_iter()
            .map(|k| k.to_bytes().to_vec())
            .collect();
        // Skip the call entirely when there is nothing to inject, mirroring
        // tui's `if !succession_predecessors.is_empty()` guard: it keeps the
        // never-succeeded path a no-op rather than an empty-vec write.
        if !keys.is_empty() {
            self.0.set_predecessor_backup_keys(keys);
        }
        let ids: Vec<Vec<u8>> = registry
            .attested_predecessor_actor_ids(&keypair.actor_id_hex())
            .into_iter()
            .map(|id| id.0.to_vec())
            .collect();
        if !ids.is_empty() {
            self.0.set_predecessor_actor_ids(ids);
        }
        // …and the keys PAIRED with those identities, replacing the bare keys
        // above: a row signed as a predecessor opens only under that
        // identity's root and its predecessors' (ruling (8)(c)).
        let (chain_ids, chain_keys): (Vec<Vec<u8>>, Vec<Vec<u8>>) = registry
            .predecessor_backup_keys_by_actor(&keypair.actor_id_hex())
            .into_iter()
            .map(|(id, key)| (id.0.to_vec(), key.to_bytes().to_vec()))
            .unzip();
        if !chain_ids.is_empty() {
            self.0.set_predecessor_chain(chain_ids, chain_keys);
        }
        Ok(())
    }

    /// The whole renderable Media page in one JSON object (`{ items, folders,
    /// sort, descending, filter, view_grid, error, loaded }`).
    #[wasm_bindgen(js_name = snapshotJson)]
    pub fn snapshot_json(&self) -> String {
        serde_json::to_string(&self.0.snapshot()).unwrap_or_default()
    }

    /// The item a **durable identity pair** names — the set's stable
    /// `FolderSummary.id` (`folderId`) plus the file's `path_hash` (hex-lowercase)
    /// — for a reference minted off this page, today `SearchNav::File`
    /// (`ui/search.md` § Implementation status today). Runs over the raw
    /// aggregate, not the active filter/sort, so a deep link doesn't depend on
    /// the user's current browse state. `null` = deleted, renamed, or in a set
    /// this caller cannot see. Returns a JSON `MediaItemSummary`, matching
    /// `snapshotJson`'s per-item shape.
    #[wasm_bindgen(js_name = locateFileJson)]
    pub fn locate_file_json(&self, folder_id: f64, path_hash: String) -> Option<String> {
        self.0
            .locate_file(folder_id as i64, path_hash)
            .map(|item| serde_json::to_string(&item).unwrap_or_default())
    }

    /// Re-read the cross-set all-media aggregate (`fauna.media.list`, paged).
    /// Resolves when done (read `snapshotJson` for the result / `error`).
    ///
    /// `secretHex` is the identity seed hex, or omitted / `undefined` for the
    /// plaintext-only read. The owner `BackupKey` (Library audience) is derived
    /// from it here, mirroring `uploadSelected`/`fetchThumbnail` — so the raw key
    /// never crosses into JS — and enables the sealed-first path render
    /// (`file-sync.md` § Sealed names & paths); web starts passing it in S3,
    /// alongside the other six apps.
    #[wasm_bindgen(js_name = refresh)]
    pub fn refresh(&self, secret_hex: Option<String>) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            let backup_key = match secret_hex {
                Some(hex) => match derive_backup_key(&hex) {
                    Ok(k) => Some(k),
                    Err(e) => return Err(JsValue::from_str(&e)),
                },
                None => None,
            };
            inner.refresh(backup_key).await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Upload a picked file into `folder` as `path` from the write-capable
    /// `deviceId`, sealed under the owner's 32-byte `backupKey` (Library
    /// audience). Resolves when done (read `snapshotJson` for the refreshed list
    /// or the `error`). A metadata-only folder is refused before anything is
    /// sealed or sent (the page error carries the reason).
    #[wasm_bindgen(js_name = upload)]
    pub fn upload(
        &self,
        folder: String,
        device_id: String,
        path: String,
        raw_bytes: Vec<u8>,
        backup_key: Vec<u8>,
    ) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            inner
                .upload(folder, device_id, path, raw_bytes, backup_key)
                .await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Upload a picked file into the **currently selected** set — the cross-app
    /// "upload into the selected folder" gesture (`media.md` § Layout & flow).
    /// The shared machine resolves the target (the `media-folder-filter` set, else
    /// the first set with media; else it sets the `media.error_no_set` page error
    /// and uploads nothing), so the "which set" + no-set policy is identical on
    /// every app (priority #1). `secretHex` is the identity seed hex — the owner
    /// `BackupKey` (Library audience) is derived from it here, mirroring
    /// `process_and_seal_library(data, secretHex)`. Like `upload`, this resolves
    /// with the page error set to "not yet supported" once a set is resolved until
    /// the web blob-upload coordinator (LEG B) lands; the no-set error path fires
    /// before that (it needs no uploader).
    #[wasm_bindgen(js_name = uploadSelected)]
    pub fn upload_selected(
        &self,
        device_id: String,
        path: String,
        raw_bytes: Vec<u8>,
        secret_hex: String,
    ) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            let backup_key = match derive_backup_key(&secret_hex) {
                Ok(k) => k,
                // A bad seed is caller glue error: surface it on the page banner via
                // the machine's own upload path so the shape matches native.
                Err(e) => return Err(JsValue::from_str(&e)),
            };
            inner
                .upload_selected(device_id, path, raw_bytes, backup_key)
                .await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Delete the member at `path` of `folder` from the write-capable `deviceId`
    /// (a tombstone over `fauna.sync.changes.record`). Pure WS-RPC — works on web
    /// today. Resolves when done (read `snapshotJson` for the refreshed list or
    /// the `error`).
    #[wasm_bindgen(js_name = delete)]
    pub fn delete(&self, folder: String, device_id: String, path: String) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            inner.delete(folder, device_id, path).await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Fetch + decrypt the thumbnail blob at `thumbnailHash` (a
    /// `MediaItem.thumbnail_hash`) into paint-ready image bytes (`Uint8Array`). The
    /// shared-Rust render leg (priority #2): `GET /api/v1/blob/<hash>` direct-by-
    /// hash (a public route, no bearer) → content-address verify → decrypt under
    /// the owner `BackupKey` → decoded JPEG bytes the SPA wraps in a blob URL for
    /// `<img>`. `secretHex` is the identity seed hex — the owner `BackupKey`
    /// (Library audience) is derived from it here, mirroring `uploadSelected`
    /// (`process_and_seal_library(data, secretHex)`), so the render decrypts under
    /// exactly the key the page's library uploads sealed with and the raw key never
    /// crosses into JS. The promise rejects (with the error message) on failure — a
    /// per-item query, so the page error banner is untouched; the caller falls back
    /// to the placeholder.
    #[wasm_bindgen(js_name = fetchThumbnail)]
    pub fn fetch_thumbnail(&self, thumbnail_hash: String, secret_hex: String) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            let backup_key = match derive_backup_key(&secret_hex) {
                Ok(k) => k,
                Err(e) => return Err(JsValue::from_str(&e)),
            };
            match inner.fetch_thumbnail(thumbnail_hash, backup_key).await {
                Ok(bytes) => Ok(js_sys::Uint8Array::from(bytes.as_slice()).into()),
                Err(e) => Err(err_to_js(e)),
            }
        })
    }

    /// The version history of the file at `path` in `folder`, oldest→newest —
    /// the `file-version-history` rows for the `media-item-detail` surface
    /// (media.md § Element IDs; semantics `file-sync.md` § File Versions).
    /// Resolves to a JSON string of `FileVersionSummary[]` (`version_num`,
    /// `manifest_hash` hex, `size_bytes`, `created_at` epoch-millis,
    /// `content_key_version`), mirroring `snapshotJson`. A per-item query: the
    /// promise rejects on failure and the page error banner is untouched.
    /// `includePruned` — `true` also returns soft-pruned rows (each carrying
    /// `pruned` + `purge_after`), the recovery browse of `file-versions.md`
    /// § Retention (3); `false` = the live-only listing.
    #[wasm_bindgen(js_name = fileVersions)]
    pub fn file_versions(
        &self,
        folder: String,
        path: String,
        include_pruned: bool,
    ) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            match inner.file_versions(folder, path, include_pruned).await {
                Ok(versions) => serde_json::to_string(&versions)
                    .map(|s| JsValue::from_str(&s))
                    .map_err(err_to_js),
                Err(e) => Err(err_to_js(e)),
            }
        })
    }

    /// Restore a soft-pruned version to the listable population — the
    /// `file-version-undelete-button` gesture (`file-versions.md` § Retention
    /// (3)). A per-item query like `fileVersions`: resolves on success (re-list
    /// to see the row live again), rejects with the error string on failure.
    #[wasm_bindgen(js_name = undeleteVersion)]
    pub fn undelete_version(&self, path: String, version_num: f64) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            // f64 at the boundary (an i64 param lands as a JS BigInt and every
            // plain-number call site throws); version seqs are far below 2^53.
            match inner.undelete_version(path, version_num as i64).await {
                Ok(()) => Ok(JsValue::TRUE),
                Err(e) => Err(err_to_js(e)),
            }
        })
    }

    /// Restore one version (a `fileVersions` item, passed back as its JSON
    /// string) of the file at `path` in `folder` from the write-capable
    /// `deviceId` — the `file-version-restore-confirm-button` gesture. Records
    /// an ordinary `modify` re-pointing the file at the historical manifest
    /// (metadata-only — restore works on web; reversible — it appends a new
    /// version; `file-sync.md` § Restore), then refreshes. Resolves when done
    /// (read `snapshotJson` for the refreshed list or the `error`).
    #[wasm_bindgen(js_name = restoreVersion)]
    pub fn restore_version(
        &self,
        folder: String,
        device_id: String,
        path: String,
        version_json: String,
    ) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            let version: fauna_media_machine::FileVersionSummary =
                serde_json::from_str(&version_json)
                    .map_err(|e| JsValue::from_str(&format!("bad version JSON: {e}")))?;
            inner
                .restore_version(folder, device_id, path, version)
                .await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Set the `media-sort-select` key from its UI value (`"name"` / `"size"` /
    /// `"date"`). An unrecognized value is ignored.
    #[wasm_bindgen(js_name = setSort)]
    pub fn set_sort(&self, value: String) {
        self.0.set_sort(value)
    }

    /// Set the sort direction (the desktop column-header asc/desc toggle).
    #[wasm_bindgen(js_name = setDescending)]
    pub fn set_descending(&self, descending: bool) {
        self.0.set_descending(descending)
    }

    /// Set the `media-folder-filter` scope: a set name, or `undefined`/`null` for
    /// the all-media default.
    #[wasm_bindgen(js_name = setFilter)]
    pub fn set_filter(&self, folder: Option<String>) {
        self.0.set_filter(folder)
    }

    /// Set the `media-view-toggle`: `true` = thumbnail grid, `false` = list.
    #[wasm_bindgen(js_name = setViewGrid)]
    pub fn set_view_grid(&self, grid: bool) {
        self.0.set_view_grid(grid)
    }

    /// Enter a **followed public folder** browse scope — the follow's options
    /// ride the snapshot's `followed` beside `folders`, and the SPA appends them
    /// to `media-folder-filter` and hands the chosen `value` straight back here
    /// (`media.md` § Followed public folders).
    ///
    /// **Selecting is what fetches**, once, on demand: a followed folder's rows
    /// live on its HOME nest, the follower's own nest keeps no copy, so the
    /// all-media default view never includes followed content — eager
    /// aggregation would put one relayed cross-nest fetch per follow on every
    /// Media refresh. Once active, the machine fills the snapshot's `items` with
    /// the followed listing, so the SPA's `media-item` render is byte-identical
    /// to the ordinary browse and nothing routes on the scope.
    ///
    /// ⚠ **The scope is read-only, structurally**: while `followed_scope` is
    /// set, offer no upload, no delete, no restore and no version history — the
    /// public plane is head-only, and an item's detail offers download alone.
    ///
    /// Leaving the scope is `setFilter` as usual. Failures surface on the page
    /// `error-message`, and the two families never render alike: the plane's own
    /// refusal enters the scope empty with the unavailable wording, a transport
    /// fault keeps the prior browse and reports itself.
    #[wasm_bindgen(js_name = selectFollowedScope)]
    pub fn select_followed_scope(&self, value: String) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            inner.select_followed_scope(value).await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Download one item from the ACTIVE followed scope, by its relative path —
    /// the keyless follower read (`media.md` § Followed public folders).
    ///
    /// Routed through the scope's home-nest fetcher, never the ordinary
    /// `download` and never the name-keyed custody resolver: a follower holds no
    /// key, structurally, and a followed read that touches custody is a bug even
    /// when it appears to work. `value` names the scope so a race with a scope
    /// switch fails loudly instead of downloading from the wrong nest; the
    /// machine resolves the head manifest from the entries it retained at scope
    /// entry, so no pointer crosses into JS (a followed item has no version rows
    /// to read one from).
    ///
    /// A per-item query, like `fetchThumbnail`: resolves to a `Uint8Array`, and
    /// the promise rejects with the message on failure — the page error banner
    /// is untouched.
    #[wasm_bindgen(js_name = downloadFollowed)]
    pub fn download_followed(&self, value: String, relative_path: String) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            match inner.download_followed(value, relative_path).await {
                Ok(bytes) => Ok(js_sys::Uint8Array::from(bytes.as_slice()).into()),
                Err(e) => Err(err_to_js(e)),
            }
        })
    }

    // ── Share links (`share-links.md` § Flows) ────────────────────────────
    //
    // Thin forwards to the shared Media machine's share gestures — the step
    // state, the reveal-after-registration rule and the error mapping all live
    // there; each gesture fires `onChanged`, and the page re-reads
    // `snapshotJson`'s `share_create` / `share_links`. Errors land in the
    // page's `error`, so the async ones resolve to `undefined` and never
    // reject.

    /// Wire the share-link author: the identity seed signs the token and
    /// derives the filename-seal root inside wasm (the raw secret never enters
    /// JS), and the links point at the nest this machine's socket rides — tui's
    /// and linux's `set_share_author(secret, nest_url)` at their build sites.
    /// Call after `setPredecessorBackupKeys`, whose keys open old list names.
    #[wasm_bindgen(js_name = setShareAuthor)]
    pub fn set_share_author(&self, secret_hex: String) -> Result<(), JsValue> {
        let keypair = fauna_rpc_wasm::keypair_from_secret_hex(&secret_hex)?;
        self.0
            .set_share_author(keypair.secret_bytes().to_vec(), self.1.nest_url());
        Ok(())
    }

    /// `share-link-button` — open the create surface on an eligible item.
    #[wasm_bindgen(js_name = openShareCreate)]
    pub fn open_share_create(&self, folder: String, path: String) {
        self.0.open_share_create(folder, path);
    }

    /// `share-link-expiry-select` — one of `share_expiry_options`.
    #[wasm_bindgen(js_name = setShareExpiry)]
    pub fn set_share_expiry(&self, value: String) {
        self.0.set_share_expiry(value);
    }

    /// `share-link-cancel-button` — close the create surface.
    #[wasm_bindgen(js_name = closeShareCreate)]
    pub fn close_share_create(&self) {
        self.0.close_share_create();
    }

    /// `share-link-create-button` — mint, register, then reveal the URL.
    #[wasm_bindgen(js_name = createShareLink)]
    pub fn create_share_link(&self) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            inner.create_share_link().await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `share-link-list-button` — open and load the list.
    #[wasm_bindgen(js_name = openShareLinks)]
    pub fn open_share_links(&self) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            inner.open_share_links().await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `share-link-list-close-button`.
    #[wasm_bindgen(js_name = closeShareLinks)]
    pub fn close_share_links(&self) {
        self.0.close_share_links();
    }

    /// `share-link-revoke-button` — arm the confirm for an Active row.
    #[wasm_bindgen(js_name = armShareRevoke)]
    pub fn arm_share_revoke(&self, token_id: String) {
        self.0.arm_share_revoke(token_id);
    }

    /// `share-link-revoke-cancel-button`.
    #[wasm_bindgen(js_name = cancelShareRevoke)]
    pub fn cancel_share_revoke(&self) {
        self.0.cancel_share_revoke();
    }

    /// `share-link-revoke-confirm-button` — revoke the armed row, re-list.
    #[wasm_bindgen(js_name = confirmShareRevoke)]
    pub fn confirm_share_revoke(&self) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            inner.confirm_share_revoke().await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Download the item at `relativePath` of `folder` — the
    /// `media-item-detail-download-button` gesture for an ordinary (not
    /// followed) item (`media.md` § Element IDs, approved 2026-09-25): the
    /// shared `MediaMachine::download_file` walk, the exact query tui's
    /// external-open confirm runs. `manifestHash` + `contentKeyVersion` are the
    /// LATEST `file-version-item` row's, handed back verbatim from
    /// `fileVersions` (rows are oldest→newest, so the last row is the current
    /// file — the same pick tui makes); the generation crosses as a JS number
    /// (`f64`), exactly as the row carried it out. A **shared** set opens under
    /// the content keys the constructor's resolver reads from this actor's
    /// custody; an owner-only set under the owner `BackupKey`, derived here
    /// from `secretHex` like `uploadSelected`'s (the raw key never enters JS).
    ///
    /// A per-item query, like `downloadFollowed`: resolves to a `Uint8Array`
    /// (the whole plaintext, in memory — the walk's documented contract), and
    /// the promise rejects with the message on failure so the page decides
    /// where it lands (web paints it on the page banner, since the user asked
    /// for this action). Never used for a followed item — that scope is
    /// keyless and routes to `downloadFollowed` (architectural rule 6).
    #[wasm_bindgen(js_name = download)]
    pub fn download(
        &self,
        manifest_hash: String,
        content_key_version: Option<f64>,
        folder: String,
        relative_path: String,
        secret_hex: String,
    ) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            let backup_key = derive_backup_key(&secret_hex).map_err(|e| JsValue::from_str(&e))?;
            let content_key_version = content_key_version.map(|v| v as u64);
            match inner
                .download_file(
                    manifest_hash,
                    content_key_version,
                    folder,
                    relative_path,
                    backup_key,
                )
                .await
            {
                Ok(bytes) => Ok(js_sys::Uint8Array::from(bytes.as_slice()).into()),
                Err(e) => Err(err_to_js(e)),
            }
        })
    }
}

/// Derive the owner 32-byte `BackupKey` (Library audience) bytes from the
/// identity seed hex — the same decode `fauna_wasm::process_and_seal_library_inner`
/// does, so the web upload seals under exactly the key the page's other library
/// uploads use.
fn derive_backup_key(secret_hex: &str) -> Result<Vec<u8>, String> {
    let bytes = hex::decode(secret_hex.trim()).map_err(|e| format!("bad secret hex: {e}"))?;
    let seed: [u8; 32] = bytes
        .try_into()
        .map_err(|_| "secret must be 64 hex chars".to_string())?;
    Ok(fauna_core::crypto::BackupKey::derive(&seed)
        .to_bytes()
        .to_vec())
}

// ── Panic hook ───────────────────────────────────────────────────────────
//
// Each wasm chunk is its own module with its own Rust runtime, so a hook
// installed in one chunk covers none of the others (see the
// `fauna-wasm-panic-hook` crate doc comment). `#[wasm_bindgen(start)]` runs
// automatically the moment this chunk's module is instantiated — no SPA-side
// call site to add or remember, unlike `fauna-wasm`'s explicit `installLogging`.
#[wasm_bindgen(start)]
fn panic_hook_start() {
    fauna_wasm_panic_hook::install("fauna-wasm-media");
}

/// Test-only: deliberately panics, so an e2e can assert the hook above really
/// names this chunk in the browser console — a headless witness, not a
/// review-only claim. Compiled out of every non-`test-helpers` build.
#[cfg(feature = "test-helpers")]
#[wasm_bindgen(js_name = panicForTestOnly)]
pub fn panic_for_test_only() {
    panic!("deliberate test panic");
}
