//! The photo-library ingress binding — which folder this device's photo
//! ingress (PhotoKit on apple, MediaStore on android) feeds.
//!
//! Thin native glue over `fauna_folders_machine::photo_library`, which owns the
//! use-or-create rule (and its tests). This layer adds the two things the
//! wasm-clean machine crate cannot: the **device-local binding store** and the
//! UniFFI face. Apple and android consume the same face — the rule that must
//! never orphan a user's photo library has exactly one implementation
//! (`docs/goal/ui/folders.md` § Photo backup → *Target set model*).
//!
//! **Why the binding is not a location binding.** A location binding is a
//! watch-dir binding: every one becomes a resident engine with a filesystem
//! watcher (the `fauna-sync-agent`'s). A photo library is not a folder — its
//! ingress is the sealed per-file `ingest_file` path precisely so there is no
//! watcher and no reconcile (and therefore no tombstone risk from deleting the
//! staged temp). A photo entry among the location bindings would resurrect
//! exactly that hazard. Separate file, in the sync state dir.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use fauna_core::folder_keys::FolderRef;
use fauna_folders_machine::nest_api::ws_rpc::WsRpcFolderNest;
use fauna_folders_machine::photo_library::{PhotoLibrarySet, ensure_photo_library_set};
use fauna_folders_machine::{DeviceOption, FolderNestApi, FolderWizardObserver};

use crate::{FfiError, FfiNestClient};

/// The device-local binding file in the sync state dir. Shape:
/// `{"folder": "<name>", "folder_id": "<FolderRef wire>"}` — the identity is
/// the binding ([`load_binding`] reads only it); the name rides along as the
/// label a human reading the file expects.
const PHOTO_INGRESS_FILE: &str = "photo-ingress.json";

fn binding_path(state_dir: &str) -> PathBuf {
    Path::new(state_dir).join(PHOTO_INGRESS_FILE)
}

/// Read the persisted binding — the bound set's identity. A missing /
/// unreadable / malformed file, or one carrying no parseable `folder_id`, is
/// simply "not bound yet" — the resolver re-derives from the nest's set list,
/// which is the authority. Never an error: a corrupt cache must not brick the
/// user's photo backup.
fn load_binding(state_dir: &str) -> Option<FolderRef> {
    let raw = std::fs::read_to_string(binding_path(state_dir)).ok()?;
    let parsed: serde_json::Value = serde_json::from_str(&raw).ok()?;
    FolderRef::parse(parsed.get("folder_id")?.as_str()?)
}

fn save_binding(state_dir: &str, set: &PhotoLibrarySet) {
    let path = binding_path(state_dir);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let body = serde_json::json!({
        "folder": set.name,
        "folder_id": set.folder_ref.to_wire(),
    })
    .to_string();
    // Best-effort: a failed write costs one extra `fauna.folders.list` on the
    // next pass (the resolver is idempotent and re-derives the same answer), so
    // it must not fail the ingest.
    if let Err(e) = std::fs::write(&path, body) {
        tracing::warn!(target: "fauna_ffi", "photo-ingress binding write failed: {e}");
    }
}

/// A no-op observer: the preset drives the wizard headlessly, so there is no
/// view to tick.
#[derive(Debug)]
struct HeadlessObserver;

impl FolderWizardObserver for HeadlessObserver {
    fn on_changed(&self) {}
}

/// The resolved photo-library set as the apps take it
/// ([`photo_library_set`]).
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiPhotoLibrarySet {
    /// The set's current name — the label the control-plane calls still
    /// address (`fauna.snapshots.create`) and a log line shows.
    pub name: String,
    /// The set's `FolderRef` wire string — the key every engine call takes
    /// (`FfiSyncEngineHost::ingest_file`, `file_states`, `transfer_backlog`).
    pub folder_id: String,
}

/// Resolve the folder this device's photo ingress feeds, creating the
/// **"Photo Library"** preset through the ordinary
/// `FolderWizardMachine` if the actor has none.
///
/// Returns the set's **identity** (`folder_id`, the key every engine call
/// takes: `FfiSyncEngineHost::ingest_file` and the per-set state DB) beside its
/// current name (the label `fauna.snapshots.create` still addresses). The
/// device-local binding is by identity, so a renamed set stays this device's
/// target and a same-named newcomer never takes over.
///
/// Call this before ingesting. The nest rejects a `changes.record` into a set
/// with no control-plane row (`not_found`), so ingesting into an unresolved set
/// silently uploads chunks that never become files.
///
/// `state_dir` is the sync state dir (the engine host's per-set state DBs).
/// `device_id` / `device_label` identify this device for enrollment into the
/// created set.
#[fauna_uniffi_async::export]
pub async fn photo_library_set(
    nest: Arc<FfiNestClient>,
    state_dir: String,
    device_id: String,
    device_label: String,
) -> Result<FfiPhotoLibrarySet, FfiError> {
    // The owner key seals a created set from birth and opens the listed names:
    // a sealed set's row rests no plaintext name (schema 114), so without it
    // the Photo Library would never be found again by name.
    let nest_arc = nest.nest_arc();
    let owner = nest_arc
        .auth()
        .keypair()
        .map(|kp| fauna_core::crypto::BackupKey::derive(kp.secret_bytes()));
    let mut seam = WsRpcFolderNest::new(nest_arc, Some(crate::account_runtime::folder_key_store()));
    if let Some(owner) = owner {
        seam = seam.with_owner_seal_key(owner);
    }
    let api: Arc<dyn FolderNestApi> = Arc::new(seam);
    let devices = vec![DeviceOption {
        device_id,
        label: device_label,
    }];

    let set = ensure_photo_library_set(
        api,
        Arc::new(HeadlessObserver),
        devices,
        load_binding(&state_dir),
    )
    .await
    .map_err(|e| FfiError::General {
        msg: format!("photo-library set: {}", e.detail()),
    })?;

    save_binding(&state_dir, &set);
    Ok(FfiPhotoLibrarySet {
        name: set.name,
        folder_id: set.folder_ref.to_wire(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_round_tripped_binding_reads_back_by_identity() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().to_str().expect("utf8").to_string();

        assert_eq!(
            load_binding(&path),
            None,
            "unbound before the first resolve"
        );

        save_binding(
            &path,
            &PhotoLibrarySet {
                name: "Photo Library".into(),
                folder_ref: FolderRef::Local(42),
            },
        );
        assert_eq!(load_binding(&path), Some(FolderRef::Local(42)));
    }

    #[test]
    fn a_corrupt_binding_reads_as_unbound_rather_than_bricking_backup() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().to_str().expect("utf8").to_string();
        std::fs::write(binding_path(&path), "{ not json").expect("write");

        assert_eq!(load_binding(&path), None);
    }

    /// A binding file carrying only a name (no identity) is no binding: the
    /// name is a label two sets can share, so the resolver re-derives from
    /// the list rather than trusting it.
    #[test]
    fn a_name_only_binding_reads_as_unbound() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().to_str().expect("utf8").to_string();
        std::fs::write(binding_path(&path), r#"{"folder": "Photo Library"}"#).expect("write");

        assert_eq!(load_binding(&path), None);
    }
}
