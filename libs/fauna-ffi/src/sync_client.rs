//! UniFFI façade for the `fauna.sync.*` device-sync control-plane WS-RPC
//! kinds — the typed twins of the deleted `GET|POST|DELETE /api/v1/sync/*`
//! HTTP routes (`docs/goal/architecture/api-layers.md` § File Sync).
//!
//! [`FfiSyncClient`] wraps `fauna_client_sync::SyncClient` (which wraps the
//! shared `NestClient`); the Rust-native Linux app calls the same
//! `SyncClient` directly. Construct via
//! [`crate::nest_client::FfiNestClient::sync`]. This seam carries the
//! media/engine/backups surface — `register`, `status`, `files`,
//! `backup_status` — the kinds a native app (apple /
//! android / windows) calls directly when it migrates its `/api/v1/sync/*`
//! HTTP call-sites off the deleted twins (priority #2, the `account.*`
//! UniFFI-seam precedent). `changes.list` is deliberately NOT surfaced: a
//! change row is consumed only through the shared reader that verifies its
//! writer signature (`fauna_protocol::sync_row_verify`, run by the sync
//! engine's pull), never raw in app glue. `changes.record` and
//! `conflicts.resolve` are NOT surfaced either: each puts this identity's
//! signature over a manifest the caller names, and a manifest is signed only
//! through a door that sealed it or found it as a judged version
//! (`writer-signed-change-records.md` ruling (10) — the sync engine, the
//! Media machine's gestures, the Devices machine's review list). The
//! **Devices-page** surface (`fauna.sync.devices.{list,delete}`) deliberately
//! lives elsewhere — the stateful `fauna-devices-machine` (`build_devices_machine`)
//! is its one canonical client seam, so it is **not** mirrored here (one
//! seam per surface). The sibling `fauna.sync.conflicts.list` kind (B14) *is*
//! surfaced here for reading — `conflicts_list` — because the
//! Backups/Devices conflict UI consumes it through this same
//! `fauna_client_sync::SyncClient` adapter (so a native app reaches them at
//! `nest.sync().conflicts_list()`, parallel to the Rust-native Linux call).
//!
//! No floats: device ids and path/manifest hashes cross as hex `String`,
//! sequences / sizes / timestamps as `i64` — matching the wire.

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_sync::SyncClient;
use fauna_protocol::folders::{ConflictCandidate, SyncConflict};
use fauna_protocol::sync::{BackupStatusEntry, SyncFile, SyncStatusReply};

use crate::{FfiError, stringify};

/// FFI mirror of [`fauna_protocol::sync::BackupStatusEntry`] — one folder
/// with the timestamp of its most recent change (`None` if no changes).
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiBackupStatusEntry {
    pub name: String,
    pub last_change_at: Option<i64>,
}

impl From<BackupStatusEntry> for FfiBackupStatusEntry {
    fn from(e: BackupStatusEntry) -> Self {
        Self {
            name: e.name,
            last_change_at: e.last_change_at,
        }
    }
}

/// FFI mirror of [`fauna_protocol::sync::SyncFile`] — one file in a set with
/// its path, hex manifest hash, size, and last-update time (the media page's
/// file list).
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiSyncFile {
    pub path: String,
    pub manifest_hash: String,
    pub size_bytes: i64,
    pub updated_at: i64,
}

impl From<SyncFile> for FfiSyncFile {
    fn from(f: SyncFile) -> Self {
        Self {
            path: f.path,
            manifest_hash: f.manifest_hash,
            size_bytes: f.size_bytes,
            updated_at: f.updated_at,
        }
    }
}

/// FFI mirror of [`fauna_protocol::sync::SyncStatusReply`] — a folder's
/// content reachability.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiSyncStatus {
    pub folder: String,
    pub source_online: bool,
}

impl From<SyncStatusReply> for FfiSyncStatus {
    fn from(r: SyncStatusReply) -> Self {
        Self {
            folder: r.folder,
            source_online: r.source_online,
        }
    }
}

/// FFI mirror of [`fauna_protocol::folders::ConflictCandidate`] — one
/// diverging version the user may choose between. `manifest_hash` is the
/// version kept if this candidate wins.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiConflictCandidate {
    pub manifest_hash: String,
    pub device_id: String,
    pub size_bytes: i64,
    pub created_at: i64,
}

impl From<ConflictCandidate> for FfiConflictCandidate {
    fn from(c: ConflictCandidate) -> Self {
        Self {
            manifest_hash: c.manifest_hash,
            device_id: c.device_id,
            size_bytes: c.size_bytes,
            created_at: c.created_at,
        }
    }
}

/// FFI mirror of [`fauna_protocol::folders::SyncConflict`] — one unresolved
/// sync conflict. `candidates` is empty for a candidate-free (mark-only) conflict
/// (resolve by `id` alone); otherwise it carries the diverging versions the
/// candidate chooser presents.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiSyncConflict {
    pub id: i64,
    pub folder: String,
    /// Hex-encoded device id that produced the conflicting change.
    pub device_id: String,
    pub path: String,
    pub conflict_type: String,
    pub details: Option<String>,
    pub created_at: i64,
    pub candidates: Vec<FfiConflictCandidate>,
}

impl From<SyncConflict> for FfiSyncConflict {
    fn from(c: SyncConflict) -> Self {
        Self {
            id: c.id,
            folder: c.folder,
            device_id: c.device_id,
            path: c.path,
            conflict_type: c.conflict_type,
            details: c.details,
            created_at: c.created_at,
            candidates: c.candidates.into_iter().map(Into::into).collect(),
        }
    }
}

/// UniFFI handle for the `fauna.sync.*` kinds. Construct via
/// [`crate::nest_client::FfiNestClient::sync`].
#[derive(uniffi::Object)]
pub struct FfiSyncClient {
    nest: Arc<NestClient>,
}

impl FfiSyncClient {
    pub(crate) fn from_nest(nest: Arc<NestClient>) -> Arc<Self> {
        Arc::new(Self { nest })
    }

    /// The typed sync surface; its change records are signed with this
    /// connection's identity, each record's nonce resolved through the holder's
    /// custody (`fauna_client_folders::record_signing`). A bearer-only
    /// connection — or a build without the resolver's feature — records
    /// unsigned.
    fn client(&self) -> SyncClient<Arc<NestClient>> {
        let client = SyncClient::new(Arc::clone(&self.nest));
        #[cfg(feature = "folders-author")]
        if let Some(kp) = self.nest.auth().keypair() {
            return client.with_record_signing(fauna_client_folders::record_signing(
                Arc::clone(&self.nest),
                kp,
                crate::account_runtime::folder_key_store(),
            ));
        }
        client
    }

    /// Seal a user-chosen device label under the registering owner's root,
    /// reached through this connection's own identity — the chain is
    /// `NestClient::auth()` → `AuthClient::keypair()` → `secret_bytes()` →
    /// `BackupKey::derive`, the identical construction the engine's owner arm
    /// uses.
    ///
    /// `None` on a bearer-only connection (no keypair — see [`Self::register`]),
    /// on a machine-authored label (the funnel refuses it), on a malformed
    /// device id, and on a seal error. Each of those is a sealless registration
    /// by design — the row rests nameless post-flip until the device's next
    /// keyed register (the register plane's accepted degrade, `file-sync.md`
    /// § Sealed names & paths) rather than a failed register: a device that
    /// cannot seal its name must still appear in the owner's device list,
    /// where it can be revoked.
    ///
    /// ⚠ Deliberately **silent**, unlike the sibling writers, which warn: this
    /// module compiles unconditionally while `tracing` is an *optional*
    /// dependency of this crate (`Cargo.toml`'s `dep:tracing` note), so a
    /// `--no-default-features` build — the one the Go mail-bridge binding is
    /// generated from — would fail to compile on a log line here.
    fn seal_device_label(&self, device_id_hex: &str, label: &str) -> Option<Vec<u8>> {
        let keypair = self.nest.auth().keypair()?;
        let salt = fauna_core::hex32::decode(device_id_hex).ok()?;
        let key = fauna_core::crypto::BackupKey::derive(keypair.secret_bytes());
        let root = fauna_core::path_crypto::LabelRoot::owner_of(&key);
        fauna_core::label_custody::seal_device_label(&root, &salt, label).unwrap_or(None)
    }

    /// The connection's label custody for the **export record path** (and the
    /// backup-status name render, [`render_backup_status`]) — the
    /// `FfiFoldersClient::client()` / `FfiSnapshotsClient::client()`
    /// assembly, reproduced here because `SyncClient` deliberately carries no
    /// custody: `path_sealed` is a **required parameter** on every record so
    /// each writer makes the sealing choice in the open (the
    /// `fauna-client-sync` policy note), and this façade is such a writer.
    ///
    /// Bearer-only connection (no keypair) ⇒ keyless custody — the export's
    /// seal mint then degrades to `None`, and post-flip the nest REFUSES the
    /// sealless record on a sealed plane (`path_seal_required`); only web-mode
    /// and reserved rails still accept one.
    fn export_custody(&self) -> fauna_core::label_custody::LabelCustody {
        let Some(keypair) = self.nest.auth().keypair() else {
            return fauna_core::label_custody::LabelCustody::default();
        };
        // The resolver gate must name THIS crate's `folders-author` feature —
        // `feature = "mls"` compiles to a permanently-false cfg here
        // (`folders_client.rs::client()`, whose reasoning binds verbatim).
        #[cfg(all(feature = "folders-author", not(target_arch = "wasm32")))]
        {
            let resolver: Arc<dyn fauna_core::folder_keys::FolderKeyResolver> =
                Arc::new(fauna_client_folders::NestFolderKeyResolver::new(
                    Arc::clone(&self.nest),
                    crate::account_runtime::folder_key_store(),
                ));
            fauna_core::label_custody::LabelCustody::new(
                Some(resolver),
                Some(fauna_core::crypto::BackupKey::derive(
                    keypair.secret_bytes(),
                )),
            )
        }
        #[cfg(not(all(feature = "folders-author", not(target_arch = "wasm32"))))]
        {
            // No resolver in this profile ⇒ a set's bound-ness is unknowable
            // here, and an owner-root fallback would be exactly the
            // silent member-side loss. Keyless custody seals NOTHING — the
            // honest S8 backfill row — matching the folders façade's stance
            // for the same profile.
            let _ = &keypair;
            fauna_core::label_custody::LabelCustody::default()
        }
    }
}

/// Render each backup-status entry's set name sealed-first under `custody`,
/// resolved by the entry's own `name_hash` (`LabelCustody::keys_for_row`), so
/// the name survives the nest's scrub of the plaintext (`path-sealing.md` § the
/// set-name plane). An entry this reader cannot open is **omitted** — the list
/// render's degrade, never a blank name. The ONLY skip is "nothing sealed".
async fn render_backup_status(
    custody: &fauna_core::label_custody::LabelCustody,
    entries: Vec<BackupStatusEntry>,
) -> Vec<FfiBackupStatusEntry> {
    if entries.iter().all(|e| e.name_sealed.is_none()) {
        return entries.into_iter().map(Into::into).collect();
    }
    let mut out = Vec::with_capacity(entries.len());
    for mut entry in entries {
        let wire_hash = entry.name_hash.as_ref().map(|b| &b[..]);
        let (keys, _) = custody.keys_for_row(&entry.name, wire_hash).await;
        match fauna_core::label_custody::render_set_name(
            &keys,
            entry.name_sealed.as_ref().map(|b| &b[..]),
            &entry.name,
            wire_hash,
        ) {
            fauna_core::path_crypto::SealedLabelRender::Sealed(name)
            | fauna_core::path_crypto::SealedLabelRender::Plaintext(name) => {
                entry.name = name;
                out.push(entry.into());
            }
            fauna_core::path_crypto::SealedLabelRender::Omit => {}
        }
    }
    out
}

#[fauna_uniffi_async::export]
impl FfiSyncClient {
    /// `fauna.sync.register` — register a device for file sync under the
    /// connection actor; returns the echoed (hex) device id. Capabilities
    /// default to `"read,write"`; re-registering the same id is idempotent.
    ///
    /// The label's seal is computed **internally** from the connection's own
    /// keypair rather than taken as a parameter, so this UniFFI **signature** is
    /// unchanged and no Go / Swift / Kotlin call site moves
    /// (`file-sync.md` § Sealed names & paths). ⚠ That is not the same as "no
    /// binding regen": UniFFI checksums hash **doc comments** too, so editing
    /// this very comment shifts `checksum_method_ffisyncclient_register` and the
    /// tracked `libs/fauna-mail-go` binding must be regenerated
    /// (`just mail-bridge-ffi`, verified by `just mail-bridge-ffi-check`).
    ///
    /// ⚠ **The keyless arm is real and load-bearing:** a bearer-only host (the
    /// Windows on-demand hydration helper) holds no keypair, so its
    /// registration is sealless and the row rests **nameless** post-flip,
    /// until the device's next keyed register re-stamps it. It is not an error
    /// and must not become one — a device that cannot seal must still appear
    /// in the device list, where it can be revoked. Permanence for a writer
    /// that never registers keyed is the accepted degrade (user ruling
    /// 2026-08-02, `file-sync.md` § Sealed names & paths — the old "row for
    /// the backfill to find" promise died with the flip's plaintext scrub).
    pub async fn register(&self, device_id: String, label: String) -> Result<String, FfiError> {
        let label_sealed = self.seal_device_label(&device_id, &label);
        let reply = self
            .client()
            .register(device_id, label, label_sealed)
            .await
            .map_err(stringify)?;
        Ok(reply.device_id)
    }

    /// `fauna.sync.status` — the sync state of one folder: its source device
    /// (+ online flag) and every destination. Ownership-checked.
    pub async fn status(&self, folder: String) -> Result<FfiSyncStatus, FfiError> {
        let reply = self.client().status(folder).await.map_err(stringify)?;
        Ok(reply.into())
    }

    /// `fauna.sync.files` — the files in one folder (the media page's file
    /// list). Ownership-checked.
    pub async fn files(&self, folder: String) -> Result<Vec<FfiSyncFile>, FfiError> {
        let reply = self.client().files(folder).await.map_err(stringify)?;
        Ok(reply.files.into_iter().map(Into::into).collect())
    }

    /// `fauna.sync.backup_status` — the bearer's folders with their most
    /// recent change timestamps (the Backups page's per-set "last backed
    /// up" line).
    /// Each name renders sealed-first through this connection's custody; an
    /// entry this reader cannot open is omitted ([`render_backup_status`]).
    pub async fn backup_status(&self) -> Result<Vec<FfiBackupStatusEntry>, FfiError> {
        let reply = self.client().backup_status().await.map_err(stringify)?;
        Ok(render_backup_status(&self.export_custody(), reply.folders).await)
    }

    /// `fauna.sync.conflicts.list` — the bearer's unresolved sync conflicts,
    /// each with its candidate versions (`FfiSyncConflict::candidates`, empty
    /// for a candidate-free (mark-only) conflict). The Backups/Devices conflict surface
    /// reads from here. Replay-safe pure read.
    pub async fn conflicts_list(&self) -> Result<Vec<FfiSyncConflict>, FfiError> {
        let reply = self.client().conflicts_list().await.map_err(stringify)?;
        Ok(reply.conflicts.into_iter().map(Into::into).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The export custody (what `backup_status` renders set names under)
    /// carries BOTH arms — the resolver so a
    /// bound set's path seals under the M2 generation its roster can open
    /// (owner-only custody here would silently seal where no member can
    /// follow), and the owner key so an unbound set still seals. The sync
    /// façade twin of `folders_client.rs`'s
    /// `the_facade_hands_its_client_a_resolver_wired_custody`, and like it a
    /// **shape** pin: the seal chain itself (`keys_for` → `label_seal_root` →
    /// `seal_path`) is pinned semantically in fauna-core, and driving it
    /// through the real resolver here would attempt network I/O.
    #[cfg(all(feature = "folders-author", not(target_arch = "wasm32")))]
    #[test]
    fn the_export_records_custody_carries_both_arms() {
        let nest = NestClient::new(
            "wss://unreachable.invalid".into(),
            fauna_core::identity::ActorKeypair::from_secret([7u8; 32]),
        );
        let facade = FfiSyncClient::from_nest(nest);
        let custody = facade.export_custody();
        assert_eq!(
            (custody.has_resolver(), custody.has_owner_key()),
            (true, true),
            "a keyless export custody drops an arm here"
        );
    }

    /// A bearer-only connection (the Windows hydration helper) holds no
    /// keypair, so the export custody is keyless and the mint degrades to
    /// `None` — which post-flip the nest answers with the `path_seal_required`
    /// refusal on a sealed plane. Network-free: keyless custody has no
    /// resolver to call.
    #[tokio::test]
    async fn a_bearer_only_export_mints_no_seal() {
        let bearer: Arc<dyn fauna_nest_http::BearerSource> =
            Arc::new(fauna_nest_http::StaticBearer("test.bearer".into()));
        let auth = fauna_client::AuthClient::bearer_only(
            "https://unreachable.invalid".into(),
            [9u8; 32],
            bearer,
            reqwest::Client::new(),
        );
        let facade = FfiSyncClient::from_nest(NestClient::with_auth(Arc::new(auth)));
        let custody = facade.export_custody();
        assert!(!custody.has_resolver() && !custody.has_owner_key());
    }

    #[test]
    fn backup_status_entry_mirror_carries_optional_timestamp() {
        let e: FfiBackupStatusEntry = BackupStatusEntry {
            name: "documents".into(),
            last_change_at: Some(1_700_000_000),
            ..Default::default()
        }
        .into();
        assert_eq!(e.name, "documents");
        assert_eq!(e.last_change_at, Some(1_700_000_000));
    }

    /// A scrubbed entry (blank plaintext) renders from its seal under custody
    /// found by its `name_hash`; one this reader cannot open is omitted.
    #[tokio::test]
    async fn backup_status_renders_a_scrubbed_name_and_omits_an_unopenable_one() {
        let owner = fauna_core::crypto::BackupKey::from_bytes([3u8; 32]);
        let scrubbed = |name: &str, key: &fauna_core::crypto::BackupKey| BackupStatusEntry {
            name: String::new(),
            name_hash: Some(fauna_protocol::ByteBuf::from(
                fauna_core::path_crypto::set_name_hash(name).to_vec(),
            )),
            name_sealed: Some(fauna_protocol::ByteBuf::from(
                fauna_core::label_custody::seal_set_name(
                    &fauna_core::path_crypto::LabelRoot::owner_of(key),
                    name,
                )
                .unwrap()
                .unwrap(),
            )),
            last_change_at: Some(7),
            ..Default::default()
        };
        let stranger = fauna_core::crypto::BackupKey::from_bytes([8u8; 32]);
        let custody = fauna_core::label_custody::LabelCustody::owner_only(owner.clone());
        let rendered = render_backup_status(
            &custody,
            vec![scrubbed("Taxes", &owner), scrubbed("Theirs", &stranger)],
        )
        .await;
        assert_eq!(
            rendered,
            vec![FfiBackupStatusEntry {
                name: "Taxes".into(),
                last_change_at: Some(7),
            }]
        );
    }

    #[test]
    fn status_mirror_maps_reachability() {
        let r: FfiSyncStatus = SyncStatusReply {
            folder: "media".into(),
            source_online: true,
            extra: Default::default(),
        }
        .into();
        assert_eq!(r.folder, "media");
        assert!(r.source_online);
    }

    #[test]
    fn conflict_mirror_maps_candidates() {
        let c: FfiSyncConflict = SyncConflict {
            id: 3,
            folder: "documents".into(),
            device_id: "aa".into(),
            path: "a/b.txt".into(),
            conflict_type: "concurrent_edit".into(),
            details: Some("two writers".into()),
            created_at: 1_700_000_000,
            candidates: vec![ConflictCandidate {
                manifest_hash: "ff".into(),
                device_id: "bb".into(),
                size_bytes: 512,
                created_at: 1_700_000_001,
                content_key_version: None,
                extra: Default::default(),
            }],
            resolution: None,
            resolved_at: None,
            winning_manifest_hash: None,
            ..Default::default()
        }
        .into();
        assert_eq!(c.id, 3);
        assert_eq!(c.conflict_type, "concurrent_edit");
        assert_eq!(c.candidates.len(), 1);
        assert_eq!(c.candidates[0].manifest_hash, "ff");
        assert_eq!(c.candidates[0].device_id, "bb");
    }

    #[test]
    fn mark_only_conflict_has_no_candidates() {
        let c: FfiSyncConflict = SyncConflict {
            id: 1,
            folder: "media".into(),
            device_id: "aa".into(),
            path: "x".into(),
            conflict_type: "mark_only".into(),
            details: None,
            created_at: 1,
            candidates: vec![],
            resolution: None,
            resolved_at: None,
            winning_manifest_hash: None,
            ..Default::default()
        }
        .into();
        assert!(c.candidates.is_empty());
        assert_eq!(c.details, None);
    }
}
