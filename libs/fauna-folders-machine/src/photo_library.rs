//! The photo-library set model — which folder the platform photo ingress
//! (PhotoKit on apple, MediaStore on android) feeds.
//!
//! Authority: `docs/goal/ui/folders.md` § Photo backup → *Target set model*.
//! The photo library is an ordinary folder created through the ordinary
//! [`FolderWizardMachine`] as a one-tap preset — never a client-invented magic
//! name — whose devices sit at the **archive point** `{originates, accepts,
//! !applies_deletes}` and whose nest place keeps an **explicit** snapshot policy
//! ([`PHOTO_LIBRARY_RETENTION`]); a device-local binding records which set the
//! ingress feeds.
//!
//! (A pre-B3 device's hardcoded `"photos"` set was once *adopted* as the
//! ingress target; that legacy adoption rule was retired 2026-09-24 by the
//! compat-remnant sweep (`version-compatibility.md` § Dimension 2, program 4)
//! — no pre-sweep installation exists. A set named `"photos"` is now an
//! ordinary user folder, never the ingress target.)
//!
//! # Why this lives in shared Rust
//!
//! The rule's failure mode is orphaning a user's photo library, and apple +
//! android both need it (`folders.md` § Photo backup: android "mirrors apple's
//! shape exactly"). One implementation, two apps — priority #2/#4. The
//! wizard is already ratified shared-Rust ("No wizard logic per client",
//! `folders.md` § Where logic lives), and this is the headless preset drive of
//! that same wizard.
//!
//! # Layering
//!
//! This module is **wasm-clean** (the crate is built for web): it takes the
//! device-local binding (the bound set's identity) as a plain argument and
//! returns the resolved set — its identity plus its current name. Reading and
//! persisting that binding is the native caller's job — `libs/fauna-ffi`,
//! which owns the on-disk store that apple and android share. It deliberately
//! does **not** live in a location binding: every one of those is a resident
//! watch-dir engine exposed to `reconcile`, which is exactly the tombstone
//! hazard the sealed per-file `ingest_file` path avoids.

use std::sync::Arc;

use fauna_core::folder_keys::FolderRef;

use crate::machine::FolderWizardMachine;
use crate::nest_api::{FolderApiError, FolderNestApi, FolderRow};
use crate::observer::FolderWizardObserver;
use crate::state::{DeviceOption, FolderWizardStep, PHOTO_LIBRARY_RETENTION};

/// The wizard-preset name for a fresh install's photo-library set. User-visible
/// (it renders as an ordinary set on the Folders and Media pages).
pub const PHOTO_LIBRARY_SET_NAME: &str = "Photo Library";

/// The resolved photo-library set: its identity — the key every downstream
/// call takes (`FfiSyncEngineHost::ingest_file`, the per-set state DB) — and
/// its current name, the label the control-plane calls still address
/// (`fauna.snapshots.create`) and a log line shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhotoLibrarySet {
    pub name: String,
    pub folder_ref: FolderRef,
}

impl PhotoLibrarySet {
    fn of_row(row: &FolderRow) -> Self {
        Self {
            name: row.name.clone(),
            folder_ref: FolderRef::Local(row.id),
        }
    }
}

/// What the ingress should do about its target set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PhotoLibraryDecision {
    /// Bind to this already-existing set.
    Use(PhotoLibrarySet),
    /// No usable set exists — create this one through the wizard.
    Create(String),
}

/// Decide which set the photo ingress feeds, given the actor's existing folder
/// rows and this device's persisted binding (`None` before the first resolve).
///
/// Order matters, and it is the whole safety property:
///
/// 1. **A live binding wins — by identity.** Once bound, the set stays the
///    target whatever it is called now: a rename must not re-target this
///    device's camera roll to a *different* set that happens to wear the
///    preset name, and a set appearing later must not silently take over.
/// 2. **An existing preset is reused, not duplicated** — a duplicate-name
///    create is a hard `Conflict`, never an idempotent success. (Names are
///    unique per owner and the list is owner-scoped, so at most one row wears
///    the preset name.)
/// 3. Otherwise create the preset.
///
/// A binding whose set has vanished (the user deleted it) re-resolves rather
/// than failing closed forever.
pub fn decide_photo_library_set(
    existing: &[FolderRow],
    bound: Option<FolderRef>,
) -> PhotoLibraryDecision {
    if let Some(bound) = bound
        && let Some(row) = existing
            .iter()
            .find(|row| FolderRef::Local(row.id) == bound)
    {
        return PhotoLibraryDecision::Use(PhotoLibrarySet::of_row(row));
    }
    if let Some(row) = existing
        .iter()
        .find(|row| row.name == PHOTO_LIBRARY_SET_NAME)
    {
        return PhotoLibraryDecision::Use(PhotoLibrarySet::of_row(row));
    }
    PhotoLibraryDecision::Create(PHOTO_LIBRARY_SET_NAME.to_string())
}

/// Resolve the photo-library set, creating the preset if needed, and
/// return it — its identity is the key every downstream call takes
/// (`ingest_file`, the state DB), its name the label the control plane still
/// addresses (`snapshot.create`).
///
/// Fails closed: an unreadable set list refuses rather than guessing, because
/// guessing could create a duplicate set beside the user's real one. A failed
/// create surfaces its error rather than returning a set the nest does not
/// have — returning a phantom set would send every subsequent `changes.record`
/// into `not_found`, which is precisely the silent failure this track fixes.
/// The create arm re-lists to learn the new row's id (the create reply carries
/// none); a created set the list then does not carry is the same phantom, and
/// refuses the same way.
///
/// `devices` is the enrollable device list (this device first); the preset
/// enrolls them at the archive point (`PlaceFlags::archive_place`,
/// `ui/folders.md` § Photo backup).
pub async fn ensure_photo_library_set(
    api: Arc<dyn FolderNestApi>,
    observer: Arc<dyn FolderWizardObserver>,
    devices: Vec<DeviceOption>,
    bound: Option<FolderRef>,
) -> Result<PhotoLibrarySet, FolderApiError> {
    let existing = api.list_folder_rows().await?;

    let name = match decide_photo_library_set(&existing, bound) {
        PhotoLibraryDecision::Use(set) => return Ok(set),
        PhotoLibraryDecision::Create(name) => name,
    };

    // The ordinary wizard, driven headlessly — the same machine, the same
    // `fauna.folders.create` + `places.set`, just with no sheet on screen.
    let device_count = devices.len();
    let wizard = FolderWizardMachine::new(observer, devices, Arc::clone(&api));
    wizard.set_name(name.clone());
    wizard.set_retention(Some(PHOTO_LIBRARY_RETENTION));
    wizard.next(); // Name -> Devices
    let archive = fauna_protocol::folders::PlaceFlags::archive_place();
    for i in 0..device_count {
        let i = i as u32;
        wizard.toggle_device_member(i);
        wizard.set_device_flags(
            i,
            archive.originates,
            archive.accepts,
            archive.applies_deletes,
        );
    }
    wizard.next(); // Devices -> Review

    // `submit()` is non-throwing: failure lands on the review snapshot.
    if wizard.submit().await != FolderWizardStep::Done {
        let review = wizard.review_snapshot();
        return Err(FolderApiError::Transient {
            detail: review
                .error
                .map(|e| e.log_line())
                .unwrap_or_else(|| format!("could not create the '{name}' folder")),
        });
    }

    // The created row's id comes from the list, not the create reply.
    let created = api.list_folder_rows().await?;
    created
        .iter()
        .find(|row| row.name == name)
        .map(PhotoLibrarySet::of_row)
        .ok_or_else(|| FolderApiError::Transient {
            detail: format!("the '{name}' folder was created but the nest does not list it"),
        })
}
